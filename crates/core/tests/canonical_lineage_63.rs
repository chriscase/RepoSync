//! Dedicated #63 regressions: operational v14 canonical lineage, typed
//! checkpoints, SVN UUID/path-incarnation preflight, and RS-11 credential
//! origin/redaction canaries. Keep `team_mode_e2e.rs` append-only.
use std::path::Path;
use std::process::Command;

use reposync_core::db::import_operations::{
    import_operation_has_retained_effects, import_target_fingerprint, ImportOperationState,
};
use reposync_core::db::lineage_checkpoints::{
    adopt_or_hold_live_svn_identity, independently_established_svn_uuid, load_canonical_lineage,
    load_directional_checkpoint, pin_canonical_lineage, verify_or_pin_live_svn_identity,
    CanonicalLineage, CheckpointDirection, LiveSvnIdentity,
};
use reposync_core::db::queries::CredentialOrigin;
use reposync_core::db::repo_scope_identity::repository_scope_uuid;
use reposync_core::db::schema::{self, CURRENT_SCHEMA_VERSION};
use reposync_core::db::Database;
use reposync_core::errors::{redact_vcs_error_detail, DatabaseError, SyncError};
use reposync_core::pair_refresh::PAIR_GENERATION;
use reposync_core::svn::SvnClient;
use rusqlite::Connection;

fn svn_available() -> bool {
    Command::new("svn").arg("--version").output().is_ok()
        && Command::new("svnadmin").arg("--version").output().is_ok()
        && Command::new("svnlook").arg("--version").output().is_ok()
}

fn version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn pair_table_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'pair_%'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn canonical_table_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name IN ('canonical_lineage','canonical_checkpoints','canonical_mapping_outcomes')",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn create_svn_repo(root: &Path) -> String {
    std::fs::create_dir_all(root).unwrap();
    let repo = root.join("svn_repo");
    assert!(Command::new("svnadmin")
        .args(["create", repo.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let url = format!("{}/trunk", file_url(&repo));
    assert!(Command::new("svn")
        .args(["mkdir", "-m", "trunk", &url])
        .status()
        .unwrap()
        .success());
    url
}

fn svn_uuid(repo: &Path) -> String {
    let output = Command::new("svnlook")
        .args(["uuid", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn insert_pair(db: &Database, svn_url: &str) {
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('pair','pair',?1,'trunk','','local','','r','main','team',60,0,0,1,'t','t',0,'','idle',0,0)",
            [svn_url],
        )
        .unwrap();
    repository_scope_uuid(&db.conn(), "pair").unwrap();
}

#[test]
fn operational_v14_has_canonical_tables_not_pair_tables() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let conn = db.conn();
    assert_eq!(version(&conn), CURRENT_SCHEMA_VERSION as i64);
    assert_eq!(canonical_table_count(&conn), 3);
    assert_eq!(pair_table_count(&conn), 0);
    assert_eq!(
        conn.query_row(
            "SELECT [notnull] FROM pragma_table_info('commit_map') WHERE name='git_sha'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_OPERATIONAL_V14_NO_PAIR_TABLES",
            "user_version":CURRENT_SCHEMA_VERSION,
            "canonical_tables":3,
            "candidate_pair_tables":0,
            "commit_map_git_sha_notnull":1
        })
    );
}

#[test]
fn lone_legacy_v12_stays_deferred_until_exclusive_owner_reaches_v14() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    {
        let conn = Connection::open(&path).unwrap();
        schema::run_migrations_up_to(&conn, 12).unwrap();
        conn.execute(
            "INSERT INTO repositories
             (id,name,svn_url,created_at,updated_at,last_svn_rev,last_git_sha,last_sync_at,enabled,sync_status)
             VALUES('legacy','Legacy','file:///unused','t','t',42,'1111111111111111111111111111111111111111','t',1,'idle')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES('last_git_sha_legacy','1111111111111111111111111111111111111111','t')",
            [],
        )
        .unwrap();
        assert_eq!(version(&conn), 12);
    }
    {
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        assert_eq!(version(&db.conn()), 12);
        assert_eq!(canonical_table_count(&db.conn()), 0);
    }
    let (db, _owner) = Database::open_with_exclusive_owner(tmp.path()).unwrap();
    assert_eq!(version(&db.conn()), CURRENT_SCHEMA_VERSION as i64);
    assert_eq!(canonical_table_count(&db.conn()), 3);
    assert_eq!(pair_table_count(&db.conn()), 0);
    let repo = db.get_repository("legacy").unwrap().unwrap();
    assert_eq!(repo.last_svn_rev, 42);
    assert_eq!(
        repo.last_git_sha,
        "1111111111111111111111111111111111111111"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_DEFERRED_V12_THEN_V14",
            "initialize_user_version":12,
            "owned_user_version":CURRENT_SCHEMA_VERSION,
            "legacy_cursors_preserved":true
        })
    );
}

