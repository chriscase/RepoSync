//! Shared full-history import module.
//!
//! Provides [`run_full_import`] which replays every SVN revision as an
//! individual Git commit, with identity mapping, file-policy enforcement
//! (including LFS), and real-time progress reporting via [`ImportProgress`].
//!
//! Also re-exports [`copy_tree_with_policy`] so both personal-mode and
//! team-mode code can share the file-copy logic.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

use crate::db::Database;
use crate::file_policy::{FilePolicy, FilePolicyDecision};
use crate::git::GitClient;
use crate::identity::mapper::{GitIdentity, IdentityMapper};
use crate::svn::SvnClient;

// ---------------------------------------------------------------------------
// Progress tracking
// ---------------------------------------------------------------------------

/// Maximum number of log lines kept in the ring buffer.
const MAX_LOG_LINES: usize = 1000;

/// Current phase of an import operation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportPhase {
    /// Not started.
    Idle,
    /// Connecting to SVN, fetching info and log.
    Connecting,
    /// Processing revisions (with incremental pushes every PUSH_BATCH_SIZE).
    Importing,
    /// Comparing SVN HEAD tree with Git working tree.
    Verifying,
    /// Pushing any remaining commits after verification.
    FinalPush,
    /// Import finished successfully.
    Completed,
    /// Import failed.
    Failed,
    /// Import was cancelled by user.
    Cancelled,
}

/// Results of the SVN/Git tree verification step.
#[derive(Debug, Clone, Serialize, Default)]
pub struct VerificationResult {
    pub files_checked: u64,
    pub files_matched: u64,
    pub mismatches: Vec<String>,
    pub svn_only: Vec<String>,
    pub git_only: Vec<String>,
    pub sample_hashed: u64,
    pub verified: bool,
}

/// Progress information for a running (or completed) import.
#[derive(Debug, Clone, Serialize)]
pub struct ImportProgress {
    pub phase: ImportPhase,
    pub current_rev: i64,
    pub total_revs: i64,
    pub commits_created: u64,
    /// Number of files in the most recent revision (not cumulative).
    pub current_file_count: u64,
    /// Number of unique LFS files (deduped by path+size).
    pub lfs_unique_count: u64,
    pub files_skipped: u64,
    pub batches_pushed: u64,
    pub errors: Vec<String>,
    pub log_lines: VecDeque<String>,
    pub started_at: Option<String>,
    pub push_started_at: Option<String>,
    pub completed_at: Option<String>,
    pub verification: Option<VerificationResult>,
    /// Set to true to request cancellation.
    #[serde(skip)]
    pub cancel_requested: bool,
    /// Tracks unique LFS files (path:size) — not serialized.
    #[serde(skip)]
    pub lfs_seen: HashSet<String>,
}

impl Default for ImportProgress {
    fn default() -> Self {
        Self {
            phase: ImportPhase::Idle,
            current_rev: 0,
            total_revs: 0,
            commits_created: 0,
            current_file_count: 0,
            lfs_unique_count: 0,
            files_skipped: 0,
            batches_pushed: 0,
            errors: Vec::new(),
            log_lines: VecDeque::new(),
            started_at: None,
            push_started_at: None,
            completed_at: None,
            verification: None,
            cancel_requested: false,
            lfs_seen: HashSet::new(),
        }
    }
}

impl ImportProgress {
    /// Push a timestamped log line, keeping the ring buffer bounded.
    pub fn push_log(&mut self, line: String) {
        let timestamp = chrono::Local::now().format("%H:%M:%S");
        let timestamped = format!("[{}] {}", timestamp, line);
        if self.log_lines.len() >= MAX_LOG_LINES {
            self.log_lines.pop_front();
        }
        self.log_lines.push_back(timestamped);
    }

