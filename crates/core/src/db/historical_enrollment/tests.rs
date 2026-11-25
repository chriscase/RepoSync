use super::*;

const ID: &str = "23bb65a0-df0b-4d69-b9f3-1f344c13e274";
const OTHER: &str = "17cec3c5-56f7-48f2-bba8-c1e59bdf6db7";
const TIP: &str = "0123456789012345678901234567890123456789";

fn fixture(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../daemon/tests/fixtures/schema-v12.sql"
    )))
    .unwrap();
    conn.execute_batch(schema::installed_team::INSTALLED_V13)
        .unwrap();
    conn.execute_batch(schema::installed_team::INSTALLED_V14)
        .unwrap();
    conn.pragma_update(None, "user_version", 14).unwrap();
    for id in [ID, OTHER] {
        conn.execute("INSERT INTO repositories(id,name,svn_url,svn_branch,git_api_url,git_repo,git_branch,enabled,created_at,updated_at) VALUES(?1,?1,'file:///svn','trunk','file:///git','fixture/project','main',0,'t','t')",[id]).unwrap();
    }
    history(&conn, ID);
    conn
}

fn history(conn: &Connection, id: &str) {
    conn.execute(
        "UPDATE repositories SET last_svn_rev=2,last_git_sha=?2 WHERE id=?1",
        params![id, TIP],
    )
    .unwrap();
    conn.execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at) VALUES(?1,2,?2,'svn_to_git','t')",params![id,TIP]).unwrap();
    conn.execute("INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,timestamp,synced_at,status) VALUES(?1,?1,2,?2,'svn_to_git','t','t','applied')",params![id,TIP]).unwrap();
}

fn checked(pair: LegacyPairPins, planned: bool) -> Reviewed {
    Reviewed {
        canonical: CanonicalLineage {
            scope_uuid: pair.scope_uuid.clone(),
            generation: pair.generation,
            repo_id: pair.repo_id.clone(),
            svn_uuid: "aaaa0000-0000-0000-0000-000000000001".into(),
            svn_root_url: pair.svn_url.clone(),
            svn_url: "file:///svn/trunk".into(),
            svn_branch_path: "/trunk".into(),
            incarnation_created_rev: 1,
            copy_from_path: None,
            copy_from_rev: None,
            git_baseline_sha: Some(pair.last_git_sha.clone()),
            git_ref: Some(pair.git_branch.clone()),
            projection_version: String::new(),
            policy_hash: String::new(),
        },
        pair,
        digest: "1".repeat(64),
        history: "2".repeat(64),
        provenance: json!({"source":"fixture"}),
        planned,
    }
}

#[test]
fn prospective_refuses_another_imported_pair_before_migrating() {
    let _guard = crate::writer_fence::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    let conn = fixture(&path);
    assert!(read_planned_cutover_pins(&conn, ID).is_ok());
    history(&conn, OTHER);
    let error = prepare_historical_cutover(dir.path(), ID, &"1".repeat(64), |_, _| {
        panic!("verified a stranded database")
    })
    .unwrap_err();
    assert!(error.to_string().contains(OTHER));
    assert!(is_installed(&conn).unwrap());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM commit_map", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM audit_log", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM kv_state", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn dirty_or_orphaned_legacy_history_is_never_an_empty_registration() {
    let db = fixture(Path::new(":memory:"));
    let other = |sql| {
        db.execute_batch("SAVEPOINT test").unwrap();
        db.execute_batch(sql).unwrap();
        assert!(read_planned_cutover_pins(&db, ID).is_err());
        db.execute_batch("ROLLBACK TO test; RELEASE test").unwrap();
    };
    other("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at) VALUES('removed',3,'abc','svn_to_git','t')");
    other("INSERT INTO kv_state(key,value,updated_at) VALUES('last_svn_rev_removed','3','t')");
    other("INSERT INTO conflicts(id,file_path,conflict_type,created_at) VALUES('c','file','text','t')");
    other("INSERT INTO audit_log(action,svn_rev,created_at) VALUES('unidentified',2,'t')");
    other("INSERT INTO watermarks(source,value,updated_at) VALUES('svn_rev','99','t')");
}

