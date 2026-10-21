//! Fail-closed `blocked_patterns` loader. Fixtures are local temp dirs and
//! `file://` URLs only: no credentials and no remote services.
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reposync_core::config::IdentityConfig;
use reposync_core::models::Repository;
use tracing_subscriber::fmt::MakeWriter;

use super::*;

struct LogMake(Arc<Mutex<String>>);

struct LogWriter(Arc<Mutex<String>>);

impl<'a> MakeWriter<'a> for LogMake {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("log buffer")
            .push_str(&String::from_utf8_lossy(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Arc<Mutex<String>>, tracing::subscriber::DefaultGuard) {
    let buf = Arc::new(Mutex::new(String::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(LogMake(buf.clone()))
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (buf, guard)
}

fn logged(buf: &str, message: &str, repo_name: &str) -> bool {
    buf.lines()
        .any(|line| line.contains(message) && line.contains(repo_name))
}

struct DueRepo {
    id: &'static str,
    name: &'static str,
    blocked_patterns: Option<&'static str>,
}

fn due_scheduler(repos: &[DueRepo]) -> (tempfile::TempDir, Scheduler) {
    let tmp = tempfile::tempdir().unwrap();
    let config_file = tmp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/fixture\"\n",
            tmp.path().display()
        ),
    )
    .unwrap();
    let config = AppConfig::load_from_file(&config_file).unwrap();
    let db = Database::new(&tmp.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let now = Utc::now().to_rfc3339();
    for repo in repos {
        db.insert_repository(&Repository {
            id: repo.id.into(),
            name: repo.name.into(),
            svn_url: "file:///nonexistent".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "gitea".into(),
            git_api_url: "file:///nonexistent".into(),
            git_repo: format!("local/{}", repo.id),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 1,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            last_svn_rev: 2,
            last_git_sha: "verified-old".into(),
            last_sync_at: Some("2000-01-01T00:00:00Z".into()),
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: repo.blocked_patterns.map(str::to_string),
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        db.set_state(&format!("last_svn_rev_{}", repo.id), "2")
            .unwrap();
        let git_repo = tmp.path().join("repos").join(repo.id).join("git-repo");
        std::fs::create_dir_all(&git_repo).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init"])
            .current_dir(&git_repo)
            .status()
            .unwrap()
            .success());
    }
    let dummy_git = tmp.path().join("dummy-git");
    assert!(std::process::Command::new("git")
        .args(["init", dummy_git.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    let (_, rx) = mpsc::channel(1);
    let (ws, _) = broadcast::channel(1);
    let scheduler = Scheduler::new(
        Arc::new(engine),
        Duration::from_secs(1),
        rx,
        ws,
        Arc::new(RwLock::new(ImportProgress::default())),
        db,
        config,
    );
    (tmp, scheduler)
}

fn install_parse_observer() -> &'static Mutex<Vec<(String, Vec<String>)>> {
    PARSED_BLOCKED_PATTERNS.get_or_init(|| Mutex::new(Vec::new()))
}

fn parsed_for(repo_id: &str) -> Vec<Vec<String>> {
    install_parse_observer()
        .lock()
        .expect("parsed blocked patterns")
        .iter()
        .filter(|(id, _)| id == repo_id)
        .map(|(_, patterns)| patterns.clone())
        .collect()
}

async fn spawned(scheduler: &Scheduler) -> usize {
    let handles: Vec<_> = scheduler.sync_handles.lock().await.drain(..).collect();
    let count = handles.len();
    for handle in handles {
        let finished = tokio::time::timeout(Duration::from_secs(30), handle).await;
        assert!(finished.is_ok(), "spawned sync cycle did not finish");
    }
    count
}

fn stored<'a>(scheduler: &'a Scheduler, repo_id: &str) -> Repository {
    scheduler
        .db
        .get_repository(repo_id)
        .unwrap()
        .expect("repository")
}

#[tokio::test(flavor = "current_thread")]
async fn valid_blocked_pattern_array_configures_rules_and_starts_the_cycle() {
    install_parse_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id: "valid-array-repo",
        name: "valid-array-repo",
        blocked_patterns: Some("[\"secret-*\"]"),
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert_eq!(
        parsed_for("valid-array-repo"),
        vec![vec!["secret-*".to_string()]]
    );
    assert!(logged(
        &log,
        "path validation rules configured",
        "valid-array-repo"
    ));
    assert!(logged(
        &log,
        "starting per-repo sync cycle",
        "valid-array-repo"
    ));
    assert!(!logged(
        &log,
        "refusing repository sync",
        "valid-array-repo"
    ));
    assert_eq!(spawned(&scheduler).await, 1);
    let repo = stored(&scheduler, "valid-array-repo");
    assert_eq!(repo.allowed_paths, None);
    assert_eq!(repo.blocked_patterns.as_deref(), Some("[\"secret-*\"]"));
}

#[tokio::test(flavor = "current_thread")]
async fn empty_blocked_pattern_array_starts_with_no_blocked_patterns() {
    install_parse_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id: "empty-array-repo",
        name: "empty-array-repo",
        blocked_patterns: Some("[]"),
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert_eq!(parsed_for("empty-array-repo"), vec![Vec::<String>::new()]);
    assert!(!logged(
        &log,
        "path validation rules configured",
        "empty-array-repo"
    ));
    assert!(logged(
        &log,
        "starting per-repo sync cycle",
        "empty-array-repo"
    ));
    assert!(!logged(
        &log,
        "refusing repository sync",
        "empty-array-repo"
    ));
    assert_eq!(spawned(&scheduler).await, 1);
    let repo = stored(&scheduler, "empty-array-repo");
    assert_eq!(repo.blocked_patterns.as_deref(), Some("[]"));
    assert_eq!(repo.allowed_paths, None);
}

#[tokio::test(flavor = "current_thread")]
async fn non_json_blocked_pattern_refuses_that_repository() {
    install_parse_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id: "raw-secret-repo",
        name: "raw-secret-repo",
        blocked_patterns: Some("secret-*"),
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert!(parsed_for("raw-secret-repo").is_empty());
    assert!(logged(
        &log,
        "blocked_patterns must be a JSON array of strings",
        "raw-secret-repo"
    ));
    assert!(
        logged(&log, "setting=blocked_patterns", "raw-secret-repo")
            || logged(&log, "setting=\"blocked_patterns\"", "raw-secret-repo")
    );
    assert!(log.contains("raw-secret-repo"));
    assert!(log.contains("repo_id"));
    assert!(!logged(
        &log,
        "starting per-repo sync cycle",
        "raw-secret-repo"
    ));
    assert!(!logged(
        &log,
        "path validation rules configured",
        "raw-secret-repo"
    ));
    assert_eq!(spawned(&scheduler).await, 0);
    let repo = stored(&scheduler, "raw-secret-repo");
    assert_eq!(repo.sync_status, "idle");
    assert_eq!(repo.consecutive_errors, 0);
    assert_eq!(repo.blocked_patterns.as_deref(), Some("secret-*"));
    assert_eq!(repo.allowed_paths, None);
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_and_non_array_blocked_patterns_refuse_that_repository() {
    install_parse_observer();
    for (id, raw) in [
        ("malformed-brace-repo", "{"),
        ("malformed-object-repo", "{}"),
    ] {
        let (_tmp, scheduler) = due_scheduler(&[DueRepo {
            id,
            name: id,
            blocked_patterns: Some(raw),
        }]);
        let (buf, _guard) = capture();
        scheduler.maybe_run_repo_cycles().await;
        let log = buf.lock().expect("log buffer").clone();
        assert!(
            parsed_for(id).is_empty(),
            "{raw} must not parse as an empty block list"
        );
        assert!(
            logged(&log, "blocked_patterns must be a JSON array of strings", id),
            "{raw}"
        );
        assert!(!logged(&log, "starting per-repo sync cycle", id), "{raw}");
        assert!(
            !logged(&log, "path validation rules configured", id),
            "{raw}"
        );
        assert_eq!(spawned(&scheduler).await, 0, "{raw}");
        let repo = stored(&scheduler, id);
        assert_eq!(repo.sync_status, "idle");
        assert_eq!(repo.consecutive_errors, 0);
        assert_eq!(repo.blocked_patterns.as_deref(), Some(raw));
        assert_eq!(repo.allowed_paths, None);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_blocked_patterns_leave_the_other_repository_running() {
    install_parse_observer();
    let (_tmp, scheduler) = due_scheduler(&[
        DueRepo {
            id: "a-bad-patterns",
            name: "a-bad-patterns",
            blocked_patterns: Some("secret-*"),
        },
        DueRepo {
            id: "b-good-repo",
            name: "b-good-repo",
            blocked_patterns: Some("[]"),
        },
    ]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert!(parsed_for("a-bad-patterns").is_empty());
    assert_eq!(parsed_for("b-good-repo"), vec![Vec::<String>::new()]);
    assert!(logged(
        &log,
        "blocked_patterns must be a JSON array of strings",
        "a-bad-patterns"
    ));
    assert!(!logged(
        &log,
        "starting per-repo sync cycle",
        "a-bad-patterns"
    ));
    assert!(logged(&log, "starting per-repo sync cycle", "b-good-repo"));
    assert!(!logged(
        &log,
        "path validation rules configured",
        "b-good-repo"
    ));
    assert_eq!(spawned(&scheduler).await, 1);
    let bad = stored(&scheduler, "a-bad-patterns");
    assert_eq!(bad.sync_status, "idle");
    assert_eq!(bad.consecutive_errors, 0);
    assert_eq!(bad.blocked_patterns.as_deref(), Some("secret-*"));
    let good = stored(&scheduler, "b-good-repo");
    assert_eq!(good.blocked_patterns.as_deref(), Some("[]"));
    assert_eq!(good.allowed_paths, None);
    assert_eq!(bad.allowed_paths, None);
}
