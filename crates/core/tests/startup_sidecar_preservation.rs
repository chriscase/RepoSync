//! Truncated or missing main storage cannot erase its remaining startup evidence.
use reposync_core::db::Database;
use std::path::{Path, PathBuf};

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn evidence(suffix: &str) -> Vec<u8> {
    let mut bytes = vec![0x39; 1024];
    if suffix == "-journal" {
        bytes[..8].copy_from_slice(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]);
        bytes[16..20].copy_from_slice(&1u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&512u32.to_be_bytes());
        bytes[24..28].copy_from_slice(&4096u32.to_be_bytes());
    }
    bytes
}

fn preserve(file: &str, suffix: &str, missing: bool, entrance: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(file);
    if !missing {
        std::fs::write(&path, [0u8; 0]).unwrap();
    }
    let sibling = sidecar(&path, suffix);
    let bytes = evidence(suffix);
    std::fs::write(&sibling, &bytes).unwrap();
    let refused = match entrance {
        "personal" => Database::open_personal_writer(directory.path()).map(|_| ()),
        "cli" => Database::open_cli_writer(directory.path()).map(|_| ()),
        "daemon" => Database::open_with_exclusive_owner(directory.path()).map(|_| ()),
        "recovery" => Database::open_existing_for_pair_refresh_recovery(&path).map(|_| ()),
        "inspection" => Database::open_existing_read_only(&path).map(|_| ()),
        _ => panic!("unknown fixture entrance"),
    };
    assert!(
        refused.is_err(),
        "{entrance} admitted {suffix} without main storage"
    );
    assert_eq!(
        std::fs::read(&sibling).unwrap(),
        bytes,
        "{entrance} changed {suffix}"
    );
    if missing {
        assert!(!path.exists(), "{entrance} created main storage");
    } else {
        assert_eq!(std::fs::read(&path).unwrap(), [0u8; 0]);
    }
    for other in ["-journal", "-wal", "-shm"] {
        if other != suffix {
            assert!(
                !sidecar(&path, other).exists(),
                "{entrance} created {other}"
            );
        }
    }
}

#[test]
fn ordinary_and_inspection_refuse_zero_or_missing_main_sidecars() {
    for (entrance, file) in [
        ("personal", "personal.db"),
        ("cli", "reposync.db"),
        ("inspection", "reposync.db"),
    ] {
        for suffix in ["-journal", "-wal", "-shm"] {
            for missing in [false, true] {
                preserve(file, suffix, missing, entrance);
            }
        }
    }
}

#[test]
fn daemon_refuses_zero_or_missing_main_sidecars() {
    for suffix in ["-journal", "-wal", "-shm"] {
        for missing in [false, true] {
            preserve("reposync.db", suffix, missing, "daemon");
        }
    }
}

#[test]
fn recovery_refuses_zero_or_missing_main_sidecars() {
    for suffix in ["-journal", "-wal", "-shm"] {
        for missing in [false, true] {
            preserve("reposync.db", suffix, missing, "recovery");
        }
    }
}

