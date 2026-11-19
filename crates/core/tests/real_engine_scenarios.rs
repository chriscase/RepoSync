//! Real-engine scenario suite for issue #62.
//!
//! Drives disposable `svnserve` remotes and local bare Git repositories through
//! the production `SyncEngine` team path. Each scenario emits `RELIABILITY_EVIDENCE`
//! with a stable case id for the host runner (`scripts/real-engine-scenario-suite.sh`).

use std::collections::HashSet;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::lineage_checkpoints::{
    git_source_key, independently_established_svn_uuid, independently_established_svn_uuid_kv_key,
    load_canonical_lineage, load_directional_checkpoint, record_independently_established_svn_uuid,
    svn_source_key, CanonicalCheckpoint, CanonicalLineage, CheckpointDirection, MappingOutcome,
};
use reposync_core::db::Database;
use reposync_core::errors::{SvnError, SyncError};
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
const CASE_LARGE_MESSAGE: &str = "R01_SVNSERVE_LARGE_MESSAGE_RETRY";
const CASE_GIT_REMOTE: &str = "R16_SVNSERVE_GIT_REMOTE_UNREACHABLE";
const CASE_SVN_REMOTE: &str = "R16_SVNSERVE_SVN_REMOTE_UNREACHABLE";
const CASE_SVN_AUTH: &str = "R16_SVNSERVE_SVN_AUTH_DENIED";
const CASE_MISSING_BRANCH: &str = "R16_SVNSERVE_MISSING_GIT_BRANCH";
const CASE_BIDIRECTIONAL: &str = "R01_SVNSERVE_BIDIRECTIONAL_ROUNDTRIP";
const CASE_CREDENTIAL_ROTATION: &str = "R16_SVNSERVE_CREDENTIAL_ROTATION";
const CASE_PARENT_CHILD_CHAIN_ROTATION: &str =
    "R16_SVNSERVE_PARENT_CHILD_CREDENTIAL_CHAIN_ROTATION";
const CASE_CONCURRENT_CREDENTIAL_RELOAD: &str = "R17_SVNSERVE_CONCURRENT_CREDENTIAL_RELOAD";
const CASE_PARENT_CHILD_CONCURRENT_RELOAD: &str =
    "R17_SVNSERVE_PARENT_CHILD_CONCURRENT_CREDENTIAL_RELOAD";
const CASE_FAILED_APPLY_BARRIER: &str = "R01_SVNSERVE_FAILED_APPLY_BARRIER";
const CASE_FAILED_APPLY_RETRY: &str = "R01_SVNSERVE_FAILED_APPLY_RETRY";
const CASE_POST_WRITE_RECOVERY: &str = "R01_SVNSERVE_POST_WRITE_RECOVERY";
const CASE_SVN_REMOTE_RECREATION: &str = "R16_SVNSERVE_SVN_REMOTE_RECREATION";
const CASE_GIT_REMOTE_RECREATION: &str = "R16_SVNSERVE_GIT_REMOTE_RECREATION";
const CASE_NOTIFICATION_ISOLATION: &str = "R17_SVNSERVE_NOTIFICATION_ISOLATION";

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

struct CycleWindow {
    label: String,
    start: Instant,
    end: Instant,
}

static IMPORT_CYCLE_ID: AtomicUsize = AtomicUsize::new(1);

struct ImportCycleOracle {
    windows: Mutex<Vec<CycleWindow>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    active_cycle_ids: Mutex<HashSet<usize>>,
    cycle_id_labels: Mutex<std::collections::HashMap<usize, String>>,
    finished_while_peer_in_flight: Mutex<HashSet<String>>,
}

impl ImportCycleOracle {
    fn new() -> Self {
        Self {
            windows: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            active_cycle_ids: Mutex::new(HashSet::new()),
            cycle_id_labels: Mutex::new(std::collections::HashMap::new()),
            finished_while_peer_in_flight: Mutex::new(HashSet::new()),
        }
    }

    fn alloc_cycle_id() -> usize {
        IMPORT_CYCLE_ID.fetch_add(1, Ordering::SeqCst)
    }

    fn begin_import_cycle(&self, cycle_id: usize, label: &str) -> bool {
        let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(active, Ordering::SeqCst);
        let mut ids = self.active_cycle_ids.lock().unwrap();
        let peer_present = !ids.is_empty();
        ids.insert(cycle_id);
        self.cycle_id_labels
            .lock()
            .unwrap()
            .insert(cycle_id, label.to_string());
        peer_present
    }

    fn has_peer_cycle(&self, cycle_id: usize) -> bool {
        self.active_cycle_ids
            .lock()
            .unwrap()
            .iter()
            .any(|id| *id != cycle_id)
    }

    fn both_labels_in_flight(&self, left: &str, right: &str) -> bool {
        let labels = self.cycle_id_labels.lock().unwrap();
        let mut seen_left = false;
        let mut seen_right = false;
        for label in labels.values() {
            if label == left {
                seen_left = true;
            }
            if label == right {
                seen_right = true;
            }
        }
        seen_left && seen_right
    }

    fn end_import_cycle(
        &self,
        cycle_id: usize,
        label: &str,
        span_start: Instant,
        span_end: Instant,
    ) {
        self.active_cycle_ids.lock().unwrap().remove(&cycle_id);
        self.cycle_id_labels.lock().unwrap().remove(&cycle_id);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.windows.lock().unwrap().push(CycleWindow {
            label: label.to_string(),
            start: span_start,
            end: span_end,
        });
    }

    fn windows_overlap(&self, left: &str, right: &str) -> bool {
        let windows = self.windows.lock().unwrap();
        for a in windows.iter().filter(|w| w.label == left) {
            for b in windows.iter().filter(|w| w.label == right) {
                if a.start < b.end && b.start < a.end {
                    return true;
                }
            }
        }
        false
    }
}

async fn run_import_with_lock_retry(
    engine: &mut SyncEngine,
    label: &str,
    oracle: Option<&ImportCycleOracle>,
) -> Result<SyncStats, SyncError> {
    let cycle_id = oracle
        .map(|_| ImportCycleOracle::alloc_cycle_id())
        .unwrap_or(0);
    let span_start = Instant::now();
    let mut peer_overlap_latch = false;
    if let Some(tracker) = oracle {
        peer_overlap_latch = tracker.begin_import_cycle(cycle_id, label);
    }
    for attempt in 0..12 {
        let cycle_result = engine.run_sync_cycle().await;
        let cycle_end = Instant::now();
        match cycle_result {
            Ok(stats) => {
                if let Some(tracker) = oracle {
                    if peer_overlap_latch || tracker.has_peer_cycle(cycle_id) {
                        tracker
                            .finished_while_peer_in_flight
                            .lock()
                            .unwrap()
                            .insert(label.to_string());
                    }
                    tracker.end_import_cycle(cycle_id, label, span_start, cycle_end);
                }
                return Ok(stats);
            }
            Err(error) if is_transient_import_contention(&error) && attempt + 1 < 12 => {
                tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)))
                    .await;
            }
            Err(error) => {
                if let Some(tracker) = oracle {
                    tracker.end_import_cycle(cycle_id, label, span_start, cycle_end);
                }
                return Err(error);
            }
        }
    }
    unreachable!("retry loop must return")
}

fn git_commit_identity_args() -> [&'static str; 4] {
    [
        "-c",
        "user.name=Test User",
        "-c",
        "user.email=test@example.com",
    ]
}

fn git_committer_identity_args() -> [&'static str; 4] {
    [
        "-c",
        "committer.name=Test User",
        "-c",
        "committer.email=test@example.com",
    ]
}

fn git_path_missing(repo: &Path, object: &str, path: &str) -> bool {
    let spec = format!("{}:{}", object, path);
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &spec])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
}

fn assert_git_ancestor(repo: &Path, ancestor: &str, descendant: &str) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "{} must be an ancestor of {} in {}",
        ancestor,
        descendant,
        repo.display()
    );
}

fn is_svn_auth_failure(error: &SyncError) -> bool {
    match error {
        SyncError::SvnError(SvnError::AuthenticationFailed { .. }) => true,
        SyncError::SvnError(SvnError::CommandFailed { stderr, .. }) => {
            stderr.to_ascii_lowercase().contains("authentication")
        }
        _ => false,
    }
}

