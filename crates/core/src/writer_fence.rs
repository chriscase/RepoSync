//! Cross-host writer fencing via a durable epoch lease in `kv_state`.
//!
//! A local `flock` on `reposync.lock` blocks a second daemon on the same host,
//! but shared storage can admit two hosts at once. The lease records a
//! monotonic epoch plus holder host/PID; operation journals refuse finalize
//! unless the current process still owns that epoch.
//!
//! ## Cross-host liveness
//!
//! `kill(pid, 0)` is meaningful only when `holder_host` matches this machine.
//! A **local** holder is live while that PID exists; `renewed_at` does not
//! affect local liveness. A **foreign** holder is treated as live until a future
//! heartbeat loop (or explicit break-glass) exists — [`LEASE_TTL_SECS`] is
//! reserved and inert for takeover today. Stale takeover is therefore a dead
//! local PID (or break-glass), never idle TTL expiry of a remote holder.

use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::db::Database;
use crate::errors::DatabaseError;

const LEASE_KEY: &str = "writer_fence_v1:lease";
/// Reserved for a future heartbeat loop. Not consulted for takeover yet.
#[allow(dead_code)]
const LEASE_TTL_SECS: i64 = 300;

static PROCESS_EPOCH: OnceLock<AtomicU64> = OnceLock::new();
static PROCESS_PID: OnceLock<AtomicU32> = OnceLock::new();
static PROCESS_HOST: OnceLock<Mutex<String>> = OnceLock::new();

fn process_epoch() -> &'static AtomicU64 {
    PROCESS_EPOCH.get_or_init(|| AtomicU64::new(0))
}

fn process_pid() -> &'static AtomicU32 {
    PROCESS_PID.get_or_init(|| AtomicU32::new(0))
}

fn process_host() -> &'static Mutex<String> {
    PROCESS_HOST.get_or_init(|| Mutex::new(String::new()))
}

fn set_process_identity(host: &str, pid: u32, epoch: u64) {
    process_epoch().store(epoch, Ordering::Release);
    process_pid().store(pid, Ordering::Release);
    *process_host().lock().unwrap_or_else(|e| e.into_inner()) = host.to_string();
}

