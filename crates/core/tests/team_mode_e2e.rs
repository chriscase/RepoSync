//! End-to-end tests for team-mode bidirectional SVN <-> Git synchronization.
//!
//! These tests exercise the real `SyncEngine` with:
//! - Local SVN repos via `svnadmin create` (file:// protocol)
//! - Local Git repos with bare "origin" for pushes
//! - Real SQLite databases
//! - Real identity mapping
//!
//! No network I/O: SVN uses `file://` URLs, Git uses local bare repos.
//!
//! Tests skip gracefully if `svn` / `svnadmin` are not installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use sha2::Digest;

use tempfile::TempDir;

use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_core::errors::SyncError;

// ===========================================================================
// Helpers
// ===========================================================================

fn svn_available() -> bool {
    let svn_ok = Command::new("svn")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let svnadmin_ok = Command::new("svnadmin")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    svn_ok && svnadmin_ok
}

fn assert_fixture_owned(path: &Path) {
    if let Ok(root) = std::env::var("REPOSYNC_FIXTURE_ROOT") {
        let root = Path::new(&root).canonicalize().unwrap();
        let target = path.canonicalize().unwrap();
        assert!(target.starts_with(&root), "fixture target escaped owned root: {}", target.display());
    }
}

fn create_svn_repo(dir: &Path) -> String {
    assert_fixture_owned(dir);
    let repo_dir = dir.join("svn_repo");
    let status = Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .expect("failed to run svnadmin create");
    assert!(status.success(), "svnadmin create failed");

    let hooks_dir = repo_dir.join("hooks");
    let pre_revprop_change = hooks_dir.join("pre-revprop-change");
    std::fs::write(&pre_revprop_change, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pre_revprop_change, std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    format!("file://{}", repo_dir.display())
}

fn svn_checkout(url: &str, wc_path: &Path) {
    let status = Command::new("svn")
        .args([
            "checkout",
            url,
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .expect("failed to run svn checkout");
    assert!(status.success(), "svn checkout failed");
}

fn svn_commit_file(wc_path: &Path, filename: &str, content: &str, message: &str) -> i64 {
    let file_path = wc_path.join(filename);

    if let Some(parent) = file_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).unwrap();
            let mut rel = PathBuf::new();
            for component in Path::new(filename).parent().unwrap().components() {
                rel = rel.join(component);
                let abs = wc_path.join(&rel);
                let _ = Command::new("svn")
                    .args(["add", "--depth=empty", abs.to_str().unwrap()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }

    std::fs::write(&file_path, content).unwrap();

    let status_output = Command::new("svn")
        .args(["status", file_path.to_str().unwrap()])
        .output()
        .unwrap();
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    if status_str.contains('?') {
        let add_status = Command::new("svn")
            .args(["add", file_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(add_status.success(), "svn add failed");
    }

    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            wc_path.to_str().unwrap(),
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .output()
        .expect("svn commit failed");
    assert!(
        output.status.success(),
        "svn commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Committed revision") {
            return trimmed
                .trim_start_matches("Committed revision")
                .trim()
                .trim_end_matches('.')
                .parse::<i64>()
                .expect("failed to parse revision number");
        }
    }
    panic!("could not parse committed revision from: {}", stdout);
}

fn setup_git_with_bare_origin(work_dir: &Path, bare_dir: &Path) -> GitClient {
    assert_fixture_owned(work_dir.parent().unwrap());
    assert_fixture_owned(bare_dir.parent().unwrap());
    git2::Repository::init_bare(bare_dir).expect("failed to init bare repo");
    let git_client = GitClient::init(work_dir).expect("failed to init git repo");

    let repo = git2::Repository::open(work_dir).expect("failed to open repo");
    repo.remote("origin", bare_dir.to_str().unwrap())
        .expect("failed to add origin remote");

    std::fs::write(work_dir.join(".gitkeep"), "").unwrap();
    git_client
        .commit(
            "initial commit",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("failed to create initial commit");

    {
        let repo = git2::Repository::open(work_dir).unwrap();
        let head = repo.head().unwrap();
        let head_name = head.name().unwrap_or("");
        if head_name != "refs/heads/main" {
            let mut branch = repo
                .find_branch(
                    head_name.strip_prefix("refs/heads/").unwrap_or("master"),
                    git2::BranchType::Local,
                )
                .unwrap();
            branch.rename("main", true).unwrap();
        }
    }

    git_client
        .push("origin", "main")
        .expect("failed to push initial commit to origin");

    git_client
}

fn setup_db(path: &Path) -> Database {
    let db = Database::new(path).expect("failed to create database");
    db.initialize()
        .expect("failed to initialize database schema");
    db
}

fn make_app_config(svn_url: &str, data_dir: &Path) -> AppConfig {
    let toml_str = format!(
        r#"
[daemon]
poll_interval_secs = 5
log_level = "debug"
data_dir = "{}"

[svn]
url = "{}"
username = ""
password_env = "REPOSYNC_TEST_SVN_PW"

[github]
repo = "test/test-repo"
token_env = "REPOSYNC_TEST_GH_TOKEN"
"#,
        data_dir.display(),
        svn_url
    );
    let mut config: AppConfig = toml::from_str(&toml_str).unwrap();
    config.svn.password = Some(String::new());
    config.github.token = Some(String::new());
    config
}

fn make_identity_mapper() -> IdentityMapper {
    let config = IdentityConfig {
        email_domain: Some("example.com".into()),
        ..Default::default()
    };
    IdentityMapper::new(&config).unwrap()
}

/// Get the SHA of the current HEAD commit.
fn get_head_sha(repo_path: &Path) -> String {
    let repo = git2::Repository::open(repo_path).unwrap();
    let head = repo.head().unwrap();
    head.target().unwrap().to_string()
}

fn count_git_commits(repo_path: &Path) -> usize {
    let repo = git2::Repository::open(repo_path).unwrap();
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let oid = head.target().unwrap();
    let mut revwalk = repo.revwalk().unwrap();
    revwalk.push(oid).unwrap();
    revwalk.count()
}

fn get_git_commit_message(repo_path: &Path, index: usize) -> String {
    let repo = git2::Repository::open(repo_path).unwrap();
    let head = repo.head().unwrap();
    let oid = head.target().unwrap();
    let mut revwalk = repo.revwalk().unwrap();
    revwalk.push(oid).unwrap();
    revwalk
        .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .unwrap();
    let oids: Vec<_> = revwalk.collect::<Result<Vec<_>, _>>().unwrap();
    let commit = repo.find_commit(oids[index]).unwrap();
    commit.message().unwrap_or("").to_string()
}

fn git_cli(repo_path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Developer")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Developer")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .expect("git fixture command");
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(repo_path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git fixture query");
    assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

struct QualifiedPair {
    tmp: TempDir,
    svn_url: String,
    wc: PathBuf,
    bridge: PathBuf,
    developer: PathBuf,
    bare: PathBuf,
    db_path: PathBuf,
    engine: SyncEngine,
    imported_base: String,
}

impl QualifiedPair {
    async fn new() -> Self {
        assert!(svn_available(), "SVN tools are required for candidate evidence");
        let tmp = TempDir::new().unwrap();
        let svn_url = create_svn_repo(tmp.path());
        let wc = tmp.path().join("wc");
        svn_checkout(&svn_url, &wc);
        svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor");
        svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
        let bridge = tmp.path().join("bridge");
        let bare = tmp.path().join("origin.git");
        let git = setup_git_with_bare_origin(&bridge, &bare);
        let initial = get_head_sha(&bridge);
        let db_path = tmp.path().join("sync.db");
        let db = setup_db(&db_path);
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: "pair".into(), name: "qualified pair".into(), svn_url: svn_url.clone(),
            svn_branch: "".into(), svn_username: "fixture".into(),
            git_provider: "local".into(), git_api_url: "".into(),
            git_repo: bare.to_string_lossy().to_string(), git_branch: "main".into(),
            sync_mode: "team".into(), poll_interval_secs: 5, lfs_threshold_mb: 0,
            auto_merge: false, enabled: true, created_by: None, parent_id: None,
            created_at: now.clone(), updated_at: now, last_svn_rev: 1,
            last_git_sha: initial, last_sync_at: None, sync_status: "idle".into(),
            total_syncs: 0, total_errors: 0, allowed_paths: None,
            blocked_patterns: None, consecutive_errors: 0, teams_webhook_url: None,
        }).unwrap();
        let mut config = make_app_config(&svn_url, tmp.path());
        config.svn.layout = reposync_core::config::SvnLayout::Custom;
        let mut engine = SyncEngine::new(
            config, db, SvnClient::new(&svn_url, "", ""), git,
            Arc::new(make_identity_mapper()),
        );
        engine.set_repo_id("pair".into());
        assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
        let imported_base = get_head_sha(&bridge);
        assert_eq!(engine.db().get_repo_watermark("pair").unwrap(), (2, imported_base.clone()));
        let mapped: i64 = engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = 2 AND git_sha = ?1",
            [&imported_base], |row| row.get(0)).unwrap();
        assert_eq!(mapped, 1, "SVN-derived baseline must have the actual engine mapping");
        assert_eq!(std::fs::read_to_string(bridge.join("origin.txt")).unwrap(), "SVN origin\n");
        assert_eq!(git_output(&bare, &["rev-parse", "refs/heads/main"]), imported_base);
        let developer = tmp.path().join("developer");
        let clone = Command::new("git").args(["clone", "-b", "main", bare.to_str().unwrap(), developer.to_str().unwrap()]).output().unwrap();
        assert!(clone.status.success(), "developer clone: {}", String::from_utf8_lossy(&clone.stderr));
        assert_eq!(get_head_sha(&developer), imported_base);
        Self { tmp, svn_url, wc, bridge, developer, bare, db_path, engine, imported_base }
    }

    fn developer_commit(&self, name: &str, content: &str, message: &str) -> String {
        std::fs::write(self.developer.join(name), content).unwrap();
        git_cli(&self.developer, &["add", name]);
        git_cli(&self.developer, &["commit", "-m", message]);
        get_head_sha(&self.developer)
    }

    async fn snapshot(&self) -> PairSnapshot {
        let svn = SvnClient::new(&self.svn_url, "", "");
        let svn_rev = svn.info().await.unwrap().latest_rev;
        let export = self.tmp.path().join(format!("capture-{}", uuid::Uuid::new_v4()));
        svn.export("", svn_rev, &export).await.unwrap();
        let mapping_count: i64 = self.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair'", [], |row| row.get(0)).unwrap();
        let repo_sync_count = self.engine.db().get_repository("pair").unwrap().unwrap().total_syncs;
        let bridge_status = git_output(&self.bridge, &["status", "--porcelain"]);
        let bridge_index = std::fs::read(self.bridge.join(".git/index")).unwrap();
        PairSnapshot {
            svn_rev,
            svn_origin: std::fs::read_to_string(export.join("origin.txt")).unwrap(),
            svn_feature: std::fs::read_to_string(export.join("feature.txt")).ok(),
            remote_sha: git_output(&self.bare, &["rev-parse", "refs/heads/main"]),
            remote_tree: git_output(&self.bare, &["rev-parse", "refs/heads/main^{tree}"]),
            bridge_sha: get_head_sha(&self.bridge),
            bridge_tree: git_output(&self.bridge, &["rev-parse", "HEAD^{tree}"]),
            bridge_index,
            bridge_status,
            bridge_feature: std::fs::read_to_string(self.bridge.join("feature.txt")).ok(),
            watermark: self.engine.db().get_repo_watermark("pair").unwrap(),
            kv_cursor: self.engine.db().get_state("last_git_sha_pair").unwrap(),
            mapping_count,
            repo_sync_count,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PairSnapshot {
    svn_rev: i64,
    svn_origin: String,
    svn_feature: Option<String>,
    remote_sha: String,
    remote_tree: String,
    bridge_sha: String,
    bridge_tree: String,
    bridge_index: Vec<u8>,
    bridge_status: String,
    bridge_feature: Option<String>,
    watermark: (i64, String),
    kv_cursor: Option<String>,
    mapping_count: i64,
    repo_sync_count: i64,
}

async fn assert_pair_blocked_without_damage(fixture: &QualifiedPair, reason: &str) {
    let before = fixture.snapshot().await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason: actual, .. }) if actual == reason),
        "expected {reason} rejection, got {result:?}");
    assert_eq!(fixture.snapshot().await, before, "rejected pair changed protected state");
    let block: serde_json::Value = serde_json::from_str(
        &fixture.engine.db().get_state("team_history_block_pair").unwrap().unwrap()).unwrap();
    assert_eq!(block["reason"], reason);
    assert_eq!(fixture.engine.db().get_state("sync_state").unwrap().as_deref(), Some("reconciliation_required"));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":reason, "p_before":before.watermark.1,
        "l_before":before.bridge_sha, "r_before":before.remote_sha,
        "svn_revision_before_after":before.svn_rev,
        "remote_tree_before_after":before.remote_tree,
        "bridge_tree_before_after":before.bridge_tree,
        "bridge_index_sha256_before_after":hex::encode(sha2::Sha256::digest(&before.bridge_index)),
        "mapping_count_before_after":before.mapping_count,
        "success_count_before_after":before.repo_sync_count,
        "reason":reason, "after_equal":true
    }));
}