#[test]
fn future_schema_is_refused_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    {
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        db.conn().pragma_update(None, "user_version", 99).unwrap();
    }
    match Database::new(&path) {
        Err(DatabaseError::UnsupportedSchema { found, supported }) => {
            assert_eq!(found, 99);
            assert_eq!(supported, CURRENT_SCHEMA_VERSION);
        }
        Err(other) => panic!("expected UnsupportedSchema, got {other}"),
        Ok(_) => panic!("expected UnsupportedSchema, opened a future schema"),
    }
    let version: i64 = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 99);
}

#[test]
fn rs11_dummy_tokens_are_redacted_and_parent_rotation_preserves_child() {
    let dummy = "ghp_canaryRS11tokenABCDEF";
    let url = format!("https://x-access-token:{dummy}@git.invalid/repo.git");
    let redacted = redact_vcs_error_detail(&format!("fatal: {url}"));
    assert!(!redacted.contains(dummy));
    assert!(!redacted.contains("ghp_canary"));
    assert!(redacted.contains("[REDACTED]") || redacted.contains("[REDACTED_TOKEN]"));

    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id)
             VALUES ('root','Root','file:///r','trunk','','github','https://api.github.com','o/r','main','team',60,0,0,1,'t','t',0,'','idle',0,0,NULL)",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id)
             VALUES ('child','Child','file:///c','trunk','','github','https://api.github.com','o/c','main','team',60,0,0,1,'t','t',0,'','idle',0,0,'root')",
            [],
        )
        .unwrap();
    db.set_state("secret_git_token_root", "parent-token")
        .unwrap();
    db.set_state("secret_git_token_child", dummy).unwrap();
    assert_eq!(
        db.resolve_credential_chain_state("child", "secret_git_token")
            .origin,
        CredentialOrigin::Explicit
    );
    db.save_repository_credentials("root", None, Some("rotated-parent"), "admin", None, None)
        .unwrap();
    assert_eq!(
        db.get_state("secret_git_token_child").unwrap().as_deref(),
        Some(dummy)
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_RS11_REDACTION_AND_PARENT_ROTATION",
            "dummy_token_redacted":true,
            "child_override_preserved":true
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_svn_uuid_change_holds_pair_before_up_to_date_return() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let url_a = create_svn_repo(&tmp.path().join("a"));
    let url_b = create_svn_repo(&tmp.path().join("b"));
    let uuid_a = svn_uuid(&tmp.path().join("a/svn_repo"));
    let uuid_b = svn_uuid(&tmp.path().join("b/svn_repo"));
    assert_ne!(uuid_a, uuid_b);

    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    insert_pair(&db, &url_a);

    let svn_a = SvnClient::new(&url_a, "", "");
    let info_a = svn_a.info().await.unwrap();
    verify_or_pin_live_svn_identity(&db, "pair", &svn_a, &info_a)
        .await
        .unwrap();
    let pinned = load_canonical_lineage(&db, "pair").unwrap().unwrap();
    assert_eq!(pinned.svn_uuid, uuid_a);

    let svn_b = SvnClient::new(&url_b, "", "");
    let info_b = svn_b.info().await.unwrap();
    let err = verify_or_pin_live_svn_identity(&db, "pair", &svn_b, &info_b)
        .await
        .unwrap_err();
    match err {
        SyncError::HistoryBlocked { reason, .. } => assert_eq!(reason, "svn_identity_changed"),
        other => panic!("expected HistoryBlocked, got {other:?}"),
    }
    assert_eq!(
        db.get_repository("pair").unwrap().unwrap().sync_status,
        "reconciliation_required"
    );
    let still = load_canonical_lineage(&db, "pair").unwrap().unwrap();
    assert_eq!(still.svn_uuid, uuid_a);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_SVN_UUID_IDENTITY_HOLD",
            "uuid_a":uuid_a,
            "uuid_b":uuid_b,
            "held":true,
            "lineage_immutable":true
        })
    );
}

