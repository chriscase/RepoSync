//! Graceful shutdown signal handling for the personal sync daemon.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::info;

/// Shared shutdown flag checked by the scheduler loop.
pub type ShutdownFlag = Arc<AtomicBool>;

/// Create a new shutdown flag and register OS signal handlers.
///
/// On SIGTERM or SIGINT (Ctrl+C), the flag is set to `true`.
pub fn setup_signal_handlers() -> ShutdownFlag {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = flag.clone();

    // Register both Unix listeners before callers publish their ready PID. A
    // spawned task may not be polled before the first shutdown signal arrives.
    #[cfg(unix)]
    let (mut sigint, mut sigterm) = {
        use tokio::signal::unix::{signal, SignalKind};
        (
            signal(SignalKind::interrupt()).expect("failed to register SIGINT handler"),
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler"),
        )
    };

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = sigint.recv() => {
                    info!("received SIGINT (Ctrl+C), initiating shutdown");
                }
                _ = sigterm.recv() => {
                    info!("received SIGTERM, initiating shutdown");
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for Ctrl+C");
            info!("received Ctrl+C, initiating shutdown");
        }

        flag_clone.store(true, Ordering::SeqCst);
    });

    flag
}

/// Check whether the shutdown flag has been set.
pub fn is_shutdown_requested(flag: &ShutdownFlag) -> bool {
    flag.load(Ordering::SeqCst)
}
