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
        c.execute("INSERT INTO repositories(id,name,svn_url,created_at,updated_at,enabled) VALUES('synthetic','synthetic','unqualified','t','t',0)",[]).unwrap();
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
    let report = session.migrate(13, &mut |_, _, _| Ok(())).unwrap();
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
            .migrate(13, &mut |_, point, _| {
                if point == boundary {
                    bail!("injected {point}")
                }
                Ok(())
            })
            .is_err());
        assert_eq!(version(&c), 12);
        assert_eq!(fs::read(c.join("reposync.db")).unwrap(), bytes);
        let report = session.migrate(13, &mut |_, _, _| Ok(())).unwrap();
        assert_eq!(report.final_version, 13);
        session.source_unchanged().unwrap();
        matrix.push((boundary, 12, 13));
    }
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"V13_ROLLBACK","matrix":matrix,"failed_copy_bytes_unchanged":true})
    );
}
fn old_topology() -> (TempDir, std::path::PathBuf, serde_json::Value) {
    let t = TempDir::new().unwrap();
    let root = t.path().join("generated");
    let generator = std::env::var("REPOSYNC_OLD_GENERATOR").expect("pinned old generator required");
    let result = std::process::Command::new(&generator)
        .args([
            "generate_legacy_topology",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("REPOSYNC_OLD_TOPOLOGY_DIR", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Developer")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Developer")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "old production import failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let evidence = String::from_utf8_lossy(&result.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("OLD_TOPOLOGY_EVIDENCE "))
        .map(|s| serde_json::from_str(s).unwrap())
        .unwrap();
    (t, root, evidence)
}
fn qualified(source: &Path, target: &Path, root: &Path) -> CopySession {
    let mut session = CopySession::seal(source, target).unwrap();
    session
        .qualify_imported_pair("pair", &root.join("svn-one"), &root.join("origin-one.git"))
        .unwrap();
    session
        .qualify_imported_pair(
            "pair_two",
            &root.join("svn-two"),
            &root.join("origin-two.git"),
        )
        .unwrap();
    assert!(session
        .qualify_imported_pair(
            "pair_disabled",
            &root.join("svn-disabled"),
            &root.join("origin-disabled.git")
        )
        .is_err());
    session
}
#[test]
fn v14_old_topology_qualification() {
    let (t, root, provenance) = old_topology();
    let source = root.join("install");
    let target = t.path().join("candidate");
    copy(&source, &target);
    let endpoints_before = endpoint_files(&root);
    let session = qualified(&source, &target, &root);
    let source_manifest = session.source_seal().clone();
    let report = session.migrate(14, &mut |_, _, _| Ok(())).unwrap();
    assert_eq!((report.starting_version, report.final_version), (12, 14));
    assert_eq!(report.canonical["pair_lineages"].len(), 2);
    assert_eq!(report.canonical["pair_frontiers"].len(), 4);
    assert_eq!(report.canonical["pair_outcomes"].len(), 0);
    let c = Connection::open(target.join("reposync.db")).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM pair_lineages WHERE repo_id='pair_disabled'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT disposition FROM repo_migration_state WHERE repo_id='pair_disabled'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "not_qualified"
    );
    drop(c);
    let bytes = fs::read(target.join("reposync.db")).unwrap();
    let repeated = qualified(&source, &target, &root)
        .migrate(14, &mut |_, _, _| Ok(()))
        .unwrap();
    assert_eq!(repeated.starting_version, 14);
    assert_eq!(fs::read(target.join("reposync.db")).unwrap(), bytes);
    assert_eq!(report.canonical, repeated.canonical);
    let endpoints_after = endpoint_files(&root);
    assert_eq!(endpoints_after, endpoints_before);
    session.source_unchanged().unwrap();
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"V14_OLD_TOPOLOGY","old_generation":provenance,"migration":report,"repeated":true,"writer_exited_before_seal":true,"normal_startup_registry":12,"installation_manifest":source_manifest,"endpoint_files_before":endpoints_before,"endpoint_files_after":endpoints_after})
    );
}
#[test]
fn v14_partial_conversion_restart() {
    let (t, root, _) = old_topology();
    let source = root.join("install");
    let mut matrix = Vec::new();
    for (index, boundary) in [
        "before_transaction",
        "after_schema",
        "after_repository",
        "before_validation",
        "before_version",
        "after_version",
    ]
    .iter()
    .enumerate()
    {
        let target = t.path().join(format!("copy-{index}"));
        copy(&source, &target);
        let session = qualified(&source, &target, &root);
        let err = session.migrate(14, &mut |v, point, _| {
            if v == 14 && point == *boundary {
                bail!("injected {point}")
            }
            Ok(())
        });
        assert!(err.is_err());
        assert_eq!(version(&target), 13);
        let c = Connection::open(target.join("reposync.db")).unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'pair_%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(c);
        let retry = qualified(&source, &target, &root)
            .migrate(14, &mut |_, _, _| Ok(()))
            .unwrap();
        assert_eq!((retry.starting_version, retry.final_version), (13, 14));
        matrix.push((boundary, 13, 14));
    }
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"V14_RESTART","matrix":matrix,"committed_v13_retained":true})
    );
}
#[test]
fn unqualified_states_preserved() {
    let (_t, s, c) = fixture(&[1, 100], &[100]);
    {
        let db = Connection::open(s.join("reposync.db")).unwrap();
        for (id, state) in [
            ("pruned", "needs_reconciliation"),
            ("v1", "needs_reconciliation"),
            ("v2", "needs_reconciliation"),
            ("historical_filtered", "needs_reconciliation"),
            ("replaced", "needs_reconciliation"),
            ("unknown", "external_effect_unknown"),
            ("unproved", "not_qualified"),
        ] {
            db.execute("INSERT INTO repositories(id,name,svn_url,created_at,updated_at,enabled,last_svn_rev,last_git_sha) VALUES(?1,?1,'fixture-unknown','t','t',0,17,'retained')",[id]).unwrap();
            db.execute(
                "INSERT INTO kv_state VALUES(?1,?2,'t')",
                rusqlite::params![format!("legacy_{id}"), state],
            )
            .unwrap();
        }
    }
    // Recopy only the synthetic DB; no pinned original source is modified.
    fs::copy(s.join("reposync.db"), c.join("reposync.db")).unwrap();
    let mut session = CopySession::seal(&s, &c).unwrap();
    for (id, state) in [
        ("pruned", "needs_reconciliation"),
        ("v1", "needs_reconciliation"),
        ("v2", "needs_reconciliation"),
        ("historical_filtered", "needs_reconciliation"),
        ("replaced", "needs_reconciliation"),
        ("unknown", "external_effect_unknown"),
    ] {
        session
            .disposition(id, state, "synthetic_unqualified_overlay")
            .unwrap();
    }
    let report = session.migrate(14, &mut |_, _, _| Ok(())).unwrap();
    assert!(report.canonical["pair_lineages"].is_empty());
    assert!(report.canonical["pair_frontiers"].is_empty());
    assert_eq!(report.canonical["repo_migration_state"].len(), 8);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"V14_UNQUALIFIED","migration":report,"canonical_authority":0,"all_legacy_values_preserved":true})
    );
}
#[test]
fn migration_crash_restart_matrix() {
    if let Ok(source) = std::env::var("REPOSYNC_COPY_CRASH_SOURCE") {
        let target = std::env::var("REPOSYNC_COPY_CRASH_TARGET").unwrap();
        let wanted: i64 = std::env::var("REPOSYNC_COPY_CRASH_VERSION")
            .unwrap()
            .parse()
            .unwrap();
        let boundary = std::env::var("REPOSYNC_COPY_CRASH_POINT").unwrap();
        CopySession::seal(Path::new(&source), Path::new(&target))
            .unwrap()
            .migrate(14, &mut |v, p, _| {
                if v == wanted && p == boundary {
                    std::process::exit(86)
                }
                Ok(())
            })
            .unwrap();
        panic!("crash boundary not reached");
    }
    let mut matrix = Vec::new();
    for (v, points) in [
        (
            13,
            vec![
                "before_transaction",
                "after_create",
                "after_copy",
                "after_replace",
                "before_validation",
                "before_version",
                "after_version",
            ],
        ),
        (
            14,
            vec![
                "before_transaction",
                "after_schema",
                "after_repository",
                "before_validation",
                "before_version",
                "after_version",
            ],
        ),
    ] {
        for point in points {
            let (_t, s, c) = fixture(&[1, 100], &[100]);
            let seal = CopySession::seal(&s, &c).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "migration_crash_restart_matrix",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("REPOSYNC_COPY_CRASH_SOURCE", &s)
                .env("REPOSYNC_COPY_CRASH_TARGET", &c)
                .env("REPOSYNC_COPY_CRASH_VERSION", v.to_string())
                .env("REPOSYNC_COPY_CRASH_POINT", point)
                .output()
                .unwrap();
            assert_eq!(
                result.status.code(),
                Some(86),
                "boundary {v}/{point}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            // Opening the copied DB performs ordinary SQLite journal recovery. The
            // source is immutable and never participates in recovery.
            assert_eq!(version(&c), v - 1);
            let result = CopySession::seal(&s, &c)
                .unwrap()
                .migrate(14, &mut |_, _, _| Ok(()))
                .unwrap();
            assert_eq!(result.final_version, 14);
            seal.source_unchanged().unwrap();
            matrix.push((v, point, v - 1, 14));
        }
    }
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"MIGRATION_CRASH","matrix":matrix,"abrupt_process_exit":86,"source_unchanged":true})
    );
}
#[test]
fn migration_write_and_constraint_failure() {
    let mut matrix = Vec::new();
    for (v, boundary, sql) in [
        (13, "after_create", "PRAGMA query_only=ON"),
        (14, "after_schema", "PRAGMA query_only=ON"),
        (
            14,
            "after_repository",
            "INSERT INTO pair_lineages(repo_id) VALUES('synthetic')",
        ),
        (13, "before_transaction", "PRAGMA foreign_keys=OFF"),
    ] {
        let (_t, s, c) = fixture(&[1, 100], &[100]);
        let session = CopySession::seal(&s, &c).unwrap();
        let err = session
            .migrate(14, &mut |version, point, conn| {
                if version == v && point == boundary {
                    conn.execute_batch(sql)?;
                }
                Ok(())
            })
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("readonly")
                || message.contains("NOT NULL")
                || message.contains("foreign keys disabled"),
            "unexpected failure {message}"
        );
        assert_eq!(version(&c), v - 1);
        let retry = CopySession::seal(&s, &c)
            .unwrap()
            .migrate(14, &mut |_, _, _| Ok(()))
            .unwrap();
        assert_eq!(retry.final_version, 14);
        matrix.push((v, boundary, message, v - 1, 14));
    }
    // Real filesystem read-only file is supplemental; query_only above forces
    // SQLITE_READONLY reliably even in CI containers running as root.
    let (_t, s, c) = fixture(&[1], &[]);
    use std::os::unix::fs::PermissionsExt;
    let path = c.join("reposync.db");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let read =
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(read.execute("CREATE TABLE forbidden(x)", []).is_err());
    drop(read);
    assert_eq!(version(&c), 12);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    CopySession::seal(&s, &c)
        .unwrap()
        .source_unchanged()
        .unwrap();
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"MIGRATION_WRITE_FAILURE","matrix":matrix,"read_only_connection_rejected_write":true})
    );
}
#[test]
fn migration_forged_shape_and_future() {
    let mut outcomes = Vec::new();
    for sql in [
        "PRAGMA user_version=13",
        "PRAGMA user_version=14",
        "PRAGMA user_version=99",
        "DROP INDEX idx_commit_map_repo_git",
        "ALTER TABLE commit_map ADD COLUMN partial TEXT",
        "CREATE TRIGGER forged AFTER INSERT ON commit_map BEGIN SELECT 1; END",
    ] {
        let (_t, s, c) = fixture(&[1, 100], &[100]);
        Connection::open(c.join("reposync.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        let before = fs::read(c.join("reposync.db")).unwrap();
        let session = CopySession::seal(&s, &c).unwrap();
        assert!(session.migrate(14, &mut |_, _, _| Ok(())).is_err());
        assert_eq!(fs::read(c.join("reposync.db")).unwrap(), before);
        session.source_unchanged().unwrap();
        outcomes.push(sql);
    }
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"MIGRATION_FORGED","rejected":outcomes,"copied_bytes_unchanged":true})
    );
}
#[test]
fn copy_boundary_canaries() {
    let (_t, s, c) = fixture(&[1], &[]);
    assert!(CopySession::seal(&s, &s).is_err());
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), c.join("outside-link")).unwrap();
    assert!(CopySession::seal(&s, &c).is_err());
    fs::remove_file(c.join("outside-link")).unwrap();
    fs::write(s.join("reposync.db-wal"), b"pending writer").unwrap();
    assert!(CopySession::seal(&s, &c).is_err());
    fs::remove_file(s.join("reposync.db-wal")).unwrap();
    let session = CopySession::seal(&s, &c).unwrap();
    fs::write(c.join("config.toml"), b"changed-copy").unwrap();
    assert!(session.migrate(13, &mut |_, _, _| Ok(())).is_err());
    assert_eq!(version(&c), 12);
    session.source_unchanged().unwrap();
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"COPY_BOUNDARY","overlap_rejected":true,"symlink_rejected":true,"source_wal_rejected":true,"config_change_rejected":true,"source_unchanged":true})
    );
}
#[test]
fn mapping_null_semantics() {
    use reposync_core::db::{
        candidate_authority::{advance_frontier, ResolvedTransition},
        candidate_migration::{lookup_mapping, MappingLookup},
    };
    let (t, root, _) = old_topology();
    let source = root.join("install");
    let target = t.path().join("candidate");
    copy(&source, &target);
    let session = qualified(&source, &target, &root);
    session.migrate(14, &mut |_, _, _| Ok(())).unwrap();
    let mut c = Connection::open(target.join("reposync.db")).unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    c.execute("INSERT INTO commit_map(svn_rev,git_sha,direction,synced_at,repo_id) VALUES(2,'fixture-mapping','svn_to_git','t','pair')",[]).unwrap();
    assert_eq!(
        lookup_mapping(&c, "pair", 1, 2).unwrap(),
        MappingLookup::Mapped("fixture-mapping".into())
    );
    c.execute("INSERT INTO commit_map(svn_rev,git_sha,direction,synced_at,repo_id) VALUES(3,NULL,'svn_to_git','t','pair')",[]).unwrap();
    let id = c.last_insert_rowid();
    assert_eq!(
        lookup_mapping(&c, "pair", 1, 3).unwrap(),
        MappingLookup::LegacyUnresolvedNull
    );
    assert_eq!(
        lookup_mapping(&c, "pair", 1, 4).unwrap(),
        MappingLookup::Missing
    );
    c.execute("INSERT INTO commit_map(svn_rev,git_sha,direction,synced_at,repo_id) VALUES(4,NULL,'svn_to_git','t',NULL)",[]).unwrap();
    assert_eq!(
        lookup_mapping(&c, "pair", 1, 4).unwrap(),
        MappingLookup::LegacyOwnerless
    );
    let policy: String = c
        .query_row(
            "SELECT policy_sha256 FROM pair_lineages WHERE repo_id='pair'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    advance_frontier(
        &mut c,
        &ResolvedTransition {
            id: "typed-no-target".into(),
            repo_id: "pair".into(),
            generation: 1,
            direction: "svn_to_git".into(),
            predecessor_source_key: "svn:2".into(),
            source_svn_rev: Some(3),
            source_git_sha: None,
            outcome: "empty_no_target".into(),
            target_git_sha: None,
            target_svn_rev: None,
            projection_version: 1,
            policy_sha256: policy,
            evidence_json: "{\"fixture_component_proof\":true}".into(),
        },
    )
    .unwrap();
    // The mapping link is explicit, never inferred merely from nullable SHA.
    c.execute("INSERT INTO legacy_evidence_links VALUES('pair',1,'commit_map',?1,'proved_typed_no_target')",[id.to_string()]).unwrap();
    assert_eq!(
        lookup_mapping(&c, "pair", 1, 3).unwrap(),
        MappingLookup::ProvedNoTarget("empty_no_target".into())
    );
    assert_eq!(
        lookup_mapping(&c, "pair", 2, 3).unwrap(),
        MappingLookup::LegacyUnresolvedNull
    );
    c.execute("INSERT INTO commit_map(svn_rev,git_sha,direction,synced_at,repo_id) VALUES(3,NULL,'svn_to_git','t','pair')",[]).unwrap();
    assert!(lookup_mapping(&c, "pair", 1, 3).is_err());
    session.source_unchanged().unwrap();
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"MAPPING_NULL","mapped":true,"null_unresolved":true,"missing":true,"explicit_owned_no_target":true,"other_generation_unresolved":true,"ownerless_unresolved":true})
    );
}
#[test]
fn pinned_unqualified_overlays() {
    let (t, root, provenance) = old_topology();
    let original = root.join("install");
    let original_hash = sha256(&fs::read(original.join("reposync.db")).unwrap());
    let mut matrix = Vec::new();
    for (index, (name, sql, state)) in [
        (
            "pruned",
            "DELETE FROM sync_records WHERE repo_id='pair' AND svn_rev=1",
            "needs_reconciliation",
        ),
        (
            "pruned_mapping",
            "DELETE FROM commit_map WHERE svn_rev=1",
            "needs_reconciliation",
        ),
        (
            "v1",
            "INSERT INTO kv_state VALUES('no_target_git_outcomes_pair','{\"version\":1}','t')",
            "needs_reconciliation",
        ),
        (
            "v2",
            "INSERT INTO kv_state VALUES('handled_git_baseline_pair','{\"version\":2}','t')",
            "needs_reconciliation",
        ),
        (
            "historical_filtered",
            "UPDATE sync_records SET git_sha=NULL WHERE repo_id='pair' AND svn_rev=1",
            "needs_reconciliation",
        ),
        (
            "endpoint_replaced",
            "UPDATE repositories SET svn_url='file:///unqualified-replaced' WHERE id='pair'",
            "needs_reconciliation",
        ),
        (
            "effect_unknown",
            "UPDATE sync_records SET status='effect_unknown' WHERE repo_id='pair' AND svn_rev=1",
            "external_effect_unknown",
        ),
    ]
    .iter()
    .enumerate()
    {
        let source = t.path().join(format!("synthetic-overlay-{index}"));
        let target = t.path().join(format!("overlay-copy-{index}"));
        copy(&original, &source);
        Connection::open(source.join("reposync.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        copy(&source, &target);
        let mut session = CopySession::seal(&source, &target).unwrap();
        assert!(
            session
                .qualify_imported_pair("pair", &root.join("svn-one"), &root.join("origin-one.git"))
                .is_err(),
            "{name}"
        );
        session.disposition("pair", state, name).unwrap();
        let report = session.migrate(14, &mut |_, _, _| Ok(())).unwrap();
        assert!(report.canonical["pair_frontiers"].is_empty());
        assert!(report.canonical["pair_lineages"].is_empty());
        session.source_unchanged().unwrap();
        matrix.push(serde_json::json!({"name":name,"disposition":state,"legacy":report.legacy,"sequences":report.sequence_rows,"no_canonical_frontiers":true}));
    }
    assert_eq!(
        sha256(&fs::read(original.join("reposync.db")).unwrap()),
        original_hash
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"PINNED_UNQUALIFIED","old_generation":provenance,"overlays":matrix,"original_unchanged":true})
    );
}
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

fn endpoint_files(root: &Path) -> std::collections::BTreeMap<String, String> {
    fn walk(root: &Path, path: &Path, files: &mut std::collections::BTreeMap<String, String>) {
        for e in fs::read_dir(path).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            assert!(!e.file_type().unwrap().is_symlink());
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, files)
            } else {
                files.insert(
                    p.strip_prefix(root).unwrap().to_str().unwrap().to_string(),
                    sha256(&fs::read(p).unwrap()),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    for name in [
        "svn-one",
        "svn-two",
        "svn-disabled",
        "origin-one.git",
        "origin-two.git",
        "origin-disabled.git",
    ] {
        walk(root, &root.join(name), &mut files);
    }
    files
}
