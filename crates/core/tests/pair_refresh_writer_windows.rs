//! Durable in-flight engine exclusion controls; no execution API is enabled.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::Database;

fn rows(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn held_scoped_and_global_window_admission_refuses_without_any_state_change() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    let before = rows(&f.db);
    for scope in [
        Some("child"),
        Some("parent"),
        None,
        Some("__reposync_personal__"),
    ] {
        assert!(f.db.begin_ordinary_writer_window(scope).is_err());
        assert_eq!(rows(&f.db), before);
    }
    assert!(!f.no_hold(None));
}

#[test]
fn live_preparing_window_excludes_refresh_until_its_owner_drops() {
    for scope in [Some("child"), Some("parent"), None] {
        let f = job::Fixture::new();
        let pins = f.pins();
        let guard = f.busy();
        let window = f.db.begin_ordinary_writer_window(scope).unwrap();
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
        assert!(f.no_hold(None));
        assert!(f.db.begin_ordinary_writer_window(scope).is_err());
        drop(window);
        f.admit(pins, &guard);
        assert!(!f.no_hold(None));
    }
}

#[test]
fn unrelated_window_and_refresh_keep_separate_ownership() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let window =
        f.db.begin_ordinary_writer_window(Some("unrelated"))
            .unwrap();
    f.admit(pins, &guard);
    drop(window);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    let other =
        f.db.begin_ordinary_writer_window(Some("unrelated"))
            .unwrap();
    drop(other);
}

#[test]
fn global_possible_publication_survives_drop_and_reopen_without_retry_permission() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let window = f.db.begin_ordinary_writer_window(None).unwrap();
    window.mark_unjournalled_publication_possible().unwrap();
    drop(window);
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    assert!(reopened.begin_ordinary_writer_window(None).is_err());
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
    assert!(f.no_hold(None));
    f.db.set_state("last_sync_at", "diagnosis").unwrap();
    assert!(rows(&f.db).iter().any(
        |(key, value)| key.starts_with("pair_refresh_writer_window_")
            && value.contains("unjournalled_publication_possible")
    ));
}

#[test]
fn abrupt_exit_preparation_evidence_is_not_cleared_by_database_reopen() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
    // Model abrupt exit: destructors do not run. The durable row is the oracle.
    std::mem::forget(window);
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    assert!(reopened
        .begin_ordinary_writer_window(Some("child"))
        .is_err());
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
    assert!(f.no_hold(None));
}

#[test]
fn future_or_corrupt_window_evidence_refuses_both_entrances_globally() {
    for (key, value) in [
        ("pair_refresh_writer_window_future:document:unknown", "{}"),
        ("pair_refresh_writer_window_v1:document:unknown", "corrupt"),
    ] {
        let f = job::Fixture::new();
        let pins = f.pins();
        let guard = f.busy();
        f.db.set_state(key, value).unwrap();
        let before = rows(&f.db);
        assert!(f
            .db
            .begin_ordinary_writer_window(Some("unrelated"))
            .is_err());
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
        assert_eq!(rows(&f.db), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn two_database_connections_choose_one_inflight_or_refresh_owner() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let other = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let start = std::sync::Barrier::new(2);
    let release = std::sync::Barrier::new(2);
    let (admitted, running) = std::thread::scope(|scope| {
        let runner = scope.spawn(|| {
            start.wait();
            let window = other.begin_ordinary_writer_window(Some("child"));
            let ok = window.is_ok();
            release.wait();
            drop(window);
            ok
        });
        start.wait();
        let admitted =
            f.db.admit_pair_refresh(
                "admin",
                "request",
                &pins.approval_digest().unwrap(),
                pins,
                &guard,
            )
            .is_ok();
        release.wait();
        (admitted, runner.join().unwrap())
    });
    assert!(admitted ^ running, "one durable owner, not two or zero");
    assert_eq!(f.no_hold(None), !admitted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_engine_refuses_held_scope_before_credential_or_working_tree_changes() {
    use reposync_core::{
        config::{AppConfig, IdentityConfig},
        git::client::GitClient,
        identity::IdentityMapper,
        svn::client::SvnClient,
        sync_engine::SyncEngine,
    };
    use std::sync::Arc;
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    drop(guard);
    for id in [Some("child"), Some("parent"), None] {
        let path = f
            .dir
            .path()
            .join(format!("engine-{}", id.unwrap_or("global")));
        std::fs::create_dir(&path).unwrap();
        git2::Repository::init(&path).unwrap();
        std::fs::write(path.join("sentinel"), b"untouched").unwrap();
        let config:AppConfig=toml::from_str("[daemon]\ndata_dir='.'\n[svn]\nurl='http://127.0.0.1:1/refused'\nusername=''\n[github]\nrepo='fixture/repo'\n").unwrap();
        let db = Database::new(f.dir.path().join("reposync.db")).unwrap();
        let git_before = std::fs::read(path.join(".git/config")).unwrap();
        let before = rows(&db);
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new("http://127.0.0.1:1/refused", "", ""),
            GitClient::new(&path).unwrap(),
            Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
        );
        if let Some(id) = id {
            engine.set_repo_id(id.into());
        }
        let error = engine.run_sync_cycle().await.unwrap_err();
        assert!(
            error.to_string().contains("durable pair refresh"),
            "{error}"
        );
        assert!(!engine.is_running());
        assert_eq!(rows(engine.db()), before);
        assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"untouched");
        assert_eq!(std::fs::read(path.join(".git/config")).unwrap(), git_before);
        assert!(!path.join(".git/index").exists());
    }
}
