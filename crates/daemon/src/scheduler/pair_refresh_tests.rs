//! Scheduler preparation must respect durable refresh exclusion after restart.
use super::*;

#[path = "../../../core/tests/fixtures/pair_refresh_job.rs"]
mod job;

fn scheduler(f: &job::Fixture) -> Scheduler {
    let config_file = f.dir.path().join("config.toml");
    std::fs::write(&config_file, format!(
        "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/fixture\"\n",
        f.dir.path().display()
    )).unwrap();
    let config = AppConfig::load_from_file(&config_file).unwrap();
    let git_path = f.dir.path().join("dummy-git");
    assert!(std::process::Command::new("git")
        .args(["init", git_path.to_str().unwrap()])
        .output()
        .unwrap()
        .status
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&git_path).unwrap(),
        Arc::new(IdentityMapper::new(&reposync_core::config::IdentityConfig::default()).unwrap()),
    );
    let (_, rx) = mpsc::channel(1);
    let (ws, _) = broadcast::channel(1);
    Scheduler::new(
        Arc::new(engine),
        Duration::from_secs(1),
        rx,
        ws,
        Arc::new(RwLock::new(ImportProgress::default())),
        Database::new(f.dir.path().join("reposync.db")).unwrap(),
        config,
    )
}

#[tokio::test]
async fn reopened_scheduler_refuses_held_child_and_parent_before_clone_preparation() {
    for target in ["child", "parent"] {
        let f = job::Fixture::new();
        f.db.conn()
            .execute("UPDATE repositories SET enabled = (id = ?1)", [target])
            .unwrap();
        let pins = f.pins();
        let guard = f.busy();
        let id = f.admit(pins, &guard);
        drop(guard);
        let scheduler = scheduler(&f);
        assert!(matches!(
            resolve_repo_import_baseline(&scheduler.db, target).unwrap(),
            RepoImportBaseline::Verified { .. }
        ));
        let before = std::fs::read(f.dir.path().join("dummy-git/.git/config")).unwrap();
        scheduler.maybe_run_repo_cycles().await;
        assert!(scheduler.sync_handles.lock().await.is_empty());
        assert!(!f.dir.path().join("repos").exists());
        assert_eq!(
            std::fs::read(f.dir.path().join("dummy-git/.git/config")).unwrap(),
            before
        );
        assert_eq!(
            scheduler.db.pair_refresh_operation(id).unwrap().phase,
            reposync_core::db::pair_refresh_operations::PairRefreshPhase::Prepared
        );
        assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
        assert!(reposync_core::busy::try_acquire_pair("parent", "child").is_some());
    }
}

#[tokio::test]
async fn scheduler_refuses_an_existing_preparation_window_without_creating_workdirs() {
    let f = job::Fixture::new();
    f.db.conn()
        .execute("UPDATE repositories SET enabled = (id = 'child')", [])
        .unwrap();
    let scheduler = scheduler(&f);
    let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
    scheduler.maybe_run_repo_cycles().await;
    assert!(scheduler.sync_handles.lock().await.is_empty());
    assert!(!f.dir.path().join("repos").exists());
    assert!(f.no_hold(None));
    assert!(reposync_core::busy::try_acquire("child").is_some());
    drop(window);
    f.admit(f.pins(), &f.busy());
    assert!(!f.no_hold(None));
}
