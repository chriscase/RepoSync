//! An operator's "sync now" request skips the poll wait once.
//! Fixtures are local temp dirs and `file://` URLs only.
use super::allowed_paths_tests::{capture, due_scheduler, logged, spawned, stored, DueRepo};
use super::*;

fn not_due(id: &'static str) -> (tempfile::TempDir, Scheduler) {
    let (tmp, scheduler) = due_scheduler(&[DueRepo {
        id,
        name: id,
        allowed_paths: None,
        blocked_patterns: None,
    }]);
    let mut repo = stored(&scheduler, id);
    repo.poll_interval_secs = 3600;
    repo.last_sync_at = Some(Utc::now().to_rfc3339());
    scheduler.db.update_repository(&repo).unwrap();
    (tmp, scheduler)
}

async fn tick(scheduler: &Scheduler) -> String {
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let _ = spawned(scheduler).await;
    let log = buf.lock().expect("log buffer").clone();
    log
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pair_that_is_not_due_waits_without_a_request() {
    let id = "manual-none";
    let (_tmp, scheduler) = not_due(id);
    let log = tick(&scheduler).await;
    assert!(logged(&log, "repo not due for sync yet", id), "{log}");
    assert!(!logged(&log, "starting per-repo sync cycle", id), "{log}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_starts_one_cycle_before_the_pair_is_due() {
    let id = "manual-once";
    let (_tmp, scheduler) = not_due(id);
    scheduler.db.request_manual_sync(id).unwrap();
    assert!(scheduler.db.manual_sync_requested(id));

    let log = tick(&scheduler).await;
    assert!(logged(&log, "starting per-repo sync cycle", id), "{log}");
    assert!(
        !scheduler.db.manual_sync_requested(id),
        "the request is used up when the cycle starts"
    );

    // The request does not repeat: put the pair back inside its wait and
    // the next inspection leaves it alone.
    let mut repo = stored(&scheduler, id);
    repo.last_sync_at = Some(Utc::now().to_rfc3339());
    repo.sync_status = "idle".into();
    repo.consecutive_errors = 0;
    scheduler.db.update_repository(&repo).unwrap();
    let log = tick(&scheduler).await;
    assert!(logged(&log, "repo not due for sync yet", id), "{log}");
    assert!(!logged(&log, "starting per-repo sync cycle", id), "{log}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_does_not_override_a_configuration_refusal() {
    let id = "manual-refused";
    let (_tmp, scheduler) = not_due(id);
    let mut repo = stored(&scheduler, id);
    repo.git_branch = "--force".into();
    scheduler.db.update_repository(&repo).unwrap();
    scheduler.db.request_manual_sync(id).unwrap();

    let log = tick(&scheduler).await;
    assert!(logged(&log, "Git branch name is not usable", id), "{log}");
    assert!(!logged(&log, "starting per-repo sync cycle", id), "{log}");
    assert!(
        scheduler.db.manual_sync_requested(id),
        "a request that could not start is kept, not used up"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_request_is_dropped_without_a_cycle() {
    let id = "manual-expired";
    let (_tmp, scheduler) = not_due(id);
    let old = Utc::now()
        - chrono::Duration::seconds(
            reposync_core::db::queries::MANUAL_SYNC_REQUEST_MAX_AGE_SECS + 5,
        );
    scheduler
        .db
        .set_state(&format!("manual_sync_requested_{id}"), &old.to_rfc3339())
        .unwrap();

    let log = tick(&scheduler).await;
    assert!(logged(&log, "repo not due for sync yet", id), "{log}");
    assert!(!logged(&log, "starting per-repo sync cycle", id), "{log}");
    assert_eq!(
        scheduler
            .db
            .get_state(&format!("manual_sync_requested_{id}"))
            .unwrap(),
        None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_request_waits_out_the_gap_after_a_requested_cycle_starts() {
    use reposync_core::db::queries::{ManualSyncRequest, MANUAL_SYNC_MIN_GAP_SECS};
    let id = "manual-gap";
    let (_tmp, scheduler) = not_due(id);
    assert_eq!(
        scheduler.db.request_manual_sync(id).unwrap(),
        ManualSyncRequest::Recorded
    );
    assert_eq!(
        scheduler.db.request_manual_sync(id).unwrap(),
        ManualSyncRequest::AlreadyWaiting
    );

    let log = tick(&scheduler).await;
    assert!(logged(&log, "starting per-repo sync cycle", id), "{log}");

    // Straight after the requested cycle started, a new request is refused
    // and nothing is recorded.
    match scheduler.db.request_manual_sync(id).unwrap() {
        ManualSyncRequest::TooSoon { retry_after_secs } => {
            assert!((1..=MANUAL_SYNC_MIN_GAP_SECS).contains(&retry_after_secs));
        }
        other => panic!("expected TooSoon, got {other:?}"),
    }
    assert!(!scheduler.db.manual_sync_requested(id));

    // Once the gap has passed, a request is accepted again.
    let earlier = Utc::now() - chrono::Duration::seconds(MANUAL_SYNC_MIN_GAP_SECS + 1);
    scheduler
        .db
        .set_state(&format!("manual_sync_started_{id}"), &earlier.to_rfc3339())
        .unwrap();
    assert_eq!(
        scheduler.db.request_manual_sync(id).unwrap(),
        ManualSyncRequest::Recorded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_ordinary_cycle_does_not_start_the_gap() {
    use reposync_core::db::queries::ManualSyncRequest;
    let id = "manual-ordinary";
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id,
        name: id,
        allowed_paths: None,
        blocked_patterns: None,
    }]);
    let log = tick(&scheduler).await;
    assert!(logged(&log, "starting per-repo sync cycle", id), "{log}");
    assert_eq!(
        scheduler
            .db
            .get_state(&format!("manual_sync_started_{id}"))
            .unwrap(),
        None
    );
    assert_eq!(
        scheduler.db.request_manual_sync(id).unwrap(),
        ManualSyncRequest::Recorded
    );
}
