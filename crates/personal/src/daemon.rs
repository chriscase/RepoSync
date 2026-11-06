//! Daemon management for personal branch mode.
//!
//! Supports both foreground (interactive) and background (daemonized) modes.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

/// Get the default PID file path.
pub fn pid_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("personal.pid")
}

/// Get the default log file path.
#[allow(dead_code)]
pub fn log_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("personal.log")
}

/// Write the current process PID to the PID file.
pub fn write_pid_file(path: &Path) -> Result<()> {
    let pid = std::process::id();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create PID file directory")?;
    }
    fs::write(path, pid.to_string()).context("failed to write PID file")?;
    info!(pid, path = %path.display(), "wrote PID file");
    Ok(())
}

/// Read the PID from the PID file, if it exists.
pub fn read_pid_file(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path).context("failed to read PID file")?;
    let pid: u32 = contents
        .trim()
        .parse()
        .context("PID file contains invalid data")?;
    Ok(Some(pid))
}

/// Remove the PID file.
pub fn remove_pid_file(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).context("failed to remove PID file")?;
        info!(path = %path.display(), "removed PID file");
    }
    Ok(())
}

/// Check whether a process with the given PID is alive.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 doesn't send a signal, just checks if process exists
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Inspect the recorded PID and liveness without deleting stale evidence.
/// Read/parse errors remain errors for status and diagnostics to report.
pub fn inspect_pid_file(data_dir: &Path) -> Result<Option<(u32, bool)>> {
    let contents = match fs::read_to_string(pid_file_path(data_dir)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to inspect PID file"),
    };
    let pid: u32 = contents
        .trim()
        .parse()
        .context("PID file contains invalid data")?;
    anyhow::ensure!(
        pid != 0 && pid <= i32::MAX as u32,
        "PID file contains invalid process identity"
    );
    Ok(Some((pid, is_process_alive(pid))))
}

/// Check if the daemon is currently running.
/// Legacy cleanup helper; startup and inspection retain stale PID evidence.
#[allow(dead_code)]
pub fn is_running(data_dir: &Path) -> Result<Option<u32>> {
    let pid_path = pid_file_path(data_dir);
    match read_pid_file(&pid_path)? {
        Some(pid) if is_process_alive(pid) => Ok(Some(pid)),
        Some(_stale_pid) => {
            // Stale PID file — process is dead
            remove_pid_file(&pid_path)?;
            Ok(None)
        }
        None => Ok(None),
    }
}

/// Stop the daemon by sending SIGTERM.
pub fn stop_daemon(data_dir: &Path) -> Result<bool> {
    let pid_path = pid_file_path(data_dir);
    match read_pid_file(&pid_path)? {
        Some(pid) if is_process_alive(pid) => {
            info!(pid, "sending SIGTERM to daemon");
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            // Wait briefly for the process to exit
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if !is_process_alive(pid) {
                    remove_pid_file(&pid_path)?;
                    return Ok(true);
                }
            }
            // Process didn't exit in 5 seconds
            anyhow::bail!("daemon (PID {}) did not exit after SIGTERM", pid);
        }
        Some(_stale) => {
            remove_pid_file(&pid_path)?;
            Ok(false) // wasn't running
        }
        None => Ok(false), // wasn't running
    }
}