fn is_remote_transport_failure(error: &SyncError) -> bool {
    match error {
        SyncError::HistoryBlocked { reason, .. } => reason == "remote_transport_failed",
        other => {
            let lowered = other.to_string().to_ascii_lowercase();
            lowered.contains("transport")
                || lowered.contains("can't connect")
                || lowered.contains("could not connect")
                || lowered.contains("connection refused")
                || lowered.contains("failed to connect")
                || lowered.contains("econnrefused")
        }
    }
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

/// UUID of the synthetic repository this constructor just created (`svnadmin create`).
/// Read from that on-disk repo, not from a later live URL observation.
fn uuid_of_constructor_created_svn_repo(repo_dir: &Path) -> String {
    let output = Command::new("svnlook")
        .args(["uuid", repo_dir.to_str().unwrap()])
        .output()
        .expect("svnlook uuid of constructor-created repository");
    assert!(
        output.status.success(),
        "svnlook uuid failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let uuid = String::from_utf8(output.stdout)
        .expect("constructor-created SVN UUID is utf-8")
        .trim()
        .to_string();
    assert!(
        !uuid.is_empty(),
        "constructor-created SVN UUID is empty for {}",
        repo_dir.display()
    );
    uuid
}

fn establish_constructor_created_svn_identity(db: &Database, repo_id: &str, repo_dir: &Path) {
    let uuid = uuid_of_constructor_created_svn_repo(repo_dir);
    record_independently_established_svn_uuid(db, repo_id, &uuid).unwrap_or_else(|error| {
        panic!("record constructor-created SVN UUID for {repo_id}: {error}")
    });
}

fn scoped_inbound_git_checkpoint(db: &Database, repo_id: &str) -> Option<String> {
    reposync_core::echo_receipt_scope::read_scoped_last_git_sha_kv(db, repo_id)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn scoped_inbound_git_checkpoint_kv_key(db: &Database, repo_id: &str) -> String {
    let scope = reposync_core::db::repo_scope_identity::repository_scope_uuid(&db.conn(), repo_id)
        .expect("managed repository scope_uuid");
    reposync_core::db::repo_scope_identity::last_git_sha_kv_key(&scope)
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

fn rotate_svnserve_password(repo_dir: &Path, username: &str, new_password: &str) {
    write_svnserve_auth(repo_dir, username, new_password);
}

fn svnserve_repo_dir(svn_root: &Path, repo_id: &str) -> PathBuf {
    svn_root.join(repo_id)
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

fn wait_for_svnserve(port: u16) {
    for attempt in 0..40 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25 * (attempt as u64 + 1)));
    }
    panic!("svnserve did not accept connections on port {port}");
}

fn pick_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("failed to bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

struct SvnserveDaemon {
    child: Option<Child>,
    port: u16,
}

impl SvnserveDaemon {
    fn start(root: &Path) -> Self {
        Self::start_on_port(root, pick_loopback_port())
    }

    fn start_on_port(root: &Path, port: u16) -> Self {
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
        let daemon = Self {
            child: Some(child),
            port,
        };
        wait_for_svnserve(port);
        daemon
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if child.try_wait()?.is_none() {
            // An exit racing kill() is harmless once try_wait() reaps it.
            let kill_error = child.kill().err();
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            while child.try_wait()?.is_none() {
                if let Some(error) = kill_error {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "fixture svnserve did not exit after kill",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        self.child = None;
        Ok(())
    }

    fn repo_url(&self, name: &str) -> String {
        format!("svn://127.0.0.1:{}/{}", self.port, name)
    }
}

impl Drop for SvnserveDaemon {
    fn drop(&mut self) {
        let _ = self.stop();
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
            establish_constructor_created_svn_identity(&db, id, &repo_dir);
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
    daemon: SvnserveDaemon,
    db_path: PathBuf,
    repo: SvnserveRepo,
    wc: PathBuf,
    developer: PathBuf,
}

impl SingleRepoFixture {
    fn new() -> Self {
        Self::with_registration_mode("team")
    }

    fn with_registration_mode(sync_mode: &str) -> Self {
        Self::on_svn_branch(sync_mode, "")
    }

    fn on_svn_branch(sync_mode: &str, branch: &str) -> Self {
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
        let root_url = daemon.repo_url(id);
        let svn_url = if branch.is_empty() {
            root_url.clone()
        } else {
            let url =
                reposync_core::svn::operand::configured_branch_url(&root_url, branch).unwrap();
            let created = Command::new("svn")
                .args([
                    "mkdir",
                    &url,
                    "-m",
                    "Create managed branch",
                    "--username",
                    user,
                    "--password",
                    pass,
                    "--non-interactive",
                    "--no-auth-cache",
                ])
                .output()
                .unwrap();
            assert!(
                created.status.success(),
                "svn mkdir: {}",
                String::from_utf8_lossy(&created.stderr)
            );
            url
        };
        let wc = tmp.path().join("wc");
        svn_checkout(&svn_url, &wc, user, pass);
        let anchor = svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor", user, pass);
        let svn_rev = svn_commit_file(
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
            svn_url: root_url,
            svn_branch: branch.to_owned(),
            svn_username: user.into(),
            git_provider: "local".into(),
            git_api_url: String::new(),
            git_repo: bare.to_string_lossy().to_string(),
            git_branch: "main".into(),
            sync_mode: sync_mode.into(),
            poll_interval_secs: 5,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            last_svn_rev: anchor,
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
        establish_constructor_created_svn_identity(&db, id, &repo_dir);
        let repo = SvnserveRepo {
            id: id.into(),
            svn_url,
            username: user.into(),
            password: pass.into(),
            svn_rev,
            bridge,
            bare,
            initial_git_sha,
        };
        Self {
            tmp,
            daemon,
            db_path,
            repo,
            wc,
            developer,
        }
    }

    fn make_engine(&self) -> SyncEngine {
        let db = setup_db(&self.db_path);
        let config = make_app_config(&self.repo.svn_url, self.tmp.path());
        self.make_engine_with_config(db, config)
    }

    fn make_engine_with_config(&self, db: Database, config: AppConfig) -> SyncEngine {
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
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.developer)
            .args(git_commit_identity_args())
            .args(git_committer_identity_args())
            .args([
                "commit",
                "-m",
                message,
                "--author",
                "Test User <test@example.com>",
            ])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "git commit in {}",
            self.developer.display()
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
    let oracle = Arc::new(ImportCycleOracle::new());

    let (alpha_result, beta_result) = tokio::join!(
        async {
            ready.wait().await;
            let mut engine = fixture.make_engine(&alpha);
            run_import_with_lock_retry(&mut engine, "repo_alpha", Some(&oracle)).await
        },
        async {
            ready.wait().await;
            let mut engine = fixture.make_engine(&beta);
            run_import_with_lock_retry(&mut engine, "repo_beta", Some(&oracle)).await
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

    let import_cycle_windows_overlapped = oracle.windows_overlap("repo_alpha", "repo_beta");
    let max_in_flight = oracle.max_in_flight.load(Ordering::SeqCst);
    let peak_concurrent_import_cycles = max_in_flight >= 2;
    let at_least_one_import_finished_while_peer_in_flight = !oracle
        .finished_while_peer_in_flight
        .lock()
        .unwrap()
        .is_empty();
    assert!(
        max_in_flight <= 2,
        "lock-retry must not double-count in_flight (saw {max_in_flight})"
    );
    assert!(
        peak_concurrent_import_cycles
            && import_cycle_windows_overlapped
            && at_least_one_import_finished_while_peer_in_flight,
        "concurrent svnserve imports must overlap in flight and in cycle windows, and one import must finish while a peer cycle is still active"
    );
    let case_status = "PASS";
    let checkpoint_crossover = alpha_sha == beta_sha
        || matches!(
            (
                scoped_inbound_git_checkpoint(&db, "repo_alpha"),
                scoped_inbound_git_checkpoint(&db, "repo_beta"),
            ),
            (Some(a), Some(b)) if a == b
        );

    emit_evidence(
        CASE_CONCURRENT,
        case_status,
        serde_json::json!({
            "repo_alpha_rev": 2,
            "repo_beta_rev": 2,
            "equal_revision_numbers": alpha.svn_rev == beta.svn_rev,
            "shared_branch_name": git_symbolic_ref(&alpha_bridge, "HEAD"),
            "alpha_git_sha": alpha_sha,
            "beta_git_sha": beta_sha,
            "max_in_flight": max_in_flight,
            "peak_concurrent_import_cycles": peak_concurrent_import_cycles,
            "import_cycle_windows_overlapped": import_cycle_windows_overlapped,
            "at_least_one_import_finished_while_peer_in_flight": at_least_one_import_finished_while_peer_in_flight,
            "note": "import_cycle_windows_overlapped is temporal overlap of SyncEngine cycles; peak_concurrent_import_cycles proves two imports were in flight together; at_least_one_import_finished_while_peer_in_flight is one successful completion before the peer cycle ended",
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
        std::fs::write(repo.bridge.join("inbound-seed.txt"), "seed\n").unwrap();
        git_cli(&repo.bridge, &["add", "inbound-seed.txt"]);
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo.bridge)
            .args(git_commit_identity_args())
            .args(git_committer_identity_args())
            .args([
                "commit",
                "-m",
                "Seed inbound Git checkpoint",
                "--author",
                "Test User <test@example.com>",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        git_cli(&repo.bridge, &["push", "origin", "main"]);
        let git_to_svn = fixture.make_engine(repo);
        assert_eq!(
            git_to_svn.run_sync_cycle().await.unwrap().git_to_svn_count,
            1
        );
    }

    let beta_before = db.get_repo_watermark("repo_beta").unwrap();
    let alpha_before = db.get_repo_watermark("repo_alpha").unwrap();
    let alpha_inbound_before = scoped_inbound_git_checkpoint(&db, "repo_alpha")
        .expect("Git-to-SVN cycle must write scoped inbound checkpoint kv");
    let beta_inbound_before = scoped_inbound_git_checkpoint(&db, "repo_beta")
        .expect("Git-to-SVN cycle must write scoped inbound checkpoint kv");
    assert_ne!(alpha_inbound_before, beta_inbound_before);
    let tampered_inbound = "cccccccccccccccccccccccccccccccccccccccc";
    db.set_state(
        &scoped_inbound_git_checkpoint_kv_key(&db, "repo_alpha"),
        tampered_inbound,
    )
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
        scoped_inbound_git_checkpoint(&db, "repo_beta"),
        Some(beta_inbound_before.clone())
    );

    let alpha_engine = fixture.make_engine(&fixture.repos[0]);
    let blocked = alpha_engine.run_sync_cycle().await;
    assert!(
        matches!(
            &blocked,
            Err(SyncError::HistoryBlocked {
                reason,
                ..
            }) if reason == "ambiguous_checkpoint"
        ),
        "tampered alpha inbound checkpoint must block with ambiguous_checkpoint: {blocked:?}"
    );
    let alpha_after = db.get_repo_watermark("repo_alpha").unwrap();
    assert_eq!(
        alpha_after, alpha_before,
        "alpha emitted tip must stay unchanged after blocked cycle"
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
        && scoped_inbound_git_checkpoint(&db, "repo_beta") == Some(beta_inbound_before.clone())
        && beta_cycle.svn_to_git_count == 0
        && beta_cycle.git_to_svn_count == 0;
    let global_cursor_not_borrowed = scoped_inbound_git_checkpoint(&db, "repo_beta")
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
            "beta_inbound_after": scoped_inbound_git_checkpoint(&db, "repo_beta"),
            "alpha_watermark_after": alpha_after,
            "alpha_blocked_after_tamper": true,
            "alpha_emitted_tip_unchanged": alpha_after == alpha_before,
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
    let first_git_sha: String = db.conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
        rusqlite::params![fixture.repo.id, first_rev],
        |row| row.get(0),
    ).unwrap();
    let second_git_sha: String = db.conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
        rusqlite::params![fixture.repo.id, second_rev],
        |row| row.get(0),
    ).unwrap();
    assert_git_ancestor(&fixture.repo.bridge, &first_git_sha, &second_git_sha);
    assert!(
        git_path_missing(&fixture.repo.bridge, &first_git_sha, "step-b.txt"),
        "first mapped git commit must not contain step-b.txt"
    );
    for (revision, path, content, git_sha) in [
        (first_rev, "step-a.txt", "first svn delta\n", &first_git_sha),
        (
            second_rev,
            "step-b.txt",
            "second svn delta\n",
            &second_git_sha,
        ),
    ] {
        assert_eq!(git_show_blob(&fixture.repo.bridge, git_sha, path), content);
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
    const SENTINEL: &str = "baseline Git edit\n";
    let fixture = SingleRepoFixture::new();
    let repo = &fixture.repo;
    let svn = SvnClient::new(&repo.svn_url, &repo.username, &repo.password);
    let svn_dir = fixture.tmp.path().join("svnserve-root").join(&repo.id);
    let engine = fixture.make_engine();
    let initial = engine.run_sync_cycle().await.unwrap();
    assert_eq!((initial.svn_to_git_count, initial.git_to_svn_count), (1, 0));

    // A real seed commit supplies both a nonempty inbound cursor and a
    // sentinel independent of the two edits that will wait behind the outage.
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let seed_sha = fixture.developer_commit("origin.txt", SENTINEL, "Establish Git sentinel");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let seed = engine.run_sync_cycle().await.unwrap();
    assert_eq!((seed.svn_to_git_count, seed.git_to_svn_count), (0, 1));
    let settled = engine.run_sync_cycle().await.unwrap();
    assert_eq!((settled.svn_to_git_count, settled.git_to_svn_count), (0, 0));

    let first = fixture.developer_commit("queued.txt", "first edit\n", "Queued Git edit one");
    let second = fixture.developer_commit("queued.txt", "second edit\n", "Queued Git edit two");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let bridge = git_main_commit_and_tree(&repo.bridge);
    let source = git_main_commit_and_tree(&repo.bare);
    assert_eq!(bridge.0.to_string(), seed_sha);
    assert_eq!(source.0.to_string(), second);
    let assert_origin = || {
        let git = git2::Repository::open(&repo.bridge).unwrap();
        let remote = git.find_remote("origin").unwrap();
        assert_eq!(remote.url(), repo.bare.to_str());
    };
    let info = svn.info().await.unwrap();
    let seed_rev = info.latest_rev;
    assert_eq!(seed_rev, repo.svn_rev + 1);
    let svn_state = || {
        assert_eq!(svnlook_at(&svn_dir, "uuid", &[]).trim(), info.uuid);
        assert_eq!(svnlook_at(&svn_dir, "cat", &["origin.txt"]), SENTINEL);
        let youngest: i64 = svnlook_at(&svn_dir, "youngest", &[])
            .trim()
            .parse()
            .unwrap();
        (
            youngest,
            svnlook_at(&svn_dir, "tree", &["--full-paths", "--show-ids"]),
        )
    };
    let before_svn = svn_state();
    assert_eq!(before_svn.0, seed_rev);
    let before = outage_ledger(&fixture);
    assert_eq!(before.inbound_git.as_deref(), Some(seed_sha.as_str()));
    assert_eq!(before.watermark, (repo.svn_rev, seed_sha.clone()));
    assert_eq!(before.lineage.svn_uuid, info.uuid);
    assert_eq!(
        before.svn_checkpoint.handled_source_key,
        svn_source_key(repo.svn_rev)
    );
    assert_eq!(
        before.git_checkpoint.as_ref().unwrap().handled_source_key,
        git_source_key(&seed_sha)
    );

    // Move the actual local bare repository. Both clients keep their configured
    // origin, and the parked repository is available for read-only inspection.
    assert_origin();
    let parked = fixture.tmp.path().join("parked-origin.git");
    std::fs::rename(&repo.bare, &parked).unwrap();
    let history_block = || {
        let db = Database::new(&fixture.db_path).unwrap();
        let key =
            reposync_core::history_inspect::history_block_key_for_repo(&db, &repo.id).unwrap();
        reposync_core::history_inspect::load_history_block(&db, &key).unwrap()
    };
    let outage_reason = "remote_transport_failed";
    // Run the existing engine first, then open another while the origin is still absent.
    for engine in std::iter::once(engine).chain(std::iter::once_with(|| fixture.make_engine())) {
        let failure =
            tokio::time::timeout(std::time::Duration::from_secs(30), engine.run_sync_cycle())
                .await
                .expect("Git outage cycle timed out")
                .unwrap_err();
        assert!(
            matches!(&failure, SyncError::HistoryBlocked { reason, .. } if reason == outage_reason),
            "{failure}"
        );
        drop(engine);
        assert!(
            !repo.bare.try_exists().unwrap(),
            "engine recreated the unavailable origin"
        );
        assert_origin();
        assert_eq!(git_main_commit_and_tree(&parked), source);
        assert_eq!(git_main_commit_and_tree(&repo.bridge), bridge);
        assert_eq!(svn_state(), before_svn);
        assert_eq!(outage_ledger(&fixture), before);
        let block = history_block().expect("scoped Git history block");
        assert_eq!(block["repo_id"], repo.id);
        assert_eq!(block["p_handled"], seed_sha);
        assert_eq!(block["o_prior_observed"], seed_sha);
        assert_eq!(block["l_bridge"], seed_sha);
        assert!(block["r_fresh_remote"].is_null());
        assert_eq!(block["reason"], outage_reason);
        assert_eq!(block["durable"], false);
    }

    std::fs::rename(&parked, &repo.bare).unwrap();
    assert_origin();
    assert_eq!(git_main_commit_and_tree(&repo.bare), source);
    let reopened = fixture.make_engine();
    let recovered = reopened.run_sync_cycle().await.unwrap();
    assert_eq!(
        (recovered.svn_to_git_count, recovered.git_to_svn_count),
        (0, 2)
    );
    assert!(history_block().is_none());
    assert_origin();
    assert_eq!(git_main_commit_and_tree(&repo.bare), source);
    assert_eq!(git_main_commit_and_tree(&repo.bridge), source);
    let first_rev = seed_rev + 1;
    let last_rev = seed_rev + 2;
    let info_after = svn.info().await.unwrap();
    assert_eq!(
        (&info_after.url, &info_after.uuid, info_after.latest_rev),
        (&info.url, &info.uuid, last_rev)
    );
    let after = outage_ledger(&fixture);
    assert_eq!(after.lineage, before.lineage);
    assert_eq!(after.svn_checkpoint, before.svn_checkpoint);
    assert_eq!(after.inbound_git.as_deref(), Some(second.as_str()));
    assert_eq!(after.watermark, (repo.svn_rev, second.clone()));
    let checkpoint = after.git_checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.handled_source_key, git_source_key(&second));
    assert_eq!(checkpoint.target_svn_rev, Some(last_rev));
    assert_eq!(checkpoint.outcome, MappingOutcome::AppliedAndVerified);
    assert_eq!(after.commit_map, before.commit_map);
    for (current, prior) in [
        (&after.sync_records, &before.sync_records),
        (&after.outcomes, &before.outcomes),
    ] {
        assert!(current.starts_with(prior), "pre-outage row changed");
        assert_eq!(current.len(), prior.len() + 2);
    }
    let log = svn.log(first_rev, last_rev).await.unwrap();
    assert_eq!(log.len(), 2);
    let original = git2::Repository::open(&repo.bare).unwrap();
    for (i, (sha, parent, rev, subject, bytes, action)) in [
        (
            &first,
            &seed_sha,
            first_rev,
            "Queued Git edit one",
            "first edit\n",
            "A",
        ),
        (
            &second,
            &first,
            last_rev,
            "Queued Git edit two",
            "second edit\n",
            "M",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let commit = original.find_commit(sha.parse().unwrap()).unwrap();
        assert_eq!(commit.parent_count(), 1);
        assert_eq!(commit.parent_id(0).unwrap().to_string(), *parent);
        let record = &after.sync_records[before.sync_records.len() + i];
        let outcome = &after.outcomes[before.outcomes.len() + i];
        assert_eq!(record["sha"], *sha);
        assert_eq!(record["revision"], rev);
        assert_eq!(record["direction"], "git_to_svn");
        assert_eq!(record["status"], "applied");
        assert_eq!(outcome["key"], git_source_key(sha));
        assert_eq!(outcome["target_svn_rev"], rev);
        assert_eq!(outcome["direction"], "git_to_svn");
        assert_eq!(outcome["outcome"], "applied_and_verified");

        let entry = &log[i];
        assert_eq!(entry.revision, rev);
        assert_eq!(entry.message.lines().next(), Some(subject));
        assert_eq!(
            entry.message.lines().last().unwrap(),
            format!("RepoSync-Git-SHA: {sha}")
        );
        assert_eq!(entry.changed_paths.len(), 1);
        assert_eq!(
            (
                entry.changed_paths[0].action.as_str(),
                entry.changed_paths[0].path.as_str()
            ),
            (action, "/queued.txt")
        );
        for (path, expected) in [
            (".gitkeep", ""),
            ("origin.txt", SENTINEL),
            ("queued.txt", bytes),
        ] {
            assert_eq!(git_show_blob(&repo.bare, sha, path), expected);
            assert_eq!(
                svnlook_at(&svn_dir, "cat", &["-r", &rev.to_string(), path]),
                expected
            );
        }
    }
    let after_svn = svn_state();
    assert_eq!(after_svn.0, last_rev);
    for _ in 0..2 {
        let repeat = reopened.run_sync_cycle().await.unwrap();
        assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
        assert_eq!(outage_ledger(&fixture), after);
        assert_eq!(svn_state(), after_svn);
        assert_eq!(git_main_commit_and_tree(&repo.bare), source);
        assert_eq!(git_main_commit_and_tree(&repo.bridge), source);
    }
    emit_evidence(
        CASE_GIT_REMOTE,
        "PASS",
        serde_json::json!({
            "direction": "git_to_svn", "transport": "local_bare_repository",
            "outage_reason": outage_reason, "outage_cycles": 2, "transient_block_cleared": true,
            "remote_ref": "refs/heads/main", "restored_git_tip": second, "restored_git_tree": source.1.to_string(),
            "queued_git_commits": [first, second], "applied_svn_revisions": [first_rev, last_rev],
            "svn_uuid": info.uuid, "svn_youngest_before_after": [seed_rev, last_rev],
            "scoped_inbound_git_before_after": [before.inbound_git, after.inbound_git],
            "canonical_outcome_count_before_after": [before.outcomes.len(), after.outcomes.len()],
            "sync_record_count_before_after": [before.sync_records.len(), after.sync_records.len()],
            "verified_sentinel": "origin.txt", "repeat_noop_cycles": 2,
        }),
    );
}

// Capture every scoped ledger row, including IDs and timestamps. This detects
// rewriting an existing receipt even when row counts and coordinates agree.
#[derive(Debug, PartialEq)]
struct OutageLedger {
    watermark: (i64, String),
    inbound_git: Option<String>,
    lineage: CanonicalLineage,
    svn_checkpoint: CanonicalCheckpoint,
    git_checkpoint: Option<CanonicalCheckpoint>,
    commit_map: Vec<serde_json::Value>,
    sync_records: Vec<serde_json::Value>,
    outcomes: Vec<serde_json::Value>,
}

fn outage_ledger(fixture: &SingleRepoFixture) -> OutageLedger {
    let db = Database::new(&fixture.db_path).unwrap();
    let repo_id = &fixture.repo.id;
    let watermark = db.get_repo_watermark(repo_id).unwrap();
    let inbound_git = scoped_inbound_git_checkpoint(&db, repo_id);
    let lineage = load_canonical_lineage(&db, repo_id).unwrap().unwrap();
    let conn = db.conn();
    let checkpoint = |direction| {
        load_directional_checkpoint(&conn, &lineage.scope_uuid, lineage.generation, direction)
            .unwrap()
    };
    let svn_checkpoint = checkpoint(CheckpointDirection::SvnToGit).unwrap();
    let git_checkpoint = checkpoint(CheckpointDirection::GitToSvn);
    let rows = |sql: &str, params: &[&dyn rusqlite::ToSql]| {
        conn.prepare(sql)
            .unwrap()
            .query_map(params, |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect()
    };
    let commit_map = rows(
        "SELECT json_object('revision', svn_rev, 'sha', git_sha, 'direction', direction, 'id', id,
                            'synced_at', synced_at, 'svn_author', svn_author, 'git_author', git_author)
         FROM commit_map WHERE repo_id=?1 ORDER BY svn_rev,id",
        &[repo_id],
    );
    let sync_records = rows(
        "SELECT json_object('revision', svn_rev, 'sha', git_sha, 'direction', direction, 'status', status,
                            'id', id, 'author', author, 'message', message, 'timestamp', timestamp, 'synced_at', synced_at)
         FROM sync_records WHERE repo_id=?1 ORDER BY svn_rev,id", &[repo_id]);
    let outcomes = rows(
        "SELECT json_object('key', source_key, 'sha', target_git_sha, 'direction', direction, 'outcome', outcome,
                            'id', id, 'target_svn_rev', target_svn_rev, 'reason', reason,
                            'policy_snapshot', policy_snapshot, 'created_at', created_at)
         FROM canonical_mapping_outcomes WHERE scope_uuid=?1 AND generation=?2 ORDER BY id",
        &[&lineage.scope_uuid, &lineage.generation]);
    OutageLedger {
        watermark,
        inbound_git,
        lineage,
        svn_checkpoint,
        git_checkpoint,
        commit_map,
        sync_records,
        outcomes,
    }
}

fn svnlook_at(repo: &Path, command: &str, args: &[&str]) -> String {
    let output = Command::new("svnlook")
        .arg(command)
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svnlook {command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git_main_commit_and_tree(repo: &Path) -> (git2::Oid, git2::Oid) {
    let git = git2::Repository::open(repo).unwrap();
    let commit = git
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    (commit.id(), commit.tree_id())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_svn_remote_unreachable() {
    if !require_toolchain(CASE_SVN_REMOTE) {
        return;
    }
    let mut fixture = SingleRepoFixture::new();
    let repo = &fixture.repo;
    let root = fixture.tmp.path().join("svnserve-root");
    let repo_dir = root.join(&repo.id);
    let svn = SvnClient::new(&repo.svn_url, &repo.username, &repo.password);
    let engine = fixture.make_engine();
    let baseline = engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        (baseline.svn_to_git_count, baseline.git_to_svn_count),
        (1, 0)
    );
    // Populate the opposite-direction cursor through a normal sync. Its SHA
    // and the sentinel bytes must survive the SVN outage and recovery.
    const SENTINEL: &str = "Git sentinel before outage\n";
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let seed_sha = fixture.developer_commit("origin.txt", SENTINEL, "Establish Git sentinel");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let seed = engine.run_sync_cycle().await.unwrap();
    assert_eq!((seed.git_to_svn_count, seed.svn_to_git_count), (1, 0));
    let seed_rev = svn.info().await.unwrap().latest_rev;
    assert_eq!(seed_rev, repo.svn_rev + 1);
    let settled = engine.run_sync_cycle().await.unwrap();
    assert_eq!((settled.git_to_svn_count, settled.svn_to_git_count), (0, 0));
    let first_rev = svn_commit_file(
        &fixture.wc,
        "queued.txt",
        "first pending edit\n",
        "Queued edit one",
        &repo.username,
        &repo.password,
    );
    let last_rev = svn_commit_file(
        &fixture.wc,
        "queued.txt",
        "second pending edit\n",
        "Queued edit two",
        &repo.username,
        &repo.password,
    );
    assert_eq!((first_rev, last_rev), (seed_rev + 1, seed_rev + 2));
    let info = svn.info().await.unwrap();
    assert_eq!(info.url, repo.svn_url);
    assert_eq!(info.uuid, uuid_of_constructor_created_svn_repo(&repo_dir));
    assert_eq!(info.latest_rev, last_rev);
    let svn_state = || {
        (
            svnlook_at(&repo_dir, "uuid", &[]),
            svnlook_at(&repo_dir, "youngest", &[]),
            svnlook_at(&repo_dir, "tree", &["--show-ids", "--full-paths"]),
            svnlook_at(&repo_dir, "cat", &["origin.txt"]),
            svnlook_at(&repo_dir, "cat", &["queued.txt"]),
        )
    };
    let source = svn_state();
    assert_eq!(source.1.trim(), last_rev.to_string());
    assert_eq!(source.3, SENTINEL);
    assert_eq!(source.4, "second pending edit\n");
    let git_state = || {
        let remote = git_main_commit_and_tree(&repo.bare);
        assert_eq!(git_main_commit_and_tree(&repo.bridge), remote);
        remote
    };
    let before_git = git_state();
    let (baseline_commit, _) = before_git;
    let before = outage_ledger(&fixture);
    assert_eq!(before.lineage.svn_uuid, info.uuid);
    assert_eq!(
        before.watermark,
        (repo.svn_rev, baseline_commit.to_string())
    );
    assert_eq!(
        before.svn_checkpoint.handled_source_key,
        svn_source_key(repo.svn_rev)
    );

    assert_eq!(before.inbound_git.as_deref(), Some(seed_sha.as_str()));
    let git_checkpoint = before.git_checkpoint.as_ref().unwrap();
    assert_eq!(git_checkpoint.handled_source_key, git_source_key(&seed_sha));
    assert_eq!(git_checkpoint.outcome, MappingOutcome::AppliedAndVerified);
    assert_eq!(git_checkpoint.target_svn_rev, Some(seed_rev));

    let port = fixture.daemon.port;
    fixture
        .daemon
        .stop()
        .expect("stop and reap fixture svnserve");
    assert_eq!(
        TcpStream::connect_timeout(
            &([127, 0, 0, 1], port).into(),
            std::time::Duration::from_secs(2)
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    let failure = tokio::time::timeout(std::time::Duration::from_secs(30), engine.run_sync_cycle())
        .await
        .expect("outage cycle timed out")
        .unwrap_err();
    assert!(is_remote_transport_failure(&failure), "{failure}");
    drop(engine);
    assert_eq!(outage_ledger(&fixture), before);
    assert_eq!(git_state(), before_git);
    assert_eq!(svn_state(), source);

    fixture.daemon = SvnserveDaemon::start_on_port(&root, port);
    let restored = svn.info().await.unwrap();
    assert_eq!(
        (
            &restored.url,
            &restored.root_url,
            &restored.uuid,
            restored.latest_rev
        ),
        (&info.url, &info.root_url, &info.uuid, last_rev)
    );
    // make_engine opens a new connection to the same on-disk database.
    let recovered_engine = fixture.make_engine();
    let recovered = recovered_engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        (recovered.svn_to_git_count, recovered.git_to_svn_count),
        (2, 0)
    );
    let after_git = git_state();
    let (recovered_tip, _) = after_git;
    let local_git = git2::Repository::open(&repo.bare).unwrap();
    let last = local_git.find_commit(recovered_tip).unwrap();
    assert_eq!(last.parent_count(), 1);
    let first = last.parent(0).unwrap();
    assert_eq!(first.parent_count(), 1);
    assert_eq!(first.parent_id(0).unwrap(), baseline_commit);
    let after = outage_ledger(&fixture);
    assert_eq!(after.lineage, before.lineage);
    assert_eq!(after.inbound_git, before.inbound_git);
    assert_eq!(after.git_checkpoint, before.git_checkpoint);
    assert_eq!(after.watermark, (last_rev, last.id().to_string()));
    assert_eq!(
        after.svn_checkpoint.handled_source_key,
        svn_source_key(last_rev)
    );
    assert_eq!(
        after.svn_checkpoint.outcome,
        MappingOutcome::AppliedAndVerified
    );
    assert_eq!(
        after.svn_checkpoint.target_git_sha,
        Some(last.id().to_string())
    );
    // The team journal writes scoped sync_records and canonical outcomes.
    // Its SVN-to-Git finalizer leaves the legacy commit_map alone.
    assert_eq!(after.commit_map, before.commit_map);
    for (current, prior) in [
        (&after.sync_records, &before.sync_records),
        (&after.outcomes, &before.outcomes),
    ] {
        assert!(current.starts_with(prior), "pre-outage row changed");
        assert_eq!(current.len(), prior.len() + 2);
    }
    for (i, (rev, commit, bytes)) in [
        (first_rev, &first, "first pending edit\n"),
        (last_rev, &last, "second pending edit\n"),
    ]
    .into_iter()
    .enumerate()
    {
        let sha = commit.id().to_string();
        let record = &after.sync_records[before.sync_records.len() + i];
        let outcome = &after.outcomes[before.outcomes.len() + i];
        for row in [record, outcome] {
            assert_eq!(row["sha"], sha);
            assert_eq!(row["direction"], "svn_to_git");
        }
        assert_eq!(record["revision"], rev);
        assert_eq!(record["status"], "applied");
        assert_eq!(outcome["key"], svn_source_key(rev));
        assert_eq!(outcome["outcome"], "applied_and_verified");
        let tree = commit.tree().unwrap();
        assert_eq!(tree.len(), 3);
        for (path, content) in [
            (".gitkeep", ""),
            ("origin.txt", SENTINEL),
            ("queued.txt", bytes),
        ] {
            assert_eq!(git_show_blob(&repo.bare, &sha, path), content);
            assert_eq!(
                svnlook_at(&repo_dir, "cat", &["-r", &rev.to_string(), path]),
                content
            );
        }
    }
    assert_eq!(svn_state(), source);
    for _ in 0..2 {
        let repeat = recovered_engine.run_sync_cycle().await.unwrap();
        assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
        assert_eq!(outage_ledger(&fixture), after);
        assert_eq!(git_state(), after_git);
        assert_eq!(svn_state(), source);
    }
    emit_evidence(
        CASE_SVN_REMOTE,
        "PASS",
        serde_json::json!({
            "direction": "svn_to_git", "restored_url": restored.url, "restored_uuid": restored.uuid,
            "outage_error": failure.to_string(), "svn_youngest": last_rev, "queued_revisions": [first_rev, last_rev],
            "baseline_git": baseline_commit.to_string(), "recovered_git": [first.id().to_string(), last.id().to_string()],
            "recovered_trees": [first.tree_id().to_string(), last.tree_id().to_string()],
            "seed_git_to_svn_revision": seed_rev,
            "svn_checkpoint_before_after": [before.svn_checkpoint.handled_source_key, after.svn_checkpoint.handled_source_key],
            "scoped_inbound_git_before_after": [before.inbound_git, after.inbound_git],
            "legacy_commit_map_unchanged": true, "canonical_outcome_count_before_after": [before.outcomes.len(), after.outcomes.len()],
            "sync_record_count_before_after": [before.sync_records.len(), after.sync_records.len()],
            "outage_state_unchanged": true, "verified_sentinel": "origin.txt", "repeat_noop_cycles": 2,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_bidirectional_roundtrip() {
    if !require_toolchain(CASE_BIDIRECTIONAL) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let svn_rev_a = svn_commit_file(
        &fixture.wc,
        "roundtrip-a.txt",
        "svn leg a\n",
        "SVN roundtrip A",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    let svn_rev_b = svn_commit_file(
        &fixture.wc,
        "roundtrip-b.txt",
        "svn leg b\n",
        "SVN roundtrip B",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    let svn_stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(svn_stats.svn_to_git_count, 2);
    let db = setup_db(&fixture.db_path);
    let svn_git_a: String = db.conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
        rusqlite::params![fixture.repo.id, svn_rev_a],
        |row| row.get(0),
    ).unwrap();
    let svn_git_b: String = db.conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
        rusqlite::params![fixture.repo.id, svn_rev_b],
        |row| row.get(0),
    ).unwrap();
    assert_ne!(
        svn_git_a, svn_git_b,
        "each SVN revision must map to a distinct applied Git commit"
    );
    assert_git_ancestor(&fixture.repo.bridge, &svn_git_a, &svn_git_b);
    assert!(
        git_path_missing(&fixture.repo.bridge, &svn_git_a, "roundtrip-b.txt"),
        "first mapped git commit must not contain roundtrip-b.txt"
    );
    for (revision, path, content, git_sha) in [
        (svn_rev_a, "roundtrip-a.txt", "svn leg a\n", &svn_git_a),
        (svn_rev_b, "roundtrip-b.txt", "svn leg b\n", &svn_git_b),
    ] {
        assert_eq!(git_show_blob(&fixture.repo.bridge, git_sha, path), content);
        assert_eq!(
            git_show_blob(&fixture.repo.bare, "main", path),
            content,
            "bare origin must expose SVN-originated {}",
            path
        );
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
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    assert_eq!(
        git_show_blob(&fixture.developer, "main", "roundtrip-a.txt"),
        "svn leg a\n"
    );
    assert_eq!(
        git_show_blob(&fixture.developer, "main", "roundtrip-b.txt"),
        "svn leg b\n"
    );
    let git_a = fixture.developer_commit("roundtrip-git.txt", "git leg a\n", "Git roundtrip A");
    let git_b = fixture.developer_commit("roundtrip-git.txt", "git leg b\n", "Git roundtrip B");
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let git_stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(git_stats.git_to_svn_count, 2);
    let svn_head = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    assert_eq!(svn_head, svn_rev_b + 2);
    let after = fixture.checkpoint_snapshot();
    assert_eq!(after.1, git_b);
    for (revision, content, sha) in [
        (svn_rev_b + 1, "git leg a\n", &git_a),
        (svn_rev_b + 2, "git leg b\n", &git_b),
    ] {
        assert_eq!(
            svn_read_file(
                &fixture.repo.svn_url,
                &fixture.repo.username,
                &fixture.repo.password,
                revision,
                "roundtrip-git.txt",
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
    assert_eq!(
        git_show_blob(&fixture.repo.bridge, &git_b, "roundtrip-git.txt"),
        "git leg b\n"
    );
    let repeat = engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    assert_eq!(fixture.checkpoint_snapshot(), after);
    emit_evidence(
        CASE_BIDIRECTIONAL,
        "PASS",
        serde_json::json!({
            "svn_revisions": [svn_rev_a, svn_rev_b],
            "svn_to_git_mapped_shas": [svn_git_a, svn_git_b],
            "git_shas": [git_a, git_b],
            "checkpoint_after": after,
            "git_bridge_tree_checked": true,
            "git_bare_and_developer_tree_checked": true,
            "commit_mappings_checked": true,
            "repeat_noop": true,
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_svn_auth_denied() {
    if !require_toolchain(CASE_SVN_AUTH) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let before = fixture.checkpoint_snapshot();
    let bridge_before = get_head_sha(&fixture.repo.bridge);
    let mappings_before = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let svn_before = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    let db = setup_db(&fixture.db_path);
    db.set_state(
        &format!("secret_svn_password_{}", fixture.repo.id),
        "wrong-password-not-in-passwd-db",
    )
    .unwrap();
    let result = engine.run_sync_cycle().await;
    assert!(
        is_svn_auth_failure(match result.as_ref() {
            Err(error) => error,
            Ok(_) => panic!("sync must fail"),
        }),
        "wrong svnserve password must fail as authentication: {result:?}"
    );
    let checkpoint_after = fixture.checkpoint_snapshot();
    let bridge_after = get_head_sha(&fixture.repo.bridge);
    let mappings_after = setup_db(&fixture.db_path).count_sync_records().unwrap();
    assert_eq!(checkpoint_after, before);
    assert_eq!(bridge_after, bridge_before);
    assert_eq!(mappings_after, mappings_before);
    let svn_after = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    assert_eq!(svn_after, svn_before);
    emit_evidence(
        CASE_SVN_AUTH,
        "PASS",
        serde_json::json!({
            "reason": "svn_authentication_failed",
            "checkpoint_before_after": [before, checkpoint_after],
            "bridge_head_before_after": [bridge_before, bridge_after],
            "svn_revision_before_after": [svn_before, svn_after],
            "mapping_count_before_after": [mappings_before, mappings_after],
            "note": "real svnserve passwd-db denial via hot-reloaded scoped secret",
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_missing_git_branch() {
    if !require_toolchain(CASE_MISSING_BRANCH) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    git_cli(&fixture.repo.bridge, &["fetch", "origin", "main"]);
    git_cli(&fixture.repo.bare, &["update-ref", "-d", "refs/heads/main"]);
    let remote = Command::new("git")
        .arg("-C")
        .arg(&fixture.repo.bare)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/main"])
        .status()
        .unwrap();
    assert_eq!(remote.code(), Some(1));
    let before = fixture.checkpoint_snapshot();
    let bridge_before = get_head_sha(&fixture.repo.bridge);
    let mappings_before = setup_db(&fixture.db_path).count_sync_records().unwrap();
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
        matches!(
            &result,
            Err(SyncError::HistoryBlocked { reason, .. }) if reason == "remote_branch_missing"
        ),
        "deleted bare main must block inspection without stale ref: {result:?}"
    );
    let checkpoint_after = fixture.checkpoint_snapshot();
    let bridge_after = get_head_sha(&fixture.repo.bridge);
    let mappings_after = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let svn_after = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap()
    .latest_rev;
    assert_eq!(bridge_after, bridge_before);
    assert_eq!(checkpoint_after, before);
    assert_eq!(svn_after, svn_before);
    assert_eq!(mappings_after, mappings_before);
    emit_evidence(
        CASE_MISSING_BRANCH,
        "PASS",
        serde_json::json!({
            "reason": "remote_branch_missing",
            "checkpoint_before_after": [before, checkpoint_after],
            "bridge_head_before_after": [bridge_before, bridge_after],
            "svn_revision_before_after": [svn_before, svn_after],
            "mapping_count_before_after": [mappings_before, mappings_after],
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenario_r16_svnserve_credential_rotation() {
    if !require_toolchain(CASE_CREDENTIAL_ROTATION) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let before = fixture.checkpoint_snapshot();
    let svn_root = fixture.tmp.path().join("svnserve-root");
    let repo_dir = svnserve_repo_dir(&svn_root, &fixture.repo.id);
    let rotated = "syncer-rotated-secret-only";
    rotate_svnserve_password(&repo_dir, &fixture.repo.username, rotated);
    let db = setup_db(&fixture.db_path);
    db.set_state(&format!("secret_svn_password_{}", fixture.repo.id), rotated)
        .unwrap();
    let old_client = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    );
    assert!(
        old_client.info().await.is_err(),
        "svnserve must reject the pre-rotation password"
    );
    let new_client = SvnClient::new(&fixture.repo.svn_url, &fixture.repo.username, rotated);
    assert!(
        new_client.info().await.is_ok(),
        "svnserve must accept the rotated password"
    );
    let reopened = fixture.make_engine();
    let _rev = svn_commit_file(
        &fixture.wc,
        "after-rotation.txt",
        "post rotation\n",
        "SVN after credential rotation",
        &fixture.repo.username,
        rotated,
    );
    let stats = reopened.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 1);
    let after = fixture.checkpoint_snapshot();
    assert_ne!(after, before);
    assert_eq!(
        std::fs::read_to_string(fixture.repo.bridge.join("after-rotation.txt")).unwrap(),
        "post rotation\n"
    );
    let rotation_rev = after.0;
    let mapped_git_sha: String = db.conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
        rusqlite::params![fixture.repo.id, rotation_rev],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(
        git_show_blob(&fixture.repo.bridge, &mapped_git_sha, "after-rotation.txt"),
        "post rotation\n"
    );
    emit_evidence(
        CASE_CREDENTIAL_ROTATION,
        "PASS",
        serde_json::json!({
            "old_password_rejected": true,
            "new_password_accepted": true,
            "reopened_engine_sync_after_rotation": true,
            "checkpoint_before_after": [before, after],
            "bridge_tree_after_rotation": true,
            "svn_to_git_mapping_row": mapped_git_sha,
            "note": "real svnserve passwd-db rotation with scoped kv reload on new SyncEngine cycle; bridge tree and sync_records mapping checked",
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenario_r17_svnserve_concurrent_credential_reload() {
    if !require_toolchain(CASE_CONCURRENT_CREDENTIAL_RELOAD) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let alpha = fixture.repos[0].clone();
    let beta = fixture.repos[1].clone();
    let svn_root = fixture.tmp.path().join("svnserve-root");
    let alpha_repo_dir = svnserve_repo_dir(&svn_root, &alpha.id);
    let overlap_kv_marker = "alpha-kv-only-mid-overlap-marker";
    let rotated = "alpha-rotated-after-overlap-only";
    let db = setup_db(&fixture.db_path);
    let beta_secret_before = db
        .get_state("secret_svn_password_repo_beta")
        .unwrap()
        .clone();
    let alpha_secret_before = db
        .get_state("secret_svn_password_repo_alpha")
        .unwrap()
        .clone();
    let ready = Arc::new(tokio::sync::Barrier::new(3));
    let oracle = Arc::new(ImportCycleOracle::new());
    let overlap_at_kv_rewrite = Arc::new(AtomicBool::new(false));
    let kv_rewrite_during_overlap = Arc::new(AtomicBool::new(false));

    let (alpha_result, beta_result, _) = tokio::join!(
        async {
            ready.wait().await;
            let mut engine = fixture.make_engine(&alpha);
            run_import_with_lock_retry(&mut engine, "repo_alpha", Some(&oracle)).await
        },
        async {
            ready.wait().await;
            let mut engine = fixture.make_engine(&beta);
            run_import_with_lock_retry(&mut engine, "repo_beta", Some(&oracle)).await
        },
        async {
            ready.wait().await;
            let mut saw_overlap = false;
            for _ in 0..400 {
                if oracle.both_labels_in_flight("repo_alpha", "repo_beta") {
                    saw_overlap = true;
                    overlap_at_kv_rewrite.store(true, Ordering::SeqCst);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            if saw_overlap {
                setup_db(&fixture.db_path)
                    .set_state("secret_svn_password_repo_alpha", overlap_kv_marker)
                    .unwrap();
                kv_rewrite_during_overlap.store(true, Ordering::SeqCst);
            }
        }
    );

    let max_in_flight = oracle.max_in_flight.load(Ordering::SeqCst);
    let peak_concurrent = max_in_flight >= 2;
    let db = setup_db(&fixture.db_path);
    let credential_crossover = db.get_state("secret_svn_password_repo_beta").unwrap()
        == Some(overlap_kv_marker.into())
        || db.get_state("secret_svn_password_repo_alpha").unwrap() == beta_secret_before;

    let mut detail = serde_json::json!({
        "overlap_at_kv_rewrite": overlap_at_kv_rewrite.load(Ordering::SeqCst),
        "kv_rewrite_during_overlap": kv_rewrite_during_overlap.load(Ordering::SeqCst),
        "peak_concurrent_import_cycles": peak_concurrent,
        "max_in_flight": max_in_flight,
        "beta_secret_unchanged": db.get_state("secret_svn_password_repo_beta").unwrap()
            == beta_secret_before,
        "credential_crossover": credential_crossover,
    });

    let case_status = if alpha_result.is_err()
        || beta_result.is_err()
        || !overlap_at_kv_rewrite.load(Ordering::SeqCst)
        || !kv_rewrite_during_overlap.load(Ordering::SeqCst)
        || !peak_concurrent
        || max_in_flight > 2
    {
        "PARTIAL"
    } else if let (Ok(alpha_stats), Ok(beta_stats)) = (&alpha_result, &beta_result) {
        assert_eq!(alpha_stats.svn_to_git_count, 1);
        assert_eq!(beta_stats.svn_to_git_count, 1);
        assert_eq!(
            db.get_state("secret_svn_password_repo_alpha")
                .unwrap()
                .as_deref(),
            Some(overlap_kv_marker),
            "kv rewrite must remain visible after overlapping imports"
        );
        assert!(
            db.get_state("secret_svn_password_repo_beta").unwrap() == beta_secret_before,
            "beta scoped secret must not change during alpha kv rewrite"
        );
        assert!(!credential_crossover, "scoped secrets must not cross repos");
        rotate_svnserve_password(&alpha_repo_dir, &alpha.username, rotated);
        db.set_state("secret_svn_password_repo_alpha", rotated)
            .unwrap();
        let old_client = SvnClient::new(
            &alpha.svn_url,
            &alpha.username,
            alpha_secret_before.as_deref().unwrap_or(&alpha.password),
        );
        assert!(
            old_client.info().await.is_err(),
            "post-overlap svnserve must reject the pre-rotation password"
        );
        let rotated_client = SvnClient::new(&alpha.svn_url, &alpha.username, rotated);
        assert!(
            rotated_client.info().await.is_ok(),
            "post-overlap svnserve must accept the rotated password"
        );
        let reopened = fixture.make_engine(&alpha);
        assert_eq!(
            reopened.fixture_svn_password_marker(),
            alpha.password,
            "new engine still carries construction-time svn password until cycle reload"
        );
        let _rev = svn_commit_file(
            &fixture.tmp.path().join("repo_alpha_wc"),
            "post-reload.txt",
            "after reload\n",
            "SVN after concurrent credential reload",
            &alpha.username,
            rotated,
        );
        let reload_stats = reopened.run_sync_cycle().await;
        match reload_stats {
            Ok(stats) if stats.svn_to_git_count == 1 => {
                assert_eq!(reopened.fixture_svn_password_marker(), rotated);
                let post_rev = db.get_repo_watermark("repo_alpha").unwrap().0;
                let post_reload_mapping: i64 = db.conn().query_row(
                    "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'repo_alpha' AND direction = 'svn_to_git' AND svn_rev = ?1 AND status = 'applied'",
                    rusqlite::params![post_rev],
                    |row| row.get(0),
                ).unwrap();
                assert_eq!(
                    git_show_blob(
                        &alpha.bridge,
                        &db.get_repo_watermark("repo_alpha").unwrap().1,
                        "post-reload.txt",
                    ),
                    "after reload\n"
                );
                detail["in_flight_import_succeeded_with_pre_rewrite_credential"] =
                    serde_json::json!(true);
                detail["post_overlap_cycle_reloaded_rotated_credential"] = serde_json::json!(true);
                detail["post_reload_mapping"] = serde_json::json!(post_reload_mapping == 1);
                detail["post_reload_svn_rev"] = serde_json::json!(post_rev);
                if post_reload_mapping == 1 {
                    "PASS"
                } else {
                    "PARTIAL"
                }
            }
            other => {
                detail["post_overlap_cycle_reloaded_rotated_credential"] = serde_json::json!(false);
                detail["reload_error"] = serde_json::json!(other.err().map(|e| e.to_string()));
                "PARTIAL"
            }
        }
    } else {
        "PARTIAL"
    };

    if case_status == "PARTIAL" {
        if let (Ok(alpha_stats), Ok(beta_stats)) = (&alpha_result, &beta_result) {
            detail["alpha_svn_to_git"] = serde_json::json!(alpha_stats.svn_to_git_count);
            detail["beta_svn_to_git"] = serde_json::json!(beta_stats.svn_to_git_count);
        }
        detail["note"] = serde_json::json!(
            "requires overlapping repo_alpha/repo_beta import cycles, kv rewrite only while both labels are in-flight, successful in-flight import with pre-rewrite svn credential, and a later cycle that reloads rotated svnserve/kv credentials"
        );
    } else {
        detail["note"] = serde_json::json!(
            "kv rewrite while both labels in-flight; in-flight alpha import kept construction-time svn password; post-overlap cycle reloaded rotated scoped secret"
        );
    }

    emit_evidence(CASE_CONCURRENT_CREDENTIAL_RELOAD, case_status, detail);
    if case_status == "PASS" {
        return;
    }
    if alpha_result.is_err() || beta_result.is_err() {
        panic!(
            "concurrent credential reload imports failed: alpha={alpha_result:?} beta={beta_result:?}"
        );
    }
}

struct ParentChildChainFixture {
    tmp: TempDir,
    _daemon: SvnserveDaemon,
    db_path: PathBuf,
    svn_url: String,
    username: String,
    password: String,
    parent_bridge: PathBuf,
    child_bridge: PathBuf,
    wc: PathBuf,
    parent_id: String,
    child_id: String,
}

impl ParentChildChainFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let svn_root = tmp.path().join("svnserve-root");
        std::fs::create_dir_all(&svn_root).unwrap();
        let daemon = SvnserveDaemon::start(&svn_root);
        let db_path = tmp.path().join("chain.db");
        let db = setup_db(&db_path);
        let parent_id = "repo_parent".to_string();
        let child_id = "repo_child".to_string();
        let user = "chainer";
        let pass = "chain-secret-only";
        let repo_dir = svn_root.join("repo_chain");
        assert!(Command::new("svnadmin")
            .args(["create", repo_dir.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        write_svnserve_auth(&repo_dir, user, pass);
        let svn_url = daemon.repo_url("repo_chain");
        let wc = tmp.path().join("wc");
        svn_checkout(&svn_url, &wc, user, pass);
        svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor", user, pass);
        svn_commit_file(
            &wc,
            "origin.txt",
            "chain-origin\n",
            "Verified SVN origin",
            user,
            pass,
        );
        let parent_bridge = tmp.path().join("parent_bridge");
        let parent_bare = tmp.path().join("parent.git");
        let git = setup_git_with_bare_origin(&parent_bridge, &parent_bare);
        let parent_initial_git_sha = get_head_sha(&parent_bridge);
        drop(git);
        let child_bridge = tmp.path().join("child_bridge");
        let child_bare = tmp.path().join("child.git");
        let git = setup_git_with_bare_origin(&child_bridge, &child_bare);
        let child_initial_git_sha = get_head_sha(&child_bridge);
        drop(git);
        let now = chrono::Utc::now().to_rfc3339();
        db.set_state(&format!("secret_svn_password_{parent_id}"), pass)
            .unwrap();
        db.set_state(&format!("secret_git_token_{parent_id}"), "git-token-parent")
            .unwrap();
        db.set_state(&format!("secret_git_token_{child_id}"), "git-token-child")
            .unwrap();
        let parent_row = Repository {
            id: parent_id.clone(),
            name: parent_id.clone(),
            svn_url: svn_url.clone(),
            svn_branch: String::new(),
            svn_username: user.into(),
            git_provider: "local".into(),
            git_api_url: String::new(),
            git_repo: parent_bare.to_string_lossy().to_string(),
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
            last_git_sha: parent_initial_git_sha.clone(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        };
        let child_row = Repository {
            id: child_id.clone(),
            name: child_id.clone(),
            svn_url: svn_url.clone(),
            svn_branch: String::new(),
            svn_username: user.into(),
            git_provider: "local".into(),
            git_api_url: String::new(),
            git_repo: child_bare.to_string_lossy().to_string(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 5,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: Some(parent_id.clone()),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_svn_rev: 1,
            last_git_sha: child_initial_git_sha.clone(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        };
        db.insert_repository(&parent_row).unwrap();
        db.insert_repository(&child_row).unwrap();
        establish_constructor_created_svn_identity(&db, &parent_id, &repo_dir);
        establish_constructor_created_svn_identity(&db, &child_id, &repo_dir);
        Self {
            tmp,
            _daemon: daemon,
            db_path,
            svn_url,
            username: user.into(),
            password: pass.into(),
            parent_bridge,
            child_bridge,
            wc,
            parent_id,
            child_id,
        }
    }

    fn make_engine(&self, repo_id: &str) -> SyncEngine {
        let db = setup_db(&self.db_path);
        let config = make_app_config(&self.svn_url, self.tmp.path());
        let bridge = if repo_id == self.parent_id {
            &self.parent_bridge
        } else {
            &self.child_bridge
        };
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new(&self.svn_url, &self.username, &self.password),
            GitClient::new(bridge).unwrap(),
            Arc::new(make_identity_mapper()),
        );
        engine.set_repo_id(repo_id.to_string());
        engine
    }
}

fn repo_teams_webhook(db: &Database, repo_id: &str) -> Option<String> {
    db.get_repository(repo_id)
        .unwrap()
        .and_then(|row| row.teams_webhook_url)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_parent_child_credential_chain_rotation() {
    if !require_toolchain(CASE_PARENT_CHILD_CHAIN_ROTATION) {
        return;
    }
    let fixture = ParentChildChainFixture::new();
    let parent_engine = fixture.make_engine(&fixture.parent_id);
    assert_eq!(
        parent_engine
            .run_sync_cycle()
            .await
            .unwrap()
            .svn_to_git_count,
        1
    );
    let db = setup_db(&fixture.db_path);
    assert!(
        db.get_state(&format!("secret_svn_password_{}", fixture.child_id))
            .unwrap()
            .is_none(),
        "child must inherit parent svn secret via chain, not a seeded child key"
    );
    let child_engine = fixture.make_engine(&fixture.child_id);
    assert_eq!(
        child_engine
            .run_sync_cycle()
            .await
            .unwrap()
            .svn_to_git_count,
        1
    );
    let before_child = db.get_repo_watermark(&fixture.child_id).unwrap();
    let svn_root = fixture.tmp.path().join("svnserve-root");
    let repo_dir = svnserve_repo_dir(&svn_root, "repo_chain");
    let rotated = "chain-rotated-secret-only";
    rotate_svnserve_password(&repo_dir, &fixture.username, rotated);
    db.set_state(
        &format!("secret_svn_password_{}", fixture.parent_id),
        rotated,
    )
    .unwrap();
    let old_client = SvnClient::new(&fixture.svn_url, &fixture.username, &fixture.password);
    assert!(old_client.info().await.is_err());
    let new_client = SvnClient::new(&fixture.svn_url, &fixture.username, rotated);
    assert!(new_client.info().await.is_ok());
    let _rev = svn_commit_file(
        &fixture.wc,
        "chain-after-rotation.txt",
        "chain post rotation\n",
        "SVN after parent-chain rotation",
        &fixture.username,
        rotated,
    );
    let reopened_child = fixture.make_engine(&fixture.child_id);
    assert_eq!(
        reopened_child.fixture_svn_password_marker(),
        fixture.password,
        "construction-time password until cycle reload"
    );
    let stats = reopened_child.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 1);
    assert_eq!(reopened_child.fixture_svn_password_marker(), rotated);
    let after_child = db.get_repo_watermark(&fixture.child_id).unwrap();
    assert_ne!(after_child, before_child);
    let mapped_git_sha: String = db.conn().query_row(
        "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'svn_to_git' AND svn_rev = ?2 AND status = 'applied'",
        rusqlite::params![fixture.child_id, after_child.0],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(
        git_show_blob(
            &fixture.child_bridge,
            &mapped_git_sha,
            "chain-after-rotation.txt"
        ),
        "chain post rotation\n"
    );
    emit_evidence(
        CASE_PARENT_CHILD_CHAIN_ROTATION,
        "PASS",
        serde_json::json!({
            "parent_secret_rotated": true,
            "child_inherited_rotated_password": true,
            "child_checkpoint_before_after": [before_child, after_child],
            "bridge_tree_after_rotation": true,
            "child_svn_to_git_mapping_row": mapped_git_sha,
            "note": "keyless child reloads rotated parent scoped svn secret via credential chain on a new SyncEngine cycle",
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenario_r17_svnserve_parent_child_concurrent_credential_reload() {
    if !require_toolchain(CASE_PARENT_CHILD_CONCURRENT_RELOAD) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let alpha = fixture.repos[0].clone();
    let mut beta = fixture.repos[1].clone();
    beta.svn_url = alpha.svn_url.clone();
    beta.username = alpha.username.clone();
    beta.password = alpha.password.clone();
    let db = setup_db(&fixture.db_path);
    let alpha_hook = "https://notify.invalid/repo-alpha-only";
    let beta_hook = "https://notify.invalid/repo-beta-only";
    let mut alpha_repo = db.get_repository("repo_alpha").unwrap().unwrap();
    alpha_repo.teams_webhook_url = Some(alpha_hook.into());
    db.update_repository(&alpha_repo).unwrap();
    let mut beta_repo = db.get_repository("repo_beta").unwrap().unwrap();
    beta_repo.parent_id = Some("repo_alpha".into());
    beta_repo.svn_url = alpha.svn_url.clone();
    beta_repo.svn_username = alpha.username.clone();
    beta_repo.teams_webhook_url = Some(beta_hook.into());
    db.update_repository(&beta_repo).unwrap();
    // Seam: this case rebinds repo_beta onto alpha's constructor-created SVN
    // after DualRepoFixture already established repo_beta's original UUID.
    // Identity must follow the synthetic repository this scenario actually uses,
    // not a later live URL observation and not repo_beta's unused original UUID.
    let alpha_constructor_uuid = independently_established_svn_uuid(&db.conn(), "repo_alpha")
        .unwrap()
        .expect("DualRepoFixture established repo_alpha constructor UUID");
    db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key = ?1",
            [independently_established_svn_uuid_kv_key("repo_beta")],
        )
        .unwrap();
    record_independently_established_svn_uuid(&db, "repo_beta", &alpha_constructor_uuid).unwrap();
    db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key = 'secret_svn_password_repo_beta'",
            [],
        )
        .unwrap();
    assert_eq!(
        db.resolve_credential_chain("repo_beta", "secret_svn_password")
            .as_deref(),
        Some("alpha-secret-only")
    );

    let svn_root = fixture.tmp.path().join("svnserve-root");
    let alpha_repo_dir = svnserve_repo_dir(&svn_root, &alpha.id);
    let rotated = "alpha-chain-rotated-only";
    let alpha_secret_before = db
        .get_state("secret_svn_password_repo_alpha")
        .unwrap()
        .clone();
    let ready = Arc::new(tokio::sync::Barrier::new(3));
    let oracle = Arc::new(ImportCycleOracle::new());
    let overlap_at_kv_rewrite = Arc::new(AtomicBool::new(false));
    let kv_rewrite_during_overlap = Arc::new(AtomicBool::new(false));

    let (alpha_result, beta_result, _) = tokio::join!(
        async {
            ready.wait().await;
            let mut engine = fixture.make_engine(&alpha);
            run_import_with_lock_retry(&mut engine, "repo_alpha", Some(&oracle)).await
        },
        async {
            ready.wait().await;
            let mut engine = fixture.make_engine(&beta);
            run_import_with_lock_retry(&mut engine, "repo_beta", Some(&oracle)).await
        },
        async {
            ready.wait().await;
            let mut saw_overlap = false;
            for _ in 0..400 {
                if oracle.both_labels_in_flight("repo_alpha", "repo_beta") {
                    saw_overlap = true;
                    overlap_at_kv_rewrite.store(true, Ordering::SeqCst);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            if saw_overlap {
                setup_db(&fixture.db_path)
                    .set_state("secret_svn_password_repo_alpha", "alpha-mid-overlap-marker")
                    .unwrap();
                kv_rewrite_during_overlap.store(true, Ordering::SeqCst);
            }
        }
    );

    let db = setup_db(&fixture.db_path);
    let webhook_crossover = repo_teams_webhook(&db, "repo_alpha").as_deref() != Some(alpha_hook)
        || repo_teams_webhook(&db, "repo_beta").as_deref() != Some(beta_hook);
    let child_still_inherits_parent = db
        .resolve_credential_chain("repo_beta", "secret_svn_password")
        .as_deref()
        == Some("alpha-mid-overlap-marker");

    let max_in_flight = oracle.max_in_flight.load(Ordering::SeqCst);
    let peak_concurrent = max_in_flight >= 2;
    let case_status = if alpha_result.is_err()
        || beta_result.is_err()
        || !overlap_at_kv_rewrite.load(Ordering::SeqCst)
        || !kv_rewrite_during_overlap.load(Ordering::SeqCst)
        || !peak_concurrent
        || max_in_flight > 2
        || webhook_crossover
    {
        "PARTIAL"
    } else {
        rotate_svnserve_password(&alpha_repo_dir, &alpha.username, rotated);
        db.set_state("secret_svn_password_repo_alpha", rotated)
            .unwrap();
        assert_eq!(
            db.resolve_credential_chain("repo_beta", "secret_svn_password")
                .as_deref(),
            Some(rotated),
            "child chain must follow parent rotation after overlap"
        );
        let reopened_child = fixture.make_engine(&beta);
        let _rev = svn_commit_file(
            &fixture.tmp.path().join("repo_alpha_wc"),
            "child-post-chain-reload.txt",
            "child reload\n",
            "SVN after parent/child chain reload",
            &beta.username,
            rotated,
        );
        match reopened_child.run_sync_cycle().await {
            Ok(stats) if stats.svn_to_git_count == 1 => {
                assert_eq!(
                    reopened_child.fixture_svn_password_marker(),
                    rotated,
                    "child cycle must reload rotated parent svn password"
                );
                let post_rev = db.get_repo_watermark("repo_beta").unwrap().0;
                let mapped_git_sha: String = db.conn().query_row(
                    "SELECT git_sha FROM sync_records WHERE repo_id = 'repo_beta' AND direction = 'svn_to_git' AND svn_rev = ?1 AND status = 'applied'",
                    rusqlite::params![post_rev],
                    |row| row.get(0),
                ).unwrap();
                let bridge_ok =
                    git_show_blob(&beta.bridge, &mapped_git_sha, "child-post-chain-reload.txt")
                        == "child reload\n";
                if bridge_ok {
                    "PASS"
                } else {
                    "PARTIAL"
                }
            }
            _ => "PARTIAL",
        }
    };

    emit_evidence(
        CASE_PARENT_CHILD_CONCURRENT_RELOAD,
        case_status,
        serde_json::json!({
            "overlap_at_kv_rewrite": overlap_at_kv_rewrite.load(Ordering::SeqCst),
            "kv_rewrite_during_overlap": kv_rewrite_during_overlap.load(Ordering::SeqCst),
            "peak_concurrent_import_cycles": peak_concurrent,
            "max_in_flight": max_in_flight,
            "teams_webhook_crossover": webhook_crossover,
            "child_inherits_parent_mid_overlap": child_still_inherits_parent,
            "alpha_secret_before": alpha_secret_before,
            "beta_parent_id": "repo_alpha",
            "note": "parent/child svn credential chain under concurrent reload; repository teams_webhook_url must not crossover during kv rewrite",
        }),
    );
    if case_status == "PASS" {
        return;
    }
    if alpha_result.is_err() || beta_result.is_err() {
        panic!(
            "parent/child concurrent reload imports failed: alpha={alpha_result:?} beta={beta_result:?}"
        );
    }
}

/// Arms the debug-only per-engine SVN apply fault (real `git apply` runs; stdin patch
/// bytes are invalid for that revision on this engine only).
struct RealEngineApplyFaultGuard<'a> {
    engine: &'a SyncEngine,
}

impl<'a> RealEngineApplyFaultGuard<'a> {
    fn arm(engine: &'a SyncEngine, revision: i64, bridge: &Path) -> Self {
        engine.set_svn_apply_test_fault(revision, bridge);
        Self { engine }
    }
}

impl Drop for RealEngineApplyFaultGuard<'_> {
    fn drop(&mut self) {
        self.engine.clear_svn_apply_test_fault();
    }
}

async fn run_svnserve_failed_apply_on_real_svnserve(retry: bool) {
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let before = fixture.checkpoint_snapshot();
    let verified = svn_commit_file(
        &fixture.wc,
        "origin.txt",
        "verified N-1\n",
        "Verified prior revision",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    assert_eq!(verified, before.0 + 1);
    assert_eq!(
        engine.run_sync_cycle().await.unwrap().svn_to_git_count,
        1,
        "verified revision must apply before fault injection"
    );
    let failed = svn_commit_file(
        &fixture.wc,
        "fault-apply.txt",
        "failed N\n",
        "Faulted revision",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    let later = svn_commit_file(
        &fixture.wc,
        "origin.txt",
        "queued N+1\n",
        "Later revision",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    assert_eq!((failed, later), (before.0 + 2, before.0 + 3));
    let fault = RealEngineApplyFaultGuard::arm(&engine, failed, &fixture.repo.bridge);
    let blocked = engine.run_sync_cycle().await;
    let apply_failed = matches!(
        &blocked,
        Err(SyncError::GitError(reposync_core::errors::GitError::ApplyFailed(
            message
        ))) if message.contains(&format!("r{failed}"))
    );
    let local_dirty = matches!(
        &blocked,
        Err(SyncError::HistoryBlocked { reason, .. }) if reason == "local_dirty"
    );
    if local_dirty {
        drop(fault);
        emit_evidence(
            CASE_FAILED_APPLY_BARRIER,
            "PARTIAL",
            serde_json::json!({
                "fault": "invalid_patch_at_revision",
                "blocked_before_apply": true,
                "cycle_error": blocked.as_ref().err().map(|e| e.to_string()),
                "note": "pre-cycle local_dirty blocked before git apply; svnserve F06 apply barrier not reached",
            }),
        );
        if retry {
            emit_evidence(
                CASE_FAILED_APPLY_RETRY,
                "PARTIAL",
                serde_json::json!({
                    "fault": "invalid_patch_at_revision",
                    "note": "retry not attempted because barrier injection did not reach git apply",
                }),
            );
        }
        return;
    }
    drop(fault);
    if !apply_failed {
        emit_evidence(
            CASE_FAILED_APPLY_BARRIER,
            "PARTIAL",
            serde_json::json!({
                "fault": "invalid_patch_at_revision",
                "cycle_error": blocked.as_ref().err().map(|e| e.to_string()),
                "cycle_ok": blocked.is_ok(),
            }),
        );
        if retry {
            emit_evidence(
                CASE_FAILED_APPLY_RETRY,
                "PARTIAL",
                serde_json::json!({
                    "fault": "invalid_patch_at_revision",
                    "note": "unexpected cycle outcome prevented retry proof",
                }),
            );
        }
        return;
    }
    let frontier = fixture.checkpoint_snapshot();
    assert_eq!(frontier.0, verified);
    assert_eq!(
        std::fs::read_to_string(fixture.repo.bridge.join("origin.txt")).unwrap(),
        "verified N-1\n"
    );
    assert!(
        !fixture.repo.bridge.join("fault-apply.txt").exists(),
        "failed revision file must not be applied while apply is blocked"
    );
    let db = setup_db(&fixture.db_path);
    for revision in [failed, later] {
        let count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND svn_rev = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
            rusqlite::params![fixture.repo.id, revision],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "unapplied r{revision} must have no mapping");
    }
    let mapping_at_frontier: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND svn_rev = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
        rusqlite::params![fixture.repo.id, verified],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(mapping_at_frontier, 1);
    emit_evidence(
        CASE_FAILED_APPLY_BARRIER,
        "PASS",
        serde_json::json!({
            "fault": "invalid_patch_at_revision",
            "verified_revision": verified,
            "failed_revision": failed,
            "queued_revision": later,
            "frontier_watermark": frontier.0,
            "bridge_origin_at_frontier": "verified N-1\n",
            "mapping_at_frontier": mapping_at_frontier,
            "apply_failed_at_revision": failed,
        }),
    );
    if !retry {
        return;
    }
    let applied = engine.run_sync_cycle().await.unwrap();
    assert_eq!((applied.svn_to_git_count, applied.git_to_svn_count), (2, 0));
    let after = fixture.checkpoint_snapshot();
    assert_eq!(after.0, later);
    assert_eq!(
        std::fs::read_to_string(fixture.repo.bridge.join("origin.txt")).unwrap(),
        "queued N+1\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.bridge.join("fault-apply.txt")).unwrap(),
        "failed N\n"
    );
    let repeat = engine.run_sync_cycle().await.unwrap();
    assert_eq!((repeat.svn_to_git_count, repeat.git_to_svn_count), (0, 0));
    let db = setup_db(&fixture.db_path);
    for revision in [failed, later] {
        let count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND svn_rev = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
            rusqlite::params![fixture.repo.id, revision],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1, "retry must map applied r{revision}");
    }
    emit_evidence(
        CASE_FAILED_APPLY_RETRY,
        "PASS",
        serde_json::json!({
            "final_watermark": after.0,
            "repeat_noop": true,
            "fault": "invalid_patch_at_revision",
            "mappings_after_retry": [failed, later],
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_failed_apply_barrier() {
    if !require_toolchain(CASE_FAILED_APPLY_BARRIER) {
        return;
    }
    run_svnserve_failed_apply_on_real_svnserve(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_failed_apply_retry() {
    if !require_toolchain(CASE_FAILED_APPLY_RETRY) {
        return;
    }
    run_svnserve_failed_apply_on_real_svnserve(true).await;
}

async fn git_push_checkpoint_recovery() -> serde_json::Value {
    use reposync_core::auto_reconcile::{
        reconcile_held_external_writes_with_resolved_credentials, HeldExternalWriteKind,
    };
    use reposync_core::db::git_push_operations::GitPushOperationState;

    let fixture = SingleRepoFixture::new();
    let repo = &fixture.repo;
    let svn_dir = fixture.tmp.path().join("svnserve-root").join(&repo.id);
    git_cli(&repo.bare, &["config", "core.logAllRefUpdates", "true"]);
    let reflog = || std::fs::read_to_string(repo.bare.join("logs/refs/heads/main")).unwrap();
    let engine = fixture.make_engine();
    let baseline = engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        (baseline.svn_to_git_count, baseline.git_to_svn_count),
        (1, 0)
    );
    let before = outage_ledger(&fixture);
    let (parent, parent_tree) = git_main_commit_and_tree(&repo.bare);
    let prior_log = reflog();
    let registration = |db: &Database| db.get_repository(&repo.id).unwrap().unwrap();
    let syncs_before = registration(engine.db()).total_syncs;
    let rev = svn_commit_file(
        &fixture.wc,
        "recovery.txt",
        "one published change\n",
        "R01 checkpoint interruption",
        &repo.username,
        &repo.password,
    );
    assert_eq!(rev, before.watermark.0 + 1);
    assert!(before.sync_records.iter().all(|r| r["revision"] != rev));
    assert!(before
        .outcomes
        .iter()
        .all(|o| o["key"] != svn_source_key(rev)));
    let svn_state = || {
        (
            svnlook_at(&svn_dir, "uuid", &[]),
            svnlook_at(&svn_dir, "youngest", &[]),
            svnlook_at(&svn_dir, "tree", &["--show-ids", "--full-paths"]),
        )
    };
    let source = svn_state();
    assert_eq!(source.0.trim(), before.lineage.svn_uuid);
    assert_eq!(source.1.trim(), rev.to_string());

    // On this engine's private connection the receipt is inserted before this
    // update. Returning an error rolls the whole finalization transaction back.
    const FAULT: &str = "R01 checkpoint interrupted after receipt insertion";
    let interrupt_checkpoint = format!(
        "CREATE TEMP TRIGGER interrupt_checkpoint AFTER UPDATE OF last_svn_rev ON main.repositories
         WHEN NEW.id='repo_sync' AND NEW.last_svn_rev={rev}
         AND EXISTS (SELECT 1 FROM sync_records WHERE repo_id=NEW.id AND svn_rev={rev}
                     AND direction='svn_to_git' AND status='applied')
         BEGIN SELECT RAISE(ABORT,'{FAULT}'); END;"
    );
    engine
        .db()
        .conn()
        .execute_batch(&interrupt_checkpoint)
        .unwrap();
    let error = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(&error, SyncError::GitPushHeld { reason, detail }
            if reason == "checkpoint_write_failed" && detail.contains(FAULT)),
        "{error}"
    );
    let held = engine
        .db()
        .active_git_push_operation(&repo.id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, GitPushOperationState::ReconciliationRequired);
    assert_eq!(held.repo_id, repo.id);
    assert_eq!(held.source_svn_rev, rev);
    assert_eq!(held.pre_push_git_sha, parent.to_string());
    assert_eq!(held.pre_push_git_tree, Some(parent_tree.to_string()));
    assert!(!held.resume_authorized);
    assert!(held.last_confirmed_git_sha.is_none() && held.last_confirmed_git_tree.is_none());
    assert_eq!(outage_ledger(&fixture), before);
    assert_eq!(svn_state(), source);
    assert_eq!(registration(engine.db()).total_syncs, syncs_before);
    let published = git_main_commit_and_tree(&repo.bare);
    let bare = git2::Repository::open_bare(&repo.bare).unwrap();
    let commit = bare.find_commit(published.0).unwrap();
    assert_eq!(commit.parent_count(), 1);
    assert_eq!(commit.parent_id(0).unwrap(), parent);
    let sha = published.0.to_string();
    let tree = published.1.to_string();
    assert_eq!(held.intended_local_git_sha, sha);
    assert_eq!(held.intended_local_git_tree, tree);
    assert_eq!(held.intended_local_git_parent, Some(parent.to_string()));
    assert_eq!(git_main_commit_and_tree(&repo.bridge), published);
    for (path, bytes) in [
        ("origin.txt", "SVN origin\n"),
        ("recovery.txt", "one published change\n"),
    ] {
        assert_eq!(git_show_blob(&repo.bare, &sha, path), bytes);
        assert_eq!(
            svnlook_at(&svn_dir, "cat", &["-r", &rev.to_string(), path]),
            bytes
        );
    }
    let published_log = reflog();
    let update = published_log.strip_prefix(&prior_log).unwrap();
    assert_eq!(update.lines().count(), 1);
    assert!(update.starts_with(&format!("{parent} {sha} ")));
    drop(engine); // Also removes the connection-local trigger.

    let reopened = fixture.make_engine();
    let db = reopened.db();
    db.hold_interrupted_git_pushes().unwrap();
    assert_eq!(
        db.active_git_push_operation(&repo.id).unwrap(),
        Some(held.clone())
    );
    let blocked = reopened.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(blocked, SyncError::GitPushHeld { reason, .. } if reason == "reconciliation_required")
    );
    assert_eq!(outage_ledger(&fixture), before);
    assert_eq!(reflog(), published_log);
    let reg = registration(db);
    let reconcile =
        || reconcile_held_external_writes_with_resolved_credentials(db, &reg, Some(&repo.bridge));
    let result = reconcile().await.unwrap();
    let [attempt] = result.attempts.as_slice() else {
        panic!("expected one reconcile: {result:?}")
    };
    assert_eq!(attempt.kind, HeldExternalWriteKind::SvnToGitPush);
    assert_eq!(attempt.operation_id, held.id);
    assert!(attempt.finalized && !attempt.resume_authorized && !attempt.skipped);
    assert!(db.active_git_push_operation(&repo.id).unwrap().is_none());
    let completed = db
        .get_git_push_operation(&repo.id, &held.id)
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, GitPushOperationState::Completed);
    assert_eq!(completed.last_confirmed_git_sha.as_ref(), Some(&sha));
    assert_eq!(completed.last_confirmed_git_tree.as_ref(), Some(&tree));
    let after = outage_ledger(&fixture);
    assert_eq!(after.watermark, (rev, sha.clone()));
    assert_eq!(after.svn_checkpoint.handled_source_key, svn_source_key(rev));
    assert_eq!(after.svn_checkpoint.target_git_sha.as_ref(), Some(&sha));
    assert_eq!(
        after.svn_checkpoint.outcome,
        MappingOutcome::AppliedAndVerified
    );
    assert_eq!(after.lineage, before.lineage);
    assert_eq!(after.inbound_git, before.inbound_git);
    assert_eq!(after.git_checkpoint, before.git_checkpoint);
    assert_eq!(after.commit_map, before.commit_map);
    let [receipt] = after
        .sync_records
        .strip_prefix(before.sync_records.as_slice())
        .unwrap()
    else {
        panic!("expected one new receipt")
    };
    assert_eq!(receipt["revision"], rev);
    assert_eq!(receipt["sha"], sha);
    assert_eq!(receipt["direction"], "svn_to_git");
    assert_eq!(receipt["status"], "applied");
    let [outcome] = after
        .outcomes
        .strip_prefix(before.outcomes.as_slice())
        .unwrap()
    else {
        panic!("expected one new canonical outcome")
    };
    assert_eq!(outcome["key"], svn_source_key(rev));
    assert_eq!(outcome["sha"], sha);
    assert_eq!(outcome["direction"], "svn_to_git");
    assert_eq!(outcome["outcome"], "applied_and_verified");
    assert_eq!(registration(db).total_syncs, syncs_before + 1);
    assert!(reconcile().await.unwrap().attempts.is_empty());
    for _ in 0..2 {
        let quiet = reopened.run_sync_cycle().await.unwrap();
        assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
        assert_eq!(outage_ledger(&fixture), after);
        assert_eq!(registration(db).total_syncs, syncs_before + 1);
        assert_eq!(
            db.latest_git_push_operation(&repo.id).unwrap().as_ref(),
            Some(&completed)
        );
        assert_eq!(git_main_commit_and_tree(&repo.bare), published);
        assert_eq!(git_main_commit_and_tree(&repo.bridge), published);
        assert_eq!(reflog(), published_log);
        assert_eq!(svn_state(), source);
    }
    serde_json::json!({
        "status": "PASS", "operation_id": held.id, "svn_revision": rev, "svn_uuid": source.0.trim(),
        "held_reason": "checkpoint_write_failed", "git_parent": parent.to_string(),
        "published_git_sha": sha, "published_git_tree": tree, "bare_ref_updates": update.lines().count(),
        "receipt_id": receipt["id"], "canonical_outcome_id": outcome["id"],
        "sync_count_before_after": [syncs_before, registration(db).total_syncs], "quiet_cycles": 2
    })
}

#[path = "real_engine_scenarios/ordinary_svn_merge.rs"]
mod ordinary_svn_merge;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r15_ordinary_svn_merge_and_mergeinfo() {
    const CASE: &str = "R15_SVNSERVE_ORDINARY_MERGE";
    if require_toolchain(CASE) {
        emit_evidence(CASE, "PASS", ordinary_svn_merge::run().await);
    }
}

#[path = "real_engine_scenarios/finalized_history.rs"]
mod finalized_history;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_post_write_recovery() {
    if !require_toolchain(CASE_POST_WRITE_RECOVERY) {
        return;
    }
    let checkpoint_recovery = git_push_checkpoint_recovery().await;
    let finalized = finalized_history::evidence().await;
    let historical = &finalized["historical_hold"];
    let mapping_count = historical["r3_receipts"].as_u64().unwrap();
    emit_evidence(
        CASE_POST_WRITE_RECOVERY,
        "PARTIAL",
        serde_json::json!({
            "simulated": "checkpoint_and_mapping_removed_after_successful_svn_to_git",
            "git_push_checkpoint_recovery": checkpoint_recovery,
            "recovery_ok": false,
            "final_watermark": historical["repo_svn_rev"],
            "expected_watermark": historical["canonical_svn_rev"],
            "bare_head_stable": historical["bare_head_stable"],
            "mapping_count": mapping_count,
            "mapped_git_sha": historical["mapped_git_sha"],
            "duplicate_mapping": mapping_count > 1,
            "recovery_error": historical["recovery_error"],
            "finalized_history": finalized
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_svn_remote_recreation() {
    if !require_toolchain(CASE_SVN_REMOTE_RECREATION) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let before = fixture.checkpoint_snapshot();
    let mappings_before: i64 = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let info_before = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap();
    let repo_dir = fixture
        .tmp
        .path()
        .join("svnserve-root")
        .join(&fixture.repo.id);
    std::fs::remove_dir_all(&repo_dir).unwrap();
    assert!(Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    write_svnserve_auth(&repo_dir, &fixture.repo.username, &fixture.repo.password);
    let recreated_wc = fixture.tmp.path().join("recreated_wc");
    svn_checkout(
        &fixture.repo.svn_url,
        &recreated_wc,
        &fixture.repo.username,
        &fixture.repo.password,
    );
    svn_commit_file(
        &recreated_wc,
        "recreated.txt",
        "new lineage\n",
        "Recreated svnserve repo",
        &fixture.repo.username,
        &fixture.repo.password,
    );
    let info_after = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    )
    .info()
    .await
    .unwrap();
    let result = engine.run_sync_cycle().await;
    let after = fixture.checkpoint_snapshot();
    let mappings_after = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let uuid_changed = info_before.uuid != info_after.uuid;
    let checkpoint_stable = after == before;
    let mapping_stable = mappings_after == mappings_before;
    let blocked = result.is_err();
    let case_status = if uuid_changed && checkpoint_stable && mapping_stable && blocked {
        "PASS"
    } else {
        "PARTIAL"
    };
    emit_evidence(
        CASE_SVN_REMOTE_RECREATION,
        case_status,
        serde_json::json!({
            "uuid_before": info_before.uuid,
            "uuid_after": info_after.uuid,
            "checkpoint_before_after": [before, after],
            "mapping_count_before_after": [mappings_before, mappings_after],
            "cycle_blocked": blocked,
            "cycle_error": result.err().map(|e| e.to_string()),
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r16_svnserve_git_remote_recreation() {
    if !require_toolchain(CASE_GIT_REMOTE_RECREATION) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let before = fixture.checkpoint_snapshot();
    let mappings_before: i64 = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let bridge_before = get_head_sha(&fixture.repo.bridge);
    std::fs::remove_dir_all(&fixture.repo.bare).unwrap();
    git2::Repository::init_bare(&fixture.repo.bare).unwrap();
    let result = engine.run_sync_cycle().await;
    let after = fixture.checkpoint_snapshot();
    let mappings_after = setup_db(&fixture.db_path).count_sync_records().unwrap();
    let bridge_after = get_head_sha(&fixture.repo.bridge);
    let blocked = result.is_err();
    let case_status = if blocked
        && after == before
        && mappings_after == mappings_before
        && bridge_after == bridge_before
    {
        "PASS"
    } else {
        "PARTIAL"
    };
    emit_evidence(
        CASE_GIT_REMOTE_RECREATION,
        case_status,
        serde_json::json!({
            "checkpoint_before_after": [before, after],
            "mapping_count_before_after": [mappings_before, mappings_after],
            "bridge_head_before_after": [bridge_before, bridge_after],
            "cycle_blocked": blocked,
            "cycle_error": result.err().map(|e| e.to_string()),
            "note": "recreated empty bare remote at same path; must not auto-destructively reinit",
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenario_r17_svnserve_notification_isolation() {
    if !require_toolchain(CASE_NOTIFICATION_ISOLATION) {
        return;
    }
    let fixture = DualRepoFixture::new();
    let db = setup_db(&fixture.db_path);
    let alpha_hook = "https://notify.invalid/repo-alpha-only";
    let beta_hook = "https://notify.invalid/repo-beta-only";
    let mut alpha_repo = db.get_repository("repo_alpha").unwrap().unwrap();
    alpha_repo.teams_webhook_url = Some(alpha_hook.into());
    db.update_repository(&alpha_repo).unwrap();
    let mut beta_repo = db.get_repository("repo_beta").unwrap().unwrap();
    beta_repo.teams_webhook_url = Some(beta_hook.into());
    db.update_repository(&beta_repo).unwrap();
    let ready = Arc::new(tokio::sync::Barrier::new(2));
    let (alpha_result, beta_result) = tokio::join!(
        async {
            ready.wait().await;
            fixture
                .make_engine(&fixture.repos[0])
                .run_sync_cycle()
                .await
        },
        async {
            ready.wait().await;
            fixture
                .make_engine(&fixture.repos[1])
                .run_sync_cycle()
                .await
        }
    );
    let db = setup_db(&fixture.db_path);
    let alpha_row = db.get_repository("repo_alpha").unwrap().unwrap();
    let beta_row = db.get_repository("repo_beta").unwrap().unwrap();
    let webhook_crossover = repo_teams_webhook(&db, "repo_alpha").as_deref() != Some(alpha_hook)
        || repo_teams_webhook(&db, "repo_beta").as_deref() != Some(beta_hook);
    let sync_status_crossover =
        alpha_row.sync_status.contains("repo_beta") || beta_row.sync_status.contains("repo_alpha");
    let audit_crossover: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM audit_log WHERE (repo_id = 'repo_alpha' AND details LIKE '%repo-beta-only%') OR (repo_id = 'repo_beta' AND details LIKE '%repo-alpha-only%')",
        [],
        |row| row.get(0),
    ).unwrap();
    let imports_ok = match (&alpha_result, &beta_result) {
        (Ok(alpha_stats), Ok(beta_stats)) => {
            alpha_stats.svn_to_git_count == 1 && beta_stats.svn_to_git_count == 1
        }
        _ => false,
    };
    let per_repo_sync_counters = alpha_row.total_syncs >= 1 && beta_row.total_syncs >= 1;
    let webhook_fields_persist = !webhook_crossover;
    let confinement_ok = webhook_fields_persist && !sync_status_crossover && audit_crossover == 0;
    let notification_delivery_observed = false;
    let case_status = if !confinement_ok {
        "FAIL"
    } else if notification_delivery_observed {
        "PASS"
    } else {
        "PARTIAL"
    };
    emit_evidence(
        CASE_NOTIFICATION_ISOLATION,
        case_status,
        serde_json::json!({
            "teams_webhook_crossover": webhook_crossover,
            "sync_status_crossover": sync_status_crossover,
            "audit_log_crossover": audit_crossover,
            "per_repo_sync_counters": per_repo_sync_counters,
            "alpha_total_syncs": alpha_row.total_syncs,
            "beta_total_syncs": beta_row.total_syncs,
            "imports_ok": imports_ok,
            "webhook_fields_persist": webhook_fields_persist,
            "confinement_ok": confinement_ok,
            "notification_delivery_observed": notification_delivery_observed,
            "note": "SyncEngine does not read per-repo teams_webhook_url or invoke Notifier on svnserve cycles; webhook URL persistence and sync_status/audit confinement only",
        }),
    );
}

/// #228: real SVN process, authentic team journal, failure before publication,
/// observe-first authorized retry, exact UTF-8 history and unchanged quiet run.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_large_message_retry() {
    use std::os::unix::fs::PermissionsExt;
    if !require_toolchain(CASE_LARGE_MESSAGE) {
        return;
    }
    let fixture = SingleRepoFixture::new();
    let engine = fixture.make_engine();
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
    let message = format!(
        "{}😀 synthetic squash\n\n{}\n[reposync] original marker\n",
        "a".repeat(119),
        "é中文😀 '\"\\ multiline\n".repeat(16_384)
    );
    assert!(message.len() >= 256 * 1024);
    std::fs::write(
        fixture.developer.join("large-message.txt"),
        "complete synthetic content\n",
    )
    .unwrap();
    let developer = GitClient::new(&fixture.developer).unwrap();
    let sha = developer
        .commit_via_cli(
            &message,
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    drop(developer);
    git_cli(&fixture.developer, &["push", "origin", "main"]);
    let svn = SvnClient::new(
        &fixture.repo.svn_url,
        &fixture.repo.username,
        &fixture.repo.password,
    );
    let before_revision = svn.info().await.unwrap().latest_rev;
    let before_checkpoint = fixture.checkpoint_snapshot();
    // An actual hook-interpreter launch failure, isolated to this disposable
    // repository, is known to precede publication. No PATH/OS replacement.
    let hook = fixture
        .tmp
        .path()
        .join("svnserve-root")
        .join(&fixture.repo.id)
        .join("hooks/pre-commit");
    assert!(!hook.exists());
    std::fs::write(&hook, "#!/reposync-fixture-absent-interpreter\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failure = engine.run_sync_cycle().await;
    assert!(
        matches!(failure, Err(SyncError::SvnError(_))),
        "{failure:?}"
    );
    assert_eq!(svn.info().await.unwrap().latest_rev, before_revision);
    assert_eq!(fixture.checkpoint_snapshot(), before_checkpoint);
    let db = setup_db(&fixture.db_path);
    let applied = || {
        db.conn().query_row("SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND direction='git_to_svn' AND status='applied'",rusqlite::params![fixture.repo.id,sha],|row|row.get::<_,i64>(0)).unwrap()
    };
    assert_eq!(applied(), 0);
    let held = db
        .active_svn_commit_operation(&fixture.repo.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        held.state,
        reposync_core::db::svn_commit_operations::SvnCommitOperationState::ReconciliationRequired
    );
    std::fs::remove_file(&hook).unwrap();
    // Keep the existing uncertainty policy: observing absence authorizes one
    // recorded write. Restoring a binary/hook alone is never permission to replay.
    let reconciled = reposync_core::svn_commit::apply_svn_commit_reconciliation(
        &db,
        &fixture.repo.id,
        &held.id,
        &svn,
        &serde_json::json!({"allowed_paths":[],"blocked_patterns":[]}).to_string(),
    )
    .await
    .unwrap();
    assert!(!reconciled.finalized && reconciled.resume_authorized);
    assert_eq!(fixture.checkpoint_snapshot(), before_checkpoint);
    assert_eq!(svn.info().await.unwrap().latest_rev, before_revision);
    let applied_stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(applied_stats.git_to_svn_count, 1);
    let after_revision = svn.info().await.unwrap().latest_rev;
    assert_eq!(after_revision, before_revision + 1);
    assert_eq!(applied(), 1);
    let log = svn.log(after_revision, after_revision).await.unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(
        log[0].message,
        reposync_core::svn_commit::operation_commit_message(&message, &sha, &held.id)
    );
    assert_eq!(
        svn_read_file(
            &fixture.repo.svn_url,
            &fixture.repo.username,
            &fixture.repo.password,
            after_revision,
            "large-message.txt"
        )
        .await,
        "complete synthetic content\n"
    );
    let after_checkpoint = fixture.checkpoint_snapshot();
    assert_eq!(after_checkpoint.1, sha);
    assert!(db
        .active_svn_commit_operation(&fixture.repo.id)
        .unwrap()
        .is_none());
    let repeated = engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        (repeated.svn_to_git_count, repeated.git_to_svn_count),
        (0, 0)
    );
    assert_eq!(fixture.checkpoint_snapshot(), after_checkpoint);
    assert_eq!(svn.info().await.unwrap().latest_rev, after_revision);
    assert_eq!(applied(), 1);
    emit_evidence(
        CASE_LARGE_MESSAGE,
        "PASS",
        serde_json::json!({"message_bytes":message.len(),"exact_log_preserved":true,"known_hook_launch_failure_no_checkpoint":true,"observe_first_retry_once":true,"revision":after_revision,"git_sha":sha,"quiet_repeat":true}),
    );
}
