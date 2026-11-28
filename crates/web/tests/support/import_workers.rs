use reposync_web::AppState;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::{JoinError, JoinHandle};

// A timeout must keep the worker in the fixture registry for failure cleanup.
pub(super) async fn join_last(state: &AppState, expected: usize) -> Result<(), JoinError> {
    let mut workers = state.import_handles.lock().await;
    assert_eq!(workers.len(), expected);
    let result = tokio::time::timeout(Duration::from_secs(60), workers.last_mut().unwrap())
        .await
        .expect("managed import worker timed out");
    workers.pop();
    result
}

async fn drain(workers: &mut Vec<JoinHandle<()>>, bound: Duration) {
    let _ = tokio::time::timeout(bound, async {
        while let Some(worker) = workers.last_mut() {
            let result = worker.await;
            workers.pop();
            if let Err(error) = result {
                eprintln!("managed import worker ended abnormally: {error}");
            }
        }
    })
    .await;
}

pub(super) async fn cleanup_failed_imports(state: &AppState) {
    // Ask supervised Git/SVN commands to terminate and reap their process
    // group before resorting to aborting a Tokio worker.
    if let Ok(by_repo) = state.repo_import_progress.try_read() {
        for progress in by_repo.values() {
            if let Ok(progress) = progress.try_read() {
                progress.cancel_signal.store(true, Ordering::Release);
            }
        }
    }
    let mut workers = state.import_handles.lock().await;
    drain(&mut workers, Duration::from_secs(10)).await;
    if !workers.is_empty() {
        let aborted = workers.len();
        for worker in workers.iter() {
            worker.abort();
        }
        drain(&mut workers, Duration::from_secs(5)).await;
        eprintln!(
            "requested abort of {aborted} worker(s); {} join(s) still pending",
            workers.len()
        );
    }
}
