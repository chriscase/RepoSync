//! SQLite persistence layer for RepoSync.
//!
//! Provides a [`Database`] handle with WAL-mode journaling, automatic schema
//! migrations, and query helpers for every table used by the sync engine.

pub mod queries;
pub mod schema;

#[cfg(feature = "reliability-fixture")]
pub mod candidate_authority;
#[cfg(feature = "reliability-fixture")]
pub mod candidate_migration;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;
use tracing::{debug, info};

use crate::errors::DatabaseError;

/// Main database handle wrapping a SQLite connection.
///
/// The connection is opened in WAL mode for concurrent-read performance and
/// uses `PRAGMA foreign_keys = ON`. The inner connection is wrapped in a
/// `Mutex` so that `Database` is `Send + Sync`, enabling use inside `Arc`.
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Open (or create) a SQLite database at `path`.
    ///
    /// The database is configured with WAL journaling mode and foreign key
    /// enforcement immediately after opening.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        info!(path = %path.display(), "opening database");

        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch("PRAGMA busy_timeout = 5000;")?;

        // Checkpoint any pending WAL data on startup to recover from
        // previous unclean shutdowns (SIGKILL, power loss, etc.).
        if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
            tracing::warn!("WAL checkpoint on startup failed: {}", e);
        }

        // Verify database integrity on startup.
        match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
            Ok(ref result) if result == "ok" => {
                debug!("database integrity check passed");
            }
            Ok(result) => {
                tracing::warn!(result = %result, "database integrity check failed");
            }
            Err(e) => {
                tracing::warn!("database integrity check error: {}", e);
            }
        }

        debug!("database opened successfully with WAL mode");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (useful for testing).
    pub fn in_memory() -> Result<Self, DatabaseError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Run all schema migrations to bring the database up to date.
    pub fn initialize(&self) -> Result<(), DatabaseError> {
        info!("initializing database schema");
        let conn = self.conn();
        schema::run_migrations(&conn)?;
        debug!("database schema is up to date");
        Ok(())
    }

    /// Obtain a lock on the underlying connection.
    ///
    /// Prefer using the typed query methods on [`Database`] over raw SQL
    /// whenever possible.
    ///
    /// # Deadlock hazard
    ///
    /// The returned `MutexGuard` holds a `std::sync::Mutex` lock, which is
    /// **NOT re-entrant**. While you hold this guard, you must NOT call any
    /// other `Database` method (e.g. `self.get_repository()`,
    /// `self.list_commit_map()`) because those methods internally call
    /// `self.conn()` — the thread will deadlock permanently, and because
    /// all web requests share this mutex via `validate_session`, the entire
    /// server will freeze.
    ///
    /// If the Mutex is poisoned (a previous holder panicked), the lock is
    /// recovered rather than propagating a panic.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("database mutex was poisoned, recovering");
            poisoned.into_inner()
        })
    }

    /// Execute a closure inside a SQLite transaction. If the closure returns
    /// `Ok`, the transaction is committed; otherwise it is rolled back.
    pub fn transaction<F, T>(&self, f: F) -> Result<T, DatabaseError>
    where
        F: FnOnce(&Connection) -> Result<T, DatabaseError>,
    {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_database() {
        let db = Database::in_memory().expect("failed to create in-memory db");
        db.initialize().expect("failed to initialize schema");
    }

    #[test]
    fn test_file_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = Database::new(&path).expect("failed to create file db");
        db.initialize().expect("failed to initialize schema");
        assert!(path.exists());
    }

    #[test]
    fn test_transaction_commit() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();

        db.transaction(|conn| {
            conn.execute(
                "INSERT INTO watermarks (source, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params!["test", "42", "2025-01-01T00:00:00Z"],
            )?;
            Ok(())
        })
        .unwrap();

        let val: String = db
            .conn()
            .query_row(
                "SELECT value FROM watermarks WHERE source = ?1",
                rusqlite::params!["test"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(val, "42");
    }

    #[test]
    fn test_transaction_rollback() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();

        let result: Result<(), DatabaseError> = db.transaction(|conn| {
            conn.execute(
                "INSERT INTO watermarks (source, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params!["rollback_test", "99", "2025-01-01T00:00:00Z"],
            )?;
            Err(DatabaseError::NotFound {
                entity: "test".into(),
                id: "forced".into(),
            })
        });
        assert!(result.is_err());

        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM watermarks WHERE source = ?1",
                rusqlite::params!["rollback_test"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
