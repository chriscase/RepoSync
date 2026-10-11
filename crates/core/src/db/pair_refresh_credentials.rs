//! Strict, read-only credential admission for the refresh execution caller.
//! Neither a configuration fallback nor ambient Git authentication is consulted.

use std::collections::BTreeSet;

use hmac::{Hmac, Mac};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::Sha256;

use super::Database;
use crate::errors::DatabaseError;

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh credentials refused: {detail}"))
}

// Deliberately not Debug or Serialize: these values may only be passed to the
// explicitly pinned Git/SVN clients. The public approval sees the keyed digest.
pub struct RefreshCredentials {
    digest: String,
    child_git: Option<String>,
    parent_git: Option<String>,
    child_svn: Option<String>,
    parent_svn: Option<String>,
}

impl RefreshCredentials {
    pub fn context_digest(&self) -> &str {
        &self.digest
    }
    pub fn child_git_token(&self) -> Option<&str> {
        self.child_git.as_deref()
    }
    pub fn parent_git_token(&self) -> Option<&str> {
        self.parent_git.as_deref()
    }
    pub fn child_svn_password(&self) -> Option<&str> {
        self.child_svn.as_deref()
    }
    pub fn parent_svn_password(&self) -> Option<&str> {
        self.parent_svn.as_deref()
    }
}

#[derive(Serialize)]
struct ChainEntry {
    id: String,
    scope_uuid: String,
    parent_id: Option<String>,
    git: Option<(String, String)>,
    svn: Option<(String, String)>,
}

fn credential(tx: &Connection, key: &str) -> Result<Option<(String, String)>, DatabaseError> {
    Ok(tx
        .query_row(
            "SELECT value,updated_at FROM kv_state WHERE key=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn chain(tx: &Connection, first: &str) -> Result<Vec<ChainEntry>, DatabaseError> {
    let mut current = Some(first.to_owned());
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    while let Some(id) = current {
        if id.is_empty() || !seen.insert(id.clone()) || entries.len() == 64 {
            return Err(refused("cyclic or oversized registration chain"));
        }
        let (scope_uuid, parent_id): (Option<String>, Option<String>) = tx
            .query_row(
                "SELECT scope_uuid,parent_id FROM repositories WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| refused("registration chain is missing"))?;
        let scope_uuid = scope_uuid
            .filter(|raw| {
                uuid::Uuid::parse_str(raw).is_ok_and(|id| !id.is_nil() && id.to_string() == *raw)
            })
            .ok_or_else(|| refused("registration chain lacks a durable identity"))?;
        let owners: i64 = tx.query_row(
            "SELECT COUNT(*) FROM repositories WHERE scope_uuid=?1",
            [&scope_uuid],
            |row| row.get(0),
        )?;
        if owners != 1 {
            return Err(refused(
                "durable identity belongs to multiple registrations",
            ));
        }
        current = parent_id.clone();
        entries.push(ChainEntry {
            git: credential(tx, &format!("secret_git_token_{id}"))?,
            svn: credential(tx, &format!("secret_svn_password_{id}"))?,
            id,
            scope_uuid,
            parent_id,
        });
    }
    let scopes: BTreeSet<_> = entries.iter().map(|entry| &entry.scope_uuid).collect();
    if scopes.len() != entries.len() {
        return Err(refused("registration chain repeats a durable identity"));
    }
    Ok(entries)
}

fn selected<'a>(
    values: impl Iterator<Item = Option<&'a (String, String)>>,
    global: Option<&'a (String, String)>,
) -> Result<Option<String>, DatabaseError> {
    match values.flatten().next().or(global) {
        Some((value, _)) if value.is_empty() => Err(refused("explicit credential revocation")),
        Some((value, _)) => Ok(Some(value.clone())),
        None => Ok(None),
    }
}

fn fingerprint_key(tx: &Connection) -> Result<[u8; 32], DatabaseError> {
    // A preview must never create a key or repair corrupt state. The daemon's
    // established encryption key supplies a private HMAC key, also when env
    // configuration is its normal source. Key rotation invalidates approval.
    let configured = match std::env::var("REPOSYNC_ENCRYPTION_KEY") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err(refused("configured fingerprint key is unreadable")),
    };
    let encoded = match configured {
        Some(value) => value,
        None => tx
            .query_row(
                "SELECT value FROM kv_state WHERE key='encryption_key'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| refused("fingerprint key is not established"))?,
    };
    hex::decode(encoded.trim())
        .map_err(|_| refused("invalid fingerprint key"))?
        .try_into()
        .map_err(|_| refused("invalid fingerprint key"))
}

pub(crate) fn resolve_refresh_credentials(
    tx: &Connection,
    child: &str,
    parent: &str,
) -> Result<RefreshCredentials, DatabaseError> {
    if child.is_empty() || parent.is_empty() || child == parent {
        return Err(refused("distinct registered child and parent are required"));
    }
    let child_chain = chain(tx, child)?;
    let parent_chain = chain(tx, parent)?;
    if child_chain[0].parent_id.as_deref() != Some(parent) {
        return Err(refused("child no longer belongs to the requested parent"));
    }
    let global_git = credential(tx, "secret_git_token")?;
    let global_svn = credential(tx, "secret_svn_password")?;
    let child_git = selected(
        child_chain.iter().map(|entry| entry.git.as_ref()),
        global_git.as_ref(),
    )?;
    let parent_git = selected(
        parent_chain.iter().map(|entry| entry.git.as_ref()),
        global_git.as_ref(),
    )?;
    let child_svn = selected(
        child_chain.iter().map(|entry| entry.svn.as_ref()),
        global_svn.as_ref(),
    )?;
    let parent_svn = selected(
        parent_chain.iter().map(|entry| entry.svn.as_ref()),
        global_svn.as_ref(),
    )?;
    let bytes = serde_json::to_vec(&(
        "pair_refresh_credentials_v1",
        &child_chain,
        &parent_chain,
        &global_git,
        &global_svn,
    ))
    .map_err(|_| refused("credential context encoding failed"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&fingerprint_key(tx)?)
        .map_err(|_| refused("invalid fingerprint key"))?;
    mac.update(&bytes);
    Ok(RefreshCredentials {
        digest: hex::encode(mac.finalize().into_bytes()),
        child_git,
        parent_git,
        child_svn,
        parent_svn,
    })
}

impl Database {
    /// Capture both managed chains in one read transaction. Misses may use
    /// anonymous pinned clients; revocations and every read/identity error refuse.
    pub fn resolve_pair_refresh_credentials(
        &self,
        child: &str,
        parent: &str,
    ) -> Result<RefreshCredentials, DatabaseError> {
        self.transaction(|tx| resolve_refresh_credentials(tx, child, parent))
    }
}
