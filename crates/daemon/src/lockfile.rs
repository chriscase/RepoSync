//! Singleton lockfile to prevent multiple daemon instances.
//!
//! Uses an exclusive file lock (`flock`) on `{data_dir}/reposync.lock`.
//! The lock is held for the lifetime of the returned [`LockGuard`], which
//! keeps the file descriptor open. When the process exits — normally, via
//! signal, or even SIGKILL — the OS releases the lock automatically.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use tracing::{info, warn};

/// Guard that holds the exclusive lock. Drop releases the lock.
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        info!(path = %self.path.display(), "releasing singleton lock");
    }
}

/// Acquire an exclusive lock on `{data_dir}/reposync.lock`.
///
/// Returns a [`LockGuard`] that must be held for the daemon's lifetime.
/// If another instance already holds the lock, returns an error with the
/// other process's PID.
pub fn acquire(data_dir: &Path) -> Result<LockGuard, String> {
    let lock_path = data_dir.join("reposync.lock");

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("failed to open lock file {}: {}", lock_path.display(), e))?;

    // Try to acquire exclusive lock (non-blocking)
    match file.try_lock_exclusive() {
        Ok(()) => {
            // Lock acquired — write our PID
            write_pid(&file, &lock_path)?;
            Ok(LockGuard {
                _file: file,
                path: lock_path,
            })
        }
        Err(_) => {
            // Lock held by another process — read its PID
            let other_pid = read_pid(&file);

            // Check if that process is still alive
            if let Some(pid) = other_pid {
                if is_process_alive(pid) {
                    return Err(format!(
                        "Another RepoSync daemon is already running (PID {}). \
                         Lock file: {}",
                        pid,
                        lock_path.display()
                    ));
                }

                // Process is dead — stale lock. The OS should have released
                // the flock when the process died, so try again.
                warn!(
                    stale_pid = pid,
                    "detected stale lock file (PID {} is not running), retrying",
                    pid
                );
                match file.try_lock_exclusive() {
                    Ok(()) => {
                        write_pid(&file, &lock_path)?;
                        Ok(LockGuard {
                            _file: file,
                            path: lock_path,
                        })
                    }
                    Err(e) => Err(format!(
                        "failed to acquire lock after stale detection: {}",
                        e
                    )),
                }
            } else {
                Err(format!(
                    "Another RepoSync daemon appears to be running \
                     (could not read PID from lock file). Lock file: {}",
                    lock_path.display()
                ))
            }
        }
    }
}

fn write_pid(file: &File, path: &Path) -> Result<(), String> {
    // Truncate and write current PID
    file.set_len(0)
        .map_err(|e| format!("failed to truncate lock file: {}", e))?;
    let mut f = file;
    write!(f, "{}", std::process::id())
        .map_err(|e| format!("failed to write PID to {}: {}", path.display(), e))?;
    f.flush()
        .map_err(|e| format!("failed to flush lock file: {}", e))?;
    Ok(())
}

fn read_pid(file: &File) -> Option<u32> {
    let mut contents = String::new();
    let mut f = file;
    // Seek to beginning before reading
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(0)).ok()?;
    f.read_to_string(&mut contents).ok()?;
    contents.trim().parse::<u32>().ok()
}

fn is_process_alive(#[allow(unused)] pid: u32) -> bool {
    // On Unix, kill(pid, 0) checks if the process exists without sending a signal
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, assume alive (conservative)
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_creates_lock_with_pid() {
        let dir = tempfile::tempdir().unwrap();
        let guard = acquire(dir.path()).unwrap();

        let lock_path = dir.path().join("reposync.lock");
        assert!(lock_path.exists());

        let contents = std::fs::read_to_string(&lock_path).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());

        drop(guard); // release lock
    }

    #[test]
    fn test_acquire_blocks_second_instance() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = acquire(dir.path()).unwrap();

        // Second acquire should fail
        let result = acquire(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("already running"), "Error was: {}", err);
    }

    #[test]
    fn test_acquire_after_drop_succeeds() {
        let dir = tempfile::tempdir().unwrap();

        {
            let _guard = acquire(dir.path()).unwrap();
            // guard drops here
        }

        // Should succeed since lock was released
        let _guard2 = acquire(dir.path()).unwrap();
    }
}
