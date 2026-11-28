//! A pair row saved before branch names were checked must not reach Git.
//! Fixtures are local temp dirs and `file://` URLs only.
use super::allowed_paths_tests::{capture, due_scheduler, logged, spawned, stored, DueRepo};

fn due(id: &'static str) -> DueRepo {
    DueRepo {
        id,
        name: id,
        allowed_paths: None,
        blocked_patterns: None,
    }
}

async fn assert_branch_refused(id: &'static str, branch: &str) {
    let (_tmp, scheduler) = due_scheduler(&[due(id)]);
    let mut repo = stored(&scheduler, id);
    repo.git_branch = branch.into();
    scheduler.db.update_repository(&repo).unwrap();

    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();

    assert!(
        logged(&log, "Git branch name is not usable", id),
        "{branch:?} must be refused: {log}"
    );
    assert!(
        !logged(&log, "starting per-repo sync cycle", id),
        "{branch:?}"
    );
    assert!(
        !logged(&log, "resolved credentials via chain", id),
        "{branch:?} must be refused before credentials are read"
    );
    assert_eq!(
        spawned(&scheduler)
            .await
            .expect("refused repository must not spawn a worker"),
        0,
        "{branch:?}"
    );
    let after = stored(&scheduler, id);
    assert_eq!(after.sync_status, "idle");
    assert_eq!(after.consecutive_errors, 0);
    assert_eq!(after.last_svn_rev, 2);
    assert_eq!(after.last_git_sha, "verified-old");
}

#[tokio::test(flavor = "current_thread")]
async fn a_branch_that_reads_as_an_option_is_refused() {
    assert_branch_refused("branch-option", "--upload-pack=touch").await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_empty_branch_is_refused() {
    assert_branch_refused("branch-empty", "").await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_full_ref_is_refused() {
    assert_branch_refused("branch-full-ref", "refs/heads/main").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_branch_still_starts_its_cycle() {
    let id = "branch-valid";
    let (_tmp, scheduler) = due_scheduler(&[due(id)]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let _ = spawned(&scheduler).await;
    let log = buf.lock().expect("log buffer").clone();
    assert!(!logged(&log, "Git branch name is not usable", id), "{log}");
    assert!(logged(&log, "resolved credentials via chain", id), "{log}");
}
