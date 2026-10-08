//! Personal-mode ordinary sync against authenticated loopback svnserve (issue #62 / R01).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use reposync_core::config::GitProvider;
use reposync_core::db::Database;
use reposync_core::git::github::GitHubClient;
use reposync_core::git::GitClient;
use reposync_core::history_inspect::inspect_personal_history;
use reposync_core::personal_config::{
    CommitFormatConfig, DeveloperConfig, PersonalConfig, PersonalGitHubConfig,
    PersonalOptionsConfig, PersonalSection, PersonalSvnConfig,
};
use reposync_core::svn::SvnClient;
use reposync_personal::commit_format::CommitFormatter;
use reposync_personal::engine::PersonalSyncEngine;
use reposync_personal::git_to_svn::GitToSvnSync;
use reposync_personal::initial_import::{ImportMode, InitialImport};
use tempfile::TempDir;

const CASE_PERSONAL_ROUNDTRIP: &str = "R01_SVNSERVE_PERSONAL_ROUNDTRIP";

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

fn write_svnserve_auth(repo_dir: &Path, username: &str, password: &str) {
    let conf_dir = repo_dir.join("conf");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("svnserve.conf"),
        "[general]\n\
anon-access = none\n\
auth-access = write\n\
password-db = passwd\n\
realm = test\n",
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

fn wait_for_svnserve(port: u16) {
    for attempt in 0..40 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25 * (attempt as u64 + 1)));
    }
    panic!("svnserve did not accept connections on port {port}");
}

struct SvnserveDaemon {
    child: Child,
    port: u16,
}

impl SvnserveDaemon {
    fn start(root: &Path) -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
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
            .unwrap();
        wait_for_svnserve(port);
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
        .unwrap();
    assert!(status.success(), "svn checkout failed for {url}");
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
    if String::from_utf8_lossy(&status_output.stdout).contains('?') {
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
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Committed revision ")
                .map(|rev| rev.trim_end_matches('.').parse::<i64>().unwrap())
        })
        .expect("committed revision")
}

fn setup_db(path: &Path) -> Database {
    let db = Database::new(path).unwrap();
    db.initialize().unwrap();
    db
}

fn make_config(
    svn_url: &str,
    username: &str,
    password: &str,
    data_dir: &Path,
    api_url: &str,
) -> PersonalConfig {
    PersonalConfig {
        personal: PersonalSection {
            poll_interval_secs: 5,
            log_level: "debug".into(),
            data_dir: data_dir.to_path_buf(),
            status_port: None,
        },
        svn: PersonalSvnConfig {
            url: svn_url.into(),
            username: username.into(),
            password_env: "REPOSYNC_TEST_SVN_PW".into(),
            password: Some(password.into()),
        },
        github: PersonalGitHubConfig {
            api_url: api_url.into(),
            git_base_url: None,
            repo: "test/test-repo".into(),
            token_env: "REPOSYNC_TEST_GH_TOKEN".into(),
            default_branch: "main".into(),
            auto_create: false,
            private: true,
            token: Some("fixture-token".into()),
        },
        developer: DeveloperConfig {
            name: "Test User".into(),
            email: "test@example.com".into(),
            svn_username: username.into(),
        },
        commit_format: CommitFormatConfig::default(),
        options: PersonalOptionsConfig::default(),
        identity: None,
    }
}

fn spawn_github_exists_stub() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{}", port), handle)
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

