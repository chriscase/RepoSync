//! A failing LFS clean filter must not be recorded as an unchanged revision.
//!
//! This is its own test process so the temporary `git-lfs` wrapper on `PATH`
//! cannot change clean-filter behavior for the other personal tests.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::personal_config::{
    CommitFormatConfig, DeveloperConfig, PersonalConfig, PersonalGitHubConfig,
    PersonalOptionsConfig, PersonalSection, PersonalSvnConfig,
};
use reposync_core::svn::SvnClient;
use reposync_personal::svn_to_git::SvnToGitSync;

#[test]
fn failing_clean_filter_does_not_advance_the_watermark() {
    if !svn_available() {
        eprintln!("SKIP: svn/svnadmin not available");
        return;
    }
    let real_lfs = String::from_utf8(
        Command::new("which")
            .arg("git-lfs")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let real_lfs = real_lfs.trim();
    if real_lfs.is_empty() {
        eprintln!("SKIP: git-lfs not available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let svn_url = create_svn_repo(dir.path());
    let svn_wc = dir.path().join("svn_wc");
    svn_checkout(&svn_url, &svn_wc);
    svn_commit_file(&svn_wc, "asset.bin", &"Q".repeat(80), "add asset");

    let git_work = dir.path().join("git_work");
    let git_bare = dir.path().join("git_bare");
    let git_client = setup_git_with_bare_origin(&git_work, &git_bare);
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Arc::new(setup_db(&data_dir.join("test.db")));
    let mut config = make_test_config(&svn_url, &data_dir);
    config.options.lfs_threshold = 40;
    let sync = SvnToGitSync::new(
        SvnClient::new(&svn_url, "", ""),
        Arc::new(Mutex::new(git_client)),
        db.clone(),
        config,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(runtime.block_on(sync.sync()).unwrap(), 1);
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("1"));
    assert!(db.is_personal_svn_rev_synced(1).unwrap());

    svn_commit_file(&svn_wc, "asset.bin", &"Z".repeat(90), "replace asset");
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git-lfs");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"clean\" ] || [ \"$1\" = \"filter-process\" ]; then\n  echo \"nothing to commit\" >&2\n  exit 1\nfi\nexec {real_lfs} \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
    let failed = runtime.block_on(sync.sync());
    std::env::set_var("PATH", old_path);

    let error = failed.expect_err("a failing clean filter must not succeed");
    let message = format!("{error:#}");
    assert!(
        message.contains("git add") || message.contains("nothing to commit"),
        "{message}"
    );
    assert_eq!(
        db.get_watermark("svn_rev").unwrap().as_deref(),
        Some("1"),
        "failed filter must not advance the watermark"
    );
    assert!(
        !db.is_personal_svn_rev_synced(2).unwrap(),
        "failed revision must not gain a receipt"
    );
}

fn svn_available() -> bool {
    Command::new("svn")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
        && Command::new("svnadmin")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

fn create_svn_repo(dir: &Path) -> String {
    let repo_dir = dir.join("svn_repo");
    assert!(Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    format!("file://{}", repo_dir.display())
}

fn svn_checkout(url: &str, wc_path: &Path) {
    assert!(Command::new("svn")
        .args([
            "checkout",
            url,
            wc_path.to_str().unwrap(),
            "--non-interactive"
        ])
        .status()
        .unwrap()
        .success());
}

fn svn_commit_file(wc_path: &Path, filename: &str, content: &str, message: &str) {
    let file_path = wc_path.join(filename);
    let existed = file_path.exists();
    std::fs::write(&file_path, content).unwrap();
    if !existed {
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
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svn commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn setup_git_with_bare_origin(work_dir: &Path, bare_dir: &Path) -> GitClient {
    git2::Repository::init_bare(bare_dir).unwrap();
    let git_client = GitClient::init(work_dir).unwrap();
    git2::Repository::open(work_dir)
        .unwrap()
        .remote("origin", bare_dir.to_str().unwrap())
        .unwrap();
    std::fs::write(work_dir.join(".gitkeep"), "").unwrap();
    git_client
        .commit(
            "initial commit",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap();
    let repo = git2::Repository::open(work_dir).unwrap();
    let head = repo.head().unwrap();
    let head_name = head.name().unwrap_or("");
    if head_name != "refs/heads/main" {
        repo.find_branch(
            head_name.strip_prefix("refs/heads/").unwrap_or("master"),
            git2::BranchType::Local,
        )
        .unwrap()
        .rename("main", true)
        .unwrap();
    }
    git_client.push("origin", "main").unwrap();
    git_client
}

fn setup_db(path: &Path) -> Database {
    let db = Database::new(path).unwrap();
    db.initialize().unwrap();
    db
}

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
