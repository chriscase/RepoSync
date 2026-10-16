//! Ordinary CLI/Personal admission before checkpoint, migration or credential writes.
use std::path::Path;

use rusqlite::{config::DbConfig, Connection, OpenFlags};

use super::{ordinary_writer_windows, pair_refresh_operations, schema, Database};
use crate::errors::DatabaseError;

pub(super) struct StartupOwner {
    _lock: crate::data_dir::LockGuard,
    fence: crate::writer_fence::RecoveryWriterFence,
    data_dir: std::path::PathBuf,
    lease: String,
    native: Option<(String, String)>,
}

impl StartupOwner {
    pub(super) fn retire(self, db: &Database) {
        if let Err(error) = self.fence.retire_ordinary_startup(
            db,
            &self._lock,
            &self.data_dir,
            &self.lease,
            self.native.as_ref(),
        ) {
            tracing::warn!(%error, "ordinary startup owner retained its durable evidence");
            // An uncertain retirement must retain local exclusion as well as
            // the durable/process evidence. The OS releases this on exit.
            std::mem::forget(self._lock);
        }
    }
}

pub(crate) fn require_quiet(conn: &Connection) -> Result<bool, DatabaseError> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    schema::refuse_future_schema(version)?;
    let has_kv: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='kv_state')",
        [],
        |row| row.get(0),
    )?;
    if !has_kv {
        let has_storage: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%')",
            [],
            |row| row.get(0),
        )?;
        if has_storage || version != 0 {
            return Err(DatabaseError::Other(
                "ordinary startup refuses unknown partially initialized storage".into(),
            ));
        }
        return Ok(false);
    }
    pair_refresh_operations::require_no_authenticated_startup_hold(conn)?;
    pair_refresh_operations::require_no_pair_refresh_hold(conn, None)?;
    ordinary_writer_windows::require_quiet_for_startup(conn)?;
    crate::writer_fence::require_claimable_for_startup(conn)?;
    Ok(true)
}

impl Database {
    /// Open existing storage for inspection without creating it, checkpointing,
    /// migrating, claiming ownership or seeding credentials.
    pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let canonical = path.as_ref().canonicalize()?;
        let path = canonical.as_path();
        if super::startup_rollback::has_journal(path)? || std::fs::metadata(path)?.len() == 0 {
            return Err(DatabaseError::WriterFenced(
                "read-only inspection refuses pending or empty rollback storage".into(),
            ));
        }
        let prior = super::OpenedDatabaseFile::capture(path);
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        schema::refuse_future_schema(version)?;
        let opened_file = super::OpenedDatabaseFile::attest(&conn, path, prior);
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
            opened_file,
            startup_owner: None,
            #[cfg(test)]
            test_force_echo_split_fail: std::sync::Mutex::new(false),
        })
    }

    /// Own a Personal writer's lock and durable epoch for this handle's entire
    /// lifetime, including when it is moved into the Personal sync engine.
    pub fn open_personal_writer(data_dir: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        Self::open_ordinary_writer(data_dir.as_ref(), "personal.db")
    }

    /// Own a team CLI writer's lock and durable epoch until the command and its
    /// engine finish. Read commands should use `open_existing_read_only`.
    pub fn open_cli_writer(data_dir: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        Self::open_ordinary_writer(data_dir.as_ref(), "reposync.db")
    }

    fn open_ordinary_writer(data_dir: &Path, file: &str) -> Result<Self, DatabaseError> {
        std::fs::create_dir_all(data_dir)?;
        let data_dir = data_dir.canonicalize()?;
        let lock = crate::data_dir::acquire(&data_dir)?;
        lock.require_current(&data_dir)?;
        let path = data_dir.join(file);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                use std::os::unix::fs::MetadataExt;
                if !metadata.is_file()
                    || metadata.nlink() != 1
                    || metadata.uid() != rustix::process::geteuid().as_raw()
                {
                    return Err(DatabaseError::Other(
                        "ordinary startup requires an owned singly linked regular database file"
                            .into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let preimage = if super::startup_rollback::has_journal(&path)? {
            // SQLite may unlink an empty main file's journal even on a
            // read-only connection. Inspect raw frozen inputs first.
            Some(super::startup_rollback::require_quiet_preimage(&path)?)
        } else {
            None
        };
        if path.try_exists()? && std::fs::metadata(&path)?.len() != 0 && preimage.is_none() {
            let probe = (|| {
                let probe = Connection::open_with_flags(
                    &path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )?;
                probe.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
                probe.execute_batch("BEGIN;")?;
                require_quiet(&probe).map(|_| ())
            })();
            probe?;
        }

        // Opening this connection cannot checkpoint on error or owner Drop.
        // Existing evidence is rechecked under the SQLite writer reservation.
        let prior = super::OpenedDatabaseFile::capture(&path);
        if let Some(preimage) = &preimage {
            preimage.require_current()?;
        } else if super::startup_rollback::has_journal(&path)? {
            return Err(DatabaseError::WriterFenced(
                "rollback journal appeared after startup inspection".into(),
            ));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        let opened_file =
            super::OpenedDatabaseFile::attest(&conn, &path, prior).ok_or_else(|| {
                DatabaseError::Other("ordinary startup database identity changed".into())
            })?;
        opened_file.require_unaliased_owner(&conn)?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let mut db = Self {
            conn: std::sync::Mutex::new(conn),
            opened_file: Some(opened_file),
            startup_owner: None,
            #[cfg(test)]
            test_force_echo_split_fail: std::sync::Mutex::new(false),
        };
        let claim = crate::writer_fence::claim_for_ordinary_startup(&db, &lock, &data_dir)?;
        db.startup_owner = Some(StartupOwner {
            _lock: lock,
            fence: claim.fence,
            data_dir,
            lease: claim.lease,
            native: claim.native,
        });
        db.conn().execute_batch("PRAGMA journal_mode=WAL;")?;
        db.initialize()?;
        Ok(db)
    }
}
