//! Signal listeners must exist before the spawned receiver task can be polled.
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct ChildOwner(Child);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn unix_shutdown_signals_are_registered_before_receiver_task_polling() {
    let signal = std::env::var("REPOSYNC_SIGNAL_READINESS_CHILD").ok();
    if let Some(signal) = signal.as_deref() {
        assert!(matches!(signal, "TERM" | "INT"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let entered = runtime.enter();
        let shutdown = reposync_personal::signals::setup_signal_handlers();
        // No task has been polled on this current-thread runtime. The default
        // OS disposition would terminate us if registration were deferred.
        assert!(Command::new("kill")
            .args([&format!("-{signal}"), &std::process::id().to_string()])
            .status()
            .unwrap()
            .success());
        drop(entered);
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !reposync_personal::signals::is_shutdown_requested(&shutdown) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        assert!(reposync_personal::signals::is_shutdown_requested(&shutdown));
        return;
    }

    for signal in ["TERM", "INT"] {
        let directory = tempfile::tempdir().unwrap();
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        let mut child = ChildOwner(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "unix_shutdown_signals_are_registered_before_receiver_task_polling",
                ])
                .env("REPOSYNC_SIGNAL_READINESS_CHILD", signal)
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "{signal}: signal receiver did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            status.success(),
            "{signal}: {}\n{}",
            std::fs::read_to_string(stdout).unwrap(),
            std::fs::read_to_string(stderr).unwrap()
        );
    }
}
