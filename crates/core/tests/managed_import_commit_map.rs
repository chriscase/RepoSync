//! Managed import writers must tag `commit_map.repo_id` for every mapping row.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::RwLock;

use reposync_core::db::import_operations::import_target_fingerprint;
use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::mapper::IdentityMapper;
use reposync_core::import::{
    ImportConfig, ImportOutcome, ImportPhase, ImportProgress, ImportRunState,
};
use reposync_core::models::Repository;
use reposync_core::snapshot::{resolve_snapshot_pin, SnapshotRevision};
use reposync_core::svn::SvnClient;
use reposync_core::writer_fence;
use tempfile::TempDir;

fn toolchain_available() -> bool {
    ["svn", "svnadmin", "git"].iter().all(|tool| {
        Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

fn create_svn_repo(dir: &Path) -> String {
    let repo_dir = dir.join("svn_repo");
    assert!(Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let hook = repo_dir.join("hooks/pre-revprop-change");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!("file://{}", repo_dir.display())
}

fn svn_checkout(url: &str, wc_path: &Path) {
    assert!(Command::new("svn")
        .args([
            "checkout",
            url,
            wc_path.to_str().unwrap(),
            "--non-interactive"
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .unwrap()
        .success());
}

fn svn_commit_file(wc_path: &Path, filename: &str, content: &str, message: &str) -> i64 {
    let file_path = wc_path.join(filename);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&file_path, content).unwrap();
    let status = Command::new("svn")
        .args(["status", file_path.to_str().unwrap()])
        .output()
        .unwrap();
    if String::from_utf8_lossy(&status.stdout).contains('?') {
        assert!(Command::new("svn")
            .args(["add", file_path.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
    }
    let output = Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            wc_path.to_str().unwrap(),
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Committed revision ")
                .map(|rev| rev.trim_end_matches('.').parse::<i64>().unwrap())
        })
        .expect("committed revision")
}

fn setup_git_with_bare_origin(
    work_dir: &Path,
    bare_dir: &Path,
) -> Arc<std::sync::Mutex<GitClient>> {
    git2::Repository::init_bare(bare_dir).unwrap();
    let git_client = GitClient::init(work_dir).unwrap();
    let repo = git2::Repository::open(work_dir).unwrap();
    repo.remote("origin", bare_dir.to_str().unwrap()).unwrap();
    // Match `import_config().branch` (`main`) so publication can succeed.
    assert!(Command::new("git")
        .args(["checkout", "-B", "main"])
        .current_dir(work_dir)
        .status()
        .unwrap()
        .success());
    Arc::new(std::sync::Mutex::new(git_client))
}

fn insert_managed_repo(
    db: &Database,
    repo_id: &str,
    svn_url: &str,
    git_workdir: &Path,
) -> Repository {
    let now = chrono::Utc::now().to_rfc3339();
    let repo = Repository {
        id: repo_id.into(),
        name: "Managed".into(),
        svn_url: svn_url.into(),
        svn_branch: String::new(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: format!("file://{}", git_workdir.display()),
        git_repo: "managed/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    };
    db.insert_repository(&repo).unwrap();
    repo
}

fn assert_commit_map_rows_tagged(db: &Database, repo_id: &str) {
    let null_rows: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE direction='svn_to_git' AND repo_id IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        null_rows, 0,
        "managed import must not leave NULL-repo_id commit_map rows"
    );
    let tagged_rows: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE direction='svn_to_git' AND repo_id = ?1",
            [repo_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        tagged_rows > 0,
        "managed import must write at least one scoped commit_map row"
    );
}

fn import_run_state(repo_id: &str, operation_id: &str) -> ImportRunState {
    ImportRunState {
        progress: Arc::new(RwLock::new(ImportProgress::default())),
        ws_broadcast: None,
        repo_id: Some(repo_id.to_string()),
        operation_id: Some(operation_id.to_string()),
        cancel_signal: None,
    }
}

fn import_config() -> ImportConfig {
    ImportConfig {
        committer_name: "Test User".into(),
        committer_email: "test@example.com".into(),
        remote_name: "origin".into(),
        branch: "main".into(),
        push_token: None,
        message_prefix: None,
        trunk_path: String::new(),
    }
}

#[tokio::test]
async fn managed_imports_tag_commit_map_repo_id() {
    if !toolchain_available() {
        eprintln!("SKIPPED: svn/svnadmin/git not found in PATH");
        return;
    }

    managed_snapshot_import_tags_commit_map_repo_id().await;
    managed_full_import_tags_commit_map_repo_id().await;
}

async fn managed_snapshot_import_tags_commit_map_repo_id() {
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "snapshot.txt", "snapshot baseline\n", "snapshot r1");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Database::new(data_dir.join("state.db")).unwrap();
    db.initialize().unwrap();
    let _fence = writer_fence::claim(&db, &data_dir).unwrap();

    let repo_id = "managed-snapshot";
    let repo = insert_managed_repo(&db, repo_id, &svn_url, &git_work);
    let fingerprint = import_target_fingerprint(&repo, &git_work);
    let op = db
        .create_import_operation(repo_id, "admin", "snapshot-import", &fingerprint)
        .unwrap();
    let pin = resolve_snapshot_pin(&SvnClient::new(&svn_url, "", ""), SnapshotRevision::Head)
        .await
        .unwrap();
    db.pin_snapshot_import(repo_id, &op.id, pin.clone())
        .unwrap();
    db.start_import_operation(repo_id, &op.id).unwrap();

    let outcome = reposync_core::import::run_snapshot_import(
        &SvnClient::new(&svn_url, "", ""),
        &git_client,
        &db,
        &FilePolicy::new(0, Vec::new()),
        &import_config(),
        &pin,
        import_run_state(repo_id, &op.id),
    )
    .await
    .expect("snapshot import must succeed");
    assert!(
        matches!(
            outcome,
            ImportOutcome::Completed { commits: 1, .. }
                | ImportOutcome::ReconciliationRequired { commits: 1, .. }
        ),
        "snapshot import outcome: {:?}",
        outcome
    );
    assert_commit_map_rows_tagged(&db, repo_id);
}

async fn managed_full_import_tags_commit_map_repo_id() {
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "one.txt", "first\n", "r1");
    svn_commit_file(&wc, "two.txt", "second\n", "r2");

    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = Database::new(data_dir.join("state.db")).unwrap();
    db.initialize().unwrap();
    let _fence = writer_fence::claim(&db, &data_dir).unwrap();

    let repo_id = "managed-full";
    let repo = insert_managed_repo(&db, repo_id, &svn_url, &git_work);
    let fingerprint = import_target_fingerprint(&repo, &git_work);
    let op = db
        .create_import_operation(repo_id, "admin", "full-import", &fingerprint)
        .unwrap();
    db.start_import_operation(repo_id, &op.id).unwrap();

    let identity_mapper =
        IdentityMapper::new(&reposync_core::config::IdentityConfig::default()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = reposync_core::import::run_full_import(
        &SvnClient::new(&svn_url, "", ""),
        &git_client,
        &identity_mapper,
        &db,
        &FilePolicy::new(0, Vec::new()),
        &import_config(),
        ImportRunState {
            progress: Arc::new(RwLock::new(ImportProgress {
                phase: ImportPhase::Importing,
                ..ImportProgress::default()
            })),
            ws_broadcast: None,
            repo_id: Some(repo_id.to_string()),
            operation_id: Some(op.id.clone()),
            cancel_signal: Some(cancel),
        },
    )
    .await
    .expect("full import must succeed");
    assert!(
        matches!(
            outcome,
            ImportOutcome::Completed { commits, .. } if commits >= 1
        ) || matches!(
            outcome,
            ImportOutcome::ReconciliationRequired { commits, .. } if commits >= 1
        ),
        "full import outcome: {:?}",
        outcome
    );
    assert_commit_map_rows_tagged(&db, repo_id);
}

/// A real snapshot is an authenticated starting frontier, not an instruction
/// to skip the first later SVN revision or replay the baseline into SVN.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_snapshot_continues_bidirectionally_and_restarts_without_replay() {
    snapshot_continuation(false, SnapshotGitPayload::Regular).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_snapshot_checkpoint_failure_reopens_and_reconciles_without_republication() {
    snapshot_continuation(true, SnapshotGitPayload::Regular).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_missing_lfs_object_after_snapshot_never_writes_pointer_text_to_svn() {
    snapshot_continuation(false, SnapshotGitPayload::MissingLfs).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_literal_pointer_text_without_filter_remains_a_regular_file() {
    snapshot_continuation(false, SnapshotGitPayload::LiteralPointer).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_explicit_filter_named_unspecified_is_not_treated_as_absent() {
    snapshot_continuation(false, SnapshotGitPayload::AmbiguousFilter).await;
}

enum SnapshotGitPayload {
    Regular,
    MissingLfs,
    LiteralPointer,
    AmbiguousFilter,
}

async fn snapshot_continuation(fail_checkpoint: bool, payload: SnapshotGitPayload) {
    assert!(toolchain_available(), "real SVN/Git tools are required");
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, "history.txt", "old history\n", "r1");
    let selected = svn_commit_file(&wc, "history.txt", "selected baseline\n", "r2");
    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git_client = setup_git_with_bare_origin(&git_work, &bare);
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db_path = data_dir.join("state.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let _fence = writer_fence::claim(&db, &data_dir).unwrap();
    let repo_id = "snapshot-continuation";
    let mut repo = insert_managed_repo(&db, repo_id, &svn_url, &git_work);
    // Reconciliation checks the actual origin against registered configuration.
    // The older mapping-only fixture did not need a matching remote identity.
    repo.git_api_url = format!("file://{}", tmp.path().display());
    repo.git_repo = "origin".into();
    db.update_repository(&repo).unwrap();
    checked_git(
        &git_work,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("file://{}", bare.display()),
        ],
    );
    let op = db
        .create_import_operation(
            repo_id,
            "fixture",
            "snapshot-continuation",
            &import_target_fingerprint(&repo, &git_work),
        )
        .unwrap();
    let svn = SvnClient::new(&svn_url, "", "");
    let pin = resolve_snapshot_pin(&svn, SnapshotRevision::Number(selected))
        .await
        .unwrap();
    db.pin_snapshot_import(repo_id, &op.id, pin.clone())
        .unwrap();
    db.start_import_operation(repo_id, &op.id).unwrap();
    // Advance after pinning: the baseline must remain r2 and r3 must remain
    // genuine pending work for the ordinary engine.
    let later = svn_commit_file(&wc, "later.txt", "later SVN bytes\n", "r3 after pin");
    let outcome = reposync_core::import::run_snapshot_import(
        &svn,
        &git_client,
        &db,
        &FilePolicy::new(0, Vec::new()),
        &import_config(),
        &pin,
        import_run_state(repo_id, &op.id),
    )
    .await
    .unwrap();
    let ImportOutcome::Completed {
        commits,
        svn_rev,
        git_sha,
    } = outcome
    else {
        panic!("snapshot must complete: {outcome:?}");
    };
    assert_eq!((commits, svn_rev), (1, selected));
    if fail_checkpoint {
        db.conn().execute_batch("CREATE TRIGGER rs68_checkpoint_failure BEFORE UPDATE OF last_svn_rev ON repositories BEGIN SELECT RAISE(ABORT,'rs68 checkpoint failure'); END;").unwrap();
        assert!(db
            .complete_import_operation(repo_id, &op.id, svn_rev, &git_sha)
            .is_err());
        assert_eq!(db.get_repo_watermark(repo_id).unwrap(), (0, String::new()));
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1",
                    [repo_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        // The published ref is real, but the failed transaction must not make
        // it an accepted checkpoint. Reopen exactly as startup does.
        let reopened = Database::new(&db_path).unwrap();
        reopened.initialize().unwrap();
        reopened.hold_interrupted_imports().unwrap();
        reopened
            .conn()
            .execute_batch("DROP TRIGGER rs68_checkpoint_failure")
            .unwrap();
        let before = checked_git(
            tmp.path(),
            &["--git-dir", bare.to_str().unwrap(), "rev-parse", "main"],
        );
        let result =
            reposync_core::import::apply_import_reconciliation(&reopened, &repo, &op.id, &git_work)
                .await
                .unwrap();
        assert!(result.finalized, "{result:?}");
        assert_eq!(result.observed_sha.as_deref(), Some(git_sha.as_str()));
        assert_eq!(
            checked_git(
                tmp.path(),
                &["--git-dir", bare.to_str().unwrap(), "rev-parse", "main"]
            ),
            before
        );
        let repeated =
            reposync_core::import::apply_import_reconciliation(&reopened, &repo, &op.id, &git_work)
                .await
                .unwrap();
        assert_eq!(repeated.operation.id, op.id);
        assert!(repeated.finalized);
    } else {
        db.complete_import_operation(repo_id, &op.id, svn_rev, &git_sha)
            .unwrap();
    }
    assert_eq!(
        db.get_repo_watermark(repo_id).unwrap(),
        (selected, git_sha.clone())
    );
    assert_eq!(
        std::fs::read(git_work.join("history.txt")).unwrap(),
        b"selected baseline\n"
    );
    assert!(!git_work.join("later.txt").exists());
    assert_eq!(
        git2::Repository::open(&git_work)
            .unwrap()
            .find_commit(git2::Oid::from_str(&git_sha).unwrap())
            .unwrap()
            .parent_count(),
        0
    );
    drop(git_client);
    drop(db);

    let reopen = || snapshot_engine(&db_path, &data_dir, &svn_url, &git_work, repo_id);
    let engine = reopen();
    let first = engine
        .run_sync_cycle()
        .await
        .expect("later SVN work must sync after snapshot");
    assert_eq!((first.svn_to_git_count, first.git_to_svn_count), (1, 0));
    assert_eq!(
        std::fs::read(git_work.join("later.txt")).unwrap(),
        b"later SVN bytes\n"
    );
    assert_eq!(engine.db().get_repo_watermark(repo_id).unwrap().0, later);
    let developer = tmp.path().join("developer");
    checked_git(
        tmp.path(),
        &[
            "clone",
            "-b",
            "main",
            bare.to_str().unwrap(),
            developer.to_str().unwrap(),
        ],
    );
    let literal_pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 1234\n",
        "b".repeat(64)
    );
    let from_git: &[u8] = if matches!(
        payload,
        SnapshotGitPayload::LiteralPointer | SnapshotGitPayload::AmbiguousFilter
    ) {
        literal_pointer.as_bytes()
    } else {
        b"Git continuation\n"
    };
    std::fs::write(developer.join("from-git.txt"), from_git).unwrap();
    checked_git(&developer, &["add", "from-git.txt"]);
    if matches!(payload, SnapshotGitPayload::AmbiguousFilter) {
        std::fs::write(
            developer.join(".gitattributes"),
            b"from-git.txt filter=unspecified\n",
        )
        .unwrap();
        checked_git(&developer, &["add", ".gitattributes"]);
    }
    if matches!(payload, SnapshotGitPayload::MissingLfs) {
        // A real Git commit containing a valid LFS pointer, with no object in
        // either checkout. Build the tree through libgit2 so the negative
        // fixture does not require installing an LFS clean/smudge filter.
        std::fs::write(
            developer.join(".gitattributes"),
            b"asset.bin filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        std::fs::write(
            developer.join("asset.bin"),
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 1234\n",
                "a".repeat(64)
            ),
        )
        .unwrap();
        GitClient::new(&developer)
            .unwrap()
            .commit(
                "Missing LFS object",
                "Fixture",
                "fixture@example.invalid",
                "Fixture",
                "fixture@example.invalid",
            )
            .unwrap();
        // Model a checkout that deliberately leaves LFS pointer blobs in the
        // working tree (the observable result of skip-smudge). These inert
        // fixture filters do not claim a real LFS lifecycle or tool install.
        // Safety must not depend on checkout smudging before the engine reads
        // committed blobs for SVN publication.
        checked_git(&git_work, &["config", "filter.lfs.process", ""]);
        checked_git(&git_work, &["config", "filter.lfs.smudge", "cat"]);
        checked_git(&git_work, &["config", "filter.lfs.clean", "cat"]);
        checked_git(&git_work, &["config", "filter.lfs.required", "false"]);
        // A later commit removes the filter. The engine must inspect the
        // pointer-producing commit's attributes, not the reset batch tip.
        std::fs::write(developer.join(".gitattributes"), b"asset.bin -filter\n").unwrap();
        GitClient::new(&developer)
            .unwrap()
            .commit(
                "Later filter removal must not rewrite earlier meaning",
                "Fixture",
                "fixture@example.invalid",
                "Fixture",
                "fixture@example.invalid",
            )
            .unwrap();
    } else {
        checked_git(&developer, &["commit", "-m", "Git work after snapshot"]);
    }
    let source_sha = checked_git(&developer, &["rev-parse", "HEAD"]);
    checked_git(&developer, &["push", "origin", "main"]);
    let checkpoint_before = engine.db().get_repo_watermark(repo_id).unwrap();
    let second = engine.run_sync_cycle().await;
    if matches!(
        payload,
        SnapshotGitPayload::MissingLfs | SnapshotGitPayload::AmbiguousFilter
    ) {
        let error =
            second.expect_err("missing LFS objects must never be published as pointer text");
        assert!(
            matches!(&error, reposync_core::errors::SyncError::HistoryBlocked { reason, detail }
            if reason == "unsupported_git_semantics" && detail.contains("LFS")),
            "{error}"
        );
        assert_eq!(
            SvnClient::new(&svn_url, "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            later
        );
        assert_eq!(
            engine.db().get_repo_watermark(repo_id).unwrap(),
            checkpoint_before
        );
        assert!(engine
            .db()
            .active_svn_commit_operation(repo_id)
            .unwrap()
            .is_none());
        assert_eq!(
            engine
                .db()
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='git_to_svn'",
                    [repo_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            checked_git(
                tmp.path(),
                &["--git-dir", bare.to_str().unwrap(), "rev-parse", "main"]
            )
            .trim(),
            source_sha.trim()
        );
        drop(engine);
        let reopened = reopen();
        assert!(
            matches!(reopened.run_sync_cycle().await, Err(reposync_core::errors::SyncError::HistoryBlocked { reason, .. }) if reason == "unsupported_git_semantics")
        );
        assert_eq!(
            reopened.db().get_repo_watermark(repo_id).unwrap(),
            checkpoint_before
        );
        assert_eq!(
            SvnClient::new(&svn_url, "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            later
        );
        return;
    }
    let second = second.expect("Git work must sync onto SVN snapshot lineage");
    assert_eq!(second.git_to_svn_count, 1);
    let current = SvnClient::new(&svn_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    assert_eq!(current, later + 1);
    let export = tmp.path().join("independent-export");
    SvnClient::new(&svn_url, "", "")
        .export("", current, &export)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(export.join("from-git.txt")).unwrap(),
        from_git
    );
    assert_eq!(
        std::fs::read(export.join("history.txt")).unwrap(),
        b"selected baseline\n"
    );
    let applied: i64 = engine.db().conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND direction='git_to_svn' AND status='applied'",
        rusqlite::params![repo_id, source_sha.trim()], |row| row.get(0),
    ).unwrap();
    assert_eq!(applied, 1);
    drop(engine);
    let engine = reopen();
    let quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    let before = (
        engine.db().get_repo_watermark(repo_id).unwrap(),
        checked_git(&git_work, &["rev-parse", "HEAD"]),
    );
    let mapping_count: i64 = engine
        .db()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1",
            [repo_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(engine);
    let engine = reopen();
    let quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    assert_eq!(engine.db().get_repo_watermark(repo_id).unwrap(), before.0);
    assert_eq!(checked_git(&git_work, &["rev-parse", "HEAD"]), before.1);
    assert_eq!(
        SvnClient::new(&svn_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        current
    );
    assert_eq!(
        engine
            .db()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1",
                [repo_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        mapping_count
    );
}

fn checked_git(workdir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Snapshot fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Snapshot fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Requires the real git-lfs already installed by the hosted CI profile. No
/// fallback, generated pointer substitute, package installation or skipped pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_snapshot_real_lfs_publication_fetch_and_missing_object_verification() {
    use sha2::{Digest, Sha256};
    assert!(toolchain_available(), "real SVN/Git tools are required");
    let available = Command::new("git")
        .args(["lfs", "version"])
        .output()
        .unwrap();
    assert!(
        available.status.success(),
        "real git-lfs is required for this qualification case"
    );
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &wc);
    svn_commit_file(&wc, ".gitignore", "target/\n", "SVN origin");
    let raw: Vec<u8> = (0..1_100_000).map(|i| (i % 251) as u8).collect();
    std::fs::write(wc.join("asset.bin"), &raw).unwrap();
    let added = Command::new("svn")
        .args(["add", "asset.bin"])
        .current_dir(&wc)
        .output()
        .unwrap();
    assert!(added.status.success());
    let committed = Command::new("svn")
        .args(["commit", "-m", "binary snapshot", "--username", "fixture"])
        .current_dir(&wc)
        .output()
        .unwrap();
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git = setup_git_with_bare_origin(&git_work, &bare);
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let db = Database::new(data.join("state.db")).unwrap();
    db.initialize().unwrap();
    let _fence = writer_fence::claim(&db, &data).unwrap();
    let mut repo = insert_managed_repo(&db, "snapshot-lfs", &svn_url, &git_work);
    repo.lfs_threshold_mb = 1;
    repo.git_api_url = format!("file://{}", tmp.path().display());
    repo.git_repo = "origin".into();
    db.update_repository(&repo).unwrap();
    checked_git(
        &git_work,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("file://{}", bare.display()),
        ],
    );
    let op = db
        .create_import_operation(
            &repo.id,
            "fixture",
            "snapshot-lfs",
            &import_target_fingerprint(&repo, &git_work),
        )
        .unwrap();
    let svn = SvnClient::new(&svn_url, "", "");
    let pin = resolve_snapshot_pin(&svn, SnapshotRevision::Head)
        .await
        .unwrap();
    db.pin_snapshot_import(&repo.id, &op.id, pin.clone())
        .unwrap();
    db.start_import_operation(&repo.id, &op.id).unwrap();
    let outcome = reposync_core::import::run_snapshot_import(
        &svn,
        &git,
        &db,
        &FilePolicy::with_lfs(0, Vec::new(), 1024 * 1024, &[]),
        &import_config(),
        &pin,
        import_run_state(&repo.id, &op.id),
    )
    .await
    .unwrap();
    let ImportOutcome::Completed {
        commits,
        svn_rev,
        git_sha,
    } = outcome
    else {
        panic!("real LFS publication must complete: {outcome:?}");
    };
    assert_eq!(commits, 1);
    db.complete_import_operation(&repo.id, &op.id, svn_rev, &git_sha)
        .unwrap();
    let pointer = checked_git(&git_work, &["show", "HEAD:asset.bin"]);
    let parsed =
        reposync_core::lfs::parse_lfs_pointer(pointer.as_bytes()).expect("committed LFS pointer");
    assert_eq!(parsed.oid, hex::encode(Sha256::digest(&raw)));
    assert_eq!(parsed.size, raw.len() as u64);
    assert_eq!(
        std::fs::read(reposync_core::lfs::local_lfs_object_path(
            &git_work,
            &parsed.oid
        ))
        .unwrap(),
        raw
    );
    assert_eq!(
        checked_git(&git_work, &["rev-list", "--count", "HEAD"]).trim(),
        "1"
    );
    let fetched = tmp.path().join("fresh-lfs-reader");
    let clone = Command::new("git")
        .args([
            "clone",
            "-b",
            "main",
            bare.to_str().unwrap(),
            fetched.to_str().unwrap(),
        ])
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
    // This configures only the new temporary clone using already-installed
    // tooling, matching the existing snapshot worker's --local setup.
    checked_git(&fetched, &["lfs", "install", "--local"]);
    checked_git(&fetched, &["lfs", "pull"]);
    assert_eq!(std::fs::read(fetched.join("asset.bin")).unwrap(), raw);
    assert_eq!(
        std::fs::read(fetched.join(".gitignore")).unwrap(),
        b"target/\n"
    );
    // Ordinary text work must still synchronize after an LFS-backed snapshot,
    // without substituting its unchanged pointer for the SVN binary bytes.
    std::fs::write(fetched.join("note.txt"), b"Git note after LFS snapshot\n").unwrap();
    checked_git(&fetched, &["add", "note.txt"]);
    checked_git(
        &fetched,
        &["commit", "-m", "Text work on LFS-backed baseline"],
    );
    checked_git(&fetched, &["push", "origin", "main"]);
    let mut engine = snapshot_engine(&data.join("state.db"), &data, &svn_url, &git_work, &repo.id);
    engine.set_lfs_threshold_bytes(1024 * 1024);
    let outbound = engine.run_sync_cycle().await.unwrap();
    assert_eq!(outbound.git_to_svn_count, 1);
    let after_git = SvnClient::new(&svn_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    assert_eq!(after_git, pin.operative_rev + 1);
    let export = tmp.path().join("after-git-export");
    SvnClient::new(&svn_url, "", "")
        .export("", after_git, &export)
        .await
        .unwrap();
    assert_eq!(std::fs::read(export.join("asset.bin")).unwrap(), raw);
    assert_eq!(
        std::fs::read(export.join("note.txt")).unwrap(),
        b"Git note after LFS snapshot\n"
    );
    let update = Command::new("svn")
        .arg("update")
        .current_dir(&wc)
        .output()
        .unwrap();
    assert!(update.status.success());
    let after_svn = svn_commit_file(
        &wc,
        "later.txt",
        "SVN note after LFS snapshot\n",
        "SVN continuation",
    );
    let inbound = engine.run_sync_cycle().await.unwrap();
    assert_eq!((inbound.svn_to_git_count, inbound.git_to_svn_count), (1, 0));
    assert_eq!(checked_git(&git_work, &["show", "HEAD:asset.bin"]), pointer);
    let final_checkpoint = engine.db().get_repo_watermark(&repo.id).unwrap();
    assert_eq!(final_checkpoint.0, after_svn);
    drop(engine);
    let mut reopened =
        snapshot_engine(&data.join("state.db"), &data, &svn_url, &git_work, &repo.id);
    reopened.set_lfs_threshold_bytes(1024 * 1024);
    let quiet = reopened.run_sync_cycle().await.unwrap();
    assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    assert_eq!(
        reopened.db().get_repo_watermark(&repo.id).unwrap(),
        final_checkpoint
    );
    let object = reposync_core::lfs::local_lfs_object_path(&fetched, &parsed.oid);
    assert_eq!(std::fs::read(&object).unwrap(), raw);
    // Deliberately remove only this fixture's fetched object. A real object
    // check must now fail; neither pointer bytes nor the hydrated worktree may
    // be substituted as proof that the LFS object remains available.
    std::fs::remove_file(&object).unwrap();
    assert!(!object.exists());
    let missing = Command::new("git")
        .args(["lfs", "fsck", "--objects", "HEAD"])
        .current_dir(&fetched)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        !missing.status.success(),
        "missing real object must fail fsck"
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(
        diagnostic.contains(&parsed.oid),
        "fsck must identify the actual missing object: {diagnostic}"
    );
    assert_eq!(db.get_repo_watermark(&repo.id).unwrap(), final_checkpoint);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"RS68_REAL_LFS_LIFECYCLE","raw_bytes":raw.len(),"pointer_bytes":pointer.len(),"oid":parsed.oid,"fresh_fetch_bytes_match":true,"missing_object_check_failed":true,"baseline_commits":1})
    );
}

fn snapshot_engine(
    db_path: &Path,
    data_dir: &Path,
    svn_url: &str,
    git_work: &Path,
    repo_id: &str,
) -> reposync_core::sync_engine::SyncEngine {
    use reposync_core::config::{AppConfig, IdentityConfig, SvnLayout};
    use reposync_core::sync_engine::SyncEngine;
    let mut config: AppConfig = toml::from_str(&format!(
        r#"
[daemon]
data_dir = "{}"
[svn]
url = "{}"
username = ""
password_env = "RS68_SYNTHETIC_UNUSED"
[github]
repo = "fixture/snapshot"
token_env = "RS68_SYNTHETIC_UNUSED"
"#,
        data_dir.display(),
        svn_url
    ))
    .unwrap();
    config.svn.layout = SvnLayout::Custom;
    config.svn.password = Some(String::new());
    config.github.token = Some(String::new());
    let db = Database::new(db_path).unwrap();
    db.initialize().unwrap();
    db.hold_interrupted_svn_commits().unwrap();
    db.hold_interrupted_git_pushes().unwrap();
    let mut engine = SyncEngine::new(
        config,
        db,
        SvnClient::new(svn_url, "", ""),
        GitClient::new(git_work).unwrap(),
        Arc::new(
            IdentityMapper::new(&IdentityConfig {
                email_domain: Some("example.invalid".into()),
                ..Default::default()
            })
            .unwrap(),
        ),
    );
    engine.set_repo_id(repo_id.into());
    engine
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs68_cancel_before_snapshot_effects_preserves_uninitialized_target_across_reopen() {
    use reposync_core::db::import_operations::ImportOperationState;
    assert!(toolchain_available(), "real SVN/Git tools are required");
    let tmp = TempDir::new().unwrap();
    let svn_url = create_svn_repo(tmp.path());
    let wc = tmp.path().join("svn_wc");
    svn_checkout(&svn_url, &wc);
    let revision = svn_commit_file(&wc, "source.txt", "SVN origin\n", "origin");
    let git_work = tmp.path().join("git_work");
    let bare = tmp.path().join("origin.git");
    let git = setup_git_with_bare_origin(&git_work, &bare);
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let path = data.join("state.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    let _fence = writer_fence::claim(&db, &data).unwrap();
    let repo = insert_managed_repo(&db, "snapshot-cancel", &svn_url, &git_work);
    let op = db
        .create_import_operation(
            &repo.id,
            "fixture",
            "snapshot-cancel",
            &import_target_fingerprint(&repo, &git_work),
        )
        .unwrap();
    let svn = SvnClient::new(&svn_url, "", "");
    let pin = resolve_snapshot_pin(&svn, SnapshotRevision::Head)
        .await
        .unwrap();
    db.pin_snapshot_import(&repo.id, &op.id, pin.clone())
        .unwrap();
    db.start_import_operation(&repo.id, &op.id).unwrap();
    db.request_import_cancel(&repo.id, &op.id).unwrap();
    let mut state = import_run_state(&repo.id, &op.id);
    state.cancel_signal = Some(Arc::new(AtomicBool::new(true)));
    let result = reposync_core::import::run_snapshot_import(
        &svn,
        &git,
        &db,
        &FilePolicy::new(0, Vec::new()),
        &import_config(),
        &pin,
        state,
    )
    .await
    .unwrap();
    assert!(matches!(result, ImportOutcome::Cancelled { commits: 0 }));
    db.finish_import_operation(
        &repo.id,
        &op.id,
        ImportOperationState::Cancelled,
        "fixture observed cancellation before any snapshot effect",
    )
    .unwrap();
    assert_eq!(db.get_repo_watermark(&repo.id).unwrap(), (0, String::new()));
    assert!(git2::Repository::open(&git_work).unwrap().head().is_err());
    assert!(git2::Repository::open_bare(&bare)
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
                [&repo.id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1",
                [&repo.id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(git);
    drop(db);
    let reopened = Database::new(&path).unwrap();
    reopened.initialize().unwrap();
    reopened.hold_interrupted_imports().unwrap();
    assert_eq!(
        reopened
            .get_import_operation(&repo.id, &op.id)
            .unwrap()
            .unwrap()
            .state,
        ImportOperationState::Cancelled
    );
    assert_eq!(
        reopened.get_repo_watermark(&repo.id).unwrap(),
        (0, String::new())
    );
    assert_eq!(
        SvnClient::new(&svn_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        revision
    );
    assert!(git2::Repository::open_bare(&bare)
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
}
