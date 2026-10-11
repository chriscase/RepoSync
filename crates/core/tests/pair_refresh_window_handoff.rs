//! Continuous scheduler preparation ownership; no refresh API is enabled.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use std::sync::Arc;

use reposync_core::{
    config::{AppConfig, IdentityConfig},
    db::{ordinary_writer_windows::begin_shared_ordinary_writer_window, Database},
    git::client::GitClient,
    identity::IdentityMapper,
    svn::client::SvnClient,
    sync_engine::SyncEngine,
};

fn rows(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn window_count(db: &Database) -> i64 {
    db.conn()
        .query_row(
            "SELECT count(*) FROM kv_state WHERE key LIKE 'pair_refresh_writer_window_%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn engine(f: &job::Fixture, id: Option<&str>, db: Database) -> SyncEngine {
    let path = f.dir.path().join("engine");
    std::fs::create_dir(&path).unwrap();
    git2::Repository::init(&path).unwrap();
    std::fs::write(path.join("sentinel"), b"untouched").unwrap();
    let config: AppConfig = toml::from_str(
        "[daemon]\ndata_dir='.'\n[svn]\nurl='file:///nonexistent'\nusername=''\n[github]\nrepo='fixture/repo'\n",
    ).unwrap();
    let mut engine = SyncEngine::new(
        config,
        db,
        SvnClient::new("file:///nonexistent", "", ""),
        GitClient::new(&path).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    if let Some(id) = id {
        engine.set_repo_id(id.into());
    }
    engine
}

fn assert_pristine(f: &job::Fixture, git_config: &[u8]) {
    let path = f.dir.path().join("engine");
    assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"untouched");
    assert_eq!(std::fs::read(path.join(".git/config")).unwrap(), git_config);
    assert!(!path.join(".git/index").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_window_moves_to_real_engine_without_reacquisition_or_admission_gap() {
    let f = job::Fixture::new();
    let db = Arc::new(Database::new(f.dir.path().join("reposync.db")).unwrap());
    let window = begin_shared_ordinary_writer_window(db.clone(), Some("child")).unwrap();
    let pins = f.pins();
    let guard = f.busy();
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &guard
        )
        .is_err());
    assert!(db.begin_ordinary_writer_window(Some("child")).is_err());
    let engine = engine(
        &f,
        Some("child"),
        Database::new(f.dir.path().join("reposync.db")).unwrap(),
    );
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (run_tx, run_rx) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        ready_tx.send(()).unwrap();
        run_rx.await.unwrap();
        let error = engine
            .run_sync_cycle_with_writer_window(window)
            .await
            .unwrap_err();
        assert!(!engine.is_running());
        assert!(
            !error.to_string().contains("ordinary writer window"),
            "{error}"
        );
        assert_eq!(window_count(engine.db()), 0);
    });
    ready_rx.await.unwrap();
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins,
            &guard
        )
        .is_err());
    assert_eq!(window_count(&db), 1);
    run_tx.send(()).unwrap();
    worker.await.unwrap();
    assert_eq!(window_count(&db), 0);
    assert!(f.no_hold(None));
}

#[tokio::test]
async fn wrong_engine_scope_refuses_before_credentials_or_working_tree_mutation() {
    let f = job::Fixture::new();
    let db = Arc::new(Database::new(f.dir.path().join("reposync.db")).unwrap());
    let window = begin_shared_ordinary_writer_window(db.clone(), Some("child")).unwrap();
    let engine = engine(
        &f,
        Some("parent"),
        Database::new(f.dir.path().join("reposync.db")).unwrap(),
    );
    let git_config = std::fs::read(f.dir.path().join("engine/.git/config")).unwrap();
    let error = engine
        .run_sync_cycle_with_writer_window(window)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("another engine scope"),
        "{error}"
    );
    assert!(!engine.is_running());
    assert_eq!(window_count(&db), 0);
    assert_eq!(
        f.db.get_repo_watermark("parent").unwrap(),
        (4, "b".repeat(40))
    );
    assert_pristine(&f, &git_config);
}

#[tokio::test]
async fn another_database_cannot_use_or_mutate_the_admitted_window() {
    let f = job::Fixture::new();
    let db = Arc::new(Database::new(f.dir.path().join("reposync.db")).unwrap());
    let window = begin_shared_ordinary_writer_window(db.clone(), Some("child")).unwrap();
    let foreign = Database::in_memory().unwrap();
    foreign.initialize().unwrap();
    let before = rows(&foreign);
    let engine = engine(&f, Some("child"), foreign);
    let git_config = std::fs::read(f.dir.path().join("engine/.git/config")).unwrap();
    let error = engine
        .run_sync_cycle_with_writer_window(window)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("ordinary writer window"),
        "{error}"
    );
    assert!(!engine.is_running());
    assert_eq!(rows(engine.db()), before);
    assert_eq!(window_count(&db), 0);
    assert_pristine(&f, &git_config);
}

#[tokio::test]
async fn uncertain_global_window_refuses_handoff_and_remains_held_after_reopen() {
    let f = job::Fixture::new();
    let db = Arc::new(Database::new(f.dir.path().join("reposync.db")).unwrap());
    let window = begin_shared_ordinary_writer_window(db.clone(), None).unwrap();
    window.mark_unjournalled_publication_possible().unwrap();
    let before = rows(&db);
    let engine = engine(
        &f,
        None,
        Database::new(f.dir.path().join("reposync.db")).unwrap(),
    );
    let git_config = std::fs::read(f.dir.path().join("engine/.git/config")).unwrap();
    let error = engine
        .run_sync_cycle_with_writer_window(window)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("uncertain publication"),
        "{error}"
    );
    assert!(!engine.is_running());
    assert_eq!(rows(&db), before);
    assert_pristine(&f, &git_config);
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    assert!(reopened.begin_ordinary_writer_window(None).is_err());
    assert_eq!(rows(&reopened), before);
}

#[tokio::test]
async fn copied_database_with_identical_window_and_epoch_cannot_accept_handoff() {
    let f = job::Fixture::new();
    let db = Arc::new(Database::new(f.dir.path().join("reposync.db")).unwrap());
    let window = begin_shared_ordinary_writer_window(db.clone(), Some("child")).unwrap();
    let copy_path = f.dir.path().join("copied.db");
    db.conn()
        .execute("VACUUM INTO ?1", [copy_path.to_str().unwrap()])
        .unwrap();
    let copied = Database::new(&copy_path).unwrap();
    assert_eq!(rows(&copied), rows(&db));
    let before = rows(&copied);
    let engine = engine(&f, Some("child"), copied);
    let git_config = std::fs::read(f.dir.path().join("engine/.git/config")).unwrap();
    let error = engine
        .run_sync_cycle_with_writer_window(window)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("another database"), "{error}");
    assert!(!engine.is_running());
    assert_eq!(rows(engine.db()), before);
    assert_eq!(window_count(&db), 0);
    assert_eq!(window_count(engine.db()), 1);
    assert_pristine(&f, &git_config);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_preparation_handoff_releases_only_its_preparing_window() {
    let f = job::Fixture::new();
    let db = Arc::new(Database::new(f.dir.path().join("reposync.db")).unwrap());
    let window = begin_shared_ordinary_writer_window(db.clone(), Some("child")).unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(window);
    });
    ready_rx.await.unwrap();
    assert_eq!(window_count(&db), 1);
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert_eq!(window_count(&db), 0);
    f.admit(f.pins(), &f.busy());
    assert!(!f.no_hold(None));
}
