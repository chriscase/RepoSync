//! Select registered read credentials without exposing them in a preview or receipt.
use std::path::Path;

use hmac::{Hmac, Mac};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::historical_enrollment::{
    read_legacy_pair_pins, read_planned_cutover_pins, LegacyPairPins,
};
use super::offline_snapshot;
use crate::errors::DatabaseError;
use crate::git::remote_url::derive_git_remote_url;
use crate::legacy_svn_recovery::{
    verify_registered, HistoricalSvnSource, HistoricalSvnWitness, VerifiedHistoricalLineage,
};

pub struct RegisteredTransport {
    pair: LegacyPairPins,
    git_url: String,
    svn_username: String,
    svn_password: Option<String>,
    git_token: Option<String>,
    context_digest: Option<String>,
}

impl RegisteredTransport {
    /// Use only the endpoints and credentials read from this registration.
    pub async fn verify(
        &self,
        pair: &LegacyPairPins,
        source: HistoricalSvnSource,
        mirror: &Path,
    ) -> anyhow::Result<VerifiedHistoricalLineage> {
        anyhow::ensure!(
            pair == &self.pair,
            "the registered transport belongs to another pair or another observed state"
        );
        let witness = HistoricalSvnWitness {
            source,
            git_mirror: mirror.to_owned(),
            configured_git_url: self.git_url.clone(),
            credential_context_sha256: self.context_digest.clone(),
        };
        verify_registered(
            pair,
            &witness,
            &self.svn_username,
            self.svn_password.as_deref(),
            self.git_token.as_deref(),
        )
        .await
    }
}

fn refuse(message: &str) -> DatabaseError {
    DatabaseError::Other(message.to_owned())
}