fn clear_process_identity() {
    process_epoch().store(0, Ordering::Release);
    process_pid().store(0, Ordering::Release);
    process_host()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

fn current_holder_identity() -> Option<(String, u32)> {
    let epoch = process_epoch().load(Ordering::Acquire);
    if epoch == 0 {
        return None;
    }
    let host = process_host()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let pid = process_pid().load(Ordering::Acquire);
    if host.is_empty() || pid == 0 {
        return None;
    }
    Some((host, pid))
}

/// RAII guard for the durable writer epoch. Drop releases the lease when still current.
#[derive(Debug)]
pub struct WriterFenceGuard {
    epoch: u64,
    host: String,
    pid: u32,
    data_dir: Option<std::path::PathBuf>,
}

impl Drop for WriterFenceGuard {
    fn drop(&mut self) {
        if let Some(dir) = &self.data_dir {
            let db_path = dir.join("reposync.db");
            if let Ok(db) = Database::new(&db_path) {
                let _ = release_lease_if_current(&db, self.epoch, &self.host, self.pid);
            }
        }
        clear_process_identity();
    }
}

impl WriterFenceGuard {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WriterLease {
    version: u8,
    epoch: u64,
    holder_pid: u32,
    holder_host: String,
    acquired_at: String,
    renewed_at: String,
}

fn local_host_id() -> String {
    if let Ok(host) = std::env::var("REPOSYNC_FENCE_HOST") {
        if !host.is_empty() {
            return host;
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| format!("host-{}", std::process::id()))
}

fn parse_lease(raw: &str) -> Result<WriterLease, DatabaseError> {
    let lease: WriterLease = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid writer fence lease: {e}")))?;
    if lease.version != 1 {
        return Err(DatabaseError::Other(
            "unsupported writer fence lease version".into(),
        ));
    }
    Ok(lease)
}

fn read_lease(conn: &Connection) -> Result<Option<WriterLease>, DatabaseError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key=?1",
            [LEASE_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(DatabaseError::from)?;
    raw.map(|v| parse_lease(&v)).transpose()
}

fn write_lease(conn: &Connection, lease: &WriterLease) -> Result<(), DatabaseError> {
    conn.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        rusqlite::params![
            LEASE_KEY,
            serde_json::to_string(lease).unwrap(),
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

fn delete_lease(conn: &Connection, epoch: u64) -> Result<(), DatabaseError> {
    conn.execute(
        "DELETE FROM kv_state WHERE key=?1 AND json_extract(value,'$.epoch')=?2",
        rusqlite::params![LEASE_KEY, epoch],
    )?;
    Ok(())
}

fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: signal 0 probes existence only.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        pid == std::process::id()
    }
}

fn holder_is_local(lease: &WriterLease) -> bool {
    lease.holder_host == local_host_id()
}

/// Whether another claimant must refuse because this holder still owns the lease.
fn lease_is_live(lease: &WriterLease) -> bool {
    if holder_is_local(lease) {
        is_process_alive(lease.holder_pid)
    } else {
        true
    }
}

fn renew_lease_tx(tx: &Connection, lease: &WriterLease) -> Result<(), DatabaseError> {
    write_lease(
        tx,
        &WriterLease {
            version: 1,
            epoch: lease.epoch,
            holder_pid: lease.holder_pid,
            holder_host: lease.holder_host.clone(),
            acquired_at: lease.acquired_at.clone(),
            renewed_at: Utc::now().to_rfc3339(),
        },
    )
}

fn in_use_message(lease: &WriterLease) -> String {
    format!(
        "Another RepoSync writer holds the data directory (host {}, PID {}, epoch {})",
        lease.holder_host, lease.holder_pid, lease.epoch
    )
}

/// Claim or renew the durable writer lease after the local flock is held.
pub fn claim(db: &Database, data_dir: &Path) -> Result<WriterFenceGuard, DatabaseError> {
    claim_with_identity(db, data_dir, &local_host_id(), std::process::id())
}

/// Claim or renew with an explicit holder identity (used by tests).
pub fn claim_with_identity(
    db: &Database,
    data_dir: &Path,
    host: &str,
    pid: u32,
) -> Result<WriterFenceGuard, DatabaseError> {
    let epoch = db.transaction(|tx| claim_lease_tx(tx, host, pid))?;
    set_process_identity(host, pid, epoch);
    info!(host, pid, epoch, "acquired durable writer fence lease");
    Ok(WriterFenceGuard {
        epoch,
        host: host.into(),
        pid,
        data_dir: Some(data_dir.to_path_buf()),
    })
}

fn claim_lease_tx(tx: &Connection, host: &str, pid: u32) -> Result<u64, DatabaseError> {
    let now = Utc::now().to_rfc3339();
    let existing = read_lease(tx)?;
    let next_epoch = existing.as_ref().map(|l| l.epoch + 1).unwrap_or(1);
    if let Some(lease) = existing {
        if lease.holder_host == host && lease.holder_pid == pid {
            let renewed = WriterLease {
                version: 1,
                epoch: lease.epoch,
                holder_pid: pid,
                holder_host: host.into(),
                acquired_at: lease.acquired_at,
                renewed_at: now,
            };
            write_lease(tx, &renewed)?;
            return Ok(lease.epoch);
        }
        if lease_is_live(&lease) {
            return Err(DatabaseError::DataDirInUse(in_use_message(&lease)));
        }
        warn!(
            stale_host = %lease.holder_host,
            stale_pid = lease.holder_pid,
            stale_epoch = lease.epoch,
            "taking over stale writer fence lease"
        );
    }
    let epoch = next_epoch;
    write_lease(
        tx,
        &WriterLease {
            version: 1,
            epoch,
            holder_pid: pid,
            holder_host: host.into(),
            acquired_at: now.clone(),
            renewed_at: now,
        },
    )?;
    Ok(epoch)
}

fn release_lease_if_current(
    db: &Database,
    epoch: u64,
    host: &str,
    pid: u32,
) -> Result<(), DatabaseError> {
    db.transaction(|tx| {
        let lease = read_lease(tx)?;
        if lease
            .as_ref()
            .is_some_and(|l| l.epoch == epoch && l.holder_host == host && l.holder_pid == pid)
        {
            delete_lease(tx, epoch)?;
        }
        Ok(())
    })
}

/// Refuse finalize/commit when this process no longer owns the durable lease.
pub fn require_current(tx: &Connection) -> Result<(), DatabaseError> {
    let lease = read_lease(tx)?;
    let Some(lease) = lease else {
        return Ok(());
    };
    let Some((holder_host, holder_pid)) = current_holder_identity() else {
        return Err(DatabaseError::WriterFenced(format!(
            "writer fence epoch not held locally; active holder is host {} PID {} epoch {}",
            lease.holder_host, lease.holder_pid, lease.epoch
        )));
    };
    let expected = process_epoch().load(Ordering::Acquire);
    if lease.epoch != expected {
        return Err(DatabaseError::WriterFenced(format!(
            "writer fence epoch mismatch: local {} durable {}",
            expected, lease.epoch
        )));
    }
    if lease.holder_host != holder_host || lease.holder_pid != holder_pid {
        return Err(DatabaseError::WriterFenced(
            "writer fence holder identity mismatch".into(),
        ));
    }
    renew_lease_tx(tx, &lease)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct TestCtx {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
        db: Database,
    }

    fn test_ctx() -> TestCtx {
        let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_process_identity();
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("reposync.db")).unwrap();
        db.initialize().unwrap();
        TestCtx {
            _lock: lock,
            dir,
            db,
        }
    }

    fn dead_pid() -> u32 {
        4_194_304_u32
    }

    fn backdate_lease_renewed_at(db: &Database) {
        let raw = db.get_state(LEASE_KEY).unwrap().unwrap();
        let lease = parse_lease(&raw).unwrap();
        let expired = (Utc::now() - chrono::Duration::seconds(LEASE_TTL_SECS + 60)).to_rfc3339();
        let stale = WriterLease {
            version: 1,
            epoch: lease.epoch,
            holder_pid: lease.holder_pid,
            holder_host: lease.holder_host,
            acquired_at: lease.acquired_at,
            renewed_at: expired,
        };
        db.set_state(LEASE_KEY, &serde_json::to_string(&stale).unwrap())
            .unwrap();
    }

    #[test]
    fn live_holder_refuses_second_host_claim() {
        let ctx = test_ctx();
        let _a = claim_with_identity(&ctx.db, ctx.dir.path(), "host-a", 10_001).unwrap();
        let err = claim_with_identity(&ctx.db, ctx.dir.path(), "host-b", 10_002).unwrap_err();
        assert!(matches!(err, DatabaseError::DataDirInUse(_)));
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_REFUSE_SECOND_HOST","host_a_pid":10001,"host_b_pid":10002,"refused":true})
        );
    }

    #[test]
    fn stale_holder_takeover_bumps_epoch() {
        let ctx = test_ctx();
        let local = local_host_id();
        let first = claim_with_identity(&ctx.db, ctx.dir.path(), &local, dead_pid()).unwrap();
        assert_eq!(first.epoch(), 1);
        let second = claim_with_identity(&ctx.db, ctx.dir.path(), "host-b", 10_002).unwrap();
        assert_eq!(second.epoch(), 2);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_STALE_TAKEOVER","dead_local_pid_takeover":true,"epoch":2})
        );
    }

    #[test]
    fn foreign_holder_survives_backdated_renewal_without_takeover() {
        let ctx = test_ctx();
        let _a = claim_with_identity(&ctx.db, ctx.dir.path(), "host-a", 10_001).unwrap();
        backdate_lease_renewed_at(&ctx.db);
        let err = claim_with_identity(&ctx.db, ctx.dir.path(), "host-b", 10_002).unwrap_err();
        assert!(matches!(err, DatabaseError::DataDirInUse(_)));
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_FOREIGN_TTL_INERT","ttl_backdated":true,"refused":true})
        );
    }

    #[test]
    fn unenrolled_writer_refused_when_lease_present() {
        let ctx = test_ctx();
        let _guard = claim_with_identity(&ctx.db, ctx.dir.path(), "host-a", 10_003).unwrap();
        backdate_lease_renewed_at(&ctx.db);
        clear_process_identity();
        let err = ctx.db.transaction(require_current).unwrap_err().to_string();
        assert!(err.contains("not held locally"), "{err}");
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_NO_IDENTITY_REFUSE","refused":true})
        );
    }

    #[test]
    fn superseded_epoch_is_refused_at_finalize() {
        let ctx = test_ctx();
        let local = local_host_id();
        let stale = claim_with_identity(&ctx.db, ctx.dir.path(), &local, dead_pid()).unwrap();
        let _current = claim_with_identity(&ctx.db, ctx.dir.path(), "host-b", 10_006).unwrap();
        set_process_identity(&local, dead_pid(), stale.epoch());
        let err = ctx.db.transaction(require_current).unwrap_err().to_string();
        assert!(err.contains("epoch mismatch"), "{err}");
        clear_process_identity();
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_SUPERSEDED_FINALIZE","refused":true})
        );
    }

    #[test]
    fn fenced_confirm_svn_to_git_push_refuses_superseded_host() {
        use crate::db::git_push_operations::GitPushIntent;

        let ctx = test_ctx();
        ctx.db.conn().execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'old','idle',0,0)",
            [],
        ).unwrap();
        let intent = GitPushIntent {
            repo_id: "pair",
            initiator_id: "worker",
            request_id: "req-fence",
            target_fingerprint: "fp",
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "msg",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: "dddddddddddddddddddddddddddddddddddddddd",
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        };
        let local = local_host_id();
        let stale = claim_with_identity(&ctx.db, ctx.dir.path(), &local, dead_pid()).unwrap();
        let op = ctx.db.begin_svn_to_git_push(intent).unwrap();
        let _current = claim_with_identity(&ctx.db, ctx.dir.path(), "host-b", 10_008).unwrap();
        set_process_identity(&local, dead_pid(), stale.epoch());
        let err = ctx
            .db
            .confirm_svn_to_git_push(
                "pair",
                &op.id,
                "dddddddddddddddddddddddddddddddddddddddd",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("epoch mismatch") || err.contains("WriterFenced"),
            "{err}"
        );
        clear_process_identity();
        let mapped: i64 = ctx
            .db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id='pair'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped, 0);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_SYNC_CONFIRM","sync_records_unchanged":true})
        );
    }

    #[test]
    fn fenced_import_finalize_refuses_superseded_host() {
        use crate::db::import_operations::{import_target_fingerprint, ImportOperationState};

        let ctx = test_ctx();
        ctx.db.conn().execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('repo-f','p','file:///svn','trunk','','local','file:///git','repo','main','team',5,0,0,1,'t','t',0,'','idle',0,0)",
            [],
        ).unwrap();
        let workdir = ctx.dir.path().join("git-repo");
        let repo = ctx.db.get_repository("repo-f").unwrap().unwrap();
        let fp = import_target_fingerprint(&repo, &workdir);
        let local = local_host_id();
        let stale = claim_with_identity(&ctx.db, ctx.dir.path(), &local, dead_pid()).unwrap();
        let op = ctx
            .db
            .create_import_operation("repo-f", "admin", "req-fence", &fp)
            .unwrap();
        ctx.db.start_import_operation("repo-f", &op.id).unwrap();
        let _current = claim_with_identity(&ctx.db, ctx.dir.path(), "host-b", 10_010).unwrap();
        set_process_identity(&local, dead_pid(), stale.epoch());
        let err = ctx
            .db
            .finish_import_operation(
                "repo-f",
                &op.id,
                ImportOperationState::Failed,
                "superseded host",
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("epoch mismatch") || err.contains("WriterFenced"),
            "{err}"
        );
        clear_process_identity();
        assert_eq!(
            ctx.db
                .get_import_operation("repo-f", &op.id)
                .unwrap()
                .unwrap()
                .state,
            ImportOperationState::Running
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_IMPORT_FINALIZE","journal_unchanged":true})
        );
    }

    #[test]
    fn begin_svn_to_git_push_refused_without_fence() {
        use crate::db::git_push_operations::GitPushIntent;

        let ctx = test_ctx();
        ctx.db.conn().execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'old','idle',0,0)",
            [],
        ).unwrap();
        let _holder = claim_with_identity(&ctx.db, ctx.dir.path(), "host-a", 10_011).unwrap();
        clear_process_identity();
        let intent = GitPushIntent {
            repo_id: "pair",
            initiator_id: "worker",
            request_id: "req-begin",
            target_fingerprint: "fp",
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "msg",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: "dddddddddddddddddddddddddddddddddddddddd",
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        };
        let err = ctx
            .db
            .begin_svn_to_git_push(intent)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not held locally") || err.contains("WriterFenced"),
            "{err}"
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_BEGIN_REFUSE","refused":true})
        );
    }

    #[test]
    fn clean_release_allows_immediate_reclaim() {
        let ctx = test_ctx();
        {
            let guard =
                claim_with_identity(&ctx.db, ctx.dir.path(), "host-a", std::process::id()).unwrap();
            assert_eq!(guard.epoch(), 1);
        }
        let again =
            claim_with_identity(&ctx.db, ctx.dir.path(), "host-a", std::process::id()).unwrap();
        assert_eq!(again.epoch(), 1);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64FENC_CLEAN_RELEASE","reclaimed":true})
        );
    }
}
