//! Recheckable current database actor and effective engine policy context.
//! Capture is read-only; durable job enrollment and publication are separate.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{SyncConfig, SyncMode};
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::sync_engine::SyncEngine;

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh actor/policy refused: {detail}"))
}

/// Safe preview metadata. A client-supplied copy cannot construct live context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshActorPolicyPin {
    pub version: u8,
    pub actor_id: String,
    pub session_context_digest: String,
    pub policy_context_digest: String,
}

/// Private construction binds this context to the actual engine and its database.
/// Bearer tokens remain in memory and cannot be serialized or debug-printed.
pub struct LiveRefreshActorPolicy<'engine> {
    engine: &'engine SyncEngine,
    db: &'engine Database,
    allowed: &'engine [String],
    blocked: &'engine [String],
    lfs_threshold: &'engine u64,
    bearer: String,
    pin: RefreshActorPolicyPin,
}

impl LiveRefreshActorPolicy<'_> {
    /// Preserve this owned resource set before any enrollment attempt. Current
    /// actor/storage checks still do not authorize a job or external effect.
    pub fn retain_workspace_before_enrollment(
        &self,
        engine: &SyncEngine,
        workspace: &super::workspace::RefreshWorkspace,
    ) -> Result<(), DatabaseError> {
        self.require_current(engine)?;
        workspace.require_binding(self.db, &self.pin)?;
        workspace.retain_before_enrollment(|| {
            self.require_current(engine)?;
            workspace.require_binding(self.db, &self.pin)
        })
    }
    /// Observe and prepare registered sources within this owned workspace.
    /// Server time/operation identity are used; approval/enrollment stay separate.
    pub async fn prepare_in_workspace(
        &self,
        engine: &SyncEngine,
        child_id: &str,
        workspace: &super::workspace::RefreshWorkspace,
    ) -> anyhow::Result<super::live_preparation::LivePreparedRefresh> {
        self.require_current(engine)?;
        workspace.require_binding(self.db, &self.pin)?;
        let prepared = super::live_preparation::prepare_registered_in_workspace(
            self.db,
            child_id,
            workspace.clone(),
            Utc::now().timestamp(),
        )
        .await?;
        self.require_current(engine)?;
        workspace.require_binding(self.db, &self.pin)?;
        Ok(prepared)
    }
    /// Allocate owned resources for this actual engine/storage context. This
    /// does not enroll a job, retain an attempt or authorize a publication.
    pub fn create_workspace(
        &self,
        engine: &SyncEngine,
    ) -> Result<super::workspace::RefreshWorkspace, DatabaseError> {
        self.require_current(engine)?;
        let workspace =
            super::workspace::create(self.db, &engine.config().daemon.data_dir, &self.pin)?;
        self.require_current(engine)?;
        workspace.require_binding(self.db, &self.pin)?;
        Ok(workspace)
    }
    pub fn pin(&self) -> &RefreshActorPolicyPin {
        &self.pin
    }

    /// The execution caller must pass its current running engine at each gate.
    /// Replacement engines require fresh approval even with identical settings.
    pub fn require_current(&self, engine: &SyncEngine) -> Result<(), DatabaseError> {
        if !std::ptr::eq(self.engine, engine) {
            return Err(refused("running engine identity changed"));
        }
        let current = capture_pin(
            self.db,
            &engine.config().sync,
            self.allowed,
            self.blocked,
            *self.lfs_threshold,
            &self.bearer,
        )?;
        if current != self.pin {
            return Err(refused("actor, session or effective policy changed"));
        }
        Ok(())
    }
}

pub(crate) fn capture<'engine>(
    engine: &'engine SyncEngine,
    db: &'engine Database,
    allowed: &'engine [String],
    blocked: &'engine [String],
    lfs_threshold: &'engine u64,
    bearer: &str,
) -> Result<LiveRefreshActorPolicy<'engine>, DatabaseError> {
    let pin = capture_pin(
        db,
        &engine.config().sync,
        allowed,
        blocked,
        *lfs_threshold,
        bearer,
    )?;
    Ok(LiveRefreshActorPolicy {
        engine,
        db,
        allowed,
        blocked,
        lfs_threshold,
        bearer: bearer.to_owned(),
        pin,
    })
}