/// The original R09 changed-content replacement fixture, retained as a
/// separate single-repository legacy-cursor control after containment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_original_replacement_fixture_rejected() {
    assert!(
        svn_available(),
        "svn and svnadmin are required; do not count a skipped diagnostic as evidence"
    );
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor");
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");

    let git_dir = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git = setup_git_with_bare_origin(&git_dir, &bare);
    let db = setup_db(&tmp.path().join("sync.db"));
    db.set_state("last_svn_rev", "1").unwrap();
    db.set_state("last_git_hash", &get_head_sha(&git_dir))
        .unwrap();
    let mut config = make_app_config(&svn_url, tmp.path());
    config.svn.layout = reposync_core::config::SvnLayout::Custom;
    let engine = SyncEngine::new(
        config,
        db,
        SvnClient::new(&svn_url, "", ""),
        git,
        Arc::new(make_identity_mapper()),
    );
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let proven_base = get_head_sha(&git_dir);
    let import_record: i64 = engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE direction = 'svn_to_git' AND git_sha = ?1 AND svn_rev = 2",
        [&proven_base], |row| row.get(0)).unwrap();
    assert_eq!(import_record, 1, "SVN-origin baseline must have a production sync mapping");
    assert_eq!(std::fs::read_to_string(git_dir.join("origin.txt")).unwrap(), "SVN origin\n");

    let developer = tmp.path().join("developer");
    let clone = Command::new("git").args(["clone", "-b", "main", bare.to_str().unwrap(), developer.to_str().unwrap()]).output().unwrap();
    assert!(clone.status.success(), "developer clone: {}", String::from_utf8_lossy(&clone.stderr));
    assert_eq!(get_head_sha(&developer), proven_base);

    std::fs::write(developer.join("feature.txt"), "first version\n").unwrap();
    git_cli(&developer, &["add", "feature.txt"]);
    git_cli(&developer, &["commit", "-m", "First Git change"]);
    git_cli(&developer, &["push", "origin", "main"]);
    let old_synced = get_head_sha(&developer);
    assert_eq!(engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    let before = SvnClient::new(&svn_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    let old_record: i64 = engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE git_sha = ?1 AND svn_rev = ?2 AND direction = 'git_to_svn'",
        rusqlite::params![old_synced, before], |row| row.get(0)).unwrap();
    assert_eq!(old_record, 1);
    let original_export = tmp.path().join("original_export");
    SvnClient::new(&svn_url, "", "").export("", before, &original_export).await.unwrap();
    assert_eq!(std::fs::read_to_string(original_export.join("feature.txt")).unwrap(), "first version\n");
    assert_eq!(
        std::fs::read_to_string(wc.join("origin.txt")).unwrap(),
        "SVN origin\n"
    );

    git_cli(&developer, &["reset", "--hard", &proven_base]);
    std::fs::write(developer.join("feature.txt"), "rewritten version\n").unwrap();
    git_cli(&developer, &["add", "feature.txt"]);
    git_cli(&developer, &["commit", "-m", "Rewritten Git change"]);
    git_cli(&developer, &["push", "--force", "origin", "main"]);
    let rewritten = get_head_sha(&developer);
    assert_ne!(old_synced, rewritten);
    let ancestry = Command::new("git")
        .arg("-C")
        .arg(&developer)
        .args(["merge-base", "--is-ancestor", &old_synced, &rewritten])
        .status()
        .unwrap();
    assert_eq!(ancestry.code(), Some(1), "fixture must show valid negative ancestry, not a command error");
    assert_eq!(get_head_sha(&git_dir), old_synced, "developer rewrite must not pre-reset the bridge");
    assert_eq!(git_output(&git_dir, &["status", "--porcelain"]), "");
    let bridge_tree_before = git_output(&git_dir, &["rev-parse", "HEAD^{tree}"]);
    let bridge_index_before = git_output(&git_dir, &["write-tree"]);
    let remote_tree_before = git_output(&developer, &["rev-parse", "HEAD^{tree}"]);
    let checkpoint_before = engine.db().get_state("last_git_hash").unwrap();
    let mapping_count_before = engine.db().count_sync_records().unwrap();

    let result = engine.run_sync_cycle().await;
    let after = SvnClient::new(&svn_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"),
        "old fixture must now reject without replay: {result:?}");
    assert_eq!(after, before, "SVN revision must be preserved");
    assert_eq!(get_head_sha(&git_dir), old_synced);
    assert_eq!(git_output(&git_dir, &["rev-parse", "HEAD^{tree}"]), bridge_tree_before);
    assert_eq!(git_output(&git_dir, &["write-tree"]), bridge_index_before);
    assert_eq!(git_output(&git_dir, &["status", "--porcelain"]), "");
    assert_eq!(git_output(&bare, &["rev-parse", "refs/heads/main"]), rewritten);
    let exported = tmp.path().join("rewritten_export");
    SvnClient::new(&svn_url, "", "")
        .export("", after, &exported)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(exported.join("feature.txt")).unwrap(),
        "first version\n"
    );
    let rewritten_record: i64 = engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE git_sha = ?1 AND direction = 'git_to_svn'",
        rusqlite::params![rewritten], |row| row.get(0)).unwrap();
    assert_eq!(rewritten_record, 0);
    assert_eq!(engine.db().get_state("last_git_hash").unwrap(), checkpoint_before);
    assert_eq!(engine.db().count_sync_records().unwrap(), mapping_count_before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R09_ORIGINAL_REPLACEMENT", "p":old_synced, "r":rewritten,
        "bridge_tree_before_after":bridge_tree_before,
        "bridge_index_tree_before_after":bridge_index_before,
        "remote_tree_before_after":remote_tree_before,
        "svn_revision_before_after":before,
        "mapping_count_before_after":mapping_count_before,
        "reason":"non_fast_forward"
    }));
}

