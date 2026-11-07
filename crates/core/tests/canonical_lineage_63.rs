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
    CanonicalLineage, CheckpointDirection, LiveSvnIdentity, MappingOutcome,
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

const R01_OLD_INSTALL_GATE: &str = "R01_OLD_INSTALL_UPGRADE";
const R01_OLD_CODE: &str = "87379741779a6259f7eeb52a68cc6f061174e5ef";
const R01_GENERATOR_SOURCE: &str = "crates/web/tests/legacy_import_generator.rs";
const R01_FIXTURE_CASE: &str = "candidate_r01_old_import_cursor_survives_svn_only_poll_and_upgrade";

fn require_svn_git() {
    assert!(
        svn_available() && git_available(),
        "PR245-1-R1-V requires svn, svnadmin, svnlook, and git"
    );
}

fn reopen_managed_db(data_dir: &Path) -> (Database, reposync_core::writer_fence::WriterFenceGuard) {
    let db = Database::new(data_dir.join("state.db")).unwrap();
    db.initialize().unwrap();
    let fence = reposync_core::writer_fence::claim(&db, data_dir).unwrap();
    (db, fence)
}

fn origin_tip(bare: &Path) -> String {
    git2::Repository::open(bare)
        .unwrap()
        .refname_to_id("refs/heads/main")
        .unwrap()
        .to_string()
}

fn origin_blob(bare: &Path, sha: &str, path: &str) -> String {
    let repo = git2::Repository::open(bare).unwrap();
    let commit = repo.find_commit(git2::Oid::from_str(sha).unwrap()).unwrap();
    let blob = repo
        .find_blob(
            commit
                .tree()
                .unwrap()
                .get_path(Path::new(path))
                .unwrap()
                .id(),
        )
        .unwrap();
    String::from_utf8(blob.content().to_vec()).unwrap()
}

fn import_cfg() -> reposync_core::import::ImportConfig {
    reposync_core::import::ImportConfig {
        committer_name: "Fixture".into(),
        committer_email: "fixture@example.invalid".into(),
        remote_name: "origin".into(),
        branch: "main".into(),
        push_token: None,
        message_prefix: None,
        trunk_path: String::new(),
    }
}

fn import_state(repo_id: &str, op_id: &str) -> reposync_core::import::ImportRunState {
    reposync_core::import::ImportRunState {
        progress: std::sync::Arc::new(tokio::sync::RwLock::new(
            reposync_core::import::ImportProgress::default(),
        )),
        ws_broadcast: None,
        repo_id: Some(repo_id.into()),
        operation_id: Some(op_id.into()),
        cancel_signal: None,
    }
}

fn assert_scope_schema(db: &Database) {
    let conn = db.conn();
    assert_eq!(version(&conn), CURRENT_SCHEMA_VERSION as i64);
    assert_eq!(canonical_table_count(&conn), 3);
    assert_eq!(pair_table_count(&conn), 0);
}

fn authorize_import_resume(db: &Database, repo_id: &str, op_id: &str) {
    let mut held = db.get_import_operation(repo_id, op_id).unwrap().unwrap();
    held.resume_authorized = true;
    db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![
                serde_json::to_string(&held).unwrap(),
                format!("import_operation_v1:document:{op_id}")
            ],
        )
        .unwrap();
    db.resume_import_operation(repo_id, op_id).unwrap();
}

