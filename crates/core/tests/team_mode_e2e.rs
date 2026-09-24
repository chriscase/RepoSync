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
use std::collections::BTreeMap;
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
        assert!(
            target.starts_with(&root),
            "fixture target escaped owned root: {}",
            target.display()
        );
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

fn tracked_tree_at(repo: &Path, revision: &str) -> BTreeMap<String, Vec<u8>> {
    let listing = Command::new("git").arg("-C").arg(repo)
        .args(["ls-tree", "-r", "--name-only", "-z", revision])
        .output().unwrap();
    assert!(listing.status.success());
    let mut files = BTreeMap::new();
    for name in listing.stdout.split(|byte| *byte == 0).filter(|name| !name.is_empty()) {
        let name = std::str::from_utf8(name).unwrap();
        let output = Command::new("git").arg("-C").arg(repo)
            .args(["show", &format!("{revision}:{name}")]).output().unwrap();
        assert!(output.status.success(), "cannot read tracked {name}");
        files.insert(name.to_string(), output.stdout);
    }
    files
}

fn tracked_tree(repo: &Path) -> BTreeMap<String, Vec<u8>> {
    tracked_tree_at(repo, "HEAD")
}

fn fixture_tree() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (".gitkeep".into(), Vec::new()),
        ("origin.txt".into(), b"SVN origin\n".to_vec()),
        ("loose.txt".into(), b"untouched top-level\n".to_vec()),
        ("notes/keep.txt".into(), b"untouched directory\n".to_vec()),
    ])
}

fn tree_hashes(tree: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    tree.iter().map(|(name, bytes)|
        (name.clone(), hex::encode(sha2::Sha256::digest(bytes)))).collect()
}

fn exported_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, files);
            } else {
                assert!(entry.file_type().unwrap().is_file());
                let name = path.strip_prefix(root).unwrap().to_str().unwrap().replace('\\', "/");
                files.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[cfg(feature = "reliability-fixture")]
fn copy_install_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_install_tree(&from, &to);
        } else {
            assert!(entry.file_type().unwrap().is_file(), "old fixture contains a non-file entry");
            std::fs::copy(from, to).unwrap();
        }
    }
}

async fn svn_tree(fixture: &QualifiedPair, revision: i64) -> BTreeMap<String, Vec<u8>> {
    let destination = fixture.tmp.path().join(format!("tree-{}-{}", revision, uuid::Uuid::new_v4()));
    SvnClient::new(&fixture.svn_url, "", "").export("", revision, &destination).await.unwrap();
    exported_tree(&destination)
}

fn svn_delete_file(wc: &Path, name: &str, message: &str) -> i64 {
    let output = Command::new("svn").args(["rm", wc.join(name).to_str().unwrap()]).output().unwrap();
    assert!(output.status.success(), "svn rm: {}", String::from_utf8_lossy(&output.stderr));
    let output = Command::new("svn").args([
        "commit", "-m", message, wc.to_str().unwrap(), "--username", "fixture", "--non-interactive",
    ]).output().unwrap();
    assert!(output.status.success(), "svn delete commit: {}", String::from_utf8_lossy(&output.stderr));
    let line = String::from_utf8_lossy(&output.stdout);
    line.lines().find_map(|line| line.strip_prefix("Committed revision "))
        .unwrap().trim_end_matches('.').parse().unwrap()
}

fn svn_property_only_revision(wc: &Path) -> i64 {
    let update = Command::new("svn").args(["update", wc.to_str().unwrap()]).output().unwrap();
    assert!(update.status.success(), "svn update: {}", String::from_utf8_lossy(&update.stderr));
    let output = Command::new("svn")
        .args(["propset", "svn:ignore", "*.cache", wc.to_str().unwrap()])
        .output().unwrap();
    assert!(output.status.success(), "svn propset: {}", String::from_utf8_lossy(&output.stderr));
    let output = Command::new("svn").args([
        "commit", "-m", "Property-only revision", wc.to_str().unwrap(),
        "--username", "fixture", "--non-interactive",
    ]).output().unwrap();
    assert!(output.status.success(), "svn property commit: {}", String::from_utf8_lossy(&output.stderr));
    let line = String::from_utf8_lossy(&output.stdout);
    line.lines().find_map(|line| line.strip_prefix("Committed revision "))
        .unwrap().trim_end_matches('.').parse().unwrap()
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
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
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
        assert!(
            svn_available(),
            "SVN tools are required for candidate evidence"
        );
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
            id: "pair".into(),
            name: "qualified pair".into(),
            svn_url: svn_url.clone(),
            svn_branch: "".into(),
            svn_username: "fixture".into(),
            git_provider: "local".into(),
            git_api_url: "".into(),
            git_repo: bare.to_string_lossy().to_string(),
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
            last_svn_rev: 1,
            last_git_sha: initial,
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        let mut config = make_app_config(&svn_url, tmp.path());
        config.svn.layout = reposync_core::config::SvnLayout::Custom;
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new(&svn_url, "", ""),
            git,
            Arc::new(make_identity_mapper()),
        );
        engine.set_repo_id("pair".into());
        assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
        let imported_base = get_head_sha(&bridge);
        assert_eq!(
            engine.db().get_repo_watermark("pair").unwrap(),
            (2, imported_base.clone())
        );
        let mapped: i64 = engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = 2 AND git_sha = ?1",
            [&imported_base], |row| row.get(0)).unwrap();
        assert_eq!(
            mapped, 1,
            "SVN-derived baseline must have the actual engine mapping"
        );
        assert_eq!(
            std::fs::read_to_string(bridge.join("origin.txt")).unwrap(),
            "SVN origin\n"
        );
        assert_eq!(
            git_output(&bare, &["rev-parse", "refs/heads/main"]),
            imported_base
        );
        let developer = tmp.path().join("developer");
        let clone = Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                developer.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            clone.status.success(),
            "developer clone: {}",
            String::from_utf8_lossy(&clone.stderr)
        );
        assert_eq!(get_head_sha(&developer), imported_base);
        Self {
            tmp,
            svn_url,
            wc,
            bridge,
            developer,
            bare,
            db_path,
            engine,
            imported_base,
        }
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
        let export = self
            .tmp
            .path()
            .join(format!("capture-{}", uuid::Uuid::new_v4()));
        svn.export("", svn_rev, &export).await.unwrap();
        let mapping_count: i64 = self
            .engine
            .db()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let repo_sync_count = self
            .engine
            .db()
            .get_repository("pair")
            .unwrap()
            .unwrap()
            .total_syncs;
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
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason: actual, .. }) if actual == reason),
        "expected {reason} rejection, got {result:?}"
    );
    assert_eq!(
        fixture.snapshot().await,
        before,
        "rejected pair changed protected state"
    );
    let block: serde_json::Value = serde_json::from_str(
        &fixture
            .engine
            .db()
            .get_state("team_history_block_pair")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(block["reason"], reason);
    assert_eq!(
        fixture
            .engine
            .db()
            .get_state("sync_state")
            .unwrap()
            .as_deref(),
        Some("reconciliation_required")
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":reason, "p_before":before.watermark.1,
            "l_before":before.bridge_sha, "r_before":before.remote_sha,
            "svn_revision_before_after":before.svn_rev,
            "remote_tree_before_after":before.remote_tree,
            "bridge_tree_before_after":before.bridge_tree,
            "bridge_index_sha256_before_after":hex::encode(sha2::Sha256::digest(&before.bridge_index)),
            "mapping_count_before_after":before.mapping_count,
            "success_count_before_after":before.repo_sync_count,
            "reason":reason, "after_equal":true
        })
    );
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
    assert_eq!(
        import_record, 1,
        "SVN-origin baseline must have a production sync mapping"
    );
    assert_eq!(
        std::fs::read_to_string(git_dir.join("origin.txt")).unwrap(),
        "SVN origin\n"
    );

    let developer = tmp.path().join("developer");
    let clone = Command::new("git")
        .args([
            "clone",
            "-b",
            "main",
            bare.to_str().unwrap(),
            developer.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "developer clone: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
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
    SvnClient::new(&svn_url, "", "")
        .export("", before, &original_export)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(original_export.join("feature.txt")).unwrap(),
        "first version\n"
    );
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
    assert_eq!(
        ancestry.code(),
        Some(1),
        "fixture must show valid negative ancestry, not a command error"
    );
    assert_eq!(
        get_head_sha(&git_dir),
        old_synced,
        "developer rewrite must not pre-reset the bridge"
    );
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
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"),
        "old fixture must now reject without replay: {result:?}"
    );
    assert_eq!(after, before, "SVN revision must be preserved");
    assert_eq!(get_head_sha(&git_dir), old_synced);
    assert_eq!(
        git_output(&git_dir, &["rev-parse", "HEAD^{tree}"]),
        bridge_tree_before
    );
    assert_eq!(git_output(&git_dir, &["write-tree"]), bridge_index_before);
    assert_eq!(git_output(&git_dir, &["status", "--porcelain"]), "");
    assert_eq!(
        git_output(&bare, &["rev-parse", "refs/heads/main"]),
        rewritten
    );
    let exported = tmp.path().join("rewritten_export");
    SvnClient::new(&svn_url, "", "")
        .export("", after, &exported)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(exported.join("feature.txt")).unwrap(),
        "first version\n"
    );
    let rewritten_record: i64 = engine
        .db()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_records WHERE git_sha = ?1 AND direction = 'git_to_svn'",
            rusqlite::params![rewritten],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rewritten_record, 0);
    assert_eq!(
        engine.db().get_state("last_git_hash").unwrap(),
        checkpoint_before
    );
    assert_eq!(
        engine.db().count_sync_records().unwrap(),
        mapping_count_before
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R09_ORIGINAL_REPLACEMENT", "p":old_synced, "r":rewritten,
            "bridge_tree_before_after":bridge_tree_before,
            "bridge_index_tree_before_after":bridge_index_before,
            "remote_tree_before_after":remote_tree_before,
            "svn_revision_before_after":before,
            "mapping_count_before_after":mapping_count_before,
            "reason":"non_fast_forward"
        })
    );
}

