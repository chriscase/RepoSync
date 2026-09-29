//! Bounded child supervision for import commands. Unix children get their own
//! process group so cancellation also stops helper descendants.

use std::io;
use std::io::{Seek, Write};
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Fixture-scoped replacement for import Git commands. It cannot select a
/// path outside the disposable fixture and is absent from normal builds.
pub fn import_git_command() -> io::Result<Command> {
    #[cfg(feature = "reliability-fixture")]
    if let Some(binary) = std::env::var_os("REPOSYNC_IMPORT_GIT_BINARY") {
        let root = std::env::var_os("REPOSYNC_FIXTURE_ROOT")
            .ok_or_else(|| io::Error::other("fixture Git override lacks sealed root"))?;
        let root = std::path::Path::new(&root).canonicalize()?;
        let binary = std::path::Path::new(&binary).canonicalize()?;
        if !binary.starts_with(root) {
            return Err(io::Error::other("fixture Git override escaped sealed root"));
        }
        // Fixture tmpfs may deny direct script execution. The shell reads the
        // sealed script; the real Git CLI still executes inside that script.
        let mut command = Command::new("sh");
        command.arg(binary);
        return Ok(command);
    }
    Ok(Command::new("git"))
}

/// Stage patch bytes in an anonymous regular file before spawning the child.
/// This removes the unbounded pipe-stdin write: a child that never reads
/// standard input cannot block delivery or prevent cancellation.
pub async fn run_with_input(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
    cancel: Option<&Arc<AtomicBool>>,
) -> io::Result<Output> {
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "import cancellation requested",
        ));
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.rewind()?;
    command.stdin(Stdio::from(file));
    run(command, timeout, cancel).await
}

pub async fn run(
    mut command: Command,
    timeout: Duration,
    cancel: Option<&Arc<AtomicBool>>,
) -> io::Result<Output> {
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "import cancellation requested",
        ));
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let child = command.spawn()?;
    let pid = child.id();
    let mut wait = Box::pin(child.wait_with_output());
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            output = &mut wait => return output,
            _ = &mut deadline => {
                let killed = kill_group(pid);
                let reaped = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                if killed.is_err() || !matches!(reaped, Ok(Ok(_))) {
                    return Err(io::Error::other("import command quiescence unconfirmed after timeout"));
                }
                return Err(io::Error::new(io::ErrorKind::TimedOut, "import command timed out"));
            }
            _ = tokio::time::sleep(Duration::from_millis(100)), if cancel.is_some() => {
                if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                    let killed = kill_group(pid);
                    let reaped = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                    if killed.is_err() || !matches!(reaped, Ok(Ok(_))) {
                        return Err(io::Error::other("import command quiescence unconfirmed after cancellation"));
                    }
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "import command cancelled"));
                }
            }
        }
    }
}

fn kill_group(pid: Option<u32>) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // A group was allocated for this exact child before spawn.
        if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn stopped(pid: i32) -> bool {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            // A killed grandchild can remain a zombie until the container's
            // PID 1 reaps it. It cannot execute or retain open descriptors.
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                return stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with("Z ") || rest.starts_with("X "));
            }
        }
        false
    }

    #[tokio::test]
    async fn cancelled_child_and_descendant_stop() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("descendant.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$PID_FILE\"; wait")
            .env("PID_FILE", &marker);
        let signal = Arc::new(AtomicBool::new(false));
        let running = tokio::spawn({
            let signal = signal.clone();
            async move { run(command, Duration::from_secs(30), Some(&signal)).await }
        });
        let descendant: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&marker) {
                    break pid.trim().parse().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        signal.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(7), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if stopped(descendant) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn staged_patch_input_cannot_block_cancellation_of_nonreader() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("nonreader-descendant.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$PID_FILE\"; wait")
            .env("PID_FILE", &marker);
        let signal = Arc::new(AtomicBool::new(false));
        // Far larger than a normal pipe buffer. The child never reads stdin.
        let input = vec![b'x'; 2 * 1024 * 1024];
        let running = tokio::spawn({
            let signal = signal.clone();
            async move { run_with_input(command, &input, Duration::from_secs(30), Some(&signal)).await }
        });
        let descendant: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&marker) {
                    break pid.trim().parse().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        signal.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(7), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if stopped(descendant) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
}
