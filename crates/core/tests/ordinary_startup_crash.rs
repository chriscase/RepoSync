//! Ordinary startup must recover a dead local owner without changing held evidence.
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use reposync_core::db::Database;

struct ChildOwner(Child);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn child(test: &str, path: &Path, stage: &str, personal: bool) -> (bool, String) {
    let logs = tempfile::tempdir().unwrap();
    let out = logs.path().join("stdout");
    let err = logs.path().join("stderr");
    let mut process = ChildOwner(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test])
            .env("REPOSYNC_STARTUP_CRASH_STAGE", stage)
            .env("REPOSYNC_STARTUP_CRASH_PATH", path)
            .env("REPOSYNC_STARTUP_CRASH_PERSONAL", personal.to_string())
            .env_remove("HOSTNAME")
            .env_remove("COMPUTERNAME")
            .env_remove("REPOSYNC_FENCE_HOST")
            .stdout(std::fs::File::create(&out).unwrap())
            .stderr(std::fs::File::create(&err).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "{stage}: child did not finish");
        std::thread::sleep(Duration::from_millis(20));
    };
    (
        status.success(),
        format!(
            "{}\n{}",
            std::fs::read_to_string(out).unwrap(),
            std::fs::read_to_string(err).unwrap()
        ),
    )
}

fn open(path: &Path, personal: bool) -> Result<Database, reposync_core::errors::DatabaseError> {
    if personal {
        Database::open_personal_writer(path)
    } else {
        Database::open_cli_writer(path)
    }
}

#[test]
fn crash_restart_without_environment_hostname_takes_a_new_local_epoch() {
    const NAME: &str = "crash_restart_without_environment_hostname_takes_a_new_local_epoch";
    if let Ok(stage) = std::env::var("REPOSYNC_STARTUP_CRASH_STAGE") {
        let path = std::env::var_os("REPOSYNC_STARTUP_CRASH_PATH").unwrap();
        let personal = std::env::var("REPOSYNC_STARTUP_CRASH_PERSONAL").unwrap() == "true";
        let db = open(Path::new(&path), personal).unwrap();
        let lease: serde_json::Value =
            serde_json::from_str(&db.get_state("writer_fence_v1:lease").unwrap().unwrap()).unwrap();
        if stage == "claim" {
            assert_eq!(lease["epoch"].as_u64(), Some(1));
            db.set_state("crash-marker", "committed-before-exit")
                .unwrap();
            // A process exit bypasses Database::Drop, like abort or SIGKILL.
            std::process::exit(0);
        }
        assert_eq!(stage, "restart");
        assert_eq!(lease["epoch"].as_u64(), Some(2));
        assert_eq!(
            db.get_state("crash-marker").unwrap().as_deref(),
            Some("committed-before-exit")
        );
        return;
    }
    for personal in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let first = child(NAME, directory.path(), "claim", personal);
        assert!(first.0, "{}", first.1);
        let second = child(NAME, directory.path(), "restart", personal);
        assert!(second.0, "{}", second.1);
        let db = Database::open_existing_read_only(directory.path().join(if personal {
            "personal.db"
        } else {
            "reposync.db"
        }))
        .unwrap();
        assert!(db.get_state("writer_fence_v1:lease").unwrap().is_none());
        assert!(db.get_state("writer_fence_v1:native").unwrap().is_none());
    }
}

#[test]
fn hardlinked_database_refuses_before_schema_lease_or_wal_mutation() {
    for personal in [false, true] {
        for initialized in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("unrelated.db");
            if initialized {
                let db = Database::new(&target).unwrap();
                db.initialize().unwrap();
                db.set_state("marker", "preserved").unwrap();
            } else {
                std::fs::write(&target, []).unwrap();
            }
            let data = directory.path().join("data");
            std::fs::create_dir(&data).unwrap();
            let path = data.join(if personal {
                "personal.db"
            } else {
                "reposync.db"
            });
            std::fs::hard_link(&target, &path).unwrap();
            let before = std::fs::read(&target).unwrap();
            assert!(open(&data, personal).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), before);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!path.with_extension("db-wal").exists());
            assert!(!path.with_extension("db-shm").exists());
        }
    }
}

