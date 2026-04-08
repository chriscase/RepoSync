//! Process-wide per-repository busy lock.
//!
//! Prevents concurrent access to the same repo working tree from the
//! scheduler (per-repo sync cycles) and the web API (full imports). Both
//! subsystems call [`try_acquire`] with the repo ID before touching the
//! working tree and hold the returned [`BusyGuard`] until the operation
//! finishes. The guard releases on drop, including panic unwinds.
//!
//! This is a last line of defense: the individual subsystems *should* be
//! polite to each other (the scheduler already skips repos whose import
//! is marked active), but tasks already in flight can still collide on
//! the filesystem. A shared lock eliminates that race entirely.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

static BUSY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn set() -> &'static Mutex<HashSet<String>> {
    BUSY.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard that releases the busy slot on drop.
pub struct BusyGuard {
    repo_id: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = set().lock() {
            guard.remove(&self.repo_id);
        }
    }
}

/// Try to acquire the busy slot for `repo_id`. Returns `None` if another
/// task is already holding it.
pub fn try_acquire(repo_id: &str) -> Option<BusyGuard> {
    let mut guard = set().lock().ok()?;
    if guard.insert(repo_id.to_string()) {
        Some(BusyGuard {
            repo_id: repo_id.to_string(),
        })
    } else {
        None
    }
}

/// Returns true if another task is currently holding the busy slot for
/// this repo.
pub fn is_busy(repo_id: &str) -> bool {
    set()
        .lock()
        .map(|g| g.contains(repo_id))
        .unwrap_or(false)
}
