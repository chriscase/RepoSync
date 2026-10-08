//! Integration tests for the SVN-to-Git sync pipeline.
//!
//! These tests exercise the full sync pipeline using:
//! - Real local SVN repos created via `svnadmin create` (file:// protocol)
//! - Real local Git repos via `git2::Repository`
//! - Real SQLite databases via `Database::new()`
//!
//! No network I/O: SVN uses `file://` URLs, Git pushes go to local bare repos.
//!
//! If `svn` / `svnadmin` are not installed, tests skip gracefully.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use std::sync::Mutex;
use tempfile::TempDir;

use chrono::Utc;
use reposync_core::db::personal_scope::{LEGACY_PERSONAL_REPO_ID, PERSONAL_SCOPE_KEY};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::models::{SyncDirection, SyncRecord, SyncRecordStatus};
use reposync_core::personal_config::{
    CommitFormatConfig, DeveloperConfig, PersonalConfig, PersonalGitHubConfig,
    PersonalOptionsConfig, PersonalSection, PersonalSvnConfig,
};
use reposync_core::svn::SvnClient;
use reposync_personal::commit_format::CommitFormatter;
use reposync_personal::git_to_svn::{
    personal_apply_abort_paths_clean_for_test, personal_svn_commit_fixture_env_key, GitToSvnSync,
};
use reposync_personal::svn_to_git::{personal_git_push_fixture_env_key, SvnToGitSync};

// ===========================================================================
// Helper functions
// ===========================================================================

/// Returns `true` if both `svn` and `svnadmin` are available on `$PATH`.
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

/// Create a local SVN repository via `svnadmin create`. Returns the `file://` URL.
fn create_svn_repo(dir: &Path) -> String {
    let repo_dir = dir.join("svn_repo");
    let status = Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .expect("failed to run svnadmin create");
    assert!(status.success(), "svnadmin create failed");

    // Enable revprop changes (needed for some tests).
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

/// Check out an SVN working copy from the given URL.
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

/// Commit a file to SVN via the working copy. Returns the new revision number.
///
/// Writes `content` to `filename` inside `wc_path`, stages it with `svn add`
/// (if unversioned), and commits with the given message.
fn svn_commit_file(wc_path: &Path, filename: &str, content: &str, message: &str) -> i64 {
    let file_path = wc_path.join(filename);

    // Ensure parent directories exist.
    if let Some(parent) = file_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).unwrap();
            // svn add each intermediate directory that is new.
            let mut rel = PathBuf::new();
            for component in Path::new(filename).parent().unwrap().components() {
                rel = rel.join(component);
                let abs = wc_path.join(&rel);
                let status_out = Command::new("svn")
                    .args(["status", abs.to_str().unwrap()])
                    .output()
                    .unwrap();
                let status_str = String::from_utf8_lossy(&status_out.stdout);
                if status_str.contains('?') || !abs.join(".svn").exists() {
                    let _ = Command::new("svn")
                        .args(["add", "--depth=empty", abs.to_str().unwrap()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
            }
        }
    }

    let is_new = !file_path.exists() || {
        let out = Command::new("svn")
            .args(["status", file_path.to_str().unwrap()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains('?')
    };

    std::fs::write(&file_path, content).unwrap();

    if is_new {
        let status = Command::new("svn")
            .args(["add", file_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "svn add failed for {}", filename);
    }

    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .output()
        .expect("failed to run svn commit");
    assert!(
        output.status.success(),
        "svn commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse "Committed revision N." from stdout.
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

/// Commit multiple files in a single SVN commit. Returns the new revision.
fn svn_commit_files(wc_path: &Path, files: &[(&str, &str)], message: &str) -> i64 {
    for (filename, content) in files {
        let file_path = wc_path.join(filename);
        if let Some(parent) = file_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).unwrap();
                // svn add parent dirs.
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
    }

    // svn add all new files.
    for (filename, _) in files {
        let file_path = wc_path.join(filename);
        let _ = Command::new("svn")
            .args(["add", "--force", file_path.to_str().unwrap()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            wc_path.to_str().unwrap(),
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
                .unwrap();
        }
    }
    panic!("could not parse committed revision from: {}", stdout);
}

/// Build a `PersonalConfig` suitable for testing, pointing at the given SVN URL
/// and temp directories.
fn make_test_config(svn_url: &str, data_dir: &Path) -> PersonalConfig {
    PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 5,
            log_level: "debug".into(),
            data_dir: data_dir.to_path_buf(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.into(),
            username: String::new(),
            password_env: "REPOSYNC_TEST_SVN_PW".into(),
            password: Some(String::new()),
        },
        github: PersonalGitHubConfig {
            api_url: "https://api.github.com".into(),
            repo: "test/test-repo".into(),
            token_env: "REPOSYNC_TEST_GH_TOKEN".into(),
            default_branch: "main".into(),
            auto_create: false,
            private: true,
            token: None,
            git_base_url: None,
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "testuser".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    }
}

/// Set up a Git working repo with a bare repo as "origin" for local push.
/// Returns `GitClient`.
///
/// After the initial commit, ensures the default branch is named "main"
/// regardless of the system's `init.defaultBranch` setting.
fn setup_git_with_bare_origin(work_dir: &Path, bare_dir: &Path) -> GitClient {
    // Create a bare repo as "origin".
    git2::Repository::init_bare(bare_dir).expect("failed to init bare repo");

    // Init the working repo.
    let git_client = GitClient::init(work_dir).expect("failed to init git repo");

    // Add the bare repo as the "origin" remote.
    let repo = git2::Repository::open(work_dir).expect("failed to open repo");
    repo.remote("origin", bare_dir.to_str().unwrap())
        .expect("failed to add origin remote");

    // Create an initial commit so we have a HEAD.
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

    // Rename the default branch to "main" if it isn't already.
    // git2::Repository::init may create "master" depending on system config.
    {
        let repo = git2::Repository::open(work_dir).unwrap();
        let head = repo.head().unwrap();
        let head_name = head.name().unwrap_or("");
        if head_name != "refs/heads/main" {
            // Rename the branch to "main".
            let mut branch = repo
                .find_branch(
                    head_name.strip_prefix("refs/heads/").unwrap_or("master"),
                    git2::BranchType::Local,
                )
                .unwrap();
            branch.rename("main", true).unwrap();
        }
    }

    // Push the initial commit to origin to establish the branch.
    git_client
        .push("origin", "main")
        .expect("failed to push initial commit to origin");

    git_client
}

/// Create and initialize a Database at the given path.
fn setup_db(path: &Path) -> Database {
    let db = Database::new(path).expect("failed to create database");
    db.initialize()
        .expect("failed to initialize database schema");
    db
}

/// Count commits in the git repo by walking from HEAD.
fn count_git_commits(repo_path: &Path) -> usize {
    let repo = git2::Repository::open(repo_path).unwrap();
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return 0,
    };
    let mut revwalk = repo.revwalk().unwrap();
    revwalk.push(head.target().unwrap()).unwrap();
    revwalk.count()
}

/// Read the message of the Nth commit from HEAD (0 = HEAD, 1 = HEAD~1, etc.).
fn get_git_commit_message(repo_path: &Path, index: usize) -> String {
    let repo = git2::Repository::open(repo_path).unwrap();
    let mut revwalk = repo.revwalk().unwrap();
    revwalk.push_head().unwrap();
    revwalk
        .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .unwrap();
    let oid = revwalk.nth(index).unwrap().unwrap();
    let commit = repo.find_commit(oid).unwrap();
    commit.message().unwrap_or("").to_string()
}

// ===========================================================================
// Test 1: Basic SVN-to-Git sync (3 revisions)
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_basic_sync() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Commit 3 files in 3 separate SVN revisions.
    svn_commit_file(&wc_path, "file1.txt", "content one", "Add file1");
    svn_commit_file(&wc_path, "file2.txt", "content two", "Add file2");
    svn_commit_file(&wc_path, "file3.txt", "content three", "Add file3");

    // Set up Git repo with bare origin.
    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    // Set up database.
    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);

    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);

    // Run sync.
    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 3, "expected 3 revisions synced");

    // Verify Git repo has 3 + 1 (initial) = 4 commits.
    assert_eq!(count_git_commits(&git_work_dir), 4);

    // Verify watermark advanced to rev 3.
    let watermark = db_arc.get_watermark("svn_rev").unwrap();
    assert_eq!(watermark.as_deref(), Some("3"));

    // Verify commit_map has 3 entries.
    let commit_map = db_arc.list_commit_map(10).unwrap();
    assert_eq!(commit_map.len(), 3);

    // Verify Git commit messages contain SVN-Revision trailers.
    // The most recent commit (index 0) is r3, so check it.
    let msg = get_git_commit_message(&git_work_dir, 0);
    assert!(
        msg.contains("SVN-Revision: r3"),
        "expected SVN-Revision trailer in: {}",
        msg
    );
    assert!(
        msg.contains("[reposync]"),
        "expected sync marker in: {}",
        msg
    );

    // Verify files exist in git working directory.
    assert!(git_work_dir.join("file1.txt").exists());
    assert!(git_work_dir.join("file2.txt").exists());
    assert!(git_work_dir.join("file3.txt").exists());
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("file1.txt")).unwrap(),
        "content one"
    );
}

// ===========================================================================
// Test 2: Echo suppression
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_marker_without_receipt_is_applied() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    svn_commit_file(&wc_path, "normal1.txt", "hello", "Normal commit 1");
    svn_commit_file(
        &wc_path,
        "echo.txt",
        "echoed content",
        "Echoed commit [reposync] synced from Git",
    );
    svn_commit_file(&wc_path, "normal2.txt", "world", "Normal commit 2");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");

    assert_eq!(
        synced, 3,
        "marker-only SVN revisions must be applied once without a repo-scoped receipt"
    );
    let watermark = db_arc.get_watermark("svn_rev").unwrap();
    assert_eq!(watermark.as_deref(), Some("3"));
    let commit_map = db_arc.list_commit_map(10).unwrap();
    assert_eq!(commit_map.len(), 3);
}

#[tokio::test]
async fn test_svn_to_git_receipt_backed_echo_suppression() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    svn_commit_file(&wc_path, "normal1.txt", "hello", "Normal commit 1");
    svn_commit_file(
        &wc_path,
        "echo.txt",
        "echoed content",
        "Echoed commit [reposync] synced from Git",
    );
    svn_commit_file(&wc_path, "normal2.txt", "world", "Normal commit 2");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let now = Utc::now();
    db.insert_sync_record(&SyncRecord {
        id: "personal-echo-receipt-test".to_string(),
        repo_id: Some("personal".to_string()),
        svn_revision: Some(2),
        git_hash: Some("b".repeat(40)),
        direction: SyncDirection::GitToSvn,
        author: "test".into(),
        message: "personal git-to-svn echo".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .expect("failed to seed git-to-svn echo receipt");

    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");

    assert_eq!(
        synced, 2,
        "receipt-backed SVN echo must skip without re-applying"
    );
    let watermark = db_arc.get_watermark("svn_rev").unwrap();
    assert_eq!(watermark.as_deref(), Some("3"));
    let commit_map = db_arc.list_commit_map(10).unwrap();
    assert_eq!(commit_map.len(), 2);
}

// ===========================================================================
// Test 3: Idempotency
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_idempotency() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    svn_commit_file(&wc_path, "a.txt", "aaa", "Add a");
    svn_commit_file(&wc_path, "b.txt", "bbb", "Add b");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(
        svn_client.clone(),
        git_arc.clone(),
        db_arc.clone(),
        config.clone(),
    );

    // First sync: 2 revisions.
    let synced1 = syncer.sync().await.expect("first sync failed");
    assert_eq!(synced1, 2);

    // Second sync: nothing new.
    let synced2 = syncer.sync().await.expect("second sync failed");
    assert_eq!(synced2, 0);

    // Add one more SVN revision.
    svn_commit_file(&wc_path, "c.txt", "ccc", "Add c");

    // Third sync: 1 new revision.
    let syncer2 = SvnToGitSync::new(
        SvnClient::new(&svn_url, "", ""),
        git_arc.clone(),
        db_arc.clone(),
        config,
    );
    let synced3 = syncer2.sync().await.expect("third sync failed");
    assert_eq!(synced3, 1);

    // Total: 3 + 1 (initial) = 4 commits.
    assert_eq!(count_git_commits(&git_work_dir), 4);
}

// ===========================================================================
// Test 4: Watermark recovery
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_watermark_recovery() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    svn_commit_file(&wc_path, "x.txt", "x", "Rev 1");
    svn_commit_file(&wc_path, "y.txt", "y", "Rev 2");
    svn_commit_file(&wc_path, "z.txt", "z", "Rev 3");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);

    // Manually set watermark to 2, pretending revisions 1 and 2 were already synced.
    db.set_watermark("svn_rev", "2").unwrap();

    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);

    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 1, "expected only revision 3 to be synced");

    let watermark = db_arc.get_watermark("svn_rev").unwrap();
    assert_eq!(watermark.as_deref(), Some("3"));

    // Git should have the initial commit + 1 synced = 2 commits.
    assert_eq!(count_git_commits(&git_work_dir), 2);
}

// ===========================================================================
// Test 5: Multi-file commit (single revision, multiple files)
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_multifile_commit() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Commit 3 files in a single SVN revision.
    let rev = svn_commit_files(
        &wc_path,
        &[
            ("alpha.txt", "alpha content"),
            ("beta.txt", "beta content"),
            ("gamma.txt", "gamma content"),
        ],
        "Add three files at once",
    );
    assert_eq!(rev, 1);

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 1, "expected 1 revision synced");

    // Git should have 2 commits (initial + 1 synced).
    assert_eq!(count_git_commits(&git_work_dir), 2);

    // All 3 files should exist.
    assert!(git_work_dir.join("alpha.txt").exists());
    assert!(git_work_dir.join("beta.txt").exists());
    assert!(git_work_dir.join("gamma.txt").exists());
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("beta.txt")).unwrap(),
        "beta content"
    );
}

// ===========================================================================
// Test 6: File modification
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_file_modification() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Rev 1: create file.
    svn_commit_file(&wc_path, "data.txt", "version 1", "Add data.txt");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(
        svn_client.clone(),
        git_arc.clone(),
        db_arc.clone(),
        config.clone(),
    );
    let synced1 = syncer.sync().await.expect("first sync failed");
    assert_eq!(synced1, 1);

    // Verify v1.
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("data.txt")).unwrap(),
        "version 1"
    );

    // Rev 2: modify file.
    // We need to write directly to the working copy (svn_commit_file handles this).
    std::fs::write(wc_path.join("data.txt"), "version 2").unwrap();
    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            "Update data.txt to v2",
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    // Sync again.
    let syncer2 = SvnToGitSync::new(
        SvnClient::new(&svn_url, "", ""),
        git_arc.clone(),
        db_arc.clone(),
        config,
    );
    let synced2 = syncer2.sync().await.expect("second sync failed");
    assert_eq!(synced2, 1);

    // Verify v2.
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("data.txt")).unwrap(),
        "version 2"
    );
}

// ===========================================================================
// Test 7: Binary file
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_binary_file() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Create a binary file with known bytes.
    let binary_data: Vec<u8> = (0..=255).collect();
    let bin_path = wc_path.join("data.bin");
    std::fs::write(&bin_path, &binary_data).unwrap();
    let _ = Command::new("svn")
        .args(["add", bin_path.to_str().unwrap()])
        .status()
        .unwrap();
    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            "Add binary file",
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 1);

    // Verify binary file matches exactly.
    let git_binary = std::fs::read(git_work_dir.join("data.bin")).unwrap();
    assert_eq!(git_binary, binary_data);
}

// ===========================================================================
// Test 8: Nested directories
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_nested_directories() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Create nested directory structure: src/main/java/App.java
    svn_commit_file(
        &wc_path,
        "src/main/java/App.java",
        "public class App {}",
        "Add nested Java file",
    );

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 1);

    // Verify the nested file exists with correct content.
    let app_path = git_work_dir.join("src/main/java/App.java");
    assert!(app_path.exists(), "nested file should exist in git repo");
    assert_eq!(
        std::fs::read_to_string(&app_path).unwrap(),
        "public class App {}"
    );
}

// ===========================================================================
// Test 9: Empty repo (no commits) -- no crash
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_empty_repo_no_crash() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer
        .sync()
        .await
        .expect("sync on empty repo should not crash");
    assert_eq!(synced, 0, "nothing to sync in an empty repo");

    // Git should only have the initial commit.
    assert_eq!(count_git_commits(&git_work_dir), 1);
}

// ===========================================================================
// Test 10: Git-to-SVN basic replay (manual simulation)
// ===========================================================================

#[tokio::test]
async fn test_git_to_svn_basic_replay() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Create initial SVN content.
    svn_commit_file(&wc_path, "readme.txt", "Hello SVN", "Initial readme");

    // Create a Git repo with the same initial content.
    let git_work_dir = tmp.path().join("git_work");
    std::fs::create_dir_all(&git_work_dir).unwrap();
    let git_client = GitClient::init(&git_work_dir).unwrap();
    std::fs::write(git_work_dir.join("readme.txt"), "Hello SVN").unwrap();
    git_client
        .commit(
            "initial commit",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();

    // Simulate a "PR merge" by adding a new file to the Git repo.
    std::fs::write(git_work_dir.join("feature.txt"), "New feature from Git").unwrap();
    let git_oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = git_oid.to_string();

    // Format the commit message for Git-to-SVN direction.
    let config = make_test_config(&svn_url, tmp.path());
    let formatter = CommitFormatter::new(&config.commit_format);
    let svn_commit_msg =
        formatter.format_git_to_svn("Add feature.txt", &git_sha, 1, "feature/new-feature");

    // Copy the new file from Git working dir to SVN working copy.
    std::fs::copy(
        git_work_dir.join("feature.txt"),
        wc_path.join("feature.txt"),
    )
    .unwrap();

    // svn add + commit in the SVN working copy.
    let _ = Command::new("svn")
        .args(["add", wc_path.join("feature.txt").to_str().unwrap()])
        .status()
        .unwrap();

    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            &svn_commit_msg,
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "svn commit failed");

    // Verify SVN now has the new file.
    let svn_client = SvnClient::new(&svn_url, "", "");
    let info = svn_client.info().await.unwrap();
    assert_eq!(info.latest_rev, 2, "SVN should be at revision 2");

    // Verify the SVN commit message contains the [reposync] marker.
    let log_entries = svn_client.log(2, 2).await.unwrap();
    assert_eq!(log_entries.len(), 1);
    assert!(
        CommitFormatter::is_sync_marker(&log_entries[0].message),
        "SVN commit message should contain [reposync] marker"
    );
    assert!(
        log_entries[0].message.contains(&git_sha),
        "SVN commit message should contain the Git SHA"
    );
}

// ===========================================================================
// Test 11: CommitFormatter roundtrip
// ===========================================================================

#[test]
fn test_commit_formatter_roundtrip() {
    let config = CommitFormatConfig::default();
    let formatter = CommitFormatter::new(&config);

    // SVN -> Git direction.
    let svn_to_git_msg =
        formatter.format_svn_to_git("Fix bug #42", 123, "alice", "2025-06-15T10:00:00Z");
    assert!(
        svn_to_git_msg.contains("[reposync]"),
        "SVN-to-Git message should contain sync marker"
    );
    assert!(
        svn_to_git_msg.contains("SVN-Revision: r123"),
        "should contain SVN-Revision trailer"
    );
    assert!(
        svn_to_git_msg.contains("SVN-Author: alice"),
        "should contain SVN-Author trailer"
    );
    assert!(
        svn_to_git_msg.contains("Fix bug #42"),
        "should contain original message"
    );
    assert!(
        CommitFormatter::is_sync_marker(&svn_to_git_msg),
        "is_sync_marker should return true for SVN-to-Git formatted message"
    );

    // Git -> SVN direction.
    let git_to_svn_msg =
        formatter.format_git_to_svn("Add search endpoint", "abc123def456", 42, "feature/search");
    assert!(
        git_to_svn_msg.contains("[reposync]"),
        "Git-to-SVN message should contain sync marker"
    );
    assert!(
        git_to_svn_msg.contains("Git-SHA: abc123def456"),
        "should contain Git-SHA trailer"
    );
    assert!(
        git_to_svn_msg.contains("PR-Number: #42"),
        "should contain PR-Number trailer"
    );
    assert!(
        git_to_svn_msg.contains("PR-Branch: feature/search"),
        "should contain PR-Branch trailer"
    );
    assert!(
        git_to_svn_msg.contains("Add search endpoint"),
        "should contain original message"
    );
    assert!(
        CommitFormatter::is_sync_marker(&git_to_svn_msg),
        "is_sync_marker should return true for Git-to-SVN formatted message"
    );

    // Verify extraction methods.
    assert_eq!(CommitFormatter::extract_svn_rev(&svn_to_git_msg), Some(123));
    assert_eq!(
        CommitFormatter::extract_git_sha(&git_to_svn_msg),
        Some("abc123def456".to_string())
    );
    assert_eq!(
        CommitFormatter::extract_pr_number(&git_to_svn_msg),
        Some(42)
    );

    // Verify non-sync messages are not detected.
    assert!(!CommitFormatter::is_sync_marker("Regular commit message"));
    assert!(!CommitFormatter::is_sync_marker("Fix bug"));
}

