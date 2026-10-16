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
const NATIVE_LEASE_KEY: &str = "writer_fence_v1:native";
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

/// A recovery server retains its claimed epoch across worker retirement. It
/// never uses ordinary database startup from Drop or silently clears identity.
pub(crate) struct RecoveryWriterFence {
    epoch: u64,
    host: String,
    pid: u32,
}

impl RecoveryWriterFence {
    pub(crate) fn retire_ordinary_startup(
        self,
        db: &Database,
        lock: &crate::data_dir::LockGuard,
        data_dir: &Path,
        original_lease: &str,
        original_native: Option<&(String, String)>,
    ) -> Result<(), DatabaseError> {
        let result = (|| {
            lock.require_current(data_dir)?;
            db.immediate_transaction(|tx| {
                let saved = db.opened_file()?;
                saved.require_unaliased_owner(tx)?;
                let actual = crate::db::opened_file::OpenedDatabaseFile::attest(
                    tx,
                    &saved.path,
                    crate::db::opened_file::OpenedDatabaseFile::capture(&saved.path),
                );
                if actual.as_ref() != Some(saved) {
                    return Err(DatabaseError::WriterFenced(
                        "startup release database identity changed".into(),
                    ));
                }
                crate::db::pair_refresh_operations::require_no_pair_refresh_hold(tx, None)?;
                crate::db::ordinary_writer_windows::require_globally_quiet_for_refresh(tx)?;
                let epoch = require_owned_current_readonly(tx)?;
                let lease = read_lease(tx)?.ok_or_else(|| {
                    DatabaseError::WriterFenced("startup release lost its lease".into())
                })?;
                let native: Option<(String, String)> = tx
                    .query_row(
                        "SELECT value,updated_at FROM kv_state WHERE key=?1",
                        [NATIVE_LEASE_KEY],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if epoch != self.epoch
                    || !same_lease_acquisition(&parse_lease(original_lease)?, &lease)
                    || native.as_ref() != original_native
                {
                    return Err(DatabaseError::WriterFenced(
                        "startup release acquisition evidence changed".into(),
                    ));
                }
                delete_lease(tx, self.epoch)
            })
        })();
        if result.is_ok()
            && process_epoch().load(Ordering::Acquire) == self.epoch
            && current_holder_identity() == Some((self.host.clone(), self.pid))
        {
            clear_process_identity();
        }
        result
    }

    pub(crate) fn require_current(&self, db: &Database) -> Result<(), DatabaseError> {
        let lease = read_lease(&db.conn())?;
        if !lease.is_some_and(|lease| {
            lease.epoch == self.epoch
                && lease.holder_host == self.host
                && lease.holder_pid == self.pid
        }) || process_epoch().load(Ordering::Acquire) != self.epoch
            || current_holder_identity() != Some((self.host.clone(), self.pid))
        {
            return Err(DatabaseError::WriterFenced(
                "recovery server epoch changed".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn release_if_quiet(&self, db: &Database) -> Result<(), DatabaseError> {
        db.immediate_transaction(|tx| {
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(tx, None)?;
            let lease = read_lease(tx)?;
            if !lease.is_some_and(|lease| {
                lease.epoch == self.epoch
                    && lease.holder_host == self.host
                    && lease.holder_pid == self.pid
            }) {
                return Err(DatabaseError::WriterFenced(
                    "recovery release lost its epoch".into(),
                ));
            }
            delete_lease(tx, self.epoch)
        })?;
        if process_epoch().load(Ordering::Acquire) == self.epoch
            && current_holder_identity() == Some((self.host.clone(), self.pid))
        {
            clear_process_identity();
        }
        Ok(())
    }
}

impl WriterFenceGuard {
    pub(crate) fn into_recovery_lifetime(self) -> RecoveryWriterFence {
        let mut guard = std::mem::ManuallyDrop::new(self);
        // Move the allocation out without running the ordinary Drop path.
        let host = std::mem::take(&mut guard.host);
        drop(guard.data_dir.take());
        RecoveryWriterFence {
            epoch: guard.epoch,
            host,
            pid: guard.pid,
        }
    }

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

/// Captured by the process that writes the lease, in the same transaction.
/// Keeping this separate preserves legacy lease encoding and takeover policy.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeLease {
    lease: WriterLease,
    owner: crate::pair_refresh::native_owner::NativeProcessOwner,
}

fn same_lease_acquisition(captured: &WriterLease, current: &WriterLease) -> bool {
    captured.version == current.version
        && captured.epoch == current.epoch
        && captured.holder_pid == current.holder_pid
        && captured.holder_host == current.holder_host
        && captured.acquired_at == current.acquired_at
}

fn native_lease_matches(captured: &WriterLease, current: &WriterLease) -> bool {
    if !same_lease_acquisition(captured, current) {
        return false;
    }
    if captured.renewed_at == current.renewed_at {
        return true;
    }
    // A heartbeat can advance only the ordinary lease's observation time. It
    // cannot change which acquisition or native process owns that epoch.
    let parse = chrono::DateTime::parse_from_rfc3339;
    match (
        parse(&captured.acquired_at),
        parse(&captured.renewed_at),
        parse(&current.renewed_at),
    ) {
        (Ok(acquired), Ok(recorded), Ok(renewed)) => acquired <= recorded && recorded <= renewed,
        _ => false,
    }
}

#[cfg(test)]
fn local_host_id() -> String {
    ordinary_host_id().expect("test process requires a stable native hostname")
}

fn ordinary_host_id() -> Result<String, DatabaseError> {
    if let Ok(host) = std::env::var("REPOSYNC_FENCE_HOST") {
        if !host.is_empty() {
            return Ok(host);
        }
    }
    let mut name = [0u8; 256];
    // SAFETY: gethostname writes at most this buffer's length; a missing
    // terminator or invalid text remains unknown instead of a PID fallback.
    let result = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) };
    if result == 0 {
        if let Some(end) = name
            .iter()
            .position(|byte| *byte == 0)
            .filter(|end| *end != 0)
        {
            if let Ok(host) = std::str::from_utf8(&name[..end]) {
                return Ok(host.to_owned());
            }
        }
    }
    Err(DatabaseError::WriterFenced(
        "ordinary startup requires a stable native hostname".into(),
    ))
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
    write_lease_requiring_owner(conn, lease, None)
}

fn write_lease_requiring_owner(
    conn: &Connection,
    lease: &WriterLease,
    required: Option<&crate::pair_refresh::native_owner::NativeProcessOwner>,
) -> Result<(), DatabaseError> {
    let native = (lease.holder_pid == std::process::id())
        .then(crate::pair_refresh::native_owner::NativeProcessOwner::capture)
        .flatten();
    store_lease_with_native(conn, lease, native, required)
}

fn store_lease_with_native(
    conn: &Connection,
    lease: &WriterLease,
    native: Option<crate::pair_refresh::native_owner::NativeProcessOwner>,
    required: Option<&crate::pair_refresh::native_owner::NativeProcessOwner>,
) -> Result<(), DatabaseError> {
    let prior = read_lease(conn)?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key=?1",
            [NATIVE_LEASE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let previous_native: Option<NativeLease> = raw
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| DatabaseError::WriterFenced("invalid native lease evidence".into()))?;
    // Renewal cannot turn an unknown acquisition into native proof. Only a
    // new epoch may capture its first owner; existing metadata may be renewed
    // only by recapturing that same owner below.
    let native = if prior.as_ref().is_some_and(|old| old.epoch == lease.epoch)
        && previous_native.is_none()
    {
        None
    } else {
        native
    };
    if let Some(previous) = &previous_native {
        if !prior
            .as_ref()
            .is_some_and(|prior| native_lease_matches(&previous.lease, prior))
            || previous.owner.pid() != previous.lease.holder_pid
            || native.is_none()
            || (previous.lease.epoch == lease.epoch
                && (!native_lease_matches(&previous.lease, lease)
                    || native.as_ref() != Some(&previous.owner)))
        {
            return Err(DatabaseError::WriterFenced(
                "native lease acquisition identity changed or became unknown".into(),
            ));
        }
    }
    if required.is_some_and(|owner| native.as_ref() != Some(owner))
        || (lease.holder_host.starts_with("pair-refresh-native-") && native.is_none())
    {
        return Err(DatabaseError::WriterFenced(
            "native recovery claim requires its freshly proved current owner".into(),
        ));
    }
    // Validate before either KV row changes. Callers reserve/own the enclosing
    // transaction, so the lease and its original acquisition identity survive
    // a failed recapture together.
    conn.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        rusqlite::params![
            LEASE_KEY,
            serde_json::to_string(lease).unwrap(),
            Utc::now().to_rfc3339()
        ],
    )?;
    if previous_native
        .as_ref()
        .is_some_and(|previous| previous.lease.epoch == lease.epoch)
    {
        // Acquisition evidence is immutable for this epoch. Validation above
        // still recaptures the actual owner before the ordinary lease changes.
        return Ok(());
    }
    if let Some(owner) = native {
        let metadata = NativeLease {
            lease: lease.clone(),
            owner,
        };
        let raw = serde_json::to_string(&metadata)
            .map_err(|error| DatabaseError::Other(error.to_string()))?;
        conn.execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            rusqlite::params![NATIVE_LEASE_KEY, raw, Utc::now().to_rfc3339()],
        )?;
    } else {
        conn.execute("DELETE FROM kv_state WHERE key=?1", [NATIVE_LEASE_KEY])?;
    }
    Ok(())
}

fn delete_lease(conn: &Connection, epoch: u64) -> Result<(), DatabaseError> {
    let deleted = conn.execute(
        "DELETE FROM kv_state WHERE key=?1 AND json_extract(value,'$.epoch')=?2",
        rusqlite::params![LEASE_KEY, epoch],
    )?;
    if deleted != 0 {
        conn.execute("DELETE FROM kv_state WHERE key=?1", [NATIVE_LEASE_KEY])?;
    }
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
    ordinary_host_id().is_ok_and(|host| lease.holder_host == host)
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
    claim_with_identity(db, data_dir, &ordinary_host_id()?, std::process::id())
}

/// Startup checks this through a read-only connection and again under its
/// SQLite writer reservation. A live or unknown foreign holder is never stolen.
pub(crate) fn require_claimable_for_startup(tx: &Connection) -> Result<(), DatabaseError> {
    if let Some(lease) = read_lease(tx)? {
        if lease.epoch == 0 || lease.holder_pid == 0 || lease.holder_host.is_empty() {
            return Err(DatabaseError::WriterFenced(
                "invalid startup writer lease".into(),
            ));
        }
        if lease_is_live(&lease) {
            return Err(DatabaseError::DataDirInUse(in_use_message(&lease)));
        }
    } else {
        let orphan: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key=?1)",
            [NATIVE_LEASE_KEY],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(DatabaseError::WriterFenced(
                "orphan native lease evidence".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) struct OrdinaryStartupClaim {
    pub fence: RecoveryWriterFence,
    pub lease: String,
    pub native: Option<(String, String)>,
}

pub(crate) fn claim_for_ordinary_startup(
    db: &Database,
    lock: &crate::data_dir::LockGuard,
    data_dir: &Path,
) -> Result<OrdinaryStartupClaim, DatabaseError> {
    let host = ordinary_host_id()?;
    let pid = std::process::id();
    let (epoch, lease, native) = db.immediate_transaction(|tx| {
        lock.require_current(data_dir)?;
        let saved = db.opened_file()?;
        saved.require_unaliased_owner(tx)?;
        let actual = crate::db::opened_file::OpenedDatabaseFile::attest(
            tx,
            &saved.path,
            crate::db::opened_file::OpenedDatabaseFile::capture(&saved.path),
        );
        if actual.as_ref() != Some(saved) {
            return Err(DatabaseError::WriterFenced(
                "startup claim database identity changed".into(),
            ));
        }
        if !crate::db::startup::require_quiet(tx)? {
            crate::db::schema::initialize_empty_startup_in_transaction(tx)?;
        }
        let epoch = claim_lease_tx(tx, &host, pid)?;
        let lease = tx.query_row(
            "SELECT value FROM kv_state WHERE key=?1",
            [LEASE_KEY],
            |row| row.get(0),
        )?;
        let native = tx
            .query_row(
                "SELECT value,updated_at FROM kv_state WHERE key=?1",
                [NATIVE_LEASE_KEY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok((epoch, lease, native))
    })?;
    set_process_identity(&host, pid, epoch);
    Ok(OrdinaryStartupClaim {
        fence: RecoveryWriterFence { epoch, host, pid },
        lease,
        native,
    })
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
    crate::db::pair_refresh_operations::require_no_authenticated_startup_hold(tx)?;
    let floor = crate::db::pair_refresh_operations::authenticated_refresh_epoch_floor(tx)?;
    let previous = existing_epoch_floor(tx)?.max(floor);
    let epoch = previous
        .checked_add(1)
        .ok_or_else(|| DatabaseError::WriterFenced("writer epoch exhausted".into()))?;
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

fn existing_epoch_floor(tx: &Connection) -> Result<u64, DatabaseError> {
    Ok(read_lease(tx)?.map_or(0, |lease| lease.epoch))
}

/// Only the authenticated job reader can supply the signed predecessor and
/// fresh native proof. This does not change ordinary foreign-holder policy.
pub(crate) fn claim_authenticated_recovery_tx(
    tx: &Connection,
    previous_epoch: u64,
    proof: &crate::pair_refresh::native_owner::NativeDeadOwnerProof,
) -> Result<(u64, String), DatabaseError> {
    proof.require_current()?;
    let host = proof.fence_host();
    let floor = crate::db::pair_refresh_operations::authenticated_refresh_epoch_floor(tx)?;
    let existing = read_lease(tx)?;
    let lease_proof = if let Some(lease) = &existing {
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [NATIVE_LEASE_KEY],
                |row| row.get(0),
            )
            .optional()?;
        let native: NativeLease = serde_json::from_str(raw.as_deref().ok_or_else(|| {
            DatabaseError::WriterFenced("recovery lease has no native acquisition evidence".into())
        })?)
        .map_err(|_| DatabaseError::WriterFenced("invalid native lease evidence".into()))?;
        if !native_lease_matches(&native.lease, lease)
            || native.owner.pid() != lease.holder_pid
            || lease.epoch < previous_epoch
            || (lease.epoch == previous_epoch && native.owner != *proof.old())
            || (lease.epoch > previous_epoch && lease.epoch <= floor)
        {
            return Err(DatabaseError::WriterFenced(
                "recovery lease differs from native acquisition evidence".into(),
            ));
        }
        Some(crate::pair_refresh::native_owner::NativeDeadOwnerProof::capture(Some(&native.owner))?)
    } else {
        let orphan: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key=?1)",
            [NATIVE_LEASE_KEY],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(DatabaseError::WriterFenced(
                "orphan native lease evidence".into(),
            ));
        }
        None
    };
    let epoch = previous_epoch
        .max(floor)
        .max(existing.as_ref().map_or(0, |lease| lease.epoch))
        .checked_add(1)
        .ok_or_else(|| DatabaseError::WriterFenced("writer epoch exhausted".into()))?;
    let now = Utc::now().to_rfc3339();
    proof.require_current()?;
    if let Some(lease_proof) = &lease_proof {
        lease_proof.require_current()?;
    }
    write_lease_requiring_owner(
        tx,
        &WriterLease {
            version: 1,
            epoch,
            holder_pid: std::process::id(),
            holder_host: host.clone(),
            acquired_at: now.clone(),
            renewed_at: now,
        },
        Some(proof.current()),
    )?;
    Ok((epoch, host))
}

pub(crate) fn finish_authenticated_recovery_claim(
    data_dir: &Path,
    epoch: u64,
    host: String,
) -> WriterFenceGuard {
    let pid = std::process::id();
    set_process_identity(&host, pid, epoch);
    WriterFenceGuard {
        epoch,
        host,
        pid,
        data_dir: Some(data_dir.to_path_buf()),
    }
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
    require_current_holder(&lease)?;
    renew_lease_tx(tx, &lease)?;
    Ok(())
}

fn require_current_holder(lease: &WriterLease) -> Result<(), DatabaseError> {
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
    Ok(())
}

/// An idempotent admission may return an existing record without renewing any
/// durable evidence. Recheck the actual holder and native acquisition just as
/// renewal would; this grants no new epoch or publication authority.
pub(crate) fn require_owned_current_readonly(tx: &Connection) -> Result<u64, DatabaseError> {
    let lease = read_lease(tx)?.ok_or_else(|| {
        DatabaseError::WriterFenced("pair refresh requires an enrolled writer lease".into())
    })?;
    if lease.epoch == 0 || lease.holder_pid == 0 || lease.holder_host.is_empty() {
        return Err(DatabaseError::WriterFenced(
            "pair refresh requires a valid writer lease".into(),
        ));
    }
    require_current_holder(&lease)?;
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM kv_state WHERE key=?1",
            [NATIVE_LEASE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(raw) = raw {
        let native: NativeLease = serde_json::from_str(&raw)
            .map_err(|_| DatabaseError::WriterFenced("invalid native lease evidence".into()))?;
        if !native_lease_matches(&native.lease, &lease)
            || native.owner.pid() != lease.holder_pid
            || crate::pair_refresh::native_owner::NativeProcessOwner::capture().as_ref()
                != Some(&native.owner)
        {
            return Err(DatabaseError::WriterFenced(
                "native lease acquisition identity changed or became unknown".into(),
            ));
        }
    } else if lease.holder_host.starts_with("pair-refresh-native-") {
        return Err(DatabaseError::WriterFenced(
            "native recovery claim has no acquisition evidence".into(),
        ));
    }
    Ok(lease.epoch)
}

/// Require an enrolled current daemon writer for a new pair-refresh job.
/// The ordinary journal compatibility behavior when no lease exists is not an
/// admission fence for a coordinated write to two registered repositories.
pub fn require_owned_current(tx: &Connection) -> Result<u64, DatabaseError> {
    let lease = read_lease(tx)?.ok_or_else(|| {
        DatabaseError::WriterFenced("pair refresh requires an enrolled writer lease".into())
    })?;
    if lease.epoch == 0 || lease.holder_pid == 0 || lease.holder_host.is_empty() {
        return Err(DatabaseError::WriterFenced(
            "pair refresh requires a valid writer lease".into(),
        ));
    }
    require_current(tx)?;
    Ok(lease.epoch)
}

/// Preserve ordinary-writer no-lease compatibility while binding an in-flight
/// capability to the lease epoch whenever this database is enrolled.
pub fn ordinary_current_epoch(tx: &Connection) -> Result<Option<u64>, DatabaseError> {
    require_current(tx)?;
    Ok(read_lease(tx)?.map(|lease| lease.epoch))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

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

#[cfg(test)]
mod native_lease_transition_tests {
    use super::*;
    use crate::pair_refresh::native_owner::NativeProcessOwner;

    fn owner(domain: &str) -> NativeProcessOwner {
        serde_json::from_value(serde_json::json!({
            "version":1,"platform":"linux","host_digest":domain.repeat(64),
            "boot_digest":"b".repeat(64),"pid_namespace":"1:2",
            "pid":std::process::id(),"uid":rustix::process::geteuid().as_raw()
        }))
        .unwrap()
    }
    fn lease(epoch: u64, host: &str) -> WriterLease {
        WriterLease {
            version: 1,
            epoch,
            holder_pid: std::process::id(),
            holder_host: host.into(),
            acquired_at: "2026-10-13T00:00:00Z".into(),
            renewed_at: "2026-10-13T00:00:00Z".into(),
        }
    }
    fn state(db: &Database) -> (Option<String>, Option<String>) {
        (
            db.get_state(LEASE_KEY).unwrap(),
            db.get_state(NATIVE_LEASE_KEY).unwrap(),
        )
    }
    fn database() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("reposync.db")).unwrap();
        db.initialize().unwrap();
        (dir, db)
    }

    #[test]
    fn same_epoch_native_renewal_preserves_original_domain_and_unknown_evidence() {
        let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, db) = database();
        let original = lease(1, "shared");
        let a = owner("a");
        db.immediate_transaction(|tx| {
            store_lease_with_native(tx, &original, Some(a.clone()), None)
        })
        .unwrap();
        let before = state(&db);
        let mut renewed = original.clone();
        renewed.renewed_at = "2026-10-13T00:00:01Z".into();
        for candidate in [Some(owner("c")), None] {
            assert!(db
                .immediate_transaction(|tx| store_lease_with_native(tx, &renewed, candidate, None))
                .is_err());
            assert_eq!(state(&db), before);
        }
        db.immediate_transaction(|tx| store_lease_with_native(tx, &renewed, Some(a.clone()), None))
            .unwrap();
        let stored: NativeLease =
            serde_json::from_str(&db.get_state(NATIVE_LEASE_KEY).unwrap().unwrap()).unwrap();
        assert_eq!(stored.lease, original);
        assert_eq!(stored.owner, a);
        assert_eq!(db.get_state(NATIVE_LEASE_KEY).unwrap(), before.1);
        assert_eq!(read_lease(&db.conn()).unwrap(), Some(renewed));
    }

    #[test]
    fn recovery_claim_requires_exact_recaptured_owner_and_retains_prior_rows() {
        let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, db) = database();
        let a = owner("a");
        let claim = lease(2, "pair-refresh-native-test");
        for candidate in [None, Some(owner("c"))] {
            assert!(db
                .immediate_transaction(|tx| store_lease_with_native(
                    tx,
                    &claim,
                    candidate,
                    Some(&a)
                ))
                .is_err());
            assert_eq!(state(&db), (None, None));
        }
        db.immediate_transaction(|tx| {
            store_lease_with_native(tx, &claim, Some(a.clone()), Some(&a))
        })
        .unwrap();
        let before = state(&db);
        let later = lease(3, "pair-refresh-native-test");
        assert!(db
            .immediate_transaction(|tx| store_lease_with_native(tx, &later, None, Some(&a)))
            .is_err());
        assert_eq!(state(&db), before);
        let mut renewed = claim.clone();
        renewed.renewed_at = "2026-10-13T00:00:01Z".into();
        assert!(db
            .immediate_transaction(|tx| store_lease_with_native(tx, &renewed, None, None))
            .is_err());
        assert_eq!(state(&db), before);
    }

    #[test]
    fn public_claim_cannot_relabel_foreign_native_owner_with_same_host_and_pid() {
        let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (dir, db) = database();
        let original = lease(1, "shared");
        let mut foreign = owner("a");
        if NativeProcessOwner::capture().as_ref() == Some(&foreign) {
            foreign = owner("c");
        }
        db.immediate_transaction(|tx| store_lease_with_native(tx, &original, Some(foreign), None))
            .unwrap();
        let before = state(&db);
        assert!(claim_with_identity(&db, dir.path(), "shared", std::process::id()).is_err());
        assert_eq!(state(&db), before);
    }
}

#[cfg(test)]
#[test]
fn legacy_native_unknown_stays_unknown_until_a_new_epoch_is_acquired() {
    let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let original = WriterLease {
        version: 1,
        epoch: 1,
        holder_pid: std::process::id(),
        holder_host: "shared".into(),
        acquired_at: "original".into(),
        renewed_at: "original".into(),
    };
    let current: crate::pair_refresh::native_owner::NativeProcessOwner =
        serde_json::from_value(serde_json::json!({"version":1,"platform":"linux",
            "host_digest":"a".repeat(64),"boot_digest":"b".repeat(64),
            "pid_namespace":"1:2","pid":std::process::id(),"uid":rustix::process::geteuid().as_raw()})).unwrap();
    db.immediate_transaction(|tx| store_lease_with_native(tx, &original, None, None))
        .unwrap();
    let before = db.get_state(LEASE_KEY).unwrap();
    let mut renewal = original.clone();
    renewal.renewed_at = "renewed".into();
    assert!(db
        .immediate_transaction(|tx| store_lease_with_native(
            tx,
            &renewal,
            Some(current.clone()),
            Some(&current)
        ))
        .is_err());
    assert_eq!(db.get_state(LEASE_KEY).unwrap(), before);
    assert!(db.get_state(NATIVE_LEASE_KEY).unwrap().is_none());
    db.immediate_transaction(|tx| {
        store_lease_with_native(tx, &renewal, Some(current.clone()), None)
    })
    .unwrap();
    assert_eq!(read_lease(&db.conn()).unwrap(), Some(renewal.clone()));
    assert!(db.get_state(NATIVE_LEASE_KEY).unwrap().is_none());
    db.immediate_transaction(|tx| store_lease_with_native(tx, &renewal, None, None))
        .unwrap();
    assert!(db.get_state(NATIVE_LEASE_KEY).unwrap().is_none());
    let mut acquired = renewal;
    acquired.epoch = 2;
    db.immediate_transaction(|tx| {
        store_lease_with_native(tx, &acquired, Some(current.clone()), Some(&current))
    })
    .unwrap();
    let stored: NativeLease =
        serde_json::from_str(&db.get_state(NATIVE_LEASE_KEY).unwrap().unwrap()).unwrap();
    assert_eq!(stored.lease, acquired);
    assert_eq!(stored.owner, current);
}

#[cfg(test)]
#[test]
fn owned_readonly_fence_check_preserves_all_rows_and_rejects_unknown_native_evidence() {
    let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let _owner = claim(&db, dir.path()).unwrap();
    let rows = || {
        let conn = db.conn();
        let mut query = conn
            .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
            .unwrap();
        query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = rows();
    assert_eq!(
        db.immediate_transaction(require_owned_current_readonly)
            .unwrap(),
        _owner.epoch()
    );
    assert_eq!(rows(), before);
    db.set_state(NATIVE_LEASE_KEY, "unknown-native-evidence")
        .unwrap();
    let corrupt = rows();
    assert!(db
        .immediate_transaction(require_owned_current_readonly)
        .is_err());
    assert_eq!(rows(), corrupt);
}

#[cfg(test)]
#[test]
fn native_acquisition_bytes_survive_heartbeats_and_still_bind_the_current_lease() {
    let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let owner: crate::pair_refresh::native_owner::NativeProcessOwner =
        serde_json::from_value(serde_json::json!({
            "version":1,"platform":"linux","host_digest":"a".repeat(64),
            "boot_digest":"b".repeat(64),"pid_namespace":"1:2",
            "pid":std::process::id(),"uid":rustix::process::geteuid().as_raw()
        }))
        .unwrap();
    let original = WriterLease {
        version: 1,
        epoch: 1,
        holder_pid: std::process::id(),
        holder_host: "fixture".into(),
        acquired_at: "2026-10-13T00:00:00Z".into(),
        renewed_at: "2026-10-13T00:00:00Z".into(),
    };
    db.immediate_transaction(|tx| {
        store_lease_with_native(tx, &original, Some(owner.clone()), None)
    })
    .unwrap();
    let native_row = || {
        db.conn()
            .query_row(
                "SELECT value,updated_at FROM kv_state WHERE key=?1",
                [NATIVE_LEASE_KEY],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap()
    };
    let acquisition = native_row();
    for at in ["2026-10-13T00:00:01Z", "2026-10-13T00:00:02Z"] {
        let renewed = WriterLease {
            renewed_at: at.into(),
            ..original.clone()
        };
        db.immediate_transaction(|tx| {
            store_lease_with_native(tx, &renewed, Some(owner.clone()), None)
        })
        .unwrap();
        assert_eq!(read_lease(&db.conn()).unwrap(), Some(renewed.clone()));
        assert_eq!(native_row(), acquisition);
        assert!(native_lease_matches(&original, &renewed));
    }
    let mut changed = original.clone();
    changed.holder_host = "another-acquisition".into();
    assert!(!native_lease_matches(&original, &changed));
    assert!(db
        .immediate_transaction(|tx| store_lease_with_native(
            tx,
            &changed,
            Some(owner.clone()),
            None
        ))
        .is_err());
    assert_eq!(native_row(), acquisition);
    let renewed = read_lease(&db.conn()).unwrap().unwrap();
    assert!(db
        .immediate_transaction(|tx| store_lease_with_native(tx, &renewed, None, None))
        .is_err());
    assert_eq!(native_row(), acquisition);
    assert_eq!(read_lease(&db.conn()).unwrap(), Some(renewed));
    let mut invalid_capture = original.clone();
    invalid_capture.renewed_at = "foreign-renewal".into();
    assert!(!native_lease_matches(&invalid_capture, &original));
    changed.epoch = 2;
    db.immediate_transaction(|tx| {
        store_lease_with_native(tx, &changed, Some(owner.clone()), Some(&owner))
    })
    .unwrap();
    assert_ne!(native_row(), acquisition);
    let captured: NativeLease = serde_json::from_str(&native_row().0).unwrap();
    assert_eq!(captured.lease, changed);
    assert_eq!(captured.owner, owner);
}

#[cfg(test)]
#[test]
fn backward_native_heartbeat_refuses_before_any_row_changes_and_forward_time_recovers() {
    let _serial = tests::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let owner: crate::pair_refresh::native_owner::NativeProcessOwner =
        serde_json::from_value(serde_json::json!({
            "version":1,"platform":"linux","host_digest":"a".repeat(64),
            "boot_digest":"b".repeat(64),"pid_namespace":"1:2",
            "pid":std::process::id(),"uid":rustix::process::geteuid().as_raw()
        }))
        .unwrap();
    let acquired = WriterLease {
        version: 1,
        epoch: 1,
        holder_pid: std::process::id(),
        holder_host: "clock-rollback-fixture".into(),
        acquired_at: "2026-10-13T00:00:00Z".into(),
        renewed_at: "2026-10-13T00:00:00Z".into(),
    };
    db.immediate_transaction(|tx| {
        store_lease_with_native(tx, &acquired, Some(owner.clone()), None)
    })
    .unwrap();
    let rows = || {
        db.conn()
            .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = rows();
    for at in ["2026-10-12T23:59:59Z", "unknown-time"] {
        let outgoing = WriterLease {
            renewed_at: at.into(),
            ..acquired.clone()
        };
        assert!(db
            .immediate_transaction(|tx| {
                store_lease_with_native(tx, &outgoing, Some(owner.clone()), None)
            })
            .is_err());
        assert_eq!(rows(), before);
    }
    let forward = WriterLease {
        renewed_at: "2026-10-13T00:00:01Z".into(),
        ..acquired.clone()
    };
    db.immediate_transaction(|tx| store_lease_with_native(tx, &forward, Some(owner.clone()), None))
        .unwrap();
    assert_eq!(read_lease(&db.conn()).unwrap(), Some(forward.clone()));
    assert!(native_lease_matches(&acquired, &forward));
    assert_eq!(
        rows()
            .into_iter()
            .filter(|r| r.0 == NATIVE_LEASE_KEY)
            .collect::<Vec<_>>(),
        before
            .into_iter()
            .filter(|r| r.0 == NATIVE_LEASE_KEY)
            .collect::<Vec<_>>()
    );
}
