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
    for repo in ["pair", "pair_two"] {
        let expected = provenance[repo]["git_sha"].as_str().unwrap();
        let baseline:(i64,String)=c.query_row("SELECT baseline_svn_rev,baseline_git_sha FROM pair_lineages WHERE repo_id=?1 AND generation=1",[repo],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(baseline, (2, expected.to_string()));
        let incoming:(i64,String,String)=c.query_row("SELECT handled_svn_rev,emitted_git_sha,authority_kind FROM pair_frontiers WHERE repo_id=?1 AND generation=1 AND direction='svn_to_git'",[repo],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(incoming, (2, expected.to_string(), "baseline".into()));
        let outgoing:(String,Option<i64>,Option<String>)=c.query_row("SELECT handled_git_sha,emitted_svn_rev,evidence_outcome_id FROM pair_frontiers WHERE repo_id=?1 AND generation=1 AND direction='git_to_svn'",[repo],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(outgoing, (expected.to_string(), None, None));
    }
    assert_eq!(c.query_row("SELECT count(*) FROM legacy_evidence_links e JOIN sync_records s ON e.legacy_table='sync_records' AND e.legacy_key=s.id WHERE e.repo_id!=s.repo_id",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM legacy_evidence_links WHERE legacy_table='commit_map'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
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
            "DELETE FROM sync_records WHERE repo_id='pair' AND svn_rev=(SELECT MIN(svn_rev) FROM sync_records WHERE repo_id='pair')",
            "needs_reconciliation",
        ),
        (
            "pruned_mapping",
            "DELETE FROM commit_map WHERE git_sha=(SELECT git_sha FROM sync_records WHERE repo_id='pair' ORDER BY svn_rev LIMIT 1)",
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
            "UPDATE sync_records SET git_sha=NULL WHERE repo_id='pair' AND svn_rev=(SELECT MIN(svn_rev) FROM sync_records WHERE repo_id='pair')",
            "needs_reconciliation",
        ),
        (
            "endpoint_replaced",
            "UPDATE repositories SET svn_url='file:///unqualified-replaced' WHERE id='pair'",
            "needs_reconciliation",
        ),
        (
            "effect_unknown",
            "UPDATE sync_records SET status='effect_unknown' WHERE repo_id='pair' AND svn_rev=(SELECT MIN(svn_rev) FROM sync_records WHERE repo_id='pair')",
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

#[test]
fn copy_storage_alias_rejection() {
    let mut matrix = Vec::new();
    for kind in ["source_hardlink", "outside_hardlink", "replace_after_seal"] {
        let (t, source, target) = fixture(&[1], &[]);
        let source_bytes = fs::read(source.join("reposync.db")).unwrap();
        let canary = t.path().join("outside-canary");
        fs::create_dir(&canary).unwrap();
        fs::copy(source.join("reposync.db"), canary.join("reposync.db")).unwrap();
        let before = fs::read(canary.join("reposync.db")).unwrap();
        let session = if kind == "replace_after_seal" {
            Some(CopySession::seal(&source, &target).unwrap())
        } else { None };
        fs::remove_file(target.join("reposync.db")).unwrap();
        fs::hard_link(if kind == "source_hardlink" { source.join("reposync.db") } else { canary.join("reposync.db") }, target.join("reposync.db")).unwrap();
        let rejected = match session {
            Some(session) => session.migrate(14, &mut |_, _, _| Ok(())).is_err(),
            None => match CopySession::seal(&source, &target) {
                Err(_) => true,
                Ok(session) => session.migrate(14, &mut |_, _, _| Ok(())).is_err(),
            },
        };
        assert!(rejected, "{kind}");
        assert!(fs::read(source.join("reposync.db")).unwrap() == source_bytes, "source damage: {kind}");
        assert!(fs::read(canary.join("reposync.db")).unwrap() == before, "canary damage: {kind}");
        assert_eq!(version(&source), 12);
        assert_eq!(version(&canary), 12);
        matrix.push(kind);
    }
    let (_t, s, c) = fixture(&[1], &[]);
    let session = CopySession::seal(&s, &c).unwrap();
    assert_eq!(session.migrate(14, &mut |_, _, _| Ok(())).unwrap().final_version, 14);
    session.source_unchanged().unwrap();
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({"case":"L01_STORAGE","rejected":matrix,"source_and_canary_bytes_and_version_unchanged":true,"independent_copy_success":true}));
}

#[test]
fn actual_legacy_admission_matrix() {
    let (t, root, provenance) = old_topology();
    let original = root.join("install");
    let original_bytes = fs::read(original.join("reposync.db")).unwrap();
    let endpoints = endpoint_files(&root);
    let sha = provenance["pair"]["git_sha"].as_str().unwrap();
    let mut matrix = Vec::new();
    let cases = vec![
        ("combined", "INSERT OR REPLACE INTO kv_state VALUES('last_svn_rev_pair','999','t'); INSERT OR REPLACE INTO kv_state VALUES('last_svn_rev','888','t'); INSERT OR REPLACE INTO watermarks VALUES('svn_rev','777','t')".to_string()),
        ("progress", "UPDATE import_progress SET repo_id='pair',current_rev=666".to_string()),
        ("ambiguous_progress", "UPDATE import_progress SET repo_id=NULL,current_rev=666".to_string()),
        ("unknown_effect", "INSERT INTO kv_state VALUES('effect_unknown_pair','{\"state\":\"unknown\"}','t')".to_string()),
        ("actual_v1", format!("INSERT INTO kv_state VALUES('handled_git_no_target_pair_{sha}','{{\"version\":1,\"outcome\":\"no_svn_delta\"}}','t')")),
        ("actual_v2", format!("INSERT INTO kv_state VALUES('handled_git_no_target_pair_{sha}','{{\"version\":2,\"outcome\":\"no_svn_delta\"}}','t')")),
        ("actual_filtered", format!("INSERT INTO kv_state VALUES('handled_git_no_target_pair_{sha}','{{\"version\":2,\"outcome\":\"filtered\"}}','t')")),
        ("actual_v3", format!("INSERT INTO kv_state VALUES('handled_git_no_target_pair_{sha}','{{\"version\":3,\"outcome\":\"no_svn_delta\"}}','t')")),
    ];
    for (index, (name, sql)) in cases.into_iter().enumerate() {
        let source = t.path().join(format!("real-vocabulary-{index}"));
        let target = t.path().join(format!("real-copy-{index}"));
        copy(&original, &source);
        Connection::open(source.join("reposync.db")).unwrap().execute_batch(&sql).unwrap();
        copy(&source, &target);
        let mut session = CopySession::seal(&source, &target).unwrap();
        assert!(session.qualify_imported_pair("pair", &root.join("svn-one"), &root.join("origin-one.git")).is_err(), "admitted {name}");
        // The real admission call must set the read-safe disposition itself.
        let report = session.migrate(14, &mut |_, _, _| Ok(())).unwrap();
        assert!(report.canonical["pair_lineages"].is_empty(), "{name}");
        let db = Connection::open(target.join("reposync.db")).unwrap();
        let state: String = db.query_row("SELECT disposition FROM repo_migration_state WHERE repo_id='pair'", [], |r| r.get(0)).unwrap();
        assert_eq!(state, if name=="unknown_effect" {"external_effect_unknown"} else {"needs_reconciliation"}, "{name}");
        session.source_unchanged().unwrap();
        matrix.push(serde_json::json!({"name":name,"state":state,"no_authority":true,"raw_legacy_preserved":report.legacy}));
    }
    // Unowned global references and similarly prefixed repository receipts are
    // not borrowed as this pair's directional authority.
    let source = t.path().join("benign-global"); let target = t.path().join("benign-copy");
    copy(&original, &source);
    Connection::open(source.join("reposync.db")).unwrap().execute_batch(&format!("INSERT OR REPLACE INTO kv_state VALUES('last_svn_rev','888','t'); INSERT INTO kv_state VALUES('handled_git_no_target_pair_two_{sha}','{{\"version\":1}}','t')")).unwrap();
    copy(&source,&target);
    let mut session=CopySession::seal(&source,&target).unwrap();
    session.qualify_imported_pair("pair",&root.join("svn-one"),&root.join("origin-one.git")).unwrap();
    assert!(session.qualify_imported_pair("pair_two",&root.join("svn-two"),&root.join("origin-two.git")).is_err());
    let report=session.migrate(14,&mut |_,_,_|Ok(())).unwrap(); assert_eq!(report.canonical["pair_lineages"].len(),1);
    assert_eq!(fs::read(original.join("reposync.db")).unwrap(),original_bytes); assert_eq!(endpoint_files(&root),endpoints);
    eprintln!("RELIABILITY_EVIDENCE {}",serde_json::json!({"case":"L02_ADMISSION","overlays":matrix,"exact_prefix_ownership":true,"benign_global_not_authority":true,"original_and_endpoints_unchanged":true}));
}

use reposync_core::db::candidate_readers::{Canonical, Direction, Raw, Scope, Source, Target};
fn reader_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf, CopySession, String) {
    use reposync_core::db::candidate_authority::{advance_frontier,ResolvedTransition};
    let (t,root,provenance)=old_topology();let source=root.join("install");let target=t.path().join("reader-copy");copy(&source,&target);
    let session=qualified(&source,&target,&root);session.migrate(14,&mut |_,_,_|Ok(())).unwrap();
    let mut c=Connection::open(target.join("reposync.db")).unwrap();c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let policy:String=c.query_row("SELECT policy_sha256 FROM pair_lineages WHERE repo_id='pair' AND generation=1",[],|r|r.get(0)).unwrap();
    let baseline=provenance["pair"]["git_sha"].as_str().unwrap().to_string();
    for (id,dir,pre,svn,git,outcome,target_git,target_svn) in [
        ("read-in-applied","svn_to_git","svn:2".to_string(),Some(3),None,"applied_verified",Some("d".repeat(40)),None),
        ("read-in-empty","svn_to_git","svn:3".to_string(),Some(4),None,"empty_no_target",None,None),
        ("read-out-applied","git_to_svn",format!("git:{baseline}"),None,Some("e".repeat(40)),"applied_verified",None,Some(3)),
        ("read-out-empty","git_to_svn",format!("git:{}","e".repeat(40)),None,Some("f".repeat(40)),"semantic_no_delta",None,None),
    ] {
        advance_frontier(&mut c,&ResolvedTransition{id:id.into(),repo_id:"pair".into(),generation:1,direction:dir.into(),predecessor_source_key:pre,source_svn_rev:svn,source_git_sha:git,outcome:outcome.into(),target_git_sha:target_git,target_svn_rev:target_svn,projection_version:1,policy_sha256:policy.clone(),evidence_json:"{\"labeled_structural_fixture_evidence\":true}".into()}).unwrap();
        if id=="read-in-applied" || id=="read-out-applied" {
            for (oid,key,rev,sha,kind,pred) in if dir=="svn_to_git" {
                vec![("pending-in","svn:5".into(),Some(5),None,"pending","svn:3".into()),("unknown-in","svn:8".into(),Some(8),None,"effect_unknown","svn:3".into())]
            }else{vec![("pending-out",format!("git:{}","a".repeat(40)),None,Some("a".repeat(40)),"pending",format!("git:{}","e".repeat(40))),("unknown-out",format!("git:{}","c".repeat(40)),None,Some("c".repeat(40)),"effect_unknown",format!("git:{}","e".repeat(40)))]} {
                c.execute("INSERT INTO pair_outcomes VALUES(?1,'pair',1,?2,?3,?4,?5,?6,?7,NULL,NULL,1,?8,'{}')",rusqlite::params![oid,dir,key,pred,rev,sha,kind,policy]).unwrap();
            }
        }
    }
    for (id,rev,sha,dir,interpretation) in [
        (200,3,Some("d".repeat(40)),"svn_to_git",Some("proved_typed_applied")),
        (201,4,None,"svn_to_git",Some("proved_typed_no_target")),
        (202,3,Some("e".repeat(40)),"git_to_svn",Some("proved_typed_applied")),
        (203,5,None,"svn_to_git",None),
        (204,6,Some("malformed-retained-display".into()),"svn_to_git",None),
    ]{
        c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,repo_id) VALUES(?1,?2,?3,?4,'t','pair')",rusqlite::params![id,rev,sha,dir]).unwrap();
        if let Some(i)=interpretation{c.execute("INSERT INTO legacy_evidence_links VALUES('pair',1,'commit_map',?1,?2)",rusqlite::params![id.to_string(),i]).unwrap();}
    }
    c.execute("INSERT INTO commit_map(id,svn_rev,git_sha,direction,synced_at,repo_id) VALUES(205,7,NULL,'svn_to_git','t',NULL)",[]).unwrap();
    drop(c);(t,root,target,session,baseline)
}
fn reader_proof(id:&str,root:&Path,target:&Path,session:&CopySession,run:impl FnOnce(&reposync_core::db::candidate_readers::CopyReaders<'_>)) {
    let db_bytes=fs::read(target.join("reposync.db")).unwrap();let endpoints=endpoint_files(root);let source_seal=session.source_seal().clone();
    let readers=session.readers().unwrap();run(&readers);drop(readers);
    assert!(fs::read(target.join("reposync.db")).unwrap()==db_bytes);assert_eq!(version(target),14);session.source_unchanged().unwrap();assert_eq!(session.source_seal(),&source_seal);assert_eq!(endpoint_files(root),endpoints);
    eprintln!("RELIABILITY_EVIDENCE {}",serde_json::json!({"case":id,"copy_db_bytes_and_version_unchanged":true,"sealed_source_config_refs_unchanged":true,"endpoints_unchanged":true,"explicit_generation":true,"read_only_immutable_connection":true}));
}
#[test]
fn typed_reader_lookup_matrix(){
    let (_t,root,target,session,baseline)=reader_fixture();
    reader_proof("T54_LOOKUP",&root,&target,&session,|r|{
        let incoming=r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(3)).unwrap();
        assert_eq!(incoming.canonical,Canonical::Mapped{target:Target::Git("d".repeat(40)),authority:"read-in-applied".into()});assert_eq!(incoming.legacy.len(),1);
        let outgoing=r.lookup("pair",Some(1),Direction::GitToSvn,&Source::Git("e".repeat(40))).unwrap();assert_eq!(outgoing.canonical,Canonical::Mapped{target:Target::Svn(3),authority:"read-out-applied".into()});assert_eq!(outgoing.legacy.len(),1);
        assert_eq!(r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(4)).unwrap().canonical,Canonical::NoTarget{outcome:"empty_no_target".into(),authority:"read-in-empty".into()});
        assert_eq!(r.lookup("pair",Some(1),Direction::GitToSvn,&Source::Git("f".repeat(40))).unwrap().canonical,Canonical::NoTarget{outcome:"semantic_no_delta".into(),authority:"read-out-empty".into()});
        for generation in [None,Some(2)]{let got=r.lookup("pair",generation,Direction::SvnToGit,&Source::Svn(3)).unwrap();assert_eq!(got.scope,Scope::MissingGeneration(generation));assert!(matches!(got.canonical,Canonical::Unresolved(_)));assert_eq!(got.legacy[0].values[2],Raw::Text("d".repeat(40)));}
        assert_eq!(r.lookup("pair",Some(1),Direction::GitToSvn,&Source::Git(baseline.clone())).unwrap().canonical,Canonical::HandledBaselineWithoutEmittedEffect);
        assert!(matches!(r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(5)).unwrap().canonical,Canonical::Unresolved(_)));
        assert_eq!(r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(8)).unwrap().canonical,Canonical::Missing);
        assert_eq!(r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(99)).unwrap().canonical,Canonical::Missing);
        assert_eq!(r.lookup("pair_two",Some(1),Direction::SvnToGit,&Source::Svn(3)).unwrap().canonical,Canonical::Missing);
        let malformed=r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(6)).unwrap();assert!(matches!(malformed.canonical,Canonical::Unresolved(_)));assert_eq!(malformed.legacy[0].values[2],Raw::Text("malformed-retained-display".into()));
        assert!(matches!(r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(7)).unwrap().canonical,Canonical::Unresolved(_)));
        assert!(r.lookup("pair",Some(1),Direction::GitToSvn,&Source::Svn(3)).is_err());
    });
    // Explicitly revoke the fixture link and introduce a duplicate, in labeled
    // copied state only. Both remain displayable and cannot be selected as truth.
    let db=Connection::open(target.join("reposync.db")).unwrap();db.execute("UPDATE commit_map SET git_sha='contradicts-linked-outcome' WHERE id=200",[]).unwrap();drop(db);
    assert!(matches!(session.readers().unwrap().lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(3)).unwrap().canonical,Canonical::Unresolved(_)));
    let db=Connection::open(target.join("reposync.db")).unwrap();db.execute("UPDATE commit_map SET git_sha=?1 WHERE id=200",["d".repeat(40)]).unwrap();db.execute("DELETE FROM legacy_evidence_links WHERE legacy_table='commit_map' AND legacy_key IN ('200','201')",[]).unwrap();drop(db);
    assert!(matches!(session.readers().unwrap().lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(4)).unwrap().canonical,Canonical::Unresolved(_)));
    assert!(matches!(session.readers().unwrap().lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(3)).unwrap().canonical,Canonical::Unresolved(_)));
    let db=Connection::open(target.join("reposync.db")).unwrap();db.execute("INSERT INTO commit_map(svn_rev,git_sha,direction,synced_at,repo_id) VALUES(4,NULL,'svn_to_git','t','pair')",[]).unwrap();drop(db);
    let got=session.readers().unwrap().lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(4)).unwrap();assert_eq!(got.legacy.len(),2);assert_eq!(got.canonical,Canonical::Unresolved("ambiguous_owned_legacy_rows".into()));
}
#[test]
fn typed_reader_list_matrix(){
    let (_t,root,target,session,_)=reader_fixture();
    let db=Connection::open(target.join("reposync.db")).unwrap();let expected:Vec<i64>=db.prepare("SELECT id FROM commit_map ORDER BY id").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();drop(db);
    reader_proof("T54_LIST",&root,&target,&session,|r|{
        let mut actual=Vec::new();let mut after=0;
        loop{let p=r.legacy_page(after,2).unwrap();if p.is_empty(){break}after=p.last().unwrap().id;actual.extend(p.into_iter().map(|r|r.id));}assert_eq!(actual,expected);
        let p=r.list("pair",Some(1),Direction::SvnToGit,199,2).unwrap();assert_eq!(p.rows.iter().map(|x|x.legacy.id).collect::<Vec<_>>(),vec![200,201]);assert_eq!(p.next_after_id,Some(201));assert!(matches!(p.rows[1].canonical,Canonical::NoTarget{..}));
        let p2=r.list("pair",Some(1),Direction::SvnToGit,201,2).unwrap();assert_eq!(p2.rows.iter().map(|x|x.legacy.id).collect::<Vec<_>>(),vec![203,204]);assert!(p2.rows.iter().all(|x|matches!(x.canonical,Canonical::Unresolved(_))));
        let p3=r.list("pair",Some(1),Direction::SvnToGit,204,2).unwrap();assert_eq!(p3.rows.len(),1);assert_eq!(p3.rows[0].legacy.id,205);assert_eq!(p3.rows[0].legacy.values[2],Raw::Null);assert!(matches!(p3.rows[0].canonical,Canonical::Unresolved(_)));
        assert!(r.legacy_page(0,0).is_err());assert!(r.list("pair",None,Direction::SvnToGit,0,201).is_err());
    });
}
#[test]
fn typed_reader_status_matrix(){
    let (_t,root,target,session,_)=reader_fixture();
    reader_proof("T54_STATUS",&root,&target,&session,|r|{
        let status=r.status("pair",Some(1)).unwrap();assert_eq!(status.scope,Scope::Qualified(1));assert_eq!(status.enabled,Some(true));assert_eq!(status.frontiers.len(),2);
        let incoming=&status.frontiers[0];assert_eq!(incoming.handled,Source::Svn(4));assert_eq!(incoming.current_target,None);assert_eq!(incoming.last_emitted.target,Some(Target::Git("d".repeat(40))));
        let outgoing=&status.frontiers[1];assert_eq!(outgoing.handled,Source::Git("f".repeat(40)));assert_eq!(outgoing.current_target,None);assert_eq!(outgoing.last_emitted.target,Some(Target::Svn(3)));
        assert_eq!(r.status("pair_disabled",Some(1)).unwrap().scope,Scope::Disabled);
        for g in [None,Some(99)]{let status=r.status("pair",g).unwrap();assert_eq!(status.scope,Scope::MissingGeneration(g));assert!(status.frontiers.is_empty());}
        assert_eq!(r.status("missing",Some(1)).unwrap().scope,Scope::MissingRepository);
    });
    let db=Connection::open(target.join("reposync.db")).unwrap();db.execute("UPDATE repo_migration_state SET disposition='needs_reconciliation' WHERE repo_id='pair'",[]).unwrap();drop(db);
    let status=session.readers().unwrap().status("pair",Some(1)).unwrap();assert_eq!(status.scope,Scope::NotQualified("needs_reconciliation".into()));assert!(status.frontiers.is_empty());
}
#[test]
fn typed_reader_emitted_matrix(){
    let (_t,root,target,session,_)=reader_fixture();
    reader_proof("T54_EMITTED",&root,&target,&session,|r|{
        for (d,target,authority,nonhandled) in [(Direction::SvnToGit,Target::Git("d".repeat(40)),"read-in-applied",vec![("pending-in".into(),"pending".into()),("unknown-in".into(),"effect_unknown".into())]),(Direction::GitToSvn,Target::Svn(3),"read-out-applied",vec![("pending-out".into(),"pending".into()),("unknown-out".into(),"effect_unknown".into())])]{let emitted=r.last_emitted("pair",Some(1),d).unwrap();assert_eq!(emitted.target,Some(target));assert_eq!(emitted.authority.as_deref(),Some(authority));assert_eq!(emitted.nonhandled_records,nonhandled);}
        assert_eq!(r.last_emitted("pair_two",Some(1),Direction::GitToSvn).unwrap().target,None);
        assert!(matches!(r.last_emitted("pair_two",Some(1),Direction::SvnToGit).unwrap().target,Some(Target::Git(_))));
        assert_eq!(r.last_emitted("pair",Some(2),Direction::SvnToGit).unwrap().target,None);
    });
}
#[test]
fn typed_reader_no_write_matrix(){
    let (_t,root,target,session,_)=reader_fixture();
    reader_proof("T54_READONLY",&root,&target,&session,|r|{for _ in 0..2{r.lookup("pair",Some(1),Direction::SvnToGit,&Source::Svn(4)).unwrap();r.list("pair",Some(1),Direction::SvnToGit,0,200).unwrap();r.legacy_page(0,200).unwrap();r.status("pair",Some(1)).unwrap();r.last_emitted("pair",Some(1),Direction::GitToSvn).unwrap();}});
    let before=fs::read(target.join("reposync.db")).unwrap();fs::write(target.join("reposync.db-journal"),b"not-quiescent").unwrap();assert!(session.readers().is_err());assert!(fs::read(target.join("reposync.db")).unwrap()==before);session.source_unchanged().unwrap();
}