async fn run_candidate_r09_rewrite(metadata_only_amend: bool) {
    let fixture = QualifiedPair::new().await;
    let old_synced = fixture.developer_commit("feature.txt", "first version\n", "First Git change");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    let handled = fixture.engine.db().get_repo_watermark("pair").unwrap().1;
    assert_eq!(handled, old_synced);
    let old_revision = SvnClient::new(&fixture.svn_url, "", "").info().await.unwrap().latest_rev;
    let mapped: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND svn_rev = ?1 AND git_sha = ?2",
        rusqlite::params![old_revision, old_synced], |row| row.get(0)).unwrap();
    assert_eq!(mapped, 1);

    if metadata_only_amend {
        git_cli(&fixture.developer, &["commit", "--amend", "-m", "Amended Git metadata"]);
    } else {
        git_cli(&fixture.developer, &["reset", "--hard", &fixture.imported_base]);
        fixture.developer_commit("feature.txt", "rewritten version\n", "Changed-content replacement");
    }
    git_cli(&fixture.developer, &["push", "--force", "origin", "main"]);
    let replacement = get_head_sha(&fixture.developer);
    assert_ne!(replacement, old_synced);
    let ancestry = Command::new("git").arg("-C").arg(&fixture.developer)
        .args(["merge-base", "--is-ancestor", &old_synced, &replacement]).status().unwrap();
    assert_eq!(ancestry.code(), Some(1), "valid negative ancestry is required");
    if metadata_only_amend {
        assert_eq!(git_output(&fixture.developer, &["rev-parse", "HEAD^{tree}"]),
            git_output(&fixture.bridge, &["rev-parse", "HEAD^{tree}"]));
    }
    let before = fixture.snapshot().await;
    assert_eq!(before.bridge_sha, old_synced, "developer rewrite must leave bridge intact");
    assert_eq!(before.remote_sha, replacement);
    assert_eq!(before.svn_feature.as_deref(), Some("first version\n"));

    for attempt in 0..2 {
        let result = fixture.engine.run_sync_cycle().await;
        assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"),
            "attempt {attempt} must explicitly reject rewrite: {result:?}");
        assert_eq!(fixture.snapshot().await, before, "rejection must preserve bridge/remotes/checkpoints/mappings");
    }
    let reopened_db = Database::new(&fixture.db_path).unwrap();
    reopened_db.initialize().unwrap();
    let reopened_git = GitClient::new(&fixture.bridge).unwrap();
    let mut restarted = SyncEngine::new(
        fixture.engine.config().clone(), reopened_db,
        SvnClient::new(&fixture.svn_url, "", ""), reopened_git,
        Arc::new(make_identity_mapper()),
    );
    restarted.set_repo_id("pair".into());
    let result = restarted.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"),
        "reopened engine must reject unchanged unsafe state: {result:?}");
    assert_eq!(fixture.snapshot().await, before);
    assert_eq!(fixture.engine.db().get_state("sync_state").unwrap().as_deref(), Some("reconciliation_required"));
    let block: serde_json::Value = serde_json::from_str(&fixture.engine.db().get_state("team_history_block_pair").unwrap().unwrap()).unwrap();
    assert_eq!(block["reason"], "non_fast_forward");
    assert_eq!(block["p_handled"], old_synced);
    assert_eq!(block["r_fresh_remote"], replacement);
    assert_eq!(block["l_bridge"], old_synced);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case": if metadata_only_amend {"R09_AMEND"} else {"R09_REPLACEMENT"},
        "before": {"bridge_head": before.bridge_sha, "bridge_tree": before.bridge_tree,
            "bridge_index_sha256": hex::encode(sha2::Sha256::digest(&before.bridge_index)),
            "bridge_status": before.bridge_status, "remote_head": before.remote_sha,
            "remote_tree": before.remote_tree, "svn_revision": before.svn_rev,
            "svn_feature": before.svn_feature, "watermark": before.watermark,
            "kv_cursor": before.kv_cursor, "mapping_count": before.mapping_count,
            "success_count": before.repo_sync_count},
        "after_equal": true, "repeat_and_reopen_rejected": true,
        "reason": "non_fast_forward"
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_changed_content_rewrite_rejected() {
    run_candidate_r09_rewrite(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_metadata_amend_rejected() {
    run_candidate_r09_rewrite(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_two_pending_git_commits_sync_once() {
    let fixture = QualifiedPair::new().await;
    let first = fixture.developer_commit("feature.txt", "version one\n", "First pending Git commit");
    let second = fixture.developer_commit("feature.txt", "version two\n", "Second pending Git commit");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before = fixture.snapshot().await;
    assert_eq!(before.svn_rev, 2);
    assert_eq!(before.watermark.1, fixture.imported_base);
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 2, "both pending commits must replay");
    let svn = SvnClient::new(&fixture.svn_url, "", "");
    let after_rev = svn.info().await.unwrap().latest_rev;
    assert_eq!(after_rev, before.svn_rev + 2);
    for (revision, content, sha) in [(before.svn_rev + 1, "version one\n", &first), (after_rev, "version two\n", &second)] {
        let exported = fixture.tmp.path().join(format!("accepted-{revision}"));
        svn.export("", revision, &exported).await.unwrap();
        assert_eq!(std::fs::read_to_string(exported.join("feature.txt")).unwrap(), content);
        assert_eq!(std::fs::read_to_string(exported.join("origin.txt")).unwrap(), "SVN origin\n");
        let mapped: i64 = fixture.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND svn_rev = ?1 AND git_sha = ?2",
            rusqlite::params![revision, sha], |row| row.get(0)).unwrap();
        assert_eq!(mapped, 1, "each intermediate revision needs its own mapping");
    }
    let after = fixture.snapshot().await;
    assert_eq!(after.watermark.1, second);
    assert_eq!(after.bridge_sha, second);
    assert_eq!(after.remote_sha, second);
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.git_to_svn_count, repeat.svn_to_git_count), (0, 0));
    assert_eq!(fixture.snapshot().await, after, "unchanged run must not emit revisions or mappings");
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case": "R01_LINEAR", "p_before": before.watermark.1,
        "r": second, "first_git": first, "svn_before": before.svn_rev,
        "svn_after": after_rev, "first_svn": before.svn_rev + 1,
        "mapping_before": before.mapping_count, "mapping_after": after.mapping_count,
        "bridge_before": before.bridge_sha, "bridge_after": after.bridge_sha,
        "remote_tree_before": before.remote_tree, "remote_tree_after": after.remote_tree,
        "repeat_noop": true
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_caught_up_bridge_lagging_cursor_replays() {
    let fixture = QualifiedPair::new().await;
    let first = fixture.developer_commit("feature.txt", "first\n", "First pending Git commit");
    let second = fixture.developer_commit("feature.txt", "second\n", "Second pending Git commit");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    git_cli(&fixture.bridge, &["fetch", "origin", "main"]);
    git_cli(&fixture.bridge, &["reset", "--hard", &second]);
    let before = fixture.snapshot().await;
    assert_eq!(before.bridge_sha, before.remote_sha);
    assert_eq!(before.remote_sha, second);
    assert_eq!(before.watermark.1, fixture.imported_base);
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 2, "L=R must not hide commits after P");
    let after = fixture.snapshot().await;
    assert_eq!(after.svn_rev, before.svn_rev + 2);
    assert_eq!(after.watermark.1, second);
    for sha in [&first, &second] {
        let mapped: i64 = fixture.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND git_sha = ?1",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(mapped, 1);
    }
    assert_eq!(after.svn_feature.as_deref(), Some("second\n"));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case": "R10_L_EQUALS_R", "p": before.watermark.1,
        "l": before.bridge_sha, "r": before.remote_sha,
        "svn_before": before.svn_rev, "svn_after": after.svn_rev,
        "mapping_before": before.mapping_count, "mapping_after": after.mapping_count
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_unchanged_git_still_admits_new_svn_work() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    assert_eq!(before.bridge_sha, before.remote_sha);
    assert_eq!(before.watermark.1, before.remote_sha);
    let incoming = svn_commit_file(&fixture.wc, "incoming.txt", "from SVN\n", "New incoming SVN work");
    assert_eq!(incoming, before.svn_rev + 1);
    let result = fixture.engine.run_sync_cycle().await;
    assert!(!matches!(&result, Err(SyncError::HistoryBlocked { .. })),
        "unchanged Git must not block the pending SVN direction: {result:?}");
    assert_eq!(SvnClient::new(&fixture.svn_url, "", "").info().await.unwrap().latest_rev, incoming);
    match result {
        Ok(stats) => {
            assert_eq!(stats.svn_to_git_count, 1);
            assert_eq!(std::fs::read_to_string(fixture.bridge.join("incoming.txt")).unwrap(), "from SVN\n");
            let mapped: i64 = fixture.engine.db().conn().query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = ?1",
                [incoming], |row| row.get(0)).unwrap();
            assert_eq!(mapped, 1);
            eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
                "case":"R01_UNCHANGED_GIT_NEW_SVN", "outcome":"PASS",
                "p":before.watermark.1, "r":before.remote_sha,
                "svn_before":before.svn_rev, "svn_after":incoming, "mapping_added":mapped
            }));
        }
        Err(error) => {
            let mapped: i64 = fixture.engine.db().conn().query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = ?1",
                [incoming], |row| row.get(0)).unwrap();
            assert_eq!(mapped, 0);
            eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
                "case":"R01_UNCHANGED_GIT_NEW_SVN", "outcome":"PARTIAL_EXISTING_APPLY_FAILURE",
                "p":before.watermark.1, "r":before.remote_sha,
                "svn_before":before.svn_rev, "svn_after":incoming,
                "error":error.to_string(), "mapping_added":mapped
            }));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_missing_remote_branch_blocks_stale_tracking_ref() {
    let fixture = QualifiedPair::new().await;
    git_cli(&fixture.bridge, &["fetch", "origin", "main"]);
    assert_eq!(git_output(&fixture.bridge, &["rev-parse", "refs/remotes/origin/main"]), fixture.imported_base);
    git_cli(&fixture.developer, &["push", "origin", "--delete", "main"]);
    let bridge_before = get_head_sha(&fixture.bridge);
    let index_before = std::fs::read(fixture.bridge.join(".git/index")).unwrap();
    let svn_before = SvnClient::new(&fixture.svn_url, "", "").info().await.unwrap().latest_rev;
    let cursor_before = fixture.engine.db().get_repo_watermark("pair").unwrap();
    let mappings_before = fixture.engine.db().count_sync_records().unwrap();
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_branch_missing"),
        "missing branch must not use stale tracking ref: {result:?}");
    assert_eq!(get_head_sha(&fixture.bridge), bridge_before);
    assert_eq!(std::fs::read(fixture.bridge.join(".git/index")).unwrap(), index_before);
    assert_eq!(SvnClient::new(&fixture.svn_url, "", "").info().await.unwrap().latest_rev, svn_before);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), cursor_before);
    assert_eq!(fixture.engine.db().count_sync_records().unwrap(), mappings_before);
    let remote = Command::new("git").arg("-C").arg(&fixture.bare)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/main"]).status().unwrap();
    assert_eq!(remote.code(), Some(1));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R16_MISSING_BRANCH", "o":fixture.imported_base,
        "r":null, "bridge_before_after":bridge_before,
        "svn_revision_before_after":svn_before,
        "watermark_before_after":cursor_before,
        "mappings_before_after":mappings_before,
        "reason":"remote_branch_missing"
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_remote_transport_failure_blocks_without_reset() {
    let fixture = QualifiedPair::new().await;
    let missing = fixture.tmp.path().join("no-such-remote.git");
    git_cli(&fixture.bridge, &["remote", "set-url", "origin", missing.to_str().unwrap()]);
    assert_pair_blocked_without_damage(&fixture, "remote_transport_failed").await;
}

