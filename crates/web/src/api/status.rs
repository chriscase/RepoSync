//! Status and health check endpoints.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Optional query parameter to scope status to a specific repository.
#[derive(Deserialize)]
struct RepoQuery {
    repo_id: Option<String>,
}

/// Health check response.
#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    version: String,
    git_commit: String,
}

/// Status response wrapping the core SyncStatus.
#[derive(Serialize)]
struct StatusResponse {
    state: String,
    last_sync_at: Option<String>,
    last_svn_revision: Option<i64>,
    last_git_hash: Option<String>,
    total_syncs: i64,
    total_conflicts: i64,
    active_conflicts: i64,
    total_errors: i64,
    last_error_at: Option<String>,
    uptime_secs: u64,
}

/// Real-time system metrics for display during import operations.
#[derive(Serialize)]
struct SystemMetrics {
    disk_free_bytes: u64,
    disk_total_bytes: u64,
    disk_usage_percent: f64,
    mem_used_bytes: u64,
    mem_total_bytes: u64,
    mem_usage_percent: f64,
    cpu_load_1m: f64,
    cpu_load_5m: f64,
    cpu_load_15m: f64,
    git_push_active: bool,
    git_push_pid: Option<u32>,
    git_push_elapsed_secs: Option<u64>,
    data_dir_size_bytes: u64,
    /// Network bytes sent since boot (from /proc/net/dev)
    net_bytes_sent: u64,
    /// Network bytes received since boot
    net_bytes_recv: u64,
    /// Network upload rate (bytes/sec), computed server-side
    net_up_bytes_per_sec: f64,
    /// Network download rate (bytes/sec), computed server-side
    net_down_bytes_per_sec: f64,
    /// SVN process active (svn export/log/info running)
    svn_active: bool,
    /// Daemon process RSS (resident set size) in bytes
    process_rss_bytes: u64,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/status", get(get_status))
        .route("/api/status/health", get(health_check))
        .route("/api/status/system", get(get_system_metrics))
        .route("/api/status/reset-errors", post(reset_errors))
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: env!("GIT_COMMIT_SHA").to_string(),
    })
}

async fn get_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<RepoQuery>,
) -> Result<Json<StatusResponse>, AppError> {
    crate::api::auth::validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    // When a specific repo is selected, return that repo's data instead of
    // the global sync engine state.
    if let Some(ref repo_id) = query.repo_id {
        let repo = db
            .get_repository(repo_id)
            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
            .ok_or_else(|| AppError::NotFound(format!("repository {} not found", repo_id)))?;

        let active_conflicts = db.count_active_conflicts_for_repo(repo_id).unwrap_or(0);

        // Use 24h rolling window for errors (same as global status)
        let errors_24h = db.count_errors_for_repo(repo_id).unwrap_or(repo.total_errors);
        let last_error = db.last_error_at_for_repo(repo_id).unwrap_or(None);

        return Ok(Json(StatusResponse {
            state: repo.sync_status,
            last_sync_at: repo.last_sync_at,
            last_svn_revision: if repo.last_svn_rev != 0 { Some(repo.last_svn_rev) } else { None },
            last_git_hash: if repo.last_git_sha.is_empty() { None } else { Some(repo.last_git_sha) },
            total_syncs: repo.total_syncs,
            total_conflicts: 0,
            active_conflicts,
            total_errors: errors_24h,
            last_error_at: last_error,
            uptime_secs: 0,
        }));
    }

    // Global status (no repo_id) — read from the sync engine's kv_state.
    let state_str = db.get_state("sync_state").unwrap_or(None).unwrap_or_else(|| "idle".into());
    let last_sync_str = db.get_state("last_sync_at").unwrap_or(None);
    let last_sync_at = last_sync_str.and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.to_rfc3339()));
    let last_svn_rev = db.get_state("last_svn_rev").unwrap_or(None).and_then(|s| s.parse::<i64>().ok())
        .or_else(|| db.get_last_svn_revision().ok().flatten());
    let last_git_hash = db.get_state("last_git_hash").unwrap_or(None).and_then(|s| if s.is_empty() { None } else { Some(s) })
        .or_else(|| db.get_last_git_hash().ok().flatten());
    let total_syncs = db.count_sync_records().unwrap_or(0);
    let total_conflicts = db.count_all_conflicts().unwrap_or(0);
    let active_conflicts = db.count_active_conflicts().unwrap_or(0);
    let total_errors = db.count_errors().unwrap_or(0);
    let last_error_at = db.get_state("last_error_at").unwrap_or(None);

    Ok(Json(StatusResponse {
        state: state_str,
        last_sync_at,
        last_svn_revision: last_svn_rev,
        last_git_hash,
        total_syncs,
        total_conflicts,
        active_conflicts,
        total_errors,
        last_error_at,
        uptime_secs: 0, // TODO: track in AppState
    }))
}

