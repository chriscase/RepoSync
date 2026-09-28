//! Bounded child supervision for import commands. Unix children get their own
//! process group so cancellation also stops helper descendants.

use std::io;
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

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
                kill_group(pid);
                let _ = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "import command timed out"));
            }
            _ = tokio::time::sleep(Duration::from_millis(100)), if cancel.is_some() => {
                if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                    kill_group(pid);
                    let _ = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "import command cancelled"));
                }
            }
        }
    }
}

fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // A group was allocated for this exact child before spawn.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
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
}
