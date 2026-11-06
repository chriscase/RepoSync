//! RepoSync #73 — active (non-ignored) team-mode overlap-conflict E2E.
//!
//! The older `team_mode_e2e.rs::test_team_mode_conflict_detection` is still
//! `#[ignore]`d and uses a flat SVN fixture; it is deliberately left
//! untouched and is NOT counted as coverage here. This dedicated test uses
//! owned, disposable fixtures only (`file://` SVN, local bare Git origin,
//! temporary SQLite) and drives the real [`SyncEngine`]:
//!
//! 1. A standard `trunk/ branches/ tags/` SVN layout is created.
//! 2. A verified SVN-origin baseline (`shared.txt`) is imported SVN→Git by the
//!    real engine and the exact durable mapping is asserted.
//! 3. SVN and Git then make divergent edits to the *same line* of `shared.txt`.
//! 4. One cycle must surface `SyncError::UnresolvableConflict` for
//!    `shared.txt` (an `ApplyFailed`/other error is a test failure, never
//!    conflict success), persist a `content` conflict row, and leave both
//!    original histories/trees plus every directional checkpoint and mapping
//!    unacknowledged. A retry must be refused by the persisted-conflict gate
//!    (`UnresolvableConflict` with the "persisted conflict row(s) block
//!    apply" detail, same row id/scope/status) and not mutate either side.
//!    The row is also re-read through a separate `Database` handle; no engine
//!    or process restart is exercised.
//!
//! Known limit (remaining whole-#73 gate): on the unresolved-conflict path the
//! engine returns `Err(UnresolvableConflict { detail: "N unresolved
//! conflict(s) recorded; ..." })` and drops `SyncStats`, so the literal
//! `SyncStats::conflicts_detected` value is not observable from
//! `run_sync_cycle`. This test asserts the equivalent public surfaces instead
//! (error variant + the engine-reported count in `detail`, and the persisted
//! conflict row count) and does not claim to assert the `SyncStats` field.
//! Exposing it requires a production change (reserved `sync_engine.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;

use reposync_core::config::{AppConfig, IdentityConfig, SvnLayout};
use reposync_core::db::Database;
use reposync_core::errors::SyncError;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;

const REPO_ID: &str = "rs73-overlap";
const BASE: &str = "alpha\nshared line\nomega\n";
const SVN_EDIT: &str = "alpha\nSVN edited shared line\nomega\n";
const GIT_EDIT: &str = "alpha\nGit edited shared line\nomega\n";

// ---------------------------------------------------------------------------
// Fixture helpers (owned, disposable; cleanup is the TempDir drop)
// ---------------------------------------------------------------------------

fn tool_ok(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

fn run_ok(cmd: &mut Command, what: &str) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_cli(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
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

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn head_sha(repo: &Path) -> String {
    git2::Repository::open(repo)
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap()
        .to_string()
}

/// Every tracked file at `revision` with its exact bytes.
fn tracked_tree_at(repo: &Path, revision: &str) -> BTreeMap<String, Vec<u8>> {
    let listing = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", "--name-only", "-z", revision])
        .output()
        .unwrap();
    assert!(listing.status.success());
    let mut files = BTreeMap::new();
    for name in listing
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).unwrap();
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["show", &format!("{revision}:{name}")])
            .output()
            .unwrap();
        assert!(output.status.success(), "cannot read tracked {name}");
        files.insert(name.to_string(), output.stdout);
    }
    files
}

fn create_svn_repo(dir: &Path) -> String {
    assert_fixture_owned(dir);
    let repo_dir = dir.join("svn_repo");
    run_ok(
        Command::new("svnadmin").args(["create", repo_dir.to_str().unwrap()]),
        "svnadmin create",
    );
    format!("file://{}", repo_dir.display())
}