// A credential can reach a network transport only for an explicitly registered,
// credential-free HTTPS endpoint. Apply this before reading any secret.
fn network_target(raw: &str) -> Result<bool, DatabaseError> {
    let url = reqwest::Url::parse(raw).map_err(|_| refuse("invalid registered endpoint"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != raw
    {
        return Err(refuse(
            "unsupported or credential-bearing registered endpoint",
        ));
    }
    match url.scheme() {
        "file" if url.host_str().is_none() && url.to_file_path().is_ok() => Ok(false),
        "https" if url.host_str().is_some() => Ok(true),
        _ => Err(refuse(
            "lineage recovery requires a local file endpoint or registered HTTPS endpoint",
        )),
    }
}

struct Secret {
    key: String,
    value: String,
    updated: String,
}

fn root_secret(
    conn: &Connection,
    prefix: &str,
    repo_id: &str,
) -> Result<Option<Secret>, DatabaseError> {
    // Only root registrations are eligible. As in the ordinary resolver, a
    // present per-repository key takes precedence over the installation key.
    let key = format!("{prefix}_{repo_id}");
    let selected = conn
        .query_row(
            "SELECT key,value,updated_at FROM kv_state WHERE key IN (?1,?2)
         ORDER BY CASE key WHEN ?1 THEN 0 ELSE 1 END LIMIT 1",
            [&key, prefix],
            |r| {
                Ok(Secret {
                    key: r.get(0)?,
                    value: r.get(1)?,
                    updated: r.get(2)?,
                })
            },
        )
        .optional()?;
    match selected {
        Some(s) if s.value.is_empty() && s.key == key => {
            Err(refuse("registered read credential was explicitly revoked"))
        }
        Some(s) if s.value.is_empty() => Ok(None),
        s => Ok(s),
    }
}

fn network_context(
    conn: &Connection,
    svn: Option<&Secret>,
    git: Option<&Secret>,
) -> Result<String, DatabaseError> {
    if svn.is_none() && git.is_none() {
        return Ok(hex::encode(Sha256::digest(
            b"reposync-historical-anonymous-read-v1",
        )));
    }
    // The reviewed digest must change on replacement or revocation. An unkeyed
    // password hash in an operator receipt would allow offline guessing.
    let key = super::pair_refresh_credentials::fingerprint_key(conn).map_err(|_| {
        refuse("existing credential review key unavailable for authenticated lineage recovery")
    })?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("SHA256 HMAC accepts a 32-byte key");
    mac.update(b"reposync-historical-registered-read-v1");
    for secret in [svn, git] {
        match secret {
            None => mac.update(&[0]),
            Some(secret) => {
                mac.update(&[1]);
                for field in [&secret.key, &secret.updated, &secret.value] {
                    mac.update(&(field.len() as u64).to_be_bytes());
                    mac.update(field.as_bytes());
                }
            }
        }
    }
    Ok(hex::encode(mac.finalize().into_bytes()))
}

pub(super) fn read_registered_transport(
    conn: &Connection,
    pair: &LegacyPairPins,
) -> Result<RegisteredTransport, DatabaseError> {
    let git_url = derive_git_remote_url(&pair.git_api_url, None, &pair.git_repo);
    let network_svn = network_target(&pair.svn_url)?;
    let network_git = network_target(&git_url)?;
    let svn = if network_svn {
        root_secret(conn, "secret_svn_password", &pair.repo_id)?
    } else {
        None
    };
    let git = if network_git {
        root_secret(conn, "secret_git_token", &pair.repo_id)?
    } else {
        None
    };
    let context_digest = if network_svn || network_git {
        Some(network_context(conn, svn.as_ref(), git.as_ref())?)
    } else {
        None
    };
    Ok(RegisteredTransport {
        pair: pair.clone(),
        git_url,
        svn_username: if network_svn {
            pair.svn_username.clone()
        } else {
            String::new()
        },
        svn_password: svn.map(|s| s.value),
        git_token: git.map(|s| s.value),
        context_digest,
    })
}

/// Read data used for a preview or pre-open digest check on a quiesced database.
pub fn read_offline_recovery_inputs(
    path: &Path,
    repo_id: &str,
    planned: bool,
) -> Result<(LegacyPairPins, RegisteredTransport), DatabaseError> {
    offline_snapshot::with_snapshot(path, |conn| {
        let pins = if planned {
            read_planned_cutover_pins(conn, repo_id)?
        } else {
            read_legacy_pair_pins(conn, repo_id)?
        };
        let transport = read_registered_transport(conn, &pins)?;
        Ok((pins, transport))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> LegacyPairPins {
        LegacyPairPins {
            repo_id: "retained".into(),
            scope_uuid: String::new(),
            generation: 0,
            svn_url: "file:///fixture/svn".into(),
            svn_branch: "trunk".into(),
            svn_username: "configured-user".into(),
            git_provider: "gitea".into(),
            git_api_url: "file:///fixture/git".into(),
            git_repo: "group/team".into(),
            git_branch: "main".into(),
            last_svn_rev: 2,
            last_git_sha: "a".repeat(40),
            mappings: vec![],
            storage_sha256: "a".repeat(64),
        }
    }

    #[tokio::test]
    async fn local_recovery_never_reads_credentials_or_transfers_a_registered_transport() {
        let conn = Connection::open_in_memory().unwrap(); // No credential table.
        let original = pair();
        let transport = read_registered_transport(&conn, &original).unwrap();
        assert!(transport.git_token.is_none() && transport.svn_password.is_none());
        assert!(transport.svn_username.is_empty() && transport.context_digest.is_none());
        let mut other = original;
        other.svn_url = "https://different.invalid/svn".into();
        let result = transport
            .verify(
                &other,
                HistoricalSvnSource::PlannedCutover,
                Path::new("/unused"),
            )
            .await;
        assert!(result.err().unwrap().to_string().contains("another pair"));
    }

    #[test]
    fn only_canonical_registered_https_endpoints_may_receive_secrets() {
        assert!(network_target("https://example.invalid/svn").unwrap());
        for raw in [
            "https://name:password@example.invalid/svn",
            "https://example.invalid/svn?token=test",
            "https://example.invalid/svn#fragment",
            "https://example.invalid/x/../svn",
            "http://example.invalid/svn",
            "svn+ssh://example.invalid/project",
            "file://remote.invalid/svn",
        ] {
            assert!(network_target(raw).is_err());
        }
    }

    #[test]
    fn authenticated_review_binds_the_selected_root_secret_and_its_source() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE kv_state(key TEXT PRIMARY KEY,value TEXT,updated_at TEXT);
            INSERT INTO kv_state VALUES('secret_git_token','test-global','t');
            INSERT INTO kv_state VALUES('encryption_key','0000000000000000000000000000000000000000000000000000000000000000','t');").unwrap();
        let selected = || {
            root_secret(&conn, "secret_git_token", "retained")
                .unwrap()
                .unwrap()
        };
        let digest = || network_context(&conn, None, Some(&selected())).unwrap();
        let global = digest();
        assert_eq!(selected().value, "test-global");
        conn.execute_batch(
            "INSERT INTO kv_state VALUES('secret_git_token_retained','test-global','t')",
        )
        .unwrap();
        assert_ne!(global, digest());
        let scoped = digest();
        conn.execute_batch(
            "UPDATE kv_state SET updated_at='u' WHERE key='secret_git_token_retained'",
        )
        .unwrap();
        assert_ne!(scoped, digest());
        let time = digest();
        conn.execute_batch(
            "UPDATE kv_state SET value='test-rotation' WHERE key='secret_git_token_retained'",
        )
        .unwrap();
        assert_ne!(time, digest());
        conn.execute_batch("UPDATE kv_state SET value='' WHERE key='secret_git_token_retained'")
            .unwrap();
        assert!(root_secret(&conn, "secret_git_token", "retained").is_err());
    }
}
