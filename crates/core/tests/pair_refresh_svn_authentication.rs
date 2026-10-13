//! Refresh authentication uses disposable SVN servers and explicit child envs.
//! No global environment mutation or real credential store is used.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;
#[path = "fixtures/owned_process_group.rs"]
mod owned_process_group;

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn command(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn cached_files(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                files.push((
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(entry.path()).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn ambient_authentication_cannot_supply_missing_or_incorrect_refresh_passwords() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    command(Command::new("svnadmin").arg("create").arg(&repo));
    let file_url = reqwest::Url::from_directory_path(&repo).unwrap();
    command(Command::new("svn").args([
        "mkdir",
        &format!("{}trunk", file_url),
        &format!("{}branches", file_url),
        &format!("{}branches/f", file_url),
        "-m",
        "disposable layout",
        "--non-interactive",
    ]));
    let realm = format!("refresh-disposable-{}", uuid::Uuid::new_v4());
    std::fs::write(
        repo.join("conf/svnserve.conf"),
        format!("[general]\nanon-access = none\nauth-access = write\npassword-db = passwd\nrealm = {realm}\n"),
    )
    .unwrap();
    std::fs::write(
        repo.join("conf/passwd"),
        "[users]\nfixture = disposable-refresh-password\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut server = Command::new("svnserve");
    server
        .args([
            "-d",
            "--foreground",
            "--listen-host",
            "127.0.0.1",
            "--listen-port",
            &port.to_string(),
            "-r",
        ])
        .arg(temp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut server = owned_process_group::OwnedProcessGroup::spawn(&mut server).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(server.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let url = format!("svn://127.0.0.1:{port}/repo");
    let home = temp.path().join("daemon-home");
    let config = home.join(".subversion");
    std::fs::create_dir_all(config.join("auth/svn.simple")).unwrap();
    std::fs::write(config.join("config"), "[auth]\npassword-stores =\n").unwrap();
    std::fs::write(config.join("servers"), "[global]\n").unwrap();
    // Existing plaintext caches are readable even by builds which disable new
    // plaintext storage. Create only this disposable server's canonical cache.
    command(Command::new("python3").args([
        "-c",
        "import hashlib,pathlib,sys; realm=sys.argv[2]; values={'passtype':'simple','password':'disposable-refresh-password','svn:realmstring':realm,'username':'fixture'}; data=''.join(f'K {len(k)}\\n{k}\\nV {len(v)}\\n{v}\\n' for k,v in values.items())+'END\\n'; pathlib.Path(sys.argv[1],hashlib.md5(realm.encode()).hexdigest()).write_text(data)",
        config.join("auth/svn.simple").to_str().unwrap(),
        &format!("<svn://127.0.0.1:{port}> {realm}"),
    ]));
    // Counterfactual: the ordinary CLI can authenticate using that cache.
    command(
        Command::new("svn")
            .args([
                "info",
                "--xml",
                &format!("{url}/trunk"),
                "--non-interactive",
                "--no-auth-cache",
            ])
            .env("HOME", &home),
    );
    let before = cached_files(&config);
    for mode in ["anonymous", "wrong", "correct"] {
        command(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "isolated_client_child", "--nocapture"])
                .env("HOME", &home)
                .env("REPOSYNC_REFRESH_AUTH_TEST_URL", &url)
                .env("REPOSYNC_REFRESH_AUTH_TEST_MODE", mode),
        );
        assert_eq!(cached_files(&config), before);
    }
}

#[tokio::test]
async fn isolated_client_child() {
    let Ok(url) = std::env::var("REPOSYNC_REFRESH_AUTH_TEST_URL") else {
        return;
    };
    let mode = std::env::var("REPOSYNC_REFRESH_AUTH_TEST_MODE").unwrap();
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_url=?1,svn_branch=CASE WHEN id='child' THEN 'branches/f' ELSE 'trunk' END,svn_username=?2", [url.as_str(), if mode == "anonymous" { "" } else { "fixture" }]).unwrap();
    if mode != "anonymous" {
        f.db.set_state(
            "secret_svn_password",
            if mode == "correct" {
                "disposable-refresh-password"
            } else {
                "disposable-wrong-password"
            },
        )
        .unwrap();
    }
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let (child, parent) = configuration.isolated_svn_clients().unwrap();
    let child_clone = child.clone();
    drop(child);
    drop(configuration);
    for client in [child_clone, parent] {
        let result = client.info().await;
        assert_eq!(result.is_ok(), mode == "correct", "{mode}: {result:?}");
        let cwd = tempfile::tempdir().unwrap();
        let result = client
            .run_svn_in_dir_public(cwd.path(), &["info", "--xml", client.url()])
            .await;
        assert_eq!(result.is_ok(), mode == "correct", "{mode}: {result:?}");
        if let Err(error) = result {
            assert!(!error.to_string().contains("disposable-wrong-password"));
        }
    }
}

#[test]
fn incomplete_svn_credentials_refuse_before_client_use() {
    for (username, password) in [
        ("fixture", None),
        ("", Some("disposable-password")),
        ("fixture", Some("")),
    ] {
        let f = job::Fixture::new();
        f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_username=?1,svn_branch=CASE WHEN id='child' THEN 'branches/f' ELSE 'trunk' END", [username]).unwrap();
        if let Some(password) = password {
            f.db.set_state("secret_svn_password", password).unwrap();
        }
        match f.db.capture_pair_refresh_execution_configuration("child") {
            Ok(configuration) => assert!(configuration.isolated_svn_clients().is_err()),
            Err(error) => assert!(
                error
                    .to_string()
                    .contains("complete explicit SVN credentials")
                    || (password == Some("") && error.to_string().contains("revocation"))
            ),
        }
    }
}