#[test]
fn recreated_path_incarnation_does_not_reuse_stale_lineage() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    insert_pair(&db, "file:///repo/trunk");
    let scope = {
        let conn = db.conn();
        let scope = repository_scope_uuid(&conn, "pair").unwrap();
        pin_canonical_lineage(
            &conn,
            &CanonicalLineage {
                scope_uuid: scope.clone(),
                generation: PAIR_GENERATION,
                repo_id: "pair".into(),
                svn_uuid: "same-uuid".into(),
                svn_root_url: "file:///repo".into(),
                svn_url: "file:///repo/trunk".into(),
                svn_branch_path: "trunk".into(),
                incarnation_created_rev: 2,
                copy_from_path: None,
                copy_from_rev: None,
                git_baseline_sha: None,
                git_ref: None,
                projection_version: String::new(),
                policy_hash: String::new(),
            },
        )
        .unwrap();
        scope
    };
    let mut recreated = load_canonical_lineage(&db, "pair").unwrap().unwrap();
    recreated.incarnation_created_rev = 41;
    recreated.copy_from_path = Some("/attic/trunk".into());
    recreated.copy_from_rev = Some(40);
    let conn = db.conn();
    assert!(pin_canonical_lineage(&conn, &recreated).is_err());
    let checkpoint = load_directional_checkpoint(
        &conn,
        &scope,
        PAIR_GENERATION,
        CheckpointDirection::SvnToGit,
    )
    .unwrap();
    assert!(checkpoint.is_none());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_PATH_INCARNATION_HOLD",
            "stale_lineage_reused":false
        })
    );
}

fn replacement_identity() -> LiveSvnIdentity {
    LiveSvnIdentity {
        uuid: "uuid-replacement".into(),
        root_url: "file:///repo".into(),
        url: "file:///repo/trunk".into(),
        branch_path: "trunk".into(),
        incarnation_created_rev: 1,
        copy_from_path: None,
        copy_from_rev: None,
    }
}

#[test]
fn upgraded_engine_same_url_different_uuid_equal_revision_holds_before_any_pin() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    insert_pair(&db, "file:///repo/trunk");
    db.conn()
        .execute(
            "UPDATE repositories SET last_svn_rev = 5, last_git_sha = ?1 WHERE id = 'pair'",
            ["c".repeat(40)],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (5, ?1, 'svn_to_git', 't', 'a', 'A', 'pair')",
            ["c".repeat(40)],
        )
        .unwrap();
    assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
    let err = adopt_or_hold_live_svn_identity(&db, "pair", &replacement_identity()).unwrap_err();
    match err {
        SyncError::HistoryBlocked { reason, .. } => assert_eq!(reason, "svn_identity_unproved"),
        other => panic!("expected HistoryBlocked, got {other:?}"),
    }
    assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
    let reopen = adopt_or_hold_live_svn_identity(&db, "pair", &replacement_identity()).unwrap_err();
    match reopen {
        SyncError::HistoryBlocked { reason, .. } => assert_eq!(reason, "svn_identity_unproved"),
        other => panic!("expected HistoryBlocked on reopen, got {other:?}"),
    }
    assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_LEGACY_ADOPTION_UNPROVED_HOLD",
            "pinned":false,
            "reopen_held":true
        })
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn writer_fence_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &LOCK
}

fn svn_commit_file(wc: &Path, name: &str, body: &str, message: &str) {
    std::fs::write(wc.join(name), body).unwrap();
    let status = Command::new("svn")
        .args(["status", wc.join(name).to_str().unwrap()])
        .output()
        .unwrap();
    if String::from_utf8_lossy(&status.stdout).contains('?') {
        assert!(Command::new("svn")
            .args(["add", wc.join(name).to_str().unwrap(), "--non-interactive"])
            .status()
            .unwrap()
            .success());
    }
    assert!(Command::new("svn")
        .args(["commit", "-m", message, "--non-interactive"])
        .current_dir(wc)
        .status()
        .unwrap()
        .success());
}