#[test]
fn hot_rollback_recovers_quiet_storage_but_preserves_held_preimage_bytes() {
    const NAME: &str = "hot_rollback_recovers_quiet_storage_but_preserves_held_preimage_bytes";
    if let Ok(stage) = std::env::var("REPOSYNC_STARTUP_CRASH_STAGE") {
        assert_eq!(stage, "spill");
        let path = std::env::var_os("REPOSYNC_STARTUP_CRASH_PATH").unwrap();
        let conn = rusqlite::Connection::open(Path::new(&path)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=1; PRAGMA cache_spill=ON; BEGIN IMMEDIATE;").unwrap();
        let value = "x".repeat(32768);
        for index in 0..64 {
            conn.execute(
                "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'uncommitted')",
                rusqlite::params![format!("spill-{index}"), value],
            )
            .unwrap();
        }
        std::process::exit(0);
    }
    for personal in [false, true] {
        for held in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(if personal {
                "personal.db"
            } else {
                "reposync.db"
            });
            {
                let db = Database::new(&path).unwrap();
                db.initialize().unwrap();
                db.set_state("marker", "committed-before-spill").unwrap();
                if held {
                    db.set_state(
                        "pair_refresh_execution_v3:document:held",
                        "preserve-unknown-recovery-evidence",
                    )
                    .unwrap();
                }
                db.conn()
                    .execute_batch("PRAGMA journal_mode=DELETE;")
                    .unwrap();
            }
            let spill = child(NAME, &path, "spill", personal);
            assert!(spill.0, "{}", spill.1);
            let journal = path.with_extension("db-journal");
            let main_before = std::fs::read(&path).unwrap();
            let journal_before = std::fs::read(&journal).unwrap();
            assert_eq!(
                &journal_before[..8],
                &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
            );
            if held {
                assert!(open(directory.path(), personal).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), main_before);
                assert_eq!(std::fs::read(&journal).unwrap(), journal_before);
            } else {
                let db = open(directory.path(), personal).unwrap();
                assert_eq!(
                    db.get_state("marker").unwrap().as_deref(),
                    Some("committed-before-spill")
                );
                assert!(db.get_state("spill-0").unwrap().is_none());
                assert!(db.get_state("spill-63").unwrap().is_none());
            }
        }
    }
}

#[test]
fn rollback_aliases_superjournals_and_wal_refuse_without_external_mutation() {
    use std::io::Write;
    const SPILL: &str = "hot_rollback_recovers_quiet_storage_but_preserves_held_preimage_bytes";
    for personal in [false, true] {
        for case in ["superjournal", "symlink", "hardlink", "wal"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(if personal {
                "personal.db"
            } else {
                "reposync.db"
            });
            {
                let db = Database::new(&path).unwrap();
                db.initialize().unwrap();
                db.set_state("marker", "preserve-external-storage").unwrap();
                db.conn()
                    .execute_batch("PRAGMA journal_mode=DELETE;")
                    .unwrap();
            }
            let spill = child(SPILL, &path, "spill", personal);
            assert!(spill.0, "{}", spill.1);
            let journal = path.with_extension("db-journal");
            let external = directory.path().join("external-evidence");
            if case == "superjournal" {
                std::fs::write(&external, b"preserve-superjournal-evidence").unwrap();
                let name = external.to_str().unwrap().as_bytes();
                let checksum = name
                    .iter()
                    .fold(0u32, |sum, byte| sum.wrapping_add((*byte as i8) as u32));
                let mut output = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&journal)
                    .unwrap();
                output.write_all(name).unwrap();
                output
                    .write_all(&(name.len() as u32).to_be_bytes())
                    .unwrap();
                output.write_all(&checksum.to_be_bytes()).unwrap();
                output
                    .write_all(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7])
                    .unwrap();
            } else if case == "wal" {
                std::fs::write(&external, b"preserve-external-evidence").unwrap();
                std::fs::write(path.with_extension("db-wal"), b"preserve-unknown-wal").unwrap();
            } else {
                std::fs::rename(&journal, &external).unwrap();
                if case == "symlink" {
                    std::os::unix::fs::symlink(&external, &journal).unwrap();
                } else {
                    std::fs::hard_link(&external, &journal).unwrap();
                }
            }
            let main_before = std::fs::read(&path).unwrap();
            let journal_before = std::fs::read(&journal).unwrap();
            let external_before = std::fs::read(&external).unwrap();
            let wal_before = std::fs::read(path.with_extension("db-wal")).ok();
            assert!(open(directory.path(), personal).is_err(), "{case}");
            assert_eq!(std::fs::read(&path).unwrap(), main_before, "{case}");
            assert_eq!(std::fs::read(&journal).unwrap(), journal_before, "{case}");
            assert_eq!(std::fs::read(&external).unwrap(), external_before, "{case}");
            assert_eq!(
                std::fs::read(path.with_extension("db-wal")).ok(),
                wal_before,
                "{case}"
            );
            if case == "symlink" {
                assert!(std::fs::symlink_metadata(&journal)
                    .unwrap()
                    .file_type()
                    .is_symlink());
            }
        }
    }
}