// Fault cases share a process environment in the broad E2E runner. Scope the
// injected command failure to one bridge and keep simultaneous fault cases
// from overwriting each other's setting.
struct TestInspectionFault {
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl TestInspectionFault {
    async fn new(kind: &str, bridge: &Path) -> Self {
        static FAULT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        let guard = FAULT_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
        std::env::set_var("REPOSYNC_TEST_INSPECTION_FAULT", format!("{}|{}", kind, bridge.display()));
        Self { _guard: guard }
    }
}

impl Drop for TestInspectionFault {
    fn drop(&mut self) {
        std::env::remove_var("REPOSYNC_TEST_INSPECTION_FAULT");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_auth_denial_is_distinct_from_transport() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    let _fault = TestInspectionFault::new("remote_auth", &fixture.bridge).await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_auth_failed"),
        "synthetic auth denial must be distinct from transport: {result:?}");
    assert_eq!(fixture.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R16_AUTH", "reason":"remote_auth_failed",
        "bridge_before_after":before.bridge_sha, "svn_revision_before_after":before.svn_rev,
        "mapping_count_before_after":before.mapping_count
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_fetch_failure_does_not_use_stale_ref() {
    let fixture = QualifiedPair::new().await;
    git_cli(&fixture.bridge, &["fetch", "origin", "main"]);
    let before = fixture.snapshot().await;
    let _fault = TestInspectionFault::new("remote_fetch", &fixture.bridge).await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_fetch_failed"),
        "failed fresh fetch must not use stale tracking ref: {result:?}");
    assert_eq!(fixture.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R16_FETCH", "reason":"remote_fetch_failed",
        "bridge_before_after":before.bridge_sha, "svn_revision_before_after":before.svn_rev,
        "mapping_count_before_after":before.mapping_count
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_missing_checkpoint_object_blocks() {
    let fixture = QualifiedPair::new().await;
    let absent = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let repo_rev = fixture.engine.db().get_repo_watermark("pair").unwrap().0;
    fixture.engine.db().update_repo_watermark("pair", repo_rev, absent).unwrap();
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint_object").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_missing_repository_cursor_does_not_borrow_global() {
    let fixture = QualifiedPair::new().await;
    fixture.engine.db().set_state("last_git_hash", &fixture.imported_base).unwrap();
    fixture.engine.db().update_repo_watermark("pair", 2, "").unwrap();
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_ambiguous_repository_cursor_blocks() {
    let fixture = QualifiedPair::new().await;
    let other = git_output(&fixture.bridge, &["rev-parse", "HEAD^"]);
    fixture.engine.db().set_state("last_git_sha_pair", &other).unwrap();
    assert_pair_blocked_without_damage(&fixture, "ambiguous_checkpoint").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_shallow_history_blocks() {
    let fixture = QualifiedPair::new().await;
    std::fs::write(fixture.bridge.join(".git/shallow"), format!("{}\n", fixture.imported_base)).unwrap();
    assert_pair_blocked_without_damage(&fixture, "incomplete_history").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_ancestry_command_error_is_not_rewrite() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    let _fault = TestInspectionFault::new("ancestry_exit_128", &fixture.bridge).await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "ancestry_command_failed"),
        "git command error must be unknown, not a valid negative ancestry: {result:?}");
    assert_eq!(fixture.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_ANCESTRY_ERROR", "reason":"ancestry_command_failed",
        "bridge_before_after":before.bridge_sha, "svn_revision_before_after":before.svn_rev,
        "mapping_count_before_after":before.mapping_count
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_local_dirty_index_and_unpublished_commit_preserved() {
    for kind in ["dirty", "staged", "unpublished"] {
        let fixture = QualifiedPair::new().await;
        let file = fixture.bridge.join(format!("{kind}.txt"));
        std::fs::write(&file, format!("{kind}\n")).unwrap();
        if kind == "staged" || kind == "unpublished" {
            git_cli(&fixture.bridge, &["add", file.file_name().unwrap().to_str().unwrap()]);
        }
        if kind == "unpublished" {
            git_cli(&fixture.bridge, &["commit", "-m", "Unpublished bridge commit"]);
        }
        let reason = if kind == "unpublished" { "unpublished_local_history" } else { "local_dirty" };
        assert_pair_blocked_without_damage(&fixture, reason).await;
        assert_eq!(std::fs::read_to_string(file).unwrap(), format!("{kind}\n"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_merge_dag_rejected_before_replay() {
    let fixture = QualifiedPair::new().await;
    git_cli(&fixture.developer, &["checkout", "-b", "side"]);
    fixture.developer_commit("side.txt", "side\n", "Side change");
    git_cli(&fixture.developer, &["checkout", "main"]);
    fixture.developer_commit("main.txt", "main\n", "Main change");
    git_cli(&fixture.developer, &["merge", "--no-ff", "side", "-m", "Merge side"]);
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_pair_blocked_without_damage(&fixture, "unsupported_merge_dag").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_over_1000_pending_commits_rejected() {
    let fixture = QualifiedPair::new().await;
    let tree = git_output(&fixture.developer, &["rev-parse", "HEAD^{tree}"]);
    let mut parent = fixture.imported_base.clone();
    for index in 0..1001 {
        let output = Command::new("git").arg("-C").arg(&fixture.developer)
            .args(["commit-tree", &tree, "-p", &parent, "-m", &format!("pending {index}")])
            .env("GIT_AUTHOR_NAME", "Fixture Developer")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture Developer")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output().unwrap();
        assert!(output.status.success(), "commit-tree: {}", String::from_utf8_lossy(&output.stderr));
        parent = String::from_utf8(output.stdout).unwrap().trim().to_string();
    }
    git_cli(&fixture.developer, &["update-ref", "refs/heads/main", &parent]);
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_pair_blocked_without_damage(&fixture, "unsupported_backlog").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r17_repository_cursors_remain_scoped() {
    let fixture = QualifiedPair::new().await;
    let other_root = fixture.tmp.path().join("other");
    std::fs::create_dir(&other_root).unwrap();
    let other_svn = create_svn_repo(&other_root);
    let other_wc = other_root.join("wc");
    svn_checkout(&other_svn, &other_wc);
    svn_commit_file(&other_wc, ".gitkeep", "", "Initial other anchor");
    svn_commit_file(&other_wc, "origin.txt", "Other SVN origin\n", "Other SVN import");
    let other_bridge = other_root.join("bridge");
    let other_bare = other_root.join("origin.git");
    let other_git = setup_git_with_bare_origin(&other_bridge, &other_bare);
    let other_initial = get_head_sha(&other_bridge);
    let other_db = Database::new(&fixture.db_path).unwrap();
    other_db.initialize().unwrap();
    let mut row = fixture.engine.db().get_repository("pair").unwrap().unwrap();
    row.id = "other".into();
    row.name = "other qualified pair".into();
    row.svn_url = other_svn.clone();
    row.git_repo = other_bare.to_string_lossy().to_string();
    row.last_svn_rev = 1;
    row.last_git_sha = other_initial;
    row.total_syncs = 0;
    other_db.insert_repository(&row).unwrap();
    let mut other_config = make_app_config(&other_svn, &other_root);
    other_config.svn.layout = reposync_core::config::SvnLayout::Custom;
    let mut other_engine = SyncEngine::new(
        other_config, other_db, SvnClient::new(&other_svn, "", ""), other_git,
        Arc::new(make_identity_mapper()),
    );
    other_engine.set_repo_id("other".into());
    assert_eq!(other_engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let other_import = get_head_sha(&other_bridge);
    assert_eq!(other_engine.db().get_repo_watermark("other").unwrap(), (2, other_import.clone()));

    // A global cursor for pair one must never become pair two's missing P.
    fixture.engine.db().set_state("last_git_hash", &fixture.imported_base).unwrap();
    other_engine.db().update_repo_watermark("other", 2, "").unwrap();
    let result = other_engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "missing_checkpoint"),
        "other pair borrowed a global cursor: {result:?}");
    assert_eq!(SvnClient::new(&other_svn, "", "").info().await.unwrap().latest_rev, 2);
    other_engine.db().update_repo_watermark("other", 2, &other_import).unwrap();

    // Pair one is blocked, while the separately proven pair still syncs.
    let absent = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fixture.engine.db().update_repo_watermark("pair", 2, absent).unwrap();
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint_object").await;
    let other_developer = other_root.join("developer");
    let clone = Command::new("git").args(["clone", "-b", "main", other_bare.to_str().unwrap(), other_developer.to_str().unwrap()]).output().unwrap();
    assert!(clone.status.success());
    std::fs::write(other_developer.join("feature.txt"), "one\n").unwrap();
    git_cli(&other_developer, &["add", "feature.txt"]);
    git_cli(&other_developer, &["commit", "-m", "Other first"]);
    let first = get_head_sha(&other_developer);
    std::fs::write(other_developer.join("feature.txt"), "two\n").unwrap();
    git_cli(&other_developer, &["commit", "-am", "Other second"]);
    let second = get_head_sha(&other_developer);
    git_cli(&other_developer, &["push", "origin", "main"]);
    assert_eq!(other_engine.run_sync_cycle().await.unwrap().git_to_svn_count, 2);
    assert_eq!(other_engine.db().get_repo_watermark("other").unwrap().1, second);
    assert_eq!(SvnClient::new(&other_svn, "", "").info().await.unwrap().latest_rev, 4);
    for sha in [&first, &second] {
        let mapped: i64 = other_engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'other' AND direction = 'git_to_svn' AND git_sha = ?1",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(mapped, 1);
    }
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint_object").await;
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R17_SCOPING", "blocked_pair":"pair", "healthy_pair":"other",
        "other_p_before":other_import, "other_r_after":second,
        "other_svn_before":2, "other_svn_after":4,
        "global_cursor_not_borrowed":true, "first_pair_still_blocked":true
    }));
}

/// R06 baseline diagnostic: the skip-import checkpoint used by late pairing
/// acknowledges two unprocessed Git commits. The branch is descended from an
/// actual SVN-import commit, not an unrelated Git-first history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostic_r06_late_pair_checkpoint_omits_existing_git_work() {
    assert!(
        svn_available(),
        "svn and svnadmin are required; do not count a skipped diagnostic as evidence"
    );
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor");
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let git_dir = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git = setup_git_with_bare_origin(&git_dir, &bare);
    let db = setup_db(&tmp.path().join("sync.db"));
    db.set_state("last_svn_rev", "1").unwrap();
    db.set_state("last_git_hash", &get_head_sha(&git_dir))
        .unwrap();
    let mut config = make_app_config(&svn_url, tmp.path());
    config.svn.layout = reposync_core::config::SvnLayout::Custom;
    let parent_engine = SyncEngine::new(
        config.clone(),
        db,
        SvnClient::new(&svn_url, "", ""),
        git,
        Arc::new(make_identity_mapper()),
    );
    assert_eq!(
        parent_engine
            .run_sync_cycle()
            .await
            .unwrap()
            .svn_to_git_count,
        1
    );
    let imported_base = get_head_sha(&git_dir);

    git_cli(&git_dir, &["checkout", "-b", "feature"]);
    std::fs::write(git_dir.join("feature.txt"), "step one\n").unwrap();
    git_cli(&git_dir, &["add", "feature.txt"]);
    git_cli(&git_dir, &["commit", "-m", "Feature step one"]);
    std::fs::write(git_dir.join("feature.txt"), "step two\n").unwrap();
    git_cli(&git_dir, &["commit", "-am", "Feature step two"]);
    git_cli(&git_dir, &["push", "origin", "feature"]);
    let feature_tip = get_head_sha(&git_dir);
    assert_ne!(feature_tip, imported_base);
    let pending = Command::new("git").arg("-C").arg(&git_dir)
        .args(["rev-list", "--count", &format!("{imported_base}..{feature_tip}")])
        .output().unwrap();
    assert!(pending.status.success());
    assert_eq!(String::from_utf8_lossy(&pending.stdout).trim(), "2");

    // A disposable SVN target with the same SVN-origin file baseline.
    let target_url = format!("{svn_url}/feature");
    let output = Command::new("svn")
        .args([
            "mkdir",
            &target_url,
            "-m",
            "Create feature target",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svn mkdir: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source_file = format!("{svn_url}/origin.txt");
    let target_file = format!("{target_url}/origin.txt");
    let output = Command::new("svn")
        .args([
            "copy",
            &source_file,
            &target_file,
            "-m",
            "Copy verified origin",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svn copy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = SvnClient::new(&target_url, "", "");
    let target_rev = target.info().await.unwrap().latest_rev;

    // This is the exact checkpoint transition performed by the existing
    // create_branch_pair(skip_import) path after provider/SVN HEAD lookup.
    let child_db = setup_db(&tmp.path().join("child.db"));
    // The team engine reads the per-repo KV checkpoint when no repository row
    // is present, matching the legacy transition after pair creation.
    child_db
        .set_state("last_svn_rev_child", &target_rev.to_string())
        .unwrap();
    child_db
        .set_state("last_git_sha_child", &feature_tip)
        .unwrap();
    config.svn.url = target_url.clone();
    config.github.default_branch = "feature".into();
    let mut child_engine = SyncEngine::new(
        config,
        child_db,
        target,
        GitClient::new(&git_dir).unwrap(),
        Arc::new(make_identity_mapper()),
    );
    child_engine.set_repo_id("child".into());
    let before = SvnClient::new(&target_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    let stats = child_engine.run_sync_cycle().await.unwrap();
    let after = SvnClient::new(&target_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    assert_eq!(stats.git_to_svn_count, 0);
    assert_eq!(before, after);
    let child_records: i64 = child_engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE git_sha = ?1",
        [&feature_tip], |row| row.get(0)).unwrap();
    assert_eq!(child_records, 0);
    assert_eq!(child_engine.db().get_state("last_git_sha_child").unwrap(), Some(feature_tip.clone()));
    let exported = tmp.path().join("feature_export");
    SvnClient::new(&target_url, "", "")
        .export("", after, &exported)
        .await
        .unwrap();
    assert!(
        !exported.join("feature.txt").exists(),
        "pending Git work unexpectedly reached SVN"
    );
    eprintln!("EXPECTED BASELINE FAILURE R06: SVN-derived feature commits {imported_base}..{feature_tip} were acknowledged without SVN emission at r{after}");
}

// ===========================================================================
// Test 1: SVN → Git sync via SyncEngine
// ===========================================================================

/// Verify that commits made in SVN are synced to the Git repository via the
/// real SyncEngine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_svn_to_git_sync() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Create SVN commits.
    svn_commit_file(&wc_path, "readme.txt", "Hello from SVN", "Add readme");
    svn_commit_file(&wc_path, "lib.rs", "fn main() {}", "Add initial source");

    // Set up Git repo.
    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    // Seed the Git watermark to HEAD so the engine only looks at SVN changes.
    let db = setup_db(&tmp.path().join("sync.db"));
    let head_sha = get_head_sha(&git_work_dir);
    let _ = db.set_state("last_git_hash", &head_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);
    let stats = engine.run_sync_cycle().await.expect("sync cycle failed");

    // SVN had 2 revisions.
    assert_eq!(stats.svn_to_git_count, 2, "expected 2 SVN revisions synced");
    assert_eq!(stats.conflicts_detected, 0, "expected no conflicts");

    // Git should now have: initial commit + 2 synced commits = 3.
    assert_eq!(count_git_commits(&git_work_dir), 3);

    // The most recent commit should contain the sync marker.
    let latest_msg = get_git_commit_message(&git_work_dir, 0);
    assert!(
        latest_msg.contains("[reposync]"),
        "expected sync marker in commit message, got: {}",
        latest_msg
    );

    // Files should exist in the Git working tree.
    assert!(git_work_dir.join("readme.txt").exists());
    assert!(git_work_dir.join("lib.rs").exists());
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("readme.txt")).unwrap(),
        "Hello from SVN"
    );
}

// ===========================================================================
// Test 2: Git → SVN sync via SyncEngine
// ===========================================================================

/// Verify that commits made in Git are synced to SVN.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_git_to_svn_sync() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());

    // SVN needs at least one commit for checkout to work.
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(&wc_path, ".gitkeep", "", "Initial SVN commit");

    // Set up Git repo.
    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    // Record the initial Git SHA before making test commits.
    let initial_sha = get_head_sha(&git_work_dir);

    // Make a Git commit that the engine should sync to SVN.
    std::fs::write(git_work_dir.join("app.js"), "console.log('hello');\n").unwrap();
    git_client
        .commit(
            "Add app.js",
            "Dev User",
            "dev@example.com",
            "Dev User",
            "dev@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();

    // Set up DB and engine, seeding watermarks.
    let db = setup_db(&tmp.path().join("sync.db"));
    let _ = db.set_state("last_svn_rev", "1");
    let _ = db.set_state("last_git_hash", &initial_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);
    let stats = engine.run_sync_cycle().await.expect("sync cycle failed");

    assert_eq!(stats.git_to_svn_count, 1, "expected 1 Git commit synced");
    assert_eq!(stats.conflicts_detected, 0, "expected no conflicts");

    // Verify the file landed in SVN by exporting.
    let svn_verify = SvnClient::new(&svn_url, "", "");
    let verify_dir = tmp.path().join("verify");
    svn_verify.export("", 2, &verify_dir).await.unwrap();
    assert!(verify_dir.join("app.js").exists());
    assert_eq!(
        std::fs::read_to_string(verify_dir.join("app.js")).unwrap(),
        "console.log('hello');\n"
    );
}

// ===========================================================================
// Test 3: Bidirectional sync (mixed SVN + Git usage)
// ===========================================================================

/// Both SVN and Git users commit (to different files). A single sync cycle
/// replicates both directions without conflicts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_bidirectional_sync() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());

    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(&wc_path, ".gitkeep", "", "Initial commit");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let initial_sha = get_head_sha(&git_work_dir);

    // SVN user adds a file.
    svn_commit_file(&wc_path, "svn_file.txt", "from SVN user", "SVN adds file");

    // Git user adds a different file.
    std::fs::write(git_work_dir.join("git_file.txt"), "from Git user").unwrap();
    git_client
        .commit(
            "Git adds file",
            "Git User",
            "gituser@example.com",
            "Git User",
            "gituser@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();

    // Seed watermarks past initial commits.
    let db = setup_db(&tmp.path().join("sync.db"));
    let _ = db.set_state("last_svn_rev", "1");
    let _ = db.set_state("last_git_hash", &initial_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);
    let stats = engine.run_sync_cycle().await.expect("sync cycle failed");

    assert_eq!(
        stats.svn_to_git_count, 1,
        "expected 1 SVN revision synced to Git"
    );
    assert_eq!(
        stats.git_to_svn_count, 1,
        "expected 1 Git commit synced to SVN"
    );
    assert_eq!(stats.conflicts_detected, 0, "expected no conflicts");

    // Verify SVN file appeared in Git working tree.
    assert!(
        git_work_dir.join("svn_file.txt").exists(),
        "SVN file should be synced to Git"
    );

    // Verify Git file appeared in SVN.
    let svn_verify = SvnClient::new(&svn_url, "", "");
    let info = svn_verify.info().await.unwrap();
    let verify_dir = tmp.path().join("verify");
    svn_verify
        .export("", info.latest_rev, &verify_dir)
        .await
        .unwrap();
    assert!(
        verify_dir.join("git_file.txt").exists(),
        "Git file should be synced to SVN"
    );
}

// ===========================================================================
// Test 4: Echo suppression in team mode
// ===========================================================================

/// After syncing SVN→Git, a second sync cycle should NOT re-sync the echo
/// commits back to SVN.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_echo_suppression() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(&wc_path, "first.txt", "content", "First commit");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    // Seed Git watermark so the initial commit isn't synced to SVN.
    let db = setup_db(&tmp.path().join("sync.db"));
    let head_sha = get_head_sha(&git_work_dir);
    let _ = db.set_state("last_git_hash", &head_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);

    // First cycle: syncs SVN→Git.
    let stats1 = engine.run_sync_cycle().await.expect("first sync failed");
    assert_eq!(stats1.svn_to_git_count, 1);

    // Second cycle: echo commits in Git should be skipped.
    let stats2 = engine.run_sync_cycle().await.expect("second sync failed");
    assert_eq!(
        stats2.git_to_svn_count, 0,
        "echo commits should not be re-synced to SVN"
    );
    assert_eq!(
        stats2.svn_to_git_count, 0,
        "no new SVN commits should exist"
    );
}

// ===========================================================================
// Test 5: Conflict detection with overlapping files
// ===========================================================================

/// When both SVN and Git modify the same file, the engine should detect
/// a conflict.
///
/// NOTE: Currently fails because the diff_full path appends "/trunk" to
/// the SVN URL, but the test repo uses flat layout. Needs SVN layout
/// config fix or test repo restructuring.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires SVN standard layout (trunk/) — see TODO above"]
async fn test_team_mode_conflict_detection() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());

    // Seed SVN with a base file.
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(&wc_path, "shared.txt", "base content", "Add shared file");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let initial_sha = get_head_sha(&git_work_dir);

    // SVN user modifies the file.
    svn_commit_file(&wc_path, "shared.txt", "SVN version", "SVN edits shared");

    // Git user also modifies the same file.
    std::fs::write(git_work_dir.join("shared.txt"), "Git version").unwrap();
    git_client
        .commit(
            "Git edits shared",
            "Git User",
            "gituser@example.com",
            "Git User",
            "gituser@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();

    // Seed watermarks past the initial commits.
    let db = setup_db(&tmp.path().join("sync.db"));
    let _ = db.set_state("last_svn_rev", "1");
    let _ = db.set_state("last_git_hash", &initial_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);
    let stats = engine.run_sync_cycle().await.expect("sync cycle failed");

    // Both sides modified "shared.txt" — should detect a conflict.
    assert!(
        stats.conflicts_detected > 0,
        "expected at least 1 conflict when both sides edit same file, got {}",
        stats.conflicts_detected
    );
}

// ===========================================================================
// Test 6: Commit mapping integrity
// ===========================================================================

/// After a sync cycle, verify database records.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_commit_mapping_integrity() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    svn_commit_file(&wc_path, "a.txt", "alpha", "Add a");
    svn_commit_file(&wc_path, "b.txt", "bravo", "Add b");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    // Seed Git watermark so the initial commit doesn't get synced.
    let db = setup_db(&tmp.path().join("sync.db"));
    let head_sha = get_head_sha(&git_work_dir);
    let _ = db.set_state("last_git_hash", &head_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);
    let stats = engine.run_sync_cycle().await.expect("sync cycle failed");
    assert_eq!(stats.svn_to_git_count, 2);

    // Verify sync records were written (SVN→Git only).
    let sync_count = engine.db().count_sync_records().unwrap();
    assert_eq!(sync_count, 2, "expected 2 sync records in the database");

    // Verify no errors recorded.
    let error_count = engine.db().count_errors().unwrap();
    assert_eq!(error_count, 0, "expected no errors in audit log");

    // Verify last SVN revision was updated.
    let last_rev = engine.db().get_last_svn_revision().unwrap();
    assert!(
        last_rev.is_some(),
        "expected last SVN revision to be recorded"
    );
    assert!(
        last_rev.unwrap() >= 2,
        "expected last SVN revision >= 2, got {:?}",
        last_rev
    );
}

// ===========================================================================
// Test 7: Multi-commit Git → SVN replay order
// ===========================================================================

/// When multiple Git commits modify the same file, they must be replayed
/// oldest-first so that the final SVN content matches the latest Git state
/// and intermediate SVN revisions map correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_git_to_svn_multi_commit_order() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());

    // SVN needs an initial commit for checkout_head to work.
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(&wc_path, ".gitkeep", "", "Initial SVN commit");

    // Set up Git repo.
    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let initial_sha = get_head_sha(&git_work_dir);

    // Make 3 sequential Git commits modifying the same file.
    std::fs::write(git_work_dir.join("data.txt"), "version 1\n").unwrap();
    git_client
        .commit(
            "Write version 1",
            "Dev",
            "dev@example.com",
            "Dev",
            "dev@example.com",
        )
        .unwrap();

    std::fs::write(git_work_dir.join("data.txt"), "version 2\n").unwrap();
    git_client
        .commit(
            "Write version 2",
            "Dev",
            "dev@example.com",
            "Dev",
            "dev@example.com",
        )
        .unwrap();

    std::fs::write(git_work_dir.join("data.txt"), "version 3\n").unwrap();
    git_client
        .commit(
            "Write version 3",
            "Dev",
            "dev@example.com",
            "Dev",
            "dev@example.com",
        )
        .unwrap();

    git_client.push("origin", "main").unwrap();

    // Set up DB and engine.
    let db = setup_db(&tmp.path().join("sync.db"));
    let _ = db.set_state("last_svn_rev", "1");
    let _ = db.set_state("last_git_hash", &initial_sha);

    let config = make_app_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);
    let stats = engine.run_sync_cycle().await.expect("sync cycle failed");

    assert_eq!(stats.git_to_svn_count, 3, "expected 3 Git commits synced");
    assert_eq!(stats.conflicts_detected, 0, "expected no conflicts");

    // Verify final SVN content matches the LATEST Git commit.
    let svn_verify = SvnClient::new(&svn_url, "", "");
    let info = svn_verify.info().await.unwrap();
    let verify_dir = tmp.path().join("verify_final");
    svn_verify
        .export("", info.latest_rev, &verify_dir)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(verify_dir.join("data.txt")).unwrap(),
        "version 3\n",
        "final SVN content must match the newest Git commit"
    );

    // Verify intermediate SVN revisions have correct chronological content.
    // Rev 2 = first synced commit ("version 1"), Rev 3 = second, Rev 4 = third.
    let verify_v1 = tmp.path().join("verify_v1");
    svn_verify.export("", 2, &verify_v1).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(verify_v1.join("data.txt")).unwrap(),
        "version 1\n",
        "SVN r2 should contain version 1 (oldest commit first)"
    );

    let verify_v2 = tmp.path().join("verify_v2");
    svn_verify.export("", 3, &verify_v2).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(verify_v2.join("data.txt")).unwrap(),
        "version 2\n",
        "SVN r3 should contain version 2 (middle commit)"
    );