fn unproved_hold_checkpoint(
    db: &Database,
    repo_id: &str,
) -> reposync_core::db::lineage_checkpoints::CanonicalCheckpoint {
    let scope = repository_scope_uuid(&db.conn(), repo_id).unwrap();
    load_directional_checkpoint(
        &db.conn(),
        &scope,
        PAIR_GENERATION,
        CheckpointDirection::SvnToGit,
    )
    .unwrap()
    .expect("unproved hold must persist a canonical checkpoint")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn predecessor_partial_full_import_holds_same_url_replacement_uuid() {
    let _serial = writer_fence_test_lock().lock().await;
    require_svn_git();
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
        &import_cfg(),
        import_state("legacy-import", &op.id),
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
    let hold_checkpoint = unproved_hold_checkpoint(&db, "legacy-import");
    assert_eq!(
        hold_checkpoint.outcome,
        MappingOutcome::UnknownReconciliationRequired
    );
    assert_eq!(
        hold_checkpoint.reason.as_deref(),
        Some("svn_identity_unproved")
    );
    assert_scope_schema(&db);

    let op_id = op.id.clone();
    drop(_fence);
    drop(db);

    let (db, _fence) = reopen_managed_db(&data_dir);
    assert_scope_schema(&db);
    let reopened = db
        .get_import_operation("legacy-import", &op_id)
        .unwrap()
        .unwrap();
    assert_eq!(reopened.state, ImportOperationState::ReconciliationRequired);
    assert!(import_operation_has_retained_effects(&reopened));
    assert_eq!(reopened.last_confirmed_svn_rev, Some(1));
    assert_eq!(
        reopened.last_confirmed_git_sha.as_deref(),
        Some(prefix_sha.as_str())
    );
    assert!(reopened.snapshot_pin.is_none());
    assert!(
        independently_established_svn_uuid(&db.conn(), "legacy-import")
            .unwrap()
            .is_none(),
        "reopen must not mint replacement UUID authority"
    );
    assert!(load_canonical_lineage(&db, "legacy-import")
        .unwrap()
        .is_none());
    let reopened_hold = unproved_hold_checkpoint(&db, "legacy-import");
    assert_eq!(
        reopened_hold.outcome,
        MappingOutcome::UnknownReconciliationRequired
    );
    assert_eq!(
        reopened_hold.reason.as_deref(),
        Some("svn_identity_unproved")
    );
    assert_eq!(origin_tip(&bare), prefix_sha);
    assert_eq!(origin_blob(&bare, &prefix_sha, "one.txt"), "one\n");
    assert_eq!(origin_commit_count(&bare), 1);

    authorize_import_resume(&db, "legacy-import", &op_id);
    let git_client = reposync_core::git::GitClient::new(&git_work).unwrap();
    let reopen = reposync_core::import::run_full_import(
        &reposync_core::svn::SvnClient::new(&svn_url, "", ""),
        &std::sync::Arc::new(std::sync::Mutex::new(git_client)),
        &identity,
        &db,
        &reposync_core::file_policy::FilePolicy::new(0, Vec::new()),
        &import_cfg(),
        import_state("legacy-import", &op_id),
    )
    .await
    .unwrap();
    match reopen {
        reposync_core::import::ImportOutcome::ReconciliationRequired { commits, reason } => {
            assert_eq!(commits, 0);
            assert!(
                reason.contains("svn_identity_unproved"),
                "reopened replacement must remain refused, got {reason}"
            );
        }
        other => panic!("expected replacement refusal after reopen, got {other:?}"),
    }
    assert!(
        independently_established_svn_uuid(&db.conn(), "legacy-import")
            .unwrap()
            .is_none()
    );
    assert_eq!(origin_commit_count(&bare), 1);
    assert_eq!(origin_tip(&bare), prefix_sha);

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
        &import_cfg(),
        import_state("fresh-import", &fresh_op.id),
    )
    .await
    .unwrap();
    let reposync_core::import::ImportOutcome::Completed {
        commits: fresh_commits,
        svn_rev: fresh_rev,
        git_sha: fresh_sha,
    } = fresh_outcome
    else {
        panic!("fresh import must complete, got {fresh_outcome:?}");
    };
    assert_eq!(fresh_commits, 3);
    assert_eq!(fresh_rev, 3);
    db.complete_import_operation("fresh-import", &fresh_op.id, fresh_rev, &fresh_sha)
        .unwrap();
    let fresh_done = db
        .get_import_operation("fresh-import", &fresh_op.id)
        .unwrap()
        .unwrap();
    assert_eq!(fresh_done.state, ImportOperationState::Completed);
    assert_eq!(fresh_done.last_confirmed_svn_rev, Some(3));
    assert_eq!(
        fresh_done.last_confirmed_git_sha.as_deref(),
        Some(fresh_sha.as_str())
    );
    assert_eq!(origin_tip(&fresh_bare), fresh_sha);
    assert_eq!(
        origin_blob(&fresh_bare, &fresh_sha, "three.txt"),
        "replacement-three\n"
    );
    assert_eq!(
        db.get_repo_watermark("fresh-import").unwrap(),
        (3, fresh_sha.clone())
    );
    assert_eq!(
        independently_established_svn_uuid(&db.conn(), "fresh-import")
            .unwrap()
            .as_deref(),
        Some(replacement_uuid.as_str())
    );
    assert_eq!(origin_commit_count(&fresh_bare), 3);

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_PREDECESSOR_IMPORT_PREFIX_HOLD",
            "journal_construction":"current_apis",
            "predecessor_binary_executed":false,
            "old_install_gate":R01_OLD_INSTALL_GATE,
            "old_code":R01_OLD_CODE,
            "persisted_reopen":true,
            "original_uuid": original_uuid,
            "replacement_uuid": replacement_uuid,
            "recorded_legacy_uuid": independently_established_svn_uuid(&db.conn(), "legacy-import").unwrap(),
            "origin_commits": origin_commit_count(&bare),
            "fresh_import_completed":true,
            "fresh_svn_rev":fresh_rev,
            "fresh_git_sha":fresh_sha
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independently_proved_same_identity_resume_continues() {
    let _serial = writer_fence_test_lock().lock().await;
    require_svn_git();
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
    authorize_import_resume(&db, "proved-import", &op.id);
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
        &import_cfg(),
        import_state("proved-import", &op.id),
    )
    .await
    .unwrap();
    let reposync_core::import::ImportOutcome::Completed {
        commits,
        svn_rev,
        git_sha,
    } = outcome
    else {
        panic!("matching proved identity must complete, got {outcome:?}");
    };
    assert_eq!(commits, 2);
    assert_eq!(svn_rev, 2);
    db.complete_import_operation("proved-import", &op.id, svn_rev, &git_sha)
        .unwrap();
    let finished = db
        .get_import_operation("proved-import", &op.id)
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, ImportOperationState::Completed);
    assert_eq!(finished.last_confirmed_svn_rev, Some(2));
    assert_eq!(
        finished.last_confirmed_git_sha.as_deref(),
        Some(git_sha.as_str())
    );
    assert_eq!(origin_tip(&bare), git_sha);
    assert_eq!(origin_blob(&bare, &git_sha, "two.txt"), "two\n");
    assert_eq!(
        db.get_repo_watermark("proved-import").unwrap(),
        (2, git_sha.clone())
    );
    assert_eq!(
        independently_established_svn_uuid(&db.conn(), "proved-import")
            .unwrap()
            .as_deref(),
        Some(live_uuid.as_str())
    );
    let scope = repository_scope_uuid(&db.conn(), "proved-import").unwrap();
    if let Some(checkpoint) = load_directional_checkpoint(
        &db.conn(),
        &scope,
        PAIR_GENERATION,
        CheckpointDirection::SvnToGit,
    )
    .unwrap()
    {
        assert_ne!(
            checkpoint.reason.as_deref(),
            Some("svn_identity_unproved"),
            "successful resume must not leave an unproved identity hold"
        );
    }
    assert_scope_schema(&db);

    let op_id = op.id.clone();
    drop(_fence);
    drop(db);
    let (db, _fence) = reopen_managed_db(&data_dir);
    let reopened = db
        .get_import_operation("proved-import", &op_id)
        .unwrap()
        .unwrap();
    assert_eq!(reopened.state, ImportOperationState::Completed);
    assert_eq!(reopened.last_confirmed_svn_rev, Some(2));
    assert_eq!(
        reopened.last_confirmed_git_sha.as_deref(),
        Some(git_sha.as_str())
    );
    assert_eq!(
        db.get_repo_watermark("proved-import").unwrap(),
        (2, git_sha.clone())
    );
    assert_eq!(origin_tip(&bare), git_sha);
    assert_eq!(origin_blob(&bare, &git_sha, "two.txt"), "two\n");
    assert_eq!(
        independently_established_svn_uuid(&db.conn(), "proved-import")
            .unwrap()
            .as_deref(),
        Some(live_uuid.as_str())
    );
    assert_scope_schema(&db);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_PROVED_IDENTITY_RESUME_COMPLETED",
            "commits":commits,
            "svn_rev":svn_rev,
            "git_sha":git_sha,
            "persisted_reopen":true,
            "origin_commits":origin_commit_count(&bare)
        })
    );
}

#[test]
fn predecessor_binary_old_install_gate_is_not_current_api_journal() {
    let generator_admitted = std::env::var_os("REPOSYNC_OLD_GENERATOR").is_some();
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS63_PREDECESSOR_BINARY_GATE_NOT_ADMITTED",
            "old_install_gate":R01_OLD_INSTALL_GATE,
            "old_code":R01_OLD_CODE,
            "fixture_production_path":R01_GENERATOR_SOURCE,
            "fixture_case":R01_FIXTURE_CASE,
            "ordinary_test":"canonical_lineage_63::predecessor_partial_full_import_holds_same_url_replacement_uuid",
            "journal_construction":"current_apis",
            "predecessor_binary_executed":false,
            "old_generator_env_present":generator_admitted,
            "reliability_fixture_feature":cfg!(feature = "reliability-fixture"),
            "qualification":"NOT RUN",
            "reason":"ordinary runtime does not admit R01_OLD_INSTALL_UPGRADE; current-API journals are not old-version upgrade evidence"
        })
    );
}
