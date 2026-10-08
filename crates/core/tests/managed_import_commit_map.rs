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
