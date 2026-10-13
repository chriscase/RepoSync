//! A read-only startup probe must read committed WAL frames without checkpointing.
use reposync_core::db::Database;

fn crashed_wal_fixture(root: &std::path::Path, corrupt_hold: bool, truncated_shm: bool) {
    let path = root.join("reposync.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    db.ensure_repository_scope_schema_upgrade().unwrap();
    db.conn()
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    db.set_state("wal-startup-marker", "committed-in-wal")
        .unwrap();
    if corrupt_hold {
        db.set_state("pair_refresh_execution_v3:hold:orphan", "retained-evidence")
            .unwrap();
    }
    let main = std::fs::read(&path).unwrap();
    let wal = std::fs::read(root.join("reposync.db-wal")).unwrap();
    assert!(wal.len() > 32);
    drop(db);
    // Restore the exact committed main/WAL pair without an open connection,
    // modeling the files left when a writer exits before checkpointing.
    std::fs::write(&path, main).unwrap();
    std::fs::write(root.join("reposync.db-wal"), wal).unwrap();
    let shm = root.join("reposync.db-shm");
    if shm.exists() {
        std::fs::remove_file(&shm).unwrap();
    }
    if truncated_shm {
        std::fs::write(shm, [0u8; 17]).unwrap();
    }
}

#[test]
fn ordinary_startup_reads_committed_wal_with_missing_or_truncated_shared_memory() {
    for truncated_shm in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        crashed_wal_fixture(temp.path(), false, truncated_shm);
        let (db, owner) = Database::open_with_exclusive_owner(temp.path()).unwrap();
        assert_eq!(
            db.get_state("wal-startup-marker").unwrap().as_deref(),
            Some("committed-in-wal")
        );
        drop(owner);
    }
}

#[test]
fn ordinary_startup_with_missing_or_truncated_shared_memory_preserves_held_wal() {
    for truncated_shm in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        crashed_wal_fixture(temp.path(), true, truncated_shm);
        let path = temp.path().join("reposync.db");
        let wal = temp.path().join("reposync.db-wal");
        let main_before = std::fs::read(&path).unwrap();
        let wal_before = std::fs::read(&wal).unwrap();
        let error = match Database::open_with_exclusive_owner(temp.path()) {
            Ok(_) => panic!("orphan recovery evidence must refuse startup"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("pair refresh journal"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), main_before);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    }
}