#[test]
fn failed_audit_rolls_back_original_schema_scope_and_writer_claim() {
    let _guard = crate::writer_fence::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    let conn = fixture(&path);
    let before = read_planned_cutover_pins(&conn, ID).unwrap();
    let result = Database::open_existing_for_historical_enrollment(
        dir.path(),
        |tx| {
            let pins = read_planned_cutover_pins(tx, ID)?;
            conn.busy_timeout(std::time::Duration::ZERO)?;
            let err = conn
                .execute(
                    "UPDATE repositories SET git_branch='elsewhere' WHERE id=?1",
                    [ID],
                )
                .unwrap_err();
            assert_eq!(
                err.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
            Ok(checked(pins, true))
        },
        |tx, pins| {
            tx.execute_batch("CREATE TEMP TRIGGER fail_audit BEFORE INSERT ON main.audit_log
          WHEN NEW.action='historical_lineage_enrolled' BEGIN SELECT RAISE(ABORT,'injected failure'); END;")?;
            persist(tx, pins, true)
        },
    );
    assert!(result.is_err());
    assert_eq!(read_planned_cutover_pins(&conn, ID).unwrap(), before);
    assert!(is_installed(&conn).unwrap());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM kv_state", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='canonical_lineage'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn canonical_enrollment_is_idempotent_and_rejects_changed_retained_state() {
    let _guard = crate::writer_fence::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    let conn = fixture(&path);
    let preview = read_planned_cutover_pins(&conn, ID).unwrap();
    for expected in [
        EnrollmentOutcome::Enrolled,
        EnrollmentOutcome::AlreadyEnrolled,
    ] {
        let (_owner, result) = Database::open_existing_for_historical_enrollment(
            dir.path(),
            |tx| {
                let pins = read_planned_cutover_pins(tx, ID)?;
                assert_eq!(pins, preview);
                Ok((is_installed(tx)?, checked(pins, true)))
            },
            |tx, (original, checked)| persist(tx, checked, original),
        )
        .unwrap();
        assert_eq!(result, expected);
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM audit_log WHERE action=?1",
            [ACTION],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM canonical_lineage WHERE repo_id=?1",
            [OTHER],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT enabled FROM repositories WHERE id=?1", [ID], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    conn.execute(
        "UPDATE sync_records SET message='changed after review' WHERE repo_id=?1",
        [ID],
    )
    .unwrap();
    assert!(read_planned_cutover_pins(&conn, ID).is_err());
    // A read-only preview has no authority to fill a missing scope on partial storage.
    conn.execute("UPDATE repositories SET scope_uuid='' WHERE id=?1", [ID])
        .unwrap();
    assert!(read_legacy_pair_pins(&conn, ID).is_err());
    assert_eq!(
        conn.query_row(
            "SELECT scope_uuid FROM repositories WHERE id=?1",
            [ID],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        ""
    );
}

#[test]
fn refused_retrospective_proof_does_not_retire_a_previous_writer_lease() {
    let _guard = crate::writer_fence::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    let conn = fixture(&path);
    let owner = Database::open_cli_writer(dir.path()).unwrap();
    let key = "writer_fence_v1:lease";
    let raw: String = conn
        .query_row("SELECT value FROM kv_state WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .unwrap();
    drop(owner);
    // This is a lease of a previous, definitively dead local worker, eligible to be
    // claimed via ordinary startup. A failed retrospective check must not retire it.
    let mut previous: Value = serde_json::from_str(&raw).unwrap();
    previous["holder_pid"] = json!(i32::MAX);
    let previous = previous.to_string();
    conn.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'original')",
        params![key, previous],
    )
    .unwrap();
    let error = enroll_verified_lineage(dir.path(), ID, &"1".repeat(64), |_, _| {
        Err(refused("synthetic changed witness"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("synthetic changed witness"));
    let after: (String, String) = conn
        .query_row(
            "SELECT value,updated_at FROM kv_state WHERE key=?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, (previous, "original".to_string()));
    assert_eq!(
        conn.query_row("SELECT count(*) FROM canonical_lineage", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(receipt(&conn, ID).unwrap().is_none());
}

#[test]
fn old_import_progress_counts_log_entries_on_a_sparse_svn_branch() {
    let conn = fixture(Path::new(":memory:"));
    let first = "1".repeat(40);
    conn.execute("UPDATE repositories SET last_svn_rev=12 WHERE id=?1", [ID])
        .unwrap();
    conn.execute("UPDATE commit_map SET svn_rev=7,git_sha=?2,synced_at='2025-01-02T00:00:00Z' WHERE repo_id=?1",params![ID,first]).unwrap();
    conn.execute("UPDATE sync_records SET svn_rev=7,git_sha=?2,synced_at='2025-01-02T00:00:00Z' WHERE repo_id=?1",params![ID,first]).unwrap();
    conn.execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at) VALUES(?1,12,?2,'svn_to_git','2025-01-02T00:00:01Z')",params![ID,TIP]).unwrap();
    conn.execute("INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,status,timestamp,synced_at) VALUES('last',?1,12,?2,'svn_to_git','applied','t','2025-01-02T00:00:01Z')",params![ID,TIP]).unwrap();
    conn.execute_batch("INSERT INTO import_progress(id,phase,current_rev,total_revs,commits_created,batches_pushed,started_at,completed_at)
        VALUES(1,'completed',2,2,2,1,'2025-01-01T00:00:00Z','2025-01-03T00:00:00Z');
        INSERT INTO audit_log(action,direction,svn_rev,details,created_at,success) VALUES('import_full','svn_to_git',12,'Full history import: 2 commits from 2 revisions','2025-01-02T00:00:02Z',1)").unwrap();
    assert_eq!(
        read_planned_cutover_pins(&conn, ID).unwrap().mappings.len(),
        2
    );
    conn.execute_batch("UPDATE import_progress SET current_rev=12,total_revs=99; UPDATE audit_log SET details='Full history import: 2 commits from 99 revisions'").unwrap();
    assert!(read_planned_cutover_pins(&conn, ID).is_err());
}