// ===========================================================================
// Test 12: Database watermark and commit_map
// ===========================================================================

#[test]
fn test_database_watermark_and_commit_map() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);

    // Watermark: initially None.
    assert!(db.get_watermark("svn_rev").unwrap().is_none());

    // Set watermark.
    db.set_watermark("svn_rev", "100").unwrap();
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("100"));

    // Update watermark (upsert).
    db.set_watermark("svn_rev", "200").unwrap();
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("200"));

    // Multiple independent watermarks.
    db.set_watermark("git_sha", "abc123").unwrap();
    assert_eq!(
        db.get_watermark("git_sha").unwrap().as_deref(),
        Some("abc123")
    );
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("200"));

    // Insert commit_map entries.
    let id1 = db
        .insert_commit_map(
            100,
            "aaa111",
            "svn_to_git",
            "alice",
            "Alice <alice@test.com>",
        )
        .unwrap();
    assert!(id1 > 0);

    let id2 = db
        .insert_commit_map(101, "bbb222", "svn_to_git", "bob", "Bob <bob@test.com>")
        .unwrap();
    assert!(id2 > id1);

    let id3 = db
        .insert_commit_map(
            50,
            "ccc333",
            "git_to_svn",
            "alice",
            "Alice <alice@test.com>",
        )
        .unwrap();
    assert!(id3 > id2);

    // is_svn_rev_synced: true for existing revisions.
    assert!(db.is_svn_rev_synced(100).unwrap());
    assert!(db.is_svn_rev_synced(101).unwrap());
    assert!(db.is_svn_rev_synced(50).unwrap());

    // is_svn_rev_synced: false for non-existent revisions.
    assert!(!db.is_svn_rev_synced(999).unwrap());
    assert!(!db.is_svn_rev_synced(0).unwrap());

    // Look up Git SHA by SVN rev.
    assert_eq!(
        db.get_git_sha_for_svn_rev(100).unwrap().as_deref(),
        Some("aaa111")
    );
    assert_eq!(
        db.get_git_sha_for_svn_rev(101).unwrap().as_deref(),
        Some("bbb222")
    );
    assert!(db.get_git_sha_for_svn_rev(999).unwrap().is_none());

    // Look up SVN rev by Git SHA.
    assert_eq!(db.get_svn_rev_for_git_sha("aaa111").unwrap(), Some(100));
    assert_eq!(db.get_svn_rev_for_git_sha("ccc333").unwrap(), Some(50));
    assert!(db.get_svn_rev_for_git_sha("nonexistent").unwrap().is_none());

    // List commit_map (ordered by id DESC).
    let all = db.list_commit_map(10).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].svn_rev, 50); // most recent insert
    assert_eq!(all[1].svn_rev, 101);
    assert_eq!(all[2].svn_rev, 100);

    // PR sync log.
    assert!(!db.is_pr_synced("merge_sha_1").unwrap());
    let pr_id = db
        .insert_pr_sync(
            42,
            "Add search",
            "feature/search",
            "merge_sha_1",
            "squash",
            3,
        )
        .unwrap();
    assert!(db.is_pr_synced("merge_sha_1").unwrap());
    assert!(!db.is_pr_synced("other_sha").unwrap());

    db.complete_pr_sync(pr_id, 200, 202).unwrap();
    let pr_entries = db.list_pr_syncs(10).unwrap();
    assert_eq!(pr_entries.len(), 1);
    assert_eq!(pr_entries[0].status, "completed");
    assert_eq!(pr_entries[0].svn_rev_start, Some(200));
    assert_eq!(pr_entries[0].svn_rev_end, Some(202));

    // Audit log.
    db.insert_audit_log(
        "test_action",
        Some("svn_to_git"),
        Some(100),
        Some("aaa111"),
        Some("alice"),
        Some("test details"),
        true,
    )
    .unwrap();
    let audit = db.list_audit_log(10, 0).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "test_action");
    assert_eq!(audit[0].svn_rev, Some(100));
    assert!(audit[0].success);
}

// ===========================================================================
// Test 13: Full SVN-to-Git cycle with metadata verification
// ===========================================================================

#[tokio::test]
async fn test_full_svn_to_git_cycle_with_metadata() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Commit as a specific user with a specific message.
    // Note: with file:// repos, the SVN author is typically empty or the
    // local user unless set via revprop. We'll set it after the commit.
    let rev = svn_commit_file(&wc_path, "bugfix.py", "print('fixed')", "fix bug #42");

    // Set the synthetic author directly on this disposable repository. The
    // isolated container cannot execute the test-only revprop hook, while
    // svnadmin's default path does not invoke it.
    let author_file = tmp.path().join("svn-author");
    std::fs::write(&author_file, "alice").unwrap();
    let status = Command::new("svnadmin")
        .args([
            "setrevprop",
            tmp.path().join("svn_repo").to_str().unwrap(),
            "-r",
            &rev.to_string(),
            "svn:author",
            author_file.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "failed to set svn:author revprop");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 1);

    // Read the Git commit message (index 0 = HEAD = the synced commit).
    let msg = get_git_commit_message(&git_work_dir, 0);

    // Verify metadata in commit message.
    assert!(
        msg.contains("[reposync]"),
        "commit message should contain [reposync] marker, got: {}",
        msg
    );
    assert!(
        msg.contains("SVN-Revision: r1"),
        "commit message should contain SVN-Revision: r1, got: {}",
        msg
    );
    assert!(
        msg.contains("SVN-Author: alice"),
        "commit message should contain SVN-Author: alice, got: {}",
        msg
    );
    assert!(
        msg.contains("fix bug #42"),
        "commit message should contain original message 'fix bug #42', got: {}",
        msg
    );

    // Verify the commit_map records the correct metadata.
    let commit_map = db_arc.list_commit_map(10).unwrap();
    assert_eq!(commit_map.len(), 1);
    assert_eq!(commit_map[0].svn_rev, 1);
    assert_eq!(commit_map[0].direction, "svn_to_git");
    assert_eq!(commit_map[0].svn_author, "alice");
    assert!(commit_map[0].git_author.contains("Test User"));

    // Verify the audit log was written.
    let audit = db_arc.list_audit_log(10, 0).unwrap();
    assert!(!audit.is_empty(), "audit log should have entries");
    assert_eq!(audit[0].action, "svn_to_git_sync");

    // Verify the file exists in git with correct content.
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("bugfix.py")).unwrap(),
        "print('fixed')"
    );
}

// ===========================================================================
// Test 14: SVN-to-Git with file deletion
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_file_deletion() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Rev 1: Add two files.
    svn_commit_files(
        &wc_path,
        &[("keep.txt", "keep me"), ("delete_me.txt", "goodbye")],
        "Add two files",
    );

    // Rev 2: Delete one file via svn rm.
    let _ = Command::new("svn")
        .args(["rm", wc_path.join("delete_me.txt").to_str().unwrap()])
        .status()
        .unwrap();
    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            "Remove delete_me.txt",
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let synced = syncer.sync().await.expect("sync failed");
    assert_eq!(synced, 2);

    // After syncing both revisions, the kept file should exist and the
    // deleted file should be gone (stale-file pruning removes it).
    assert!(git_work_dir.join("keep.txt").exists());
    assert_eq!(
        std::fs::read_to_string(git_work_dir.join("keep.txt")).unwrap(),
        "keep me"
    );
    assert!(
        !git_work_dir.join("delete_me.txt").exists(),
        "deleted SVN file should be removed from Git working tree"
    );
}

// ===========================================================================
// Test 15: SvnClient.info() and .log() against a real local repo
// ===========================================================================

#[tokio::test]
async fn test_svn_client_info_and_log() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Empty repo: revision 0.
    let svn_client = SvnClient::new(&svn_url, "", "");
    let info = svn_client.info().await.unwrap();
    assert_eq!(info.latest_rev, 0);

    // Add some commits.
    svn_commit_file(&wc_path, "a.txt", "aaa", "First commit");
    svn_commit_file(&wc_path, "b.txt", "bbb", "Second commit");

    let info2 = svn_client.info().await.unwrap();
    assert_eq!(info2.latest_rev, 2);
    assert!(info2.url.contains("svn_repo"));

    // Log: all revisions.
    let log = svn_client.log(1, 2).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].revision, 1);
    assert_eq!(log[0].message, "First commit");
    assert_eq!(log[1].revision, 2);
    assert_eq!(log[1].message, "Second commit");

    // Log: single revision.
    let log_single = svn_client.log(2, 2).await.unwrap();
    assert_eq!(log_single.len(), 1);
    assert_eq!(log_single[0].revision, 2);
}

// ===========================================================================
// Test 16: SvnClient.export() against a real local repo
// ===========================================================================

#[tokio::test]
async fn test_svn_client_export() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    svn_commit_file(&wc_path, "hello.txt", "hello world", "Add hello");
    svn_commit_file(&wc_path, "sub/nested.txt", "nested content", "Add nested");

    let svn_client = SvnClient::new(&svn_url, "", "");

    // Export revision 2 to a temp dir.
    let export_dir = tmp.path().join("export");
    svn_client.export("", 2, &export_dir).await.unwrap();

    assert!(export_dir.join("hello.txt").exists());
    assert_eq!(
        std::fs::read_to_string(export_dir.join("hello.txt")).unwrap(),
        "hello world"
    );
    assert!(export_dir.join("sub/nested.txt").exists());
    assert_eq!(
        std::fs::read_to_string(export_dir.join("sub/nested.txt")).unwrap(),
        "nested content"
    );

    // Export should NOT contain .svn metadata.
    assert!(!export_dir.join(".svn").exists());
}

// ===========================================================================
// Test 17: GitClient operations (init, commit, push to bare, head_sha)
// ===========================================================================

#[test]
fn test_git_client_init_commit_push() {
    let tmp = TempDir::new().unwrap();
    let work_dir = tmp.path().join("repo");
    let bare_dir = tmp.path().join("bare.git");

    let git_client = setup_git_with_bare_origin(&work_dir, &bare_dir);

    // Should have 1 commit (initial).
    assert_eq!(count_git_commits(&work_dir), 1);

    // Add a file and commit.
    std::fs::write(work_dir.join("test.txt"), "test content").unwrap();
    let oid = git_client
        .commit(
            "add test file",
            "Alice",
            "alice@test.com",
            "Alice",
            "alice@test.com",
        )
        .unwrap();
    assert!(!oid.is_zero());
    assert_eq!(git_client.get_head_sha().unwrap(), oid.to_string());

    // Push to bare origin.
    git_client.push("origin", "main").unwrap();

    // Verify push landed in bare repo.
    let bare_repo = git2::Repository::open_bare(&bare_dir).unwrap();
    let bare_head = bare_repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(bare_head.id().to_string(), oid.to_string());

    // Now 2 commits total.
    assert_eq!(count_git_commits(&work_dir), 2);
}

// ===========================================================================
// Test 18: Database in-memory with full schema
// ===========================================================================

#[test]
fn test_database_in_memory_full_schema() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();

    // Verify all operations work on in-memory DB.
    db.set_watermark("test", "42").unwrap();
    assert_eq!(db.get_watermark("test").unwrap().as_deref(), Some("42"));

    let id = db
        .insert_commit_map(1, "sha1", "svn_to_git", "user", "User <u@t.com>")
        .unwrap();
    assert!(id > 0);
    assert!(db.is_svn_rev_synced(1).unwrap());
    assert!(!db.is_svn_rev_synced(2).unwrap());

    db.insert_audit_log("test", None, None, None, None, None, true)
        .unwrap();
    assert_eq!(db.count_audit_log().unwrap(), 1);

    let pr_id = db
        .insert_pr_sync(10, "Title", "branch", "merge_sha", "squash", 1)
        .unwrap();
    assert!(db.is_pr_synced("merge_sha").unwrap());
    db.complete_pr_sync(pr_id, 1, 1).unwrap();
}

// ===========================================================================
// Test 19: Multiple sequential syncs build correct history
// ===========================================================================

#[tokio::test]
async fn test_svn_to_git_sequential_syncs_history() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    // Sync in 3 separate passes, adding 1 revision each time.
    for i in 1..=3 {
        svn_commit_file(
            &wc_path,
            &format!("file{}.txt", i),
            &format!("content {}", i),
            &format!("Add file{}", i),
        );

        let syncer = SvnToGitSync::new(
            SvnClient::new(&svn_url, "", ""),
            git_arc.clone(),
            db_arc.clone(),
            config.clone(),
        );
        let synced = syncer.sync().await.expect("sync failed");
        assert_eq!(synced, 1, "pass {} should sync exactly 1 revision", i);
    }

    // Total: 1 (initial) + 3 (synced) = 4 commits.
    assert_eq!(count_git_commits(&git_work_dir), 4);

    // Watermark at 3.
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some("3")
    );

    // commit_map has 3 entries.
    assert_eq!(db_arc.list_commit_map(10).unwrap().len(), 3);

    // All files exist.
    for i in 1..=3 {
        assert!(git_work_dir.join(format!("file{}.txt", i)).exists());
    }
}

// ===========================================================================
// Test 20: Concurrent-safe database access (Arc<Database>)
// ===========================================================================

#[tokio::test]
async fn test_database_concurrent_access() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("concurrent.db");
    let db = Arc::new(setup_db(&db_path));

    // Spawn multiple tasks that write to the database concurrently.
    let mut handles = Vec::new();
    for i in 0..10 {
        let db_clone = db.clone();
        handles.push(tokio::spawn(async move {
            db_clone
                .set_watermark(&format!("source_{}", i), &format!("{}", i * 10))
                .unwrap();
            db_clone
                .insert_commit_map(
                    i as i64,
                    &format!("sha_{}", i),
                    "svn_to_git",
                    "user",
                    "User <u@t.com>",
                )
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all watermarks were written.
    for i in 0..10 {
        let wm = db.get_watermark(&format!("source_{}", i)).unwrap().unwrap();
        assert_eq!(wm, format!("{}", i * 10));
    }

    // Verify all commit_map entries exist.
    let all = db.list_commit_map(20).unwrap();
    assert_eq!(all.len(), 10);
}

// ===========================================================================
// Issue #36: personal.log_level and personal.log tests
// ===========================================================================

/// personal.log_level defaults to "info" when not set in config.
#[test]
fn test_personal_log_level_defaults_to_info() {
    let toml_str = r#"
[personal]

[svn]
url = "https://svn.example.com/repos/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
repo = "jdoe/mirror"
token_env = "GITHUB_TOKEN"

[developer]
name = "John Doe"
email = "jdoe@example.com"
svn_username = "jdoe"
"#;
    let config: PersonalConfig = toml::from_str(toml_str).expect("parse config");
    assert_eq!(
        config.personal.log_level, "info",
        "log_level should default to 'info'"
    );
}

/// personal.log_level is respected when explicitly set.
#[test]
fn test_personal_log_level_explicit() {
    let toml_str = r#"
[personal]
log_level = "debug"

[svn]
url = "https://svn.example.com/repos/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
repo = "jdoe/mirror"
token_env = "GITHUB_TOKEN"

[developer]
name = "John Doe"
email = "jdoe@example.com"
svn_username = "jdoe"
"#;
    let config: PersonalConfig = toml::from_str(toml_str).expect("parse config");
    assert_eq!(
        config.personal.log_level, "debug",
        "log_level should be 'debug' when explicitly set"
    );
}

/// EnvFilter honors log_level from PersonalConfig (not RUST_LOG).
/// This test verifies that an EnvFilter created with a config-derived
/// log_level actually parses correctly (does not panic).
#[test]
fn test_env_filter_from_config_log_level() {
    use tracing_subscriber::EnvFilter;
    for level in &["trace", "debug", "info", "warn", "error"] {
        let filter = EnvFilter::new(level);
        // If this doesn't panic, the filter is valid.
        let _ = format!("{:?}", filter);
    }
}

/// personal.log file path is correctly derived from data_dir.
#[test]
fn test_personal_log_file_path() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("test_data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let expected_log = data_dir.join("personal.log");
    // The daemon writes to {data_dir}/personal.log. Verify the path
    // is constructable and the parent directory exists.
    assert!(data_dir.exists());
    assert_eq!(expected_log.file_name().unwrap(), "personal.log");
    assert_eq!(expected_log.parent().unwrap(), data_dir);
}

// ===========================================================================
// Issue #37: runtime personal logging verification tests
// ===========================================================================

/// Runtime test: logs are actually written to `{data_dir}/personal.log`.
///
/// This creates the same tracing layers as `init_tracing`, scoped with
/// `tracing::subscriber::with_default` (not global `.init()`), emits log
/// events, flushes, and verifies the file has content.
#[test]
fn test_runtime_personal_log_file_written() {
    use tracing_subscriber::layer::SubscriberExt;

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("runtime_log_test");
    std::fs::create_dir_all(&data_dir).unwrap();

    let log_file = data_dir.join("personal.log");

    // Build a subscriber with a file appender (same as init_tracing).
    let file_appender = tracing_appender::rolling::never(&data_dir, "personal.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::new("info");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_target(true)
        .with_ansi(false);

    let subscriber = tracing_subscriber::registry().with(filter).with(file_layer);

    // Use a scoped subscriber (not global .init()) so tests don't conflict.
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("runtime_log_test: file logging is active");
        tracing::warn!("runtime_log_test: this is a warning");
    });

    // Drop the guard to flush the non-blocking writer.
    drop(guard);

    // Allow a tiny delay for the non-blocking writer to flush.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Verify the log file exists and has content.
    assert!(
        log_file.exists(),
        "personal.log should exist at {:?}",
        log_file
    );
    let contents = std::fs::read_to_string(&log_file).unwrap();
    assert!(!contents.is_empty(), "personal.log should not be empty");
    assert!(
        contents.contains("runtime_log_test: file logging is active"),
        "personal.log should contain the info-level message, got: {}",
        contents
    );
    assert!(
        contents.contains("runtime_log_test: this is a warning"),
        "personal.log should contain the warning message, got: {}",
        contents
    );
}

/// Runtime test: `personal.log_level` filtering is honored.
///
/// When the config says `log_level = "warn"`, debug and info events
/// should NOT appear in the log file — only warn and above.
#[test]
fn test_runtime_personal_log_level_filtering() {
    use tracing_subscriber::layer::SubscriberExt;

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("level_filter_test");
    std::fs::create_dir_all(&data_dir).unwrap();

    let log_file = data_dir.join("personal.log");

    let file_appender = tracing_appender::rolling::never(&data_dir, "personal.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Use "warn" level — should filter out debug and info.
    let filter = tracing_subscriber::EnvFilter::new("warn");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_target(true)
        .with_ansi(false);

    let subscriber = tracing_subscriber::registry().with(filter).with(file_layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!("level_filter_test: debug message SHOULD NOT APPEAR");
        tracing::info!("level_filter_test: info message SHOULD NOT APPEAR");
        tracing::warn!("level_filter_test: warn message SHOULD APPEAR");
        tracing::error!("level_filter_test: error message SHOULD APPEAR");
    });

    drop(guard);
    std::thread::sleep(std::time::Duration::from_millis(100));

    let contents = std::fs::read_to_string(&log_file).unwrap();

    // Warn and error should be present.
    assert!(
        contents.contains("warn message SHOULD APPEAR"),
        "warn-level message should be in log file, got: {}",
        contents
    );
    assert!(
        contents.contains("error message SHOULD APPEAR"),
        "error-level message should be in log file, got: {}",
        contents
    );

    // Debug and info should NOT be present.
    assert!(
        !contents.contains("debug message SHOULD NOT APPEAR"),
        "debug-level message should be filtered out when log_level=warn, got: {}",
        contents
    );
    assert!(
        !contents.contains("info message SHOULD NOT APPEAR"),
        "info-level message should be filtered out when log_level=warn, got: {}",
        contents
    );
}

// ===========================================================================
// Issue #38: spawn-based black-box personal logging tests
// ===========================================================================

/// Return the path to the compiled `reposync-personal` binary.
/// Looks in `target/debug` which `cargo test` populates.
fn personal_binary_path() -> PathBuf {
    // The binary sits next to the test binary's directory.
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove `deps`
    path.push("reposync-personal");
    path
}

/// Write a minimal TOML config that `PersonalConfig::load_from_file` can parse.
/// The config points at `data_dir` for log output.  SVN/GitHub values are
/// syntactically valid but don't need to resolve.
fn write_test_config(path: &Path, data_dir: &Path, log_level: &str) {
    let toml = format!(
        r#"[personal]
log_level = "{log_level}"
data_dir = "{data_dir}"

[svn]
url = "file:///tmp/nonexistent_svn_repo"
username = "testuser"
password_env = "REPOSYNC_TEST_SVN_PW"

[github]
repo = "test/test-repo"
token_env = "REPOSYNC_TEST_GH_TOKEN"

[developer]
name = "Test User"
email = "test@example.com"
svn_username = "testuser"
"#,
        log_level = log_level,
        data_dir = data_dir.display(),
    );
    std::fs::write(path, toml).unwrap();
}

/// Spawn-based black-box test: verify that the real process writes to
/// `{data_dir}/personal.log` and the file is non-empty.
#[test]
fn test_spawn_personal_log_file_written() {
    let bin = personal_binary_path();
    if !bin.exists() {
        eprintln!("SKIPPED: reposync-personal binary not found at {:?}", bin);
        return;
    }

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp.path().join("personal.toml");
    write_test_config(&config_path, &data_dir, "info");

    let output = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap(), "log-probe"])
        .env_remove("RUST_LOG") // ensure config log_level is used
        .output()
        .expect("failed to spawn reposync-personal");

    // The process should exit successfully (log-probe always succeeds).
    assert!(
        output.status.success(),
        "log-probe should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log_file = data_dir.join("personal.log");
    assert!(
        log_file.exists(),
        "personal.log should exist at {:?}",
        log_file
    );
    let contents = std::fs::read_to_string(&log_file).unwrap();
    assert!(!contents.is_empty(), "personal.log should not be empty");
    assert!(
        contents.contains("LOG_PROBE"),
        "personal.log should contain LOG_PROBE marker, got: {}",
        contents
    );
}

/// Spawn-based black-box test: config-only behavior (`RUST_LOG` unset).
/// With `personal.log_level = "warn"`, debug/info should NOT appear;
/// warn/error should appear.
#[test]
fn test_spawn_config_only_log_level_filtering() {
    let bin = personal_binary_path();
    if !bin.exists() {
        eprintln!("SKIPPED: binary not found");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp.path().join("personal.toml");
    write_test_config(&config_path, &data_dir, "warn");

    let output = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap(), "log-probe"])
        .env_remove("RUST_LOG")
        .output()
        .expect("failed to spawn");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contents = std::fs::read_to_string(data_dir.join("personal.log")).unwrap();

    // warn and error should be present.
    assert!(
        contents.contains("LOG_PROBE error-level marker"),
        "error should appear, got: {}",
        contents
    );
    assert!(
        contents.contains("LOG_PROBE warn-level marker"),
        "warn should appear, got: {}",
        contents
    );
    // debug and info should be filtered out.
    assert!(
        !contents.contains("LOG_PROBE info-level marker"),
        "info should be filtered at log_level=warn, got: {}",
        contents
    );
    assert!(
        !contents.contains("LOG_PROBE debug-level marker"),
        "debug should be filtered at log_level=warn, got: {}",
        contents
    );
}

