//! Direct import exclusion before local preparation and legacy publication.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::{
    config::IdentityConfig,
    db::{
        import_operations::{resolve_repo_import_baseline, SnapshotPin},
        Database,
    },
    file_policy::FilePolicy,
    git::GitClient,
    identity::IdentityMapper,
    import::{ImportConfig, ImportOutcome, ImportProgress, ImportRunState},
    svn::SvnClient,
};
use std::sync::{atomic::AtomicBool, Arc, Mutex};
use tokio::sync::RwLock;

fn rows(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn client(f: &job::Fixture) -> Arc<Mutex<GitClient>> {
    let path = f.dir.path().join("git");
    git2::Repository::init(&path).unwrap();
    std::fs::write(path.join("sentinel"), b"untouched").unwrap();
    Arc::new(Mutex::new(GitClient::new(&path).unwrap()))
}

fn config() -> ImportConfig {
    ImportConfig {
        committer_name: "fixture".into(),
        committer_email: "fixture@example.test".into(),
        remote_name: "origin".into(),
        branch: "main".into(),
        push_token: None,
        message_prefix: None,
        trunk_path: String::new(),
    }
}

fn state(scope: Option<&str>) -> ImportRunState {
    ImportRunState {
        progress: Arc::new(RwLock::new(ImportProgress::default())),
        ws_broadcast: None,
        repo_id: scope.map(str::to_owned),
        operation_id: scope.map(|_| "unused-operation".into()),
        cancel_signal: Some(Arc::new(AtomicBool::new(false))),
    }
}

fn pin() -> SnapshotPin {
    SnapshotPin {
        svn_uuid: "unused".into(),
        canonical_url: "file:///nonexistent".into(),
        operative_rev: 3,
        peg_rev: 3,
        copy_from_path: None,
        copy_from_rev: None,
        requested: "3".into(),
    }
}

async fn run_refused(
    f: &job::Fixture,
    git: &Arc<Mutex<GitClient>>,
    scope: Option<&str>,
    snapshot: bool,
) -> anyhow::Error {
    let policy = FilePolicy::with_lfs(0, Vec::new(), 1, &[]);
    let svn = SvnClient::new("file:///nonexistent", "", "");
    if snapshot {
        reposync_core::import::run_snapshot_import(
            &svn,
            git,
            &f.db,
            &policy,
            &config(),
            &pin(),
            state(scope),
        )
        .await
        .unwrap_err()
    } else {
        let identity = IdentityMapper::new(&IdentityConfig::default()).unwrap();
        reposync_core::import::run_full_import(
            &svn,
            git,
            &identity,
            &f.db,
            &policy,
            &config(),
            state(scope),
        )
        .await
        .unwrap_err()
    }
}

fn pristine(f: &job::Fixture, config: &[u8]) {
    assert_eq!(
        std::fs::read(f.dir.path().join("git/sentinel")).unwrap(),
        b"untouched"
    );
    assert_eq!(
        std::fs::read(f.dir.path().join("git/.git/config")).unwrap(),
        config
    );
    assert!(!f.dir.path().join("git/.git/index").exists());
    assert!(!f.dir.path().join("git/.git/hooks/pre-push").exists());
    assert!(!f.dir.path().join("git/.gitattributes").exists());
}

#[tokio::test]
async fn full_import_refuses_held_scopes_before_lfs_credentials_or_local_changes() {
    let f = job::Fixture::new();
    let git = client(&f);
    f.admit(f.pins(), &f.busy());
    let before = rows(&f.db);
    let config = std::fs::read(f.dir.path().join("git/.git/config")).unwrap();
    for scope in [
        Some("child"),
        Some("parent"),
        None,
        Some("__reposync_personal__"),
    ] {
        let error = run_refused(&f, &git, scope, false).await;
        assert!(
            format!("{error:#}").contains("durable pair refresh"),
            "{error:#}"
        );
        assert_eq!(rows(&f.db), before);
        pristine(&f, &config);
    }
}

#[tokio::test]
async fn snapshot_import_refuses_held_scopes_before_materialization_or_lfs() {
    let f = job::Fixture::new();
    let git = client(&f);
    f.admit(f.pins(), &f.busy());
    let before = rows(&f.db);
    let config = std::fs::read(f.dir.path().join("git/.git/config")).unwrap();
    for scope in [
        Some("child"),
        Some("parent"),
        None,
        Some("__reposync_personal__"),
    ] {
        let error = run_refused(&f, &git, scope, true).await;
        assert!(
            format!("{error:#}").contains("durable pair refresh"),
            "{error:#}"
        );
        assert_eq!(rows(&f.db), before);
        pristine(&f, &config);
    }
}

#[tokio::test]
async fn ordinary_preparation_and_uncertain_global_publication_exclude_direct_imports() {
    for uncertain in [false, true] {
        let f = job::Fixture::new();
        let git = client(&f);
        let window = f.db.begin_ordinary_writer_window(None).unwrap();
        if uncertain {
            window.mark_unjournalled_publication_possible().unwrap();
            drop(window);
        } else {
            std::mem::forget(window);
        }
        let before = rows(&f.db);
        let config = std::fs::read(f.dir.path().join("git/.git/config")).unwrap();
        for snapshot in [false, true] {
            let error = run_refused(&f, &git, None, snapshot).await;
            assert!(
                format!("{error:#}").contains("another run owns"),
                "{error:#}"
            );
            assert_eq!(rows(&f.db), before);
            pristine(&f, &config);
        }
    }
}

#[test]
fn held_checkpoint_backfill_cannot_repair_drift_before_writer_admission() {
    let f = job::Fixture::new();
    f.admit(f.pins(), &f.busy());
    f.db.conn()
        .execute("DELETE FROM kv_state WHERE key='last_svn_rev_child'", [])
        .unwrap();
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let before = rows(&reopened);
    assert!(resolve_repo_import_baseline(&reopened, "child")
        .unwrap_err()
        .to_string()
        .contains("durable pair refresh"));
    assert_eq!(rows(&reopened), before);
    assert!(reopened.get_state("last_svn_rev_child").unwrap().is_none());
}

#[test]
fn active_config_import_journals_block_refresh_before_global_clone_preparation() {
    for scope in ["__reposync_personal__", "personal"] {
        let f = job::Fixture::new();
        let op =
            f.db.create_import_operation(scope, "admin", "config-import", "target")
                .unwrap();
        let pins = f.pins();
        let guard = f.busy();
        let before = rows(&f.db);
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
        assert_eq!(f.db.active_import_operation(scope).unwrap().unwrap(), op);
        assert!(f.no_hold(None));
    }
}

fn source(f: &job::Fixture) -> String {
    fn command(args: &[&str]) {
        let out = std::process::Command::new(args[0])
            .args(&args[1..])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let svn = f.dir.path().join("svn");
    command(&["svnadmin", "create", svn.to_str().unwrap()]);
    let payload = f.dir.path().join("payload");
    std::fs::create_dir(&payload).unwrap();
    std::fs::write(payload.join("file.txt"), b"baseline\n").unwrap();
    let url = format!("file://{}", svn.display());
    command(&[
        "svn",
        "import",
        payload.to_str().unwrap(),
        &url,
        "-m",
        "baseline",
        "--username",
        "fixture",
        "--non-interactive",
    ]);
    url
}

async fn legacy(f: &job::Fixture, db: &Database, reject: bool) -> anyhow::Result<ImportOutcome> {
    let url = source(f);
    let bare = f.dir.path().join("bare.git");
    git2::Repository::init_bare(&bare).unwrap();
    if reject {
        use std::os::unix::fs::PermissionsExt;
        let hook = bare.join("hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\nprintf x >> attempts\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = f.dir.path().join("legacy-git");
    let repository = git2::Repository::init(&path).unwrap();
    repository.set_head("refs/heads/main").unwrap();
    repository.remote("origin", bare.to_str().unwrap()).unwrap();
    let git = Arc::new(Mutex::new(GitClient::new(&path).unwrap()));
    let mut run = state(None);
    // Model a later legacy batch so this disposable target receives a normal
    // push. No forced ref update is needed for the publication control.
    run.progress = Arc::new(RwLock::new(ImportProgress {
        batches_pushed: 1,
        ..ImportProgress::default()
    }));
    reposync_core::import::run_full_import(
        &SvnClient::new(&url, "fixture", ""),
        &git,
        &IdentityMapper::new(&IdentityConfig::default()).unwrap(),
        db,
        &FilePolicy::new(0, Vec::new()),
        &config(),
        run,
    )
    .await
}

#[tokio::test]
async fn successful_legacy_import_verifies_the_remote_before_releasing_its_window() {
    let f = job::Fixture::new();
    let parent = f.db.get_repo_watermark("parent").unwrap();
    let child = f.db.get_repo_watermark("child").unwrap();
    let legacy_db = Database::in_memory().unwrap();
    legacy_db.initialize().unwrap();
    assert!(legacy_db.list_repositories().unwrap().is_empty());
    let ImportOutcome::Completed {
        svn_rev, git_sha, ..
    } = legacy(&f, &legacy_db, false).await.unwrap()
    else {
        panic!("legacy import did not complete")
    };
    assert_eq!(svn_rev, 1);
    let bare = git2::Repository::open_bare(f.dir.path().join("bare.git")).unwrap();
    assert_eq!(
        bare.find_reference("refs/heads/main")
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        git_sha
    );
    assert!(!rows(&legacy_db)
        .iter()
        .any(|(key, _)| key.starts_with("pair_refresh_writer_window_")));
    assert_eq!(
        legacy_db.get_watermark("svn_rev").unwrap(),
        Some("1".into())
    );
    assert_eq!(legacy_db.get_watermark("git_sha").unwrap(), Some(git_sha));
    assert_eq!(f.db.get_repo_watermark("parent").unwrap(), parent);
    assert_eq!(f.db.get_repo_watermark("child").unwrap(), child);
    drop(legacy_db.begin_ordinary_writer_window(None).unwrap());
}

#[tokio::test]
async fn rejected_legacy_publication_retains_exclusion_without_retry_or_checkpoint_advance() {
    let f = job::Fixture::new();
    let legacy_db = Database::in_memory().unwrap();
    legacy_db.initialize().unwrap();
    let prior_svn = legacy_db.get_watermark("svn_rev").unwrap();
    let prior_git = legacy_db.get_watermark("git_sha").unwrap();
    let error = legacy(&f, &legacy_db, true).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("writer window retained"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read(f.dir.path().join("bare.git/attempts")).unwrap(),
        b"x"
    );
    assert!(git2::Repository::open_bare(f.dir.path().join("bare.git"))
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
    assert_eq!(legacy_db.get_watermark("svn_rev").unwrap(), prior_svn);
    assert_eq!(legacy_db.get_watermark("git_sha").unwrap(), prior_git);
    assert!(rows(&legacy_db).iter().any(|(key, value)| key
        .starts_with("pair_refresh_writer_window_")
        && value.contains("unjournalled_publication_possible")));
    let pins = f.pins();
    for pin in [&pins.child, &pins.parent] {
        assert!(
            reposync_core::db::ordinary_writer_windows::require_quiet_for_refresh(
                &legacy_db.conn(),
                &pin.repo_id,
                &pin.scope_uuid,
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn legacy_import_refuses_managed_checkpoints_before_remote_publication() {
    let f = job::Fixture::new();
    let parent = f.db.get_repo_watermark("parent").unwrap();
    let child = f.db.get_repo_watermark("child").unwrap();
    let svn = f.db.get_watermark("svn_rev").unwrap();
    let git = f.db.get_watermark("git_sha").unwrap();
    let error = legacy(&f, &f.db, false).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("refusing global git_sha watermark"),
        "{error:#}"
    );
    assert!(git2::Repository::open_bare(f.dir.path().join("bare.git"))
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
    assert_eq!(f.db.get_repo_watermark("parent").unwrap(), parent);
    assert_eq!(f.db.get_repo_watermark("child").unwrap(), child);
    assert_eq!(f.db.get_watermark("svn_rev").unwrap(), svn);
    assert_eq!(f.db.get_watermark("git_sha").unwrap(), git);
    assert!(!rows(&f.db)
        .iter()
        .any(|(key, _)| key.starts_with("pair_refresh_writer_window_")));
    let pins = f.pins();
    let guard = f.busy();
    let op =
        f.db.admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins,
            &guard,
        )
        .unwrap();
    f.db.cancel_unpublished_pair_refresh(op.id).unwrap();
    assert!(f.no_hold(None));
}

#[tokio::test]
async fn automatic_reconciliation_refuses_held_scopes_before_workdir_or_credentials() {
    let f = job::Fixture::new();
    let _git = client(&f);
    f.admit(f.pins(), &f.busy());
    let before = rows(&f.db);
    let config = std::fs::read(f.dir.path().join("git/.git/config")).unwrap();
    for scope in ["child", "parent"] {
        let repo = f.db.get_repository(scope).unwrap().unwrap();
        let error = reposync_core::auto_reconcile::reconcile_held_external_writes(
            &f.db,
            &repo,
            &SvnClient::new("file:///nonexistent", "", ""),
            Some(f.dir.path().join("git").as_path()),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("durable pair refresh"),
            "{error}"
        );
        assert_eq!(rows(&f.db), before);
        pristine(&f, &config);
    }
}

#[tokio::test]
async fn unrelated_reconciliation_releases_only_its_ordinary_window() {
    let f = job::Fixture::new();
    let id = f.admit(f.pins(), &f.busy());
    let op = f.db.pair_refresh_operation(id).unwrap();
    let repo = f.db.get_repository("unrelated").unwrap().unwrap();
    let result = reposync_core::auto_reconcile::reconcile_held_external_writes(
        &f.db,
        &repo,
        &SvnClient::new("file:///nonexistent", "", ""),
        None,
    )
    .await
    .unwrap();
    assert!(result.attempts.is_empty());
    assert_eq!(f.db.pair_refresh_operation(id).unwrap(), op);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    assert!(!rows(&f.db)
        .iter()
        .any(|(key, _)| key.starts_with("pair_refresh_writer_window_")));
}

#[tokio::test]
async fn corrupt_refresh_evidence_refuses_even_unrelated_reconciliation() {
    let f = job::Fixture::new();
    f.db.set_state("pair_refresh_execution_future:document:unknown", "corrupt")
        .unwrap();
    let before = rows(&f.db);
    let repo = f.db.get_repository("unrelated").unwrap().unwrap();
    assert!(
        reposync_core::auto_reconcile::reconcile_held_external_writes(
            &f.db,
            &repo,
            &SvnClient::new("file:///nonexistent", "", ""),
            None,
        )
        .await
        .is_err()
    );
    assert_eq!(rows(&f.db), before);
}
