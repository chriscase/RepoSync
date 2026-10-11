use reposync_core::db::{personal_scope::PERSONAL_SCOPE_KEY, Database};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const TOKEN: &str = "disposable-personal-http-fixture-token";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Promoted from the fresh-import reproduction: a real authenticated smart-HTTP
// origin, a usable config token, and an explicit Personal-scope revocation.
// Exercise the shipped entry points so their clone/init fallback is covered.
pub fn assert_fresh_clone_revocation(binary: &str, prefix: &[&str], command: &str) {
    let tmp = TempDir::new().unwrap();
    let owner = tmp.path().join("owner");
    std::fs::create_dir(&owner).unwrap();
    let bare = owner.join("repo.git");
    git(tmp.path(), &["init", "--bare", bare.to_str().unwrap()]);
    let seed = tmp.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("seed.txt"), "seed\n").unwrap();
    git(&seed, &["add", "seed.txt"]);
    git(&seed, &["commit", "-m", "seed"]);
    git(&seed, &["push", bare.to_str().unwrap(), "main"]);
    git(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&bare, &["config", "http.receivepack", "true"]);
    let original_head = git(&bare, &["rev-parse", "main"]);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/git_http_auth_server.py");
    let mut server = ChildGuard(
        Command::new("python3")
            .arg(script)
            .env("GIT_PROJECT_ROOT", tmp.path())
            .env("GIT_HTTP_PORT", port.to_string())
            .env("GIT_TEST_TOKEN", TOKEN)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = server.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    assert!(rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .contains("listening"));

    // The allowed control proves this origin/token can clone. A managed token
    // must also take precedence over a stale config token. Each case is fresh.
    for (case, managed_token, config_token) in [
        ("config", None, TOKEN),
        ("revoked", Some(""), TOKEN),
        ("managed", Some(TOKEN), "stale-config-token"),
    ] {
        let data = tmp.path().join(case);
        std::fs::create_dir(&data).unwrap();
        let db = Database::new(data.join("personal.db")).unwrap();
        db.initialize().unwrap();
        if let Some(value) = managed_token {
            db.set_state(&format!("secret_git_token_{PERSONAL_SCOPE_KEY}"), value)
                .unwrap();
        }
        let config = tmp.path().join(format!("{case}.toml"));
        // The API/SVN endpoints deliberately fail after startup. Only Git
        // clone is under test; no real service or user credential is involved.
        std::fs::write(
            &config,
            format!(
                r#"
[personal]
data_dir = {data:?}
[svn]
url = "file:///reposync-fixture-does-not-exist"
username = "fixture"
password_env = "REPOSYNC_FRESH_CLONE_SVN_PASSWORD"
[github]
api_url = "http://127.0.0.1:{port}/api"
git_base_url = "http://127.0.0.1:{port}"
repo = "owner/repo"
token_env = "REPOSYNC_FRESH_CLONE_TOKEN"
auto_create = false
[developer]
name = "Fixture"
email = "fixture@example.invalid"
svn_username = "fixture"
"#
            ),
        )
        .unwrap();
        let stdout = std::fs::File::create(tmp.path().join(format!("{case}.out"))).unwrap();
        let stderr_path = tmp.path().join(format!("{case}.err"));
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let mut child = ChildGuard(
            Command::new(binary)
                .args(prefix)
                .arg(&config)
                .arg(command)
                .env("REPOSYNC_FRESH_CLONE_TOKEN", config_token)
                .env("REPOSYNC_FRESH_CLONE_SVN_PASSWORD", "fixture-only")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdout(stdout)
                .stderr(stderr)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "{case}: startup did not finish");
            std::thread::sleep(Duration::from_millis(20));
        };
        let stderr = std::fs::read_to_string(stderr_path).unwrap();
        let checkout = data.join("git-repo");
        if case == "revoked" {
            assert!(!status.success(), "revoked startup succeeded");
            assert!(
                stderr.contains("git credentials explicitly revoked"),
                "wrong refusal: {stderr}"
            );
            assert!(
                !checkout.exists(),
                "revoked startup cloned or fell back to init"
            );
        } else {
            assert_eq!(
                git(&checkout, &["rev-parse", "HEAD"]),
                original_head,
                "{case}: {stderr}"
            );
            assert_eq!(
                git(&checkout, &["remote", "get-url", "origin"]),
                format!("http://127.0.0.1:{port}/owner/repo.git")
            );
        }
        assert_eq!(
            git(&bare, &["rev-parse", "main"]),
            original_head,
            "startup changed the remote"
        );
        assert!(
            !stderr.contains(config_token),
            "startup disclosed the config token"
        );
    }
}