fn svn_youngest(root_url: &str) -> i64 {
    let repo = root_url.strip_prefix("file://").unwrap();
    run_ok(
        Command::new("svnlook").args(["youngest", repo]),
        "svnlook youngest",
    )
    .parse()
    .unwrap()
}

/// Revisions of the whole SVN history, oldest first (`svn log -q`).
fn svn_history(root_url: &str) -> Vec<String> {
    run_ok(
        Command::new("svn").args(["log", "-q", "-r", "1:HEAD", root_url, "--non-interactive"]),
        "svn log",
    )
    .lines()
    .filter(|line| line.starts_with('r'))
    .map(str::to_string)
    .collect()
}

/// Sorted recursive listing of the SVN root (proves the intended layout).
fn svn_listing(root_url: &str) -> Vec<String> {
    let mut entries: Vec<String> = run_ok(
        Command::new("svn").args(["ls", "-R", root_url, "--non-interactive"]),
        "svn ls -R",
    )
    .lines()
    .map(str::to_string)
    .collect();
    entries.sort();
    entries
}

fn svn_cat(trunk_url: &str, path: &str, rev: i64) -> String {
    run_ok(
        Command::new("svn").args([
            "cat",
            &format!("{trunk_url}/{path}@{rev}"),
            "-r",
            &rev.to_string(),
            "--non-interactive",
        ]),
        "svn cat",
    )
}

fn svn_update(wc: &Path) {
    run_ok(
        Command::new("svn").args(["update", wc.to_str().unwrap(), "--non-interactive"]),
        "svn update",
    );
}

/// Write (and `svn add` when new), commit, and return the committed revision.
fn svn_commit_file(wc: &Path, name: &str, content: &str, message: &str) -> i64 {
    let file = wc.join(name);
    let is_new = !file.exists();
    std::fs::write(&file, content).unwrap();
    if is_new {
        run_ok(
            Command::new("svn").args(["add", file.to_str().unwrap()]),
            "svn add",
        );
    }
    let out = run_ok(
        Command::new("svn").args([
            "commit",
            "-m",
            message,
            wc.to_str().unwrap(),
            "--username",
            "fixture",
            "--non-interactive",
        ]),
        "svn commit",
    );
    out.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Committed revision")
                .map(|rest| rest.trim().trim_end_matches('.').parse::<i64>().unwrap())
        })
        .unwrap_or_else(|| panic!("could not parse committed revision from: {out}"))
}