/// Spawn-based black-box test: `RUST_LOG=debug` overrides config
/// `log_level = "error"`. Debug messages should appear.
#[test]
fn test_spawn_rust_log_override_precedence() {
    let bin = personal_binary_path();
    if !bin.exists() {
        eprintln!("SKIPPED: binary not found");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = tmp.path().join("personal.toml");
    write_test_config(&config_path, &data_dir, "error");

    let output = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap(), "log-probe"])
        .env("RUST_LOG", "debug")
        .output()
        .expect("failed to spawn");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contents = std::fs::read_to_string(data_dir.join("personal.log")).unwrap();

    // RUST_LOG=debug should override config error-only level.
    assert!(
        contents.contains("LOG_PROBE debug-level marker"),
        "debug should appear with RUST_LOG=debug override, got: {}",
        contents
    );
    assert!(
        contents.contains("LOG_PROBE info-level marker"),
        "info should appear with RUST_LOG=debug override, got: {}",
        contents
    );
}

/// Runtime test: `RUST_LOG` overrides `personal.log_level`.
///
/// Even when the config says `log_level = "error"`, setting `RUST_LOG=debug`
/// should allow debug messages through.
#[test]
fn test_runtime_rust_log_override_precedence() {
    use tracing_subscriber::layer::SubscriberExt;

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("rust_log_override_test");
    std::fs::create_dir_all(&data_dir).unwrap();

    let log_file = data_dir.join("personal.log");

    let file_appender = tracing_appender::rolling::never(&data_dir, "personal.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Simulate the same logic as init_tracing:
    // RUST_LOG takes precedence; otherwise use config log_level.
    // Config says error-only, but RUST_LOG says debug.
    // In init_tracing: EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level))
    // Here we simulate RUST_LOG being set by directly using the override value.
    let filter = tracing_subscriber::EnvFilter::new("debug");

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_target(true)
        .with_ansi(false);

    let subscriber = tracing_subscriber::registry().with(filter).with(file_layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!("rust_log_override_test: debug SHOULD APPEAR due to RUST_LOG=debug");
        tracing::info!("rust_log_override_test: info SHOULD APPEAR due to RUST_LOG=debug");
    });

    drop(guard);
    std::thread::sleep(std::time::Duration::from_millis(100));

    let contents = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        contents.contains("debug SHOULD APPEAR"),
        "RUST_LOG=debug should override config log_level=error, got: {}",
        contents
    );
    assert!(
        contents.contains("info SHOULD APPEAR"),
        "RUST_LOG=debug should allow info messages, got: {}",
        contents
    );
}

// ===========================================================================
// File policy tests (#43): max_file_size and ignore_patterns enforcement
// ===========================================================================

/// Files under the size limit should sync normally (SVN→Git).
#[tokio::test]
async fn test_file_policy_under_limit_syncs() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }

    let dir = TempDir::new().unwrap();
    let svn_url = create_svn_repo(dir.path());
    let svn_wc = dir.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);

    // Commit a small file (50 bytes, well under any limit).
    svn_commit_file(
        &svn_wc,
        "small.txt",
        "hello world - small file",
        "add small file",
    );

    let git_work = dir.path().join("git_work");
    let git_bare = dir.path().join("git_bare");
    let git_client = setup_git_with_bare_origin(&git_work, &git_bare);
    let git_client = Arc::new(Mutex::new(git_client));

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Arc::new(setup_db(&data_dir.join("test.db")));

    let mut config = make_test_config(&svn_url, &data_dir);
    // Set a max_file_size of 1000 bytes — small.txt is well under.
    config.options.max_file_size = 1000;

    let svn_client = SvnClient::new(&svn_url, "", "");
    let sync = SvnToGitSync::new(svn_client, git_client, db, config);

    let count = sync.sync().await.unwrap();
    assert_eq!(count, 1, "one revision should sync");

    // Verify the file made it into the Git working tree.
    assert!(
        git_work.join("small.txt").exists(),
        "small.txt should be in Git tree"
    );
}

/// Files exceeding max_file_size should be skipped (SVN→Git).
#[tokio::test]
async fn test_file_policy_oversize_skipped() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }

    let dir = TempDir::new().unwrap();
    let svn_url = create_svn_repo(dir.path());
    let svn_wc = dir.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);

    // Commit an oversized file (200 bytes, limit will be 100).
    let big_content: String = "x".repeat(200);
    svn_commit_file(&svn_wc, "big.bin", &big_content, "add big file");

    // Also add a small file in the same revision range to verify it still syncs.
    svn_commit_file(&svn_wc, "ok.txt", "fine", "add ok file");

    let git_work = dir.path().join("git_work");
    let git_bare = dir.path().join("git_bare");
    let git_client = setup_git_with_bare_origin(&git_work, &git_bare);
    let git_client = Arc::new(Mutex::new(git_client));

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Arc::new(setup_db(&data_dir.join("test.db")));

    let mut config = make_test_config(&svn_url, &data_dir);
    config.options.max_file_size = 100; // 100-byte limit.

    let svn_client = SvnClient::new(&svn_url, "", "");
    let sync = SvnToGitSync::new(svn_client, git_client, db.clone(), config);

    let count = sync.sync().await.unwrap();
    assert_eq!(count, 2, "both revisions should attempt sync");

    // big.bin should NOT be in the Git working tree.
    assert!(
        !git_work.join("big.bin").exists(),
        "big.bin should be skipped by policy"
    );
    // ok.txt should be there.
    assert!(
        git_work.join("ok.txt").exists(),
        "ok.txt should sync normally"
    );

    // Verify audit log recorded the skip.
    let audit_entries = db
        .list_audit_entries(10, None, Some("file_policy_skip"))
        .unwrap();
    assert!(
        !audit_entries.is_empty(),
        "audit log should contain file_policy_skip entry"
    );
}

/// Files matching ignore_patterns should be skipped (SVN→Git).
#[tokio::test]
async fn test_file_policy_ignore_pattern_skipped() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }

    let dir = TempDir::new().unwrap();
    let svn_url = create_svn_repo(dir.path());
    let svn_wc = dir.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);

    // Commit a .log file (should be ignored) and a .rs file (should sync).
    svn_commit_file(&svn_wc, "app.log", "log data", "add log");
    svn_commit_file(&svn_wc, "main.rs", "fn main() {}", "add rust");

    let git_work = dir.path().join("git_work");
    let git_bare = dir.path().join("git_bare");
    let git_client = setup_git_with_bare_origin(&git_work, &git_bare);
    let git_client = Arc::new(Mutex::new(git_client));

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Arc::new(setup_db(&data_dir.join("test.db")));

    let mut config = make_test_config(&svn_url, &data_dir);
    config.options.ignore_patterns = vec!["*.log".into()];

    let svn_client = SvnClient::new(&svn_url, "", "");
    let sync = SvnToGitSync::new(svn_client, git_client, db, config);

    let count = sync.sync().await.unwrap();
    assert_eq!(count, 2, "both revisions should process");

    // app.log should NOT be in Git.
    assert!(
        !git_work.join("app.log").exists(),
        "app.log should be skipped by ignore pattern"
    );
    // main.rs should be there.
    assert!(
        git_work.join("main.rs").exists(),
        "main.rs should sync normally"
    );
}

/// Directory-level ignore patterns (e.g. build/**) should work.
#[tokio::test]
async fn test_file_policy_ignore_directory_pattern() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }

    let dir = TempDir::new().unwrap();
    let svn_url = create_svn_repo(dir.path());
    let svn_wc = dir.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);

    // Commit files in build/ and src/.
    svn_commit_file(&svn_wc, "src/main.rs", "fn main() {}", "add src");
    svn_commit_file(&svn_wc, "build/output.o", "binary data", "add build");

    let git_work = dir.path().join("git_work");
    let git_bare = dir.path().join("git_bare");
    let git_client = setup_git_with_bare_origin(&git_work, &git_bare);
    let git_client = Arc::new(Mutex::new(git_client));

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Arc::new(setup_db(&data_dir.join("test.db")));

    let mut config = make_test_config(&svn_url, &data_dir);
    config.options.ignore_patterns = vec!["build/**".into()];

    let svn_client = SvnClient::new(&svn_url, "", "");
    let sync = SvnToGitSync::new(svn_client, git_client, db, config);

    let count = sync.sync().await.unwrap();
    assert_eq!(count, 2);

    assert!(
        git_work.join("src/main.rs").exists(),
        "src/main.rs should sync"
    );
    assert!(
        !git_work.join("build/output.o").exists(),
        "build/output.o should be skipped"
    );
}

// ===========================================================================
// LFS integration tests (#44)
// ===========================================================================

/// When lfs_threshold is set, files above the threshold should trigger
/// .gitattributes updates during SVN→Git sync.
#[tokio::test]
async fn test_lfs_threshold_creates_gitattributes() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }

    let dir = TempDir::new().unwrap();
    let svn_url = create_svn_repo(dir.path());
    let svn_wc = dir.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);

    // Commit a large file (200 bytes) and a small file (10 bytes).
    let large_content = "x".repeat(200);
    svn_commit_file(&svn_wc, "model.bin", &large_content, "add large binary");
    svn_commit_file(&svn_wc, "readme.txt", "hello", "add readme");

    let git_work = dir.path().join("git_work");
    let git_bare = dir.path().join("git_bare");
    let git_client = setup_git_with_bare_origin(&git_work, &git_bare);
    let git_client = Arc::new(Mutex::new(git_client));

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Arc::new(setup_db(&data_dir.join("test.db")));

    let mut config = make_test_config(&svn_url, &data_dir);
    // Set LFS threshold at 100 bytes — model.bin (200 bytes) exceeds it.
    config.options.lfs_threshold = 100;

    let svn_client = SvnClient::new(&svn_url, "", "");
    let sync = SvnToGitSync::new(svn_client, git_client, db.clone(), config);

    let count = sync.sync().await.unwrap();
    assert_eq!(count, 2, "two revisions should sync");
    assert!(
        db.active_personal_git_push_operation().unwrap().is_none(),
        "LFS threshold sync must confirm the journal, not leave a hold"
    );
    assert_eq!(
        db.get_watermark("svn_rev").unwrap().as_deref(),
        Some("2"),
        "LFS threshold sync must advance the watermark through journal confirm"
    );

    // Verify model.bin was copied.
    assert!(
        git_work.join("model.bin").exists(),
        "model.bin should be in Git tree"
    );

    // Verify .gitattributes was created with LFS tracking for *.bin.
    let gitattr_path = git_work.join(".gitattributes");
    assert!(
        gitattr_path.exists(),
        ".gitattributes should be created for LFS-tracked files"
    );
    let gitattr_content = std::fs::read_to_string(&gitattr_path).unwrap();
    assert!(
        gitattr_content.contains("*.bin filter=lfs diff=lfs merge=lfs -text"),
        ".gitattributes should contain LFS tracking for *.bin, got: {}",
        gitattr_content
    );

    // Verify readme.txt was copied normally (under threshold).
    assert!(
        git_work.join("readme.txt").exists(),
        "readme.txt should be in Git tree"
    );
}

/// LFS pointer detection: is_lfs_pointer and parse_lfs_pointer.
#[test]
fn test_lfs_pointer_detection_in_git_to_svn() {
    // Verify the LFS pointer detection works correctly for content
    // that would be encountered during Git→SVN sync.
    let pointer =
        "version https://git-lfs.github.com/spec/v1\noid sha256:abc123def456789\nsize 1048576\n";
    assert!(reposync_core::lfs::is_lfs_pointer(pointer.as_bytes()));

    let parsed = reposync_core::lfs::parse_lfs_pointer(pointer.as_bytes()).unwrap();
    assert_eq!(parsed.oid, "abc123def456789");
    assert_eq!(parsed.size, 1048576);

    // Normal file content should NOT be detected as LFS pointer.
    let normal = b"fn main() { println!(\"hello\"); }";
    assert!(!reposync_core::lfs::is_lfs_pointer(normal));

    // Binary content should NOT be detected as LFS pointer.
    let binary = vec![0xFF, 0xFE, 0x00, 0x01, 0x02, 0x03];
    assert!(!reposync_core::lfs::is_lfs_pointer(&binary));
}

/// LFS config wiring: lfs_threshold in PersonalOptionsConfig correctly
/// creates a FilePolicy with LFS enabled.
#[test]
fn test_lfs_config_wiring() {
    use reposync_core::file_policy::FilePolicy;

    // Default config: no LFS.
    let opts = PersonalOptionsConfig::default();
    let policy = FilePolicy::from(&opts);
    assert!(!policy.lfs_enabled());

    // Config with lfs_threshold: LFS enabled.
    let opts = PersonalOptionsConfig {
        lfs_threshold: 5_000_000,
        ..Default::default()
    };
    let policy = FilePolicy::from(&opts);
    assert!(policy.lfs_enabled());
    assert_eq!(policy.lfs_threshold(), 5_000_000);

    // A file under the threshold → Allow.
    let decision = policy.evaluate("small.txt", 100);
    assert_eq!(
        decision,
        reposync_core::file_policy::FilePolicyDecision::Allow
    );

    // A file over the threshold → LfsTrack.
    let decision = policy.evaluate("large.bin", 10_000_000);
    assert!(matches!(
        decision,
        reposync_core::file_policy::FilePolicyDecision::LfsTrack { .. }
    ));
    assert!(decision.should_sync());
}

// ===========================================================================
// Test: LFS pointer text must never be written as regular content
// ===========================================================================

/// Validates that LFS pointer content is detectable and that attempting to
/// resolve a pointer outside of a real LFS repo fails (which triggers the
/// skip-and-audit path in git_to_svn instead of writing pointer text).
#[test]
fn test_lfs_pointer_text_never_reaches_svn() {
    // A valid LFS pointer — this is what Git stores for LFS-tracked files.
    let pointer_text = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";

    // Verify we can detect it as an LFS pointer.
    assert!(
        reposync_core::lfs::is_lfs_pointer(pointer_text),
        "valid LFS pointer must be detected"
    );

    // Verify parsing extracts the correct OID and size.
    let parsed =
        reposync_core::lfs::parse_lfs_pointer(pointer_text).expect("valid pointer should parse");
    assert_eq!(
        parsed.oid,
        "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"
    );
    assert_eq!(parsed.size, 12345);

    // Attempting to resolve the pointer in a non-LFS directory should fail.
    // This is exactly what triggers the skip-and-audit path in git_to_svn
    // (the fix ensures this failure skips the file instead of writing pointer
    // text as-is).
    let tmp = TempDir::new().unwrap();
    let result = reposync_core::lfs::resolve_lfs_pointer(tmp.path(), pointer_text);
    assert!(
        result.is_err(),
        "LFS pointer resolution must fail without a real LFS repo — \
         if this passed, the safety net in git_to_svn would not trigger"
    );

    // Regular file content must NOT be detected as an LFS pointer.
    let normal_content = b"fn main() { println!(\"hello world\"); }";
    assert!(
        !reposync_core::lfs::is_lfs_pointer(normal_content),
        "regular source code must not be detected as LFS pointer"
    );
}

/// Validates that a roundtrip create→detect→parse works correctly and that
/// the pointer detection is precise enough to avoid false positives.
#[test]
fn test_lfs_pointer_detection_precision() {
    // Create an LFS pointer from known content.
    let original = b"some binary data \x00\x01\x02\x03";
    let pointer = reposync_core::lfs::create_lfs_pointer(original);

    // The pointer itself must be detected.
    assert!(reposync_core::lfs::is_lfs_pointer(pointer.as_bytes()));

    // The pointer must parse back to the correct size.
    let parsed = reposync_core::lfs::parse_lfs_pointer(pointer.as_bytes()).unwrap();
    assert_eq!(parsed.size, original.len() as u64);

    // Content that merely *mentions* LFS but isn't a pointer must NOT match.
    let fake = b"This file talks about https://git-lfs.github.com/spec/v1 but is not a pointer";
    assert!(
        !reposync_core::lfs::is_lfs_pointer(fake),
        "text mentioning LFS URL but not starting with the magic prefix must not be detected"
    );
}

// ===========================================================================
// Test: Replay-path safety — production path via apply_git_changes_to_svn
// ===========================================================================

