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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    #[serde(skip)]
    pub cancel_signal: Arc<AtomicBool>,
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
            cancel_signal: Arc::new(AtomicBool::new(false)),
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
            return Err(e).with_context(|| format!("failed to read directory: {}", dst.display()));
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
            std::fs::remove_file(&dst_path)
                .with_context(|| format!("failed to remove stale file: {}", dst_path.display()))?;
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
    /// SVN trunk path to strip from diff paths (e.g. "trunk").
    /// Empty means no stripping (custom layout or branch-level import).
    pub trunk_path: String,
}

/// Run a full SVN history import, replaying every revision as a Git commit.
///
/// Progress is updated in real-time via `progress` and optionally broadcast
/// via `ws_broadcast` for the web UI.
pub struct ImportRunState {
    pub progress: Arc<RwLock<ImportProgress>>,
    pub ws_broadcast: Option<broadcast::Sender<String>>,
    pub repo_id: Option<String>,
    /// Present for durable per-repository imports; absent for the setup wizard.
    pub operation_id: Option<String>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
}

#[derive(Debug)]
pub enum ImportOutcome {
    Completed {
        commits: u64,
        svn_rev: i64,
        git_sha: String,
    },
    Cancelled {
        commits: u64,
    },
    ReconciliationRequired {
        commits: u64,
        reason: String,
    },
}

async fn stop_requested(
    progress: &Arc<RwLock<ImportProgress>>,
    signal: Option<&Arc<AtomicBool>>,
) -> bool {
    signal.is_some_and(|s| s.load(Ordering::Acquire)) || progress.read().await.cancel_requested
}