#[test]
fn empty_main_journal_is_never_deleted_by_writer_or_inspection() {
    for personal in [false, true] {
        for aliased in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(if personal {
                "personal.db"
            } else {
                "reposync.db"
            });
            std::fs::write(&path, []).unwrap();
            let journal = path.with_extension("db-journal");
            let bytes = [1u8; 1024];
            std::fs::write(&journal, bytes).unwrap();
            let alias = directory.path().join("external-journal");
            if aliased {
                std::fs::hard_link(&journal, &alias).unwrap();
            }
            assert!(open(directory.path(), personal).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), [0u8; 0]);
            assert_eq!(std::fs::read(&journal).unwrap(), bytes);
            assert!(Database::open_existing_read_only(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), [0u8; 0]);
            assert_eq!(std::fs::read(&journal).unwrap(), bytes);
            if aliased {
                assert_eq!(std::fs::read(alias).unwrap(), bytes);
            }
        }
    }
}

#[test]
fn environment_hostname_cannot_make_a_foreign_lease_local() {
    const NAME: &str = "environment_hostname_cannot_make_a_foreign_lease_local";
    if let Some(path) = std::env::var_os("REPOSYNC_FOREIGN_LEASE_CHILD") {
        assert!(Database::open_personal_writer(Path::new(&path)).is_err());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("personal.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    db.conn()
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let host = format!("foreign-{}", uuid::Uuid::new_v4());
    let lease = serde_json::json!({"version":1,"epoch":17,"holder_pid":1073741824u32,"holder_host":host,"acquired_at":"2026-10-01T00:00:00Z","renewed_at":"2026-10-01T00:00:00Z"});
    db.set_state("writer_fence_v1:lease", &lease.to_string())
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let wal_path = path.with_extension("db-wal");
    let wal = std::fs::read(&wal_path).unwrap();
    let stdout = directory.path().join("stdout");
    let stderr = directory.path().join("stderr");
    let mut process = ChildOwner(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME])
            .env("REPOSYNC_FOREIGN_LEASE_CHILD", directory.path())
            .env("HOSTNAME", host)
            .env_remove("COMPUTERNAME")
            .env_remove("REPOSYNC_FENCE_HOST")
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "foreign lease child did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success(),
        "{}\n{}",
        std::fs::read_to_string(stdout).unwrap(),
        std::fs::read_to_string(stderr).unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
    assert_eq!(
        db.get_state("writer_fence_v1:lease").unwrap().unwrap(),
        lease.to_string()
    );
    assert!(db.get_state("writer_fence_v1:native").unwrap().is_none());
}
