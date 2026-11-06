//! Owned startup and inspection have distinct storage boundaries.
use reposync_core::db::Database;
use std::path::Path;

fn open(
    directory: &Path,
    personal: bool,
) -> Result<Database, reposync_core::errors::DatabaseError> {
    if personal {
        Database::open_personal_writer(directory)
    } else {
        Database::open_cli_writer(directory)
    }
}

#[test]
fn owned_startup_initializes_and_holds_both_mode_entrances_until_last_database_drop() {
    for personal in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let name = if personal {
            "personal.db"
        } else {
            "reposync.db"
        };
        let path = directory.path().join(name);
        let db = open(directory.path(), personal).unwrap();
        assert!(db.get_repository("absent").unwrap().is_none());
        assert!(db.get_state("writer_fence_v1:lease").unwrap().is_some());
        db.set_state("preserve", "completed-command").unwrap();
        let db = std::sync::Arc::new(db);
        let engine_reference = db.clone();
        drop(db);
        assert!(open(directory.path(), personal).is_err());
        assert!(open(directory.path(), !personal).is_err());
        assert!(!directory
            .path()
            .join(if personal {
                "reposync.db"
            } else {
                "personal.db"
            })
            .exists());
        drop(engine_reference);
        let inspected = Database::open_existing_read_only(&path).unwrap();
        assert_eq!(
            inspected.get_state("preserve").unwrap().as_deref(),
            Some("completed-command")
        );
        assert!(inspected
            .get_state("writer_fence_v1:lease")
            .unwrap()
            .is_none());
        assert!(inspected
            .get_state("writer_fence_v1:native")
            .unwrap()
            .is_none());
        drop(inspected);
        let restarted = open(directory.path(), personal).unwrap();
        assert_eq!(
            restarted.get_state("preserve").unwrap().as_deref(),
            Some("completed-command")
        );
    }
}

#[test]
fn startup_refuses_preparing_and_uncertain_windows_before_any_checkpoint() {
    for personal in [false, true] {
        for uncertain in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(if personal {
                "personal.db"
            } else {
                "reposync.db"
            });
            let db = Database::new(&path).unwrap();
            db.initialize().unwrap();
            db.conn()
                .execute_batch("PRAGMA wal_autocheckpoint=0;")
                .unwrap();
            let window = db.begin_ordinary_writer_window(None).unwrap();
            if uncertain {
                window.mark_unjournalled_publication_possible().unwrap();
            }
            let main = std::fs::read(&path).unwrap();
            let wal_path = path.with_extension("db-wal");
            let wal = std::fs::read(&wal_path).unwrap();
            let error = open(directory.path(), personal).err().unwrap();
            assert!(
                error
                    .to_string()
                    .contains("another run owns this writer scope"),
                "{error}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), main);
            assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
            assert!(db.get_state("writer_fence_v1:lease").unwrap().is_none());
        }
    }
}

#[test]
fn readonly_inspection_preserves_committed_wal_and_never_creates_missing_storage() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.db");
    assert!(Database::open_existing_read_only(&missing).is_err());
    assert!(!missing.exists());
    let path = directory.path().join("personal.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    db.conn()
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    db.set_state("marker", "committed-in-wal").unwrap();
    let main = std::fs::read(&path).unwrap();
    let wal_path = path.with_extension("db-wal");
    let wal = std::fs::read(&wal_path).unwrap();
    let reader = Database::open_existing_read_only(&path).unwrap();
    assert_eq!(
        reader.get_state("marker").unwrap().as_deref(),
        Some("committed-in-wal")
    );
    assert!(reader.set_state("marker", "mutation").is_err());
    drop(reader);
    assert_eq!(std::fs::read(&path).unwrap(), main);
    assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
    assert!(db.get_state("writer_fence_v1:lease").unwrap().is_none());
}

#[test]
fn startup_retirement_preserves_uncertain_or_replaced_ownership_evidence() {
    // Each active owner is process-scoped. Run this control in its own process
    // so other tests cannot replace the process-wide writer identity.
    if std::env::var("REPOSYNC_STARTUP_RETIRE_CHILD").as_deref() != Ok("1") {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "startup_retirement_preserves_uncertain_or_replaced_ownership_evidence",
            ])
            .env("REPOSYNC_STARTUP_RETIRE_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for case in ["uncertain", "native", "lock", "database"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("personal.db");
        let db = Database::open_personal_writer(directory.path()).unwrap();
        if case == "uncertain" {
            let window = db.begin_ordinary_writer_window(None).unwrap();
            window.mark_unjournalled_publication_possible().unwrap();
            drop(window);
        }
        if case == "native" {
            db.conn().execute_batch("INSERT INTO kv_state(key,value,updated_at) VALUES('writer_fence_v1:native','retained-unknown-native-proof','fixture') ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at;").unwrap();
        }
        let lease = db.get_state("writer_fence_v1:lease").unwrap();
        let native = db.get_state("writer_fence_v1:native").unwrap();
        assert!(lease.is_some());
        let reader = Database::open_existing_read_only(&path).unwrap();
        if case == "lock" {
            std::fs::rename(
                directory.path().join("reposync.lock"),
                directory.path().join("retained.lock"),
            )
            .unwrap();
            std::fs::write(directory.path().join("reposync.lock"), b"replacement").unwrap();
        }
        let main_path = if case == "database" {
            let retained = directory.path().join("retained.db");
            std::fs::rename(&path, &retained).unwrap();
            retained
        } else {
            path.clone()
        };
        let main = std::fs::read(&main_path).unwrap();
        let wal_path = path.with_extension("db-wal");
        let wal = std::fs::read(&wal_path).unwrap();
        drop(db);
        assert_eq!(
            reader.get_state("writer_fence_v1:lease").unwrap(),
            lease,
            "{case}"
        );
        assert_eq!(
            reader.get_state("writer_fence_v1:native").unwrap(),
            native,
            "{case}"
        );
        assert_eq!(std::fs::read(&main_path).unwrap(), main, "{case}");
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal, "{case}");
    }
}

#[test]
fn writer_lock_symlink_refuses_before_target_truncation_or_database_creation() {
    for personal in [false, true] {
        for contents in [
            b"preserve unrelated contents".as_slice(),
            b"4294967295".as_slice(),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("unrelated.lock");
            let lock = directory.path().join("reposync.lock");
            std::fs::write(&target, contents).unwrap();
            std::os::unix::fs::symlink(&target, &lock).unwrap();
            assert!(open(directory.path(), personal).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), contents);
            assert!(std::fs::symlink_metadata(&lock)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(!directory.path().join("personal.db").exists());
            assert!(!directory.path().join("reposync.db").exists());
        }
    }
}

#[test]
fn writer_hardlinked_lock_refuses_before_target_truncation_or_database_creation() {
    for personal in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("unrelated.lock");
        let lock = directory.path().join("reposync.lock");
        let contents = b"preserve unrelated hardlink contents";
        std::fs::write(&target, contents).unwrap();
        std::fs::hard_link(&target, &lock).unwrap();
        assert!(open(directory.path(), personal).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), contents);
        assert_eq!(std::fs::read(&lock).unwrap(), contents);
        assert!(!directory.path().join("personal.db").exists());
        assert!(!directory.path().join("reposync.db").exists());
    }
}