fn setup_git_with_bare_origin(work: &Path, bare: &Path) -> GitClient {
    assert_fixture_owned(work.parent().unwrap());
    assert_fixture_owned(bare.parent().unwrap());
    git2::Repository::init_bare(bare).expect("init bare");
    let client = GitClient::init(work).expect("init work repo");
    git2::Repository::open(work)
        .unwrap()
        .remote("origin", bare.to_str().unwrap())
        .expect("add origin");
    std::fs::write(work.join(".gitkeep"), "").unwrap();
    client
        .commit(
            "initial commit",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("initial commit");
    {
        let repo = git2::Repository::open(work).unwrap();
        let head = repo.head().unwrap();
        let name = head.name().unwrap_or("");
        if name != "refs/heads/main" {
            let mut branch = repo
                .find_branch(
                    name.strip_prefix("refs/heads/").unwrap_or("master"),
                    git2::BranchType::Local,
                )
                .unwrap();
            branch.rename("main", true).unwrap();
        }
    }
    client.push("origin", "main").expect("push initial");
    client
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

// ---------------------------------------------------------------------------
// State captured before/after to prove nothing was acknowledged or rewritten
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct State {
    // SVN side (original history and trunk tree).
    svn_youngest: i64,
    svn_history: Vec<String>,
    svn_listing: Vec<String>,
    svn_shared: String,
    // Git side (original history and trees).
    remote_sha: String,
    remote_tree: String,
    remote_commits: Vec<String>,
    remote_files: BTreeMap<String, Vec<u8>>,
    // Directional checkpoints.
    repo_row: (i64, String, i64),
    watermark: (i64, String),
    svn_cursor_kv: Option<String>,
    git_cursor_kv: Option<String>,
    // Directional mappings: (direction, svn_rev, git_sha) for every record.
    mappings: Vec<(String, Option<i64>, Option<String>)>,
    // Durable publication operations in each direction.
    git_push_latest: Option<reposync_core::db::git_push_operations::GitPushOperation>,
    git_push_active: Option<reposync_core::db::git_push_operations::GitPushOperation>,
    svn_commit_latest: Option<reposync_core::db::svn_commit_operations::SvnCommitOperation>,
    svn_commit_active: Option<reposync_core::db::svn_commit_operations::SvnCommitOperation>,
}

struct Fixture {
    _tmp: TempDir,
    svn_root: String,
    trunk_url: String,
    wc: PathBuf,
    bridge: PathBuf,
    bare: PathBuf,
    developer: PathBuf,
    db_path: PathBuf,
    engine: SyncEngine,
    baseline_svn_rev: i64,
    imported_base: String,
}

impl Fixture {
    async fn new() -> Self {
        for tool in ["svn", "svnadmin", "svnlook", "git"] {
            // Missing tooling must fail loudly: a silent return would make
            // this test pass without proving anything.
            assert!(
                tool_ok(tool),
                "{tool} is required for the #73 overlap-conflict E2E and was not found"
            );
        }
        let tmp = TempDir::new().unwrap();
        let svn_root = create_svn_repo(tmp.path());
        let trunk_url = reposync_core::svn::operand::configured_branch_url(&svn_root, "trunk")
            .expect("trunk URL");

        // Intended standard SVN layout (rev 1).
        run_ok(
            Command::new("svn").args([
                "mkdir",
                "-m",
                "RepoSync standard layout",
                &format!("{svn_root}/trunk"),
                &format!("{svn_root}/branches"),
                &format!("{svn_root}/tags"),
                "--username",
                "fixture",
                "--non-interactive",
            ]),
            "svn mkdir layout",
        );
        assert_eq!(
            svn_listing(&svn_root),
            vec!["branches/", "tags/", "trunk/"],
            "intended standard SVN layout must exist before the baseline"
        );

        let wc = tmp.path().join("wc");
        run_ok(
            Command::new("svn").args([
                "checkout",
                &trunk_url,
                wc.to_str().unwrap(),
                "--non-interactive",
            ]),
            "svn checkout trunk",
        );
        svn_commit_file(&wc, ".gitkeep", "", "Initial SVN anchor");
        let baseline_svn_rev = svn_commit_file(&wc, "shared.txt", BASE, "Verified SVN origin");
        assert_eq!(baseline_svn_rev, svn_youngest(&svn_root));

        // Git bridge/origin start at the anchor; the real engine imports
        // the baseline revision (shared.txt) SVN→Git.
        let bridge = tmp.path().join("bridge");
        let bare = tmp.path().join("origin.git");
        let git = setup_git_with_bare_origin(&bridge, &bare);
        let initial = head_sha(&bridge);

        let db_path = tmp.path().join("sync.db");
        let db = Database::new(&db_path).expect("create database");
        db.initialize().expect("initialize schema");
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: REPO_ID.into(),
            name: "rs73 overlap".into(),
            svn_url: svn_root.clone(),
            svn_branch: "trunk".into(),
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
            last_svn_rev: baseline_svn_rev - 1,
            last_git_sha: initial.clone(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .expect("insert repository");

        let mut config = make_app_config(&svn_root, tmp.path());
        config.svn.layout = SvnLayout::Standard;
        config.svn.trunk_path = "trunk".into();
        config.sync.auto_merge = false;
        let mapper = Arc::new(
            IdentityMapper::new(&IdentityConfig {
                email_domain: Some("example.com".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        let mut engine =
            SyncEngine::new(config, db, SvnClient::new(&trunk_url, "", ""), git, mapper);
        engine.set_repo_id(REPO_ID.into());

        // Verified SVN-origin baseline through the real engine.
        let baseline = engine
            .run_sync_cycle()
            .await
            .expect("baseline SVN→Git import must succeed");
        assert_eq!(
            baseline.svn_to_git_count, 1,
            "exactly the shared.txt revision"
        );
        assert_eq!(baseline.git_to_svn_count, 0);
        assert_eq!(baseline.conflicts_detected, 0);
        let imported_base = head_sha(&bridge);
        assert_ne!(imported_base, initial);
        assert_eq!(
            engine.db().get_repo_watermark(REPO_ID).unwrap(),
            (baseline_svn_rev, imported_base.clone())
        );
        let mapped: i64 = engine
            .db()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git' AND svn_rev=?2 AND git_sha=?3",
                rusqlite::params![REPO_ID, baseline_svn_rev, &imported_base],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            mapped, 1,
            "SVN-derived baseline must carry the engine mapping"
        );
        assert_eq!(
            svn_cat(&trunk_url, "shared.txt", baseline_svn_rev),
            BASE.trim_end(),
            "SVN origin content"
        );
        assert_eq!(
            std::fs::read_to_string(bridge.join("shared.txt")).unwrap(),
            BASE
        );
        assert_eq!(
            git_output(&bare, &["rev-parse", "refs/heads/main"]),
            imported_base,
            "origin must hold the imported baseline"
        );
        assert_eq!(
            tracked_tree_at(&bare, "refs/heads/main")
                .get("shared.txt")
                .map(Vec::as_slice),
            Some(BASE.as_bytes())
        );

        let developer = tmp.path().join("developer");
        run_ok(
            Command::new("git").args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                developer.to_str().unwrap(),
            ]),
            "git clone developer",
        );
        assert_eq!(head_sha(&developer), imported_base);

        Self {
            _tmp: tmp,
            svn_root,
            trunk_url,
            wc,
            bridge,
            bare,
            developer,
            db_path,
            engine,
            baseline_svn_rev,
            imported_base,
        }
    }

    fn state(&self) -> State {
        let db = self.engine.db();
        let youngest = svn_youngest(&self.svn_root);
        let repo = db.get_repository(REPO_ID).unwrap().unwrap();
        let mappings = {
            let conn = db.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT direction, svn_rev, git_sha FROM sync_records WHERE repo_id = ?1 ORDER BY direction, svn_rev, git_sha",
                )
                .unwrap();
            let rows = stmt
                .query_map([REPO_ID], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        State {
            svn_youngest: youngest,
            svn_history: svn_history(&self.svn_root),
            svn_listing: svn_listing(&self.svn_root),
            svn_shared: svn_cat(&self.trunk_url, "shared.txt", youngest),
            remote_sha: git_output(&self.bare, &["rev-parse", "refs/heads/main"]),
            remote_tree: git_output(&self.bare, &["rev-parse", "refs/heads/main^{tree}"]),
            remote_commits: git_output(&self.bare, &["rev-list", "refs/heads/main"])
                .lines()
                .map(str::to_string)
                .collect(),
            remote_files: tracked_tree_at(&self.bare, "refs/heads/main"),
            repo_row: (repo.last_svn_rev, repo.last_git_sha, repo.total_syncs),
            watermark: db.get_repo_watermark(REPO_ID).unwrap(),
            svn_cursor_kv: db.get_state(&format!("last_svn_rev_{REPO_ID}")).unwrap(),
            git_cursor_kv: reposync_core::echo_receipt_scope::read_scoped_last_git_sha_kv(
                db, REPO_ID,
            )
            .unwrap(),
            mappings,
            git_push_latest: db.latest_git_push_operation(REPO_ID).unwrap(),
            git_push_active: db.active_git_push_operation(REPO_ID).unwrap(),
            svn_commit_latest: db.latest_svn_commit_operation(REPO_ID).unwrap(),
            svn_commit_active: db.active_svn_commit_operation(REPO_ID).unwrap(),
        }
    }
}

/// The database must hold exactly the first-cycle conflict row: same id,
/// repository scope, path, type, status and metadata, still blocking apply.
fn assert_single_blocking_row(
    db: &Database,
    first: &reposync_core::db::queries::ConflictEntry,
    context: &str,
) {
    let all = db.list_conflicts(None, 10).unwrap();
    assert_eq!(
        all.len(),
        1,
        "{context}: exactly one conflict row, none added or removed: {all:?}"
    );
    let row = &all[0];
    assert_eq!(row.id, first.id, "{context}: row id must be unchanged");
    assert_eq!(
        row.repo_id.as_deref(),
        Some(REPO_ID),
        "{context}: repository scope"
    );
    assert_eq!(row.repo_id, first.repo_id, "{context}: repository scope");
    assert_eq!(row.file_path, first.file_path, "{context}: path");
    assert_eq!(row.conflict_type, first.conflict_type, "{context}: type");
    assert_eq!(row.status, "detected", "{context}: status");
    assert_eq!(row.status, first.status, "{context}: status");
    assert_eq!(row.resolution, first.resolution, "{context}: resolution");
    assert_eq!(row.resolved_by, first.resolved_by, "{context}: resolved_by");
    assert_eq!(row.resolved_at, first.resolved_at, "{context}: resolved_at");
    assert_eq!(row.created_at, first.created_at, "{context}: created_at");
    assert_eq!(row.svn_rev, first.svn_rev, "{context}: svn_rev");
    assert_eq!(row.git_sha, first.git_sha, "{context}: git_sha");
    assert_eq!(
        db.count_conflicts_blocking_apply_for_repo(REPO_ID).unwrap(),
        1,
        "{context}: still exactly one blocking row"
    );
    assert_eq!(
        db.first_conflict_blocking_apply_for_repo(REPO_ID).unwrap(),
        Some((first.id.clone(), first.file_path.clone())),
        "{context}: the gate's first blocking row must be the original row"
    );
}

/// The bridge checkout is hard-reset by the engine to the admitted Git tip
/// before conflict detection, so it may legitimately sit on the baseline or
/// on the (unmodified, already-published) Git edit. It must never gain a new
/// local commit, hold dirty state, or absorb the SVN edit.
fn assert_bridge_intact(fx: &Fixture, git_edit_sha: &str) {
    let head = head_sha(&fx.bridge);
    assert!(
        head == fx.imported_base || head == git_edit_sha,
        "bridge HEAD {head} must be the baseline or the published Git edit, never a new commit"
    );
    assert_eq!(
        git_output(&fx.bridge, &["status", "--porcelain"]),
        "",
        "bridge worktree must be clean"
    );
    let shared = std::fs::read_to_string(fx.bridge.join("shared.txt")).unwrap();
    assert!(
        (head == fx.imported_base && shared == BASE)
            || (head == git_edit_sha && shared == GIT_EDIT),
        "bridge shared.txt must match its HEAD and never carry the SVN edit: {shared:?}"
    );
    assert_ne!(shared, SVN_EDIT);
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// Overlapping SVN and Git edits to one shared line must produce a persisted,
/// surfaced conflict while both original histories/trees stay intact and no
/// directional checkpoint or mapping acknowledges the pending work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs73_overlapping_shared_file_edits_surface_persisted_conflict_without_acknowledgement() {
    let fx = Fixture::new().await;
    let baseline = fx.state();
    assert_eq!(baseline.svn_youngest, fx.baseline_svn_rev);
    assert_eq!(baseline.remote_sha, fx.imported_base);
    let baseline_mapping = (
        "svn_to_git".to_string(),
        Some(fx.baseline_svn_rev),
        Some(fx.imported_base.clone()),
    );
    assert!(
        baseline.mappings.contains(&baseline_mapping),
        "baseline mapping must exist: {:?}",
        baseline.mappings
    );
    assert!(
        fx.engine.db().list_conflicts(None, 10).unwrap().is_empty(),
        "no conflicts before the divergence"
    );

    // Divergent edits to the same line of the same shared file.
    svn_update(&fx.wc);
    let svn_edit_rev = svn_commit_file(&fx.wc, "shared.txt", SVN_EDIT, "SVN edits shared line");
    assert_eq!(svn_edit_rev, fx.baseline_svn_rev + 1);
    std::fs::write(fx.developer.join("shared.txt"), GIT_EDIT).unwrap();
    git_cli(&fx.developer, &["add", "--", "shared.txt"]);
    git_cli(&fx.developer, &["commit", "-m", "Git edits shared line"]);
    let git_edit_sha = head_sha(&fx.developer);
    assert_ne!(git_edit_sha, fx.imported_base);
    git_cli(&fx.developer, &["push", "origin", "main"]);
    assert_eq!(
        git_output(&fx.bare, &["rev-parse", "refs/heads/main"]),
        git_edit_sha
    );
    assert_eq!(
        git_output(&fx.bare, &["rev-parse", "refs/heads/main~1"]),
        fx.imported_base,
        "Git edit must be a child of the imported SVN baseline"
    );

    // Pre-cycle state with both divergent histories in place.
    let before = fx.state();
    assert_eq!(before.svn_youngest, svn_edit_rev);
    assert_eq!(before.svn_shared, SVN_EDIT.trim_end());
    assert_eq!(before.remote_sha, git_edit_sha);
    assert_eq!(
        before.remote_files.get("shared.txt").map(Vec::as_slice),
        Some(GIT_EDIT.as_bytes())
    );
    assert_eq!(
        before.watermark,
        (fx.baseline_svn_rev, fx.imported_base.clone())
    );

    // The cycle must surface the conflict, not any other failure.
    let result = fx.engine.run_sync_cycle().await;
    match &result {
        Err(SyncError::UnresolvableConflict { file_path, detail }) => {
            assert_eq!(file_path, "shared.txt", "conflict must name the shared file");
            assert!(
                detail.contains("1 unresolved conflict(s) recorded"),
                "engine-reported conflict count must be exactly 1: {detail}"
            );
        }
        other => panic!(
            "expected UnresolvableConflict for shared.txt (not an apply error or success), got {other:?}"
        ),
    }

    // Persisted, surfaced conflict row (queried through public DB API).
    let conflicts = fx.engine.db().list_conflicts(None, 10).unwrap();
    let rows: Vec<_> = conflicts
        .iter()
        .filter(|c| c.repo_id.as_deref() == Some(REPO_ID))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "exactly one persisted conflict: {conflicts:?}"
    );
    assert_eq!(
        conflicts.len(),
        rows.len(),
        "no unscoped/foreign conflict rows"
    );
    assert_eq!(rows[0].file_path, "shared.txt");
    assert_eq!(rows[0].conflict_type, "content");
    assert_eq!(rows[0].status, "detected");
    // Identity of the first-cycle row; retries and fresh handles must see
    // this exact row (same id, scope, status), not a replacement.
    let first_row = (*rows[0]).clone();
    assert!(!first_row.id.is_empty());
    assert_eq!(first_row.repo_id.as_deref(), Some(REPO_ID));
    assert_eq!(
        fx.engine
            .db()
            .count_conflicts_blocking_apply_for_repo(REPO_ID)
            .unwrap(),
        1,
        "the persisted conflict must block apply"
    );
    let repo_row = fx.engine.db().get_repository(REPO_ID).unwrap().unwrap();
    assert_eq!(
        repo_row.sync_status, "error",
        "failure must be surfaced as status"
    );

    // Both original histories/trees intact; nothing acknowledged.
    let after = fx.state();
    assert_eq!(
        after, before,
        "conflict cycle must not mutate or acknowledge anything"
    );
    assert_eq!(
        after.watermark,
        (fx.baseline_svn_rev, fx.imported_base.clone())
    );
    assert_eq!(
        after.mappings, baseline.mappings,
        "no new directional mapping"
    );
    assert!(
        !after.mappings.iter().any(|(d, _, _)| d == "git_to_svn"),
        "no Git→SVN mapping may exist"
    );
    assert!(
        !after
            .mappings
            .iter()
            .any(|(_, rev, sha)| *rev == Some(svn_edit_rev)
                || sha.as_deref() == Some(git_edit_sha.as_str())),
        "neither pending edit may be mapped"
    );
    assert!(
        !fx.engine
            .db()
            .has_repo_emitted_git_commit(REPO_ID, &git_edit_sha)
            .unwrap(),
        "pending Git commit must not be recorded as emitted"
    );
    assert_ne!(after.git_cursor_kv.as_deref(), Some(git_edit_sha.as_str()));
    assert_ne!(after.watermark.1, git_edit_sha);
    assert!(after.watermark.0 < svn_edit_rev);
    assert_eq!(after.svn_shared, SVN_EDIT.trim_end(), "SVN edit preserved");
    assert_eq!(
        after.remote_files.get("shared.txt").map(Vec::as_slice),
        Some(GIT_EDIT.as_bytes()),
        "Git edit preserved"
    );
    assert_bridge_intact(&fx, &git_edit_sha);
    assert!(after.git_push_active.is_none() && after.svn_commit_active.is_none());

    // Retry: the engine's persisted-conflict gate (the first apply gate in
    // the sync cycle) must be what blocks it. Its wording differs from the
    // first-cycle detection wording ("N persisted conflict row(s) block
    // apply; ..." vs "N unresolved conflict(s) recorded; ..."). Any other
    // error (AlreadyRunning, admission/database failure, history or hold
    // errors) or success fails here.
    let retry = fx.engine.run_sync_cycle().await;
    match &retry {
        Err(SyncError::UnresolvableConflict { file_path, detail }) => {
            assert_eq!(
                file_path, "shared.txt",
                "retry must name the first persisted blocking row's path"
            );
            assert_eq!(
                detail,
                "1 persisted conflict row(s) block apply; only resolved or dismissed rows allow SVN/Git mutation",
                "retry must be blocked by the persisted gate with exactly one row and no unattributed legacy rows"
            );
        }
        other => panic!(
            "retry must fail with the persisted-conflict UnresolvableConflict gate, got {other:?}"
        ),
    }
    assert_single_blocking_row(fx.engine.db(), &first_row, "after blocked retry");
    assert_eq!(
        fx.state(),
        before,
        "retry must not mutate or acknowledge anything"
    );
    assert_bridge_intact(&fx, &git_edit_sha);

    // Persistence through a separate Database handle on the same SQLite
    // file. This is NOT an engine/process restart: no SyncEngine is rebuilt
    // or run against the reopened handle.
    let reopened = Database::new(&fx.db_path).unwrap();
    reopened.initialize().unwrap();
    assert_single_blocking_row(&reopened, &first_row, "via separate Database handle");
    assert_eq!(
        reopened.get_repo_watermark(REPO_ID).unwrap(),
        (fx.baseline_svn_rev, fx.imported_base.clone())
    );

    eprintln!(
        "RS73_EVIDENCE {}",
        serde_json::json!({
            "case": "RS73_OVERLAP_CONFLICT",
            "layout": "standard trunk/branches/tags",
            "baseline_svn_rev": fx.baseline_svn_rev,
            "svn_edit_rev": svn_edit_rev,
            "imported_base": fx.imported_base,
            "git_edit_sha": git_edit_sha,
            "persisted_conflicts": rows.len(),
            "state_unchanged_after_conflict_and_retry": true
        })
    );
}
