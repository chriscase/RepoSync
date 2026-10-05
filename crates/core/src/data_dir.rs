//! Exclusive data-directory owner via the daemon lockfile and durable lease.
//!
//! Mixed-version writer policy: one process owns `{data_dir}/reposync.lock`
//! with an exclusive `flock` and a durable writer-fence epoch in `kv_state`.
//! A second daemon on the same host is blocked by `flock`; a second host on
//! shared storage is refused or fenced by the epoch lease checked at finalize.
//!
//! Ordinary daemon startup acquires this owner. CLI/personal
//! `Database::new`/`initialize` paths do not; wiring those writers is a later
//! slice.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::errors::DatabaseError;

/// Guard that holds the exclusive lock. Drop releases the lock.
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
    path: PathBuf,
}

/// Combined local flock and durable cross-host writer fence.
pub struct DataDirOwner {
    pub lock: LockGuard,
    pub fence: crate::writer_fence::WriterFenceGuard,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        info!(path = %self.path.display(), "releasing singleton lock");
    }
}

/// Acquire an exclusive lock on `{data_dir}/reposync.lock`.
///
/// Returns a [`LockGuard`] that must be held for the daemon's lifetime.
/// If another instance already holds the lock, returns [`DatabaseError::DataDirInUse`]
/// with the other process's PID when it can be read.
pub fn acquire(data_dir: &Path) -> Result<LockGuard, DatabaseError> {
    let lock_path = data_dir.join("reposync.lock");

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            DatabaseError::Other(format!(
                "failed to open lock file {}: {}",
                lock_path.display(),
                e
            ))
        })?;

    match try_lock_exclusive(&file) {
        Ok(()) => {
            write_pid(&file, &lock_path)?;
            Ok(LockGuard {
                _file: file,
                path: lock_path,
            })
        }
        Err(_) => {
            let other_pid = read_pid(&file);

            if let Some(pid) = other_pid {
                if is_process_alive(pid) {
                    return Err(in_use(pid, &lock_path));
                }

                warn!(
                    stale_pid = pid,
                    "detected stale lock file (PID {} is not running), retrying", pid
                );
                match try_lock_exclusive(&file) {
                    Ok(()) => {
                        write_pid(&file, &lock_path)?;
                        Ok(LockGuard {
                            _file: file,
                            path: lock_path,
                        })
                    }
                    Err(_) => Err(DatabaseError::Other(
                        "failed to acquire lock after stale detection".into(),
                    )),
                }
            } else {
                Err(DatabaseError::DataDirInUse(format!(
                    "Another RepoSync daemon appears to be running \
                     (could not read PID from lock file). Lock file: {}",
                    lock_path.display()
                )))
            }
        }
    }
}

fn in_use(pid: u32, lock_path: &Path) -> DatabaseError {
    DatabaseError::DataDirInUse(format!(
        "Another RepoSync daemon is already running (PID {}). \
         Lock file: {}",
        pid,
        lock_path.display()
    ))
}

fn try_lock_exclusive(file: &File) -> Result<(), ()> {
    // SAFETY: `file` is an open descriptor we own for the lockfile lifetime;
    // flock(LOCK_EX|LOCK_NB) does not take ownership of the fd.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn write_pid(file: &File, path: &Path) -> Result<(), DatabaseError> {
    file.set_len(0)
        .map_err(|e| DatabaseError::Other(format!("failed to truncate lock file: {}", e)))?;
    let mut f = file;
    write!(f, "{}", std::process::id()).map_err(|e| {
        DatabaseError::Other(format!("failed to write PID to {}: {}", path.display(), e))
    })?;
    f.flush()
        .map_err(|e| DatabaseError::Other(format!("failed to flush lock file: {}", e)))?;
    Ok(())
}

fn read_pid(file: &File) -> Option<u32> {
    let mut contents = String::new();
    let mut f = file;
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(0)).ok()?;
    f.read_to_string(&mut contents).ok()?;
    contents.trim().parse::<u32>().ok()
}

fn is_process_alive(#[allow(unused)] pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) only probes existence; it does not deliver a signal.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
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

        drop(guard);
    }

    #[test]
    fn test_acquire_blocks_second_instance() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = acquire(dir.path()).unwrap();

        let result = acquire(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already running"), "Error was: {}", msg);
        assert!(matches!(err, DatabaseError::DataDirInUse(_)));
    }

    #[test]
    fn test_acquire_after_drop_succeeds() {
        let dir = tempfile::tempdir().unwrap();

        {
            let _guard = acquire(dir.path()).unwrap();
        }

        let _guard2 = acquire(dir.path()).unwrap();
    }
}
