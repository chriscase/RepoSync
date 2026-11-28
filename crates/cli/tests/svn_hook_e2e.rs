//! #71: the hook `reposync svn-hook install` writes really notifies RepoSync
//! on a real SVN commit, with the secret read from a file (SVN runs hooks with
//! an empty environment) and kept off curl's command line.

use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Accept one HTTP request and return (head, body).
fn serve_one(listener: TcpListener, tx: mpsc::Sender<(String, String)>) {
    let (stream, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap();
        }
        head.push_str(&line);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    use std::io::Write;
    let mut stream = stream;
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    tx.send((head, String::from_utf8(body).unwrap())).unwrap();
}

fn commit_file(repo: &Path, work: &Path) {
    let url = format!("file://{}", repo.display());
    run(Command::new("svn").args(["checkout", "-q", &url]).arg(work));
    std::fs::write(work.join("hello.txt"), "hi\n").unwrap();
    run(Command::new("svn")
        .arg("add")
        .arg("-q")
        .arg(work.join("hello.txt")));
    run(Command::new("svn")
        .args(["commit", "-q", "-m", "first", "--username", "alice"])
        .arg(work));
}

#[test]
fn installed_hook_notifies_on_a_real_commit_with_the_secret_from_a_file() {
    if !(have("svnadmin") && have("svn") && have("curl")) {
        assert!(
            std::env::var_os("CI").is_none(),
            "svnadmin, svn and curl are required in CI"
        );
        eprintln!("skipping: svnadmin, svn or curl not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    run(Command::new("svnadmin").arg("create").arg(&repo));
    let secret = tmp.path().join("svn-webhook-secret");
    std::fs::write(&secret, "file\"secret\\71\r\n").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve_one(listener, tx));

    let out = run(Command::new(env!("CARGO_BIN_EXE_reposync"))
        .args(["svn-hook", "install", "--repository"])
        .arg(&repo)
        .args(["--server-url", &format!("http://127.0.0.1:{port}")])
        .arg("--secret-file")
        .arg(&secret));
    assert!(out.contains("post-commit"), "{out}");

    commit_file(&repo, &tmp.path().join("wc"));

    let (head, body) = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the post-commit hook did not notify RepoSync");
    assert!(head.starts_with("POST /webhook/svn "), "{head}");
    assert!(
        head.contains("X-Webhook-Secret: file\"secret\\71\r\n"),
        "{head}"
    );
    let cksum = run(Command::new("sh")
        .arg("-c")
        .arg("printf '%s' \"$1\" | cksum | cut -d' ' -f1")
        .arg("sh")
        .arg(&repo));
    assert!(
        head.contains(&format!("X-Webhook-Delivery: svn-{}-1\r\n", cksum.trim())),
        "{head}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json, serde_json::json!({"revision": 1}), "{body}");

    // Removing it takes both files away again.
    run(Command::new(env!("CARGO_BIN_EXE_reposync"))
        .args(["svn-hook", "remove", "--repository"])
        .arg(&repo));
    assert!(!repo.join("hooks/post-commit").exists());
    assert!(!repo.join("hooks/reposync-post-commit").exists());
}

#[test]
fn an_unresponsive_server_does_not_delay_the_commit() {
    if !(have("svnadmin") && have("svn") && have("curl")) {
        assert!(
            std::env::var_os("CI").is_none(),
            "svnadmin, svn and curl are required in CI"
        );
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    run(Command::new("svnadmin").arg("create").arg(&repo));
    // Accepts connections (backlog) but never answers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    run(Command::new(env!("CARGO_BIN_EXE_reposync"))
        .args(["svn-hook", "install", "--repository"])
        .arg(&repo)
        .args(["--server-url", &format!("http://127.0.0.1:{port}")]));
    let started = std::time::Instant::now();
    commit_file(&repo, &tmp.path().join("wc"));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "checkout + commit took {:?}; the hook must not wait for the server",
        started.elapsed()
    );
    drop(listener);
}
