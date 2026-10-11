use reposync_core::db::{personal_scope::PERSONAL_SCOPE_KEY, Database};
use reposync_core::git::GitClient;
use reposync_core::history_inspect::{inspect_fetched_history, inspect_personal_history};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

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

struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn authenticated_personal_clone_must_remain_usable_by_history_gate() {
    const TOKEN: &str = "disposable-personal-http-fixture-token";
    let tmp = TempDir::new().unwrap();
    let bare = tmp.path().join("repo.git");
    git(tmp.path(), &["init", "--bare", bare.to_str().unwrap()]);
    let seed = tmp.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("seed.txt"), "seed\n").unwrap();
    git(&seed, &["add", "seed.txt"]);
    git(&seed, &["commit", "-m", "seed"]);
    git(&seed, &["push", bare.to_str().unwrap(), "main"]);
    git(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let checkpoint = git(&seed, &["rev-parse", "HEAD"]);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/git_http_auth_server.py");
    let mut server = ServerGuard(
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

    // This is the personal startup clone path, with a real authenticated HTTP origin.
    let url = format!("http://127.0.0.1:{port}/repo.git");
    let bridge = tmp.path().join("bridge");
    let client = GitClient::clone_repo(&url, &bridge, Some(TOKEN))
        .expect("authenticated personal startup clone must succeed");
    assert_eq!(client.stored_http_auth_token().as_deref(), Some(TOKEN));
    assert_eq!(git(&bridge, &["remote", "get-url", "origin"]), url);
    // Ensure any host credential helper cannot make the anonymous call succeed.
    git(&bridge, &["config", "credential.helper", ""]);
    inspect_fetched_history(&bridge, "main", Some(checkpoint.clone()), Some(TOKEN))
        .expect("control: authenticated history inspection must succeed");

    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db.set_watermark("git_sha", &checkpoint).unwrap();
    let result = inspect_personal_history(&db, &bridge, "main", PERSONAL_SCOPE_KEY);
    assert!(
        result.is_ok(),
        "personal history gate lost the valid startup credential: {result:?}"
    );
}
