//! Exercise startup admission through the shipped CLI, before storage or Git mutation.
use reposync_core::db::Database;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn config(root: &Path, data: &Path) -> PathBuf {
    let path = root.join("personal.toml");
    std::fs::write(
        &path,
        format!(
            r#"
[personal]
data_dir = {data:?}
poll_interval_secs = 60
[svn]
url = "file:///reposync-startup-fixture-does-not-exist"
username = "fixture"
password_env = "REPOSYNC_STARTUP_SVN_PASSWORD"
[github]
api_url = "http://127.0.0.1:1/api"
git_base_url = "http://127.0.0.1:1"
repo = "fixture/repo"
token_env = "REPOSYNC_STARTUP_GIT_TOKEN"
auto_create = false
[developer]
name = "Startup Fixture"
email = "fixture@example.invalid"
svn_username = "fixture"
"#
        ),
    )
    .unwrap();
    path
}

fn spawn(root: &Path, config: &Path, arguments: &[&str]) -> Process {
    Process(
        Command::new(env!("CARGO_BIN_EXE_reposync"))
            .args(["personal", "--personal-config"])
            .arg(config)
            .args(arguments)
            .env(
                "REPOSYNC_STARTUP_SVN_PASSWORD",
                "disposable-fixture-password",
            )
            .env("REPOSYNC_STARTUP_GIT_TOKEN", "disposable-fixture-token")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(std::fs::File::create(root.join("stdout")).unwrap())
            .stderr(std::fs::File::create(root.join("stderr")).unwrap())
            .spawn()
            .unwrap(),
    )
}

fn wait(process: &mut Process, root: &Path) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "CLI did not exit: {}",
            std::fs::read_to_string(root.join("stderr")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn personal_writers_refuse_orphan_recovery_before_checkpoint_or_git_preparation() {
    for arguments in [
        vec!["import", "--full"],
        vec!["import", "--snapshot"],
        vec!["start", "--foreground"],
        vec![
            "conflicts",
            "resolve",
            "fixture-conflict",
            "--accept",
            "svn",
        ],
    ] {
        for suffix in ["document", "authority", "hold", "request"] {
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("data");
            std::fs::create_dir(&data).unwrap();
            let path = data.join("personal.db");
            let db = Database::new(&path).unwrap();
            db.initialize().unwrap();
            db.conn()
                .execute_batch("PRAGMA wal_autocheckpoint=0;")
                .unwrap();
            db.set_state("secret_git_token", "preserve-plaintext")
                .unwrap();
            db.set_state(
                &format!("pair_refresh_execution_v3:{suffix}:retained"),
                "preserve-unknown-recovery-evidence",
            )
            .unwrap();
            let main = std::fs::read(&path).unwrap();
            let wal_path = path.with_extension("db-wal");
            let wal = std::fs::read(&wal_path).unwrap();
            let config = config(root.path(), &data);
            let mut process = spawn(root.path(), &config, &arguments);
            let status = wait(&mut process, root.path());
            assert!(!status.success(), "{arguments:?}: {suffix}");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                main,
                "{arguments:?}: {suffix}"
            );
            assert_eq!(
                std::fs::read(&wal_path).unwrap(),
                wal,
                "{arguments:?}: {suffix}"
            );
            let stderr = std::fs::read_to_string(root.path().join("stderr")).unwrap();
            assert!(
                stderr.contains("pair refresh journal"),
                "{arguments:?}: {stderr}"
            );
            assert!(!data.join("git-repo").exists());
            assert!(!data.join("personal.pid").exists());
            assert!(db.get_state("encryption_key").unwrap().is_none());
            assert_eq!(
                db.get_state(&format!("pair_refresh_execution_v3:{suffix}:retained"))
                    .unwrap()
                    .as_deref(),
                Some("preserve-unknown-recovery-evidence")
            );
        }
    }
}

#[test]
fn fresh_personal_start_initializes_storage_and_restarts_after_joined_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    reposync_core::git::GitClient::init(data.join("git-repo")).unwrap();
    let config = config(root.path(), &data);
    assert!(!data.join("personal.db").exists());
    for _ in 0..2 {
        let mut process = spawn(root.path(), &config, &["start", "--foreground"]);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !data.join("personal.pid").exists() {
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "fresh startup exited: {}",
                std::fs::read_to_string(root.path().join("stderr")).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "personal start never became ready"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            std::fs::read_to_string(data.join("personal.pid")).unwrap(),
            process.0.id().to_string()
        );
        // Startup publishes the PID only after installing signal handlers.
        // The PID acknowledgment therefore makes shutdown deterministic.
        assert!(Command::new("kill")
            .args(["-TERM", &process.0.id().to_string()])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success());
        assert!(wait(&mut process, root.path()).success());
        assert!(!data.join("personal.pid").exists());
        let db = Database::new(data.join("personal.db")).unwrap();
        assert!(db.get_repository("personal").is_ok());
        assert!(db.list_commit_map(10).unwrap().is_empty());
        assert!(db.get_state("writer_fence_v1:lease").unwrap().is_none());
        assert!(db.get_state("writer_fence_v1:native").unwrap().is_none());
        assert!(!data.join("reposync.db").exists());
    }
}