    /// Record an LFS file, returning true if it's a new unique file.
    pub fn track_lfs_file(&mut self, path: &str, size: u64) -> bool {
        let key = format!("{}:{}", path, size);
        if self.lfs_seen.insert(key) {
            self.lfs_unique_count += 1;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// copy_tree_with_policy (moved from personal::svn_to_git)
// ---------------------------------------------------------------------------

/// Recursively copy files from SVN export `src` into Git working tree `dst`,
/// enforcing the given [`FilePolicy`].  Returns the number of files skipped,
/// the number of LFS-tracked files, and total files copied.
pub fn copy_tree_with_policy(
    src: &Path,
    dst: &Path,
    policy: &FilePolicy,
    db: &Database,
) -> Result<CopyStats> {
    let mut stats = CopyStats::default();
    copy_tree_policy_inner(src, dst, dst, src, true, policy, &mut stats)?;

    // Audit skipped count if any.
    if stats.skipped > 0 {
        let _ = db.insert_audit_log(
            "file_policy_skip",
            Some("svn_to_git"),
            None,
            None,
            None,
            Some(&format!(
                "Skipped {} files by policy during SVN→Git copy",
                stats.skipped
            )),
            true,
        );
    }

    Ok(stats)
}

/// Statistics from a copy operation.
#[derive(Debug, Default, Clone)]
pub struct CopyStats {
    pub copied: usize,
    pub skipped: usize,
    pub lfs_tracked: usize,
}

fn copy_tree_policy_inner(
    src: &Path,
    dst: &Path,
    dst_root: &Path,
    export_root: &Path,
    is_root: bool,
    policy: &FilePolicy,
    stats: &mut CopyStats,
) -> Result<()> {
    let entries = std::fs::read_dir(src)
        .with_context(|| format!("failed to read directory: {}", src.display()))?;

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // At the root level of the destination, skip dotfiles/dotdirs to
        // avoid overwriting `.git/` and similar metadata.
        if is_root && name_str.starts_with('.') {
            debug!(name = %name_str, "skipping dotfile/dotdir in export root");
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            if !dst_path.exists() {
                std::fs::create_dir_all(&dst_path).with_context(|| {
                    format!("failed to create directory: {}", dst_path.display())
                })?;
            }
            copy_tree_policy_inner(
                &src_path,
                &dst_path,
                dst_root,
                export_root,
                false,
                policy,
                stats,
            )?;
        } else {
            // Compute relative path for policy evaluation.
            let rel = src_path
                .strip_prefix(export_root)
                .unwrap_or(&src_path)
                .to_string_lossy()
                .replace('\\', "/");

            let decision = policy.evaluate_path(export_root, &rel);
            match &decision {
                FilePolicyDecision::Allow => {
                    std::fs::copy(&src_path, &dst_path).with_context(|| {
                        format!(
                            "failed to copy {} -> {}",
                            src_path.display(),
                            dst_path.display()
                        )
                    })?;
                    stats.copied += 1;
                }
                FilePolicyDecision::LfsTrack { size, threshold } => {
                    // Copy the actual file content to the Git working tree.
                    std::fs::copy(&src_path, &dst_path).with_context(|| {
                        format!(
                            "failed to copy {} -> {}",
                            src_path.display(),
                            dst_path.display()
                        )
                    })?;

                    // Ensure `.gitattributes` has the appropriate LFS tracking pattern.
                    let pattern = crate::lfs::pattern_for_path(&rel);
                    if let Err(e) = crate::lfs::ensure_lfs_tracked(dst_root, &pattern) {
                        warn!(
                            path = rel.as_str(),
                            pattern = pattern.as_str(),
                            error = %e,
                            "failed to update .gitattributes for LFS tracking"
                        );
                    } else {
                        info!(
                            path = rel.as_str(),
                            size,
                            threshold,
                            pattern = pattern.as_str(),
                            "LFS: file copied and .gitattributes updated"
                        );
                    }
                    stats.copied += 1;
                    stats.lfs_tracked += 1;
                }
                FilePolicyDecision::Ignored { pattern } => {
                    warn!(
                        path = rel.as_str(),
                        pattern = pattern.as_str(),
                        "file ignored by policy — not copied to Git"
                    );
                    stats.skipped += 1;
                }
                FilePolicyDecision::Oversize { size, limit } => {
                    warn!(
                        path = rel.as_str(),
                        size, limit, "file exceeds max_file_size — not copied to Git"
                    );
                    stats.skipped += 1;
                }
            }
        }
    }

    Ok(())
}

/// Remove files from `dst` (Git working tree) that no longer exist in `src`
/// (SVN export).  Preserves root-level dotfiles/dirs (e.g. `.git/`).
pub fn remove_stale_files(src: &Path, dst: &Path) -> Result<()> {
    remove_stale_inner(src, dst, true)
}

fn remove_stale_inner(src: &Path, dst: &Path, is_root: bool) -> Result<()> {
    let entries = match std::fs::read_dir(dst) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to read directory: {}", dst.display()));
        }
    };

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if is_root && name_str.starts_with('.') {
            continue;
        }

