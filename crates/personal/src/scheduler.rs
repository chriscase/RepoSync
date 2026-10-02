//! Polling scheduler for the personal sync daemon.
//!
//! RS-C07 (#64 ownership-before-mutable-prep): NOT RUN here — personal mode is
//! a single-repo CLI daemon without per-repo `busy` slots or team import/sync
//! prep helpers; the team scheduler and web import paths own that contract.

use std::time::Duration;

use anyhow::Result;
use tracing::{error, info};

use crate::engine::PersonalSyncEngine;
use crate::signals::{is_shutdown_requested, ShutdownFlag};

/// Run the sync engine in a polling loop.
pub async fn run_polling_loop(
    engine: &PersonalSyncEngine,
    poll_interval: Duration,
    shutdown: ShutdownFlag,
) -> Result<()> {
    info!(
        interval_secs = poll_interval.as_secs(),
        "starting polling loop"
    );

    loop {
        if is_shutdown_requested(&shutdown) {
            info!("shutdown requested, exiting polling loop");
            break;
        }

        match engine.run_cycle().await {
            Ok(stats) => {
                if stats.svn_to_git_count > 0 || stats.git_to_svn_count > 0 {
                    info!(
                        svn_to_git = stats.svn_to_git_count,
                        git_to_svn = stats.git_to_svn_count,
                        prs = stats.prs_processed,
                        "sync cycle completed with changes"
                    );
                }
            }
            Err(e) => {
                error!(error = %e, "sync cycle failed");
            }
        }

        // Sleep with early exit on shutdown
        let sleep_step = Duration::from_secs(1);
        let mut slept = Duration::ZERO;
        while slept < poll_interval {
            if is_shutdown_requested(&shutdown) {
                info!("shutdown requested during sleep, exiting");
                return Ok(());
            }
            tokio::time::sleep(sleep_step).await;
            slept += sleep_step;
        }
    }

    Ok(())
}
