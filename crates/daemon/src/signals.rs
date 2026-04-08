//! Signal handling for graceful daemon shutdown.
//!
//! Listens for SIGTERM and SIGINT on Unix platforms and Ctrl+C on all
//! platforms. SIGHUP is explicitly **ignored** so the daemon survives
//! SSH session termination when it was launched interactively (e.g. via
//! `nohup ./reposync-daemon &`). Without this, systemd-logind will send
//! SIGHUP to the entire user session when the SSH connection closes and
//! the daemon would die silently. Running under a proper systemd unit or
//! `setsid` would also solve this, but explicit SIGHUP handling is a
//! belt-and-braces approach that works regardless of how the daemon is
//! launched.
//!
//! When a termination signal is received, the async function returns so
//! the caller can begin its shutdown sequence.

use tracing::info;
#[cfg(unix)]
use tracing::warn;

/// Install a persistent SIGHUP handler that logs but does not exit.
/// Must be called early in `main` so the handler is registered before
/// any SSH session cleanup can occur.
#[cfg(unix)]
pub fn ignore_sighup() {
    tokio::spawn(async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(mut stream) => {
                while stream.recv().await.is_some() {
                    warn!(
                        "received SIGHUP — ignoring (daemon continues running). \
                         This is expected when launched via nohup over SSH."
                    );
                }
            }
            Err(e) => warn!("failed to install SIGHUP handler: {}", e),
        }
    });
}

#[cfg(not(unix))]
pub fn ignore_sighup() {}

/// Wait for a shutdown signal (SIGTERM, SIGINT, or Ctrl+C).
///
/// Note: SIGHUP is explicitly **not** included here — see `ignore_sighup`.
///
/// This function resolves once any termination signal is received.
pub async fn wait_for_shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("received SIGINT (Ctrl+C)");
        }
        _ = terminate => {
            info!("received SIGTERM");
        }
    }
}