        let src_path = src.join(&file_name);
        let dst_path = entry.path();

        if dst_path.is_dir() {
            if src_path.is_dir() {
                remove_stale_inner(&src_path, &dst_path, false)?;
            } else {
                std::fs::remove_dir_all(&dst_path).with_context(|| {
                    format!("failed to remove stale directory: {}", dst_path.display())
                })?;
                debug!(path = %dst_path.display(), "removed stale directory");
            }
        } else if !src_path.exists() {
            std::fs::remove_file(&dst_path).with_context(|| {
                format!("failed to remove stale file: {}", dst_path.display())
            })?;
            debug!(path = %dst_path.display(), "removed stale file");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Full history import
// ---------------------------------------------------------------------------

/// Configuration for a full import run.
pub struct ImportConfig {
    /// Committer name (the person running the import).
    pub committer_name: String,
    /// Committer email.
    pub committer_email: String,
    /// Git remote name (e.g. "origin").
    pub remote_name: String,
    /// Git branch to push to (e.g. "main").
    pub branch: String,
    /// Git push token.
    pub push_token: Option<String>,
    /// Commit message prefix format.  `{rev}`, `{author}`, `{date}` are
    /// available as placeholders.  If empty, uses the original SVN message.
    pub message_prefix: Option<String>,
}

/// Run a full SVN history import, replaying every revision as a Git commit.
///
/// Progress is updated in real-time via `progress` and optionally broadcast
/// via `ws_broadcast` for the web UI.
pub async fn run_full_import(
    svn_client: &SvnClient,
    git_client: &Arc<std::sync::Mutex<GitClient>>,
    identity_mapper: &IdentityMapper,
    db: &Database,
    file_policy: &FilePolicy,
    import_config: &ImportConfig,
    progress: Arc<RwLock<ImportProgress>>,
    ws_broadcast: Option<broadcast::Sender<String>>,
    repo_id: Option<String>,
) -> Result<u64> {
    // Helper to push a log line and broadcast it.
    let log = |progress: &Arc<RwLock<ImportProgress>>,
               ws: &Option<broadcast::Sender<String>>,
               line: String| {
        let progress = progress.clone();
        let ws = ws.clone();
        async move {
            let mut p = progress.write().await;
            p.push_log(line.clone());
            // Broadcast progress update to WebSocket clients.
            if let Some(ref sender) = ws {
                let json = serde_json::json!({
                    "type": "import_progress",
                    "phase": format!("{:?}", p.phase).to_lowercase(),
                    "current_rev": p.current_rev,
                    "total_revs": p.total_revs,
                    "commits_created": p.commits_created,
                    "message": line,
                });
                let _ = sender.send(json.to_string());
            }
        }
    };

    // LFS preflight: check availability and install hooks in the repo
    let lfs_available = if file_policy.lfs_enabled() {
        match crate::lfs::preflight_check() {
            Ok(version) => {
                log(
                    &progress,
                    &ws_broadcast,
                    format!("[info] Git LFS available: {}", version),
                )
                .await;

                // Install LFS hooks/filters in the repo so `git add` invokes
                // the clean filter and creates pointer files for tracked patterns.
                let rp = {
                    let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
                    git_guard.repo_workdir()
                };
                match crate::lfs::install_lfs_hooks(&rp) {
                    Ok(()) => {
                        log(
                            &progress,
                            &ws_broadcast,
                            "[info] Git LFS installed in repo (filters active)".into(),
                        )
                        .await;
                        true
                    }
                    Err(e) => {
                        log(
                            &progress,
                            &ws_broadcast,
                            format!("[warn] git lfs install failed: {} — LFS tracking will not work", e),
                        )
                        .await;
                        false
                    }
                }
            }
            Err(e) => {
                log(
                    &progress,
                    &ws_broadcast,
                    format!("[warn] Git LFS not available: {} — large files will be committed directly", e),
                )
                .await;
                false
            }
        }
    } else {
        false
    };

    // Get SVN info
    log(
        &progress,
        &ws_broadcast,
        "[info] Connecting to SVN repository...".into(),
    )
    .await;

    // Persist initial importing state
    {
        let p = progress.read().await;
        if let Err(e) = db.persist_import_progress(&p) {
            warn!("failed to persist import progress: {}", e);
        }
    }

    let svn_info = svn_client
        .info()
        .await
        .context("failed to get SVN info")?;
    let head_rev = svn_info.latest_rev;

    {
        let mut p = progress.write().await;
        p.total_revs = head_rev;
    }

    log(
        &progress,
        &ws_broadcast,
        format!(
            "[info] SVN HEAD is r{}, importing {} revisions",
            head_rev, head_rev
        ),
    )
    .await;

    // Get all log entries
    log(
        &progress,
        &ws_broadcast,
        "[info] Fetching SVN history...".into(),
    )
    .await;

    let log_entries = svn_client
        .log(1, head_rev)
        .await
        .context("failed to get SVN log")?;

    {
        let mut p = progress.write().await;
        p.total_revs = log_entries.len() as i64;
    }

    log(
        &progress,
        &ws_broadcast,
        format!("[info] Found {} revisions to import", log_entries.len()),
    )
    .await;

    let repo_path = {
        let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
        git_guard.repo_path().to_path_buf()
    };

    let mut count = 0u64;
    let mut commits_since_push = 0u64;
    const PUSH_BATCH_SIZE: u64 = 50;

    for (idx, entry) in log_entries.iter().enumerate() {
        // Check for cancellation
        {
            let p = progress.read().await;
            if p.cancel_requested {
                let mut p = progress.write().await;
                p.phase = ImportPhase::Cancelled;
                p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                log(
                    &progress,
                    &ws_broadcast,
                    "[warn] Import cancelled by user".into(),
                )
                .await;
                return Ok(count);
            }
        }

        let rev = entry.revision;
        {
            let mut p = progress.write().await;
            p.current_rev = idx as i64 + 1;
        }

        // Export this revision
        let export_dir = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("[error] r{}: failed to create temp dir: {}", rev, e);
                log(&progress, &ws_broadcast, msg.clone()).await;
                let mut p = progress.write().await;
                p.errors.push(msg);
                continue;
            }
        };

        // P6 optimization: for revisions after the first, try applying an
        // incremental SVN diff instead of a full export.  Falls back to full
        // export if the diff cannot be applied.
        let mut used_incremental = false;
        if idx > 0 {
            match svn_client.diff_full(rev).await {
                Ok(diff_text) if !diff_text.trim().is_empty() => {
                    match crate::sync_engine::apply_diff_to_path(&repo_path, &diff_text).await {
                        Ok(()) => {
                            used_incremental = true;
                            debug!(rev, "applied incremental SVN diff");
                        }
                        Err(e) => {
                            debug!(rev, error = %e, "incremental diff failed, falling back to full export");
                        }
                    }
                }
                _ => {
                    debug!(rev, "no diff available, using full export");
                }
            }
        }

        if !used_incremental {
            if let Err(e) = svn_client.export("", rev, export_dir.path()).await {
                let msg = format!("[error] r{}: SVN export failed: {}", rev, e);
                log(&progress, &ws_broadcast, msg.clone()).await;
                let mut p = progress.write().await;
                p.errors.push(msg);
                continue;
            }

            // Remove stale files from Git working tree
            if let Err(e) = remove_stale_files(export_dir.path(), &repo_path) {
                let msg = format!("[warn] r{}: failed to remove stale files: {}", rev, e);
                log(&progress, &ws_broadcast, msg).await;
            }
        }

        // Copy with policy enforcement (only needed for full export path)
        let copy_stats = if used_incremental {
            CopyStats::default()
        } else {
            match copy_tree_with_policy(export_dir.path(), &repo_path, file_policy, db)
            {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("[error] r{}: copy failed: {}", rev, e);
                log(&progress, &ws_broadcast, msg.clone()).await;
                let mut p = progress.write().await;
                p.errors.push(msg);
                continue;
            }
        }
        };