/// Production-path test: drives the ACTUAL `GitToSvnSync::apply_git_changes_to_svn`
/// method with a Git commit containing an unresolved LFS pointer.
///
/// Asserts from real execution:
///   - Pointer text is NOT written to the SVN working copy.
///   - `lfs_resolution_failed` audit row is emitted by the production path.
///   - The method returns Err (fail-closed) so the SHA is not checkpointed.
#[tokio::test]
async fn test_replay_path_lfs_pointer_skipped_not_committed() {
    use reposync_core::db::queries::AuditLogEntry;
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};
    use reposync_personal::git_to_svn::GitToSvnSync;

    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();

    // --- Set up local SVN repo with a working copy ---
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    // --- Set up Git repo with a committed LFS pointer ---
    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare_dir);

    let pointer_text = "version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 99999\n";
    std::fs::write(git_work.join("large-asset.bin"), pointer_text).unwrap();
    let oid = git_client
        .commit(
            "add LFS-tracked asset",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("git commit failed");
    let commit_sha = oid.to_string();

    // --- Set up database ---
    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let db_arc = Arc::new(db);

    // --- Build PersonalConfig for GitToSvnSync ---
    let config = PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 30,
            data_dir: tmp.path().to_path_buf(),
            log_level: "info".into(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.clone(),
            username: "test".into(),
            password_env: String::new(),
            password: Some("test".into()),
        },
        github: PersonalGitHubConfig {
            api_url: "https://localhost:0/unused".into(),
            git_base_url: None,
            repo: "test/unused".into(),
            token_env: String::new(),
            default_branch: "main".into(),
            auto_create: false,
            private: false,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "test".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    };

    let svn_client = SvnClient::new(&svn_url, "test", "test");
    let github_client = reposync_core::git::github::GitHubClient::new(
        "https://localhost:0/unused",
        "unused",
        reposync_core::config::GitProvider::default(),
    );

    let sync = GitToSvnSync::new(
        svn_client,
        github_client,
        db_arc.clone(),
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );

    // --- Build a GitHubCommit matching the real commit SHA ---
    let gh_commit = GitHubCommit {
        sha: commit_sha.clone(),
        commit: GitHubCommitDetail {
            message: "add LFS-tracked asset".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };

    // --- Call the PRODUCTION apply_git_changes_to_svn ---
    let result = sync.apply_git_changes_to_svn(&gh_commit).await;

    assert!(
        result.is_err(),
        "apply_git_changes_to_svn must fail closed on unresolved LFS pointer, got: {:?}",
        result
    );

    // --- Verify: SVN working copy must NOT contain the pointer text ---
    let svn_target = svn_wc.join("large-asset.bin");
    assert!(
        !svn_target.exists(),
        "LFS pointer file must NOT be written to SVN working copy"
    );

    // --- Verify: audit log has the lfs_resolution_failed entry (from production code) ---
    let audit_entries: Vec<AuditLogEntry> = db_arc
        .list_audit_log_by_action("lfs_resolution_failed", 10)
        .expect("failed to query audit log");
    assert!(
        !audit_entries.is_empty(),
        "production code must emit an lfs_resolution_failed audit entry"
    );
    let entry = &audit_entries[0];
    assert_eq!(entry.action, "lfs_resolution_failed");
    assert_eq!(entry.direction.as_deref(), Some("git_to_svn"));
    assert_eq!(entry.git_sha.as_deref(), Some(commit_sha.as_str()));
    assert!(
        !entry.success,
        "lfs_resolution_failed must be marked as failure"
    );
    assert!(
        entry
            .details
            .as_deref()
            .unwrap_or("")
            .contains("large-asset.bin"),
        "audit details must mention the held file path"
    );
}

/// Abort restore must remove a newly-created parent directory when a later file in
/// the same commit fails (e.g. unresolved LFS pointer).
#[tokio::test]
async fn test_replay_path_abort_restore_removes_newdir_after_lfs_failure() {
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};
    use reposync_personal::git_to_svn::GitToSvnSync;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare_dir);

    std::fs::create_dir_all(git_work.join("src/newdir")).unwrap();
    std::fs::write(git_work.join("src/newdir/file.txt"), "new file\n").unwrap();
    let pointer_text = "version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 99999\n";
    std::fs::write(git_work.join("zzz-large.bin"), pointer_text).unwrap();
    let oid = git_client
        .commit(
            "add newdir file then unresolved LFS pointer",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("git commit failed");
    let commit_sha = oid.to_string();

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));

    let config = PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 30,
            data_dir: tmp.path().to_path_buf(),
            log_level: "info".into(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.clone(),
            username: "test".into(),
            password_env: String::new(),
            password: Some("test".into()),
        },
        github: PersonalGitHubConfig {
            api_url: "https://localhost:0/unused".into(),
            git_base_url: None,
            repo: "test/unused".into(),
            token_env: String::new(),
            default_branch: "main".into(),
            auto_create: false,
            private: false,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "test".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    };

    let svn_client = SvnClient::new(&svn_url, "test", "test");
    let github_client = reposync_core::git::github::GitHubClient::new(
        "https://localhost:0/unused",
        "unused",
        reposync_core::config::GitProvider::default(),
    );

    let sync = GitToSvnSync::new(
        svn_client,
        github_client,
        db_arc,
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );

    let gh_commit = GitHubCommit {
        sha: commit_sha.clone(),
        commit: GitHubCommitDetail {
            message: "add newdir file then unresolved LFS pointer".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };

    let result = sync.apply_git_changes_to_svn(&gh_commit).await;
    assert!(
        result.is_err(),
        "apply_git_changes_to_svn must fail closed on unresolved LFS pointer, got: {:?}",
        result
    );
    assert!(
        !svn_wc.join("src/newdir").exists(),
        "abort restore must remove the newly-created parent directory after LFS failure"
    );
}

/// Abort restore must not delete pre-existing unversioned siblings under a shared parent.
#[tokio::test]
async fn test_replay_path_abort_restore_preserves_preexisting_unversioned_sibling() {
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    std::fs::create_dir_all(svn_wc.join("scratch")).unwrap();
    std::fs::write(svn_wc.join("scratch/other.txt"), "keep me\n").unwrap();

    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare_dir);

    std::fs::create_dir_all(git_work.join("scratch")).unwrap();
    std::fs::write(git_work.join("scratch/note.txt"), "new note\n").unwrap();
    let pointer_text = "version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 99999\n";
    std::fs::write(git_work.join("zzz-large.bin"), pointer_text).unwrap();
    let oid = git_client
        .commit(
            "scratch note then unresolved LFS pointer",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("git commit failed");
    let commit_sha = oid.to_string();

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    let config = personal_config_for_abort_restore_tests(&tmp, &svn_url);
    let sync = git_to_svn_sync_for_abort_restore_tests(&config, &db_arc, &svn_wc, &git_work);

    let gh_commit = GitHubCommit {
        sha: commit_sha.clone(),
        commit: GitHubCommitDetail {
            message: "scratch note then unresolved LFS pointer".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };

    let result = sync.apply_git_changes_to_svn(&gh_commit).await;
    assert!(result.is_err(), "apply must fail on unresolved LFS pointer");
    assert!(
        svn_wc.join("scratch/other.txt").is_file(),
        "pre-existing unversioned sibling must survive abort restore"
    );
    assert!(
        !svn_wc.join("scratch/note.txt").exists(),
        "apply-created file must be removed on abort"
    );
    let status = Command::new("svn")
        .args(["status", svn_wc.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "svn status must succeed after abort restore"
    );
    let status_text = String::from_utf8_lossy(&status.stdout);
    assert!(
        personal_apply_abort_paths_clean_for_test(&status_text, &["scratch/note.txt"]),
        "cleanliness check must ignore pre-existing unversioned ancestor; status: {}",
        status_text.trim()
    );
}

/// After SVN commit succeeds, a failed local checkpoint must not run abort restore.
#[tokio::test]
async fn test_replay_commit_confirm_fail_preserves_committed_tree() {
    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::create_dir_all(git_work.join("src/newdir")).unwrap();
    std::fs::write(git_work.join("src/newdir/file.txt"), "from git\n").unwrap();
    let oid = git_client
        .commit(
            "Add src/newdir/file.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha_commit = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work,
        tmp.path(),
    );

    let _fault = SvnCommitFaultGuard::confirm_fail(&svn_wc);
    let err = syncer
        .replay_commit(
            &github_commit(git_sha_commit.clone(), "Add src/newdir/file.txt"),
            9,
            "feature/confirm-fail",
        )
        .await
        .expect_err("confirm failure must surface the checkpoint error");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("held for reconcile"),
        "expected checkpoint hold error, got: {err_text}"
    );
    assert!(
        !err_text.contains("failed to restore SVN working copy"),
        "abort restore must not run after SVN commit: {err_text}"
    );

    assert!(
        svn_wc.join("src/newdir/file.txt").is_file(),
        "committed file must remain in the working copy"
    );
    let status = Command::new("svn")
        .args(["status", svn_wc.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "svn status must be clean after commit+confirm-fail, got: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    let syncer2 = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        tmp.path().join("git_work"),
        tmp.path(),
    );
    drop(_fault);
    let svn_after_hold = svn_youngest(&svn_url);
    let recovered = syncer2
        .replay_commit(
            &github_commit(git_sha_commit.clone(), "Add src/newdir/file.txt"),
            9,
            "feature/confirm-fail",
        )
        .await
        .expect("held confirm-fail checkpoint must finalize on replay");
    assert_eq!(
        recovered, svn_after_hold,
        "finalize must not create a second SVN revision"
    );
    assert!(
        db_arc
            .active_personal_svn_commit_operation()
            .unwrap()
            .is_none(),
        "journal must clear after finalize"
    );
    let mapped = db_arc
        .list_commit_map(10)
        .unwrap()
        .into_iter()
        .find(|entry| entry.direction == "git_to_svn" && entry.git_sha == git_sha_commit);
    assert!(
        mapped.is_some(),
        "scoped commit_map must record the held git-to-svn mapping"
    );
    assert_eq!(
        svn_youngest(&svn_url),
        svn_after_hold,
        "SVN youngest must remain unchanged after finalize"
    );
    assert!(
        svn_wc.join("src/newdir/file.txt").is_file(),
        "committed file must remain in SVN working copy after finalize"
    );
    assert_eq!(
        std::fs::read_to_string(svn_wc.join("src/newdir/file.txt")).unwrap(),
        "from git\n"
    );
}

/// Apply must refuse writes and deletes through a versioned symlink parent.
#[tokio::test]
async fn test_apply_refuses_versioned_symlink_parent() {
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(outside.join("link")).unwrap();
    std::fs::write(outside.join("link/keep.txt"), "original\n").unwrap();

    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    std::os::unix::fs::symlink(outside.join("link"), svn_wc.join("link")).unwrap();
    let add = Command::new("svn")
        .args(["add", "link"])
        .current_dir(&svn_wc)
        .output()
        .unwrap();
    assert!(add.status.success(), "svn add: {}", add.status);
    let propset = Command::new("svn")
        .args(["propset", "svn:special", "*", "link"])
        .current_dir(&svn_wc)
        .output()
        .unwrap();
    assert!(
        propset.status.success(),
        "svn propset: {} stderr={}",
        propset.status,
        String::from_utf8_lossy(&propset.stderr)
    );
    let commit = Command::new("svn")
        .args(["commit", "-m", "add symlink link"])
        .current_dir(&svn_wc)
        .output()
        .unwrap();
    assert!(commit.status.success());

    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare_dir);
    std::fs::create_dir_all(git_work.join("link")).unwrap();
    std::fs::write(git_work.join("link/keep.txt"), "injected\n").unwrap();
    let oid = git_client
        .commit(
            "write through symlink parent",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let write_sha = oid.to_string();

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    let config = personal_config_for_abort_restore_tests(&tmp, &svn_url);
    let sync = git_to_svn_sync_for_abort_restore_tests(&config, &db_arc, &svn_wc, &git_work);

    let write_commit = GitHubCommit {
        sha: write_sha,
        commit: GitHubCommitDetail {
            message: "write through symlink parent".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };
    let write_err = sync.apply_git_changes_to_svn(&write_commit).await;
    assert!(
        write_err.is_err(),
        "write through symlink parent must be refused"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("link/keep.txt")).unwrap(),
        "original\n",
        "outside file must be unchanged after refused write"
    );

    std::fs::write(git_work.join("link/keep.txt"), "original\n").unwrap();
    std::fs::remove_file(git_work.join("link/keep.txt")).unwrap();
    let del_oid = git_client
        .commit(
            "delete through symlink parent",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let delete_commit = GitHubCommit {
        sha: del_oid.to_string(),
        commit: GitHubCommitDetail {
            message: "delete through symlink parent".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };
    let delete_err = sync.apply_git_changes_to_svn(&delete_commit).await;
    assert!(
        delete_err.is_err(),
        "delete through symlink parent must be refused"
    );
    assert!(
        outside.join("link/keep.txt").is_file(),
        "outside file must remain after refused delete"
    );
}

/// Abort restore must not remove versioned empty parent directories.
#[tokio::test]
async fn test_replay_path_abort_restore_preserves_versioned_empty_parent() {
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");
    std::fs::create_dir_all(svn_wc.join("keep/emptydir")).unwrap();
    let status = Command::new("svn")
        .args([
            "add",
            "--depth=empty",
            svn_wc.join("keep").to_str().unwrap(),
            svn_wc.join("keep/emptydir").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "svn add empty keep/emptydir failed");
    let status = Command::new("svn")
        .args([
            "commit",
            "-m",
            "versioned empty keep/emptydir",
            svn_wc.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "svn commit keep/emptydir failed");

    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare_dir);

    std::fs::create_dir_all(git_work.join("keep/emptydir")).unwrap();
    std::fs::write(git_work.join("keep/emptydir/file.txt"), "new file\n").unwrap();
    let pointer_text = "version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 99999\n";
    std::fs::write(git_work.join("zzz-large.bin"), pointer_text).unwrap();
    let oid = git_client
        .commit(
            "file under versioned emptydir then LFS failure",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("git commit failed");
    let commit_sha = oid.to_string();

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    let config = personal_config_for_abort_restore_tests(&tmp, &svn_url);
    let sync = git_to_svn_sync_for_abort_restore_tests(&config, &db_arc, &svn_wc, &git_work);

    let gh_commit = GitHubCommit {
        sha: commit_sha.clone(),
        commit: GitHubCommitDetail {
            message: "file under versioned emptydir then LFS failure".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };

    let result = sync.apply_git_changes_to_svn(&gh_commit).await;
    assert!(result.is_err(), "apply must fail on unresolved LFS pointer");
    assert!(svn_wc.join("keep").is_dir());
    assert!(svn_wc.join("keep/emptydir").is_dir());
    let status_out = Command::new("svn")
        .args(["status", svn_wc.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        status_out.status.success(),
        "svn status failed: {}",
        String::from_utf8_lossy(&status_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&status_out.stdout)
            .trim()
            .is_empty(),
        "svn status must be clean after abort restore, got: {}",
        String::from_utf8_lossy(&status_out.stdout)
    );
}

/// Touched paths with `..` must fail closed and must not delete outside the WC.
#[tokio::test]
async fn test_abort_restore_escape_path_does_not_delete_outside_wc() {
    use reposync_personal::git_to_svn::ApplyAbortOwnership;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    let outside_dir = tmp.path().join("outside-empty");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside_file = outside_dir.join("outside_only.txt");
    std::fs::write(&outside_file, "outside\n").unwrap();

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    let config = personal_config_for_abort_restore_tests(&tmp, &svn_url);
    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    setup_git_with_bare_origin(&git_work, &bare_dir);
    let sync = git_to_svn_sync_for_abort_restore_tests(&config, &db_arc, &svn_wc, &git_work);

    std::fs::write(svn_wc.join("probe_in_wc.txt"), "probe\n").unwrap();
    let ownership = ApplyAbortOwnership::with_recorded_paths(
        vec!["../outside-empty/outside_only.txt".to_string()],
        vec!["probe_in_wc.txt".to_string()],
        vec![],
    );

    let result = sync
        .restore_working_copy_after_apply_abort(&ownership)
        .await;
    assert!(
        result.is_err(),
        "restore must fail closed on escape paths, got {:?}",
        result
    );
    assert!(
        outside_file.is_file(),
        "abort restore must not delete files outside the working copy"
    );
    assert!(
        svn_wc.join("probe_in_wc.txt").is_file(),
        "filesystem cleanup must not run when validation fails"
    );
}

/// Non-E155010 `svn revert` errors must abort restore before filesystem cleanup.
#[tokio::test]
async fn test_abort_restore_non_e155010_revert_error_skips_cleanup() {
    use reposync_personal::git_to_svn::ApplyAbortOwnership;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    let config = personal_config_for_abort_restore_tests(&tmp, &svn_url);
    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    setup_git_with_bare_origin(&git_work, &bare_dir);
    let sync = git_to_svn_sync_for_abort_restore_tests(&config, &db_arc, &svn_wc, &git_work);

    std::fs::write(svn_wc.join("probe_new.txt"), "probe\n").unwrap();
    let ownership = ApplyAbortOwnership::with_recorded_paths(
        vec!["seed.txt".to_string()],
        vec!["probe_new.txt".to_string()],
        vec![],
    );

    let result = sync
        .restore_apply_abort_reverting_paths_for_test(&ownership, &["/etc/hostname"])
        .await;
    assert!(
        result.is_err(),
        "restore must fail on E155007 revert, got {:?}",
        result
    );
    assert!(
        svn_wc.join("probe_new.txt").is_file(),
        "filesystem cleanup must not run after non-E155010 revert failure"
    );
}

fn personal_config_for_abort_restore_tests(tmp: &TempDir, svn_url: &str) -> PersonalConfig {
    PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 30,
            data_dir: tmp.path().to_path_buf(),
            log_level: "info".into(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.to_string(),
            username: "test".into(),
            password_env: String::new(),
            password: Some("test".into()),
        },
        github: PersonalGitHubConfig {
            api_url: "https://localhost:0/unused".into(),
            git_base_url: None,
            repo: "test/unused".into(),
            token_env: String::new(),
            default_branch: "main".into(),
            auto_create: false,
            private: false,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "test".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    }
}

fn git_to_svn_sync_for_abort_restore_tests(
    config: &PersonalConfig,
    db_arc: &Arc<Database>,
    svn_wc: &Path,
    git_work: &Path,
) -> GitToSvnSync {
    let svn_client = SvnClient::new(&config.svn.url, "test", "test");
    let github_client = reposync_core::git::github::GitHubClient::new(
        "https://localhost:0/unused",
        "unused",
        reposync_core::config::GitProvider::default(),
    );
    GitToSvnSync::new(
        svn_client,
        github_client,
        Arc::clone(db_arc),
        config,
        svn_wc.to_path_buf(),
        git_work.to_path_buf(),
    )
}

#[test]
fn test_resolve_lfs_pointer_reads_local_object_with_skip_smudge() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = b"local lfs payload with skip smudge";
    let pointer = reposync_core::lfs::create_lfs_pointer(payload);
    let parsed = reposync_core::lfs::parse_lfs_pointer(pointer.as_bytes()).unwrap();
    let object_path = reposync_core::lfs::local_lfs_object_path(tmp.path(), &parsed.oid);
    std::fs::create_dir_all(object_path.parent().unwrap()).unwrap();
    std::fs::write(&object_path, payload).unwrap();

    let resolved = reposync_core::lfs::resolve_lfs_pointer(tmp.path(), pointer.as_bytes()).unwrap();

    assert_eq!(resolved, payload);
}

#[tokio::test]
async fn test_personal_git_to_svn_unresolved_lfs_holds_without_checkpoint() {
    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();

    let pointer_text = "version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 99999\n";
    std::fs::write(git_work.join("large-asset.bin"), pointer_text).unwrap();
    let oid = git_client
        .commit(
            "add LFS-tracked asset",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let err = syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "add LFS-tracked asset"),
            9,
            "feature/lfs",
        )
        .await
        .expect_err("unresolved LFS pointer must fail closed before checkpoint");
    assert!(
        format!("{err:#}").contains("LFS pointer"),
        "expected LFS hold error, got: {err:#}"
    );

    assert_eq!(
        svn_youngest(&svn_url),
        svn_before,
        "SVN must not advance when LFS resolution fails"
    );
    assert_eq!(
        db_arc
            .list_commit_map(10)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.direction == "git_to_svn")
            .count(),
        0,
        "commit_map must not advance on unresolved LFS pointer"
    );
}

/// LFS abort with a sibling file in the same commit must not leak edits into SVN,
/// must stop the batch before a later PR, and must leave the working copy clean.
#[tokio::test]
async fn candidate_rs05_personal_lfs_abort_cleans_sibling_and_stops_batch() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");
    std::fs::write(svn_wc.join("unrelated-unversioned.txt"), "leave me").unwrap();
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();

    let pointer_text = "version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 99999\n";
    std::fs::write(git_work.join("before-lfs.txt"), "sibling edit\n").unwrap();
    std::fs::write(git_work.join("large-asset.bin"), pointer_text).unwrap();
    let pr1_commit = git_client
        .commit(
            "add sibling and unresolved LFS pointer",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    git_client.push("origin", "main").unwrap();
    let pr1_merge_sha = git_sha(&git_work);

    std::fs::write(git_work.join("second-pr.txt"), "second pr\n").unwrap();
    let pr2_commit = git_client
        .commit(
            "second merged PR",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    git_client.push("origin", "main").unwrap();
    let pr2_merge_sha = git_sha(&git_work);
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, svn_before);

    let (api_url, stub) = spawn_github_two_pr_sync_stub(
        &pr1_merge_sha,
        &pr1_commit,
        "2025-01-02T00:00:00Z",
        &pr2_merge_sha,
        &pr2_commit,
        "2025-01-03T00:00:00Z",
    );
    let mut config = make_test_config(&svn_url, tmp.path());
    config.github.api_url = api_url;
    config.github.token = Some("test-token".into());

    let sync = GitToSvnSync::new(
        SvnClient::new(&svn_url, "", ""),
        GitHubClient::new(&config.github.api_url, "test-token", GitProvider::GitHub),
        db_arc.clone(),
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );

    let result = sync.sync().await.expect("sync must return a summary");
    assert_eq!(
        result.prs_synced, 0,
        "no PR must complete during LFS abort pass"
    );
    assert_eq!(result.prs_failed, 1, "first PR must fail closed");
    assert_eq!(
        svn_youngest(&svn_url),
        svn_before,
        "SVN must not advance when LFS abort stops the batch"
    );
    assert!(
        !svn_path_exists_at_head(&svn_url, "before-lfs.txt"),
        "sibling edits must not reach SVN"
    );
    assert!(
        !svn_path_exists_at_head(&svn_url, "second-pr.txt"),
        "later PR must not be committed in the same pass"
    );
    assert!(
        !db_arc.is_personal_pr_synced(&pr1_merge_sha).unwrap(),
        "first PR must remain unsynced for retry"
    );
    assert!(
        !db_arc.is_personal_pr_synced(&pr2_merge_sha).unwrap(),
        "second PR must not be processed after batch stop"
    );
    assert_eq!(
        db_arc
            .list_commit_map(10)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.direction == "git_to_svn")
            .count(),
        0,
        "commit_map must not advance on LFS abort"
    );
    let wc_status = Command::new("svn")
        .args(["status", svn_wc.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        wc_status.status.success(),
        "svn status must succeed after abort restore"
    );
    let status_text = String::from_utf8_lossy(&wc_status.stdout);
    let status_lines: Vec<&str> = status_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(
        status_lines.len(),
        1,
        "unexpected svn status after abort restore"
    );
    assert!(
        status_lines[0].starts_with('?') && status_lines[0].ends_with("unrelated-unversioned.txt"),
        "abort restore must clean only apply-touched paths, leaving unrelated unversioned files: {:?}",
        status_lines
    );
    assert!(
        svn_wc.join("unrelated-unversioned.txt").is_file(),
        "narrow abort restore must not delete unrelated unversioned files"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "RS05_PERSONAL_LFS_ABORT_STOPS_BATCH",
            "svn_revision_before_after": svn_before,
            "pr1_merge_sha": pr1_merge_sha,
            "pr2_merge_sha": pr2_merge_sha,
            "working_copy_clean": true,
            "mode": "personal"
        })
    );
    drop(stub);
}

/// After the LFS object becomes available locally, the next sync pass must replay PRs in order.
#[tokio::test]
async fn candidate_rs05_personal_lfs_abort_retries_in_order() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();

    let lfs_payload = b"resolved lfs payload for retry";
    let pointer = reposync_core::lfs::create_lfs_pointer(lfs_payload);
    let parsed = reposync_core::lfs::parse_lfs_pointer(pointer.as_bytes()).unwrap();
    std::fs::write(git_work.join("before-lfs.txt"), "sibling edit\n").unwrap();
    std::fs::write(git_work.join("large-asset.bin"), &pointer).unwrap();
    let pr1_commit = git_client
        .commit(
            "add sibling and LFS pointer",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    git_client.push("origin", "main").unwrap();
    let pr1_merge_sha = git_sha(&git_work);

    std::fs::write(git_work.join("second-pr.txt"), "second pr\n").unwrap();
    let pr2_commit = git_client
        .commit(
            "second merged PR",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    git_client.push("origin", "main").unwrap();
    let pr2_merge_sha = git_sha(&git_work);
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, svn_before);

    let (api_url, stub) = spawn_github_two_pr_sync_stub(
        &pr1_merge_sha,
        &pr1_commit,
        "2025-01-02T00:00:00Z",
        &pr2_merge_sha,
        &pr2_commit,
        "2025-01-03T00:00:00Z",
    );
    let mut config = make_test_config(&svn_url, tmp.path());
    config.github.api_url = api_url.clone();
    config.github.token = Some("test-token".into());

    let sync = GitToSvnSync::new(
        SvnClient::new(&svn_url, "", ""),
        GitHubClient::new(&config.github.api_url, "test-token", GitProvider::GitHub),
        db_arc.clone(),
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );

    let blocked = sync.sync().await.expect("first pass must return summary");
    assert_eq!(blocked.prs_synced, 0);
    assert_eq!(blocked.prs_failed, 1);
    assert_eq!(svn_youngest(&svn_url), svn_before);

    let object_path = reposync_core::lfs::local_lfs_object_path(&git_work, &parsed.oid);
    std::fs::create_dir_all(object_path.parent().unwrap()).unwrap();
    std::fs::write(&object_path, lfs_payload).unwrap();

    let retry = GitToSvnSync::new(
        SvnClient::new(&svn_url, "", ""),
        GitHubClient::new(&api_url, "test-token", GitProvider::GitHub),
        db_arc.clone(),
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );
    let recovered = retry.sync().await.expect("retry pass must succeed");
    assert_eq!(recovered.prs_synced, 2, "both PRs must replay in order");
    assert!(
        svn_youngest(&svn_url) > svn_before,
        "SVN must advance after successful retry"
    );
    assert!(db_arc.is_personal_pr_synced(&pr1_merge_sha).unwrap());
    assert!(db_arc.is_personal_pr_synced(&pr2_merge_sha).unwrap());
    assert!(svn_path_exists_at_head(&svn_url, "before-lfs.txt"));
    assert!(svn_path_exists_at_head(&svn_url, "large-asset.bin"));
    assert!(svn_path_exists_at_head(&svn_url, "second-pr.txt"));

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "RS05_PERSONAL_LFS_ABORT_RETRY_IN_ORDER",
            "svn_revision_before": svn_before,
            "svn_revision_after": svn_youngest(&svn_url),
            "pr1_merge_sha": pr1_merge_sha,
            "pr2_merge_sha": pr2_merge_sha,
            "mode": "personal"
        })
    );
    drop(stub);
}