fn git_head_sha(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r01_svnserve_personal_roundtrip() {
    if let Some(missing) = toolchain_available() {
        emit_evidence(
            CASE_PERSONAL_ROUNDTRIP,
            "NOT RUN",
            serde_json::json!({"reason": "missing_tool", "tool": missing}),
        );
        return;
    }

    let tmp = TempDir::new().unwrap();
    let svn_root = tmp.path().join("svnserve-root");
    std::fs::create_dir_all(&svn_root).unwrap();
    let daemon = SvnserveDaemon::start(&svn_root);
    let user = "personal";
    let pass = "personal-svnserve-secret";
    let repo_dir = svn_root.join("personal_repo");
    assert!(Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    write_svnserve_auth(&repo_dir, user, pass);
    let svn_url = daemon.repo_url("personal_repo");
    let wc = tmp.path().join("wc");
    svn_checkout(&svn_url, &wc, user, pass);
    svn_commit_file(&wc, "seed.txt", "personal seed\n", "Seed", user, pass);
    let svn_after_seed = svn_commit_file(&wc, ".gitkeep", "", "Anchor", user, pass);

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    let git_client = GitClient::init(&git_work).unwrap();
    {
        let repo = git2::Repository::open(&git_work).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
    }
    Command::new("git")
        .args(["-C", git_work.to_str().unwrap(), "checkout", "-b", "main"])
        .status()
        .unwrap();
    let git_client = Arc::new(std::sync::Mutex::new(git_client));

    let db_path = tmp.path().join("personal.db");
    let db = setup_db(&db_path);
    let (api_url, stub) = spawn_github_exists_stub();
    let config = make_config(&svn_url, user, pass, tmp.path(), &api_url);
    let formatter = CommitFormatter::new(&config.commit_format);
    let importer = InitialImport {
        svn_client: &SvnClient::new(&svn_url, user, pass),
        git_client: &git_client,
        github_client: &GitHubClient::new(&api_url, "fixture-token", GitProvider::GitHub),
        db: &db,
        config: &config,
        formatter: &formatter,
    };
    importer
        .import(ImportMode::Snapshot)
        .await
        .expect("snapshot import");
    drop(stub);

    inspect_personal_history(&db, &git_work, "main", "personal")
        .expect("inspect")
        .expect("admitted");

    let engine = PersonalSyncEngine::new(
        config.clone(),
        Database::new(&db_path).unwrap(),
        SvnClient::new(&svn_url, user, pass),
        GitClient::new(&git_work).unwrap(),
        GitHubClient::new(&api_url, "fixture-token", GitProvider::GitHub),
    );
    engine.run_cycle().await.expect("post-import cycle");

    svn_commit_file(
        &wc,
        "from_svn_personal.txt",
        "svn leg\n",
        "SVN personal delta",
        user,
        pass,
    );
    let svn_stats = engine.run_cycle().await.expect("SVN→Git personal cycle");
    assert_eq!(svn_stats.svn_to_git_count, 1);

    std::fs::write(git_work.join("from_git_personal.txt"), "git leg\n").unwrap();
    let git_client = GitClient::new(&git_work).unwrap();
    let git_sha = git_client
        .commit(
            "Git personal delta",
            "Test User",
            "test@example.com",
            "Test User",
            "test@example.com",
        )
        .unwrap()
        .to_string();
    git_client.push("origin", "main").unwrap();

    let svn_wc = tmp.path().join("svn-wc");
    svn_checkout(&svn_url, &svn_wc, user, pass);
    let db_arc = Arc::new(Database::new(&db_path).unwrap());
    let syncer = GitToSvnSync::new(
        SvnClient::new(&svn_url, user, pass),
        GitHubClient::new(&api_url, "fixture-token", GitProvider::GitHub),
        db_arc.clone(),
        &config,
        svn_wc,
        git_work.clone(),
    );
    let svn_rev = syncer
        .replay_commit(
            &github_commit(git_sha.clone(), "Git personal delta"),
            9,
            "feature/personal",
        )
        .await
        .expect("git→svn replay");
    assert!(svn_rev > svn_after_seed);
    assert!(db_arc.is_personal_git_sha_synced(&git_sha).unwrap());

    emit_evidence(
        CASE_PERSONAL_ROUNDTRIP,
        "PASS",
        serde_json::json!({
            "mode": "personal",
            "svn_url_scheme": "svn",
            "svn_to_git_count": svn_stats.svn_to_git_count,
            "git_to_svn_rev": svn_rev,
            "git_sha": git_sha,
            "git_head_after": git_head_sha(&git_work),
        }),
    );
}
