//! Exercise the legacy import caller at actual publication boundaries.
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use reposync_core::{
    config::IdentityConfig,
    db::Database,
    file_policy::FilePolicy,
    git::GitClient,
    identity::IdentityMapper,
    import::{ImportConfig, ImportOutcome, ImportProgress, ImportRunState},
    svn::SvnClient,
};
use tokio::sync::RwLock;

fn command(dir: &Path, program: &str, args: &[&str]) {
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    dir: tempfile::TempDir,
    db: Database,
    git: Arc<Mutex<GitClient>>,
    svn_url: String,
    cancelled: Arc<AtomicBool>,
}

impl Fixture {
    fn new(revisions: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let svn = dir.path().join("svn");
        command(dir.path(), "svnadmin", &["create", svn.to_str().unwrap()]);
        let payload = dir.path().join("payload");
        std::fs::create_dir(&payload).unwrap();
        std::fs::write(payload.join("file.txt"), b"revision 1\n").unwrap();
        let svn_url = format!("file://{}", svn.display());
        command(
            dir.path(),
            "svn",
            &[
                "import",
                payload.to_str().unwrap(),
                &svn_url,
                "-m",
                "baseline",
                "--username",
                "fixture",
                "--non-interactive",
            ],
        );
        if revisions > 1 {
            let wc = dir.path().join("svn-wc");
            command(
                dir.path(),
                "svn",
                &[
                    "checkout",
                    &svn_url,
                    wc.to_str().unwrap(),
                    "--non-interactive",
                ],
            );
            for revision in 2..=revisions {
                std::fs::write(wc.join("file.txt"), format!("revision {revision}\n")).unwrap();
                command(
                    &wc,
                    "svn",
                    &[
                        "commit",
                        "file.txt",
                        "-m",
                        "advance",
                        "--username",
                        "fixture",
                        "--non-interactive",
                    ],
                );
            }
        }
        let bare = dir.path().join("origin.git");
        git2::Repository::init_bare(&bare).unwrap();
        let workdir = dir.path().join("git");
        let repo = git2::Repository::init(&workdir).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
        let git = Arc::new(Mutex::new(GitClient::new(&workdir).unwrap()));
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        Self {
            dir,
            db,
            git,
            svn_url,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn hook(&self, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.dir.path().join("origin.git/hooks/pre-receive");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    async fn run(&self) -> anyhow::Result<ImportOutcome> {
        let config = ImportConfig {
            committer_name: "Fixture".into(),
            committer_email: "fixture@example.invalid".into(),
            remote_name: "origin".into(),
            branch: "main".into(),
            push_token: None,
            message_prefix: None,
            trunk_path: String::new(),
        };
        reposync_core::import::run_full_import(
            &SvnClient::new(&self.svn_url, "fixture", ""),
            &self.git,
            &IdentityMapper::new(&IdentityConfig::default()).unwrap(),
            &self.db,
            &FilePolicy::new(0, Vec::new()),
            &config,
            ImportRunState {
                progress: Arc::new(RwLock::new(ImportProgress {
                    batches_pushed: 1,
                    ..ImportProgress::default()
                })),
                ws_broadcast: None,
                repo_id: None,
                operation_id: None,
                cancel_signal: Some(self.cancelled.clone()),
            },
        )
        .await
    }

    fn windows(&self) -> Vec<String> {
        self.db
            .conn()
            .prepare("SELECT value FROM kv_state WHERE key LIKE 'pair_refresh_writer_window_%'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

#[tokio::test]
async fn rejected_intermediate_batch_is_an_error_with_one_attempt_and_no_managed_receipt() {
    let f = Fixture::new(51);
    f.hook("#!/bin/sh\nprintf x >> attempts\nexit 1\n");
    let error = f.run().await.unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("legacy Git publication outcome uncertain; writer window retained"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read(f.dir.path().join("origin.git/attempts")).unwrap(),
        b"x"
    );
    let bare = git2::Repository::open_bare(f.dir.path().join("origin.git")).unwrap();
    assert!(bare.find_reference("refs/heads/main").is_err());
    assert!(f.db.get_watermark("svn_rev").unwrap().is_none());
    assert!(f.db.get_watermark("git_sha").unwrap().is_none());
    assert!(f
        .db
        .active_import_operation("__reposync_personal__")
        .unwrap()
        .is_none());
    assert_eq!(f.windows().len(), 1);
    assert!(f.windows()[0].contains("unjournalled_publication_possible"));
    assert!(f.db.begin_ordinary_writer_window(None).is_err());
}

#[tokio::test]
async fn cancellation_during_final_push_confirms_its_exact_tip_and_checkpoints_before_release() {
    let f = Fixture::new(1);
    f.hook("#!/bin/sh\nprintf x >> attempts\n: > ready\ncount=0\nwhile [ ! -f release ]; do\n  count=$((count + 1))\n  [ \"$count\" -lt 1000 ] || exit 1\n  sleep 0.01\ndone\nexit 0\n");
    let bare_path = f.dir.path().join("origin.git");
    let monitor_path = bare_path.clone();
    let cancelled = f.cancelled.clone();
    let monitor = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !monitor_path.join("ready").exists() {
            assert!(
                Instant::now() < deadline,
                "publication barrier was never reached"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        cancelled.store(true, Ordering::SeqCst);
        std::fs::write(monitor_path.join("release"), b"release").unwrap();
    });
    let outcome = f.run().await;
    monitor.join().unwrap();
    let ImportOutcome::Completed {
        svn_rev,
        git_sha,
        commits,
    } = outcome.unwrap()
    else {
        panic!("confirmed final publication did not complete");
    };
    assert!(f.cancelled.load(Ordering::SeqCst));
    assert_eq!(commits, 1);
    assert_eq!(svn_rev, 1);
    let tip = git2::Repository::open_bare(&bare_path)
        .unwrap()
        .find_reference("refs/heads/main")
        .unwrap()
        .target()
        .unwrap()
        .to_string();
    assert_eq!(tip, git_sha);
    assert_eq!(f.db.get_watermark("svn_rev").unwrap(), Some("1".into()));
    assert_eq!(f.db.get_watermark("git_sha").unwrap(), Some(git_sha));
    assert_eq!(std::fs::read(bare_path.join("attempts")).unwrap(), b"x");
    assert!(f.windows().is_empty());
    drop(f.db.begin_ordinary_writer_window(None).unwrap());
}