#[test]
fn test_resolve_lfs_pointer_accepts_pointer_shaped_payload() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .status()
        .expect("git init");
    let pointer_shaped_payload =
        b"version https://git-lfs.github.com/spec/v1\noid sha256:deadbeef\nsize 12\n";
    let pointer = reposync_core::lfs::store_lfs_object(tmp.path(), pointer_shaped_payload)
        .expect("store pointer-shaped payload as LFS object");
    let resolved = reposync_core::lfs::resolve_lfs_pointer(tmp.path(), &pointer)
        .expect("pointer-shaped payload must resolve when real bytes are available");
    assert_eq!(resolved, pointer_shaped_payload);
}

/// Production-path companion test: drives `apply_git_changes_to_svn` with
/// normal (non-pointer) content and verifies it IS written to SVN WC.
/// Guards against false-positive skipping.
#[tokio::test]
async fn test_replay_path_normal_content_written_to_svn() {
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};
    use reposync_personal::git_to_svn::GitToSvnSync;

    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();

    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed", "initial seed");

    let git_work = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare_dir);

    // Commit a normal file (not an LFS pointer).
    let normal_content = "fn main() { println!(\"hello world\"); }";
    std::fs::write(git_work.join("feature.txt"), normal_content).unwrap();
    let oid = git_client
        .commit(
            "add feature file",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .expect("git commit failed");
    let commit_sha = oid.to_string();

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let db_arc = Arc::new(db);

    let config = PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 30,
            data_dir: tmp.path().to_path_buf(),
            log_level: "info".into(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.clone(),
            username: "test".into(),
            password_env: String::new(),
            password: Some("test".into()),
        },
        github: PersonalGitHubConfig {
            api_url: "https://localhost:0/unused".into(),
            git_base_url: None,
            repo: "test/unused".into(),
            token_env: String::new(),
            default_branch: "main".into(),
            auto_create: false,
            private: false,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "test".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    };

    let svn_client = SvnClient::new(&svn_url, "test", "test");
    let github_client = reposync_core::git::github::GitHubClient::new(
        "https://localhost:0/unused",
        "unused",
        reposync_core::config::GitProvider::default(),
    );

    let sync = GitToSvnSync::new(
        svn_client,
        github_client,
        db_arc,
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );

    let gh_commit = GitHubCommit {
        sha: commit_sha,
        commit: GitHubCommitDetail {
            message: "add feature file".into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    };

    // --- Call the PRODUCTION apply_git_changes_to_svn ---
    let result = sync.apply_git_changes_to_svn(&gh_commit).await;
    assert!(
        result.is_ok(),
        "apply_git_changes_to_svn must succeed for normal content, got: {:?}",
        result.err()
    );

    // --- Verify: file IS written to SVN working copy with correct content ---
    let svn_target = svn_wc.join("feature.txt");
    assert!(
        svn_target.exists(),
        "normal file must be written to SVN working copy"
    );
    let written = std::fs::read_to_string(&svn_target).unwrap();
    assert_eq!(
        written, normal_content,
        "SVN WC file content must match Git content"
    );
}

fn git_sha(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn seed_personal_svn_import_checkpoint(db: &Database, imported_git_sha: &str, svn_rev: i64) {
    db.insert_commit_map(
        svn_rev,
        imported_git_sha,
        "svn_to_git",
        "testuser",
        "Test User",
    )
    .unwrap();
    db.set_watermark("git_sha", imported_git_sha).unwrap();
}

fn git_cmd(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Point inspection and tracking refs at `tip` so a fresh inspect would admit
/// live Git; durable kv blocks must still refuse after restart.
fn retarget_personal_remote_observation(git_work: &Path, tip: &str, branch: &str) {
    git_cmd(
        git_work,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("+{tip}:refs/reposync/inspection/incoming"),
        ],
    );
    git_cmd(
        git_work,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("+{tip}:refs/remotes/origin/{branch}"),
        ],
    );
    assert_eq!(
        git_sha_at(git_work, "refs/reposync/inspection/incoming"),
        tip,
        "inspection ref must match restored tip"
    );
    assert_eq!(
        git_sha_at(git_work, &format!("refs/remotes/origin/{branch}")),
        tip,
        "tracking ref must match restored tip"
    );
}

/// Personal-mode Git→SVN uses the same P/O/R/L inspection before replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_personal_rewrite_contained() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::history_block_key;
    use reposync_personal::engine::PersonalSyncEngine;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = {
        let repo = svn_url.strip_prefix("file://").unwrap();
        String::from_utf8_lossy(
            &Command::new("svnlook")
                .args(["youngest", repo])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .parse::<i64>()
        .unwrap()
    };

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();
    drop(db);

    git_cmd(&git_work, &["reset", "--hard", &imported_base]);
    std::fs::write(git_work.join("feature.txt"), "rewritten version\n").unwrap();
    git_client
        .commit(
            "Rewritten Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push_force("origin", "main").unwrap();
    let rewritten = git_sha(&git_work);
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    drop(git_client);

    let config = make_test_config(&svn_url, tmp.path());
    let engine = PersonalSyncEngine::new(
        config.clone(),
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let err = engine
        .run_cycle()
        .await
        .expect_err("personal rewrite must be refused before replay");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("non_fast_forward"),
        "personal inspection must use the team classification: {err_text}"
    );
    drop(engine);
    let block_raw = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .unwrap();
    let block: serde_json::Value = serde_json::from_str(&block_raw).unwrap();
    assert_eq!(block["state"], "reconciliation_required");
    assert_eq!(block["reason"], "non_fast_forward");
    assert_eq!(block["durable"], true);
    assert_eq!(block["p_handled"], handled);
    assert_eq!(block["r_fresh_remote"], rewritten);
    let restore_ref = format!("{handled}:refs/heads/main");
    git_cmd(&git_work, &["push", "--force", "origin", &restore_ref]);
    assert_eq!(
        git_sha_at(&bare, "refs/heads/main"),
        handled,
        "live remote restored to the handled checkpoint"
    );
    retarget_personal_remote_observation(&git_work, &handled, "main");

    let restarted = PersonalSyncEngine::new(
        config,
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let again = restarted
        .run_cycle()
        .await
        .expect_err("durable personal block must survive restored remote");
    assert!(
        format!("{again:#}").contains("non_fast_forward"),
        "{again:#}"
    );
    let still = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .unwrap();
    let still_block: serde_json::Value = serde_json::from_str(&still).unwrap();
    assert_eq!(still_block["r_fresh_remote"], rewritten);
    let commit_map_len = Database::new(&db_path)
        .unwrap()
        .list_commit_map(10_000)
        .unwrap()
        .len();
    assert_eq!(
        commit_map_len, 1,
        "rewrite must not add git_to_svn mappings"
    );
    let svn_after = {
        let repo = svn_url.strip_prefix("file://").unwrap();
        String::from_utf8_lossy(
            &Command::new("svnlook")
                .args(["youngest", repo])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .parse::<i64>()
        .unwrap()
    };
    assert_eq!(svn_after, svn_before, "personal rewrite must not write SVN");
    assert_eq!(
        git_sha(&git_work),
        handled,
        "personal checkout must not reset"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R09_PERSONAL", "p":handled, "blocked_r":rewritten,
            "restored_remote":handled, "reason":"non_fast_forward",
            "durable":true, "svn_revision_before_after":svn_before,
            "mode":"personal", "inspection_refs_retargeted":true
        })
    );
}

/// Qualified linear P→R history is admitted through personal inspect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r66_personal_linear_history_admitted() {
    use reposync_core::history_inspect::inspect_personal_history;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);

    std::fs::write(git_work.join("feature.txt"), "second version\n").unwrap();
    git_client
        .commit(
            "Second Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let remote_tip = git_sha(&git_work);
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    drop(git_client);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();

    let admission = inspect_personal_history(&db, &git_work, "main", "personal")
        .expect("qualified linear history must be admitted");
    let admission = admission.expect("origin remote and handled checkpoint require inspection");
    assert_eq!(admission.checkpoint, handled);
    assert_eq!(admission.remote_tip, remote_tip);
    assert!(
        db.get_state(&reposync_core::history_inspect::history_block_key(Some(
            "personal"
        )))
        .unwrap()
        .is_none(),
        "admitted history must not record a durable block"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R66_PERSONAL_LINEAR_ADMIT",
            "p":handled,
            "r":remote_tip,
            "mode":"personal"
        })
    );
}

/// Qualified merge-DAG P→R history is admitted through personal inspect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r66_personal_merge_dag_admitted() {
    use reposync_core::history_inspect::inspect_personal_history;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);

    git_cmd(&git_work, &["checkout", "-b", "side"]);
    std::fs::write(git_work.join("side.txt"), "side\n").unwrap();
    git_client
        .commit(
            "Side change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_cmd(&git_work, &["checkout", "main"]);
    std::fs::write(git_work.join("main.txt"), "main\n").unwrap();
    git_client
        .commit(
            "Main change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_cmd(&git_work, &["merge", "--no-ff", "side", "-m", "Merge side"]);
    git_client.push("origin", "main").unwrap();
    let merge_tip = git_sha(&git_work);
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    drop(git_client);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();

    let admission = inspect_personal_history(&db, &git_work, "main", "personal")
        .expect("qualified merge-DAG history must be admitted");
    let admission = admission.expect("origin remote and handled checkpoint require inspection");
    assert_eq!(admission.checkpoint, handled);
    assert_eq!(admission.remote_tip, merge_tip);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R66_PERSONAL_MERGE_DAG_ADMIT",
            "p":handled,
            "r":merge_tip,
            "mode":"personal"
        })
    );
}

/// Legacy unsupported_merge_dag durable blocks survive restart in personal mode
/// even when live Git would admit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r66_personal_merge_dag_contained() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::history_block_key;
    use reposync_personal::engine::PersonalSyncEngine;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();
    drop(db);

    git_cmd(&git_work, &["checkout", "-b", "side"]);
    std::fs::write(git_work.join("side.txt"), "side\n").unwrap();
    git_client
        .commit(
            "Side change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_cmd(&git_work, &["checkout", "main"]);
    std::fs::write(git_work.join("main.txt"), "main\n").unwrap();
    git_client
        .commit(
            "Main change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_cmd(&git_work, &["merge", "--no-ff", "side", "-m", "Merge side"]);
    git_client.push("origin", "main").unwrap();
    let merge_tip = git_sha(&git_work);
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    drop(git_client);

    let block = serde_json::json!({
        "state": "reconciliation_required",
        "reason": "unsupported_merge_dag",
        "detail": reposync_core::pending_frontier::DETAIL_MERGE_DAG,
        "repo_id": "personal",
        "p_handled": handled,
        "r_fresh_remote": merge_tip,
        "durable": true,
    });
    Database::new(&db_path)
        .unwrap()
        .set_state(
            &history_block_key(Some(PERSONAL_SCOPE_KEY)),
            &block.to_string(),
        )
        .unwrap();

    let config = make_test_config(&svn_url, tmp.path());
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    let restore_ref = format!("{handled}:refs/heads/main");
    git_cmd(&git_work, &["push", "--force", "origin", &restore_ref]);
    assert_eq!(git_sha_at(&bare, "refs/heads/main"), handled);
    retarget_personal_remote_observation(&git_work, &handled, "main");

    let restarted = PersonalSyncEngine::new(
        config,
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let again = restarted
        .run_cycle()
        .await
        .expect_err("durable personal merge-DAG block must survive restored remote");
    assert!(
        format!("{again:#}").contains("unsupported_merge_dag"),
        "{again:#}"
    );
    let still = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .unwrap();
    let still_block: serde_json::Value = serde_json::from_str(&still).unwrap();
    assert_eq!(still_block["r_fresh_remote"], merge_tip);
    let commit_map_len = Database::new(&db_path)
        .unwrap()
        .list_commit_map(10_000)
        .unwrap()
        .len();
    assert_eq!(
        commit_map_len, 1,
        "merge DAG must not add git_to_svn mappings"
    );
    assert_eq!(svn_youngest(&svn_url), svn_before);
    assert_eq!(git_sha(&git_work), handled);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R66_PERSONAL_MERGE_DAG",
            "p":handled,
            "blocked_r":merge_tip,
            "restored_remote":handled,
            "reason":"unsupported_merge_dag",
            "durable":true,
            "seeded_legacy_block":true,
            "svn_revision_before_after":svn_before,
            "mode":"personal",
            "inspection_refs_retargeted":true
        })
    );
}