        // Update file stats — use current file count (not cumulative)
        info!(
            rev,
            copied = copy_stats.copied,
            lfs_tracked = copy_stats.lfs_tracked,
            skipped = copy_stats.skipped,
            "import: tree copy completed for revision"
        );
        {
            let mut p = progress.write().await;
            p.current_file_count = copy_stats.copied as u64;
            p.files_skipped += copy_stats.skipped as u64;
            // LFS dedup is handled in copy_tree_with_policy via track_lfs_file
        }

        // Resolve Git identity for this author
        let (author_name, author_email) = match identity_mapper.svn_to_git(&entry.author) {
            Ok(GitIdentity { name, email }) => {
                debug!(rev, svn_author = %entry.author, git_name = %name, "mapped SVN author");
                (name, email)
            }
            Err(e) => {
                // Fall back to SVN username as both name and email prefix
                debug!(
                    rev,
                    svn_author = %entry.author,
                    error = %e,
                    "identity mapping failed, using fallback {}@svn",
                    entry.author
                );
                (
                    entry.author.clone(),
                    format!("{}@svn", entry.author),
                )
            }
        };

        // Build commit message
        let message = format!(
            "{}\n\n[reposync] imported from SVN r{}\nSVN-Author: {}\nSVN-Date: {}",
            entry.message, rev, entry.author, entry.date
        );