fn git_commit_prefix(workdir: &Path, body: &[u8]) -> String {
    let repo = git2::Repository::open(workdir).unwrap();
    let oid = repo.blob(body).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("one.txt", oid, 0o100644).unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let commit = repo
        .commit(Some("HEAD"), &sig, &sig, "predecessor prefix", &tree, &[])
        .unwrap();
    assert!(Command::new("git")
        .args(["checkout", "-B", "main"])
        .current_dir(workdir)
        .status()
        .unwrap()
        .success());
    commit.to_string()
}

fn origin_commit_count(bare: &Path) -> usize {
    let repo = git2::Repository::open(bare).unwrap();
    let mut walk = repo.revwalk().unwrap();
    walk.push_ref("refs/heads/main").unwrap();
    walk.count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn predecessor_partial_full_import_holds_same_url_replacement_uuid() {
    let _serial = writer_fence_test_lock().lock().await;
    if !svn_available() || !git_available() {
        eprintln!("SKIP: svn/svnadmin/git not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let svn_path = tmp.path().join("svn_repo");
    assert!(Command::new("svnadmin")
        .args(["create", svn_path.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let svn_url = file_url(&svn_path);
    let original_uuid = svn_uuid(&svn_path);
    let wc = tmp.path().join("svn_wc");
    assert!(Command::new("svn")
        .args([
            "checkout",
            &svn_url,
            wc.to_str().unwrap(),
            "--non-interactive"
        ])
        .status()
        .unwrap()
        .success());
    svn_commit_file(&wc, "one.txt", "one\n", "r1");
    svn_commit_file(&wc, "two.txt", "two\n", "r2");
    svn_commit_file(&wc, "three.txt", "three\n", "r3");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    let git_client = reposync_core::git::GitClient::init(&git_work).unwrap();
    assert!(Command::new("git")
        .args(["checkout", "-B", "main"])
        .current_dir(&git_work)
        .status()
        .unwrap()
        .success());
    git2::Repository::open(&git_work)
        .unwrap()
        .remote("origin", bare.to_str().unwrap())
        .unwrap();
    let prefix_sha = git_commit_prefix(&git_work, b"one\n");
    assert!(Command::new("git")
        .args(["push", "origin", "main"])
        .current_dir(&git_work)
        .status()
        .unwrap()
        .success());
    assert_eq!(origin_commit_count(&bare), 1);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Database::new(data_dir.join("state.db")).unwrap();
    db.initialize().unwrap();
    let _fence = reposync_core::writer_fence::claim(&db, &data_dir).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let repo = reposync_core::models::Repository {
        id: "legacy-import".into(),
        name: "Legacy import".into(),
        svn_url: svn_url.clone(),
        svn_branch: String::new(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: format!("file://{}", git_work.display()),
        git_repo: "legacy/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    };
    db.insert_repository(&repo).unwrap();
    let fingerprint = import_target_fingerprint(&repo, &git_work);
    let op = db
        .create_import_operation("legacy-import", "admin", "predecessor-full", &fingerprint)
        .unwrap();
    db.start_import_operation("legacy-import", &op.id).unwrap();
    db.note_import_total("legacy-import", &op.id, 3).unwrap();
    db.note_import_local("legacy-import", &op.id, 1, &prefix_sha, 1, 1)
        .unwrap();
    db.stage_import_commit_map(
        "legacy-import",
        &op.id,
        1,
        &prefix_sha,
        "svn",
        "git <fixture@example.invalid>",
    )
    .unwrap();
    db.begin_import_publication("legacy-import", &op.id, "refs/heads/main", &prefix_sha)
        .unwrap();
    db.confirm_import_publication("legacy-import", &op.id, &prefix_sha)
        .unwrap();
    let predecessor = db
        .get_import_operation("legacy-import", &op.id)
        .unwrap()
        .unwrap();
    assert!(predecessor.snapshot_pin.is_none());
    assert!(import_operation_has_retained_effects(&predecessor));
    assert!(
        independently_established_svn_uuid(&db.conn(), "legacy-import")
            .unwrap()
            .is_none()
    );
    db.finish_import_operation(
        "legacy-import",
        &op.id,
        ImportOperationState::ReconciliationRequired,
        "worker stopped before a verified terminal result; inspect local and remote refs",
    )
    .unwrap();
    let reconciled = db
        .reconcile_verified_import(
            "legacy-import",
            &op.id,
            &git_work,
            "refs/heads/main",
            &prefix_sha,
        )
        .unwrap();
    assert!(reconciled.resume_authorized);
    assert!(!reconciled.completed);

    std::fs::remove_dir_all(&svn_path).unwrap();
    assert!(Command::new("svnadmin")
        .args(["create", svn_path.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let replacement_uuid = svn_uuid(&svn_path);
    assert_ne!(replacement_uuid, original_uuid);
    let wc2 = tmp.path().join("svn_wc_replacement");
    assert!(Command::new("svn")
        .args([
            "checkout",
            &svn_url,
            wc2.to_str().unwrap(),
            "--non-interactive"
        ])
        .status()
        .unwrap()
        .success());
    svn_commit_file(&wc2, "one.txt", "replacement-one\n", "r1");
    svn_commit_file(&wc2, "two.txt", "replacement-two\n", "r2");
    svn_commit_file(&wc2, "three.txt", "replacement-three\n", "r3");

    db.resume_import_operation("legacy-import", &op.id).unwrap();
    let identity = reposync_core::identity::IdentityMapper::new(
        &reposync_core::config::IdentityConfig::default(),
    )
    .unwrap();
    let outcome = reposync_core::import::run_full_import(
        &reposync_core::svn::SvnClient::new(&svn_url, "", ""),
        &std::sync::Arc::new(std::sync::Mutex::new(git_client)),
        &identity,
        &db,
        &reposync_core::file_policy::FilePolicy::new(0, Vec::new()),
        &reposync_core::import::ImportConfig {
            committer_name: "Fixture".into(),
            committer_email: "fixture@example.invalid".into(),
            remote_name: "origin".into(),
            branch: "main".into(),
            push_token: None,
            message_prefix: None,
            trunk_path: String::new(),
        },
        reposync_core::import::ImportRunState {
            progress: std::sync::Arc::new(tokio::sync::RwLock::new(
                reposync_core::import::ImportProgress::default(),
            )),
            ws_broadcast: None,
            repo_id: Some("legacy-import".into()),
            operation_id: Some(op.id.clone()),
            cancel_signal: None,
        },
    )
    .await
    .expect("identity hold is an import outcome, not a transport crash");
    match outcome {
        reposync_core::import::ImportOutcome::ReconciliationRequired { commits, reason } => {
            assert_eq!(commits, 0);
            assert!(
                reason.contains("svn_identity_unproved"),
                "expected unproved hold, got {reason}"
            );
        }
        other => panic!("expected ReconciliationRequired hold, got {other:?}"),
    }
    let held = db
        .get_import_operation("legacy-import", &op.id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, ImportOperationState::ReconciliationRequired);
    assert_eq!(held.last_confirmed_svn_rev, Some(1));
    assert_eq!(
        held.last_confirmed_git_sha.as_deref(),
        Some(prefix_sha.as_str())
    );
    assert!(held.snapshot_pin.is_none());
    assert!(
        independently_established_svn_uuid(&db.conn(), "legacy-import")
            .unwrap()
            .is_none()
    );
    assert!(load_canonical_lineage(&db, "legacy-import")
        .unwrap()
        .is_none());
    assert_eq!(origin_commit_count(&bare), 1);

    let mut held = db
        .get_import_operation("legacy-import", &op.id)
        .unwrap()
        .unwrap();
    held.resume_authorized = true;
    db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![
                serde_json::to_string(&held).unwrap(),
                format!("import_operation_v1:document:{}", op.id)
            ],
        )
        .unwrap();
    db.resume_import_operation("legacy-import", &op.id).unwrap();
    let git_client = reposync_core::git::GitClient::new(&git_work).unwrap();
    let reopen = reposync_core::import::run_full_import(
        &reposync_core::svn::SvnClient::new(&svn_url, "", ""),
        &std::sync::Arc::new(std::sync::Mutex::new(git_client)),
        &identity,
        &db,
        &reposync_core::file_policy::FilePolicy::new(0, Vec::new()),
        &reposync_core::import::ImportConfig {
            committer_name: "Fixture".into(),
            committer_email: "fixture@example.invalid".into(),
            remote_name: "origin".into(),
            branch: "main".into(),
            push_token: None,
            message_prefix: None,
            trunk_path: String::new(),
        },
        reposync_core::import::ImportRunState {
            progress: std::sync::Arc::new(tokio::sync::RwLock::new(
                reposync_core::import::ImportProgress::default(),
            )),
            ws_broadcast: None,
            repo_id: Some("legacy-import".into()),
            operation_id: Some(op.id.clone()),
            cancel_signal: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        reopen,
        reposync_core::import::ImportOutcome::ReconciliationRequired { ref reason, .. }
            if reason.contains("svn_identity_unproved")
    ));
    assert!(
        independently_established_svn_uuid(&db.conn(), "legacy-import")
            .unwrap()
            .is_none()
    );
    assert_eq!(origin_commit_count(&bare), 1);

    let fresh_work = tmp.path().join("fresh_git");
    let fresh_bare = tmp.path().join("fresh.origin.git");
    git2::Repository::init_bare(&fresh_bare).unwrap();
    let fresh_git = reposync_core::git::GitClient::init(&fresh_work).unwrap();
    assert!(Command::new("git")
        .args(["checkout", "-B", "main"])
        .current_dir(&fresh_work)
        .status()
        .unwrap()
        .success());
    git2::Repository::open(&fresh_work)
        .unwrap()
        .remote("origin", fresh_bare.to_str().unwrap())
        .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let fresh_repo = reposync_core::models::Repository {
        id: "fresh-import".into(),
        name: "Fresh import".into(),
        svn_url: svn_url.clone(),
        svn_branch: String::new(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: format!("file://{}", fresh_work.display()),
        git_repo: "fresh/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    };
    db.insert_repository(&fresh_repo).unwrap();
    let fresh_fp = import_target_fingerprint(&fresh_repo, &fresh_work);
    let fresh_op = db
        .create_import_operation("fresh-import", "admin", "fresh-full", &fresh_fp)
        .unwrap();
    db.start_import_operation("fresh-import", &fresh_op.id)
        .unwrap();
    let fresh_outcome = reposync_core::import::run_full_import(
        &reposync_core::svn::SvnClient::new(&svn_url, "", ""),
        &std::sync::Arc::new(std::sync::Mutex::new(fresh_git)),
        &identity,
        &db,
        &reposync_core::file_policy::FilePolicy::new(0, Vec::new()),
        &reposync_core::import::ImportConfig {
            committer_name: "Fixture".into(),
            committer_email: "fixture@example.invalid".into(),
            remote_name: "origin".into(),
            branch: "main".into(),
            push_token: None,
            message_prefix: None,
            trunk_path: String::new(),
        },
        reposync_core::import::ImportRunState {
            progress: std::sync::Arc::new(tokio::sync::RwLock::new(
                reposync_core::import::ImportProgress::default(),
            )),
            ws_broadcast: None,
            repo_id: Some("fresh-import".into()),
            operation_id: Some(fresh_op.id.clone()),
            cancel_signal: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(
            fresh_outcome,
            reposync_core::import::ImportOutcome::Completed { commits, .. } if commits >= 1
        ) || matches!(
            fresh_outcome,
            reposync_core::import::ImportOutcome::ReconciliationRequired { commits, .. } if commits >= 1
        ),
        "fresh import should proceed, got {fresh_outcome:?}"
    );
    assert_eq!(
        independently_established_svn_uuid(&db.conn(), "fresh-import")
            .unwrap()
            .as_deref(),
        Some(replacement_uuid.as_str())
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_PREDECESSOR_IMPORT_PREFIX_HOLD",
            "original_uuid": original_uuid,
            "replacement_uuid": replacement_uuid,
            "recorded_legacy_uuid": independently_established_svn_uuid(&db.conn(), "legacy-import").unwrap(),
            "origin_commits": origin_commit_count(&bare)
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independently_proved_same_identity_resume_continues() {
    let _serial = writer_fence_test_lock().lock().await;
    if !svn_available() || !git_available() {
        eprintln!("SKIP: svn/svnadmin/git not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let svn_path = tmp.path().join("svn_repo");
    assert!(Command::new("svnadmin")
        .args(["create", svn_path.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let svn_url = file_url(&svn_path);
    let live_uuid = svn_uuid(&svn_path);
    let wc = tmp.path().join("svn_wc");
    assert!(Command::new("svn")
        .args([
            "checkout",
            &svn_url,
            wc.to_str().unwrap(),
            "--non-interactive"
        ])
        .status()
        .unwrap()
        .success());
    svn_commit_file(&wc, "one.txt", "one\n", "r1");
    svn_commit_file(&wc, "two.txt", "two\n", "r2");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    let git_client = reposync_core::git::GitClient::init(&git_work).unwrap();
    assert!(Command::new("git")
        .args(["checkout", "-B", "main"])
        .current_dir(&git_work)
        .status()
        .unwrap()
        .success());
    git2::Repository::open(&git_work)
        .unwrap()
        .remote("origin", bare.to_str().unwrap())
        .unwrap();
    let prefix_sha = git_commit_prefix(&git_work, b"one\n");
    assert!(Command::new("git")
        .args(["push", "origin", "HEAD:refs/heads/main"])
        .current_dir(&git_work)
        .status()
        .unwrap()
        .success());

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Database::new(data_dir.join("state.db")).unwrap();
    db.initialize().unwrap();
    let _fence = reposync_core::writer_fence::claim(&db, &data_dir).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let repo = reposync_core::models::Repository {
        id: "proved-import".into(),
        name: "Proved import".into(),
        svn_url: svn_url.clone(),
        svn_branch: String::new(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: format!("file://{}", git_work.display()),
        git_repo: "proved/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    };
    db.insert_repository(&repo).unwrap();
    reposync_core::db::lineage_checkpoints::record_independently_established_svn_uuid(
        &db,
        "proved-import",
        &live_uuid,
    )
    .unwrap();
    let fingerprint = import_target_fingerprint(&repo, &git_work);
    let op = db
        .create_import_operation("proved-import", "admin", "proved-full", &fingerprint)
        .unwrap();
    db.start_import_operation("proved-import", &op.id).unwrap();
    db.note_import_total("proved-import", &op.id, 2).unwrap();
    db.note_import_local("proved-import", &op.id, 1, &prefix_sha, 1, 1)
        .unwrap();
    db.begin_import_publication("proved-import", &op.id, "refs/heads/main", &prefix_sha)
        .unwrap();
    db.confirm_import_publication("proved-import", &op.id, &prefix_sha)
        .unwrap();
    db.finish_import_operation(
        "proved-import",
        &op.id,
        ImportOperationState::ReconciliationRequired,
        "worker stopped",
    )
    .unwrap();
    let mut held = db
        .get_import_operation("proved-import", &op.id)
        .unwrap()
        .unwrap();
    held.resume_authorized = true;
    db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![
                serde_json::to_string(&held).unwrap(),
                format!("import_operation_v1:document:{}", op.id)
            ],
        )
        .unwrap();
    db.resume_import_operation("proved-import", &op.id).unwrap();
    let identity = reposync_core::identity::IdentityMapper::new(
        &reposync_core::config::IdentityConfig::default(),
    )
    .unwrap();
    let outcome = reposync_core::import::run_full_import(
        &reposync_core::svn::SvnClient::new(&svn_url, "", ""),
        &std::sync::Arc::new(std::sync::Mutex::new(git_client)),
        &identity,
        &db,
        &reposync_core::file_policy::FilePolicy::new(0, Vec::new()),
        &reposync_core::import::ImportConfig {
            committer_name: "Fixture".into(),
            committer_email: "fixture@example.invalid".into(),
            remote_name: "origin".into(),
            branch: "main".into(),
            push_token: None,
            message_prefix: None,
            trunk_path: String::new(),
        },
        reposync_core::import::ImportRunState {
            progress: std::sync::Arc::new(tokio::sync::RwLock::new(
                reposync_core::import::ImportProgress::default(),
            )),
            ws_broadcast: None,
            repo_id: Some("proved-import".into()),
            operation_id: Some(op.id.clone()),
            cancel_signal: None,
        },
    )
    .await
    .unwrap();
    assert!(
        !matches!(
            outcome,
            reposync_core::import::ImportOutcome::ReconciliationRequired { ref reason, .. }
                if reason.contains("svn_identity_unproved")
        ),
        "matching proved identity must not hold unproved, got {outcome:?}"
    );
    assert_eq!(
        independently_established_svn_uuid(&db.conn(), "proved-import")
            .unwrap()
            .as_deref(),
        Some(live_uuid.as_str())
    );
}