fn build_personal_linear_chain(
    git_work: &Path,
    parent: &str,
    count: usize,
) -> (String, Vec<String>) {
    let tree = {
        let output = Command::new("git")
            .args(["-C", git_work.to_str().unwrap(), "rev-parse", "HEAD^{tree}"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    let mut tip = parent.to_string();
    let mut chain = Vec::new();
    for index in 0..count {
        let output = Command::new("git")
            .arg("-C")
            .arg(git_work)
            .args([
                "commit-tree",
                &tree,
                "-p",
                &tip,
                "-m",
                &format!("pending {index}"),
            ])
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(output.status.success());
        tip = String::from_utf8(output.stdout).unwrap().trim().to_string();
        chain.push(tip.clone());
    }
    (tip, chain)
}

/// Qualified linear histories beyond the replay cap are admitted at inspect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r66_personal_linear_over_1000_admitted() {
    use reposync_core::history_inspect::inspect_personal_history;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");

    let git_work = tmp.path().join("git_work");
    let _bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &_bare);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);
    let (remote_tip, _) = build_personal_linear_chain(&git_work, &handled, 1001);
    git_cmd(&git_work, &["update-ref", "refs/heads/main", &remote_tip]);
    git_client.push("origin", "main").unwrap();
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    drop(git_client);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();

    let admission = inspect_personal_history(&db, &git_work, "main", "personal")
        .expect("qualified linear backlog must be admitted");
    let admission = admission.expect("origin remote and handled checkpoint require inspection");
    assert_eq!(admission.checkpoint, handled);
    assert_eq!(admission.remote_tip, remote_tip);
    assert!(
        db.get_state(&reposync_core::history_inspect::history_block_key(Some(
            "personal"
        )))
        .unwrap()
        .is_none(),
        "admitted backlog must not record a durable block"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R66_PERSONAL_LINEAR_OVER_1000",
            "p":handled,
            "r":remote_tip,
            "pending_total":1001,
            "mode":"personal"
        })
    );
}

/// Legacy unsupported_backlog durable blocks survive personal restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r66_personal_backlog_block_survives_restart() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::history_block_key;
    use reposync_personal::engine::PersonalSyncEngine;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);
    let (blocked_tip, _) = build_personal_linear_chain(&git_work, &handled, 7);
    git_cmd(&git_work, &["update-ref", "refs/heads/main", &blocked_tip]);
    git_client.push("origin", "main").unwrap();
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    drop(git_client);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();
    db.set_state(
        &history_block_key(Some(PERSONAL_SCOPE_KEY)),
        &serde_json::json!({
            "state": "reconciliation_required",
            "reason": "unsupported_backlog",
            "detail": reposync_core::pending_frontier::DETAIL_BACKLOG,
            "repo_id": "personal",
            "p_handled": handled,
            "r_fresh_remote": blocked_tip,
            "durable": true,
        })
        .to_string(),
    )
    .unwrap();
    drop(db);

    let config = make_test_config(&svn_url, tmp.path());
    git_cmd(&git_work, &["reset", "--hard", &handled]);
    let restore_ref = format!("{handled}:refs/heads/main");
    git_cmd(&git_work, &["push", "--force", "origin", &restore_ref]);
    assert_eq!(git_sha_at(&bare, "refs/heads/main"), handled);
    retarget_personal_remote_observation(&git_work, &handled, "main");

    let restarted = PersonalSyncEngine::new(
        config,
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let again = restarted
        .run_cycle()
        .await
        .expect_err("legacy backlog durable block must survive restored remote");
    assert!(
        format!("{again:#}").contains("unsupported_backlog"),
        "{again:#}"
    );
    let still = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .unwrap();
    let still_block: serde_json::Value = serde_json::from_str(&still).unwrap();
    assert_eq!(still_block["r_fresh_remote"], blocked_tip);
    assert_eq!(svn_youngest(&svn_url), svn_before);
    assert_eq!(git_sha(&git_work), handled);
    let commit_map_len = Database::new(&db_path)
        .unwrap()
        .list_commit_map(10_000)
        .unwrap()
        .len();
    assert_eq!(commit_map_len, 1);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R66_PERSONAL_BACKLOG_RESTART",
            "p":handled,
            "blocked_r":blocked_tip,
            "restored_remote":handled,
            "reason":"unsupported_backlog",
            "durable":true,
            "seeded_legacy_block":true,
            "svn_revision_before_after":svn_before,
            "mode":"personal",
            "inspection_refs_retargeted":true
        })
    );
}

fn git_sha_at(repo: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "rev-parse", rev])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct GitPushFaultGuard {
    scoped_key: String,
}

impl GitPushFaultGuard {
    fn lost_reply(git_repo_path: &Path) -> Self {
        let scoped_key =
            personal_git_push_fixture_env_key("REPOSYNC_GIT_PUSH_LOST_REPLY", git_repo_path);
        std::env::set_var(&scoped_key, "1");
        Self { scoped_key }
    }
}

struct SvnCommitFaultGuard {
    scoped_key: String,
}

impl SvnCommitFaultGuard {
    fn lost_reply(svn_wc_path: &Path) -> Self {
        let scoped_key =
            personal_svn_commit_fixture_env_key("REPOSYNC_SVN_COMMIT_LOST_REPLY", svn_wc_path);
        std::env::set_var(&scoped_key, "1");
        Self { scoped_key }
    }

    fn confirm_fail(svn_wc_path: &Path) -> Self {
        let scoped_key =
            personal_svn_commit_fixture_env_key("REPOSYNC_SVN_COMMIT_CONFIRM_FAIL", svn_wc_path);
        std::env::set_var(&scoped_key, "1");
        Self { scoped_key }
    }

    fn crash_before(svn_wc_path: &Path) -> Self {
        let scoped_key =
            personal_svn_commit_fixture_env_key("REPOSYNC_SVN_COMMIT_CRASH_BEFORE", svn_wc_path);
        std::env::set_var(&scoped_key, "1");
        Self { scoped_key }
    }
}

impl Drop for SvnCommitFaultGuard {
    fn drop(&mut self) {
        std::env::remove_var(&self.scoped_key);
    }
}

fn svn_youngest(svn_url: &str) -> i64 {
    let repo = svn_url.strip_prefix("file://").unwrap();
    String::from_utf8_lossy(
        &Command::new("svnlook")
            .args(["youngest", repo])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .parse::<i64>()
    .unwrap()
}

fn github_commit(sha: String, message: &str) -> reposync_core::git::github::GitHubCommit {
    use reposync_core::git::github::{GitHubCommit, GitHubCommitDetail, GitHubGitActor};
    GitHubCommit {
        sha,
        commit: GitHubCommitDetail {
            message: message.into(),
            author: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
            committer: GitHubGitActor {
                name: "Test User".into(),
                email: "test@example.com".into(),
                date: None,
            },
        },
        author: None,
    }
}

/// Minimal HTTP server that returns an empty GitHub pulls JSON array.
fn spawn_empty_github_pulls_api() -> String {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..8 {
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let body = "[]";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(Shutdown::Write);
            }
        }
    });
    format!("http://{}", addr)
}

fn personal_git_to_svn(
    svn_url: &str,
    db: Arc<Database>,
    svn_wc: PathBuf,
    git_work: PathBuf,
    data_dir: &Path,
) -> GitToSvnSync {
    personal_git_to_svn_with_github_api(
        svn_url,
        db,
        svn_wc,
        git_work,
        data_dir,
        "https://localhost:0/unused",
    )
}

fn personal_git_to_svn_with_github_api(
    svn_url: &str,
    db: Arc<Database>,
    svn_wc: PathBuf,
    git_work: PathBuf,
    data_dir: &Path,
    github_api_url: &str,
) -> GitToSvnSync {
    let config = make_test_config(svn_url, data_dir);
    let svn_client = SvnClient::new(svn_url, "", "");
    let github_client = reposync_core::git::github::GitHubClient::new(
        github_api_url,
        "unused",
        reposync_core::config::GitProvider::default(),
    );
    GitToSvnSync::new(svn_client, github_client, db, &config, svn_wc, git_work)
}

impl Drop for GitPushFaultGuard {
    fn drop(&mut self) {
        std::env::remove_var(&self.scoped_key);
    }
}

#[tokio::test]
async fn test_personal_svn_to_git_lost_reply_holds_without_checkpoint() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(
        &wc_path,
        "feature.txt",
        "lost reply\n",
        "Personal lost reply",
    );

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);
    let remote_before = git_sha_at(&bare_dir, "refs/heads/main");

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let _fault = GitPushFaultGuard::lost_reply(&git_work_dir);
    let err = syncer
        .sync()
        .await
        .expect_err("lost reply must hold, not succeed");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("reconciliation_required"),
        "expected reconciliation hold: {err_text}"
    );
    drop(_fault);

    let remote_after = git_sha_at(&bare_dir, "refs/heads/main");
    assert_ne!(
        remote_before, remote_after,
        "remote push must have landed before the hold"
    );
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        None,
        "watermark must not advance on uncertain outcome"
    );
    assert_eq!(
        db_arc.list_commit_map(10).unwrap().len(),
        0,
        "commit_map must not advance on uncertain outcome"
    );
    let op = db_arc
        .active_personal_git_push_operation()
        .unwrap()
        .expect("active personal svn-to-git push journal");
    assert_eq!(
        op.state,
        reposync_core::db::git_push_operations::GitPushOperationState::ReconciliationRequired
    );
    assert!(!op.resume_authorized);

    let syncer2 = SvnToGitSync::new(
        SvnClient::new(&svn_url, "", ""),
        git_arc.clone(),
        db_arc.clone(),
        make_test_config(&svn_url, tmp.path()),
    );
    let again = syncer2
        .sync()
        .await
        .expect_err("held push must block retry");
    assert!(
        format!("{again:#}").contains("reconciliation_required"),
        "{again:#}"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "PERSONAL_LOST_REPLY",
            "operation_id": op.id,
            "remote_before": remote_before,
            "remote_after": remote_after,
            "lifecycle": "reconciliation_required",
            "mode": "personal"
        })
    );
}

#[tokio::test]
async fn test_personal_git_to_svn_confirm_writes_commit_map() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("feature.txt"), "from git\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work,
        tmp.path(),
    );
    let svn_rev = syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "Add feature.txt"),
            7,
            "feature/x",
        )
        .await
        .expect("personal git-to-svn confirm must succeed");

    assert!(
        svn_rev > svn_before,
        "SVN must advance after a confirmed write"
    );
    assert_eq!(svn_youngest(&svn_url), svn_rev);
    assert!(
        db_arc
            .active_personal_svn_commit_operation()
            .unwrap()
            .is_none(),
        "confirmed journal must clear the active hold"
    );
    let mapped = db_arc
        .list_commit_map(10)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.direction == "git_to_svn")
        .collect::<Vec<_>>();
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].git_sha, git_sha);
    assert_eq!(mapped[0].svn_rev, svn_rev);
}

#[tokio::test]
async fn test_personal_git_to_svn_rename_removes_source() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("old.txt"), "rename me\n").unwrap();
    let seed_oid = git_client
        .commit(
            "Add old.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let seed_sha = seed_oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    syncer
        .replay_commit(&github_commit(seed_sha, "Add old.txt"), 1, "feature/x")
        .await
        .expect("seed commit must sync");

    let status = Command::new("git")
        .args(["mv", "old.txt", "new.txt"])
        .current_dir(&git_work)
        .status()
        .expect("git mv failed");
    assert!(status.success(), "git mv old.txt new.txt failed");
    let status = Command::new("git")
        .args(["commit", "-m", "Rename old.txt to new.txt"])
        .current_dir(&git_work)
        .status()
        .expect("git commit failed");
    assert!(status.success(), "git commit rename failed");
    let rename_sha = git_sha_at(&git_work, "HEAD");
    assert!(Command::new("git")
        .args(["-C", git_work.to_str().unwrap(), "push", "origin", "main"])
        .status()
        .unwrap()
        .success());

    let svn_rev = syncer
        .replay_commit(
            &github_commit(rename_sha.clone(), "Rename old.txt to new.txt"),
            2,
            "feature/x",
        )
        .await
        .expect("rename commit must sync");
    assert!(svn_rev > svn_before);

    assert!(
        !svn_wc.join("old.txt").exists(),
        "old SVN path must be removed after rename"
    );
    let new_content = std::fs::read_to_string(svn_wc.join("new.txt")).unwrap();
    assert_eq!(new_content, "rename me\n");

    let op = db_arc
        .latest_svn_commit_operation(PERSONAL_SCOPE_KEY)
        .unwrap()
        .expect("rename journal");
    assert_eq!(op.source_git_sha, rename_sha);
    let journal_paths: Vec<_> = op
        .intended_changed_paths
        .iter()
        .map(|p| (p.path.as_str(), p.action.as_str()))
        .collect();
    assert_eq!(journal_paths, vec![("new.txt", "A"), ("old.txt", "D")]);

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "PERSONAL_GIT_TO_SVN_RENAME",
            "sha": rename_sha,
            "journal_paths": journal_paths,
            "old_path_removed": true
        })
    );
}

#[tokio::test]
async fn test_personal_git_to_svn_sync_stays_held_without_svn_successor() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("feature.txt"), "no svn yet\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let _fault = SvnCommitFaultGuard::crash_before(&svn_wc);
    let err = syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "Add feature.txt"),
            8,
            "feature/x",
        )
        .await
        .expect_err("crash-before must hold without SVN successor");
    assert!(
        format!("{err:#}").contains("reconciliation_required"),
        "{err:#}"
    );
    drop(_fault);

    let syncer2 = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work,
        tmp.path(),
    );
    let blocked = syncer2
        .sync()
        .await
        .expect_err("sync must stay held when SVN has no proven successor");
    assert!(
        format!("{blocked:#}").contains("reconciliation_required"),
        "{blocked:#}"
    );
    assert!(
        db_arc
            .active_personal_svn_commit_operation()
            .unwrap()
            .is_some(),
        "journal must remain active"
    );
}

#[tokio::test]
async fn test_personal_git_to_svn_finalize_rejects_wrong_operation_trailer() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    use reposync_core::svn_commit::append_durable_git_to_svn_identity;

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("feature.txt"), "wrong op id\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let _fault = SvnCommitFaultGuard::crash_before(&svn_wc);
    syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "Add feature.txt"),
            8,
            "feature/x",
        )
        .await
        .expect_err("crash-before must hold");
    drop(_fault);

    let op = db_arc
        .active_personal_svn_commit_operation()
        .unwrap()
        .expect("active journal");
    let wrong_message =
        append_durable_git_to_svn_identity("Add feature.txt", &git_sha, "wrong-operation-id");
    let svn_after = svn_commit_file(&svn_wc, "feature.txt", "wrong op id\n", &wrong_message);
    assert_eq!(svn_after, svn_before + 1);

    let syncer2 = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work,
        tmp.path(),
    );
    let blocked = syncer2
        .sync()
        .await
        .expect_err("wrong RepoSync-Operation trailer must not finalize");
    assert!(
        format!("{blocked:#}").contains("reconciliation_required"),
        "{blocked:#}"
    );
    assert_eq!(
        db_arc
            .active_personal_svn_commit_operation()
            .unwrap()
            .expect("journal")
            .id,
        op.id
    );
    assert_eq!(
        svn_youngest(&svn_url),
        svn_after,
        "must not write SVN again"
    );
}

#[tokio::test]
async fn test_personal_git_to_svn_lost_reply_holds_without_checkpoint() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("feature.txt"), "lost reply\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let _fault = SvnCommitFaultGuard::lost_reply(&svn_wc);
    let err = syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "Add feature.txt"),
            8,
            "feature/x",
        )
        .await
        .expect_err("lost reply must hold, not succeed");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("reconciliation_required"),
        "expected reconciliation hold: {err_text}"
    );
    drop(_fault);

    let svn_after = svn_youngest(&svn_url);
    assert!(
        svn_after > svn_before,
        "SVN commit must have landed before the hold"
    );
    assert_eq!(
        db_arc
            .list_commit_map(10)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.direction == "git_to_svn")
            .count(),
        0,
        "commit_map must not advance on uncertain outcome"
    );
    let op = db_arc
        .active_personal_svn_commit_operation()
        .unwrap()
        .expect("active personal git-to-svn commit journal");
    assert_eq!(
        op.state,
        reposync_core::db::svn_commit_operations::SvnCommitOperationState::ReconciliationRequired
    );
    assert!(!op.resume_authorized);

    let github_api = spawn_empty_github_pulls_api();
    let syncer2 = personal_git_to_svn_with_github_api(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
        &github_api,
    );
    let sync_result = syncer2
        .sync()
        .await
        .expect("sync must finalize a proven lost-reply hold without replay_commit");
    assert_eq!(sync_result.commits_synced, 0);
    assert_eq!(sync_result.prs_synced, 0);
    assert_eq!(
        svn_youngest(&svn_url),
        svn_after,
        "held retry must not write SVN again"
    );
    assert!(db_arc
        .active_personal_svn_commit_operation()
        .unwrap()
        .is_none());
    assert!(
        db_arc
            .list_commit_map(10)
            .unwrap()
            .into_iter()
            .any(|entry| entry.direction == "git_to_svn" && entry.svn_rev == svn_after),
        "commit_map must record the held git-to-svn mapping"
    );
    assert!(
        svn_wc.join("feature.txt").is_file(),
        "feature.txt must remain in SVN working copy after sync finalize"
    );
    assert_eq!(
        std::fs::read_to_string(svn_wc.join("feature.txt")).unwrap(),
        "lost reply\n"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "PERSONAL_GIT_TO_SVN_LOST_REPLY",
            "operation_id": op.id,
            "svn_before": svn_before,
            "svn_after": svn_after,
            "lifecycle": "reconciliation_required",
            "mode": "personal"
        })
    );
}

#[tokio::test]
async fn test_personal_svn_to_git_defers_while_git_to_svn_running() {
    use reposync_core::db::svn_commit_operations::{
        svn_commit_target_fingerprint, SvnCommitIntent, SvnCommitOperationState,
    };
    use reposync_core::svn_commit::hash_regular_file_tree;

    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("feature.txt"), "running hold\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    let (parent, tree) = git_client.commit_parent_and_tree(&git_sha).unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, svn_before);
    db_arc
        .set_watermark("svn_rev", &svn_before.to_string())
        .unwrap();

    let svn_client = SvnClient::new(&svn_url, "", "");
    let info = svn_client.info().await.unwrap();
    let pre_write_tree = hash_regular_file_tree(&svn_wc).unwrap();
    let fingerprint =
        svn_commit_target_fingerprint("personal", &info.uuid, svn_client.url(), &info.url, "{}");
    db_arc
        .begin_git_to_svn_commit(SvnCommitIntent {
            repo_id: "personal",
            initiator_id: "personal_worker",
            request_id: "running-hold-test",
            target_fingerprint: &fingerprint,
            source_git_sha: &git_sha,
            source_git_parent: parent.as_deref(),
            source_git_tree: &tree,
            target_svn_uuid: &info.uuid,
            target_svn_path: &info.url,
            target_svn_root_url: &info.root_url,
            target_svn_branch_path: "",
            pre_write_svn_rev: svn_before,
            pre_write_svn_tree: &pre_write_tree,
            projection: "{}",
            intended_changed_paths: vec![],
            intended_svn_tree: &pre_write_tree,
            author: "testuser",
            source_message: "Add feature.txt",
        })
        .expect("failed to seed Running git-to-svn journal");

    let running = db_arc
        .active_personal_svn_commit_operation()
        .unwrap()
        .expect("Running git-to-svn journal");
    assert_eq!(running.state, SvnCommitOperationState::Running);

    svn_commit_file(&svn_wc, "external.txt", "external\n", "External SVN commit");
    let held_rev = svn_youngest(&svn_url);
    assert_eq!(held_rev, svn_before + 1);

    let git_work_dir = tmp.path().join("git_work_svn_to_git");
    let bare_dir = tmp.path().join("origin_svn_to_git.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_to_git_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let svn_to_git = SvnToGitSync::new(svn_to_git_client, git_arc, db_arc.clone(), config);
    let blocked = svn_to_git
        .sync()
        .await
        .expect_err("Running git-to-svn journal must block svn-to-git");
    let blocked_text = format!("{blocked:#}");
    assert!(
        blocked_text.contains("git-to-svn") || blocked_text.contains("deferring"),
        "expected hold or defer: {blocked_text}"
    );
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some(svn_before.to_string().as_str()),
        "watermark must not advance while git-to-svn is Running"
    );
    assert!(
        !db_arc
            .list_commit_map(10)
            .unwrap()
            .into_iter()
            .any(|entry| entry.direction == "svn_to_git" && entry.svn_rev == held_rev),
        "svn-to-git must not import the revision claimed by the Running journal"
    );
}

