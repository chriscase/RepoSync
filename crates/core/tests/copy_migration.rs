#![cfg(feature = "reliability-fixture")]
use anyhow::bail;
use reposync_core::db::{candidate_migration::CopySession, Database};
use rusqlite::Connection;
use std::{fs, path::Path};
use tempfile::TempDir;
fn copy(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for e in fs::read_dir(source).unwrap() {
        let e = e.unwrap();
        let to = target.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy(&e.path(), &to)
        } else {
            fs::copy(e.path(), to).unwrap();
        }
    }
}
fn fixture(ids: &[i64], delete: &[i64]) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("copy");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("config.toml"), "synthetic configuration\n").unwrap();
    {
        let db = Database::new(source.join("reposync.db")).unwrap();
        db.initialize().unwrap();
        let c = db.conn();
        for id in ids {
            c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id) VALUES(?1,?1,'sha','svn_to_git','original-timestamp','original-author','original-git-author','orphan-legacy')",[id]).unwrap();
        }
        for id in delete {
            c.execute("DELETE FROM commit_map WHERE id=?1", [id])
                .unwrap();
        }
        c.execute(
            "INSERT INTO audit_log(id,action,created_at) VALUES(81,'fixture','t')",
            [],
        )
        .unwrap();
        c.execute("DELETE FROM audit_log", []).unwrap();
    }
    copy(&source, &target);
    (temp, source, target)
}
fn version(p: &Path) -> i64 {
    Connection::open(p.join("reposync.db"))
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}
fn sequence_case(id: &str, ids: &[i64], delete: &[i64], expected: Option<i64>, next: i64) {
    let (_t, s, c) = fixture(ids, delete);
    let session = CopySession::seal(&s, &c).unwrap();
    let report = session.migrate(13, &mut |_, _| Ok(())).unwrap();
    assert_eq!(report.final_version, 13);
    let db = Connection::open(c.join("reposync.db")).unwrap();
    use rusqlite::OptionalExtension;
    let seq = db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='commit_map'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .unwrap();
    assert_eq!(seq, expected);
    db.execute("INSERT INTO commit_map(svn_rev,git_sha,direction,synced_at) VALUES(42,NULL,'svn_to_git','t')",[]).unwrap();
    assert_eq!(db.last_insert_rowid(), next);
    assert_eq!(
        db.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='audit_log'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        81
    );
    session.source_unchanged().unwrap();
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":id,"migration":report,"expected_sequence":expected,"actual_sequence":seq,"next_generated_id":next,"other_sequence":81})
    );
}
#[test]
fn v13_never_used() {
    sequence_case("V13_NEVER_USED", &[], &[], None, 1)
}
#[test]
fn v13_rows() {
    sequence_case("V13_ROWS", &[1, 2, 3], &[], Some(3), 4)
}
#[test]
fn v13_sparse_deleted_high() {
    sequence_case("V13_SPARSE_HIGH", &[1, 20, 100], &[100], Some(100), 101)
}
#[test]
fn v13_empty_used() {
    sequence_case("V13_EMPTY_USED", &[100], &[100], Some(100), 101)
}
#[test]
fn v13_large_sequence() {
    sequence_case(
        "V13_LARGE_SEQUENCE",
        &[1, 4_294_967_296],
        &[4_294_967_296],
        Some(4_294_967_296),
        4_294_967_297,
    )
}
#[test]
fn v13_failure_rollback_retry() {
    let mut matrix = Vec::new();
    for boundary in [
        "before_transaction",
        "after_create",
        "after_copy",
        "after_replace",
        "before_validation",
        "before_version",
        "after_version",
    ] {
        let (_t, s, c) = fixture(&[1, 100], &[100]);
        let session = CopySession::seal(&s, &c).unwrap();
        let bytes = fs::read(c.join("reposync.db")).unwrap();
        assert!(session
            .migrate(13, &mut |_, point| {
                if point == boundary {
                    bail!("injected {point}")
                }
                Ok(())
            })
            .is_err());
        assert_eq!(version(&c), 12);
        assert_eq!(fs::read(c.join("reposync.db")).unwrap(), bytes);
        let report = session.migrate(13, &mut |_, _| Ok(())).unwrap();
        assert_eq!(report.final_version, 13);
        session.source_unchanged().unwrap();
        matrix.push((boundary, 12, 13));
    }
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"V13_ROLLBACK","matrix":matrix,"failed_copy_bytes_unchanged":true})
    );
}
#[test]
fn ordinary_startup_stays_v12() {
    let (_t, s, c) = fixture(&[1], &[]);
    let before = fs::read(s.join("reposync.db")).unwrap();
    {
        let db = Database::new(c.join("reposync.db")).unwrap();
        db.initialize().unwrap();
        db.initialize().unwrap();
        assert_eq!(
            db.conn()
                .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            12
        );
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'pair_%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    assert_eq!(fs::read(s.join("reposync.db")).unwrap(), before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"STARTUP_V12","user_version":12,"candidate_tables":0,"repeat_initialize":true})
    );
}