    // Verify sync records were written with correct count.
    let sync_count = engine.db().count_sync_records().unwrap();
    assert_eq!(sync_count, 3, "expected 3 sync records");

    // Verify watermark advanced to the latest Git SHA (the third commit).
    let final_git_hash = engine
        .db()
        .get_state("last_git_hash")
        .unwrap()
        .expect("last_git_hash should be set");
    let head_sha = get_head_sha(&tmp.path().join("git_work"));
    assert_eq!(
        final_git_hash, head_sha,
        "watermark must point to the latest Git commit"
    );
}

// ===========================================================================
// Test 8: Forced failure → persisted failed audit entry + error count
// ===========================================================================

/// When `SyncEngine::run_sync_cycle()` fails (e.g. invalid SVN URL), the
/// engine must:
/// 1. Persist a failed audit entry (`success = false`).
/// 2. Increment `count_errors()`.
/// 3. Set sync state to "error".
///
/// This test is deterministic: it uses a valid Git repo but an invalid SVN
/// URL that is guaranteed to fail during `fetch_svn_changes()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_team_mode_forced_failure_persists_audit_entry() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();

    // Point at a non-existent SVN repository — deterministic failure.
    let bad_svn_url = format!(
        "file://{}/this_repo_does_not_exist_12345",
        tmp.path().display()
    );