fn policy_digest(
    policy: &SyncConfig,
    allowed: &[String],
    blocked: &[String],
    lfs_threshold: u64,
) -> Result<String, DatabaseError> {
    if policy.mode != SyncMode::Direct
        || !policy.sync_branches.is_empty()
        || policy.max_file_size != 0
        || policy.lfs_threshold != 0
        || lfs_threshold != 0
        || !policy.lfs_patterns.is_empty()
        || !policy.ignore_patterns.is_empty()
        || !allowed.is_empty()
        || !blocked.is_empty()
    {
        return Err(refused(
            "unsupported effective mode, branch, size, LFS or projection policy",
        ));
    }
    let encoded = serde_json::to_vec(&(
        "pair_refresh_effective_policy_v2",
        "direct_whole_tree_no_lfs",
        policy.auto_merge,
        policy.sync_tags,
    ))
    .map_err(|_| refused("effective policy encoding failed"))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

#[derive(Serialize)]
struct Actor {
    id: String,
    role: String,
    enabled: i64,
    created_at: String,
    updated_at: String,
    session_created_at: String,
    session_expires_at: String,
}

fn actor(tx: &Connection, bearer: &str) -> Result<Actor, DatabaseError> {
    let actor = tx
        .query_row(
            "SELECT u.id,u.role,u.enabled,u.created_at,u.updated_at,s.created_at,s.expires_at
             FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=?1",
            [bearer],
            |row| {
                Ok(Actor {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    enabled: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                    session_created_at: row.get(5)?,
                    session_expires_at: row.get(6)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| refused("current database admin session required"))?;
    // The connection lock and query may wait. Sample expiry only after reading
    // the session, so an old entrance timestamp cannot extend authorization.
    let now = Utc::now();
    let instant = |raw: &str| {
        DateTime::parse_from_rfc3339(raw)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| refused("actor or session timestamp is invalid"))
    };
    let created = instant(&actor.created_at)?;
    let updated = instant(&actor.updated_at)?;
    let session_created = instant(&actor.session_created_at)?;
    let expires = instant(&actor.session_expires_at)?;
    if actor.id.is_empty()
        || actor.role != "admin"
        || actor.enabled != 1
        || created > updated
        || updated > now
        || session_created < created
        || session_created > now
        || expires <= now
        || expires <= session_created
    {
        return Err(refused("current enabled database admin session required"));
    }
    Ok(actor)
}

fn capture_pin(
    db: &Database,
    policy: &SyncConfig,
    allowed: &[String],
    blocked: &[String],
    lfs_threshold: u64,
    bearer: &str,
) -> Result<RefreshActorPolicyPin, DatabaseError> {
    if bearer.is_empty()
        || bearer.len() > 4096
        || bearer
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(refused("current database admin session required"));
    }
    let policy_context_digest = policy_digest(policy, allowed, blocked, lfs_threshold)?;
    db.transaction(|tx| {
        let actor = actor(tx, bearer)?;
        let encoded = serde_json::to_vec(&("pair_refresh_actor_session_v2", bearer, &actor))
            .map_err(|_| refused("actor context encoding failed"))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(
            &crate::db::pair_refresh_credentials::fingerprint_key(tx)?,
        )
        .map_err(|_| refused("invalid actor fingerprint key"))?;
        mac.update(&encoded);
        Ok(RefreshActorPolicyPin {
            version: 2,
            actor_id: actor.id,
            session_context_digest: hex::encode(mac.finalize().into_bytes()),
            policy_context_digest,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Session, User};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    #[test]
    fn a_session_that_expires_while_waiting_for_the_database_is_refused() {
        let db = Arc::new(Database::in_memory().unwrap());
        db.initialize().unwrap();
        db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        let created = (Utc::now() - chrono::Duration::minutes(2)).to_rfc3339();
        db.insert_user(&User {
            id: "admin".into(),
            username: "admin".into(),
            display_name: "Admin".into(),
            email: "admin@example.invalid".into(),
            password_hash: "fixture".into(),
            role: "admin".into(),
            enabled: true,
            created_at: created.clone(),
            updated_at: created,
        })
        .unwrap();
        db.insert_session(&Session {
            token: "disposable-waiting-session".into(),
            user_id: "admin".into(),
            created_at: (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
            expires_at: (Utc::now() + chrono::Duration::milliseconds(200)).to_rfc3339(),
        })
        .unwrap();
        let held = db.conn();
        let other = db.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            capture_pin(
                &other,
                &SyncConfig::default(),
                &[],
                &[],
                0,
                "disposable-waiting-session",
            )
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        drop(held);
        assert!(thread.join().unwrap().is_err());
    }
}
