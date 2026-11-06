//! Dedicated #63 regressions: operational v14 canonical lineage, typed
//! checkpoints, SVN UUID/path-incarnation preflight, and RS-11 credential
//! origin/redaction canaries. Keep `team_mode_e2e.rs` append-only.
use std::path::Path;
use std::process::Command;

use reposync_core::db::lineage_checkpoints::{
    load_canonical_lineage, load_directional_checkpoint, pin_canonical_lineage,
    verify_or_pin_live_svn_identity, CanonicalLineage, CheckpointDirection,
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
             VALUES ('pair','pair',?1,'trunk','','local','','r','main','team',60,0,0,1,'t','t',1,'','idle',0,0)",
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