async fn reset_errors(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<RepoQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::api::auth::validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    let cleared = if let Some(ref repo_id) = query.repo_id {
        // Clear errors for a specific repo (also resets total_errors column)
        db.clear_errors_for_repo(repo_id)
            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
    } else {
        // Clear all errors
        db.clear_errors()
            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
    };

    let _ = db.insert_audit_log(
        "errors_cleared",
        query.repo_id.as_deref(),
        None,
        None,
        None,
        Some(&format!("Cleared {} error entries{}", cleared,
            query.repo_id.as_ref().map(|id| format!(" for repo {}", id)).unwrap_or_default())),
        true,
    );

    Ok(Json(serde_json::json!({
        "ok": true,
        "cleared": cleared
    })))
}

async fn get_system_metrics(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<SystemMetrics>, AppError> {
    crate::api::auth::validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    // All system metric collection involves blocking I/O (reading /proc,
    // scanning directories, calling libc::statvfs). Run on a blocking thread
    // so we never stall the async web server.
    tracing::debug!("get_system_metrics: before spawn_blocking");
    let state_clone = state.clone();
    let metrics = tokio::task::spawn_blocking(move || {
        tracing::debug!("get_system_metrics: inside spawn_blocking");
        let data_dir = &state_clone.config.daemon.data_dir;

        let (disk_free_bytes, disk_total_bytes) = disk_usage(data_dir);
        let disk_usage_percent = if disk_total_bytes > 0 {
            ((disk_total_bytes - disk_free_bytes) as f64 / disk_total_bytes as f64) * 100.0
        } else {
            0.0
        };

        let (mem_used_bytes, mem_total_bytes) = mem_usage();
        let mem_usage_percent = if mem_total_bytes > 0 {
            (mem_used_bytes as f64 / mem_total_bytes as f64) * 100.0
        } else {
            0.0
        };

        let (cpu_load_1m, cpu_load_5m, cpu_load_15m) = cpu_load();
        let (git_push_active, git_push_pid, git_push_elapsed_secs) = find_git_push_process();

        let git_repo_path = data_dir.join("git-repo");
        let data_dir_size_bytes = dir_size(&git_repo_path);

        let (net_bytes_sent, net_bytes_recv) = read_net_bytes();

        let (net_up_bytes_per_sec, net_down_bytes_per_sec) = {
            let mut prev = state_clone.prev_net_snapshot.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            let rates = if let Some((prev_sent, prev_recv, prev_time)) = prev.as_ref() {
                let dt = now.duration_since(*prev_time).as_secs_f64();
                if dt > 0.5 {
                    let up = (net_bytes_sent.saturating_sub(*prev_sent)) as f64 / dt;
                    let down = (net_bytes_recv.saturating_sub(*prev_recv)) as f64 / dt;
                    (up, down)
                } else {
                    (0.0, 0.0)
                }
            } else {
                (0.0, 0.0)
            };
            *prev = Some((net_bytes_sent, net_bytes_recv, now));
            rates
        };

        let svn_active = is_process_running("svn");
        let process_rss_bytes = process_rss();

        SystemMetrics {
            disk_free_bytes,
            disk_total_bytes,
            disk_usage_percent,
            mem_used_bytes,
            mem_total_bytes,
            mem_usage_percent,
            cpu_load_1m,
            cpu_load_5m,
            cpu_load_15m,
            git_push_active,
            git_push_pid,
            git_push_elapsed_secs,
            data_dir_size_bytes,
            net_bytes_sent,
            net_bytes_recv,
            net_up_bytes_per_sec,
            net_down_bytes_per_sec,
            svn_active,
            process_rss_bytes,
        }
    })
    .await
    .map_err(|e| AppError::Internal(format!("spawn_blocking: {}", e)))?;
    tracing::debug!("get_system_metrics: spawn_blocking completed");

    Ok(Json(metrics))
}

// ---------------------------------------------------------------------------
// System metric helpers
// ---------------------------------------------------------------------------

/// Return (free_bytes, total_bytes) for the filesystem containing `path`.
#[cfg(target_os = "linux")]
fn disk_usage(path: &Path) -> (u64, u64) {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let c_path = match CString::new(path.to_string_lossy().as_bytes()) {
        Ok(p) => p,
        Err(_) => return (0, 0),
    };

    unsafe {
        let mut buf = MaybeUninit::<libc::statvfs>::uninit();
        if libc::statvfs(c_path.as_ptr(), buf.as_mut_ptr()) == 0 {
            let stat = buf.assume_init();
            let total = stat.f_blocks as u64 * stat.f_frsize as u64;
            let free = stat.f_bavail as u64 * stat.f_frsize as u64;
            (free, total)
        } else {
            (0, 0)
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn disk_usage(_path: &Path) -> (u64, u64) {
    // Fallback: no disk info available on non-Linux platforms.
    (0, 0)
}

/// Return (used_bytes, total_bytes) from `/proc/meminfo`.
#[cfg(target_os = "linux")]
fn mem_usage() -> (u64, u64) {
    let content = match std::fs::read_to_string("/proc/meminfo") {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };

    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_meminfo_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_meminfo_kb(rest);
        }
    }

    let total = total_kb * 1024;
    let used = total.saturating_sub(available_kb * 1024);
    (used, total)
}

#[cfg(target_os = "linux")]
fn parse_meminfo_kb(s: &str) -> u64 {
    s.trim()
        .trim_end_matches("kB")
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn mem_usage() -> (u64, u64) {
    // Fallback defaults: report 0 on non-Linux.
    (0, 0)
}

/// Return daemon process RSS in bytes from `/proc/self/status`.
#[cfg(target_os = "linux")]
fn process_rss() -> u64 {
    let content = match std::fs::read_to_string("/proc/self/status") {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .trim()
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024; // kB to bytes
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn process_rss() -> u64 {
    0
}

/// Return (load_1m, load_5m, load_15m) from `/proc/loadavg`.
#[cfg(target_os = "linux")]
fn cpu_load() -> (f64, f64, f64) {
    let content = match std::fs::read_to_string("/proc/loadavg") {
        Ok(c) => c,
        Err(_) => return (0.0, 0.0, 0.0),
    };

    let mut parts = content.split_whitespace();
    let load_1m = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let load_5m = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let load_15m = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    (load_1m, load_5m, load_15m)
}

#[cfg(not(target_os = "linux"))]
fn cpu_load() -> (f64, f64, f64) {
    (0.0, 0.0, 0.0)
}

/// Scan `/proc` for a running `git push` process.
/// Returns (active, pid, elapsed_secs).
#[cfg(target_os = "linux")]
fn find_git_push_process() -> (bool, Option<u32>, Option<u64>) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (false, None, None);
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only look at numeric (PID) directories.
        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let cmdline_path = entry.path().join("cmdline");
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
            // cmdline uses NUL separators; replace for easy matching.
            let cmdline_readable = cmdline.replace('\0', " ");
            if cmdline_readable.contains("git push")
                || cmdline_readable.contains("git-push")
            {
                // Try to read process start time from /proc/<pid>/stat for elapsed.
                let elapsed = process_elapsed_secs(pid);
                return (true, Some(pid), elapsed);
            }
        }
    }

    (false, None, None)
}

/// Compute how many seconds a process has been running using `/proc/<pid>/stat`
/// and `/proc/uptime`.
#[cfg(target_os = "linux")]
fn process_elapsed_secs(pid: u32) -> Option<u64> {
    // Read system uptime in seconds.
    let uptime_str = std::fs::read_to_string("/proc/uptime").ok()?;
    let uptime_secs: f64 = uptime_str.split_whitespace().next()?.parse().ok()?;

    // Read clock ticks per second (USER_HZ, typically 100).
    let clk_tck: f64 = 100.0;

    // Read /proc/<pid>/stat — field 22 (1-indexed) is starttime in clock ticks.
    let stat_str = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    // The comm field (field 2) may contain spaces/parens; find the closing paren
    // then split the rest.
    let after_comm = stat_str.rfind(')')?.checked_add(1)?;
    let fields: Vec<&str> = stat_str[after_comm..].split_whitespace().collect();
    // After the closing paren, field index 0 = state (field 3), so starttime
    // is at index 19 (field 22 − 3).
    let starttime_ticks: f64 = fields.get(19)?.parse().ok()?;

    let start_secs = starttime_ticks / clk_tck;
    let elapsed = uptime_secs - start_secs;
    if elapsed >= 0.0 {
        Some(elapsed as u64)
    } else {
        Some(0)
    }
}

#[cfg(not(target_os = "linux"))]
fn find_git_push_process() -> (bool, Option<u32>, Option<u64>) {
    (false, None, None)
}

/// Recursively compute the total size of a directory in bytes.
fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    dir_size_inner(path)
}

fn dir_size_inner(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            total += dir_size_inner(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Read total network bytes sent/received from /proc/net/dev (Linux only).
/// Returns (bytes_sent, bytes_recv) summed across all non-loopback interfaces.
#[cfg(target_os = "linux")]
fn read_net_bytes() -> (u64, u64) {
    let content = match std::fs::read_to_string("/proc/net/dev") {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };
    let mut total_sent = 0u64;
    let mut total_recv = 0u64;
    for line in content.lines().skip(2) {
        let line = line.trim();
        if line.starts_with("lo:") {
            continue; // skip loopback
        }
        if let Some(data) = line.split(':').nth(1) {
            let fields: Vec<&str> = data.split_whitespace().collect();
            if fields.len() >= 10 {
                if let Ok(recv) = fields[0].parse::<u64>() {
                    total_recv += recv;
                }
                if let Ok(sent) = fields[8].parse::<u64>() {
                    total_sent += sent;
                }
            }
        }
    }
    (total_sent, total_recv)
}

#[cfg(not(target_os = "linux"))]
fn read_net_bytes() -> (u64, u64) {
    (0, 0)
}

/// Check if any process with the given name is running (Linux only).
#[cfg(target_os = "linux")]
fn is_process_running(name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if fname_str.chars().all(|c| c.is_ascii_digit()) {
            let cmdline_path = entry.path().join("cmdline");
            if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                if cmdline.contains(name) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn is_process_running(_name: &str) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Shared error type for API handlers
// ---------------------------------------------------------------------------

/// Simple API error type that converts to an Axum response.
pub enum AppError {
    BadRequest(String),
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Internal(String),
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, _message) = match &self {
            AppError::BadRequest(_) => (axum::http::StatusCode::BAD_REQUEST, String::new()),
            AppError::NotFound(_) => (axum::http::StatusCode::NOT_FOUND, String::new()),
            AppError::Unauthorized(_) => (axum::http::StatusCode::UNAUTHORIZED, String::new()),
            AppError::Forbidden(_) => (axum::http::StatusCode::FORBIDDEN, String::new()),
            AppError::Internal(msg) => {
                // Log the full error server-side but return a generic message to clients
                tracing::error!(detail = %msg, "internal server error");
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new())
            }
        };

        let client_message = match self {
            AppError::Internal(_) => "internal server error".to_string(),
            AppError::BadRequest(msg) | AppError::NotFound(msg) |
            AppError::Unauthorized(msg) | AppError::Forbidden(msg) => msg,
        };

        let body = serde_json::json!({ "error": client_message });
        (status, Json(body)).into_response()
    }
}