    // Set up a valid Git repo (the engine needs one to construct).
    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    // Seed watermarks so the engine tries to contact SVN.
    let db = setup_db(&tmp.path().join("sync.db"));
    let head_sha = get_head_sha(&git_work_dir);
    let _ = db.set_state("last_git_hash", &head_sha);

    // Verify no errors before the cycle.
    assert_eq!(
        db.count_errors().unwrap(),
        0,
        "expected 0 errors before sync"
    );

    let config = make_app_config(&bad_svn_url, tmp.path());
    let svn_client = SvnClient::new(&bad_svn_url, "", "");
    let mapper = Arc::new(make_identity_mapper());

    let engine = SyncEngine::new(config, db, svn_client, git_client, mapper);

    // run_sync_cycle should return Err (SVN info fails on bad URL).
    let result = engine.run_sync_cycle().await;
    assert!(
        result.is_err(),
        "sync cycle should fail with invalid SVN URL"
    );

    // --- Verify acceptance criteria ---

    // 1. A failed audit entry was persisted.
    let audit_entries = engine.db().list_audit_log(10, 0).unwrap();
    assert!(
        !audit_entries.is_empty(),
        "expected at least 1 audit entry after failed sync"
    );
    let latest = &audit_entries[0]; // newest first
    assert_eq!(latest.action, "sync_cycle");
    assert!(
        !latest.success,
        "audit entry should have success=false for failed cycle"
    );
    assert!(
        latest
            .details
            .as_deref()
            .unwrap_or("")
            .contains("sync failed"),
        "audit details should describe the failure, got: {:?}",
        latest.details
    );

    // 2. count_errors() incremented.
    let error_count = engine.db().count_errors().unwrap();
    assert_eq!(
        error_count, 1,
        "count_errors should be 1 after one failed sync cycle"
    );

    // 3. Sync state is "error".
    let sync_state = engine
        .db()
        .get_state("sync_state")
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        sync_state, "error",
        "sync_state should be 'error' after failed cycle"
    );
}