async fn run_candidate_r09_rewrite(metadata_only_amend: bool) {
    let fixture = QualifiedPair::new().await;
    let old_synced = fixture.developer_commit("feature.txt", "first version\n", "First Git change");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(
        fixture
            .engine
            .run_sync_cycle()
            .await
            .unwrap()
            .git_to_svn_count,
        1
    );
    let handled = fixture.engine.db().get_repo_watermark("pair").unwrap().1;
    assert_eq!(handled, old_synced);
    let old_revision = SvnClient::new(&fixture.svn_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    let mapped: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND svn_rev = ?1 AND git_sha = ?2",
        rusqlite::params![old_revision, old_synced], |row| row.get(0)).unwrap();
    assert_eq!(mapped, 1);

    if metadata_only_amend {
        git_cli(
            &fixture.developer,
            &["commit", "--amend", "-m", "Amended Git metadata"],
        );
    } else {
        git_cli(
            &fixture.developer,
            &["reset", "--hard", &fixture.imported_base],
        );
        fixture.developer_commit(
            "feature.txt",
            "rewritten version\n",
            "Changed-content replacement",
        );
    }
    git_cli(&fixture.developer, &["push", "--force", "origin", "main"]);
    let replacement = get_head_sha(&fixture.developer);
    assert_ne!(replacement, old_synced);
    let ancestry = Command::new("git")
        .arg("-C")
        .arg(&fixture.developer)
        .args(["merge-base", "--is-ancestor", &old_synced, &replacement])
        .status()
        .unwrap();
    assert_eq!(
        ancestry.code(),
        Some(1),
        "valid negative ancestry is required"
    );
    if metadata_only_amend {
        assert_eq!(
            git_output(&fixture.developer, &["rev-parse", "HEAD^{tree}"]),
            git_output(&fixture.bridge, &["rev-parse", "HEAD^{tree}"])
        );
    }
    let before = fixture.snapshot().await;
    assert_eq!(
        before.bridge_sha, old_synced,
        "developer rewrite must leave bridge intact"
    );
    assert_eq!(before.remote_sha, replacement);
    assert_eq!(before.svn_feature.as_deref(), Some("first version\n"));

    for attempt in 0..2 {
        let result = fixture.engine.run_sync_cycle().await;
        assert!(
            matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"),
            "attempt {attempt} must explicitly reject rewrite: {result:?}"
        );
        assert_eq!(
            fixture.snapshot().await,
            before,
            "rejection must preserve bridge/remotes/checkpoints/mappings"
        );
    }
    let reopened_db = Database::new(&fixture.db_path).unwrap();
    reopened_db.initialize().unwrap();
    let reopened_git = GitClient::new(&fixture.bridge).unwrap();
    let mut restarted = SyncEngine::new(
        fixture.engine.config().clone(),
        reopened_db,
        SvnClient::new(&fixture.svn_url, "", ""),
        reopened_git,
        Arc::new(make_identity_mapper()),
    );
    restarted.set_repo_id("pair".into());
    let result = restarted.run_sync_cycle().await;
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"),
        "reopened engine must reject unchanged unsafe state: {result:?}"
    );
    assert_eq!(fixture.snapshot().await, before);
    assert_eq!(
        fixture
            .engine
            .db()
            .get_state("sync_state")
            .unwrap()
            .as_deref(),
        Some("reconciliation_required")
    );
    let block: serde_json::Value = serde_json::from_str(
        &fixture
            .engine
            .db()
            .get_state("team_history_block_pair")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(block["reason"], "non_fast_forward");
    assert_eq!(block["p_handled"], old_synced);
    assert_eq!(block["r_fresh_remote"], replacement);
    assert_eq!(block["l_bridge"], old_synced);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
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
        })
    );
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
    let first =
        fixture.developer_commit("feature.txt", "version one\n", "First pending Git commit");
    let second =
        fixture.developer_commit("feature.txt", "version two\n", "Second pending Git commit");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before = fixture.snapshot().await;
    assert_eq!(before.svn_rev, 2);
    assert_eq!(before.watermark.1, fixture.imported_base);
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        stats.git_to_svn_count, 2,
        "both pending commits must replay"
    );
    let svn = SvnClient::new(&fixture.svn_url, "", "");
    let after_rev = svn.info().await.unwrap().latest_rev;
    assert_eq!(after_rev, before.svn_rev + 2);
    for (revision, content, sha) in [
        (before.svn_rev + 1, "version one\n", &first),
        (after_rev, "version two\n", &second),
    ] {
        let exported = fixture.tmp.path().join(format!("accepted-{revision}"));
        svn.export("", revision, &exported).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(exported.join("feature.txt")).unwrap(),
            content
        );
        assert_eq!(
            std::fs::read_to_string(exported.join("origin.txt")).unwrap(),
            "SVN origin\n"
        );
        let mapped: i64 = fixture.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND svn_rev = ?1 AND git_sha = ?2",
            rusqlite::params![revision, sha], |row| row.get(0)).unwrap();
        assert_eq!(
            mapped, 1,
            "each intermediate revision needs its own mapping"
        );
    }
    let after = fixture.snapshot().await;
    assert_eq!(after.watermark.1, second);
    assert_eq!(after.bridge_sha, second);
    assert_eq!(after.remote_sha, second);
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.git_to_svn_count, repeat.svn_to_git_count), (0, 0));
    assert_eq!(
        fixture.snapshot().await,
        after,
        "unchanged run must not emit revisions or mappings"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "R01_LINEAR", "p_before": before.watermark.1,
            "r": second, "first_git": first, "svn_before": before.svn_rev,
            "svn_after": after_rev, "first_svn": before.svn_rev + 1,
            "mapping_before": before.mapping_count, "mapping_after": after.mapping_count,
            "bridge_before": before.bridge_sha, "bridge_after": after.bridge_sha,
            "remote_tree_before": before.remote_tree, "remote_tree_after": after.remote_tree,
            "repeat_noop": true
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_alternating_directional_cursors_survive_restart() {
    let fixture = QualifiedPair::new().await;
    let git_sha = fixture.developer_commit("config", "from Git\n", "First direction");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap().1, git_sha);
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(git_sha.clone()));

    let svn_rev = svn_commit_file(&fixture.wc, "origin.txt", "Second direction\n", "Second direction");
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let emitted = get_head_sha(&fixture.bridge);
    assert_ne!(emitted, git_sha);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (svn_rev, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(git_sha.clone()));
    let mapped: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = ?1 AND git_sha = ?2 AND status = 'applied'",
        rusqlite::params![svn_rev, emitted], |row| row.get(0)).unwrap();
    assert_eq!(mapped, 1);

    let db = Database::new(&fixture.db_path).unwrap();
    db.initialize().unwrap();
    let mut restarted = SyncEngine::new(
        fixture.engine.config().clone(), db,
        SvnClient::new(&fixture.svn_url, "", ""),
        GitClient::new(&fixture.bridge).unwrap(), Arc::new(make_identity_mapper()));
    restarted.set_repo_id("pair".into());
    let repeat = restarted.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let further_git = fixture.developer_commit("config", "further Git\n", "Further Git direction");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(restarted.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    assert_eq!(restarted.db().get_repo_watermark("pair").unwrap().1, further_git);
    assert_eq!(restarted.db().get_state("last_git_sha_pair").unwrap(), Some(further_git.clone()));
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("config")).unwrap(), "further Git\n");
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("origin.txt")).unwrap(), "Second direction\n");
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_ALTERNATING_DUAL_CURSOR", "git_handled":git_sha,
        "svn_emitted":emitted, "svn_revision":svn_rev,
        "legacy_copy":fixture.engine.db().get_state("last_git_sha_pair").unwrap(),
        "restarted_noop":true, "further_git":further_git, "mapped":mapped
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_pending_both_directions_after_legacy_split() {
    let fixture = QualifiedPair::new().await;
    let handled = fixture.developer_commit("config", "first Git\n", "Handled Git direction");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    let emitted_rev = svn_commit_file(&fixture.wc, "origin.txt", "prior SVN\n", "Prior SVN direction");
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let emitted_sha = get_head_sha(&fixture.bridge);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap().1, emitted_sha);
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(handled.clone()));

    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let pending_git = fixture.developer_commit("config", "pending Git\n", "Pending Git direction");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let pending_svn = svn_commit_file(&fixture.wc, "origin.txt", "pending SVN\n", "Pending SVN direction");
    assert_eq!(pending_svn, emitted_rev + 1);
    let before_mapping = fixture.engine.db().count_sync_records().unwrap();
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((stats.svn_to_git_count, stats.git_to_svn_count), (1, 1));
    let after_mapping = fixture.engine.db().count_sync_records().unwrap();
    assert_eq!(after_mapping, before_mapping + 2);
    let git_mapped: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND git_sha = ?1 AND status = 'applied'",
        [&pending_git], |row| row.get(0)).unwrap();
    let svn_mapped: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = ?1 AND status = 'applied'",
        [pending_svn], |row| row.get(0)).unwrap();
    assert_eq!((git_mapped, svn_mapped), (1, 1));
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("config")).unwrap(), "pending Git\n");
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("origin.txt")).unwrap(), "pending SVN\n");
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_PENDING_BOTH_DIRECTIONS", "handled_git":handled,
        "prior_emitted":emitted_sha, "pending_git":pending_git,
        "pending_svn":pending_svn, "git_mapping":git_mapped,
        "svn_mapping":svn_mapped, "mapping_before":before_mapping,
        "mapping_after":after_mapping
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_legacy_stale_copy_requires_mapped_transition() {
    let fixture = QualifiedPair::new().await;
    // Reproduce the pinned legacy writers with actual engine operations:
    // Git->SVN stores the handled Git SHA in both copies, then SVN->Git
    // advances only the repository column to its emitted Git SHA.
    let handled = fixture.developer_commit("config", "legacy Git\n", "Legacy handled Git");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    let revision = svn_commit_file(&fixture.wc, "origin.txt", "legacy SVN\n", "Legacy emitted SVN");
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let emitted = get_head_sha(&fixture.bridge);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (revision, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(handled.clone()));
    let before = fixture.snapshot().await;
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.git_to_svn_count, repeat.svn_to_git_count), (0, 0));
    assert_eq!(fixture.snapshot().await, before);
    fixture.engine.db().conn().execute("DELETE FROM kv_state WHERE key = 'last_git_sha_pair'", []).unwrap();
    let missing_kv = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((missing_kv.git_to_svn_count, missing_kv.svn_to_git_count), (0, 0));
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (revision, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), None);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_LEGACY_STALE_COPY", "handled_git":handled,
        "emitted_git":emitted, "svn_revision":revision,
        "reconciled_without_new_mapping":true, "missing_kv_reconciled":true
    }));
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_old_import_cursor_survives_svn_only_poll_and_upgrade() {
    let tmp = TempDir::new().unwrap();
    assert_fixture_owned(tmp.path());
    let old_root = tmp.path().join("old-install-generation");
    let generator = std::env::var("REPOSYNC_OLD_GENERATOR").expect("pinned old generator must be packaged");
    let old_binary = std::fs::read(&generator).unwrap();
    let generation = Command::new(&generator)
        .args(["generate_legacy_import", "--exact", "--nocapture", "--test-threads=1"])
        .env("REPOSYNC_OLD_FIXTURE_DIR", &old_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Developer")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Developer")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output().unwrap();
    assert!(generation.status.success(), "pinned old import: {} {}", String::from_utf8_lossy(&generation.stdout), String::from_utf8_lossy(&generation.stderr));
    let old_generation = String::from_utf8_lossy(&generation.stderr).lines()
        .find_map(|line| line.strip_prefix("OLD_FIXTURE_EVIDENCE "))
        .map(|text| serde_json::from_str::<serde_json::Value>(text).unwrap())
        .expect("old generator did not report production import provenance");
    let original = old_root.join("install");
    let candidate = tmp.path().join("candidate-install");
    let restored = tmp.path().join("restored-prewrite-install");
    copy_install_tree(&original, &candidate);
    copy_install_tree(&original, &restored);
    let original_manifest = tree_hashes(&exported_tree(&original));
    assert_eq!(tree_hashes(&exported_tree(&restored)), original_manifest);
    assert_eq!(tree_hashes(&exported_tree(&candidate)), original_manifest);
    let original_db_hash = hex::encode(sha2::Sha256::digest(std::fs::read(original.join("reposync.db")).unwrap()));
    assert_eq!(original_db_hash, hex::encode(sha2::Sha256::digest(std::fs::read(restored.join("reposync.db")).unwrap())));
    let svn_url = format!("file://{}/trunk", old_root.join("svn_repo").display());
    let bare = old_root.join("old-origin.git");
    let bridge = candidate.join("repos/pair/git-repo");
    let old_tip = get_head_sha(&bridge);
    let old_tree = tracked_tree(&bridge);
    let old_export = tmp.path().join("old-import-export");
    SvnClient::new(&svn_url, "", "").export("", 2, &old_export).await.unwrap();
    assert_eq!(exported_tree(&old_export), old_tree);
    assert_eq!(git_output(&bare, &["rev-parse", "refs/heads/main"]), old_tip);
    let mut config = make_app_config(&svn_url, &candidate);
    config.svn.layout = reposync_core::config::SvnLayout::Custom;
    let db_path = candidate.join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    let old_schema: i64 = db.conn().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    db.initialize().unwrap();
    let new_schema: i64 = db.conn().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!((old_schema, new_schema), (12, 12));
    let old_repo = db.get_repository("pair").unwrap().unwrap();
    assert_eq!(old_repo.last_git_sha, old_tip);
    assert_eq!(db.get_state("last_git_sha_pair").unwrap(), Some(old_tip.clone()));
    assert_eq!(db.get_state("secret_svn_password_pair").unwrap().as_deref(), Some("fixture-only-svn-secret"));
    assert_eq!(db.get_state("secret_git_token_pair").unwrap().as_deref(), Some("fixture-only-git-secret"));
    assert_eq!(std::fs::read(candidate.join("config.toml")).unwrap(), std::fs::read(original.join("config.toml")).unwrap());
    let old_svn_rev = old_repo.last_svn_rev;
    let mut engine = SyncEngine::new(config.clone(), db, SvnClient::new(&svn_url, "", ""),
        GitClient::new(&bridge).unwrap(), Arc::new(make_identity_mapper()));
    engine.set_repo_id("pair".into());
    let idle = engine.run_sync_cycle().await.unwrap();
    assert_eq!((idle.svn_to_git_count, idle.git_to_svn_count), (0, 0));
    assert_eq!(get_head_sha(&bridge), old_tip);
    assert_eq!(tracked_tree(&bridge), old_tree);
    assert_eq!(git_output(&bare, &["rev-parse", "refs/heads/main"]), old_tip);
    assert_eq!(engine.db().get_repo_watermark("pair").unwrap(), (old_svn_rev, old_tip.clone()));
    let baseline_receipt: serde_json::Value = serde_json::from_str(
        &engine.db().get_state("handled_git_baseline_pair").unwrap().unwrap()).unwrap();
    assert_eq!(baseline_receipt["git_sha"], old_tip);
    assert_eq!(baseline_receipt["svn_rev"], old_svn_rev);
    let restored_bridge = restored.join("repos/pair/git-repo");
    assert_eq!(get_head_sha(&restored_bridge), old_tip);
    let restored_db = Database::new(&restored.join("reposync.db")).unwrap();
    assert_eq!(restored_db.get_repo_watermark("pair").unwrap(), (old_svn_rev, old_tip.clone()));
    assert_eq!(restored_db.get_state("secret_svn_password_pair").unwrap().as_deref(), Some("fixture-only-svn-secret"));
    let mut restore_config = make_app_config(&svn_url, &restored);
    restore_config.svn.layout = reposync_core::config::SvnLayout::Custom;
    let mut restore_engine = SyncEngine::new(restore_config, restored_db,
        SvnClient::new(&svn_url,"",""), GitClient::new(&restored_bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    restore_engine.set_repo_id("pair".into());
    let restore_idle = restore_engine.run_sync_cycle().await.unwrap();
    assert_eq!((restore_idle.svn_to_git_count, restore_idle.git_to_svn_count), (0, 0));
    assert_eq!(git_output(&bare, &["rev-parse", "refs/heads/main"]), old_tip);
    drop(restore_engine);

    let source_wc = old_root.join("source-wc");
    let svn_only_rev = svn_commit_file(&source_wc, "origin.txt", "post-upgrade SVN\n", "SVN-only after old import");
    assert_eq!(svn_only_rev, old_svn_rev + 1);
    let incoming = engine.run_sync_cycle().await.unwrap();
    assert_eq!((incoming.svn_to_git_count, incoming.git_to_svn_count), (1, 0));
    let emitted = get_head_sha(&bridge);
    assert_ne!(emitted, old_tip);
    assert_eq!(engine.db().get_repo_watermark("pair").unwrap(), (svn_only_rev, emitted.clone()));
    assert_eq!(engine.db().get_state("last_git_sha_pair").unwrap(), Some(old_tip.clone()));
    assert_eq!(std::fs::read_to_string(bridge.join("origin.txt")).unwrap(), "post-upgrade SVN\n");
    let wrong_db = Database::new(&db_path).unwrap();
    let mut wrong_projection = SyncEngine::new(config.clone(), wrong_db,
        SvnClient::new(&svn_url, "", ""), GitClient::new(&bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    wrong_projection.set_repo_id("pair".into());
    wrong_projection.set_path_rules(vec!["restricted/".into()], vec![]);
    assert!(matches!(wrong_projection.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "ambiguous_checkpoint"));
    assert_eq!(wrong_projection.db().get_repo_watermark("pair").unwrap(), (svn_only_rev, emitted.clone()));
    assert_eq!(git_output(&bare, &["rev-parse", "refs/heads/main"]), emitted);
    drop(wrong_projection);
    let after_incoming = engine.run_sync_cycle().await.unwrap();
    assert_eq!((after_incoming.svn_to_git_count, after_incoming.git_to_svn_count), (0, 0));
    drop(engine);
    let reopened_db = Database::new(&db_path).unwrap();
    let mut reopened = SyncEngine::new(config, reopened_db, SvnClient::new(&svn_url, "", ""),
        GitClient::new(&bridge).unwrap(), Arc::new(make_identity_mapper()));
    reopened.set_repo_id("pair".into());
    let restart_idle = reopened.run_sync_cycle().await.unwrap();
    assert_eq!((restart_idle.svn_to_git_count, restart_idle.git_to_svn_count), (0, 0));
    let developer = tmp.path().join("old-upgrade-developer");
    git_cli(tmp.path(), &["clone", "-b", "main", bare.to_str().unwrap(), developer.to_str().unwrap()]);
    std::fs::write(developer.join("post-upgrade-git.txt"), b"Git after upgrade\n").unwrap();
    git_cli(&developer, &["add", "post-upgrade-git.txt"]);
    git_cli(&developer, &["commit", "-m", "Git after upgrade"]);
    let outgoing_sha = get_head_sha(&developer);
    git_cli(&developer, &["push", "origin", "main"]);
    let outgoing = reopened.run_sync_cycle().await.unwrap();
    assert_eq!((outgoing.svn_to_git_count, outgoing.git_to_svn_count), (0, 1));
    let after = reopened.run_sync_cycle().await.unwrap();
    assert_eq!((after.svn_to_git_count, after.git_to_svn_count), (0, 0));
    let outgoing_map: i64 = reopened.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND git_sha = ?1 AND status = 'applied'",
        [&outgoing_sha], |row| row.get(0)).unwrap();
    assert_eq!(outgoing_map, 1);
    assert_eq!(reopened.db().get_state("last_git_sha_pair").unwrap(), Some(outgoing_sha.clone()));
    assert_eq!(reopened.db().get_repo_watermark("pair").unwrap().1, outgoing_sha);
    let final_rev = SvnClient::new(&svn_url,"","").info().await.unwrap().latest_rev;
    let final_export = tmp.path().join("final-upgrade-export");
    SvnClient::new(&svn_url,"","").export("", final_rev, &final_export).await.unwrap();
    assert_eq!(exported_tree(&final_export), tracked_tree(&bridge));
    let mapped_rows: Vec<(i64, String, String, String)> = {
        let conn = reopened.db().conn();
        let mut statement = conn.prepare(
            "SELECT COALESCE(svn_rev, 0), COALESCE(git_sha, ''), direction, status FROM sync_records WHERE repo_id = 'pair' ORDER BY rowid").unwrap();
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap().map(|row| row.unwrap()).collect()
    };
    let final_install_manifest = tree_hashes(&exported_tree(&candidate));
    let svn_uuid_output = Command::new("svnlook")
        .args(["uuid", old_root.join("svn_repo").to_str().unwrap()]).output().unwrap();
    assert!(svn_uuid_output.status.success());
    let svn_uuid = String::from_utf8(svn_uuid_output.stdout).unwrap().trim().to_string();
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_OLD_INSTALL_UPGRADE", "old_code":"87379741779a6259f7eeb52a68cc6f061174e5ef",
        "old_generator_sha256":hex::encode(sha2::Sha256::digest(old_binary)),
        "old_db_sha256":original_db_hash, "old_generation":old_generation,
        "old_svn_rev":old_svn_rev, "old_git":old_tip, "svn_uuid":svn_uuid,
        "schema_before":old_schema, "schema_after":new_schema,
        "verified_baseline_receipt":baseline_receipt,
        "old_tree":tree_hashes(&old_tree), "old_install_file_count":original_manifest.len(),
        "old_install_manifest":original_manifest,
        "prewrite_restore_verified":true, "restore_noop":true,
        "configuration_and_synthetic_credentials_preserved":true,
        "changed_projection_rejected_without_remote_write":true,
        "svn_only_rev":svn_only_rev, "svn_emitted":emitted,
        "restart_noop":true, "git_outgoing":outgoing_sha, "outgoing_mapping":outgoing_map,
        "final_svn_rev":final_rev, "final_svn_tree":tree_hashes(&exported_tree(&final_export)),
        "final_git_tree":tree_hashes(&tracked_tree(&bridge)),
        "post_upgrade_install_manifest":final_install_manifest,
        "repository_mapping_rows":mapped_rows,
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_empty_git_no_target_cursor_survives_svn_publication() {
    let fixture = QualifiedPair::new().await;
    let old_svn_rev = fixture.engine.db().get_repo_watermark("pair").unwrap().0;
    let old_tree = tracked_tree(&fixture.bridge);
    git_cli(&fixture.developer, &["commit", "--allow-empty", "-m", "Intentional empty Git change"]);
    let empty_sha = get_head_sha(&fixture.developer);
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let no_target = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((no_target.svn_to_git_count, no_target.git_to_svn_count), (0, 0));
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (old_svn_rev, empty_sha.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(empty_sha.clone()));
    let receipt_key = format!("handled_git_no_target_pair_{empty_sha}");
    let receipt: serde_json::Value = serde_json::from_str(&fixture.engine.db().get_state(&receipt_key).unwrap().unwrap()).unwrap();
    assert_eq!(receipt["outcome"], "empty_commit");
    assert_eq!(receipt["repo_id"], "pair");
    assert_eq!(receipt["git_sha"], empty_sha);
    assert_eq!(tracked_tree(&fixture.bridge), old_tree);
    assert_eq!(SvnClient::new(&fixture.svn_url,"","").info().await.unwrap().latest_rev, old_svn_rev);
    let svn_rev = svn_commit_file(&fixture.wc, "origin.txt", "SVN after empty Git\n", "Incoming after no-target");
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let emitted = get_head_sha(&fixture.bridge);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (svn_rev, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(empty_sha.clone()));
    let before_remote = git_output(&fixture.bare, &["rev-parse", "refs/heads/main"]);
    let before_tree = tracked_tree(&fixture.bridge);
    let wrong_db = Database::new(&fixture.db_path).unwrap();
    let mut wrong_policy = SyncEngine::new(fixture.engine.config().clone(), wrong_db,
        SvnClient::new(&fixture.svn_url,"",""), GitClient::new(&fixture.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    wrong_policy.set_repo_id("pair".into());
    wrong_policy.set_path_rules(vec!["restricted/".into()], vec![]);
    assert!(matches!(wrong_policy.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "ambiguous_checkpoint"));
    assert_eq!(git_output(&fixture.bare, &["rev-parse", "refs/heads/main"]), before_remote);
    assert_eq!(tracked_tree(&fixture.bridge), before_tree);
    assert_eq!(wrong_policy.db().get_repo_watermark("pair").unwrap(), (svn_rev, emitted.clone()));
    drop(wrong_policy);
    let db = Database::new(&fixture.db_path).unwrap();
    let mut restarted = SyncEngine::new(fixture.engine.config().clone(), db,
        SvnClient::new(&fixture.svn_url,"",""), GitClient::new(&fixture.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    restarted.set_repo_id("pair".into());
    let idle = restarted.run_sync_cycle().await.unwrap();
    assert_eq!((idle.svn_to_git_count, idle.git_to_svn_count), (0, 0));
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let actual = fixture.developer_commit("after-empty.txt", "real Git content\n", "Real Git after no-target");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(restarted.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    assert_eq!(restarted.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    let applied: i64 = restarted.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND git_sha = ?1 AND status = 'applied'",
        [&actual], |row| row.get(0)).unwrap();
    assert_eq!(applied, 1);
    assert_eq!(restarted.db().get_repo_watermark("pair").unwrap().1, actual);
    let final_revision = SvnClient::new(&fixture.svn_url,"","").info().await.unwrap().latest_rev;
    assert_eq!(svn_tree(&fixture, final_revision).await, tracked_tree(&fixture.bridge));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_NO_TARGET_GIT_CURSOR", "empty_git":empty_sha,
        "empty_outcome":receipt, "svn_revision":svn_rev, "svn_emitted":emitted,
        "changed_policy_rejected_without_remote_write":true,
        "remote_svn_unchanged_after_empty":old_svn_rev, "restart_noop":true,
        "actual_git":actual, "actual_applied_once":applied,
        "final_svn_tree":tree_hashes(&svn_tree(&fixture, final_revision).await),
        "final_git_tree":tree_hashes(&tracked_tree(&fixture.bridge))
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_retention_preserves_missing_kv_pending_frontier() {
    let fixture = QualifiedPair::new().await;
    let baseline = fixture.imported_base.clone();
    let pending = fixture.developer_commit("pending.txt", "pending Git survives\n", "Pending before SVN apply failure");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let verified = svn_commit_file(&fixture.wc, "origin.txt", "retention B\n", "Verified B");
    let failed = svn_commit_file(&fixture.wc, "origin.txt", "retention C\n", "Failed C");
    let fault = TestApplyFault::new(failed, &fixture.bridge).await;
    assert!(fixture.engine.run_sync_cycle().await.is_err());
    drop(fault);
    let emitted = get_head_sha(&fixture.bridge);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (verified, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), None);
    let conn = fixture.engine.db().conn();
    let old_time = "2000-01-01 00:00:00";
    assert_eq!(conn.execute(
        "UPDATE sync_records SET synced_at = ?1 WHERE repo_id = 'pair' AND git_sha = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
        rusqlite::params![old_time, baseline],).unwrap(), 1);
    conn.execute("INSERT INTO sync_records (id, repo_id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status) VALUES ('old-diagnostic', 'pair', NULL, NULL, 'svn_to_git', '', '', ?1, ?1, 'pending')", [old_time]).unwrap();
    drop(conn);
    fixture.engine.db().run_maintenance(90).unwrap();
    let retained_baseline: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'svn_to_git' AND status = 'applied'",
        [&baseline], |row| row.get(0)).unwrap();
    let pruned_diagnostic: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE id = 'old-diagnostic'", [], |row| row.get(0)).unwrap();
    assert_eq!((retained_baseline, pruned_diagnostic), (1, 0));
    let db = Database::new(&fixture.db_path).unwrap();
    let mut restarted = SyncEngine::new(fixture.engine.config().clone(), db,
        SvnClient::new(&fixture.svn_url,"",""), GitClient::new(&fixture.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    restarted.set_repo_id("pair".into());
    let retry = restarted.run_sync_cycle().await.unwrap();
    assert_eq!((retry.svn_to_git_count, retry.git_to_svn_count), (1, 1));
    let pending_map: i64 = restarted.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
        [&pending], |row| row.get(0)).unwrap();
    assert_eq!(pending_map, 1);
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("pending.txt")).unwrap(), "pending Git survives\n");
    let final_revision = SvnClient::new(&fixture.svn_url,"","").info().await.unwrap().latest_rev;
    assert_eq!(svn_tree(&fixture, final_revision).await, tracked_tree(&fixture.bridge));
    let repeat = restarted.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_RETENTION_FRONTIER", "baseline":baseline, "pending_git":pending,
        "verified_svn_revision":verified, "failed_svn_revision":failed,
        "emitted_before_retention":emitted, "baseline_applied_row_retained":retained_baseline,
        "old_diagnostic_pruned":pruned_diagnostic == 0, "pending_git_applied_once":pending_map,
        "restarted_after_maintenance":true, "repeat_noop":true,
        "final_git_tree":tree_hashes(&tracked_tree(&fixture.bridge)),
        "final_svn_tree":tree_hashes(&svn_tree(&fixture, final_revision).await)
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_prior_pruned_baseline_blocks_without_guessing() {
    let fixture = QualifiedPair::new().await;
    let baseline = fixture.imported_base.clone();
    let pending = fixture.developer_commit("pending.txt", "must remain pending\n", "Pending Git before degraded retention");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let verified = svn_commit_file(&fixture.wc, "origin.txt", "verified B\n", "Verified B");
    let failed = svn_commit_file(&fixture.wc, "origin.txt", "failed C\n", "Failed C");
    let fault = TestApplyFault::new(failed, &fixture.bridge).await;
    assert!(fixture.engine.run_sync_cycle().await.is_err());
    drop(fault);
    let emitted = get_head_sha(&fixture.bridge);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (verified, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), None);
    // Model an installation already pruned by the original maintenance code.
    // Candidate retention cannot recreate a lost authority from the next row.
    let conn = fixture.engine.db().conn();
    conn.execute("DELETE FROM kv_state WHERE key = 'handled_git_baseline_pair'", []).unwrap();
    conn.execute("DELETE FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'svn_to_git'", [&baseline]).unwrap();
    drop(conn);
    let before_remote = git_output(&fixture.bare, &["rev-parse", "refs/heads/main"]);
    let before_tree = tracked_tree(&fixture.bridge);
    let before_svn = SvnClient::new(&fixture.svn_url,"","").info().await.unwrap().latest_rev;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(result, Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "ambiguous_checkpoint"));
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (verified, emitted.clone()));
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), None);
    assert_eq!(git_output(&fixture.bare, &["rev-parse", "refs/heads/main"]), before_remote);
    assert_eq!(tracked_tree(&fixture.bridge), before_tree);
    assert_eq!(SvnClient::new(&fixture.svn_url,"","").info().await.unwrap().latest_rev, before_svn);
    let applied: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
        [&pending], |row| row.get(0)).unwrap();
    assert_eq!(applied, 0);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_PRIOR_PRUNED_BASELINE", "baseline":baseline,
        "pending_git":pending, "emitted_git":emitted, "failed_revision":failed,
        "safe_block":"ambiguous_checkpoint", "outgoing_mapping":applied,
        "remote_git_unchanged":before_remote, "svn_revision_unchanged":before_svn,
        "git_tree_preserved":tree_hashes(&before_tree)
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_retention_preserves_present_kv_applied_mapping() {
    let fixture = QualifiedPair::new().await;
    let handled = fixture.developer_commit("handled.txt", "handled Git\n", "Handled before retention");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    let revision = svn_commit_file(&fixture.wc, "origin.txt", "SVN after handled\n", "Incoming before retention");
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let emitted = get_head_sha(&fixture.bridge);
    assert_eq!(fixture.engine.db().get_state("last_git_sha_pair").unwrap(), Some(handled.clone()));
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), (revision, emitted.clone()));
    let conn = fixture.engine.db().conn();
    assert_eq!(conn.execute(
        "UPDATE sync_records SET synced_at = '2000-01-01 00:00:00' WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
        [&handled]).unwrap(), 1);
    drop(conn);
    fixture.engine.db().run_maintenance(90).unwrap();
    let applied: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
        [&handled], |row| row.get(0)).unwrap();
    assert_eq!(applied, 1);
    let db = Database::new(&fixture.db_path).unwrap();
    let mut restarted = SyncEngine::new(fixture.engine.config().clone(), db,
        SvnClient::new(&fixture.svn_url,"",""), GitClient::new(&fixture.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    restarted.set_repo_id("pair".into());
    let idle = restarted.run_sync_cycle().await.unwrap();
    assert_eq!((idle.svn_to_git_count, idle.git_to_svn_count), (0, 0));
    assert_eq!(restarted.db().get_repo_watermark("pair").unwrap(), (revision, emitted.clone()));
    let final_revision = SvnClient::new(&fixture.svn_url,"","").info().await.unwrap().latest_rev;
    assert_eq!(svn_tree(&fixture, final_revision).await, tracked_tree(&fixture.bridge));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_RETENTION_PRESENT_KV", "handled_git":handled,
        "emitted_git":emitted, "svn_revision":revision,
        "old_applied_mapping_retained":applied, "restart_noop":true,
        "git_tree":tree_hashes(&tracked_tree(&fixture.bridge)),
        "svn_tree":tree_hashes(&svn_tree(&fixture, final_revision).await)
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_unmapped_stale_copy_rejected() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    let false_cursor = "a".repeat(40);
    fixture.engine.db().set_state("last_git_sha_pair", &false_cursor).unwrap();
    assert_pair_blocked_without_damage(&fixture, "ambiguous_checkpoint").await;
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), before.watermark);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_UNMAPPED_STALE_COPY", "column":before.watermark.1,
        "unmapped_copy":false_cursor, "blocked":true
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_ignored_file_collision_preserved() {
    let fixture = QualifiedPair::new().await;
    std::fs::write(fixture.bridge.join(".git/info/exclude"), "cache.dat\n").unwrap();
    std::fs::write(fixture.bridge.join("cache.dat"), "private cache\n").unwrap();
    fixture.developer_commit("cache.dat", "remote content\n", "Tracked collision");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before = fixture.snapshot().await;
    assert_pair_blocked_without_damage(&fixture, "ignored_path_collision").await;
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("cache.dat")).unwrap(), "private cache\n");
    assert_eq!(fixture.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R09_IGNORED_FILE_COLLISION", "protected_tree":before.bridge_tree,
        "protected_index_sha256":hex::encode(sha2::Sha256::digest(&before.bridge_index)),
        "protected_cache":true, "checkpoint":before.watermark
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_ignored_directory_collision_preserved() {
    let fixture = QualifiedPair::new().await;
    std::fs::write(fixture.bridge.join(".git/info/exclude"), "cache/\n").unwrap();
    std::fs::create_dir(fixture.bridge.join("cache")).unwrap();
    std::fs::write(fixture.bridge.join("cache/private.dat"), "private cache\n").unwrap();
    fixture.developer_commit("cache", "remote file\n", "Directory collision");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before = fixture.snapshot().await;
    assert_pair_blocked_without_damage(&fixture, "ignored_path_collision").await;
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("cache/private.dat")).unwrap(), "private cache\n");
    assert_eq!(fixture.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R09_IGNORED_DIRECTORY_COLLISION", "protected_tree":before.bridge_tree,
        "protected_index_sha256":hex::encode(sha2::Sha256::digest(&before.bridge_index)),
        "protected_cache":true, "checkpoint":before.watermark
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_rejection_preserves_raw_index_before_status() {
    let fixture = QualifiedPair::new().await;
    let initial = fixture.developer_commit("feature.txt", "version one\n", "Handled version");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap().1, initial);
    git_cli(&fixture.developer, &["commit", "--amend", "-m", "Rewritten metadata"]);
    git_cli(&fixture.developer, &["push", "--force", "origin", "main"]);
    // Change only a tracked file's metadata, then capture raw bytes before
    // any status/snapshot helper can refresh the index stat cache.
    let tracked = fixture.bridge.join("origin.txt");
    let touch = Command::new("touch").args(["-m", "-t", "202001010000"]).arg(&tracked).status().unwrap();
    assert!(touch.success());
    let index_before = std::fs::read(fixture.bridge.join(".git/index")).unwrap();
    let bridge_before = get_head_sha(&fixture.bridge);
    let cursor_before = fixture.engine.db().get_repo_watermark("pair").unwrap();
    let mapping_before = fixture.engine.db().count_sync_records().unwrap();
    let svn_before = SvnClient::new(&fixture.svn_url, "", "").info().await.unwrap().latest_rev;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "non_fast_forward"));
    assert_eq!(std::fs::read(fixture.bridge.join(".git/index")).unwrap(), index_before);
    assert_eq!(get_head_sha(&fixture.bridge), bridge_before);
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap(), cursor_before);
    assert_eq!(fixture.engine.db().count_sync_records().unwrap(), mapping_before);
    assert_eq!(SvnClient::new(&fixture.svn_url, "", "").info().await.unwrap().latest_rev, svn_before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R09_READONLY_INDEX", "index_sha256":hex::encode(sha2::Sha256::digest(&index_before)),
        "bridge":bridge_before, "checkpoint":cursor_before, "svn_revision":svn_before,
        "mapping_count":mapping_before
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_ignored_noncollision_allows_qualified_sync() {
    let fixture = QualifiedPair::new().await;
    std::fs::write(fixture.bridge.join(".git/info/exclude"), "cache.dat\n").unwrap();
    std::fs::write(fixture.bridge.join("cache.dat"), "private cache\n").unwrap();
    let pending = fixture.developer_commit("feature.txt", "ordinary change\n", "Ordinary sync");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before = fixture.snapshot().await;
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 1);
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("cache.dat")).unwrap(), "private cache\n");
    assert_eq!(fixture.engine.db().get_repo_watermark("pair").unwrap().1, pending);
    let after = fixture.snapshot().await;
    assert_eq!(after.svn_rev, before.svn_rev + 1);
    assert_eq!(after.svn_feature.as_deref(), Some("ordinary change\n"));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_IGNORED_NONCOLLISION", "p":before.watermark.1,
        "r":pending, "svn_revision":after.svn_rev,
        "cache_preserved":true, "bridge_tree":after.bridge_tree
    }));
}

