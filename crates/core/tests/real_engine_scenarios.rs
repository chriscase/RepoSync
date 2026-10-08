//! Real-engine scenario suite for issue #62.
//!
//! Drives disposable `svnserve` remotes and local bare Git repositories through
//! the production `SyncEngine` team path. Each scenario emits `RELIABILITY_EVIDENCE`
//! with a stable case id for the host runner (`scripts/real-engine-scenario-suite.sh`).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::Database;
use reposync_core::errors::SyncError;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::{SyncEngine, SyncStats};
use tempfile::TempDir;

const CASE_CONCURRENT: &str = "R17_SVNSERVE_CONCURRENT_OVERLAP";
const CASE_CHECKPOINT: &str = "R17_SVNSERVE_CHECKPOINT_ISOLATION";
const CASE_CREDENTIAL: &str = "R17_SVNSERVE_CREDENTIAL_ISOLATION";
const CASE_JOB: &str = "R17_SVNSERVE_JOB_ISOLATION";
const CASE_SVN_MULTI: &str = "R01_SVNSERVE_MULTI_COMMIT_SVN_TO_GIT";
const CASE_GIT_MULTI: &str = "R01_SVNSERVE_MULTI_COMMIT_GIT_TO_SVN";
const CASE_GIT_REMOTE: &str = "R16_SVNSERVE_GIT_REMOTE_UNREACHABLE";
const CASE_SVN_REMOTE: &str = "R16_SVNSERVE_SVN_REMOTE_UNREACHABLE";

// ===========================================================================
// Tooling and evidence helpers
// ===========================================================================

fn toolchain_available() -> Option<String> {
    for tool in ["svn", "svnadmin", "svnserve", "git"] {
        let ok = Command::new(tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Some(tool.to_string());
        }
    }
    None
}

fn emit_evidence(case: &str, status: &str, detail: serde_json::Value) {
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": case,
            "status": status,
            "detail": detail,
        })
    );
}

fn is_transient_import_contention(error: &SyncError) -> bool {
    let message = error.to_string();
    message.contains("database is locked")
        || message.contains("pending_journal_finalize")
        || message.contains("checkpoint_write_failed")
}

async fn run_import_with_lock_retry(engine: &mut SyncEngine) -> Result<SyncStats, SyncError> {
    for attempt in 0..12 {
        match engine.run_sync_cycle().await {
            Ok(stats) => return Ok(stats),
            Err(error) if is_transient_import_contention(&error) && attempt + 1 < 12 => {
                tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)))
                    .await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("retry loop must return")
}

fn require_toolchain(case: &str) -> bool {
    if let Some(missing) = toolchain_available() {
        emit_evidence(
            case,
            "NOT RUN",
            serde_json::json!({
                "reason": "missing_tool",
                "tool": missing,
            }),
        );
        return false;
    }
    true
}

fn git_cli(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} in {}", args, repo.display());
}

fn git_symbolic_ref(repo: &Path, ref_name: &str) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--short", ref_name])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_show_blob(repo: &Path, object: &str, path: &str) -> String {
    let spec = format!("{}:{}", object, path);
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &spec])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git show {} in {}",
        spec,
        repo.display()
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

async fn svn_read_file(
    svn_url: &str,
    username: &str,
    password: &str,
    revision: i64,
    relative_path: &str,
) -> String {
    let export_root = tempfile::tempdir().unwrap();
    SvnClient::new(svn_url, username, password)
        .export("", revision, export_root.path())
        .await
        .unwrap();
    std::fs::read_to_string(export_root.path().join(relative_path)).unwrap()
}

