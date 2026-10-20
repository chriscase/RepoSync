//! RS-10: poll-interval maintenance must not divide/modulo-panic, and cadence
//! stays bounded near ten minutes (or once per tick when the interval is longer).
use super::*;
use reposync_core::config::IdentityConfig;
use std::sync::atomic::Ordering;

const VALID_POLL_INTERVALS_SECS: [u64; 5] = [1, 30, 600, 601, 3600];
const HORIZON_SECS: u64 = 7_200;

struct CadenceSample {
    poll_interval_secs: u64,
    sync_ticks: u64,
    maintenance_runs: u64,
    max_maintenance_gap_secs: u64,
}

/// Walk whole-second poll ticks over a horizon using the same `maintenance_due`
/// helper as the run loop. Time is virtual: no wall-clock sleeps.
fn sample_cadence(poll_interval_secs: u64, horizon_secs: u64) -> CadenceSample {
    let poll: u64 = poll_interval_secs.max(1);
    let mut sync_ticks: u64 = 0;
    let mut maintenance_runs: u64 = 0;
    let mut last_maint: u64 = 0;
    let mut max_gap: u64 = 0;
    let mut elapsed: u64 = 0;
    loop {
        match elapsed.checked_add(poll) {
            Some(next) if next <= horizon_secs => elapsed = next,
            _ => break,
        }
        sync_ticks += 1;
        let since_last = elapsed - last_maint;
        if maintenance_due(Duration::from_secs(since_last)) {
            max_gap = max_gap.max(since_last);
            last_maint = elapsed;
            maintenance_runs += 1;
        }
    }
    if maintenance_runs == 0 {
        max_gap = horizon_secs.saturating_sub(last_maint);
    }
    CadenceSample {
        poll_interval_secs,
        sync_ticks,
        maintenance_runs,
        max_maintenance_gap_secs: max_gap,
    }
}

#[test]
fn historical_tick_divisor_is_zero_for_intervals_above_ten_minutes() {
    for poll in [601_u64, 3600] {
        let old_ticks = 600 / poll.max(1);
        assert_eq!(
            old_ticks, 0,
            "documents the pre-fix truncation to a zero divisor at poll={poll}"
        );
        assert!(
            1u64.checked_rem(old_ticks).is_none(),
            "checked_rem(0) is the panic the run loop used to hit"
        );
    }
}

#[test]
fn maintenance_due_is_false_until_ten_minutes() {
    assert!(!maintenance_due(Duration::from_secs(0)));
    assert!(!maintenance_due(Duration::from_secs(599)));
    assert!(maintenance_due(Duration::from_secs(600)));
    assert!(maintenance_due(Duration::from_secs(601)));
    assert!(maintenance_due(Duration::from_secs(3600)));
}

#[test]
fn cadence_stays_bounded_for_valid_poll_intervals() {
    for poll in VALID_POLL_INTERVALS_SECS {
        let sample = sample_cadence(poll, HORIZON_SECS);
        assert!(sample.sync_ticks > 0, "poll={poll}: sync must keep running");
        assert!(
            sample.maintenance_runs > 0,
            "poll={poll}: maintenance must run at least once in {HORIZON_SECS}s"
        );
        if poll >= MAINTENANCE_CADENCE.as_secs() {
            assert_eq!(
                sample.maintenance_runs, sample.sync_ticks,
                "poll={poll}: interval exceeds ten minutes, so maintenance runs every tick"
            );
            assert_eq!(
                sample.max_maintenance_gap_secs, poll,
                "poll={poll}: gap between maintenance passes is the poll interval"
            );
        } else {
            assert_eq!(
                sample.maintenance_runs,
                HORIZON_SECS / MAINTENANCE_CADENCE.as_secs(),
                "poll={poll}: about one maintenance pass per ten minutes"
            );
            assert!(
                sample.max_maintenance_gap_secs >= MAINTENANCE_CADENCE.as_secs(),
                "poll={poll}: gap {} shorter than cadence",
                sample.max_maintenance_gap_secs
            );
            assert!(
                sample.max_maintenance_gap_secs < MAINTENANCE_CADENCE.as_secs() + poll,
                "poll={poll}: gap {} exceeds ten minutes by a full poll tick",
                sample.max_maintenance_gap_secs
            );
        }
        assert_eq!(sample.poll_interval_secs, poll);
    }
}