struct TestApplyFault {
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl TestApplyFault {
    async fn new(revision: i64, bridge: &Path) -> Self {
        static FAULT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        let guard = FAULT_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
        std::env::set_var("REPOSYNC_TEST_SVN_APPLY_FAULT", format!("{}|{}", revision, bridge.display()));
        Self { _guard: guard }
    }
}

impl Drop for TestApplyFault {
    fn drop(&mut self) {
        std::env::remove_var("REPOSYNC_TEST_SVN_APPLY_FAULT");
    }
}

async fn run_failed_apply_barrier(retry: bool) {
    let fixture = QualifiedPair::new().await;
    let pending_git = fixture.developer_commit("config", "pending outgoing\n", "Pending outgoing Git");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before = fixture.snapshot().await;
    let verified = svn_commit_file(&fixture.wc, "origin.txt", "verified N-1\n", "Verified prior revision");
    let failed = svn_commit_file(&fixture.wc, "origin.txt", "failed N\n", "Faulted revision");
    let later = svn_commit_file(&fixture.wc, "origin.txt", "queued N+1\n", "Later revision");
    assert_eq!((verified, failed, later), (before.svn_rev + 1, before.svn_rev + 2, before.svn_rev + 3));
    let fault = TestApplyFault::new(failed, &fixture.bridge).await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(matches!(&result, Err(SyncError::GitError(reposync_core::errors::GitError::ApplyFailed(message)))
        if message.contains(&format!("r{failed}"))), "failed N must stop cycle: {result:?}");
    drop(fault);
    let frontier = fixture.snapshot().await;
    assert_eq!(frontier.watermark.0, verified);
    assert_eq!(frontier.svn_rev, later);
    assert_eq!(frontier.svn_origin, "queued N+1\n");
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("origin.txt")).unwrap(), "verified N-1\n");
    assert_eq!(frontier.remote_sha, frontier.bridge_sha);
    assert_eq!(frontier.remote_tree, frontier.bridge_tree);
    assert!(frontier.bridge_status.is_empty(), "failed git apply left bridge dirty");
    assert_eq!(frontier.mapping_count, before.mapping_count + 1);
    assert_eq!(frontier.repo_sync_count, before.repo_sync_count + 1);
    for revision in [failed, later] {
        let count: i64 = fixture.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND svn_rev = ?1 AND direction = 'svn_to_git' AND status = 'applied'",
            [revision], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "unapplied r{revision} must have no successful mapping");
    }
    let outgoing_at_barrier: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
        [&pending_git], |row| row.get(0)).unwrap();
    assert_eq!(outgoing_at_barrier, 0, "outgoing Git must wait behind failed incoming SVN");
    let verified_sha = frontier.bridge_sha.clone();
    assert_eq!(git_output(&fixture.bridge, &["show", &format!("{verified_sha}:origin.txt")]), "verified N-1");
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_FAILED_APPLY_BARRIER", "svn_source_head":later,
        "frontier_revision":verified, "failed_revision":failed,
        "queued_revision":later, "frontier_git":verified_sha,
        "frontier_tree":frontier.bridge_tree, "remote_tree":frontier.remote_tree,
        "frontier_index_sha256":hex::encode(sha2::Sha256::digest(&frontier.bridge_index)),
        "frontier_status":frontier.bridge_status,
        "mapping_before":before.mapping_count, "mapping_at_frontier":frontier.mapping_count,
        "success_before":before.repo_sync_count, "success_at_frontier":frontier.repo_sync_count,
        "pending_git":pending_git, "outgoing_mapping_at_barrier":outgoing_at_barrier,
        "fault":"debug-only exact-revision invalid patch to real git apply"
    }));
    if !retry { return; }

    let applied = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((applied.svn_to_git_count, applied.git_to_svn_count), (2, 1));
    let after = fixture.snapshot().await;
    assert_eq!(after.watermark.0, later);
    assert_eq!(after.mapping_count, frontier.mapping_count + 3);
    assert_eq!(after.repo_sync_count, frontier.repo_sync_count + 3);
    assert_eq!(after.svn_rev, later + 1);
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("config")).unwrap(), "pending outgoing\n");
    let outgoing_mapped: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
        [&pending_git], |row| row.get(0)).unwrap();
    assert_eq!(outgoing_mapped, 1);
    assert_eq!(after.remote_sha, after.bridge_sha);
    assert_eq!(after.remote_tree, after.bridge_tree);
    assert_eq!(std::fs::read_to_string(fixture.bridge.join("origin.txt")).unwrap(), "queued N+1\n");
    for (revision, expected) in [(failed, "failed N"), (later, "queued N+1")] {
        let mapped: String = fixture.engine.db().conn().query_row(
            "SELECT git_sha FROM sync_records WHERE repo_id = 'pair' AND svn_rev = ?1 AND direction = 'svn_to_git' AND status = 'applied'",
            [revision], |row| row.get(0)).unwrap();
        assert_eq!(git_output(&fixture.bridge, &["show", &format!("{mapped}:origin.txt")]), expected);
    }
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    assert_eq!(fixture.snapshot().await, after);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_FAILED_APPLY_RETRY", "frontier_git":verified_sha,
        "final_git":after.bridge_sha, "final_tree":after.bridge_tree,
        "watermark":after.watermark, "mapping_after":after.mapping_count,
        "success_after":after.repo_sync_count, "outgoing_mapping":outgoing_mapped,
        "repeat_noop":true
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_failed_apply_stops_before_later_revision() {
    run_failed_apply_barrier(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_failed_apply_retries_pending_revisions_once() {
    run_failed_apply_barrier(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_property_only_revision_has_explicit_no_target_checkpoint() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    let revision = svn_property_only_revision(&fixture.wc);
    assert_eq!(revision, before.svn_rev + 1);
    let content_only = SvnClient::new(&fixture.svn_url, "", "")
        .diff_content_only(revision).await.unwrap();
    assert!(content_only.trim().is_empty(), "fixture must have no target file delta: {content_only}");
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((stats.svn_to_git_count, stats.git_to_svn_count), (0, 0));
    let after = fixture.snapshot().await;
    assert_eq!(after.watermark.0, revision);
    assert_eq!(after.watermark.1, before.watermark.1);
    assert_eq!(after.bridge_tree, before.bridge_tree);
    assert_eq!(after.remote_tree, before.remote_tree);
    assert_eq!(after.mapping_count, before.mapping_count);
    assert_eq!(after.repo_sync_count, before.repo_sync_count);
    let recorded: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM audit_log WHERE repo_id = 'pair' AND action = 'svn_to_git_no_target' AND svn_rev = ?1 AND success = 1",
        [revision], |row| row.get(0)).unwrap();
    assert_eq!(recorded, 1);
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    assert_eq!(fixture.snapshot().await, after);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_METADATA_ONLY", "revision":revision,
        "bridge_tree_before_after":before.bridge_tree,
        "remote_tree_before_after":before.remote_tree,
        "mapping_before_after":before.mapping_count,
        "recorded_no_target_checkpoint":recorded,
        "repeat_noop":true
    }));
}

