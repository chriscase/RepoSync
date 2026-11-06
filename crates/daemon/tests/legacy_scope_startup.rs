//! Component regressions for the database entrance used by ordinary daemon startup.
//! The frozen SQL models the recognized old storage shape; these are not an
//! old-executable, scheduler/HTTP, remote-content, or live qualification receipt.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reposync_core::db::import_operations::{resolve_repo_import_baseline, RepoImportBaseline};
use reposync_core::db::Database;
use rusqlite::{params, Connection, OpenFlags};

static SERIAL: Mutex<()> = Mutex::new(());
const TIP: &str = "1111111111111111111111111111111111111111";
const CHILD_DATA: &str = "REPOSYNC_SCOPE_MIGRATION_FAILURE_TEST_CHILD";

struct ChildOwner(Option<Child>);

impl Drop for ChildOwner {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn legacy_store(data: &Path, enabled: bool, status: &str) -> PathBuf {
    std::fs::create_dir_all(data).unwrap();
    let path = data.join("reposync.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("fixtures/schema-v12.sql"))
        .unwrap();
    conn.execute(
        "INSERT INTO repositories
         (id,name,svn_url,created_at,updated_at,last_svn_rev,last_git_sha,last_sync_at,enabled,sync_status)
         VALUES('legacy','Legacy','file:///disposable-not-opened','t','t',42,?1,'t',?2,?3)",
        params![TIP, enabled, status],
    )
    .unwrap();
    for (key, value) in [
        ("last_git_sha_legacy", TIP),
        ("last_svn_rev_legacy", "42"),
        ("fixture-retained-value", "preserve-exactly"),
    ] {
        conn.execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'t')",
            params![key, value],
        )
        .unwrap();
    }
    assert_eq!(version(&conn), 12);
    assert_eq!(scope_columns(&conn), 0);
    path
}

