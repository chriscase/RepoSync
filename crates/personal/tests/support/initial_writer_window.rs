use reposync_core::db::{personal_scope::PERSONAL_SCOPE_KEY, Database};
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn rows(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// Exercise the real entry point before either a fresh clone or an existing
// checkout's credentials can be touched. Revocation is deliberately present:
// writer exclusion must win before the credential resolver is even reached.
pub fn assert_startup_window(binary: &str, prefix: &[&str], commands: &[&[&str]]) {
    for uncertain in [false, true] {
        for existing in [false, true] {
            for command in commands {
                let dir = tempfile::tempdir().unwrap();
                let data = dir.path().join("data");
                std::fs::create_dir(&data).unwrap();
                let db = Database::new(data.join("personal.db")).unwrap();
                db.initialize().unwrap();
                db.set_state(&format!("secret_git_token_{PERSONAL_SCOPE_KEY}"), "")
                    .unwrap();
                let window = db.begin_ordinary_writer_window(None).unwrap();
                if uncertain {
                    window.mark_unjournalled_publication_possible().unwrap();
                }
                let before = rows(&db);
                let checkout = data.join("git-repo");
                let original_config = if existing {
                    Some(initialize_checkout(&checkout))
                } else {
                    None
                };
                let config = dir.path().join("personal.toml");
                std::fs::write(
                    &config,
                    format!(
                        r#"
[personal]
data_dir = {data:?}
[svn]
url = "file:///reposync-fixture-does-not-exist"
username = "fixture"
password_env = "REPOSYNC_WRITER_WINDOW_PASSWORD"
[github]
api_url = "http://127.0.0.1:1/api"
git_base_url = "http://127.0.0.1:1"
repo = "owner/repo"
token_env = "REPOSYNC_WRITER_WINDOW_TOKEN"
auto_create = false
[developer]
name = "Fixture"
email = "fixture@example.invalid"
svn_username = "fixture"
"#
                    ),
                )
                .unwrap();
                let output = std::fs::File::create(dir.path().join("stdout")).unwrap();
                let error_path = dir.path().join("stderr");
                let error = std::fs::File::create(&error_path).unwrap();
                let mut child = ChildGuard(
                    Command::new(binary)
                        .args(prefix)
                        .arg(&config)
                        .args(*command)
                        .env("REPOSYNC_WRITER_WINDOW_PASSWORD", "fixture-only")
                        .env("REPOSYNC_WRITER_WINDOW_TOKEN", "disposable-window-token")
                        .env("GIT_CONFIG_GLOBAL", "/dev/null")
                        .env("GIT_CONFIG_NOSYSTEM", "1")
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .stdout(output)
                        .stderr(error)
                        .spawn()
                        .unwrap(),
                );
                let deadline = Instant::now() + Duration::from_secs(15);
                let status = loop {
                    if let Some(status) = child.0.try_wait().unwrap() {
                        break status;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "startup did not finish: {command:?}"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                };
                let error = std::fs::read_to_string(error_path).unwrap();
                assert!(!status.success(), "held startup succeeded: {command:?}");
                assert!(
                    error.contains("another run owns this writer scope"),
                    "{error}"
                );
                assert!(
                    !error.contains("git credentials explicitly revoked"),
                    "{error}"
                );
                assert!(!error.contains("disposable-window-token"), "{error}");
                assert_eq!(rows(&db), before);
                assert!(db.list_commit_map(10).unwrap().is_empty());
                assert!(db.get_watermark("svn_rev").unwrap().is_none());
                assert!(db.get_watermark("git_sha").unwrap().is_none());
                if let Some(original) = original_config {
                    assert_eq!(
                        std::fs::read(checkout.join(".git/config")).unwrap(),
                        original
                    );
                    assert_eq!(
                        std::fs::read(checkout.join("keep.txt")).unwrap(),
                        b"untouched"
                    );
                    assert!(!checkout.join(".git/index").exists());
                    assert!(!checkout.join(".gitattributes").exists());
                } else {
                    assert!(!checkout.exists(), "held startup cloned or initialized Git");
                }
                drop(window);
                assert_eq!(db.begin_ordinary_writer_window(None).is_err(), uncertain);
            }
        }
    }
}

fn initialize_checkout(path: &Path) -> Vec<u8> {
    // Use the shipped core client so the shared fixture needs no extra crate
    // dependency in the CLI package.
    reposync_core::git::GitClient::init(path).unwrap();
    std::fs::write(path.join("keep.txt"), b"untouched").unwrap();
    std::fs::read(path.join(".git/config")).unwrap()
}
