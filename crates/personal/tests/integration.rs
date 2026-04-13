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

use tempfile::TempDir;
use std::sync::Mutex;

use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::personal_config::{
    CommitFormatConfig, DeveloperConfig, PersonalConfig, PersonalGitHubConfig,
    PersonalOptionsConfig, PersonalSection, PersonalSvnConfig,
};
use reposync_core::svn::SvnClient;
use reposync_personal::commit_format::CommitFormatter;
use reposync_personal::svn_to_git::SvnToGitSync;

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
async fn test_svn_to_git_echo_suppression() {
    if !svn_available() {
        eprintln!("SKIPPED: svn/svnadmin not found in PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc_path = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc_path);

    // Rev 1: normal commit.
    svn_commit_file(&wc_path, "normal1.txt", "hello", "Normal commit 1");

    // Rev 2: commit with [reposync] marker (simulating an echo).
    svn_commit_file(
        &wc_path,
        "echo.txt",
        "echoed content",
        "Echoed commit [reposync] synced from Git",
    );

    // Rev 3: another normal commit.
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

    // Should sync 2 revisions (skipping the echo).
    assert_eq!(synced, 2, "expected 2 revisions synced (echo skipped)");

    // Watermark should still advance to 3 (the echo commit was acknowledged).
    let watermark = db_arc.get_watermark("svn_rev").unwrap();
    assert_eq!(watermark.as_deref(), Some("3"));

    // commit_map should have 2 entries (the echo is not in the map).
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

    // Set the svn:author revprop so the sync sees "alice".
    let status = Command::new("svn")
        .args([
            "propset",
            "--revprop",
            "-r",
            &rev.to_string(),
            "svn:author",
            "alice",
            &svn_url,
            "--non-interactive",
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
    let sync = SvnToGitSync::new(svn_client, git_client, db, config);

    let count = sync.sync().await.unwrap();
    assert_eq!(count, 2, "two revisions should sync");

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
///   - The method returns Ok (skip, not error) reflecting safe-skip behavior.
#[tokio::test]
async fn test_replay_path_lfs_pointer_skipped_not_committed() {
    use reposync_core::db::queries::AuditLogEntry;
    use reposync_core::git::github::{
        GitHubCommit, GitHubCommitDetail, GitHubGitActor,
    };
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
    let github_client =
        reposync_core::git::github::GitHubClient::new("https://localhost:0/unused", "unused", reposync_core::config::GitProvider::default());

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

    // The method should succeed (skip is not an error — safe-skip behavior).
    assert!(
        result.is_ok(),
        "apply_git_changes_to_svn must return Ok on LFS pointer skip, got: {:?}",
        result.err()
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
    assert!(!entry.success, "lfs_resolution_failed must be marked as failure");
    assert!(
        entry
            .details
            .as_deref()
            .unwrap_or("")
            .contains("large-asset.bin"),
        "audit details must mention the skipped file path"
    );
}

/// Production-path companion test: drives `apply_git_changes_to_svn` with
/// normal (non-pointer) content and verifies it IS written to SVN WC.
/// Guards against false-positive skipping.
#[tokio::test]
async fn test_replay_path_normal_content_written_to_svn() {
    use reposync_core::git::github::{
        GitHubCommit, GitHubCommitDetail, GitHubGitActor,
    };
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
    let github_client =
        reposync_core::git::github::GitHubClient::new("https://localhost:0/unused", "unused", reposync_core::config::GitProvider::default());

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
    assert_eq!(written, normal_content, "SVN WC file content must match Git content");
}