fn get_head_sha(repo_path: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn setup_git_with_bare_origin(work_dir: &Path, bare_dir: &Path) -> GitClient {
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
    config.svn.layout = reposync_core::config::SvnLayout::Custom;
    config
}

fn make_identity_mapper() -> IdentityMapper {
    let config = IdentityConfig {
        email_domain: Some("example.com".into()),
        ..Default::default()
    };
    IdentityMapper::new(&config).unwrap()
}

fn write_svnserve_auth(repo_dir: &Path, username: &str, password: &str) {
    let conf_dir = repo_dir.join("conf");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("svnserve.conf"),
        format!(
            "[general]\n\
anon-access = none\n\
auth-access = write\n\
password-db = passwd\n\
realm = {}\n",
            repo_dir.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();
    std::fs::write(
        conf_dir.join("passwd"),
        format!("[users]\n{username} = {password}\n"),
    )
    .unwrap();
    let hook = repo_dir.join("hooks/pre-revprop-change");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn svn_commit_file(
    wc_path: &Path,
    filename: &str,
    content: &str,
    message: &str,
    username: &str,
    password: &str,
) -> i64 {
    let file_path = wc_path.join(filename);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&file_path, content).unwrap();
    let status_output = Command::new("svn")
        .args(["status", file_path.to_str().unwrap()])
        .output()
        .unwrap();
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    if status_str.contains('?') {
        assert!(Command::new("svn")
            .args(["add", file_path.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
    }
    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            wc_path.to_str().unwrap(),
            "--username",
            username,
            "--password",
            password,
            "--non-interactive",
            "--no-auth-cache",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svn commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Committed revision ")
                .map(|rev| rev.trim_end_matches('.').parse::<i64>().unwrap())
        })
        .expect("committed revision")
}

fn svn_checkout(url: &str, wc_path: &Path, username: &str, password: &str) {
    let status = Command::new("svn")
        .args([
            "checkout",
            url,
            wc_path.to_str().unwrap(),
            "--username",
            username,
            "--password",
            password,
            "--non-interactive",
            "--no-auth-cache",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .expect("failed to run svn checkout");
    assert!(status.success(), "svn checkout failed for {url}");
}

fn pick_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("failed to bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

struct SvnserveDaemon {
    child: Child,
    port: u16,
}

impl SvnserveDaemon {
    fn start(root: &Path) -> Self {
        let port = pick_loopback_port();
        let port_arg = port.to_string();
        let child = Command::new("svnserve")
            .args([
                "-d",
                "--foreground",
                "-r",
                root.to_str().unwrap(),
                "--listen-host",
                "127.0.0.1",
                "--listen-port",
                port_arg.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn svnserve");
        std::thread::sleep(std::time::Duration::from_millis(200));
        Self { child, port }
    }

    fn repo_url(&self, name: &str) -> String {
        format!("svn://127.0.0.1:{}/{}", self.port, name)
    }
}

impl Drop for SvnserveDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct SvnserveRepo {
    id: String,
    svn_url: String,
    username: String,
    password: String,
    svn_rev: i64,
    bridge: PathBuf,
    bare: PathBuf,
    initial_git_sha: String,
}

struct DualRepoFixture {
    tmp: TempDir,
    _daemon: SvnserveDaemon,
    db_path: PathBuf,
    repos: [SvnserveRepo; 2],
}

impl DualRepoFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let svn_root = tmp.path().join("svnserve-root");
        std::fs::create_dir_all(&svn_root).unwrap();
        let daemon = SvnserveDaemon::start(&svn_root);
        let db_path = tmp.path().join("shared.db");
        let db = setup_db(&db_path);
        let now = chrono::Utc::now().to_rfc3339();
        let mut repos = Vec::new();

        for (id, user, pass, marker) in [
            ("repo_alpha", "alpha", "alpha-secret-only", "alpha-origin\n"),
            ("repo_beta", "beta", "beta-secret-only", "beta-origin\n"),
        ] {
            let repo_dir = svn_root.join(id);
            assert!(Command::new("svnadmin")
                .args(["create", repo_dir.to_str().unwrap()])
                .status()
                .unwrap()
                .success());
            write_svnserve_auth(&repo_dir, user, pass);
            let svn_url = daemon.repo_url(id);
            let wc = tmp.path().join(format!("{id}_wc"));
            svn_checkout(&svn_url, &wc, user, pass);
            svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor", user, pass);
            svn_commit_file(&wc, "origin.txt", marker, "Verified SVN origin", user, pass);
            let svn_rev = 2;
            let bridge = tmp.path().join(format!("{id}_bridge"));
            let bare = tmp.path().join(format!("{id}.git"));
            let git = setup_git_with_bare_origin(&bridge, &bare);
            std::fs::write(bridge.join("git-anchor.txt"), marker).unwrap();
            git.commit(
                &format!("Git anchor for {id}"),
                "Test User",
                "test@example.com",
                "Test User",
                "test@example.com",
            )
            .expect("failed to commit per-repo git anchor");
            git.push("origin", "main")
                .expect("failed to push git anchor");
            let initial_git_sha = get_head_sha(&bridge);
            drop(git);
            db.set_state(&format!("secret_svn_password_{id}"), pass)
                .unwrap();
            db.set_state(
                &format!("secret_git_token_{id}"),
                &format!("git-token-{id}"),
            )
            .unwrap();
            db.insert_repository(&Repository {
                id: id.into(),
                name: id.into(),
                svn_url: svn_url.clone(),
                svn_branch: String::new(),
                svn_username: user.into(),
                git_provider: "local".into(),
                git_api_url: String::new(),
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
                updated_at: now.clone(),
                last_svn_rev: 1,
                last_git_sha: initial_git_sha.clone(),
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
            repos.push(SvnserveRepo {
                id: id.into(),
                svn_url,
                username: user.into(),
                password: pass.into(),
                svn_rev,
                bridge,
                bare,
                initial_git_sha,
            });
        }

        Self {
            tmp,
            _daemon: daemon,
            db_path,
            repos: [repos[0].clone(), repos[1].clone()],
        }
    }

    fn make_engine(&self, repo: &SvnserveRepo) -> SyncEngine {
        let db = setup_db(&self.db_path);
        let config = make_app_config(&repo.svn_url, self.tmp.path());
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new(&repo.svn_url, &repo.username, &repo.password),
            GitClient::new(&repo.bridge).unwrap(),
            Arc::new(make_identity_mapper()),
        );
        engine.set_repo_id(repo.id.clone());
        engine
    }
}

struct SingleRepoFixture {
    tmp: TempDir,
    _daemon: SvnserveDaemon,
    db_path: PathBuf,
    repo: SvnserveRepo,
    wc: PathBuf,
    developer: PathBuf,
}

impl SingleRepoFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let svn_root = tmp.path().join("svnserve-root");
        std::fs::create_dir_all(&svn_root).unwrap();
        let daemon = SvnserveDaemon::start(&svn_root);
        let db_path = tmp.path().join("sync.db");
        let db = setup_db(&db_path);
        let id = "repo_sync";
        let user = "syncer";
        let pass = "syncer-secret-only";
        let repo_dir = svn_root.join(id);
        assert!(Command::new("svnadmin")
            .args(["create", repo_dir.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        write_svnserve_auth(&repo_dir, user, pass);
        let svn_url = daemon.repo_url(id);
        let wc = tmp.path().join("wc");
        svn_checkout(&svn_url, &wc, user, pass);
        svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor", user, pass);
        svn_commit_file(
            &wc,
            "origin.txt",
            "SVN origin\n",
            "Verified SVN origin",
            user,
            pass,
        );
        let bridge = tmp.path().join("bridge");
        let bare = tmp.path().join("origin.git");
        let git = setup_git_with_bare_origin(&bridge, &bare);
        let initial_git_sha = get_head_sha(&bridge);
        drop(git);
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
        assert!(clone.status.success());
        let now = chrono::Utc::now().to_rfc3339();
        db.set_state(&format!("secret_svn_password_{id}"), pass)
            .unwrap();
        db.set_state(
            &format!("secret_git_token_{id}"),
            &format!("git-token-{id}"),
        )
        .unwrap();
        db.insert_repository(&Repository {
            id: id.into(),
            name: id.into(),
            svn_url: svn_url.clone(),
            svn_branch: String::new(),
            svn_username: user.into(),
            git_provider: "local".into(),
            git_api_url: String::new(),
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
            updated_at: now.clone(),
            last_svn_rev: 1,
            last_git_sha: initial_git_sha.clone(),
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
        let repo = SvnserveRepo {
            id: id.into(),
            svn_url,
            username: user.into(),
            password: pass.into(),
            svn_rev: 2,
            bridge,
            bare,
            initial_git_sha,
        };
        Self {
            tmp,
            _daemon: daemon,
            db_path,
            repo,
            wc,
            developer,
        }
    }

    fn make_engine(&self) -> SyncEngine {
        let db = setup_db(&self.db_path);
        let config = make_app_config(&self.repo.svn_url, self.tmp.path());
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new(&self.repo.svn_url, &self.repo.username, &self.repo.password),
            GitClient::new(&self.repo.bridge).unwrap(),
            Arc::new(make_identity_mapper()),
        );
        engine.set_repo_id(self.repo.id.clone());
        engine
    }

    fn developer_commit(&self, path: &str, content: &str, message: &str) -> String {
        let file = self.developer.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&file, content).unwrap();
        git_cli(&self.developer, &["add", path]);
        git_cli(
            &self.developer,
            &[
                "commit",
                "-m",
                message,
                "--author",
                "Test User <test@example.com>",
            ],
        );
        get_head_sha(&self.developer)
    }

    fn checkpoint_snapshot(&self) -> (i64, String, Option<String>) {
        let db = setup_db(&self.db_path);
        let watermark = db.get_repo_watermark(&self.repo.id).unwrap();
        let inbound = db
            .get_state(&format!("last_git_sha_{}", self.repo.id))
            .unwrap();
        (watermark.0, watermark.1, inbound)
    }
}

// Clone only the metadata we need for the array.
impl Clone for SvnserveRepo {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            svn_url: self.svn_url.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            svn_rev: self.svn_rev,
            bridge: self.bridge.clone(),
            bare: self.bare.clone(),
            initial_git_sha: self.initial_git_sha.clone(),
        }
    }
}

// ===========================================================================
// Scenarios
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenario_r17_svnserve_concurrent_overlap() {
    if !require_toolchain(CASE_CONCURRENT) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let alpha = fixture.repos[0].clone();
    let beta = fixture.repos[1].clone();
    assert_eq!(alpha.svn_rev, beta.svn_rev, "overlapping revision numbers");
    assert_eq!(git_symbolic_ref(&alpha.bridge, "HEAD"), "main");
    assert_eq!(git_symbolic_ref(&beta.bridge, "HEAD"), "main");
    assert_ne!(
        alpha.initial_git_sha, beta.initial_git_sha,
        "distinct pending imports on the same branch name"
    );

    let alpha_bridge = alpha.bridge.clone();
    let beta_bridge = beta.bridge.clone();
    let ready = Arc::new(tokio::sync::Barrier::new(2));
    let overlap_ready = Arc::new(tokio::sync::Barrier::new(2));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));

    let (alpha_result, beta_result) = tokio::join!(
        async {
            ready.wait().await;
            let active = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(active, Ordering::SeqCst);
            overlap_ready.wait().await;
            let mut engine = fixture.make_engine(&alpha);
            let result = run_import_with_lock_retry(&mut engine).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        },
        async {
            ready.wait().await;
            let active = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(active, Ordering::SeqCst);
            overlap_ready.wait().await;
            let mut engine = fixture.make_engine(&beta);
            let result = run_import_with_lock_retry(&mut engine).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        }
    );
    let alpha_stats = alpha_result.unwrap();
    let beta_stats = beta_result.unwrap();
    assert_eq!(alpha_stats.svn_to_git_count, 1);
    assert_eq!(beta_stats.svn_to_git_count, 1);

    let db = setup_db(&fixture.db_path);
    let alpha_sha = db.get_repo_watermark("repo_alpha").unwrap().1;
    let beta_sha = db.get_repo_watermark("repo_beta").unwrap().1;
    assert_ne!(alpha_sha, beta_sha, "imports must remain content-distinct");
    assert_eq!(db.get_repo_watermark("repo_alpha").unwrap().0, 2);
    assert_eq!(db.get_repo_watermark("repo_beta").unwrap().0, 2);
    assert_eq!(
        std::fs::read_to_string(alpha_bridge.join("origin.txt")).unwrap(),
        "alpha-origin\n"
    );
    assert_eq!(
        std::fs::read_to_string(beta_bridge.join("origin.txt")).unwrap(),
        "beta-origin\n"
    );

    let concurrent_import = max_in_flight.load(Ordering::SeqCst) >= 2;
    assert!(
        concurrent_import,
        "both imports must overlap inside run_sync_cycle"
    );
    let checkpoint_crossover = alpha_sha == beta_sha
        || matches!(
            (
                db.get_state("last_git_sha_repo_alpha").unwrap(),
                db.get_state("last_git_sha_repo_beta").unwrap(),
            ),
            (Some(a), Some(b)) if a == b
        );

    emit_evidence(
        CASE_CONCURRENT,
        "PASS",
        serde_json::json!({
            "repo_alpha_rev": 2,
            "repo_beta_rev": 2,
            "equal_revision_numbers": alpha.svn_rev == beta.svn_rev,
            "shared_branch_name": git_symbolic_ref(&alpha_bridge, "HEAD"),
            "alpha_git_sha": alpha_sha,
            "beta_git_sha": beta_sha,
            "max_in_flight": max_in_flight.load(Ordering::SeqCst),
            "concurrent_import": concurrent_import,
            "checkpoint_crossover": checkpoint_crossover,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r17_svnserve_checkpoint_isolation() {
    if !require_toolchain(CASE_CHECKPOINT) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let db = setup_db(&fixture.db_path);
    for repo in &fixture.repos {
        let engine = fixture.make_engine(repo);
        assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
        let watermark = db.get_repo_watermark(&repo.id).unwrap();
        db.set_state(&format!("last_git_sha_{}", repo.id), &watermark.1)
            .unwrap();
    }

    let beta_before = db.get_repo_watermark("repo_beta").unwrap();
    let alpha_before = db.get_repo_watermark("repo_alpha").unwrap();
    let alpha_inbound_before = db
        .get_state("last_git_sha_repo_alpha")
        .unwrap()
        .expect("import must seed inbound checkpoint kv");
    let beta_inbound_before = db
        .get_state("last_git_sha_repo_beta")
        .unwrap()
        .expect("import must seed inbound checkpoint kv");
    assert_ne!(alpha_inbound_before, beta_inbound_before);
    let tampered_inbound = "cccccccccccccccccccccccccccccccccccccccc";
    db.set_state("last_git_sha_repo_alpha", tampered_inbound)
        .unwrap();
    assert_eq!(
        db.get_repo_watermark("repo_alpha").unwrap(),
        alpha_before,
        "tamper must not rewrite emitted tip column"
    );

    let beta_engine = fixture.make_engine(&fixture.repos[1]);
    let beta_cycle = beta_engine.run_sync_cycle().await.unwrap();
    assert_eq!(beta_cycle.svn_to_git_count, 0);
    assert_eq!(db.get_repo_watermark("repo_beta").unwrap(), beta_before);
    assert_eq!(
        db.get_state("last_git_sha_repo_beta").unwrap(),
        Some(beta_inbound_before.clone())
    );

    let alpha_engine = fixture.make_engine(&fixture.repos[0]);
    let blocked = alpha_engine.run_sync_cycle().await;
    assert!(
        blocked.is_err(),
        "tampered alpha inbound checkpoint must block"
    );

    let alpha_records: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'repo_alpha'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let beta_records: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'repo_beta'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(alpha_records >= 1);
    assert!(beta_records >= 1);
    assert_ne!(alpha_before.1, beta_before.1);

    let beta_still_healthy = db.get_repo_watermark("repo_beta").unwrap() == beta_before
        && db.get_state("last_git_sha_repo_beta").unwrap() == Some(beta_inbound_before.clone())
        && beta_cycle.svn_to_git_count == 0
        && beta_cycle.git_to_svn_count == 0;
    let global_cursor_not_borrowed = db.get_state("last_git_sha_repo_beta").unwrap()
        != Some(tampered_inbound.to_string())
        && db.get_repo_watermark("repo_beta").unwrap() == beta_before;

    emit_evidence(
        CASE_CHECKPOINT,
        "PASS",
        serde_json::json!({
            "alpha_watermark_before": alpha_before,
            "alpha_inbound_before": alpha_inbound_before,
            "beta_watermark_before": beta_before,
            "beta_inbound_before": beta_inbound_before,
            "beta_watermark_after": db.get_repo_watermark("repo_beta").unwrap(),
            "beta_inbound_after": db.get_state("last_git_sha_repo_beta").unwrap(),
            "alpha_blocked_after_tamper": blocked.is_err(),
            "beta_still_healthy": beta_still_healthy,
            "global_cursor_not_borrowed": global_cursor_not_borrowed,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r17_svnserve_credential_isolation() {
    if !require_toolchain(CASE_CREDENTIAL) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let db = setup_db(&fixture.db_path);
    assert_eq!(
        db.get_state("secret_svn_password_repo_alpha")
            .unwrap()
            .as_deref(),
        Some("alpha-secret-only")
    );
    assert_eq!(
        db.get_state("secret_svn_password_repo_beta")
            .unwrap()
            .as_deref(),
        Some("beta-secret-only")
    );
    assert_ne!(
        db.get_state("secret_svn_password_repo_alpha").unwrap(),
        db.get_state("secret_svn_password_repo_beta").unwrap()
    );

    let wrong_client = SvnClient::new(
        &fixture.repos[0].svn_url,
        &fixture.repos[0].username,
        "beta-secret-only",
    );
    let cross_credential_auth_rejected = wrong_client.info().await.is_err();
    assert!(
        cross_credential_auth_rejected,
        "beta credential must not authenticate alpha svnserve repo"
    );

    for repo in &fixture.repos {
        let engine = fixture.make_engine(repo);
        assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    }

    assert_eq!(
        db.get_state("secret_svn_password_repo_alpha")
            .unwrap()
            .as_deref(),
        Some("alpha-secret-only")
    );
    assert_eq!(
        db.get_state("secret_svn_password_repo_beta")
            .unwrap()
            .as_deref(),
        Some("beta-secret-only")
    );
    assert_eq!(
        db.get_state("secret_git_token_repo_alpha")
            .unwrap()
            .as_deref(),
        Some("git-token-repo_alpha")
    );
    assert_eq!(
        db.get_state("secret_git_token_repo_beta")
            .unwrap()
            .as_deref(),
        Some("git-token-repo_beta")
    );

    let per_repo_svn_secrets = db.get_state("secret_svn_password_repo_alpha").unwrap()
        != db.get_state("secret_svn_password_repo_beta").unwrap();
    let per_repo_git_secrets = db.get_state("secret_git_token_repo_alpha").unwrap()
        != db.get_state("secret_git_token_repo_beta").unwrap();
    let credential_crossover = db.get_state("secret_svn_password_repo_alpha").unwrap()
        == Some("beta-secret-only".into())
        || db.get_state("secret_svn_password_repo_beta").unwrap()
            == Some("alpha-secret-only".into());

    emit_evidence(
        CASE_CREDENTIAL,
        "PASS",
        serde_json::json!({
            "per_repo_svn_secrets": per_repo_svn_secrets,
            "per_repo_git_secrets": per_repo_git_secrets,
            "cross_credential_auth_rejected": cross_credential_auth_rejected,
            "credential_crossover": credential_crossover,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r17_svnserve_job_isolation() {
    if !require_toolchain(CASE_JOB) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let db = setup_db(&fixture.db_path);
    let op = db
        .create_import_operation(
            "repo_alpha",
            "fixture-admin",
            "request-alpha-only",
            "fingerprint-alpha",
        )
        .unwrap();
    db.start_import_operation("repo_alpha", &op.id).unwrap();
    assert!(db.active_import_operation("repo_beta").unwrap().is_none());
    let active_beta = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM kv_state WHERE key LIKE 'import_operation_v1:active:repo_beta%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(active_beta, 0);

    let beta_engine = fixture.make_engine(&fixture.repos[1]);
    let beta_stats = beta_engine.run_sync_cycle().await.unwrap();
    assert_eq!(beta_stats.svn_to_git_count, 1);
    assert!(db.active_import_operation("repo_alpha").unwrap().is_some());
    assert!(db.active_import_operation("repo_beta").unwrap().is_none());

    let failed = db
        .finish_import_operation(
            "repo_alpha",
            &op.id,
            ImportOperationState::Failed,
            "fixture cleanup without touching beta",
        )
        .unwrap();
    assert_eq!(failed.state, ImportOperationState::Failed);
    assert!(db.active_import_operation("repo_beta").unwrap().is_none());
    let alpha_active = db.active_import_operation("repo_alpha").unwrap();
    assert!(alpha_active.is_some());
    assert_eq!(alpha_active.unwrap().repo_id, "repo_alpha");

    let beta_import_while_alpha_job_active =
        beta_stats.svn_to_git_count == 1 && beta_stats.git_to_svn_count == 0;
    let job_crossover = db.active_import_operation("repo_beta").unwrap().is_some()
        || db
            .get_state("secret_svn_password_repo_beta")
            .unwrap()
            .as_deref()
            == Some("alpha-secret-only");

    emit_evidence(
        CASE_JOB,
        "PASS",
        serde_json::json!({
            "alpha_active_import": op.id,
            "alpha_terminal_state": format!("{:?}", failed.state),
            "beta_active_import": db.active_import_operation("repo_beta").unwrap().map(|row| row.id),
            "beta_import_while_alpha_job_active": beta_import_while_alpha_job_active,
            "job_crossover": job_crossover,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_multi_commit_svn_to_git() {
    if !require_toolchain(CASE_SVN_MULTI) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let base_rev = 2;
    let first_rev = svn_commit_file(
        &fixture.wc,
        "step-a.txt",
        "first svn delta\n",
        "First SVN delta",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    let second_rev = svn_commit_file(
        &fixture.wc,
        "step-b.txt",
        "second svn delta\n",
        "Second SVN delta",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    assert_eq!((first_rev, second_rev), (base_rev + 1, base_rev + 2));
    let before_checkpoint = fixture.checkpoint_snapshot();
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 2);
    assert_eq!(stats.git_to_svn_count, 0);
    let db = setup_db(&fixture.db_path);
    for (revision, path, content) in [
        (first_rev, "step-a.txt", "first svn delta\n"),
        (second_rev, "step-b.txt", "second svn delta\n"),
    ] {
        let git_sha: String = db.conn().query_row(
            "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
            rusqlite::params![fixture.repo.id, revision],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(git_show_blob(&fixture.repo.bridge, &git_sha, path), content);
        assert_eq!(
            svn_read_file(
                &fixture.repo.svn_url,
                &fixture.repo.username,
                &fixture.repo.password,
                revision,
                path,
            )
            .await,
            content
        );
    }
    let after_checkpoint = fixture.checkpoint_snapshot();
    assert_eq!(after_checkpoint.0, second_rev);
    let repeat = engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    assert_eq!(fixture.checkpoint_snapshot(), after_checkpoint);
    emit_evidence(
        CASE_SVN_MULTI,
        "PASS",
        serde_json::json!({
            "svn_revisions": [first_rev, second_rev],
            "checkpoint_before": before_checkpoint,
            "checkpoint_after": after_checkpoint,
            "repeat_noop": repeat.svn_to_git_count == 0 && repeat.git_to_svn_count == 0,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_multi_commit_git_to_svn() {
    if !require_toolchain(CASE_GIT_MULTI) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let svn_before = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    let first =
        fixture.developer_commit("feature.txt", "version one\n", "First pending Git commit");
    let second =
        fixture.developer_commit("feature.txt", "version two\n", "Second pending Git commit");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let before_checkpoint = fixture.checkpoint_snapshot();
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 2);
    assert_eq!(stats.svn_to_git_count, 0);
    let svn_after = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    assert_eq!(svn_after, svn_before + 2);
    let db = setup_db(&fixture.db_path);
    for (revision, content, sha) in [
        (svn_before + 1, "version one\n", &first),
        (svn_after, "version two\n", &second),
    ] {
        assert_eq!(
            svn_read_file(
                &fixture.repo.svn_url,
                &fixture.repo.username,
                &fixture.repo.password,
                revision,
                "feature.txt",
            )
            .await,
            content
        );
        let mapped: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND direction = 'git_to_svn' AND svn_rev = ?2 AND git_sha = ?3 AND status = 'applied'",
            rusqlite::params![fixture.repo.id, revision, sha],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(mapped, 1);
    }
    let after_checkpoint = fixture.checkpoint_snapshot();
    assert_eq!(after_checkpoint.1, second);
    let repeat = engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.git_to_svn_count, repeat.svn_to_git_count), (0, 0));
    assert_eq!(fixture.checkpoint_snapshot(), after_checkpoint);
    emit_evidence(
        CASE_GIT_MULTI,
        "PASS",
        serde_json::json!({
            "first_git": first,
            "second_git": second,
            "svn_before": svn_before,
            "svn_after": svn_after,
            "checkpoint_before": before_checkpoint,
            "checkpoint_after": after_checkpoint,
            "repeat_noop": repeat.git_to_svn_count == 0 && repeat.svn_to_git_count == 0,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_git_remote_unreachable() {
    if !require_toolchain(CASE_GIT_REMOTE) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let missing = fixture.tmp.path().join("missing-remote.git");
    git_cli(
        &fixture.repo.bridge,
        &["remote", "set-url", "origin", missing.to_str().unwrap()],
    );
    let before = fixture.checkpoint_snapshot();
    let bridge_before = get_head_sha(&fixture.repo.bridge);
    let mappings_before: i64 = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let svn_before = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    let result = engine.run_sync_cycle().await;
    assert!(
        matches!(&result, Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_transport_failed"),
        "unreachable git remote must block without reset: {result:?}"
    );
    assert_eq!(get_head_sha(&fixture.repo.bridge), bridge_before);
    assert_eq!(fixture.checkpoint_snapshot(), before);
    assert_eq!(
        SvnClient::new(
            &fixture.repo.svn_url,
            &fixture.repo.username,
            &fixture.repo.password,
        )
        .info()
        .await
        .unwrap()
        .latest_rev,
        svn_before
    );
    assert_eq!(
        setup_db(&fixture.db_path).count_sync_records().unwrap(),
        mappings_before
    );
    emit_evidence(
        CASE_GIT_REMOTE,
        "PARTIAL",
        serde_json::json!({
            "reason": "remote_transport_failed",
            "checkpoint_before_after": before,
            "bridge_head_before_after": bridge_before,
            "svn_revision_before_after": svn_before,
            "mapping_count_before": mappings_before,
            "note": "svnserve fixture covers local transport failure only",
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_svn_remote_unreachable() {
    if !require_toolchain(CASE_SVN_REMOTE) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let before = fixture.checkpoint_snapshot();
    let bridge_before = get_head_sha(&fixture.repo.bridge);
    let mappings_before: i64 = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let bad_url = "svn://127.0.0.1:1/unreachable";
    let db = setup_db(&fixture.db_path);
    let config = make_app_config(bad_url, fixture.tmp.path());
    let mut blocked_engine = SyncEngine::new(
        config,
        db,
        SvnClient::new(bad_url, &fixture.repo.username, &fixture.repo.password),
        GitClient::new(&fixture.repo.bridge).unwrap(),
        Arc::new(make_identity_mapper()),
    );
    blocked_engine.set_repo_id(fixture.repo.id.clone());
    let result = blocked_engine.run_sync_cycle().await;
    assert!(result.is_err(), "unreachable svn remote must not succeed");
    assert_eq!(get_head_sha(&fixture.repo.bridge), bridge_before);
    assert_eq!(fixture.checkpoint_snapshot(), before);
    assert_eq!(
        setup_db(&fixture.db_path).count_sync_records().unwrap(),
        mappings_before
    );
    emit_evidence(
        CASE_SVN_REMOTE,
        "PARTIAL",
        serde_json::json!({
            "bad_svn_url": bad_url,
            "error": result.err().map(|e| e.to_string()),
            "checkpoint_before_after": before,
            "bridge_head_before_after": bridge_before,
            "mapping_count_before": mappings_before,
            "note": "loopback svnserve fixture does not recreate remotes on failure",
        }),
    );
}