        // Commit — use CLI when LFS files are present so that `git add`
        // invokes the LFS clean filter and stores large files as pointers.
        // libgit2's Index::add_all() bypasses LFS filters entirely.
        let use_cli = lfs_available && copy_stats.lfs_tracked > 0;
        let (commit_result, push_repo_path) = {
            let git_client_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
            let rp = git_client_guard.repo_workdir();
            let result = if use_cli {
                debug!(
                    rev,
                    lfs_count = copy_stats.lfs_tracked,
                    "using git CLI for commit (LFS files present)"
                );
                git_client_guard.commit_via_cli(
                    &message,
                    &author_name,
                    &author_email,
                    &import_config.committer_name,
                    &import_config.committer_email,
                )
            } else {
                git_client_guard.commit(
                    &message,
                    &author_name,
                    &author_email,
                    &import_config.committer_name,
                    &import_config.committer_email,
                )
            };
            (result, rp)
        }; // git_client_guard dropped here, before any .await
        match commit_result {
            Ok(oid) => {
                let sha = oid.to_string();
                let short_sha = &sha[..8.min(sha.len())];

                // Log with details
                let mut detail_parts = vec![format!("{} files", copy_stats.copied)];
                if copy_stats.lfs_tracked > 0 {
                    detail_parts.push(format!("LFS: {}", copy_stats.lfs_tracked));
                }
                if copy_stats.skipped > 0 {
                    detail_parts.push(format!("skipped: {}", copy_stats.skipped));
                }
                let details = detail_parts.join(", ");

                let log_line = format!(
                    "[ok] r{} → {} ({}) \"{}\" [{}]",
                    rev,
                    short_sha,
                    author_name,
                    entry.message.lines().next().unwrap_or("").chars().take(60).collect::<String>(),
                    details,
                );
                log(&progress, &ws_broadcast, log_line).await;

                // Record in DB (commit_map for bidirectional mapping)
                db.insert_commit_map(
                    rev,
                    &sha,
                    "svn_to_git",
                    &entry.author,
                    &format!("{} <{}>", author_name, author_email),
                )
                .ok();

                // Record sync_record for audit trail and UI display
                let sync_record = crate::models::SyncRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    repo_id: repo_id.clone(),
                    svn_revision: Some(rev),
                    git_hash: Some(sha.clone()),
                    direction: crate::models::SyncDirection::SvnToGit,
                    author: entry.author.clone(),
                    message: entry.message.clone(),
                    timestamp: chrono::Utc::now(),
                    synced_at: chrono::Utc::now(),
                    status: crate::models::SyncRecordStatus::Applied,
                };
                if let Err(e) = db.insert_sync_record(&sync_record) {
                    debug!(rev, error = %e, "failed to insert sync_record during import");
                } else {
                    debug!(rev, sha = %short_sha, "import: sync_record created");
                }

                count += 1;
                commits_since_push += 1;
                {
                    let mut p = progress.write().await;
                    p.commits_created = count;
                }

                let repo_path = push_repo_path;

                // Incremental push every PUSH_BATCH_SIZE commits
                if commits_since_push >= PUSH_BATCH_SIZE {
                    let is_first_push = {
                        let p = progress.read().await;
                        p.batches_pushed == 0
                    };
                    let push_type = if is_first_push { "force-push" } else { "push" };
                    log(
                        &progress,
                        &ws_broadcast,
                        format!("[info] {} batch of {} commits to remote...", push_type, commits_since_push),
                    )
                    .await;

                    // Use spawn_blocking to avoid blocking the tokio runtime
                    let remote = import_config.remote_name.clone();
                    let branch = import_config.branch.clone();
                    let force = is_first_push;
                    let rp = repo_path.clone();

                    // Heartbeat task: log "still pushing..." every 30s
                    let hb_progress = progress.clone();
                    let hb_ws = ws_broadcast.clone();
                    let hb_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let hb_cancel2 = hb_cancel.clone();
                    let hb_handle = tokio::spawn(async move {
                        let start = std::time::Instant::now();
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                            if hb_cancel2.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            let elapsed = start.elapsed().as_secs();
                            push_log_line(
                                &hb_progress,
                                &hb_ws,
                                format!("[push] still uploading... ({}m {}s elapsed)", elapsed / 60, elapsed % 60),
                            ).await;
                        }
                    });

                    let push_result = tokio::task::spawn_blocking(move || {
                        let start = std::time::Instant::now();
                        info!(remote = %remote, branch = %branch, force, "spawn_blocking push starting");

                        let mut args = vec!["push".to_string(), "--progress".to_string()];
                        if force {
                            args.push("--force".to_string());
                        }
                        args.push(remote.clone());
                        args.push(branch.clone());

                        let output = std::process::Command::new("git")
                            .args(&args)
                            .current_dir(&rp)
                            .env("GIT_TERMINAL_PROMPT", "0")
                            .output();

                        let elapsed = start.elapsed();

                        match output {
                            Ok(out) => {
                                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                                if out.status.success() {
                                    info!(elapsed_secs = elapsed.as_secs_f64(), "push completed");
                                    Ok(stderr)
                                } else {
                                    error!(stderr = %stderr, elapsed_secs = elapsed.as_secs_f64(), "push failed");
                                    Err(format!("git push failed (exit {:?}, {:.1}s): {}", out.status.code(), elapsed.as_secs_f64(), stderr.trim()))
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "failed to spawn git push");
                                Err(format!("failed to spawn git push: {}", e))
                            }
                        }
                    }).await;

                    // Stop heartbeat
                    hb_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    hb_handle.abort();

                    match push_result {
                        Ok(Ok(stderr)) => {
                            // Log any git push output (remote warnings, etc.)
                            for line in stderr.lines() {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    log(&progress, &ws_broadcast, format!("[push] {}", trimmed)).await;
                                }
                            }
                            {
                                let mut p = progress.write().await;
                                p.batches_pushed += 1;
                            }
                            // Persist progress after batch push
                            {
                                let p = progress.read().await;
                                if let Err(e) = db.persist_import_progress(&p) {
                                    warn!("failed to persist import progress after batch push: {}", e);
                                }
                            }
                            log(
                                &progress,
                                &ws_broadcast,
                                format!("[ok] Batch pushed ({} of {} total commits)", count, log_entries.len()),
                            )
                            .await;
                        }
                        Ok(Err(e)) => {
                            let msg = format!("[warn] Batch push failed (will retry at end): {}", e);
                            log(&progress, &ws_broadcast, msg).await;
                        }
                        Err(e) => {
                            let msg = format!("[warn] Batch push task panicked: {}", e);
                            log(&progress, &ws_broadcast, msg).await;
                        }
                    }
                    commits_since_push = 0;
                }
            }
            Err(e) => {
                // Empty commits (property-only revisions) are expected
                let msg = format!(
                    "[skip] r{}: no changes to commit ({})",
                    rev,
                    e.to_string().lines().next().unwrap_or("unknown")
                );
                log(&progress, &ws_broadcast, msg).await;
            }
        }

        // Broadcast progress JSON update
        if let Some(ref sender) = ws_broadcast {
            let p = progress.read().await;
            let json = serde_json::json!({
                "type": "import_progress",
                "phase": "importing",
                "current_rev": p.current_rev,
                "total_revs": p.total_revs,
                "commits_created": p.commits_created,
                "current_file_count": p.current_file_count,
                "lfs_unique_count": p.lfs_unique_count,
                "batches_pushed": p.batches_pushed,
                "percentage": if p.total_revs > 0 { (p.current_rev as f64 / p.total_revs as f64 * 100.0) as u32 } else { 0 },
            });
            let _ = sender.send(json.to_string());
        }

        // Persist progress to DB every 10 revisions
        if (idx + 1) % 10 == 0 {
            let p = progress.read().await;
            if let Err(e) = db.persist_import_progress(&p) {
                warn!("failed to persist import progress at rev {}: {}", idx + 1, e);
            }
        }
    }

    // Push remaining commits (those since last batch push)
    if commits_since_push > 0 {
        let max_retries = 3;
        let mut push_success = false;

        for attempt in 1..=max_retries {
            log(
                &progress,
                &ws_broadcast,
                format!(
                    "[info] Pushing remaining {} commits to remote (attempt {}/{})...",
                    commits_since_push, attempt, max_retries
                ),
            )
            .await;

            let repo_path = {
                let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
                git_guard.repo_workdir()
            };

            let is_first_push = {
                let p = progress.read().await;
                p.batches_pushed == 0
            };

            let remote = import_config.remote_name.clone();
            let branch = import_config.branch.clone();
            let force = is_first_push;
            let rp = repo_path.clone();

            let push_result = tokio::task::spawn_blocking(move || {
                let mut args = vec!["push".to_string(), "--progress".to_string()];
                if force { args.push("--force".to_string()); }
                args.push(remote); args.push(branch);
                let output = std::process::Command::new("git")
                    .args(&args).current_dir(&rp)
                    .env("GIT_TERMINAL_PROMPT", "0").output();
                match output {
                    Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stderr).to_string()),
                    Ok(out) => Err(format!("exit {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr).trim())),
                    Err(e) => Err(format!("spawn failed: {}", e)),
                }
            }).await;

            match push_result {
                Ok(Ok(stderr)) => {
                    for line in stderr.lines() {
                        let t = line.trim();
                        if !t.is_empty() { log(&progress, &ws_broadcast, format!("[push] {}", t)).await; }
                    }
                    { let mut p = progress.write().await; p.batches_pushed += 1; }
                    log(&progress, &ws_broadcast, format!("[ok] All {} commits pushed successfully", count)).await;
                    push_success = true;
                    break;
                }
                Ok(Err(e)) => {
                    let _msg = format!("[warn] Push attempt {}/{} failed: {}", attempt, max_retries, e);
                }
                Err(e) => {
                    let msg = format!("[warn] Push attempt {}/{} failed (panic): {}", attempt, max_retries, e);
                    log(&progress, &ws_broadcast, msg.clone()).await;

                    if attempt < max_retries {
                        let delay_secs = attempt as u64 * 5; // 5s, 10s, 15s backoff
                        log(
                            &progress,
                            &ws_broadcast,
                            format!("[info] Retrying in {} seconds...", delay_secs),
                        )
                        .await;
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    }
                }
            }
        }

        if !push_success {
            let msg = format!(
                "[error] Push failed after {} attempts. {} commits are saved locally and can be pushed manually with: cd /opt/reposync/git-repo && git push origin main",
                max_retries, commits_since_push
            );
            log(&progress, &ws_broadcast, msg.clone()).await;
            let mut p = progress.write().await;
            p.errors.push(msg);
        }
    }

    // Persist progress before final watermarks
    {
        let p = progress.read().await;
        if let Err(e) = db.persist_import_progress(&p) {
            warn!("failed to persist import progress before watermarks: {}", e);
        }
    }

    // Set watermarks
    if let Some(last) = log_entries.last() {
        db.set_watermark("svn_rev", &last.revision.to_string())
            .ok();
    }

    {
        let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(sha) = git_guard.get_head_sha() {
            db.set_watermark("git_sha", &sha).ok();
        }
    }

    // Final audit log
    db.insert_audit_log(
        "import_full",
        Some("svn_to_git"),
        Some(head_rev),
        None,
        None,
        Some(&format!(
            "Full history import: {} commits from {} revisions",
            count,
            log_entries.len()
        )),
        true,
    )
    .ok();

    info!(
        count,
        revisions = log_entries.len(),
        "full import completed"
    );
    Ok(count)
}