fn command(directory: &Path, tool: &str, arguments: &[&str]) -> String {
    let output = Command::new(tool)
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{tool} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn fresh_full_import_records_two_real_revisions_and_repeat_keeps_published_history() {
    use std::io::{BufRead, BufReader};
    let root = tempfile::tempdir().unwrap();
    let svn = root.path().join("svn");
    command(root.path(), "svnadmin", &["create", svn.to_str().unwrap()]);
    let svn_url = format!("file://{}", svn.display());
    let seed = root.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    std::fs::write(seed.join("content.txt"), b"first revision\n").unwrap();
    command(
        root.path(),
        "svn",
        &[
            "import",
            seed.to_str().unwrap(),
            &svn_url,
            "-m",
            "first",
            "--username",
            "fixture",
            "--no-auth-cache",
            "--non-interactive",
        ],
    );
    let wc = root.path().join("wc");
    command(
        root.path(),
        "svn",
        &[
            "checkout",
            &svn_url,
            wc.to_str().unwrap(),
            "--no-auth-cache",
            "--non-interactive",
        ],
    );
    std::fs::write(wc.join("content.txt"), b"second revision\n").unwrap();
    command(
        &wc,
        "svn",
        &[
            "commit",
            "-m",
            "second",
            "--username",
            "fixture",
            "--no-auth-cache",
            "--non-interactive",
        ],
    );
    let owner = root.path().join("fixture");
    std::fs::create_dir(&owner).unwrap();
    let bare = owner.join("repo.git");
    command(
        root.path(),
        "git",
        &["init", "--bare", bare.to_str().unwrap()],
    );
    command(&bare, "git", &["symbolic-ref", "HEAD", "refs/heads/main"]);
    command(&bare, "git", &["config", "http.receivepack", "true"]);
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut provider = Process(
        Command::new("python3")
            .arg(manifest.join("tests/fixtures/startup_provider.py"))
            .arg(manifest.join("../core/tests/fixtures"))
            .env("GIT_PROJECT_ROOT", root.path())
            .env("GIT_TEST_TOKEN", "disposable-fixture-token")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = provider.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = send.send(line);
    });
    let port: u16 = receive
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let data = root.path().join("data");
    let config = config(root.path(), &data);
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("file:///reposync-startup-fixture-does-not-exist", &svn_url)
        .replace("http://127.0.0.1:1", &format!("http://127.0.0.1:{port}"));
    std::fs::write(&config, text).unwrap();
    assert!(!data.join("personal.db").exists());
    let mut first = spawn(root.path(), &config, &["import", "--full"]);
    assert!(
        wait(&mut first, root.path()).success(),
        "{}",
        std::fs::read_to_string(root.path().join("stderr")).unwrap()
    );
    let original = command(&bare, "git", &["rev-parse", "main"]);
    assert_eq!(command(&bare, "git", &["rev-list", "--count", "main"]), "2");
    assert_eq!(
        command(&bare, "git", &["show", "main:content.txt"]),
        "second revision"
    );
    let db = Database::open_existing_read_only(data.join("personal.db")).unwrap();
    assert_eq!(db.list_commit_map(10).unwrap().len(), 2);
    assert!(db.is_personal_svn_rev_synced(1).unwrap());
    assert!(db.is_personal_svn_rev_synced(2).unwrap());
    assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("2"));
    assert_eq!(
        db.get_watermark("git_sha").unwrap().as_deref(),
        Some(original.as_str())
    );
    assert!(db.get_state("writer_fence_v1:lease").unwrap().is_none());
    drop(db);
    let mut repeat = spawn(root.path(), &config, &["import", "--full"]);
    assert!(
        wait(&mut repeat, root.path()).success(),
        "{}",
        std::fs::read_to_string(root.path().join("stderr")).unwrap()
    );
    assert_eq!(command(&bare, "git", &["rev-parse", "main"]), original);
    assert_eq!(command(&bare, "git", &["rev-list", "--count", "main"]), "2");
    let db = Database::open_existing_read_only(data.join("personal.db")).unwrap();
    assert_eq!(db.list_commit_map(10).unwrap().len(), 2);
    assert!(db.get_state("writer_fence_v1:lease").unwrap().is_none());
    assert!(db.get_state("writer_fence_v1:native").unwrap().is_none());
    assert!(!data.join("reposync.db").exists());
}

#[test]
fn personal_status_reports_existing_database_query_failures_without_checkpointing() {
    for table in ["watermarks", "audit_log"] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let path = data.join("personal.db");
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute_batch("PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        db.conn()
            .execute_batch(&format!("DROP TABLE {table}"))
            .unwrap();
        let main = std::fs::read(&path).unwrap();
        let wal_path = path.with_extension("db-wal");
        let wal = std::fs::read(&wal_path).unwrap();
        let config = config(root.path(), &data);
        let mut process = spawn(root.path(), &config, &["status"]);
        assert!(
            !wait(&mut process, root.path()).success(),
            "{table}: query error was hidden"
        );
        let stderr = std::fs::read_to_string(root.path().join("stderr")).unwrap();
        assert!(
            stderr.contains("failed to read Personal"),
            "{table}: {stderr}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), main);
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
        assert!(!data.join("reposync.lock").exists());
        assert!(!data.join("reposync.db").exists());
        assert!(db.get_state("encryption_key").unwrap().is_none());
    }
}

#[test]
fn personal_doctor_labels_failed_watermark_reads_without_checkpointing() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let path = data.join("personal.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    db.conn()
        .execute_batch("PRAGMA wal_autocheckpoint=0; DROP TABLE watermarks;")
        .unwrap();
    let main = std::fs::read(&path).unwrap();
    let wal_path = path.with_extension("db-wal");
    let wal = std::fs::read(&wal_path).unwrap();
    let config = config(root.path(), &data);
    let mut process = spawn(root.path(), &config, &["doctor"]);
    assert!(wait(&mut process, root.path()).success());
    let stdout = std::fs::read_to_string(root.path().join("stdout")).unwrap();
    assert!(stdout.contains("Read failed"), "{stdout}");
    assert!(!stdout.contains("Watermarks        Not set"), "{stdout}");
    assert_eq!(std::fs::read(&path).unwrap(), main);
    assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
    assert!(!data.join("reposync.lock").exists());
}