fn scheduler_with_poll(tmp: &tempfile::TempDir, poll: Duration) -> Scheduler {
    let config_file = tmp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(
            "[daemon]\ndata_dir = \"{}\"\nmemory_limit_mb = 0\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/fixture\"\n",
            tmp.path().display()
        ),
    )
    .unwrap();
    let config = AppConfig::load_from_file(&config_file).unwrap();
    let db_path = tmp.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let dummy_git = tmp.path().join("dummy-git");
    assert!(std::process::Command::new("git")
        .args(["init", dummy_git.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    let (_, rx) = mpsc::channel(1);
    let (ws, _) = broadcast::channel(1);
    Scheduler::new(
        Arc::new(engine),
        poll,
        rx,
        ws,
        Arc::new(RwLock::new(ImportProgress::default())),
        db,
        config,
    )
}

async fn wait_for_poll_ticks(stats: &SchedulerStats, min: u64, poll_secs: u64) {
    for _ in 0..64 {
        if stats.poll_ticks.load(Ordering::SeqCst) >= min {
            return;
        }
        // Paused time: 1ms auto-advances virtually and lets the scheduler
        // task run. 64ms stays well below the smallest poll interval (1s).
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!(
        "poll={poll_secs}: expected at least {min} ticks, have {}",
        stats.poll_ticks.load(Ordering::SeqCst)
    );
}

/// Drive the production tick handler on paused tokio time. No enabled repos,
/// so sync work is a no-op; we assert the handler keeps ticking and that
/// maintenance follows the monotonic deadline.
#[tokio::test(start_paused = true)]
async fn paused_ticks_keep_syncing_and_bound_maintenance() {
    for poll in VALID_POLL_INTERVALS_SECS {
        let tmp = tempfile::tempdir().unwrap();
        let interval = Duration::from_secs(poll);
        let scheduler = scheduler_with_poll(&tmp, interval);
        let start = time::Instant::now();
        let mut last_maintenance_at = start;

        tokio::time::advance(interval).await;
        scheduler
            .handle_interval_tick(&mut last_maintenance_at)
            .await;
        tokio::time::advance(interval).await;
        scheduler
            .handle_interval_tick(&mut last_maintenance_at)
            .await;

        let after_two = time::Instant::now()
            .checked_duration_since(start)
            .unwrap_or(Duration::ZERO);
        if after_two < MAINTENANCE_CADENCE {
            tokio::time::advance(MAINTENANCE_CADENCE - after_two).await;
            scheduler
                .handle_interval_tick(&mut last_maintenance_at)
                .await;
        }

        let ticks = scheduler.stats.poll_ticks.load(Ordering::SeqCst);
        let maint = scheduler.stats.maintenance_runs.load(Ordering::SeqCst);
        assert!(
            ticks >= 2,
            "poll={poll}: scheduler must keep polling, got {ticks} ticks"
        );
        assert!(
            maint >= 1,
            "poll={poll}: maintenance must have run by the ten-minute bound, got {maint}"
        );
        if poll >= MAINTENANCE_CADENCE.as_secs() {
            assert_eq!(
                maint, ticks,
                "poll={poll}: maintenance once per tick when interval exceeds ten minutes"
            );
        } else {
            assert!(
                maint <= ticks,
                "poll={poll}: maintenance must not outrun poll ticks"
            );
            let elapsed = time::Instant::now()
                .checked_duration_since(start)
                .unwrap_or(Duration::ZERO);
            assert!(
                elapsed >= MAINTENANCE_CADENCE,
                "poll={poll}: first maintenance requires ~10 minutes of monotonic time"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn run_loop_survives_valid_poll_intervals_without_panic() {
    for poll_secs in VALID_POLL_INTERVALS_SECS {
        let tmp = tempfile::tempdir().unwrap();
        let poll = Duration::from_secs(poll_secs);
        let mut scheduler = scheduler_with_poll(&tmp, poll);
        let stats = scheduler.stats.clone();
        let shutdown = Arc::new(Notify::new());
        let shutdown_run = shutdown.clone();
        let handle = tokio::spawn(async move {
            scheduler.run(shutdown_run).await;
        });

        // Let run() consume the immediate first interval tick and block.
        tokio::time::sleep(Duration::from_millis(1)).await;

        tokio::time::advance(poll).await;
        wait_for_poll_ticks(&stats, 1, poll_secs).await;
        tokio::time::advance(poll).await;
        wait_for_poll_ticks(&stats, 2, poll_secs).await;

        shutdown.notify_one();
        handle
            .await
            .unwrap_or_else(|e| panic!("poll={poll_secs}: scheduler task panicked: {e}"));

        assert!(
            stats.poll_ticks.load(Ordering::SeqCst) >= 2,
            "poll={poll_secs}: run loop must keep ticking after start"
        );
        if poll_secs >= MAINTENANCE_CADENCE.as_secs() {
            assert!(
                stats.maintenance_runs.load(Ordering::SeqCst) >= 2,
                "poll={poll_secs}: long interval must maintain at least once per tick"
            );
        }
    }
}