/// Helper to push a log line and broadcast it via WebSocket.
async fn push_log_line(
    progress: &Arc<RwLock<ImportProgress>>,
    ws: &Option<broadcast::Sender<String>>,
    line: String,
) {
    let mut p = progress.write().await;
    p.push_log(line.clone());
    if let Some(ref sender) = ws {
        let json = serde_json::json!({
            "type": "import_progress",
            "phase": format!("{:?}", p.phase).to_lowercase(),
            "current_rev": p.current_rev,
            "total_revs": p.total_revs,
            "commits_created": p.commits_created,
            "message": line,
        });
        let _ = sender.send(json.to_string());
    }
}

/// Async git push that doesn't block the tokio runtime.
/// Streams stderr output into the import progress log so users see real-time
/// push progress (object counting, compression, upload, LFS transfers).
#[allow(dead_code)]
async fn async_git_push(
    repo_path: &std::path::Path,
    remote: &str,
    branch: &str,
    force: bool,
    progress: &Arc<RwLock<ImportProgress>>,
    ws_broadcast: &Option<tokio::sync::broadcast::Sender<String>>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let start = std::time::Instant::now();

    let mut args = vec!["push", "--progress"];
    if force {
        args.push("--force");
    }
    args.push(remote);
    args.push(branch);

    info!(
        remote,
        branch,
        force,
        repo_path = %repo_path.display(),
        "async git push starting"
    );

    let mut child = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git push")?;

    // Stream stderr (where git push progress goes) into the import log
    let stderr = child.stderr.take();
    if let Some(stderr) = stderr {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        let mut last_heartbeat = std::time::Instant::now();

        loop {
            line.clear();
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                reader.read_line(&mut line),
            )
            .await
            {
                Ok(Ok(0)) => break, // EOF
                Ok(Ok(_)) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        // Filter verbose git progress lines — only show summaries
                        let is_progress = trimmed.starts_with("Counting objects:")
                            || trimmed.starts_with("Compressing objects:")
                            || trimmed.starts_with("Writing objects:")
                            || trimmed.starts_with("Resolving deltas:")
                            || trimmed.starts_with("Delta compression");

                        if is_progress {
                            // Only log the final "Total" or "100%" lines
                            if trimmed.starts_with("Total ") || trimmed.contains("100%") {
                                // Extract a clean summary from "Total N (delta M), reused X, SIZE | SPEED"
                                if trimmed.starts_with("Total ") {
                                    push_log_line(progress, ws_broadcast, format!("[push] {}", trimmed)).await;
                                }
                                // Skip the 100% lines — redundant with Total
                            }
                        } else {
                            push_log_line(progress, ws_broadcast, format!("[push] {}", trimmed)).await;
                        }
                    }
                    last_heartbeat = std::time::Instant::now();
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "error reading git push stderr");
                    break;
                }
                Err(_) => {
                    // Timeout — push is still running but no output for 30s
                    let elapsed = start.elapsed().as_secs();
                    push_log_line(
                        progress,
                        ws_broadcast,
                        format!(
                            "[push] still uploading... ({}m {}s elapsed)",
                            elapsed / 60,
                            elapsed % 60
                        ),
                    )
                    .await;
                    let _ = last_heartbeat;
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .context("failed to wait for git push")?;

    let elapsed = start.elapsed();

    if !status.success() {
        let msg = format!(
            "git push failed (exit {:?}, {:.1}s)",
            status.code(),
            elapsed.as_secs_f64(),
        );
        error!(msg = %msg, "push failed");
        anyhow::bail!(msg);
    }

    // Update batches pushed counter
    {
        let mut p = progress.write().await;
        p.batches_pushed += 1;
    }

    info!(
        elapsed_secs = elapsed.as_secs_f64(),
        "async push completed successfully"
    );
    Ok(())
}