fn version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn scope_columns(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM pragma_table_info('repositories') WHERE name='scope_uuid'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn scope(db: &Database, id: &str) -> String {
    db.conn()
        .query_row(
            "SELECT scope_uuid FROM repositories WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn owned_legacy_startup_upgrades_before_scheduler_baseline() {
    let _serial = SERIAL.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = legacy_store(root.path(), true, "idle");

    // Preserve the existing direct-initialize deferral contract. The owned
    // startup entrance must add the missing precondition for its consumers.
    let direct = Database::new(&path).unwrap();
    direct.initialize().unwrap();
    assert_eq!(version(&direct.conn()), 12);
    drop(direct);

    let (db, owner) = Database::open_with_exclusive_owner(root.path()).unwrap();
    assert_eq!(version(&db.conn()), 13);
    assert_eq!(scope_columns(&db.conn()), 1);
    let first_scope = scope(&db, "legacy");
    assert!(!first_scope.is_empty());
    // The scheduler opens a separate connection after owned daemon startup;
    // it does not initialize or construct an engine before resolving this.
    let scheduler = Database::new(&path).unwrap();
    assert_eq!(version(&scheduler.conn()), 13);
    match resolve_repo_import_baseline(&scheduler, "legacy").unwrap() {
        RepoImportBaseline::Verified { svn_rev, git_sha } => {
            assert_eq!(svn_rev, 42);
            assert_eq!(git_sha, TIP);
        }
        _ => panic!("owned startup did not preserve the verified legacy baseline"),
    }
    assert_eq!(
        db.get_state("fixture-retained-value").unwrap().as_deref(),
        Some("preserve-exactly")
    );
    drop(scheduler);
    drop(db);
    drop(owner);

    let (db, owner) = Database::open_with_exclusive_owner(root.path()).unwrap();
    assert_eq!(scope(&db, "legacy"), first_scope);
    assert_eq!(
        db.get_repository("legacy").unwrap().unwrap().last_git_sha,
        TIP
    );
    drop(db);
    drop(owner);
}

#[test]
fn disabled_and_paused_legacy_rows_allow_registration_before_engine() {
    let _serial = SERIAL.lock().unwrap();
    for (enabled, status) in [(false, "idle"), (true, "error_paused")] {
        let root = tempfile::tempdir().unwrap();
        let path = legacy_store(root.path(), enabled, status);
        let (db, owner) = Database::open_with_exclusive_owner(root.path()).unwrap();
        assert_eq!(version(&db.conn()), 13);
        let registration = Database::new(&path).unwrap();
        let original_scope = scope(&db, "legacy");
        let original = db.get_repository("legacy").unwrap().unwrap();
        assert_eq!(original.enabled, enabled);
        assert_eq!(original.sync_status, status);
        assert_eq!(original.last_svn_rev, 42);
        assert_eq!(original.last_git_sha, TIP);

        // This is the storage operation used by admin registration. No engine
        // is constructed or run to make the scope column available first.
        let mut added = original.clone();
        added.id = "new-registration".into();
        added.name = "New registration".into();
        added.last_svn_rev = 0;
        added.last_git_sha.clear();
        added.last_sync_at = None;
        added.enabled = false;
        added.sync_status = "idle".into();
        added.total_syncs = 0;
        added.total_errors = 0;
        added.consecutive_errors = 0;
        registration.insert_repository(&added).unwrap();
        assert_eq!(scope(&db, "legacy"), original_scope);
        let added_scope = scope(&db, "new-registration");
        assert!(!added_scope.is_empty());
        assert_ne!(original_scope, added_scope);
        let preserved = db.get_repository("legacy").unwrap().unwrap();
        assert_eq!(preserved.enabled, enabled);
        assert_eq!(preserved.sync_status, status);
        assert_eq!(preserved.last_svn_rev, 42);
        assert_eq!(preserved.last_git_sha, TIP);
        drop(registration);
        drop(db);
        drop(owner);
    }
}

#[test]
fn retained_window_refuses_legacy_migration_before_checkpoint() {
    let _serial = SERIAL.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = legacy_store(root.path(), true, "idle");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    let window = db.begin_ordinary_writer_window(None).unwrap();
    let wal = root.path().join("reposync.db-wal");
    let main_before = std::fs::read(&path).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let error = match Database::open_with_exclusive_owner(root.path()) {
        Err(error) => error,
        Ok((opened, owner)) => {
            drop(opened);
            drop(owner);
            panic!("startup accepted an existing ordinary writer window");
        }
    };
    assert!(
        error
            .to_string()
            .contains("another run owns this writer scope"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), main_before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert_eq!(version(&db.conn()), 12);
    assert_eq!(scope_columns(&db.conn()), 0);
    drop(window);
    drop(db);
}

#[test]
fn failed_scope_upgrade_preserves_schema_wal_and_exclusion() {
    let _serial = SERIAL.lock().unwrap();
    if let Some(data) = std::env::var_os(CHILD_DATA) {
        let data = PathBuf::from(data);
        let error = match Database::open_with_exclusive_owner(&data) {
            Err(error) => error,
            Ok((db, owner)) => {
                drop(db);
                drop(owner);
                panic!("injected scope migration failure was ignored");
            }
        };
        assert!(error
            .to_string()
            .contains("injected scope migration failure"));
        assert!(reposync_core::data_dir::acquire(&data).is_err());
        assert!(
            std::fs::metadata(data.join("reposync.db-wal"))
                .unwrap()
                .len()
                > 0
        );
        let db = Database::open_existing_read_only(data.join("reposync.db")).unwrap();
        assert_eq!(version(&db.conn()), 12);
        assert_eq!(scope_columns(&db.conn()), 0);
        assert_eq!(
            db.get_state("last_git_sha_legacy").unwrap().as_deref(),
            Some(TIP)
        );
        let lease = db
            .get_state("writer_fence_v1:lease")
            .unwrap()
            .expect("failed migration must retain the claimed lease");
        // The claim legitimately writes WAL before migration fails. Capture
        // that committed post-failure state immediately before child teardown,
        // so the parent can detect a checkpoint or retirement during exit.
        std::fs::write(data.join("scope-failure-pre-exit-lease.json"), lease).unwrap();
        std::fs::write(
            data.join("scope-failure-pre-exit.wal"),
            std::fs::read(data.join("reposync.db-wal")).unwrap(),
        )
        .unwrap();
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let path = legacy_store(root.path(), true, "idle");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_scope_update BEFORE UPDATE ON repositories
         BEGIN SELECT RAISE(ABORT, 'injected scope migration failure'); END;",
    )
    .unwrap();
    drop(conn);
    let mut child = ChildOwner(Some(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "failed_scope_upgrade_preserves_schema_wal_and_exclusion",
                "--nocapture",
            ])
            .env(CHILD_DATA, root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.0.as_mut().unwrap().kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.0.take().unwrap().wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(root.path().join("reposync.db-wal")).unwrap(),
        std::fs::read(root.path().join("scope-failure-pre-exit.wal")).unwrap(),
        "failed startup teardown checkpointed or changed the committed WAL"
    );
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    assert_eq!(version(&conn), 12);
    assert_eq!(scope_columns(&conn), 0);
    let lease: String = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key='writer_fence_v1:lease'",
            [],
            |row| row.get(0),
        )
        .expect("failed startup teardown retired the claimed lease");
    assert_eq!(
        lease,
        std::fs::read_to_string(root.path().join("scope-failure-pre-exit-lease.json")).unwrap(),
        "failed startup teardown changed the retained lease"
    );
}