#[test]
fn real_committed_wal_survives_zero_main_startup_refusal() {
    let seed_directory = tempfile::tempdir().unwrap();
    let seed_path = seed_directory.path().join("seed.db");
    let seed = Database::new(&seed_path).unwrap();
    seed.initialize().unwrap();
    seed.conn()
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    seed.set_state("committed-evidence", "must survive")
        .unwrap();
    let wal = std::fs::read(sidecar(&seed_path, "-wal")).unwrap();
    assert!(wal.len() > 32);
    for (entrance, file) in [
        ("personal", "personal.db"),
        ("cli", "reposync.db"),
        ("daemon", "reposync.db"),
        ("recovery", "reposync.db"),
        ("inspection", "reposync.db"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(file);
        std::fs::write(&path, [0u8; 0]).unwrap();
        let sibling = sidecar(&path, "-wal");
        std::fs::write(&sibling, &wal).unwrap();
        let outcome = match entrance {
            "personal" => Database::open_personal_writer(directory.path()).map(|_| ()),
            "cli" => Database::open_cli_writer(directory.path()).map(|_| ()),
            "daemon" => Database::open_with_exclusive_owner(directory.path()).map(|_| ()),
            "recovery" => Database::open_existing_for_pair_refresh_recovery(&path).map(|_| ()),
            "inspection" => Database::open_existing_read_only(&path).map(|_| ()),
            _ => unreachable!(),
        };
        assert!(
            outcome.is_err(),
            "{entrance} admitted a truncated committed WAL"
        );
        assert_eq!(std::fs::read(&sibling).unwrap(), wal);
        assert_eq!(std::fs::read(&path).unwrap(), [0u8; 0]);
    }
    assert_eq!(
        seed.get_state("committed-evidence").unwrap().as_deref(),
        Some("must survive")
    );
}

#[test]
fn future_schema_recovery_refusal_preserves_committed_wal_without_close_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reposync.db");
    let script = "import sqlite3,sys,os\nc=sqlite3.connect(sys.argv[1])\nc.executescript(\"PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE marker(value TEXT); INSERT INTO marker VALUES('committed'); PRAGMA user_version=2147483647;\")\nos._exit(0)\n";
    let mut child = std::process::Command::new("python3")
        .args(["-c", script])
        .arg(&path)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("WAL fixture child timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let wal_path = sidecar(&path, "-wal");
    let main = std::fs::read(&path).unwrap();
    let wal = std::fs::read(&wal_path).unwrap();
    assert!(!main.is_empty());
    assert!(wal.len() > 32);
    assert!(Database::open_existing_for_pair_refresh_recovery(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), main);
    assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
}

fn alias_fixture_open(
    entrance: &str,
    directory: &Path,
    path: &Path,
) -> Result<(), reposync_core::errors::DatabaseError> {
    match entrance {
        "personal" => Database::open_personal_writer(directory).map(|_| ()),
        "cli" => Database::open_cli_writer(directory).map(|_| ()),
        "daemon" => Database::open_with_exclusive_owner(directory).map(|_| ()),
        "recovery" => Database::open_existing_for_pair_refresh_recovery(path).map(|_| ()),
        "inspection" => Database::open_existing_read_only(path).map(|_| ()),
        _ => unreachable!(),
    }
}

#[test]
fn healthy_main_refuses_aliased_wal_and_shm_without_touching_targets() {
    for (entrance, file) in [
        ("personal", "personal.db"),
        ("cli", "reposync.db"),
        ("daemon", "reposync.db"),
        ("recovery", "reposync.db"),
        ("inspection", "reposync.db"),
    ] {
        for suffix in ["-wal", "-shm"] {
            for hardlink in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join(file);
                {
                    let db = Database::new(&path).unwrap();
                    db.initialize().unwrap();
                    db.ensure_repository_scope_schema_upgrade().unwrap();
                    db.set_state("alias-marker", "unchanged").unwrap();
                }
                let main = std::fs::read(&path).unwrap();
                assert!(!main.is_empty());
                let target = directory.path().join("external-target");
                let target_bytes = vec![0x3a; 32768];
                std::fs::write(&target, &target_bytes).unwrap();
                let sibling = sidecar(&path, suffix);
                if hardlink {
                    std::fs::hard_link(&target, &sibling).unwrap();
                } else {
                    std::os::unix::fs::symlink(&target, &sibling).unwrap();
                }
                let outcome = alias_fixture_open(entrance, directory.path(), &path);
                assert!(
                    outcome.is_err(),
                    "{entrance} admitted aliased {suffix}, hardlink={hardlink}"
                );
                assert_eq!(std::fs::read(&path).unwrap(), main);
                assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
                assert_eq!(std::fs::read(&sibling).unwrap(), target_bytes);
            }
        }
    }
}

#[test]
fn healthy_hardlinked_main_refuses_all_startup_entrances_without_other_name_mutation() {
    for (entrance, file) in [
        ("personal", "personal.db"),
        ("cli", "reposync.db"),
        ("daemon", "reposync.db"),
        ("recovery", "reposync.db"),
        ("inspection", "reposync.db"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(file);
        {
            let db = Database::new(&path).unwrap();
            db.initialize().unwrap();
            db.ensure_repository_scope_schema_upgrade().unwrap();
            db.set_state("hardlink-marker", "kept").unwrap();
        }
        let other = directory.path().join("other-main");
        std::fs::hard_link(&path, &other).unwrap();
        let before = std::fs::read(&other).unwrap();
        assert!(
            alias_fixture_open(entrance, directory.path(), &path).is_err(),
            "{entrance} admitted a hardlinked main"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(&other).unwrap(), before);
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(
                !sidecar(&path, suffix).exists(),
                "{entrance} created {suffix}"
            );
        }
    }
}
