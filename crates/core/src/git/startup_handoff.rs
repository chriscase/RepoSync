//! Process-local HTTP auth handoff after startup `clone_repo` (never persisted in `.git`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static STARTUP_HTTP_AUTH: LazyLock<Mutex<HashMap<PathBuf, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn workdir_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Remember a clean HTTP token for `workdir` until credentials are cleared or re-resolved.
pub fn remember_startup_http_auth(workdir: &Path, token: &str) {
    if token.is_empty() {
        return;
    }
    STARTUP_HTTP_AUTH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(workdir_key(workdir), token.to_string());
}

/// Peek the startup handoff for `workdir` without consuming it.
pub fn peek_startup_http_auth(workdir: &Path) -> Option<String> {
    STARTUP_HTTP_AUTH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&workdir_key(workdir))
        .cloned()
}

/// Drop any startup handoff for `workdir`.
pub fn forget_startup_http_auth(workdir: &Path) {
    STARTUP_HTTP_AUTH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&workdir_key(workdir));
}