#[tokio::test]
async fn test_personal_svn_to_git_defers_while_git_to_svn_reconciliation_required() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    git_client.push("origin", "main").unwrap();
    std::fs::write(git_work.join("feature.txt"), "lost reply hold\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let git_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, 1);
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let _fault = SvnCommitFaultGuard::lost_reply(&svn_wc);
    let err = syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "Add feature.txt"),
            10,
            "feature/x",
        )
        .await
        .expect_err("lost reply must hold git-to-svn");
    assert!(
        format!("{err:#}").contains("reconciliation_required"),
        "{err:#}"
    );
    drop(_fault);

    let held = db_arc
        .active_personal_svn_commit_operation()
        .unwrap()
        .expect("held git-to-svn journal");
    assert_eq!(
        held.state,
        reposync_core::db::svn_commit_operations::SvnCommitOperationState::ReconciliationRequired
    );
    assert!(!held.resume_authorized);

    let held_rev = svn_youngest(&svn_url);
    db_arc
        .set_watermark("svn_rev", &(held_rev - 1).to_string())
        .unwrap();

    let git_work_dir = tmp.path().join("git_work_svn_to_git");
    let bare_dir = tmp.path().join("origin_svn_to_git.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);
    let remote_before = git_sha_at(&bare_dir, "refs/heads/main");
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let svn_to_git = SvnToGitSync::new(svn_client, git_arc, db_arc.clone(), config);

    let blocked = svn_to_git
        .sync()
        .await
        .expect_err("held git-to-svn journal must block svn-to-git retry");
    let blocked_text = format!("{blocked:#}");
    assert!(
        blocked_text.contains("reconciliation_required")
            || blocked_text.contains("deferring")
            || blocked_text.contains("git-to-svn"),
        "expected hold or defer: {blocked_text}"
    );
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some((held_rev - 1).to_string().as_str()),
        "watermark must not advance while git-to-svn is held"
    );
    assert!(
        !db_arc
            .list_commit_map(10)
            .unwrap()
            .into_iter()
            .any(|entry| entry.direction == "svn_to_git" && entry.svn_rev == held_rev),
        "svn-to-git must not import the held SVN revision"
    );
    let remote_after = git_sha_at(&bare_dir, "refs/heads/main");
    assert_eq!(
        remote_before, remote_after,
        "svn-to-git retry must not push a duplicate Git commit"
    );

    let retry = svn_to_git
        .sync()
        .await
        .expect_err("second svn-to-git retry must remain blocked");
    assert!(
        format!("{retry:#}").contains("reconciliation_required")
            || format!("{retry:#}").contains("deferring")
            || format!("{retry:#}").contains("git-to-svn"),
        "{retry:#}"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "PERSONAL_SVN_TO_GIT_BLOCKED_BY_GIT_TO_SVN_HOLD",
            "operation_id": held.id,
            "lifecycle": "reconciliation_required",
            "mode": "personal"
        })
    );
}

#[tokio::test]
async fn test_personal_git_to_svn_defers_while_svn_to_git_reconciliation_required() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(
        &wc_path,
        "feature.txt",
        "svn to git hold\n",
        "Personal svn-to-git hold",
    );

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc.clone(), db_arc.clone(), config);
    let _fault = GitPushFaultGuard::lost_reply(&git_work_dir);
    let err = syncer
        .sync()
        .await
        .expect_err("lost reply must hold svn-to-git");
    assert!(
        format!("{err:#}").contains("reconciliation_required"),
        "{err:#}"
    );
    drop(_fault);

    let svn_before = svn_youngest(&svn_url);
    let commit_map_before = db_arc.list_commit_map(10).unwrap().len();

    let held = db_arc
        .active_personal_git_push_operation()
        .unwrap()
        .expect("held svn-to-git journal");
    assert_eq!(
        held.state,
        reposync_core::db::git_push_operations::GitPushOperationState::ReconciliationRequired
    );
    assert!(!held.resume_authorized);

    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    let git_to_svn =
        personal_git_to_svn(&svn_url, db_arc.clone(), svn_wc, git_work_dir, tmp.path());
    let blocked = git_to_svn
        .sync()
        .await
        .expect_err("held svn-to-git journal must block git-to-svn");
    assert!(
        format!("{blocked:#}").contains("reconciliation_required")
            || format!("{blocked:#}").contains("svn-to-git"),
        "{blocked:#}"
    );
    assert_eq!(
        svn_youngest(&svn_url),
        svn_before,
        "SVN youngest revision must not change while svn-to-git is held"
    );
    assert_eq!(
        db_arc.list_commit_map(10).unwrap().len(),
        commit_map_before,
        "commit_map must not change while svn-to-git is held"
    );
    let blocked_again = git_to_svn
        .sync()
        .await
        .expect_err("retry must still refuse while hold persists");
    assert!(
        format!("{blocked_again:#}").contains("reconciliation_required")
            || format!("{blocked_again:#}").contains("svn-to-git"),
        "{blocked_again:#}"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "PERSONAL_GIT_TO_SVN_BLOCKED_BY_SVN_TO_GIT_HOLD",
            "operation_id": held.id,
            "lifecycle": "reconciliation_required",
            "mode": "personal"
        })
    );
}

fn spawn_github_exists_stub() -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = github_stub_http_response("{}");
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{}", port), handle)
}

/// Missing handled Git cursor blocks personal sync before any SVN write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_personal_missing_checkpoint_blocks_before_svn_write() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::history_block_key;
    use reposync_personal::engine::PersonalSyncEngine;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let _git_client = setup_git_with_bare_origin(&git_work, &bare);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    drop(db);
    let config = make_test_config(&svn_url, tmp.path());
    let engine = PersonalSyncEngine::new(
        config,
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let err = engine
        .run_cycle()
        .await
        .expect_err("missing checkpoint must block before SVN writes");
    assert!(format!("{err:#}").contains("missing_checkpoint"), "{err:#}");
    let block = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .expect("missing checkpoint must persist a history block");
    let block: serde_json::Value = serde_json::from_str(&block).unwrap();
    assert_eq!(block["reason"], "missing_checkpoint");
    assert_eq!(svn_youngest(&svn_url), svn_before);
    assert_eq!(
        Database::new(&db_path)
            .unwrap()
            .list_commit_map(10)
            .unwrap()
            .len(),
        0
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_PERSONAL_MISSING_CHECKPOINT",
            "reason":"missing_checkpoint",
            "svn_revision_before_after":svn_before,
            "mode":"personal"
        })
    );
}

/// Missing origin remote blocks personal sync before any SVN write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_personal_missing_origin_blocks_before_svn_write() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::history_block_key;
    use reposync_personal::engine::PersonalSyncEngine;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    std::fs::write(git_work.join("feature.txt"), "first version\n").unwrap();
    git_client
        .commit(
            "First Git change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    git_client.push("origin", "main").unwrap();
    let handled = git_sha(&git_work);
    drop(git_client);
    git_cmd(&git_work, &["remote", "remove", "origin"]);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &handled, "git_to_svn", "testuser", "Test User")
        .unwrap();
    drop(db);

    let config = make_test_config(&svn_url, tmp.path());
    let engine = PersonalSyncEngine::new(
        config,
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let err = engine
        .run_cycle()
        .await
        .expect_err("missing origin must block before SVN writes");
    assert!(format!("{err:#}").contains("missing_origin"), "{err:#}");
    let block = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .expect("missing origin must persist a history block");
    let block: serde_json::Value = serde_json::from_str(&block).unwrap();
    assert_eq!(block["reason"], "missing_origin");
    assert_eq!(svn_youngest(&svn_url), svn_before);
    assert_eq!(
        Database::new(&db_path)
            .unwrap()
            .list_commit_map(10)
            .unwrap()
            .len(),
        1
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_PERSONAL_MISSING_ORIGIN",
            "reason":"missing_origin",
            "p":handled,
            "svn_revision_before_after":svn_before,
            "mode":"personal"
        })
    );
}

/// Initial import still owns initialization without the personal inspect gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_personal_initial_import_still_works() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_personal::initial_import::{ImportMode, InitialImport};

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "seed.txt", "SVN seed\n", "Snapshot seed");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    let git_client = GitClient::init(&git_work).unwrap();
    {
        let repo = git2::Repository::open(&git_work).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
    }
    git_cmd(&git_work, &["checkout", "-b", "main"]);
    let git_client = Arc::new(Mutex::new(git_client));

    let db_path = tmp.path().join("personal.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();

    let (api_url, stub) = spawn_github_exists_stub();
    let config = PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 5,
            log_level: "debug".into(),
            data_dir: tmp.path().to_path_buf(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.clone(),
            username: String::new(),
            password_env: "REPOSYNC_TEST_SVN_PW".into(),
            password: Some(String::new()),
        },
        github: PersonalGitHubConfig {
            api_url,
            git_base_url: None,
            repo: "test/test-repo".into(),
            token_env: "REPOSYNC_TEST_GH_TOKEN".into(),
            default_branch: "main".into(),
            auto_create: false,
            private: true,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "testuser".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    };

    let svn_client = SvnClient::new(&svn_url, "", "");
    let github_client = GitHubClient::new(&config.github.api_url, "unused", GitProvider::GitHub);
    let formatter = CommitFormatter::new(&config.commit_format);
    let importer = InitialImport {
        svn_client: &svn_client,
        git_client: &git_client,
        github_client: &github_client,
        db: &db,
        config: &config,
        formatter: &formatter,
    };
    let count = importer
        .import(ImportMode::Snapshot)
        .await
        .expect("initial snapshot import must succeed");
    drop(stub);

    assert_eq!(count, 1);
    assert_eq!(db.list_commit_map(10).unwrap().len(), 1);
    assert!(
        db.list_commit_map(10)
            .unwrap()
            .iter()
            .all(|entry| entry.repo_id.as_deref() == Some(PERSONAL_SCOPE_KEY)),
        "initial import must tag commit_map rows with the personal scope key"
    );
    assert!(db.get_watermark("svn_rev").unwrap().is_some());
    assert!(db.get_watermark("git_sha").unwrap().is_some());
    assert!(
        git_sha_at(&bare, "refs/heads/main").len() >= 40,
        "snapshot import must push to origin"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_PERSONAL_INITIAL_IMPORT",
            "commits":count,
            "mode":"personal"
        })
    );
}

/// Healthy personal pairs keep syncing after mapping writes advance commit_map
/// ahead of the import git_sha watermark.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_personal_checkpoint_progress_survives_mapping_writes() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::inspect_personal_history;
    use reposync_personal::engine::PersonalSyncEngine;
    use reposync_personal::initial_import::{ImportMode, InitialImport};

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "seed.txt", "SVN seed\n", "Snapshot seed");
    let svn_after_import = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    let git_client = GitClient::init(&git_work).unwrap();
    {
        let repo = git2::Repository::open(&git_work).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
    }
    git_cmd(&git_work, &["checkout", "-b", "main"]);
    let git_client = Arc::new(Mutex::new(git_client));

    let db_path = tmp.path().join("personal.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();

    let (api_url, stub) = spawn_github_exists_stub();
    let config = PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 5,
            log_level: "debug".into(),
            data_dir: tmp.path().to_path_buf(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.clone(),
            username: String::new(),
            password_env: "REPOSYNC_TEST_SVN_PW".into(),
            password: Some(String::new()),
        },
        github: PersonalGitHubConfig {
            api_url,
            git_base_url: None,
            repo: "test/test-repo".into(),
            token_env: "REPOSYNC_TEST_GH_TOKEN".into(),
            default_branch: "main".into(),
            auto_create: false,
            private: true,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "testuser".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    };

    let svn_client = SvnClient::new(&svn_url, "", "");
    let github_client = GitHubClient::new(&config.github.api_url, "unused", GitProvider::GitHub);
    let formatter = CommitFormatter::new(&config.commit_format);
    let importer = InitialImport {
        svn_client: &svn_client,
        git_client: &git_client,
        github_client: &github_client,
        db: &db,
        config: &config,
        formatter: &formatter,
    };
    importer
        .import(ImportMode::Snapshot)
        .await
        .expect("snapshot import must succeed");
    drop(stub);

    let import_sha = db
        .get_watermark("git_sha")
        .unwrap()
        .expect("import watermark");
    let import_map = db.get_last_git_hash().unwrap().expect("import mapping");
    assert_eq!(import_sha, import_map);

    inspect_personal_history(&db, &git_work, "main", "personal")
        .expect("post-import inspect must admit")
        .expect("origin and checkpoint require inspection");

    let engine = PersonalSyncEngine::new(
        config.clone(),
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new(&config.github.api_url, "unused", GitProvider::GitHub),
    );
    engine
        .run_cycle()
        .await
        .expect("idle post-import cycle must admit");

    svn_commit_file(&wc, "from_svn.txt", "SVN change\n", "SVN-side change");
    let stats = engine
        .run_cycle()
        .await
        .expect("SVN→Git cycle must admit after mapping progress");
    assert_eq!(
        stats.svn_to_git_count, 1,
        "expected one SVN revision synced"
    );

    let svn_to_git_sha = db
        .get_last_git_hash()
        .unwrap()
        .expect("mapping after svn→git");
    assert_ne!(
        svn_to_git_sha, import_sha,
        "commit_map must advance beyond import watermark"
    );
    assert_eq!(
        db.get_watermark("git_sha").unwrap().as_deref(),
        Some(import_sha.as_str()),
        "import git_sha watermark stays at snapshot baseline"
    );
    inspect_personal_history(&db, &git_work, "main", "personal")
        .expect("inspect after svn→git must not false-block")
        .expect("origin and checkpoint require inspection");

    std::fs::write(git_work.join("from_git.txt"), "Git change\n").unwrap();
    let git_client = GitClient::new(&git_work).unwrap();
    let git_commit = git_client
        .commit(
            "Git-side change",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    git_client.push("origin", "main").unwrap();
    drop(git_client);

    let svn_wc = tmp.path().join("svn-wc");
    svn_checkout(&svn_url, &svn_wc);
    let db_arc = Arc::new(Database::new(&db_path).unwrap());
    let syncer = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        svn_wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let svn_rev = syncer
        .replay_commit(
            &github_commit(git_commit.clone(), "Git-side change"),
            9,
            "feature/git",
        )
        .await
        .expect("real git→svn mapping must succeed");
    assert!(svn_rev > svn_after_import);
    assert_eq!(
        db_arc.get_last_git_hash().unwrap().as_deref(),
        Some(git_commit.as_str())
    );
    assert_eq!(
        db_arc.get_watermark("git_sha").unwrap().as_deref(),
        Some(import_sha.as_str()),
        "git_sha watermark remains at import baseline after git→svn"
    );

    engine
        .run_cycle()
        .await
        .expect("cycle after git→svn must not trip ambiguous_checkpoint");
    inspect_personal_history(&db_arc, &git_work, "main", "personal")
        .expect("inspect after git→svn must admit")
        .expect("origin and checkpoint require inspection");

    svn_commit_file(&wc, "from_svn2.txt", "Second SVN\n", "Second SVN change");
    let stats = engine
        .run_cycle()
        .await
        .expect("further SVN→Git cycle must admit");
    assert_eq!(stats.svn_to_git_count, 1);

    assert_eq!(
        std::fs::read_to_string(git_work.join("from_svn.txt")).unwrap(),
        "SVN change\n"
    );
    assert_eq!(
        std::fs::read_to_string(git_work.join("from_svn2.txt")).unwrap(),
        "Second SVN\n"
    );
    assert_eq!(
        std::fs::read_to_string(svn_wc.join("from_git.txt")).unwrap(),
        "Git change\n"
    );
    assert!(svn_rev > svn_after_import);
    assert_eq!(
        svn_youngest(&svn_url),
        4,
        "SVN must reflect git→svn and later commits"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_PERSONAL_CHECKPOINT_PROGRESS",
            "import_sha":import_sha,
            "svn_to_git_sha":svn_to_git_sha,
            "git_to_svn_sha":git_commit,
            "svn_revision":svn_rev,
            "mode":"personal"
        })
    );
}

/// Unrelated commit_map and git_sha watermark copies still fail closed before SVN writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_personal_ambiguous_checkpoint_blocks_before_svn_write() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_core::history_inspect::history_block_key;
    use reposync_personal::engine::PersonalSyncEngine;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let watermark = git_sha(&git_work);
    git_cmd(&git_work, &["checkout", "--orphan", "foreign"]);
    std::fs::write(git_work.join("foreign.txt"), "foreign\n").unwrap();
    git_cmd(&git_work, &["add", "foreign.txt"]);
    git_client
        .commit(
            "Foreign history",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let unrelated = git_sha(&git_work);
    drop(git_client);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &unrelated, "git_to_svn", "testuser", "Test User")
        .unwrap();
    db.set_watermark("git_sha", &watermark).unwrap();
    drop(db);

    let config = make_test_config(&svn_url, tmp.path());
    let engine = PersonalSyncEngine::new(
        config,
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, "", ""),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new("http://127.0.0.1:1", "unused", GitProvider::GitHub),
    );
    let err = engine
        .run_cycle()
        .await
        .expect_err("unrelated checkpoint copies must block before SVN writes");
    assert!(
        format!("{err:#}").contains("ambiguous_checkpoint"),
        "{err:#}"
    );
    let block = Database::new(&db_path)
        .unwrap()
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .expect("ambiguous checkpoint must persist a history block");
    let block: serde_json::Value = serde_json::from_str(&block).unwrap();
    assert_eq!(block["reason"], "ambiguous_checkpoint");
    assert_eq!(block["state"], "reconciliation_required");
    assert_eq!(svn_youngest(&svn_url), svn_before);
    assert_eq!(
        Database::new(&db_path)
            .unwrap()
            .list_commit_map(10)
            .unwrap()
            .len(),
        1
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_PERSONAL_AMBIGUOUS_CHECKPOINT",
            "reason":"ambiguous_checkpoint",
            "watermark":watermark,
            "mapping":unrelated,
            "svn_revision_before_after":svn_before,
            "mode":"personal"
        })
    );
}

/// Git ancestry command failures are distinct from rewrite claims and block replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_personal_ancestry_command_failed_blocks_before_svn_write() {
    use reposync_core::history_inspect::history_block_key;

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "origin.txt", "SVN origin\n", "Verified SVN origin");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let watermark = git_sha(&git_work);
    std::fs::write(git_work.join("progress.txt"), "progress\n").unwrap();
    git_cmd(&git_work, &["add", "progress.txt"]);
    git_client
        .commit(
            "Progress after import",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let mapping = git_sha(&git_work);
    drop(git_client);

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(1, &mapping, "svn_to_git", "testuser", "Test User")
        .unwrap();
    db.set_watermark("git_sha", &watermark).unwrap();
    let db_arc = Arc::new(db);

    let watermark_object = git_work.join(format!(
        ".git/objects/{}/{}",
        &watermark[..2],
        &watermark[2..]
    ));
    assert!(
        watermark_object.exists(),
        "watermark object must exist before corruption"
    );
    std::fs::remove_file(&watermark_object).unwrap();

    let sync = personal_git_to_svn(
        &svn_url,
        db_arc.clone(),
        wc.clone(),
        git_work.clone(),
        tmp.path(),
    );
    let commit = github_commit(mapping.clone(), "Progress after import");
    let err = sync
        .replay_commit(&commit, 1, "main")
        .await
        .expect_err("ancestry command failure must block before SVN writes");
    assert!(
        format!("{err:#}").contains("ancestry_command_failed"),
        "{err:#}"
    );
    assert_eq!(svn_youngest(&svn_url), svn_before);
    let block = db_arc
        .get_state(&history_block_key(Some(PERSONAL_SCOPE_KEY)))
        .unwrap()
        .expect("ancestry command failure must persist a history block");
    let block: serde_json::Value = serde_json::from_str(&block).unwrap();
    assert_eq!(block["reason"], "ancestry_command_failed");
    assert_eq!(block["state"], "reconciliation_required");
    assert_eq!(block["durable"], true);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_PERSONAL_ANCESTRY_COMMAND_FAILED",
            "reason":"ancestry_command_failed",
            "watermark":watermark,
            "mapping":mapping,
            "svn_revision_before_after":svn_before,
            "mode":"personal"
        })
    );
}

