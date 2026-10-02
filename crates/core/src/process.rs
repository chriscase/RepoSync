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

/// The child or its output-producing descendants could not be proven stopped.
/// Callers must hold the operation, irrespective of a cancellation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnconfirmedCleanup {
    GroupTermination,
    OutputCompletion,
}

impl std::fmt::Display for UnconfirmedCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "import command cleanup unconfirmed ({self:?})")
    }
}

impl std::error::Error for UnconfirmedCleanup {}

#[derive(Debug)]
struct ConfirmedStop;

impl std::fmt::Display for ConfirmedStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("import command stopped with confirmed cleanup")
    }
}

impl std::error::Error for ConfirmedStop {}

pub fn cleanup_unconfirmed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<UnconfirmedCleanup>())
}

pub fn confirmed_cancelled(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
        && error
            .get_ref()
            .is_some_and(|source| source.is::<ConfirmedStop>())
}

fn stop_result(
    killed: io::Result<()>,
    reaped: Result<io::Result<Output>, tokio::time::error::Elapsed>,
    kind: io::ErrorKind,
) -> io::Result<Output> {
    if killed.is_err() {
        return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
    }
    if !matches!(reaped, Ok(Ok(_))) {
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    Err(io::Error::new(kind, ConfirmedStop))
}

#[cfg(feature = "reliability-fixture")]
async fn fixture_cleanup_fault(command: &Command) -> io::Result<()> {
    use std::io::Write as _;
    let Some(root) = std::env::var_os("REPOSYNC_FIXTURE_ROOT") else {
        return Ok(());
    };
    let Some(dir) = std::env::var_os("REPOSYNC_IMPORT_FAULT_DIR") else {
        return Ok(());
    };
    let root_path = std::path::Path::new(&root);
    let root = root_path.canonicalize()?;
    let dir = std::path::Path::new(&dir).canonicalize()?;
    if !dir.starts_with(&root) {
        return Err(io::Error::other("fixture fault escaped sealed root"));
    }
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let program = command.as_std().get_program().to_string_lossy();
    let kind = if program.contains("svn") || args.first().is_some_and(|a| a.contains("svn")) {
        "svn"
    } else if program.contains("git") || args.first().is_some_and(|a| a.contains("git")) {
        "git"
    } else {
        return Ok(());
    };
    let stage = [
        "clone",
        "ls-remote",
        "info",
        "log",
        "diff",
        "export",
        "apply",
        "push",
        "commit",
    ]
    .into_iter()
    .find(|token| args.iter().any(|arg| arg == token))
    .map(|token| format!("{kind}-{token}"));
    let Some(stage) = stage else {
        return Ok(());
    };
    let scoped = command
        .as_std()
        .get_current_dir()
        .is_some_and(|p| p.starts_with(&root) || p.starts_with(root_path))
        || args.iter().any(|arg| {
            arg.contains(root.to_string_lossy().as_ref())
                || arg.contains(root_path.to_string_lossy().as_ref())
        });
    if !scoped {
        return Err(io::Error::other(
            "fixture fault command escaped sealed root",
        ));
    }
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("commands.log"))?,
        "{stage}"
    )?;
    if std::env::var("REPOSYNC_IMPORT_FAULT_STAGE").ok().as_deref() == Some(stage.as_str()) {
        std::fs::write(dir.join("fault.ready"), stage.as_bytes())?;
        let released = tokio::time::timeout(Duration::from_secs(15), async {
            while !dir.join("fault.release").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if released.is_err() {
            return Err(io::Error::other("fixture fault barrier timed out"));
        }
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    Ok(())
}

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
        return Err(io::Error::new(io::ErrorKind::Interrupted, ConfirmedStop));
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.rewind()?;
    command.stdin(Stdio::from(file));
    run(command, timeout, cancel).await
}

/// Keep import Git/LFS children on the command's workdir instead of an
/// ambient checkout. CI runs `git lfs install` globally; without this, a
/// supervised `git lfs version` / clone can inherit `GIT_DIR` or smudge
/// against the RepoSync workspace LFS remote and never finish.
fn isolate_import_git_env(command: &mut Command) {
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_LFS_SKIP_SMUDGE", "1");
    command.env_remove("GIT_DIR");
    command.env_remove("GIT_WORK_TREE");
    command.env_remove("GIT_OBJECT_DIRECTORY");
    command.env_remove("GIT_COMMON_DIR");
    command.env_remove("GIT_INDEX_FILE");
}

pub async fn run(
    mut command: Command,
    timeout: Duration,
    cancel: Option<&Arc<AtomicBool>>,
) -> io::Result<Output> {
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, ConfirmedStop));
    }
    #[cfg(feature = "reliability-fixture")]
    fixture_cleanup_fault(&command).await?;
    isolate_import_git_env(&mut command);
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
            output = &mut wait => return output.map_err(|_| io::Error::other(UnconfirmedCleanup::OutputCompletion)),
            _ = &mut deadline => {
                let killed = kill_group(pid);
                let reaped = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                return stop_result(killed, reaped, io::ErrorKind::TimedOut);
            }
            _ = tokio::time::sleep(Duration::from_millis(100)), if cancel.is_some() => {
                if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                    let killed = kill_group(pid);
                    let reaped = tokio::time::timeout(Duration::from_secs(5), &mut wait).await;
                    return stop_result(killed, reaped, io::ErrorKind::Interrupted);
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
    use std::os::unix::process::ExitStatusExt;

    use crate::errors::{GitError, SvnError};

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
    async fn cleanup_certainty_survives_git_svn_wrappers_with_and_without_cancel() {
        for requested_cancel in [false, true] {
            let confirmed_kind = if requested_cancel {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::TimedOut
            };
            for cause in [
                UnconfirmedCleanup::GroupTermination,
                UnconfirmedCleanup::OutputCompletion,
            ] {
                let reaped = if cause == UnconfirmedCleanup::OutputCompletion {
                    tokio::time::timeout(
                        Duration::ZERO,
                        std::future::pending::<io::Result<Output>>(),
                    )
                    .await
                } else {
                    Ok(Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }))
                };
                let killed = if cause == UnconfirmedCleanup::GroupTermination {
                    Err(io::Error::other("kill failed"))
                } else {
                    Ok(())
                };
                let error = stop_result(killed, reaped, confirmed_kind).unwrap_err();
                assert!(cleanup_unconfirmed(&error));
                let git = GitError::IoError(io::Error::other(cause));
                let svn = SvnError::IoError(io::Error::other(cause));
                assert!(matches!(git, GitError::IoError(ref e) if cleanup_unconfirmed(e)));
                assert!(matches!(svn, SvnError::IoError(ref e) if cleanup_unconfirmed(e)));
            }
            let confirmed = stop_result(
                Ok(()),
                Ok(Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })),
                confirmed_kind,
            )
            .unwrap_err();
            assert_eq!(confirmed.kind(), confirmed_kind);
            assert!(!cleanup_unconfirmed(&confirmed));
            assert_eq!(confirmed_cancelled(&confirmed), requested_cancel);
            assert!(!confirmed_cancelled(&io::Error::new(
                io::ErrorKind::Interrupted,
                "untyped I/O interruption"
            )));
        }
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