async fn prepare_r19_tree(fixture: &QualifiedPair) -> (i64, BTreeMap<String, Vec<u8>>) {
    let loose = svn_commit_file(&fixture.wc, "loose.txt", "untouched top-level\n", "Add top-level survivor");
    let notes = svn_commit_file(&fixture.wc, "notes/keep.txt", "untouched directory\n", "Add directory survivor");
    assert_eq!(notes, loose + 1);
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 2);
    let expected = fixture_tree();
    assert_eq!(tracked_tree(&fixture.bridge), expected);
    assert_eq!(svn_tree(fixture, notes).await, expected);
    (notes, expected)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r19_untouched_top_level_tree_survives_deltas() {
    let fixture = QualifiedPair::new().await;
    let (before_rev, mut expected) = prepare_r19_tree(&fixture).await;
    let before_tree = git_output(&fixture.bridge, &["rev-parse", "HEAD^{tree}"]);
    let added = svn_commit_file(&fixture.wc, "added.txt", "new path\n", "Add path");
    let modified = svn_commit_file(&fixture.wc, "origin.txt", "modified origin\n", "Modify path");
    assert_eq!((added, modified), (before_rev + 1, before_rev + 2));
    let stats = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 2);
    expected.insert("added.txt".into(), b"new path\n".to_vec());
    let added_sha: String = fixture.engine.db().conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = 'pair' AND svn_rev = ?1 AND direction = 'svn_to_git' AND status = 'applied'",
        [added], |row| row.get(0)).unwrap();
    assert_eq!(tracked_tree_at(&fixture.bridge, &added_sha), expected);
    assert_eq!(svn_tree(&fixture, added).await, expected);
    expected.insert("origin.txt".into(), b"modified origin\n".to_vec());
    assert_eq!(tracked_tree(&fixture.bridge), expected);
    assert_eq!(svn_tree(&fixture, modified).await, expected);
    let after = fixture.snapshot().await;
    assert_eq!(after.watermark.0, modified);
    assert_eq!(after.remote_tree, after.bridge_tree);
    let after_tree = after.bridge_tree.clone();
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    assert_eq!(fixture.snapshot().await, after);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R19_UNTOUCHED_TREE", "before_revision":before_rev,
        "added_revision":added, "modified_revision":modified,
        "before_tree":before_tree, "added_commit":added_sha,
        "before_full_tree_content_sha256":tree_hashes(&fixture_tree()),
        "after_tree":after_tree, "expected_paths":expected.keys().collect::<Vec<_>>(),
        "full_tree_content_sha256":tree_hashes(&expected),
        "expected_sha256":hex::encode(sha2::Sha256::digest(serde_json::to_vec(&expected).unwrap())),
        "repeat_noop":true
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r19_explicit_svn_delete_only_removes_target() {
    let fixture = QualifiedPair::new().await;
    let (_, mut expected) = prepare_r19_tree(&fixture).await;
    let added = svn_commit_file(&fixture.wc, "remove-me.txt", "delete target\n", "Add delete target");
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    expected.insert("remove-me.txt".into(), b"delete target\n".to_vec());
    assert_eq!(tracked_tree(&fixture.bridge), expected);
    let before_head = get_head_sha(&fixture.bridge);
    let deleted = svn_delete_file(&fixture.wc, "remove-me.txt", "Explicit deletion");
    assert_eq!(deleted, added + 1);
    assert_eq!(fixture.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    expected.remove("remove-me.txt");
    assert_eq!(tracked_tree(&fixture.bridge), expected);
    assert_eq!(svn_tree(&fixture, deleted).await, expected);
    let diff = git_output(&fixture.bridge, &["diff", "--name-status", &before_head, "HEAD"]);
    assert_eq!(diff, "D\tremove-me.txt");
    let after = fixture.snapshot().await;
    assert_eq!(after.watermark.0, deleted);
    assert_eq!(after.remote_tree, after.bridge_tree);
    let mapping: i64 = fixture.engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND svn_rev = ?1 AND direction = 'svn_to_git' AND status = 'applied'",
        [deleted], |row| row.get(0)).unwrap();
    assert_eq!(mapping, 1);
    let repeat = fixture.engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    assert_eq!(fixture.snapshot().await, after);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R19_EXPLICIT_DELETE", "added_revision":added,
        "deleted_revision":deleted, "before_git":before_head,
        "after_git":after.bridge_sha, "after_tree":after.bridge_tree,
        "git_change":diff, "expected_paths":expected.keys().collect::<Vec<_>>(),
        "full_tree_content_sha256":tree_hashes(&expected),
        "expected_sha256":hex::encode(sha2::Sha256::digest(serde_json::to_vec(&expected).unwrap())),
        "mapping":mapping, "repeat_noop":true
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
    assert_eq!(
        stats.git_to_svn_count, 2,
        "L=R must not hide commits after P"
    );
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
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "R10_L_EQUALS_R", "p": before.watermark.1,
            "l": before.bridge_sha, "r": before.remote_sha,
            "svn_before": before.svn_rev, "svn_after": after.svn_rev,
            "mapping_before": before.mapping_count, "mapping_after": after.mapping_count
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_unchanged_git_still_admits_new_svn_work() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    assert_eq!(before.bridge_sha, before.remote_sha);
    assert_eq!(before.watermark.1, before.remote_sha);
    let incoming = svn_commit_file(
        &fixture.wc,
        "incoming.txt",
        "from SVN\n",
        "New incoming SVN work",
    );
    assert_eq!(incoming, before.svn_rev + 1);
    let result = fixture.engine.run_sync_cycle().await;
    assert!(
        !matches!(&result, Err(SyncError::HistoryBlocked { .. })),
        "unchanged Git must not block the pending SVN direction: {result:?}"
    );
    assert_eq!(
        SvnClient::new(&fixture.svn_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        incoming
    );
    match result {
        Ok(stats) => {
            assert_eq!(stats.svn_to_git_count, 1);
            assert_eq!(
                std::fs::read_to_string(fixture.bridge.join("incoming.txt")).unwrap(),
                "from SVN\n"
            );
            let mapped: i64 = fixture.engine.db().conn().query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = ?1",
                [incoming], |row| row.get(0)).unwrap();
            assert_eq!(mapped, 1);
            eprintln!(
                "RELIABILITY_EVIDENCE {}",
                serde_json::json!({
                    "case":"R01_UNCHANGED_GIT_NEW_SVN", "outcome":"PASS",
                    "p":before.watermark.1, "r":before.remote_sha,
                    "svn_before":before.svn_rev, "svn_after":incoming, "mapping_added":mapped
                })
            );
        }
        Err(error) => {
            let mapped: i64 = fixture.engine.db().conn().query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND svn_rev = ?1",
                [incoming], |row| row.get(0)).unwrap();
            assert_eq!(mapped, 0);
            eprintln!(
                "RELIABILITY_EVIDENCE {}",
                serde_json::json!({
                    "case":"R01_UNCHANGED_GIT_NEW_SVN", "outcome":"PARTIAL_EXISTING_APPLY_FAILURE",
                    "p":before.watermark.1, "r":before.remote_sha,
                    "svn_before":before.svn_rev, "svn_after":incoming,
                    "error":error.to_string(), "mapping_added":mapped
                })
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_missing_remote_branch_blocks_stale_tracking_ref() {
    let fixture = QualifiedPair::new().await;
    git_cli(&fixture.bridge, &["fetch", "origin", "main"]);
    assert_eq!(
        git_output(&fixture.bridge, &["rev-parse", "refs/remotes/origin/main"]),
        fixture.imported_base
    );
    git_cli(&fixture.developer, &["push", "origin", "--delete", "main"]);
    let bridge_before = get_head_sha(&fixture.bridge);
    let index_before = std::fs::read(fixture.bridge.join(".git/index")).unwrap();
    let svn_before = SvnClient::new(&fixture.svn_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    let cursor_before = fixture.engine.db().get_repo_watermark("pair").unwrap();
    let mappings_before = fixture.engine.db().count_sync_records().unwrap();
    let result = fixture.engine.run_sync_cycle().await;
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_branch_missing"),
        "missing branch must not use stale tracking ref: {result:?}"
    );
    assert_eq!(get_head_sha(&fixture.bridge), bridge_before);
    assert_eq!(
        std::fs::read(fixture.bridge.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        SvnClient::new(&fixture.svn_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        svn_before
    );
    assert_eq!(
        fixture.engine.db().get_repo_watermark("pair").unwrap(),
        cursor_before
    );
    assert_eq!(
        fixture.engine.db().count_sync_records().unwrap(),
        mappings_before
    );
    let remote = Command::new("git")
        .arg("-C")
        .arg(&fixture.bare)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/main"])
        .status()
        .unwrap();
    assert_eq!(remote.code(), Some(1));
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R16_MISSING_BRANCH", "o":fixture.imported_base,
            "r":null, "bridge_before_after":bridge_before,
            "svn_revision_before_after":svn_before,
            "watermark_before_after":cursor_before,
            "mappings_before_after":mappings_before,
            "reason":"remote_branch_missing"
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_remote_transport_failure_blocks_without_reset() {
    let fixture = QualifiedPair::new().await;
    let missing = fixture.tmp.path().join("no-such-remote.git");
    git_cli(
        &fixture.bridge,
        &["remote", "set-url", "origin", missing.to_str().unwrap()],
    );
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
        let guard = FAULT_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        std::env::set_var(
            "REPOSYNC_TEST_INSPECTION_FAULT",
            format!("{}|{}", kind, bridge.display()),
        );
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
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_auth_failed"),
        "synthetic auth denial must be distinct from transport: {result:?}"
    );
    assert_eq!(fixture.snapshot().await, before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R16_AUTH", "reason":"remote_auth_failed",
            "bridge_before_after":before.bridge_sha, "svn_revision_before_after":before.svn_rev,
            "mapping_count_before_after":before.mapping_count
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r16_fetch_failure_does_not_use_stale_ref() {
    let fixture = QualifiedPair::new().await;
    git_cli(&fixture.bridge, &["fetch", "origin", "main"]);
    let before = fixture.snapshot().await;
    let _fault = TestInspectionFault::new("remote_fetch", &fixture.bridge).await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_fetch_failed"),
        "failed fresh fetch must not use stale tracking ref: {result:?}"
    );
    assert_eq!(fixture.snapshot().await, before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R16_FETCH", "reason":"remote_fetch_failed",
            "bridge_before_after":before.bridge_sha, "svn_revision_before_after":before.svn_rev,
            "mapping_count_before_after":before.mapping_count
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_missing_checkpoint_object_blocks() {
    let fixture = QualifiedPair::new().await;
    let absent = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let repo_rev = fixture.engine.db().get_repo_watermark("pair").unwrap().0;
    fixture
        .engine
        .db()
        .update_repo_watermark("pair", repo_rev, absent)
        .unwrap();
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint_object").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_missing_repository_cursor_does_not_borrow_global() {
    let fixture = QualifiedPair::new().await;
    fixture
        .engine
        .db()
        .set_state("last_git_hash", &fixture.imported_base)
        .unwrap();
    fixture
        .engine
        .db()
        .update_repo_watermark("pair", 2, "")
        .unwrap();
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_ambiguous_repository_cursor_blocks() {
    let fixture = QualifiedPair::new().await;
    let other = git_output(&fixture.bridge, &["rev-parse", "HEAD^"]);
    fixture
        .engine
        .db()
        .set_state("last_git_sha_pair", &other)
        .unwrap();
    assert_pair_blocked_without_damage(&fixture, "ambiguous_checkpoint").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_shallow_history_blocks() {
    let fixture = QualifiedPair::new().await;
    std::fs::write(
        fixture.bridge.join(".git/shallow"),
        format!("{}\n", fixture.imported_base),
    )
    .unwrap();
    assert_pair_blocked_without_damage(&fixture, "incomplete_history").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_ancestry_command_error_is_not_rewrite() {
    let fixture = QualifiedPair::new().await;
    let before = fixture.snapshot().await;
    let _fault = TestInspectionFault::new("ancestry_exit_128", &fixture.bridge).await;
    let result = fixture.engine.run_sync_cycle().await;
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "ancestry_command_failed"),
        "git command error must be unknown, not a valid negative ancestry: {result:?}"
    );
    assert_eq!(fixture.snapshot().await, before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R10_ANCESTRY_ERROR", "reason":"ancestry_command_failed",
            "bridge_before_after":before.bridge_sha, "svn_revision_before_after":before.svn_rev,
            "mapping_count_before_after":before.mapping_count
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_local_dirty_index_and_unpublished_commit_preserved() {
    for kind in ["dirty", "staged", "unpublished"] {
        let fixture = QualifiedPair::new().await;
        let file = fixture.bridge.join(format!("{kind}.txt"));
        std::fs::write(&file, format!("{kind}\n")).unwrap();
        if kind == "staged" || kind == "unpublished" {
            git_cli(
                &fixture.bridge,
                &["add", file.file_name().unwrap().to_str().unwrap()],
            );
        }
        if kind == "unpublished" {
            git_cli(
                &fixture.bridge,
                &["commit", "-m", "Unpublished bridge commit"],
            );
        }
        let reason = if kind == "unpublished" {
            "unpublished_local_history"
        } else {
            "local_dirty"
        };
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
    git_cli(
        &fixture.developer,
        &["merge", "--no-ff", "side", "-m", "Merge side"],
    );
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    assert_pair_blocked_without_damage(&fixture, "unsupported_merge_dag").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_over_1000_pending_commits_rejected() {
    let fixture = QualifiedPair::new().await;
    let tree = git_output(&fixture.developer, &["rev-parse", "HEAD^{tree}"]);
    let mut parent = fixture.imported_base.clone();
    for index in 0..1001 {
        let output = Command::new("git")
            .arg("-C")
            .arg(&fixture.developer)
            .args([
                "commit-tree",
                &tree,
                "-p",
                &parent,
                "-m",
                &format!("pending {index}"),
            ])
            .env("GIT_AUTHOR_NAME", "Fixture Developer")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture Developer")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "commit-tree: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        parent = String::from_utf8(output.stdout).unwrap().trim().to_string();
    }
    git_cli(
        &fixture.developer,
        &["update-ref", "refs/heads/main", &parent],
    );
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
    svn_commit_file(
        &other_wc,
        "origin.txt",
        "Other SVN origin\n",
        "Other SVN import",
    );
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
        other_config,
        other_db,
        SvnClient::new(&other_svn, "", ""),
        other_git,
        Arc::new(make_identity_mapper()),
    );
    other_engine.set_repo_id("other".into());
    assert_eq!(
        other_engine
            .run_sync_cycle()
            .await
            .unwrap()
            .svn_to_git_count,
        1
    );
    let other_import = get_head_sha(&other_bridge);
    assert_eq!(
        other_engine.db().get_repo_watermark("other").unwrap(),
        (2, other_import.clone())
    );

    // A global cursor for pair one must never become pair two's missing P.
    fixture
        .engine
        .db()
        .set_state("last_git_hash", &fixture.imported_base)
        .unwrap();
    other_engine
        .db()
        .update_repo_watermark("other", 2, "")
        .unwrap();
    let result = other_engine.run_sync_cycle().await;
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "missing_checkpoint"),
        "other pair borrowed a global cursor: {result:?}"
    );
    assert_eq!(
        SvnClient::new(&other_svn, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        2
    );
    other_engine
        .db()
        .update_repo_watermark("other", 2, &other_import)
        .unwrap();

    // Pair one is blocked, while the separately proven pair still syncs.
    let absent = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fixture
        .engine
        .db()
        .update_repo_watermark("pair", 2, absent)
        .unwrap();
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint_object").await;
    let other_developer = other_root.join("developer");
    let clone = Command::new("git")
        .args([
            "clone",
            "-b",
            "main",
            other_bare.to_str().unwrap(),
            other_developer.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success());
    std::fs::write(other_developer.join("feature.txt"), "one\n").unwrap();
    git_cli(&other_developer, &["add", "feature.txt"]);
    git_cli(&other_developer, &["commit", "-m", "Other first"]);
    let first = get_head_sha(&other_developer);
    std::fs::write(other_developer.join("feature.txt"), "two\n").unwrap();
    git_cli(&other_developer, &["commit", "-am", "Other second"]);
    let second = get_head_sha(&other_developer);
    git_cli(&other_developer, &["push", "origin", "main"]);
    assert_eq!(
        other_engine
            .run_sync_cycle()
            .await
            .unwrap()
            .git_to_svn_count,
        2
    );
    assert_eq!(
        other_engine.db().get_repo_watermark("other").unwrap().1,
        second
    );
    assert_eq!(
        SvnClient::new(&other_svn, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    for sha in [&first, &second] {
        let mapped: i64 = other_engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'other' AND direction = 'git_to_svn' AND git_sha = ?1",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(mapped, 1);
    }
    assert_pair_blocked_without_damage(&fixture, "missing_checkpoint_object").await;
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R17_SCOPING", "blocked_pair":"pair", "healthy_pair":"other",
            "other_p_before":other_import, "other_r_after":second,
            "other_svn_before":2, "other_svn_after":4,
            "global_cursor_not_borrowed":true, "first_pair_still_blocked":true
        })
    );
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

// Review 5: receipt policy must be checked before admitting any cursor shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_policy_equal_cursor_filtered_commit_rejected() {
    let mut pair = QualifiedPair::new().await;
    pair.developer_commit("handled.txt", "ordinary baseline\n", "Establish applied outbound cursor");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    pair.engine.set_path_rules(vec!["allow".into()], vec![]);
    let filtered = pair.developer_commit("blocked.txt", "filtered content\n", "Nonempty filtered Git commit");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    assert_eq!(pair.engine.db().get_repo_watermark("pair").unwrap().1, filtered);
    assert_eq!(pair.engine.db().get_state("last_git_sha_pair").unwrap(), Some(filtered.clone()));
    let receipt_key = format!("handled_git_no_target_pair_{filtered}");
    let receipt: serde_json::Value = serde_json::from_str(&pair.engine.db().get_state(&receipt_key).unwrap().unwrap()).unwrap();
    assert_eq!(receipt["outcome"], "filtered");
    let before = pair.snapshot().await;
    let db = Database::new(&pair.db_path).unwrap();
    let mut changed = SyncEngine::new(pair.engine.config().clone(), db,
        SvnClient::new(&pair.svn_url, "", ""), GitClient::new(&pair.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    changed.set_repo_id("pair".into());
    changed.set_path_rules(vec!["blocked".into()], vec![]);
    assert!(matches!(changed.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "receipt_policy_changed"));
    assert_eq!(pair.snapshot().await, before);
    drop(changed);
    let saved_receipt = pair.engine.db().get_state(&receipt_key).unwrap().unwrap();
    pair.engine.db().set_state(&receipt_key, "{malformed").unwrap();
    assert!(matches!(pair.engine.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "unverified_no_target_receipt"));
    assert_eq!(pair.snapshot().await, before);
    pair.engine.db().set_state(&receipt_key, &saved_receipt).unwrap();
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    let allowed = pair.developer_commit("allow.txt", "ordinary work\n", "Ordinary work under unchanged policy");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    assert_eq!(pair.engine.db().get_repo_watermark("pair").unwrap().1, allowed);
    let svn_rev = SvnClient::new(&pair.svn_url, "", "").info().await.unwrap().latest_rev;
    let tree = svn_tree(&pair, svn_rev).await;
    assert_eq!(tree.get("allow.txt"), Some(&b"ordinary work\n".to_vec()));
    assert!(!tree.contains_key("blocked.txt"));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_POLICY_EQUAL_CURSOR", "filtered":filtered,
        "receipt":receipt, "changed_policy_block":"receipt_policy_changed",
        "prewrite_snapshot_preserved":true, "same_policy_successor":allowed,
        "same_policy_applied_once":1, "svn_tree":tree_hashes(&tree)
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_policy_split_cursor_filtered_commit_rejected() {
    let mut pair = QualifiedPair::new().await;
    pair.developer_commit("handled.txt", "ordinary baseline\n", "Establish applied outbound cursor");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    pair.engine.set_path_rules(vec!["allow".into()], vec![]);
    let filtered = pair.developer_commit("blocked.txt", "filtered content\n", "Filtered before SVN publication");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    let svn_rev = svn_commit_file(&pair.wc, "origin.txt", "SVN after filtered Git\n", "Incoming after filtered Git");
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_ne!(pair.engine.db().get_repo_watermark("pair").unwrap().1, filtered);
    assert_eq!(pair.engine.db().get_state("last_git_sha_pair").unwrap(), Some(filtered.clone()));
    let before = pair.snapshot().await;
    let db = Database::new(&pair.db_path).unwrap();
    let mut changed = SyncEngine::new(pair.engine.config().clone(), db,
        SvnClient::new(&pair.svn_url, "", ""), GitClient::new(&pair.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    changed.set_repo_id("pair".into());
    changed.set_path_rules(vec!["blocked".into()], vec![]);
    assert!(matches!(changed.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "receipt_policy_changed"));
    assert_eq!(pair.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_POLICY_SPLIT_CURSOR", "filtered":filtered,
        "svn_revision":svn_rev, "emitted":before.watermark.1,
        "kv":before.kv_cursor, "changed_policy_block":"receipt_policy_changed",
        "prewrite_snapshot_preserved":true
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_policy_kv_only_filtered_commit_rejected() {
    let mut pair = QualifiedPair::new().await;
    pair.developer_commit("handled.txt", "ordinary baseline\n", "Establish applied outbound cursor");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    pair.engine.set_path_rules(vec!["allow".into()], vec![]);
    let filtered = pair.developer_commit("blocked.txt", "filtered content\n", "Filtered before KV-only overlay");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    // Explicit synthetic legacy overlay: the pinned old generator does not
    // create this absent-column shape.
    pair.engine.db().conn().execute("UPDATE repositories SET last_git_sha = '' WHERE id = 'pair'", []).unwrap();
    assert_eq!(pair.engine.db().get_state("last_git_sha_pair").unwrap(), Some(filtered.clone()));
    let before = pair.snapshot().await;
    let db = Database::new(&pair.db_path).unwrap();
    let mut changed = SyncEngine::new(pair.engine.config().clone(), db,
        SvnClient::new(&pair.svn_url, "", ""), GitClient::new(&pair.bridge).unwrap(),
        Arc::new(make_identity_mapper()));
    changed.set_repo_id("pair".into());
    changed.set_path_rules(vec!["blocked".into()], vec![]);
    assert!(matches!(changed.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "receipt_policy_changed"));
    assert_eq!(pair.snapshot().await, before);
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_POLICY_KV_ONLY", "filtered":filtered,
        "synthetic_column_absent":true, "kv":before.kv_cursor,
        "changed_policy_block":"receipt_policy_changed", "prewrite_snapshot_preserved":true
    }));
}

struct TestOutboundFault(&'static str);
impl TestOutboundFault {
    fn new(kind: &'static str, sha: &str, bridge: &Path) -> Self {
        let name = match kind { "read" => "REPOSYNC_TEST_GIT_CONTENT_FAULT", "stage" => "REPOSYNC_TEST_SVN_STAGE_FAULT", _ => panic!("unknown fault") };
        std::env::set_var(name, format!("{}|{}", sha, bridge.display()));
        Self(name)
    }
}
impl Drop for TestOutboundFault {
    fn drop(&mut self) { std::env::remove_var(self.0); }
}

struct TestOutboundPause(String);
impl TestOutboundPause {
    fn new(key: &'static str, sha: &str, bridge: &Path, dir: &Path) -> Self {
        let scoped_key = format!("{key}_{sha}_{}", hex::encode(sha2::Sha256::digest(bridge.to_string_lossy().as_bytes())));
        std::env::set_var(&scoped_key, dir);
        Self(scoped_key)
    }
}
impl Drop for TestOutboundPause {
    fn drop(&mut self) { std::env::remove_var(&self.0); }
}

async fn wait_outbound_pause(dir: &Path) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while !dir.join("ready").exists() {
        assert!(tokio::time::Instant::now() < deadline, "outbound test boundary not reached");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn assert_outbound_fault_stops_and_retries(kind: &'static str, case: &str) {
    let pair = QualifiedPair::new().await;
    let first = pair.developer_commit("fault.txt", "must not vanish\n", "First queued Git change");
    let second = pair.developer_commit("later.txt", "later work\n", "Second queued Git change");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    let before = pair.snapshot().await;
    let fault = TestOutboundFault::new(kind, &first, &pair.bridge);
    let result = pair.engine.run_sync_cycle().await;
    assert!(result.is_err(), "{kind} fault was incorrectly treated as no-target proof");
    drop(fault);
    let after_failure = pair.snapshot().await;
    assert_eq!(after_failure.svn_rev, before.svn_rev);
    assert_eq!(after_failure.svn_origin, before.svn_origin);
    assert_eq!(after_failure.remote_sha, before.remote_sha);
    assert_eq!(after_failure.remote_tree, before.remote_tree);
    assert_eq!(after_failure.watermark, before.watermark);
    assert_eq!(after_failure.kv_cursor, before.kv_cursor);
    assert_eq!(after_failure.mapping_count, before.mapping_count);
    assert_eq!(after_failure.bridge_sha, second);
    assert!(after_failure.bridge_status.is_empty());
    assert_eq!(pair.engine.db().get_repo_watermark("pair").unwrap().1, pair.imported_base);
    for sha in [&first, &second] {
        assert_eq!(pair.engine.db().get_state(&format!("handled_git_no_target_pair_{sha}")).unwrap(), None);
        let count: i64 = pair.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }
    let retry = pair.engine.run_sync_cycle().await.unwrap();
    assert_eq!(retry.git_to_svn_count, 2);
    assert_eq!(pair.engine.run_sync_cycle().await.unwrap().git_to_svn_count, 0);
    assert_eq!(pair.engine.db().get_repo_watermark("pair").unwrap().1, second);
    for sha in [&first, &second] {
        let count: i64 = pair.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }
    let rev = SvnClient::new(&pair.svn_url, "", "").info().await.unwrap().latest_rev;
    let svn = svn_tree(&pair, rev).await;
    assert_eq!(svn, tracked_tree(&pair.bridge));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":case, "fault":kind, "first":first, "queued_successor":second,
        "failed_without_receipt_or_advance":true, "retry_applied_each_once":true,
        "final_svn_revision":rev, "final_svn_tree":tree_hashes(&svn),
        "final_git_tree":tree_hashes(&tracked_tree(&pair.bridge))
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_no_target_read_failure_preserves_pending_successor() {
    assert_outbound_fault_stops_and_retries("read", "R01_NO_TARGET_READ_FAILURE").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_no_target_stage_failure_preserves_pending_successor() {
    assert_outbound_fault_stops_and_retries("stage", "R01_NO_TARGET_STAGE_FAILURE").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_nonempty_already_represented_delta_is_verified() {
    let pair = QualifiedPair::new().await;
    let rev = SvnClient::new(&pair.svn_url, "", "").info().await.unwrap().latest_rev;
    git_cli(&pair.developer, &["update-index", "--chmod=+x", "origin.txt"]);
    git_cli(&pair.developer, &["commit", "-m", "Git mode-only delta with represented content"]);
    let mode_sha = get_head_sha(&pair.developer);
    let successor = pair.developer_commit("later.txt", "queued after mode change\n", "Queued successor");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    let before = pair.snapshot().await;
    assert!(matches!(pair.engine.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "unsupported_git_semantics"));
    let after = pair.snapshot().await;
    assert_eq!(SvnClient::new(&pair.svn_url, "", "").info().await.unwrap().latest_rev, rev);
    assert_eq!(after.svn_origin, before.svn_origin);
    assert_eq!(after.watermark, before.watermark);
    assert_eq!(after.kv_cursor, before.kv_cursor);
    assert_eq!(after.mapping_count, before.mapping_count);
    assert_eq!(after.remote_sha, before.remote_sha);
    for sha in [&mode_sha, &successor] {
        assert_eq!(pair.engine.db().get_state(&format!("handled_git_no_target_pair_{sha}")).unwrap(), None);
        let count: i64 = pair.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_VERIFIED_NO_DELTA", "source_commit":mode_sha,
        "queued_successor":successor, "old_oracle":"v2_byte_only_receipt_success",
        "new_oracle":"unsupported_git_semantics_before_write",
        "pinned_svn_revision":rev, "checkpoint_unchanged":true,
        "target_byte_tree":tree_hashes(&svn_tree(&pair, rev).await),
        "source_git_entry":git_output(&pair.developer, &["ls-tree", &mode_sha, "origin.txt"])
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_regular_content_already_represented_is_verified() {
    let pair = QualifiedPair::new().await;
    let source_sha = pair.developer_commit("origin.txt", "same regular content\n", "Regular content delta");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    let pause_dir = pair.tmp.path().join("before-outbound-checkout");
    std::fs::create_dir(&pause_dir).unwrap();
    let pause = TestOutboundPause::new("REPOSYNC_TEST_BEFORE_GIT_TO_SVN_CHECKOUT", &source_sha, &pair.bridge, &pause_dir);
    let (cycle, represented_rev) = tokio::join!(pair.engine.run_sync_cycle(), async {
        wait_outbound_pause(&pause_dir).await;
        let rev = svn_commit_file(&pair.wc, "origin.txt", "same regular content\n", "Independent matching SVN content");
        std::fs::write(pause_dir.join("release"), b"").unwrap();
        rev
    });
    drop(pause);
    let cycle = cycle.unwrap();
    assert_eq!(cycle.git_to_svn_count, 0);
    assert_eq!(SvnClient::new(&pair.svn_url, "", "").info().await.unwrap().latest_rev, represented_rev);
    assert_eq!(pair.engine.db().get_repo_watermark("pair").unwrap().1, source_sha);
    let key = format!("handled_git_no_target_pair_{source_sha}");
    let receipt: serde_json::Value = serde_json::from_str(
        &pair.engine.db().get_state(&key).unwrap().expect("semantic no-target receipt")).unwrap();
    assert_eq!(receipt["version"], 3);
    assert_eq!(receipt["outcome"], "no_svn_delta");
    assert_eq!(receipt["target"]["svn_revision"], represented_rev);
    assert_eq!(receipt["target"]["semantic_projection"], "regular_file_bytes_no_properties_v1");
    assert_eq!(receipt["target"]["paths"]["origin.txt"]["git_mode"], 33188);
    assert_eq!(receipt["target"]["paths"]["origin.txt"]["svn_executable"], false);
    let old_v2 = serde_json::json!({
        "version":2,"repo_id":"pair","git_sha":source_sha,
        "outcome":"no_svn_delta","projection":receipt["projection"],
        "target":{"svn_revision":represented_rev,"svn_uuid":receipt["target"]["svn_uuid"],
                  "svn_url":receipt["target"]["svn_url"],"paths":{"origin.txt":receipt["target"]["paths"]["origin.txt"]["sha256"]}}
    });
    let v3 = pair.engine.db().get_state(&key).unwrap().unwrap();
    pair.engine.db().set_state(&key, &old_v2.to_string()).unwrap();
    assert!(matches!(pair.engine.run_sync_cycle().await,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "unverified_no_target_receipt"));
    pair.engine.db().set_state(&key, &v3).unwrap();
    assert_eq!(pair.engine.db().get_state(&key).unwrap(), Some(v3));
    let svn = svn_tree(&pair, represented_rev).await;
    assert_eq!(svn, tracked_tree(&pair.developer));
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_REPRESENTED_CONTENT", "source_commit":source_sha,
        "pinned_svn_revision":represented_rev, "semantic_receipt":receipt,
        "old_v2_unverified":true, "full_svn_bytes":tree_hashes(&svn),
        "full_git_bytes":tree_hashes(&tracked_tree(&pair.developer)),
        "git_entry":git_output(&pair.developer, &["ls-tree", &source_sha, "origin.txt"])
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r01_target_changes_during_no_delta_proof_blocks_successor() {
    let pair = QualifiedPair::new().await;
    let first = pair.developer_commit("origin.txt", "represented first\n", "Regular content first");
    let successor = pair.developer_commit("later.txt", "queued successor\n", "Queued after first");
    git_cli(&pair.developer, &["push", "origin", "main"]);
    let checkout_pause = pair.tmp.path().join("before-checkout");
    let verify_pause = pair.tmp.path().join("before-verify");
    std::fs::create_dir(&checkout_pause).unwrap();
    std::fs::create_dir(&verify_pause).unwrap();
    let checkout_guard = TestOutboundPause::new("REPOSYNC_TEST_BEFORE_GIT_TO_SVN_CHECKOUT", &first, &pair.bridge, &checkout_pause);
    let verify_guard = TestOutboundPause::new("REPOSYNC_TEST_BEFORE_NO_TARGET_VERIFY", &first, &pair.bridge, &verify_pause);
    let old_checkpoint = pair.engine.db().get_repo_watermark("pair").unwrap();
    let old_mapping_count = pair.snapshot().await.mapping_count;
    let (result, (represented_rev, mismatched_rev)) = tokio::join!(pair.engine.run_sync_cycle(), async {
        wait_outbound_pause(&checkout_pause).await;
        let represented = svn_commit_file(&pair.wc, "origin.txt", "represented first\n", "Independent representation");
        std::fs::write(checkout_pause.join("release"), b"").unwrap();
        wait_outbound_pause(&verify_pause).await;
        let mismatch = svn_commit_file(&pair.wc, "origin.txt", "different newer target\n", "Intervening target change");
        std::fs::write(verify_pause.join("release"), b"").unwrap();
        (represented, mismatch)
    });
    drop(checkout_guard);
    drop(verify_guard);
    assert!(matches!(result,
        Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "unverified_no_target"));
    assert_eq!(pair.engine.db().get_repo_watermark("pair").unwrap(), old_checkpoint);
    assert_eq!(pair.snapshot().await.mapping_count, old_mapping_count);
    assert_eq!(SvnClient::new(&pair.svn_url, "", "").info().await.unwrap().latest_rev, mismatched_rev);
    assert_eq!(mismatched_rev, represented_rev + 1);
    for sha in [&first, &successor] {
        assert_eq!(pair.engine.db().get_state(&format!("handled_git_no_target_pair_{sha}")).unwrap(), None);
        let count: i64 = pair.engine.db().conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND git_sha = ?1 AND direction = 'git_to_svn' AND status = 'applied'",
            [sha], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }
    let svn = svn_tree(&pair, mismatched_rev).await;
    assert_eq!(svn["origin.txt"], b"different newer target\n");
    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R01_TARGET_MISMATCH", "first":first, "queued_successor":successor,
        "represented_revision":represented_rev, "mismatched_revision":mismatched_rev,
        "failed_without_receipt_or_checkpoint_advance":true,
        "target_byte_tree":tree_hashes(&svn),
        "source_git_entry":git_output(&pair.developer, &["ls-tree", &first, "origin.txt"])
    }));
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_pinned_old_topology_read_safe_inventory() {
    let tmp = TempDir::new().unwrap();
    assert_fixture_owned(tmp.path());
    let old_root = tmp.path().join("pinned-old-topology");
    let generator = std::env::var("REPOSYNC_OLD_GENERATOR").expect("pinned old generator must be packaged");
    let generation = Command::new(&generator)
        .args(["generate_legacy_topology", "--exact", "--nocapture", "--test-threads=1"])
        .env("REPOSYNC_OLD_TOPOLOGY_DIR", &old_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Developer")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Developer")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output().unwrap();
    assert!(generation.status.success(), "pinned old topology: {} {}",
        String::from_utf8_lossy(&generation.stdout), String::from_utf8_lossy(&generation.stderr));
    let old_evidence = String::from_utf8_lossy(&generation.stderr).lines()
        .find_map(|line| line.strip_prefix("OLD_TOPOLOGY_EVIDENCE "))
        .map(|text| serde_json::from_str::<serde_json::Value>(text).unwrap())
        .expect("old production topology evidence missing");
    assert_eq!(old_evidence["pair"]["svn_rev"], 2);
    assert_eq!(old_evidence["pair_two"]["svn_rev"], 2);
    assert_ne!(old_evidence["pair"]["git_sha"], old_evidence["pair_two"]["git_sha"]);
    assert_eq!(old_evidence["disabled"]["enabled"], false);
    let original = old_root.join("install");
    let copy = tmp.path().join("quiesced-copy");
    copy_install_tree(&original, &copy);
    assert_eq!(tree_hashes(&exported_tree(&original)), tree_hashes(&exported_tree(&copy)));
    let inventory_script = std::env::var("REPOSYNC_INVENTORY_SCRIPT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/reliability-inventory.py").to_string());
    let python_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("python3")).find(|path| path.is_file())
        .expect("fixture Python interpreter");
    let python = |args: &[&str]| {
        Command::new(&python_bin).arg(&inventory_script).args(args)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PATH", "/nonexistent")
            .output().unwrap()
    };
    let seal_output = python(&["--seal-copy", copy.to_str().unwrap()]);
    assert!(seal_output.status.success(), "seal: {}", String::from_utf8_lossy(&seal_output.stderr));
    let seal_path = tmp.path().join("seal.json");
    std::fs::write(&seal_path, &seal_output.stdout).unwrap();
    let report_output = python(&["--copy", copy.to_str().unwrap(), "--manifest", seal_path.to_str().unwrap()]);
    assert!(report_output.status.success(), "inventory: {}", String::from_utf8_lossy(&report_output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&report_output.stdout).unwrap();
    let repositories = report["repositories"].as_array().unwrap();
    assert_eq!(repositories.len(), 3);
    for (id, classification) in [
        ("pair", "qualified_fixture_shape"),
        ("pair_two", "qualified_fixture_shape"),
        ("pair_disabled", "not_qualified"),
    ] {
        let row = repositories.iter().find(|row| row["id"] == id).unwrap();
        assert_eq!(row["classification"], classification);
        assert_eq!(row["source"]["svn_uuid"], "UNKNOWN_LOCAL_ONLY");
        assert_eq!(row["credentials"]["repo_svn_secret_present"], true);
        assert_eq!(row["credentials"]["repo_git_secret_present"], true);
        if id != "pair_disabled" {
            assert_eq!(row["checkpoints"]["repository_svn_revision"], 2);
            assert_eq!(row["checkpoints"]["scoped_git_kv"], row["checkpoints"]["repository_git_column"]);
            assert_eq!(row["target"]["local_ref"], row["checkpoints"]["repository_git_column"]);
            assert!(row["applied_mappings"].as_array().unwrap().iter().any(|entry|
                entry["svn_rev"] == 2 && entry["git_sha"] == row["checkpoints"]["repository_git_column"]));
        }
    }
    let report_text = String::from_utf8(report_output.stdout.clone()).unwrap();
    for secret in ["synthetic-svn-pair", "synthetic-git-pair", "synthetic-svn-pair_two",
                   "synthetic-git-pair_two", "synthetic-svn-pair_disabled", "synthetic-git-pair_disabled"] {
        assert!(!report_text.contains(secret), "inventory leaked a synthetic credential");
    }
    let repeated = python(&["--copy", copy.to_str().unwrap(), "--manifest", seal_path.to_str().unwrap()]);
    assert!(repeated.status.success());
    assert_eq!(repeated.stdout, report_output.stdout, "inventory is not deterministic");
    let after_seal = python(&["--seal-copy", copy.to_str().unwrap()]);
    assert_eq!(after_seal.stdout, seal_output.stdout, "input hashes or permissions changed");
    assert_eq!(tree_hashes(&exported_tree(&original)), tree_hashes(&exported_tree(&copy)));

    // These are explicit synthetic fault overlays, not states claimed to have
    // been emitted by unchanged old production code.
    let degraded = tmp.path().join("synthetic-pruned-overlay");
    copy_install_tree(&original, &degraded);
    {
        let db = rusqlite::Connection::open(degraded.join("reposync.db")).unwrap();
        assert_eq!(db.execute("DELETE FROM sync_records WHERE repo_id = 'pair' AND svn_rev = 2 AND direction = 'svn_to_git'", []).unwrap(), 1);
    }
    let degraded_seal = python(&["--seal-copy", degraded.to_str().unwrap()]);
    assert!(degraded_seal.status.success());
    let degraded_manifest = tmp.path().join("pruned-seal.json");
    std::fs::write(&degraded_manifest, degraded_seal.stdout).unwrap();
    let degraded_report = python(&["--copy", degraded.to_str().unwrap(), "--manifest", degraded_manifest.to_str().unwrap()]);
    assert!(degraded_report.status.success());
    let degraded_json: serde_json::Value = serde_json::from_slice(&degraded_report.stdout).unwrap();
    assert_eq!(degraded_json["repositories"][0]["classification"], "needs_reconciliation");

    let missing_receipt = tmp.path().join("synthetic-missing-receipt-overlay");
    copy_install_tree(&original, &missing_receipt);
    let synthetic_cursor = "a".repeat(40);
    {
        let db = rusqlite::Connection::open(missing_receipt.join("reposync.db")).unwrap();
        db.execute("UPDATE repositories SET last_git_sha = ?1 WHERE id = 'pair'", [&synthetic_cursor]).unwrap();
        db.execute("UPDATE kv_state SET value = ?1 WHERE key = 'last_git_sha_pair'", [&synthetic_cursor]).unwrap();
    }
    let missing_seal = python(&["--seal-copy", missing_receipt.to_str().unwrap()]);
    assert!(missing_seal.status.success());
    let missing_manifest = tmp.path().join("missing-seal.json");
    std::fs::write(&missing_manifest, missing_seal.stdout).unwrap();
    let missing_report = python(&["--copy", missing_receipt.to_str().unwrap(), "--manifest", missing_manifest.to_str().unwrap()]);
    assert!(missing_report.status.success());
    let missing_json: serde_json::Value = serde_json::from_slice(&missing_report.stdout).unwrap();
    assert_eq!(missing_json["repositories"][0]["classification"], "needs_reconciliation");
    assert!(missing_json["repositories"][0]["missing_proof"].as_array().unwrap().iter()
        .any(|entry| entry == "no_target_receipt_or_applied_mapping_missing"));

    let unverified_receipt = tmp.path().join("synthetic-v1-receipt-overlay");
    copy_install_tree(&original, &unverified_receipt);
    {
        let db = rusqlite::Connection::open(unverified_receipt.join("reposync.db")).unwrap();
        let sha = old_evidence["pair"]["git_sha"].as_str().unwrap();
        let value = serde_json::json!({"version":1,"repo_id":"pair","git_sha":sha,
            "outcome":"no_svn_delta","projection":"{\"allowed_paths\":[],\"blocked_patterns\":[]}"});
        db.execute("INSERT INTO kv_state (key,value,updated_at) VALUES (?1,?2,'')",
            rusqlite::params![format!("handled_git_no_target_pair_{sha}"), value.to_string()]).unwrap();
    }
    let receipt_seal = python(&["--seal-copy", unverified_receipt.to_str().unwrap()]);
    assert!(receipt_seal.status.success());
    let receipt_manifest = tmp.path().join("receipt-seal.json");
    std::fs::write(&receipt_manifest, receipt_seal.stdout).unwrap();
    let receipt_report = python(&["--copy", unverified_receipt.to_str().unwrap(), "--manifest", receipt_manifest.to_str().unwrap()]);
    assert!(receipt_report.status.success());
    let receipt_json: serde_json::Value = serde_json::from_slice(&receipt_report.stdout).unwrap();
    assert_eq!(receipt_json["repositories"][0]["classification"], "needs_reconciliation");
    assert_eq!(receipt_json["repositories"][0]["no_target_receipts"][0]["target_proof_present"], false);

    let unknown = tmp.path().join("synthetic-effect-unknown-overlay");
    copy_install_tree(&original, &unknown);
    {
        let db = rusqlite::Connection::open(unknown.join("reposync.db")).unwrap();
        db.execute("INSERT INTO kv_state (key,value,updated_at) VALUES ('effect_unknown_pair','synthetic-test-only','')", []).unwrap();
    }
    let unknown_seal = python(&["--seal-copy", unknown.to_str().unwrap()]);
    assert!(unknown_seal.status.success());
    let unknown_manifest = tmp.path().join("unknown-seal.json");
    std::fs::write(&unknown_manifest, unknown_seal.stdout).unwrap();
    let unknown_report = python(&["--copy", unknown.to_str().unwrap(), "--manifest", unknown_manifest.to_str().unwrap()]);
    assert!(unknown_report.status.success());
    let unknown_json: serde_json::Value = serde_json::from_slice(&unknown_report.stdout).unwrap();
    assert_eq!(unknown_json["repositories"][0]["classification"], "external_effect_unknown");

    let wal_copy = tmp.path().join("synthetic-incomplete-wal-overlay");
    copy_install_tree(&original, &wal_copy);
    std::fs::write(wal_copy.join("reposync.db-wal"), b"synthetic incomplete WAL").unwrap();
    assert!(!python(&["--seal-copy", wal_copy.to_str().unwrap()]).status.success());
    let wrong_schema = tmp.path().join("synthetic-future-schema-overlay");
    copy_install_tree(&original, &wrong_schema);
    {
        let db = rusqlite::Connection::open(wrong_schema.join("reposync.db")).unwrap();
        db.execute_batch("PRAGMA user_version = 13").unwrap();
    }
    let wrong_seal = python(&["--seal-copy", wrong_schema.to_str().unwrap()]);
    assert!(wrong_seal.status.success());
    let wrong_manifest = tmp.path().join("wrong-seal.json");
    std::fs::write(&wrong_manifest, wrong_seal.stdout).unwrap();
    assert!(!python(&["--copy", wrong_schema.to_str().unwrap(), "--manifest", wrong_manifest.to_str().unwrap()]).status.success());

    eprintln!("RELIABILITY_EVIDENCE {}", serde_json::json!({
        "case":"R10_PINNED_OLD_TOPOLOGY_INVENTORY", "old_generation":old_evidence,
        "old_generator_sha256":hex::encode(sha2::Sha256::digest(std::fs::read(&generator).unwrap())),
        "original_copy_file_count":report["file_count"],
        "input_hashes_and_permissions_unchanged":true,
        "repeated_report_equal":true, "no_git_or_svn_cli_on_inventory_path":true,
        "credential_values_redacted":true,
        "classifications":repositories.iter().map(|row| serde_json::json!({
            "id":row["id"], "classification":row["classification"],
            "svn_revision":row["checkpoints"]["repository_svn_revision"],
            "mapped_rows":row["applied_mappings"].as_array().unwrap().len()
        })).collect::<Vec<_>>(),
        "synthetic_pruned_reconciliation":true,
        "synthetic_missing_receipt_reconciliation":true,
        "synthetic_v1_unverified_receipt_reconciliation":true,
        "synthetic_external_effect_unknown":true,
        "incomplete_wal_refused":true, "future_schema_refused":true,
        "production_eligibility":"NOT_ESTABLISHED"
    }));
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_inventory_authority_and_confined_reads() {
    let tmp = TempDir::new().unwrap();
    assert_fixture_owned(tmp.path());
    let old_root = tmp.path().join("pinned-old-authority");
    let generator = std::env::var("REPOSYNC_OLD_GENERATOR").expect("pinned old generator must be packaged");
    let generated = Command::new(&generator)
        .args(["generate_legacy_topology", "--exact", "--nocapture", "--test-threads=1"])
        .env("REPOSYNC_OLD_TOPOLOGY_DIR", &old_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Developer")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Developer")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output().unwrap();
    assert!(generated.status.success(), "pinned old topology generation failed: {}",
        String::from_utf8_lossy(&generated.stderr));
    let inventory = std::env::var("REPOSYNC_INVENTORY_SCRIPT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/reliability-inventory.py").to_string());
    let probes = std::env::var("REPOSYNC_INVENTORY_PROBES_SCRIPT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/reliability-inventory-probes.py").to_string());
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("python3")).find(|path| path.is_file())
        .expect("fixture Python interpreter");
    let work = tmp.path().join("inventory-overlays");
    let result = Command::new(python)
        .args([&probes, "--inventory-script", &inventory,
               "--old-install", old_root.join("install").to_str().unwrap(),
               "--work", work.to_str().unwrap()])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PATH", "/nonexistent")
        .output().unwrap();
    assert!(result.status.success(), "inventory probes: {}", String::from_utf8_lossy(&result.stderr));
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(evidence["case"], "R10_INVENTORY_AUTHORITY_CONFINEMENT");
    assert_eq!(evidence["outside_canary_open_count"], 0);
    assert!(evidence["reports"].as_object().unwrap().len() >= 14);
    eprintln!("RELIABILITY_EVIDENCE {}", evidence);
}