fn svn_path_exists_at_head(svn_url: &str, path: &str) -> bool {
    let target = format!("{}/{}@HEAD", svn_url, path);
    Command::new("svn")
        .args(["cat", &target, "--non-interactive"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Minimal HTTP/1.1 response for raw-TCP GitHub API stubs in this file.
///
/// `Connection: close` is required: reqwest pools keep-alive sockets, but these
/// stubs accept once per request and do not read further bytes on the same TCP
/// connection. Without close, a later `sync_pr` call can reuse a dead socket and
/// fail with "connection closed before message completed".
fn github_stub_http_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn spawn_github_two_pr_sync_stub(
    pr1_merge_sha: &str,
    pr1_commit_sha: &str,
    pr1_merged_at: &str,
    pr2_merge_sha: &str,
    pr2_commit_sha: &str,
    pr2_merged_at: &str,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let pulls_body = format!(
        r#"[{{
            "number": 1,
            "title": "LFS PR",
            "html_url": "https://example.invalid/pr/1",
            "state": "closed",
            "head": {{"ref": "feature/lfs", "sha": "{pr1_commit_sha}"}},
            "base": {{"ref": "main", "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},
            "merged": true,
            "merge_commit_sha": "{pr1_merge_sha}",
            "merged_at": "{pr1_merged_at}"
        }}, {{
            "number": 2,
            "title": "Second PR",
            "html_url": "https://example.invalid/pr/2",
            "state": "closed",
            "head": {{"ref": "feature/second", "sha": "{pr2_commit_sha}"}},
            "base": {{"ref": "main", "sha": "cccccccccccccccccccccccccccccccccccccccc"}},
            "merged": true,
            "merge_commit_sha": "{pr2_merge_sha}",
            "merged_at": "{pr2_merged_at}"
        }}]"#,
        pr1_commit_sha = pr1_commit_sha,
        pr1_merge_sha = pr1_merge_sha,
        pr1_merged_at = pr1_merged_at,
        pr2_commit_sha = pr2_commit_sha,
        pr2_merge_sha = pr2_merge_sha,
        pr2_merged_at = pr2_merged_at,
    );
    let pr1_commits_body = format!(
        r#"[{{
            "sha": "{pr1_commit_sha}",
            "commit": {{
                "message": "Add LFS and sibling",
                "author": {{"name": "Test User", "email": "test@example.com", "date": null}},
                "committer": {{"name": "Test User", "email": "test@example.com", "date": null}}
            }},
            "author": null
        }}]"#,
        pr1_commit_sha = pr1_commit_sha
    );
    let pr2_commits_body = format!(
        r#"[{{
            "sha": "{pr2_commit_sha}",
            "commit": {{
                "message": "Second merged PR",
                "author": {{"name": "Test User", "email": "test@example.com", "date": null}},
                "committer": {{"name": "Test User", "email": "test@example.com", "date": null}}
            }},
            "author": null
        }}]"#,
        pr2_commit_sha = pr2_commit_sha
    );
    let merge_detail_body = |merge_sha: &str, parents: &[&str]| {
        let parent_json = parents
            .iter()
            .map(|sha| format!(r#"{{"sha": "{sha}"}}"#))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"{{
            "sha": "{merge_sha}",
            "commit": {{
                "message": "Merge PR",
                "author": {{"name": "Test User", "email": "test@example.com", "date": null}},
                "committer": {{"name": "Test User", "email": "test@example.com", "date": null}}
            }},
            "parents": [{parent_json}]
        }}"#,
            merge_sha = merge_sha,
            parent_json = parent_json
        )
    };
    let pr1_merge_detail = merge_detail_body(
        pr1_merge_sha,
        &["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", pr1_commit_sha],
    );
    let pr2_merge_detail = merge_detail_body(
        pr2_merge_sha,
        &["cccccccccccccccccccccccccccccccccccccccc", pr2_commit_sha],
    );
    let pr1_merge_path = format!("/commits/{}", pr1_merge_sha);
    let pr2_merge_path = format!("/commits/{}", pr2_merge_sha);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for _ in 0..16 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.contains("/pulls/1/commits") {
                    pr1_commits_body.clone()
                } else if req.contains("/pulls/2/commits") {
                    pr2_commits_body.clone()
                } else if req.contains(&pr1_merge_path) {
                    pr1_merge_detail.clone()
                } else if req.contains(&pr2_merge_path) {
                    pr2_merge_detail.clone()
                } else if req.contains("/pulls") {
                    pulls_body.clone()
                } else {
                    "[]".to_string()
                };
                let response = github_stub_http_response(&body);
                let _ = stream.write_all(response.as_bytes());
            }
        }
    });
    (format!("http://127.0.0.1:{}", port), handle)
}

fn spawn_github_pr_sync_stub(
    merge_sha: &str,
    pr_commit_sha: &str,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let pulls_body = format!(
        r#"[{{
            "number": 1,
            "title": "Feature PR",
            "html_url": "https://example.invalid/pr/1",
            "state": "closed",
            "head": {{"ref": "feature", "sha": "{pr_commit_sha}"}},
            "base": {{"ref": "main", "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},
            "merged": true,
            "merge_commit_sha": "{merge_sha}",
            "merged_at": "2025-01-02T00:00:00Z"
        }}]"#,
        pr_commit_sha = pr_commit_sha,
        merge_sha = merge_sha
    );
    let commits_body = format!(
        r#"[{{
            "sha": "{pr_commit_sha}",
            "commit": {{
                "message": "Add feature from PR",
                "author": {{"name": "Test User", "email": "test@example.com", "date": null}},
                "committer": {{"name": "Test User", "email": "test@example.com", "date": null}}
            }},
            "author": null
        }}]"#,
        pr_commit_sha = pr_commit_sha
    );
    let merge_detail_body = format!(
        r#"{{
            "sha": "{merge_sha}",
            "commit": {{
                "message": "Merge PR",
                "author": {{"name": "Test User", "email": "test@example.com", "date": null}},
                "committer": {{"name": "Test User", "email": "test@example.com", "date": null}}
            }},
            "parents": [
                {{"sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},
                {{"sha": "{pr_commit_sha}"}}
            ]
        }}"#,
        merge_sha = merge_sha,
        pr_commit_sha = pr_commit_sha
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        // Two sync_pr passes × (commits list + merge detail) = 4 requests; keep
        // headroom for reqwest opening spare connections during pool churn.
        for _ in 0..16 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.contains("/pulls/1/commits") {
                    commits_body.clone()
                } else if req.contains("/commits/") {
                    merge_detail_body.clone()
                } else if req.contains("/pulls") {
                    pulls_body.clone()
                } else {
                    "[]".to_string()
                };
                let response = github_stub_http_response(&body);
                let _ = stream.write_all(response.as_bytes());
            }
        }
    });
    (format!("http://127.0.0.1:{}", port), handle)
}

/// Managed snapshot import leaves an untagged commit_map row with no sync_record.
/// Personal mode at watermark 0 must still import SVN r1 instead of treating it
/// as already synced.
#[tokio::test]
async fn test_personal_svn_to_git_imports_after_managed_snapshot_null_commit_map() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(
        &wc_path,
        "shared.txt",
        "managed snapshot baseline\n",
        "Managed snapshot r1",
    );

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    db.insert_commit_map_with_repo(
        1,
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "svn_to_git",
        "snapshot",
        "Snapshot <snap@example.com>",
        None,
    )
    .unwrap();
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), None);
    assert!(!db.is_personal_svn_rev_synced(1).unwrap());

    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc, db_arc.clone(), config);
    let synced = syncer
        .sync()
        .await
        .expect("personal svn-to-git must import managed snapshot collision at r1");
    assert_eq!(synced, 1, "revision 1 must be imported, not skipped");
    let head_after = git_sha(&git_work_dir);
    assert_ne!(
        head_after, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "personal import must create a new Git commit, not pass on the seeded NULL row"
    );
    assert!(
        db_arc.list_commit_map(10).unwrap().iter().any(|entry| {
            entry.svn_rev == 1
                && entry.direction == "svn_to_git"
                && entry.repo_id.as_deref() == Some(PERSONAL_SCOPE_KEY)
                && entry.git_sha == head_after
        }),
        "personal import must record a scoped commit_map row for the emitted Git SHA"
    );
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some("1"),
        "personal svn_rev watermark must advance after import"
    );
}

/// A crash after scoped `commit_map` but before the watermark must not create a
/// second Git commit when `SvnToGitSync::sync` resumes.
#[tokio::test]
async fn test_personal_initial_import_crash_resume_no_duplicate_commit() {
    use reposync_core::config::GitProvider;
    use reposync_core::git::github::GitHubClient;
    use reposync_personal::initial_import::{ImportMode, InitialImport};

    if !svn_available() {
        panic!("svn and svnadmin are required; do not count a skipped diagnostic as evidence");
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "resume.txt", "resume seed\n", "resume seed");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    let git_client = GitClient::init(&git_work).unwrap();
    {
        let repo = git2::Repository::open(&git_work).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
    }
    git_cmd(&git_work, &["checkout", "-b", "main"]);
    let git_arc = Arc::new(Mutex::new(git_client));

    let db_path = tmp.path().join("personal.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();

    let (api_url, stub) = spawn_github_exists_stub();
    let config = PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 5,
            log_level: "debug".into(),
            data_dir: tmp.path().to_path_buf(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.clone(),
            username: String::new(),
            password_env: "REPOSYNC_TEST_SVN_PW".into(),
            password: Some(String::new()),
        },
        github: PersonalGitHubConfig {
            api_url,
            git_base_url: None,
            repo: "test/test-repo".into(),
            token_env: "REPOSYNC_TEST_GH_TOKEN".into(),
            default_branch: "main".into(),
            auto_create: false,
            private: true,
            token: Some("unused".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: "testuser".into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    };

    let svn_client = SvnClient::new(&svn_url, "", "");
    let github_client = GitHubClient::new(&config.github.api_url, "unused", GitProvider::GitHub);
    let formatter = CommitFormatter::new(&config.commit_format);
    let importer = InitialImport {
        svn_client: &svn_client,
        git_client: &git_arc,
        github_client: &github_client,
        db: &db,
        config: &config,
        formatter: &formatter,
    };
    importer
        .import(ImportMode::Snapshot)
        .await
        .expect("initial snapshot import must succeed");
    drop(stub);

    let head_before = git_sha(&git_work);
    db.conn()
        .execute("DELETE FROM watermarks WHERE source = 'svn_rev'", [])
        .unwrap();
    db.conn().execute("DELETE FROM sync_records", []).unwrap();
    assert!(
        db.is_personal_svn_rev_synced(1).unwrap(),
        "scoped commit_map alone must count as synced after crash before watermark"
    );
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), None);

    let db_arc = Arc::new(db);
    let syncer = SvnToGitSync::new(
        SvnClient::new(&svn_url, "", ""),
        git_arc,
        db_arc.clone(),
        config,
    );
    let synced = syncer
        .sync()
        .await
        .expect("resume sync must succeed without duplicate commit");
    assert_eq!(
        synced, 0,
        "already-mapped revision must not be re-exported after crash"
    );
    assert_eq!(git_sha(&git_work), head_before);
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some("1")
    );
}

/// NULL-repo_id commit_map rows from a non-personal source must not advance the
/// personal svn_rev watermark or skip importing.
#[tokio::test]
async fn test_personal_svn_to_git_imports_despite_null_foreign_commit_map_row() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(
        &wc_path,
        "team.txt",
        "foreign null collision\n",
        "Foreign null rev",
    );

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(
        1,
        "ffffffffffffffffffffffffffffffffffffffff",
        "svn_to_git",
        "foreign",
        "Foreign <f@example.com>",
    )
    .unwrap();
    let now = Utc::now();
    db.insert_sync_record(&SyncRecord {
        id: "foreign-null-svn-to-git".into(),
        repo_id: Some("foreign-team".into()),
        svn_revision: Some(1),
        git_hash: Some("ffffffffffffffffffffffffffffffffffffffff".into()),
        direction: SyncDirection::SvnToGit,
        author: "foreign".into(),
        message: "team import".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .unwrap();
    assert!(!db.is_personal_svn_rev_synced(1).unwrap());

    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc, db_arc.clone(), config);
    let synced = syncer
        .sync()
        .await
        .expect("personal svn-to-git must import despite NULL foreign commit_map");
    assert_eq!(synced, 1, "revision 1 must be imported, not skipped");
    assert!(
        db_arc
            .list_commit_map(10)
            .unwrap()
            .iter()
            .any(|entry| entry.svn_rev == 1 && entry.direction == "svn_to_git"),
        "personal import must record commit_map for r1"
    );
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some("1")
    );
}

/// Foreign team commit_map rows must not cause personal SVN→Git to skip importing.
#[tokio::test]
async fn test_personal_svn_to_git_imports_despite_foreign_commit_map_row() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(
        &wc_path,
        "team.txt",
        "foreign collision\n",
        "Foreign team rev",
    );

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    db.insert_commit_map(
        1,
        "ffffffffffffffffffffffffffffffffffffffff",
        "svn_to_git",
        "foreign",
        "Foreign <f@example.com>",
    )
    .unwrap();
    db.conn()
        .execute(
            "UPDATE commit_map SET repo_id = 'foreign-team' WHERE svn_rev = 1",
            [],
        )
        .unwrap();
    assert!(!db.is_personal_svn_rev_synced(1).unwrap());

    let config = make_test_config(&svn_url, tmp.path());
    let svn_client = SvnClient::new(&svn_url, "", "");
    let git_arc = Arc::new(Mutex::new(git_client));
    let db_arc = Arc::new(db);

    let syncer = SvnToGitSync::new(svn_client, git_arc, db_arc.clone(), config);
    let synced = syncer
        .sync()
        .await
        .expect("personal svn-to-git must import");
    assert_eq!(synced, 1, "revision 1 must be imported, not skipped");
    assert!(
        db_arc
            .list_commit_map(10)
            .unwrap()
            .iter()
            .any(|entry| entry.svn_rev == 1 && entry.direction == "svn_to_git"),
        "personal import must record commit_map for r1"
    );
    assert_eq!(
        db_arc.get_watermark("svn_rev").unwrap().as_deref(),
        Some("1")
    );
}

/// Deferred PR sync must abandon pending pr_sync_log and retry after the journal finalizes.
#[tokio::test]
async fn test_personal_git_to_svn_retries_pr_after_journal_defer() {
    use reposync_core::config::GitProvider;
    use reposync_core::db::git_push_operations::{git_push_target_fingerprint, GitPushIntent};
    use reposync_core::git::github::GitHubClient;

    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let svn_wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "seed.txt", "seed\n", "SVN seed");
    let svn_before = svn_youngest(&svn_url);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let imported_base = git_sha(&git_work);
    std::fs::write(git_work.join("feature.txt"), "from pr\n").unwrap();
    let oid = git_client
        .commit(
            "Add feature.txt",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let pr_commit_sha = oid.to_string();
    git_client.push("origin", "main").unwrap();
    let merge_sha = git_sha(&git_work);

    let db_path = tmp.path().join("test.db");
    let db_arc = Arc::new(setup_db(&db_path));
    seed_personal_svn_import_checkpoint(&db_arc, &imported_base, svn_before);

    let fingerprint = git_push_target_fingerprint(PERSONAL_SCOPE_KEY, "origin", "main");
    db_arc
        .begin_svn_to_git_push(GitPushIntent {
            repo_id: PERSONAL_SCOPE_KEY,
            initiator_id: "test",
            request_id: "defer-pr",
            target_fingerprint: &fingerprint,
            source_svn_rev: svn_before,
            source_svn_author: "testuser",
            source_svn_message: "pending journal",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: &imported_base,
            pre_push_git_tree: None,
            intended_local_git_sha: &pr_commit_sha,
            intended_local_git_parent: Some(&imported_base),
            intended_local_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
        })
        .expect("seed running svn-to-git journal");

    let (api_url, stub) = spawn_github_pr_sync_stub(&merge_sha, &pr_commit_sha);
    let mut config = make_test_config(&svn_url, tmp.path());
    config.github.api_url = api_url;
    config.github.token = Some("test-token".into());

    let sync = GitToSvnSync::new(
        SvnClient::new(&svn_url, "", ""),
        GitHubClient::new(&config.github.api_url, "test-token", GitProvider::GitHub),
        db_arc.clone(),
        &config,
        svn_wc.clone(),
        git_work.clone(),
    );

    use reposync_core::git::github::{PullRequest, PullRequestRef};
    let pr = PullRequest {
        number: 1,
        title: "Feature PR".into(),
        html_url: "https://example.invalid/pr/1".into(),
        state: "closed".into(),
        head: PullRequestRef {
            ref_name: "feature".into(),
            sha: pr_commit_sha.clone(),
        },
        base: PullRequestRef {
            ref_name: "main".into(),
            sha: imported_base.clone(),
        },
        merged: Some(true),
        merge_commit_sha: Some(merge_sha.clone()),
        merged_at: Some("2025-01-02T00:00:00Z".into()),
    };

    let defer_err = sync
        .sync_pr_for_test(&pr, &merge_sha)
        .await
        .expect_err("running svn-to-git journal must defer PR replay");
    assert!(
        format!("{defer_err:#}").contains("deferring git-to-svn replay"),
        "{defer_err:#}"
    );
    assert!(!db_arc.is_personal_pr_synced(&merge_sha).unwrap());
    let pending: i64 = db_arc
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pr_sync_log WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "defer must abandon pending pr_sync_log row");

    db_arc
        .conn()
        .execute(
            "DELETE FROM kv_state WHERE key LIKE 'svn_to_git_push_v1:active:%'",
            [],
        )
        .unwrap();

    let replayed = sync
        .sync_pr_for_test(&pr, &merge_sha)
        .await
        .expect("PR must replay once journal is finalized");
    assert_eq!(replayed, 1, "one commit must replay to SVN");
    assert!(db_arc.is_personal_pr_synced(&merge_sha).unwrap());
    assert!(
        svn_youngest(&svn_url) > svn_before,
        "SVN must advance after successful PR replay"
    );
    drop(stub);
}

/// Personal scope key must not collide with a managed repository id `personal`.
#[tokio::test]
async fn test_personal_scope_key_isolated_from_managed_personal_repo_id() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);
    svn_commit_file(&wc_path, "team.txt", "managed team\n", "Managed team rev");

    let git_work_dir = tmp.path().join("git_work");
    let bare_dir = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work_dir, &bare_dir);

    let db_path = tmp.path().join("test.db");
    let db = setup_db(&db_path);
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('personal','Managed Personal','file:///managed','','','local','','managed/repo','main','team',5,0,0,1,'t','t',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','idle',0,0)",
            [],
        )
        .unwrap();
    let now = Utc::now();
    db.insert_sync_record(&SyncRecord {
        id: "managed-team-receipt".into(),
        repo_id: Some(LEGACY_PERSONAL_REPO_ID.to_string()),
        svn_revision: Some(1),
        git_hash: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
        direction: SyncDirection::SvnToGit,
        author: "team".into(),
        message: "managed team receipt".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .unwrap();
    assert!(db
        .has_personal_svn_to_git_receipt(LEGACY_PERSONAL_REPO_ID, 1)
        .unwrap());
    assert!(!db
        .has_personal_svn_to_git_receipt(PERSONAL_SCOPE_KEY, 1)
        .unwrap());

    let config = make_test_config(&svn_url, tmp.path());
    let syncer = SvnToGitSync::new(
        SvnClient::new(&svn_url, "", ""),
        Arc::new(Mutex::new(git_client)),
        Arc::new(db),
        config,
    );
    let synced = syncer
        .sync()
        .await
        .expect("personal must not treat team receipt as synced");
    assert_eq!(
        synced, 1,
        "personal must import r1 despite managed repo id collision"
    );
}

/// Legacy personal receipts under repo id `personal` remain visible after scope-key upgrade.
#[tokio::test]
async fn test_personal_legacy_receipt_upgrade_path() {
    use reposync_core::echo_suppression::{
        classify_incoming_svn_revision_personal, EchoDisposition, TeamEchoContext,
    };

    let db = Database::in_memory().expect("in-memory db");
    db.initialize().expect("schema");
    let now = Utc::now();
    db.insert_sync_record(&SyncRecord {
        id: "legacy-git-to-svn-receipt".into(),
        repo_id: Some(LEGACY_PERSONAL_REPO_ID.to_string()),
        svn_revision: Some(4),
        git_hash: Some("cccccccccccccccccccccccccccccccccccccccc".into()),
        direction: SyncDirection::GitToSvn,
        author: "dev".into(),
        message: "legacy git-to-svn".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .unwrap();

    let ctx = TeamEchoContext {
        db: &db,
        repo_id: PERSONAL_SCOPE_KEY,
        no_target_projection: "{}",
    };
    assert_eq!(
        classify_incoming_svn_revision_personal(&ctx, 4, "no marker").unwrap(),
        EchoDisposition::SkipEcho
    );
}