#[cfg(feature = "reliability-fixture")]
async fn fixture_barrier(
    stage: &str,
    repo: Option<&str>,
    signal: Option<&Arc<AtomicBool>>,
) -> bool {
    let Ok(root) = std::env::var("REPOSYNC_FIXTURE_ROOT") else {
        return false;
    };
    let Ok(dir) = std::env::var("REPOSYNC_IMPORT_BARRIER_DIR") else {
        return false;
    };
    let Ok(root) = Path::new(&root).canonicalize() else {
        return false;
    };
    let Ok(dir) = Path::new(&dir).canonicalize() else {
        return false;
    };
    if !dir.starts_with(root) || repo.is_none_or(|id| !dir.ends_with(id)) {
        return false;
    }
    std::fs::write(dir.join(format!("{stage}.ready")), b"ready").expect("fixture barrier ready");
    loop {
        if signal.is_some_and(|s| s.load(Ordering::Acquire)) {
            return true;
        }
        if dir.join(format!("{stage}.release")).exists() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct PublicationTarget<'a> {
    workdir: &'a Path,
    remote: &'a str,
    branch: &'a str,
    sha: &'a str,
    force: bool,
}

async fn publish_checked(
    db: &Database,
    repo: &str,
    op: &str,
    target: PublicationTarget<'_>,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<(), String> {
    let PublicationTarget {
        workdir,
        remote,
        branch,
        sha,
        force,
    } = target;
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        return Err("cancelled before publication".into());
    }
    db.begin_import_publication(repo, op, &format!("refs/heads/{branch}"), sha)
        .map_err(|e| format!("cannot persist publication intent: {e}"))?;
    let mut push = tokio::process::Command::new("git");
    push.arg("push");
    if force {
        push.arg(format!("--force-with-lease=refs/heads/{branch}:"));
    }
    push.arg(remote)
        .arg(branch)
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = crate::process::run(push, Duration::from_secs(300), cancel)
        .await
        .map_err(|e| format!("Git push outcome uncertain: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git push outcome uncertain (exit {:?})",
            output.status.code()
        ));
    }
    #[cfg(feature = "reliability-fixture")]
    if std::env::var("REPOSYNC_IMPORT_LOST_PUSH_REPLY")
        .ok()
        .as_deref()
        == Some(repo)
        && std::env::var("REPOSYNC_FIXTURE_ROOT")
            .ok()
            .and_then(|root| Path::new(&root).canonicalize().ok())
            .is_some_and(|root| {
                workdir
                    .canonicalize()
                    .is_ok_and(|workdir| workdir.starts_with(root))
            })
    {
        return Err("fixture: successful Git push reply lost before verification".into());
    }
    // A successful local exit is not the checkpoint proof: read the actual ref.
    let mut inspect = tokio::process::Command::new("git");
    inspect
        .args([
            "ls-remote",
            "--exit-code",
            remote,
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0");
    let observed = crate::process::run(inspect, Duration::from_secs(60), None)
        .await
        .map_err(|e| format!("published ref could not be verified: {e}"))?;
    let observed_sha = String::from_utf8_lossy(&observed.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    if !observed.status.success() || observed_sha != sha {
        return Err("published ref differs from intended local SHA".into());
    }
    db.confirm_import_publication(repo, op, sha)
        .map_err(|e| format!("published ref verified but receipt failed: {e}"))?;
    Ok(())
}

async fn import_cli_commit(
    workdir: &Path,
    message: &str,
    author_name: &str,
    author_email: &str,
    committer_name: &str,
    committer_email: &str,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<git2::Oid> {
    let mut add = tokio::process::Command::new("git");
    add.args(["add", "--all"]).current_dir(workdir);
    let output = crate::process::run(add, Duration::from_secs(120), cancel).await?;
    anyhow::ensure!(output.status.success(), "LFS-aware git add failed");
    let mut diff = tokio::process::Command::new("git");
    diff.args(["diff", "--cached", "--quiet"])
        .current_dir(workdir);
    let output = crate::process::run(diff, Duration::from_secs(60), cancel).await?;
    anyhow::ensure!(
        output.status.code() == Some(1),
        "no staged import changes or git diff failed"
    );
    let author = format!("{author_name} <{author_email}>");
    let mut commit = tokio::process::Command::new("git");
    commit
        .args(["commit", "-m", message, "--author", &author])
        .current_dir(workdir)
        .env("GIT_COMMITTER_NAME", committer_name)
        .env("GIT_COMMITTER_EMAIL", committer_email);
    let output = crate::process::run(commit, Duration::from_secs(120), cancel).await?;
    anyhow::ensure!(output.status.success(), "LFS-aware git commit failed");
    let mut rev = tokio::process::Command::new("git");
    rev.args(["rev-parse", "HEAD"]).current_dir(workdir);
    let output = crate::process::run(rev, Duration::from_secs(30), None).await?;
    anyhow::ensure!(output.status.success(), "could not read new local commit");
    Ok(git2::Oid::from_str(
        String::from_utf8_lossy(&output.stdout).trim(),
    )?)
}

async fn import_lfs_command(
    args: &[&str],
    workdir: &Path,
    cancel: Option<&Arc<AtomicBool>>,
) -> std::io::Result<std::process::Output> {
    let mut command = crate::process::import_git_command()?;
    command.args(args).current_dir(workdir);
    crate::process::run(command, Duration::from_secs(60), cancel).await
}

pub async fn run_full_import(
    svn_client: &SvnClient,
    git_client: &Arc<std::sync::Mutex<GitClient>>,
    identity_mapper: &IdentityMapper,
    db: &Database,
    file_policy: &FilePolicy,
    import_config: &ImportConfig,
    run_state: ImportRunState,
) -> Result<ImportOutcome> {
    let ImportRunState {
        progress,
        ws_broadcast,
        repo_id,
        operation_id,
        cancel_signal,
    } = run_state;

    if stop_requested(&progress, cancel_signal.as_ref()).await {
        let mut p = progress.write().await;
        p.phase = ImportPhase::Cancelled;
        p.completed_at = Some(chrono::Utc::now().to_rfc3339());
        return Ok(ImportOutcome::Cancelled { commits: 0 });
    }
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
        let rp = {
            let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
            git_guard.repo_workdir()
        };
        let preflight = if operation_id.is_some() {
            match import_lfs_command(&["lfs", "version"], &rp, cancel_signal.as_ref()).await {
                Ok(output) if output.status.success() => {
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
                }
                Ok(output) => Err(format!(
                    "git lfs version failed (exit {:?}): {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::Interrupted
                        && stop_requested(&progress, cancel_signal.as_ref()).await
                    {
                        return Ok(ImportOutcome::Cancelled { commits: 0 });
                    }
                    return Err(e).context("LFS preflight did not quiesce safely");
                }
            }
        } else {
            crate::lfs::preflight_check()
        };
        match preflight {
            Ok(version) => {
                log(
                    &progress,
                    &ws_broadcast,
                    format!("[info] Git LFS available: {}", version),
                )
                .await;

                // Install LFS hooks/filters in the repo so `git add` invokes
                // the clean filter and creates pointer files for tracked patterns.
                let install = if operation_id.is_some() {
                    match import_lfs_command(
                        &["lfs", "install", "--local"],
                        &rp,
                        cancel_signal.as_ref(),
                    )
                    .await
                    {
                        Ok(output) if output.status.success() => Ok(()),
                        Ok(output) => Err(format!(
                            "git lfs install failed (exit {:?}): {}",
                            output.status.code(),
                            String::from_utf8_lossy(&output.stderr).trim()
                        )),
                        Err(e) => {
                            return Err(e)
                                .context("LFS hook installation outcome requires inspection")
                        }
                    }
                } else {
                    crate::lfs::install_lfs_hooks(&rp)
                };
                match install {
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
                            format!(
                                "[warn] git lfs install failed: {} — LFS tracking will not work",
                                e
                            ),
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
                    format!(
                        "[warn] Git LFS not available: {} — large files will be committed directly",
                        e
                    ),
                )
                .await;
                false
            }
        }
    } else {
        false
    };

    // Get SVN info
    #[cfg(feature = "reliability-fixture")]
    if fixture_barrier("connecting", repo_id.as_deref(), cancel_signal.as_ref()).await {
        return Ok(ImportOutcome::Cancelled { commits: 0 });
    }
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

    let svn_info = match svn_client.info().await {
        Ok(info) => info,
        Err(_) if stop_requested(&progress, cancel_signal.as_ref()).await => {
            return Ok(ImportOutcome::Cancelled { commits: 0 });
        }
        Err(e) => return Err(e).context("failed to get SVN info"),
    };
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

    if stop_requested(&progress, cancel_signal.as_ref()).await {
        return Ok(ImportOutcome::Cancelled { commits: 0 });
    }
    let log_entries = match svn_client.log(1, head_rev).await {
        Ok(entries) => entries,
        Err(_) if stop_requested(&progress, cancel_signal.as_ref()).await => {
            return Ok(ImportOutcome::Cancelled { commits: 0 });
        }
        Err(e) => return Err(e).context("failed to get SVN log"),
    };

    if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
        db.note_import_total(repo, op, log_entries.len() as u64)?;
    }

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
        if stop_requested(&progress, cancel_signal.as_ref()).await {
            {
                let mut p = progress.write().await;
                p.phase = ImportPhase::Cancelled;
                p.completed_at = Some(chrono::Utc::now().to_rfc3339());
            }
            log(
                &progress,
                &ws_broadcast,
                "[warn] Import stopped; already published commits are not undone".into(),
            )
            .await;
            return Ok(ImportOutcome::Cancelled { commits: count });
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
                if operation_id.is_some() {
                    return Err(e).context("import export directory unavailable");
                }
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
                    // Strip trunk prefix from diff paths so they match the
                    // git repo layout (e.g. "a/trunk/source/..." → "a/source/...")
                    let processed_diff = if !import_config.trunk_path.is_empty() {
                        let tp = import_config.trunk_path.trim_matches('/');
                        diff_text
                            .replace(&format!("a/{}/", tp), "a/")
                            .replace(&format!("b/{}/", tp), "b/")
                    } else {
                        diff_text
                    };
                    let apply = if let Some(signal) = cancel_signal.as_ref() {
                        crate::sync_engine::apply_diff_to_path_for_import(
                            &repo_path,
                            &processed_diff,
                            signal,
                        )
                        .await
                    } else {
                        crate::sync_engine::apply_diff_to_path(&repo_path, &processed_diff).await
                    };
                    match apply {
                        Ok(()) => {
                            used_incremental = true;
                            debug!(rev, "applied incremental SVN diff");
                        }
                        Err(e) => {
                            if stop_requested(&progress, cancel_signal.as_ref()).await {
                                if matches!(&e, crate::errors::GitError::IoError(io)
                                    if io.kind() == std::io::ErrorKind::Interrupted)
                                {
                                    return Ok(ImportOutcome::Cancelled { commits: count });
                                }
                                return Err(e)
                                    .context("Git apply stopped without confirmed quiescence");
                            }
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
            if stop_requested(&progress, cancel_signal.as_ref()).await {
                return Ok(ImportOutcome::Cancelled { commits: count });
            }
            if let Err(e) = svn_client.export("", rev, export_dir.path()).await {
                if stop_requested(&progress, cancel_signal.as_ref()).await {
                    return Ok(ImportOutcome::Cancelled { commits: count });
                }
                let msg = format!("[error] r{}: SVN export failed: {}", rev, e);
                log(&progress, &ws_broadcast, msg.clone()).await;
                let mut p = progress.write().await;
                p.errors.push(msg);
                if operation_id.is_some() {
                    return Err(e).context("SVN export failed");
                }
                continue;
            }

            // Remove stale files from Git working tree
            if let Err(e) = remove_stale_files(export_dir.path(), &repo_path) {
                let msg = format!("[warn] r{}: failed to remove stale files: {}", rev, e);
                log(&progress, &ws_broadcast, msg).await;
            }
        }

        if stop_requested(&progress, cancel_signal.as_ref()).await {
            return Ok(ImportOutcome::Cancelled { commits: count });
        }

        // Copy with policy enforcement (only needed for full export path)
        let copy_stats = if used_incremental {
            CopyStats::default()
        } else {
            match copy_tree_with_policy(export_dir.path(), &repo_path, file_policy, db) {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("[error] r{}: copy failed: {}", rev, e);
                    log(&progress, &ws_broadcast, msg.clone()).await;
                    let mut p = progress.write().await;
                    p.errors.push(msg);
                    if operation_id.is_some() {
                        return Err(e).context("import copy failed");
                    }
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
                (entry.author.clone(), format!("{}@svn", entry.author))
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
        if stop_requested(&progress, cancel_signal.as_ref()).await {
            return Ok(ImportOutcome::Cancelled { commits: count });
        }
        let push_repo_path = {
            let git_client_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
            git_client_guard.repo_workdir()
        };
        let commit_result: Result<git2::Oid> = if use_cli && operation_id.is_some() {
            import_cli_commit(
                &push_repo_path,
                &message,
                &author_name,
                &author_email,
                &import_config.committer_name,
                &import_config.committer_email,
                cancel_signal.as_ref(),
            )
            .await
        } else {
            let git_client_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
            if use_cli {
                git_client_guard
                    .commit_via_cli(
                        &message,
                        &author_name,
                        &author_email,
                        &import_config.committer_name,
                        &import_config.committer_email,
                    )
                    .map_err(Into::into)
            } else {
                git_client_guard
                    .commit(
                        &message,
                        &author_name,
                        &author_email,
                        &import_config.committer_name,
                        &import_config.committer_email,
                    )
                    .map_err(Into::into)
            }
        };
        match commit_result {
            Ok(oid) => {
                let sha = oid.to_string();
                if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
                    db.note_import_local(repo, op, rev, &sha, idx as u64 + 1, count + 1)
                        .context("failed to persist local import progress")?;
                }
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
                    entry
                        .message
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect::<String>(),
                    details,
                );
                log(&progress, &ws_broadcast, log_line).await;

                // Record in DB (commit_map for bidirectional mapping)
                let map_result = db.insert_commit_map(
                    rev,
                    &sha,
                    "svn_to_git",
                    &entry.author,
                    &format!("{} <{}>", author_name, author_email),
                );
                if operation_id.is_some() {
                    map_result.context("failed to persist import mapping")?;
                }

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
                    if operation_id.is_some() {
                        return Err(e).context("failed to persist import sync record");
                    }
                    debug!(rev, error = %e, "failed to insert sync_record during import");
                } else {
                    debug!(rev, sha = %short_sha, "import: sync_record created");
                }

                count += 1;
                commits_since_push += 1;
                #[cfg(feature = "reliability-fixture")]
                if count == 1
                    && fixture_barrier(
                        "after_first_local",
                        repo_id.as_deref(),
                        cancel_signal.as_ref(),
                    )
                    .await
                {
                    return Ok(ImportOutcome::Cancelled { commits: count });
                }
                {
                    let mut p = progress.write().await;
                    p.commits_created = count;
                }

                let repo_path = push_repo_path;

                // Incremental push every PUSH_BATCH_SIZE commits
                if commits_since_push >= PUSH_BATCH_SIZE {
                    if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
                        if stop_requested(&progress, cancel_signal.as_ref()).await {
                            return Ok(ImportOutcome::Cancelled { commits: count });
                        }
                        let force = progress.read().await.batches_pushed == 0;
                        if let Err(reason) = publish_checked(
                            db,
                            repo,
                            op,
                            PublicationTarget {
                                workdir: &repo_path,
                                remote: &import_config.remote_name,
                                branch: &import_config.branch,
                                sha: &sha,
                                force,
                            },
                            cancel_signal.as_ref(),
                        )
                        .await
                        {
                            return Ok(ImportOutcome::ReconciliationRequired {
                                commits: count,
                                reason,
                            });
                        }
                        progress.write().await.batches_pushed += 1;
                    } else {
                        let is_first_push = {
                            let p = progress.read().await;
                            p.batches_pushed == 0
                        };
                        let push_type = if is_first_push { "force-push" } else { "push" };
                        log(
                            &progress,
                            &ws_broadcast,
                            format!(
                                "[info] {} batch of {} commits to remote...",
                                push_type, commits_since_push
                            ),
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
                                    format!(
                                        "[push] still uploading... ({}m {}s elapsed)",
                                        elapsed / 60,
                                        elapsed % 60
                                    ),
                                )
                                .await;
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
                                        log(
                                            &progress,
                                            &ws_broadcast,
                                            format!("[push] {}", trimmed),
                                        )
                                        .await;
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
                                        warn!(
                                        "failed to persist import progress after batch push: {}",
                                        e
                                    );
                                    }
                                }
                                log(
                                    &progress,
                                    &ws_broadcast,
                                    format!(
                                        "[ok] Batch pushed ({} of {} total commits)",
                                        count,
                                        log_entries.len()
                                    ),
                                )
                                .await;
                            }
                            Ok(Err(e)) => {
                                let msg =
                                    format!("[warn] Batch push failed (will retry at end): {}", e);
                                log(&progress, &ws_broadcast, msg).await;
                            }
                            Err(e) => {
                                let msg = format!("[warn] Batch push task panicked: {}", e);
                                log(&progress, &ws_broadcast, msg).await;
                            }
                        }
                    }
                    commits_since_push = 0;
                }
            }
            Err(e) => {
                if use_cli && operation_id.is_some() {
                    return Ok(ImportOutcome::ReconciliationRequired {
                        commits: count,
                        reason: format!(
                            "LFS-aware local commit stopped with uncertain local state: {e}"
                        ),
                    });
                }
                if operation_id.is_some() {
                    return Err(e)
                        .context("Git import commit failed; local work requires inspection");
                }
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
                warn!(
                    "failed to persist import progress at rev {}: {}",
                    idx + 1,
                    e
                );
            }
        }
    }

    // Push remaining commits (those since last batch push)
    if commits_since_push > 0 {
        if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
            #[cfg(feature = "reliability-fixture")]
            if fixture_barrier("before_final_push", Some(repo), cancel_signal.as_ref()).await {
                return Ok(ImportOutcome::Cancelled { commits: count });
            }
            if stop_requested(&progress, cancel_signal.as_ref()).await {
                return Ok(ImportOutcome::Cancelled { commits: count });
            }
            progress.write().await.phase = ImportPhase::FinalPush;
            let (repo_path, sha) = {
                let git = git_client.lock().unwrap_or_else(|p| p.into_inner());
                (
                    git.repo_workdir(),
                    git.get_head_sha().context("missing local import tip")?,
                )
            };
            let force = progress.read().await.batches_pushed == 0;
            if let Err(reason) = publish_checked(
                db,
                repo,
                op,
                PublicationTarget {
                    workdir: &repo_path,
                    remote: &import_config.remote_name,
                    branch: &import_config.branch,
                    sha: &sha,
                    force,
                },
                cancel_signal.as_ref(),
            )
            .await
            {
                return Ok(ImportOutcome::ReconciliationRequired {
                    commits: count,
                    reason,
                });
            }
            progress.write().await.batches_pushed += 1;
        } else {
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
                    if force {
                        args.push("--force".to_string());
                    }
                    args.push(remote);
                    args.push(branch);
                    let output = std::process::Command::new("git")
                        .args(&args)
                        .current_dir(&rp)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .output();
                    match output {
                        Ok(out) if out.status.success() => {
                            Ok(String::from_utf8_lossy(&out.stderr).to_string())
                        }
                        Ok(out) => Err(format!(
                            "exit {:?}: {}",
                            out.status.code(),
                            String::from_utf8_lossy(&out.stderr).trim()
                        )),
                        Err(e) => Err(format!("spawn failed: {}", e)),
                    }
                })
                .await;

                match push_result {
                    Ok(Ok(stderr)) => {
                        for line in stderr.lines() {
                            let t = line.trim();
                            if !t.is_empty() {
                                log(&progress, &ws_broadcast, format!("[push] {}", t)).await;
                            }
                        }
                        {
                            let mut p = progress.write().await;
                            p.batches_pushed += 1;
                        }
                        log(
                            &progress,
                            &ws_broadcast,
                            format!("[ok] All {} commits pushed successfully", count),
                        )
                        .await;
                        push_success = true;
                        break;
                    }
                    Ok(Err(e)) => {
                        let _msg = format!(
                            "[warn] Push attempt {}/{} failed: {}",
                            attempt, max_retries, e
                        );
                    }
                    Err(e) => {
                        let msg = format!(
                            "[warn] Push attempt {}/{} failed (panic): {}",
                            attempt, max_retries, e
                        );
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
    }

    // Persist progress before final watermarks
    {
        let p = progress.read().await;
        if let Err(e) = db.persist_import_progress(&p) {
            warn!("failed to persist import progress before watermarks: {}", e);
        }
    }

    let last_rev = log_entries.last().map(|e| e.revision).unwrap_or(0);
    let sha = {
        let git = git_client.lock().unwrap_or_else(|p| p.into_inner());
        git.get_head_sha().context("missing final Git import tip")?
    };
    if stop_requested(&progress, cancel_signal.as_ref()).await {
        let all_confirmed = match (&repo_id, &operation_id) {
            (Some(repo), Some(op_id)) => db.get_import_operation(repo, op_id)?.is_some_and(|op| {
                op.last_confirmed_svn_rev == Some(last_rev)
                    && op.last_confirmed_git_sha.as_deref() == Some(sha.as_str())
                    && op.intended_git_sha.is_none()
            }),
            _ => false,
        };
        if !all_confirmed {
            return Ok(ImportOutcome::Cancelled { commits: count });
        }
    }
    if operation_id.is_none() {
        db.set_watermark("svn_rev", &last_rev.to_string())?;
        db.set_watermark("git_sha", &sha)?;
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
    Ok(ImportOutcome::Completed {
        commits: count,
        svn_rev: last_rev,
        git_sha: sha,
    })
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
                                    push_log_line(
                                        progress,
                                        ws_broadcast,
                                        format!("[push] {}", trimmed),
                                    )
                                    .await;
                                }
                                // Skip the 100% lines — redundant with Total
                            }
                        } else {
                            push_log_line(progress, ws_broadcast, format!("[push] {}", trimmed))
                                .await;
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

    let status = child.wait().await.context("failed to wait for git push")?;

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
