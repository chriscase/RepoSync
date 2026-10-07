//! Shared full-history and snapshot import module.
//!
//! Provides [`run_full_import`] which replays every SVN revision as an
//! individual Git commit, and [`run_snapshot_import`] which materializes one
//! pinned SVN revision through the same file-policy helpers used by personal
//! `ImportMode::Snapshot`.
//!
//! Also re-exports [`copy_tree_with_policy`] so both personal-mode and
//! team-mode code can share the file-copy logic. The copier is no-follow,
//! preserves ordinary dotfiles, excludes only reserved VCS metadata, and
//! rejects regular files with `nlink > 1` before reading or publishing them.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

use crate::db::import_operations::{
    import_target_fingerprint, ImportOperation, ImportOperationState,
};
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::file_policy::{FilePolicy, FilePolicyDecision};
use crate::git::remote_url;
use crate::git::GitClient;
use crate::identity::mapper::{GitIdentity, IdentityMapper};
use crate::models::Repository;
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

/// Reserved VCS metadata names. These are excluded at any depth so an export
/// cannot overwrite destination `.git/` (or similar) and so nested VCS dirs
/// are not published. Ordinary dotfiles (`.gitignore`, `.editorconfig`,
/// `.github/…`) are not reserved and are copied.
pub const RESERVED_VCS_METADATA_NAMES: &[&str] = &[".git", ".svn", ".hg", ".bzr"];

/// True when `name` is documented reserved VCS metadata (not an ordinary dotfile).
pub fn is_reserved_vcs_metadata(name: &OsStr) -> bool {
    RESERVED_VCS_METADATA_NAMES.iter().any(|n| name == *n)
}

/// Destination names `remove_stale_files` must keep even when they are absent
/// from the SVN export. Only reserved VCS metadata (`.git`, `.svn`, `.hg`,
/// `.bzr`). `.gitattributes` is not name-protected: engine LFS output is
/// reconciled from patterns [`crate::lfs::ensure_lfs_tracked`] recorded under
/// `.git/`, and any other `.gitattributes` is removed when the export omits it.
pub fn is_stale_remove_protected(name: &OsStr) -> bool {
    is_reserved_vcs_metadata(name)
}

/// Recursively copy files from SVN export `src` into Git working tree `dst`,
/// enforcing the given [`FilePolicy`].
///
/// Traversal is no-follow (`symlink_metadata` / `O_NOFOLLOW`). Symlinks,
/// specials, directory cycles, and regular files with `nlink > 1` (hardlink
/// aliases that may name outside-root bytes) are rejected before the target
/// is read or published. Reserved VCS metadata is excluded and recorded;
/// ordinary dotfiles are preserved.
pub fn copy_tree_with_policy(
    src: &Path,
    dst: &Path,
    policy: &FilePolicy,
    db: &Database,
) -> Result<CopyStats> {
    let mut stats = CopyStats::default();
    let mut visited = HashSet::new();
    admit_export_root(src, &mut visited)?;
    let mut ctx = CopyTreePolicyCtx {
        dst_root: dst,
        export_root: src,
        policy,
        db,
        stats: &mut stats,
        visited: &mut visited,
    };
    copy_tree_policy_inner(src, dst, &mut ctx)?;
    if stats.lfs_tracked > 0 {
        refresh_export_present_engine_lfs(dst, src)?;
    }
    record_copy_exclusions(db, &stats);
    Ok(stats)
}

/// After copy, re-merge export `.gitattributes` with engine LFS lines when this
/// pass tracked large files. `ensure_lfs_tracked` may have run before the
/// export file was copied, so refresh once the export body is in place.
fn refresh_export_present_engine_lfs(dst_root: &Path, export_root: &Path) -> Result<()> {
    let export_gitattr = export_root.join(".gitattributes");
    let export_meta = match std::fs::symlink_metadata(&export_gitattr) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "failed to stat exported .gitattributes without following: {}",
                    export_gitattr.display()
                )
            });
        }
        Ok(meta) => meta,
    };
    if export_meta.file_type().is_symlink() {
        bail!(
            "unsupported symlink at exported .gitattributes: refusing to follow outside the export root"
        );
    }
    if !export_meta.file_type().is_file() {
        return Ok(());
    }
    let export_body = read_regular_file_no_follow(&export_gitattr).with_context(|| {
        format!(
            "failed to read exported .gitattributes: {}",
            export_gitattr.display()
        )
    })?;
    let dst_gitattr = dst_root.join(".gitattributes");
    let engine_body = crate::lfs::engine_gitattributes_body(dst_root)
        .with_context(|| format!("failed to read engine LFS marker in {}", dst_root.display()))?;
    let merged = crate::lfs::merge_export_present_gitattributes(
        &export_body,
        None,
        engine_body.as_deref(),
        true,
    );
    write_root_gitattributes_regular_file(&dst_gitattr, &merged)?;
    Ok(())
}

/// Shared no-follow copy context. Kept off the recursive signature so
/// `copy_tree_policy_inner` stays under the clippy argument limit without
/// changing traversal, rejection, or publish behavior.
struct CopyTreePolicyCtx<'a> {
    dst_root: &'a Path,
    export_root: &'a Path,
    policy: &'a FilePolicy,
    db: &'a Database,
    stats: &'a mut CopyStats,
    visited: &'a mut HashSet<(u64, u64)>,
}

fn record_copy_exclusions(db: &Database, stats: &CopyStats) {
    if stats.skipped > 0 {
        let _ = db.insert_audit_log(
            "file_policy_skip",
            Some("svn_to_git"),
            None,
            None,
            None,
            Some(&format!(
                "Skipped {} files by policy during SVN→Git copy: {}",
                stats.skipped,
                stats.exclusions.join("; ")
            )),
            true,
        );
    }
    if stats.reserved_excluded > 0 {
        let reserved: Vec<&str> = stats
            .exclusions
            .iter()
            .filter(|e| e.starts_with("reserved:"))
            .map(String::as_str)
            .collect();
        let _ = db.insert_audit_log(
            "reserved_metadata_exclude",
            Some("svn_to_git"),
            None,
            None,
            None,
            Some(&format!(
                "Excluded {} reserved VCS metadata path(s): {}",
                stats.reserved_excluded,
                reserved.join("; ")
            )),
            true,
        );
    }
}

/// Statistics from a copy operation.
#[derive(Debug, Default, Clone)]
pub struct CopyStats {
    pub copied: usize,
    pub skipped: usize,
    pub lfs_tracked: usize,
    /// Deliberate reserved-VCS exclusions (not file-policy skips).
    pub reserved_excluded: usize,
    /// Every deliberate exclusion, e.g. `reserved:.svn` or `policy:ignored:tmp.log`.
    pub exclusions: Vec<String>,
}

fn admit_export_root(src: &Path, visited: &mut HashSet<(u64, u64)>) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)
        .with_context(|| format!("failed to stat export root: {}", src.display()))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        bail!(
            "unsupported symlink at export root '{}': refusing to follow outside the export root",
            src.display()
        );
    }
    if !ft.is_dir() {
        bail!(
            "export root '{}' is not a directory (type={})",
            src.display(),
            file_type_label(&ft)
        );
    }
    if let Some(id) = dir_identity(&meta) {
        visited.insert(id);
    }
    Ok(())
}

fn copy_tree_policy_inner(src: &Path, dst: &Path, ctx: &mut CopyTreePolicyCtx<'_>) -> Result<()> {
    let entries = std::fs::read_dir(src)
        .with_context(|| format!("failed to read directory: {}", src.display()))?;

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        let rel = src_path
            .strip_prefix(ctx.export_root)
            .unwrap_or(&src_path)
            .to_string_lossy()
            .replace('\\', "/");

        let meta = std::fs::symlink_metadata(&src_path).with_context(|| {
            format!(
                "failed to stat export entry without following: {}",
                src_path.display()
            )
        })?;
        let ft = meta.file_type();

        if is_reserved_vcs_metadata(&file_name) {
            let reason = format!("reserved:{rel}");
            info!(path = rel.as_str(), "excluding reserved VCS metadata");
            ctx.stats.reserved_excluded += 1;
            ctx.stats.exclusions.push(reason);
            continue;
        }

        if ft.is_symlink() {
            bail!("unsupported symlink at '{rel}': refusing to follow outside the export root");
        }
        if is_special_file_type(&ft) {
            bail!(
                "unsupported special file at '{rel}' ({})",
                file_type_label(&ft)
            );
        }
        if ft.is_dir() {
            if let Some(id) = dir_identity(&meta) {
                if !ctx.visited.insert(id) {
                    bail!("cycle detected at '{rel}': refusing to re-enter directory");
                }
            }
            if !dst_path.exists() {
                std::fs::create_dir_all(&dst_path).with_context(|| {
                    format!("failed to create directory: {}", dst_path.display())
                })?;
            }
            copy_tree_policy_inner(&src_path, &dst_path, ctx)?;
            continue;
        }
        if !ft.is_file() {
            bail!(
                "unsupported file type at '{rel}' ({})",
                file_type_label(&ft)
            );
        }
        if is_hardlink_alias(&meta) {
            reject_hardlink_alias(ctx.db, ctx.stats, &rel)?;
        }

        // Size comes from no-follow metadata; never call evaluate_path (it follows).
        let decision = ctx.policy.evaluate(&rel, meta.len());
        match &decision {
            FilePolicyDecision::Allow => {
                copy_regular_file_no_follow(&src_path, &dst_path, &meta)?;
                ctx.stats.copied += 1;
            }
            FilePolicyDecision::LfsTrack { size, threshold } => {
                copy_regular_file_no_follow(&src_path, &dst_path, &meta)?;
                let pattern = crate::lfs::pattern_for_path(&rel);
                if let Err(e) = crate::lfs::ensure_lfs_tracked(ctx.dst_root, &pattern) {
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
                ctx.stats.copied += 1;
                ctx.stats.lfs_tracked += 1;
            }
            FilePolicyDecision::Ignored { pattern } => {
                warn!(
                    path = rel.as_str(),
                    pattern = pattern.as_str(),
                    "file ignored by policy — not copied to Git"
                );
                ctx.stats.skipped += 1;
                ctx.stats.exclusions.push(format!("policy:ignored:{rel}"));
            }
            FilePolicyDecision::Oversize { size, limit } => {
                warn!(
                    path = rel.as_str(),
                    size, limit, "file exceeds max_file_size — not copied to Git"
                );
                ctx.stats.skipped += 1;
                ctx.stats.exclusions.push(format!("policy:oversize:{rel}"));
            }
        }
    }

    Ok(())
}

fn copy_regular_file_no_follow(src: &Path, dst: &Path, src_meta: &std::fs::Metadata) -> Result<()> {
    unlink_dest_symlink_before_copy(dst)?;
    let mut reader = open_no_follow_read(src)?;
    let mut writer = open_no_follow_write(dst)?;
    std::io::copy(&mut reader, &mut writer)
        .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
    writer
        .flush()
        .with_context(|| format!("failed to flush {}", dst.display()))?;
    apply_source_permissions(dst, src_meta)?;
    Ok(())
}

/// Remove a planted destination symlink before publish. Applies to every copied
/// path, not only root `.gitattributes`.
fn unlink_dest_symlink_before_copy(dst: &Path) -> Result<()> {
    crate::lfs::unlink_gitattributes_symlink(dst).with_context(|| {
        format!(
            "failed to unlink planted destination symlink before copy: {}",
            dst.display()
        )
    })
}

/// Open a destination for write without following symlinks. Uses `O_NOFOLLOW`
/// and create-exclusive for new paths so a symlink raced in after unlink is
/// refused instead of written through.
fn open_no_follow_write(dst: &Path) -> Result<std::fs::File> {
    match std::fs::symlink_metadata(dst) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "refusing to write through symlink at {}: destination must be a regular file",
                dst.display()
            );
        }
        Ok(meta) if meta.file_type().is_file() => {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.custom_flags(libc::O_NOFOLLOW);
            }
            opts.open(dst).with_context(|| {
                format!(
                    "failed to open {} for write without following",
                    dst.display()
                )
            })
        }
        Ok(_) => {
            bail!(
                "refusing to overwrite non-regular file at {}",
                dst.display()
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.custom_flags(libc::O_NOFOLLOW);
            }
            opts.open(dst)
                .with_context(|| format!("failed to create {} without following", dst.display()))
        }
        Err(e) => Err(e).with_context(|| {
            format!(
                "failed to stat {} without following before write",
                dst.display()
            )
        }),
    }
}

fn open_no_follow_read(path: &Path) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
        .with_context(|| format!("failed to open {} without following", path.display()))
}

fn apply_source_permissions(dst: &Path, src_meta: &std::fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dst,
            std::fs::Permissions::from_mode(src_meta.permissions().mode()),
        )
        .with_context(|| format!("failed to set permissions on {}", dst.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (dst, src_meta);
    }
    Ok(())
}

fn dir_identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn is_special_file_type(ft: &std::fs::FileType) -> bool {
    !ft.is_dir() && !ft.is_file() && !ft.is_symlink()
}

/// Regular files with more than one directory entry can alias bytes that live
/// outside the export root. `symlink_metadata` still reports them as ordinary
/// files, so link count is the fail-closed signal. Directories are not
/// aliases: their link count includes `.` and child subdirectories.
fn is_hardlink_alias(meta: &std::fs::Metadata) -> bool {
    if !meta.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Record the hardlink rejection, then fail closed before any read or publish.
fn reject_hardlink_alias(db: &Database, stats: &mut CopyStats, rel: &str) -> Result<()> {
    let decision = format!("unsupported:hardlink:{rel}");
    warn!(
        path = rel,
        decision = decision.as_str(),
        "rejecting hardlink alias before read or publish"
    );
    stats.exclusions.push(decision);
    let _ = db.insert_audit_log(
        "unsupported_hardlink_reject",
        Some("svn_to_git"),
        None,
        None,
        None,
        Some(&format!(
            "Rejected hardlink alias at '{rel}' (nlink>1): refusing to read or publish a file that may alias outside-root bytes"
        )),
        false,
    );
    bail!(
        "unsupported hardlink at '{rel}' (nlink>1): refusing to read or publish a file that may alias outside-root bytes"
    );
}

fn file_type_label(ft: &std::fs::FileType) -> &'static str {
    if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "directory"
    } else if ft.is_file() {
        "file"
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if ft.is_fifo() {
                return "fifo";
            }
            if ft.is_socket() {
                return "socket";
            }
            if ft.is_char_device() {
                return "char_device";
            }
            if ft.is_block_device() {
                return "block_device";
            }
        }
        "special"
    }
}

/// One independently observed export entry. Built without calling
/// [`copy_tree_with_policy`], so a follow-y double-copy cannot hide itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestEntry {
    File {
        mode: u32,
        size: u64,
        sha256: String,
    },
    Directory {
        mode: u32,
    },
    Reserved {
        name: String,
    },
    Unsupported {
        kind: String,
        detail: String,
    },
}

/// No-follow inventory of an export tree (paths, types/modes, bytes or
/// an explicit unsupported outcome).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndependentTreeManifest {
    pub entries: BTreeMap<String, ManifestEntry>,
}

impl IndependentTreeManifest {
    pub fn has_unsupported(&self) -> bool {
        self.entries
            .values()
            .any(|e| matches!(e, ManifestEntry::Unsupported { .. }))
    }

    /// True when any regular-file digest equals SHA-256 of `needle`.
    pub fn contains_file_digest_of(&self, needle: &[u8]) -> bool {
        let digest = hex::encode(Sha256::digest(needle));
        self.entries.values().any(|e| match e {
            ManifestEntry::File { sha256, .. } => sha256 == &digest,
            _ => false,
        })
    }

    pub fn unsupported_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|(path, entry)| match entry {
                ManifestEntry::Unsupported { .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Walk `root` with `symlink_metadata` only. Never follows links, never
/// calls [`copy_tree_with_policy`], and never opens a non-regular file or a
/// regular file with `nlink > 1`.
pub fn independent_tree_manifest(root: &Path) -> Result<IndependentTreeManifest> {
    let mut manifest = IndependentTreeManifest::default();
    let mut visited = HashSet::new();
    let root_meta = std::fs::symlink_metadata(root).with_context(|| {
        format!(
            "failed to stat independent-manifest root {}",
            root.display()
        )
    })?;
    if root_meta.file_type().is_symlink() {
        manifest.entries.insert(
            String::new(),
            ManifestEntry::Unsupported {
                kind: "symlink".into(),
                detail: "export root is a symlink".into(),
            },
        );
        return Ok(manifest);
    }
    if !root_meta.file_type().is_dir() {
        manifest.entries.insert(
            String::new(),
            ManifestEntry::Unsupported {
                kind: file_type_label(&root_meta.file_type()).into(),
                detail: "export root is not a directory".into(),
            },
        );
        return Ok(manifest);
    }
    if let Some(id) = dir_identity(&root_meta) {
        visited.insert(id);
    }
    independent_manifest_inner(root, root, &mut visited, &mut manifest)?;
    Ok(manifest)
}

fn independent_manifest_inner(
    dir: &Path,
    root: &Path,
    visited: &mut HashSet<(u64, u64)>,
    manifest: &mut IndependentTreeManifest,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {} for independent manifest", dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let meta = std::fs::symlink_metadata(&path).with_context(|| {
            format!("failed to stat {} for independent manifest", path.display())
        })?;
        let ft = meta.file_type();
        if is_reserved_vcs_metadata(&name) {
            manifest.entries.insert(
                rel,
                ManifestEntry::Reserved {
                    name: name.to_string_lossy().into_owned(),
                },
            );
            continue;
        }
        if ft.is_symlink() {
            manifest.entries.insert(
                rel,
                ManifestEntry::Unsupported {
                    kind: "symlink".into(),
                    detail: "symlink (target not read)".into(),
                },
            );
            continue;
        }
        if is_special_file_type(&ft) {
            manifest.entries.insert(
                rel,
                ManifestEntry::Unsupported {
                    kind: file_type_label(&ft).into(),
                    detail: "special file (not opened)".into(),
                },
            );
            continue;
        }
        if ft.is_dir() {
            if let Some(id) = dir_identity(&meta) {
                if !visited.insert(id) {
                    manifest.entries.insert(
                        rel,
                        ManifestEntry::Unsupported {
                            kind: "cycle".into(),
                            detail: "directory cycle (not re-entered)".into(),
                        },
                    );
                    continue;
                }
            }
            manifest.entries.insert(
                rel.clone(),
                ManifestEntry::Directory {
                    mode: unix_mode(&meta),
                },
            );
            independent_manifest_inner(&path, root, visited, manifest)?;
            continue;
        }
        if !ft.is_file() {
            manifest.entries.insert(
                rel,
                ManifestEntry::Unsupported {
                    kind: file_type_label(&ft).into(),
                    detail: "unsupported type (not opened)".into(),
                },
            );
            continue;
        }
        if is_hardlink_alias(&meta) {
            manifest.entries.insert(
                rel,
                ManifestEntry::Unsupported {
                    kind: "hardlink".into(),
                    detail: "nlink>1 (not opened)".into(),
                },
            );
            continue;
        }
        let (size, sha256) = hash_regular_file_no_follow(&path)?;
        manifest.entries.insert(
            rel,
            ManifestEntry::File {
                mode: unix_mode(&meta),
                size,
                sha256,
            },
        );
    }
    Ok(())
}

fn hash_regular_file_no_follow(path: &Path) -> Result<(u64, String)> {
    let mut file = open_no_follow_read(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf).with_context(|| {
            format!("failed to read {} for independent manifest", path.display())
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn unix_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

/// Verify `dest` against an independently built source manifest. Does not
/// call [`copy_tree_with_policy`] to invent expectations. A destination that
/// published a followed symlink or a hardlink alias (outside-root canary)
/// fails.
pub fn verify_against_independent_manifest(
    dest: &Path,
    source_manifest: &IndependentTreeManifest,
    policy: &FilePolicy,
) -> Result<()> {
    let dest_manifest = independent_tree_manifest(dest)?;
    if dest_manifest.has_unsupported() {
        bail!(
            "destination published unsupported entries: {:?}",
            dest_manifest.unsupported_paths()
        );
    }

    for (path, entry) in &source_manifest.entries {
        match entry {
            ManifestEntry::Unsupported { kind, detail } => {
                if dest_manifest.entries.contains_key(path) {
                    bail!("destination published unsupported {kind} at '{path}' ({detail})");
                }
            }
            ManifestEntry::Reserved { .. } => match dest_manifest.entries.get(path) {
                None | Some(ManifestEntry::Reserved { .. }) => {}
                Some(other) => {
                    bail!("destination published reserved VCS metadata at '{path}' as {other:?}")
                }
            },
            ManifestEntry::Directory { .. } => match dest_manifest.entries.get(path) {
                Some(ManifestEntry::Directory { .. }) => {}
                Some(other) => bail!("destination type mismatch at '{path}': {other:?}"),
                None => bail!("destination missing directory '{path}'"),
            },
            ManifestEntry::File { sha256, size, mode } => match policy.evaluate(path, *size) {
                FilePolicyDecision::Ignored { .. } | FilePolicyDecision::Oversize { .. } => {
                    if dest_manifest.entries.contains_key(path) {
                        bail!("destination published policy-excluded file '{path}'");
                    }
                }
                FilePolicyDecision::Allow | FilePolicyDecision::LfsTrack { .. } => {
                    match dest_manifest.entries.get(path) {
                        Some(ManifestEntry::File {
                            sha256: dest_hash,
                            mode: dest_mode,
                            size: dest_size,
                        }) => {
                            if dest_hash != sha256 || dest_size != size {
                                bail!("destination byte mismatch at '{path}'");
                            }
                            if file_exec_bit(*mode) != file_exec_bit(*dest_mode) {
                                bail!("destination executable-bit mismatch at '{path}'");
                            }
                        }
                        Some(other) => bail!("destination type mismatch at '{path}': {other:?}"),
                        None => bail!("destination missing file '{path}'"),
                    }
                }
            },
        }
    }

    for (path, entry) in &dest_manifest.entries {
        if matches!(entry, ManifestEntry::Reserved { .. }) {
            continue;
        }
        if !source_manifest.entries.contains_key(path) {
            bail!("destination has unpublished-from-source extra path '{path}'");
        }
    }
    Ok(())
}

fn file_exec_bit(mode: u32) -> bool {
    mode & 0o111 != 0
}

/// Remove files from `dst` (Git working tree) that no longer exist in `src`
/// (SVN export). Preserves reserved VCS metadata (e.g. destination `.git/`).
///
/// A root `.gitattributes` that the export omits is not kept by name. It is
/// replaced with the engine LFS patterns recorded by
/// [`crate::lfs::ensure_lfs_tracked`] under `.git/`, or deleted when that
/// marker is absent, so SVN-planted filter rules do not survive. Nested
/// `.gitattributes` files are ordinary and can be stale-removed. Ordinary
/// root dotfiles such as `.gitignore` can be stale-removed.
pub fn remove_stale_files(src: &Path, dst: &Path) -> Result<()> {
    remove_stale_inner(src, dst, true)
}

fn remove_stale_inner(src: &Path, dst: &Path, at_root: bool) -> Result<()> {
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
        if is_stale_remove_protected(&file_name) {
            continue;
        }

        let src_path = src.join(&file_name);
        let dst_path = entry.path();

        if at_root
            && file_name == ".gitattributes"
            && reconcile_root_gitattributes(dst, &src_path, &dst_path)?
        {
            continue;
        }

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

/// Handle a root `.gitattributes` entry.
///
/// Returns `true` when the entry was reconciled (export copy left in place,
/// replaced with engine LFS lines, or removed as non-engine). Returns `false`
/// when the path is a directory and ordinary stale-remove should run.
fn reconcile_root_gitattributes(dst_root: &Path, src_path: &Path, dst_path: &Path) -> Result<bool> {
    let dst_meta = std::fs::symlink_metadata(dst_path).with_context(|| {
        format!(
            "failed to stat .gitattributes without following: {}",
            dst_path.display()
        )
    })?;
    if dst_meta.is_dir() {
        return Ok(false);
    }

    let src_meta = match std::fs::symlink_metadata(src_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "failed to stat exported .gitattributes without following: {}",
                    src_path.display()
                )
            });
        }
        Ok(meta) => Some(meta),
    };

    // The current export ships this path. Merge export lines with engine LFS
    // patterns and drop any destination-only planted `filter=` rules.
    if let Some(src_meta) = src_meta {
        if src_meta.file_type().is_symlink() {
            bail!(
                "unsupported symlink at exported .gitattributes: refusing to follow outside the export root"
            );
        }
        if src_meta.file_type().is_file() {
            let dest_was_symlink = dst_meta.file_type().is_symlink();
            let dest_body = if dst_meta.file_type().is_file() {
                Some(std::fs::read_to_string(dst_path).with_context(|| {
                    format!(
                        "failed to read destination .gitattributes: {}",
                        dst_path.display()
                    )
                })?)
            } else {
                None
            };
            let export_body = read_regular_file_no_follow(src_path).with_context(|| {
                format!(
                    "failed to read exported .gitattributes: {}",
                    src_path.display()
                )
            })?;
            let strip_planted = crate::lfs::export_present_gitattributes_needs_strip(
                dest_was_symlink,
                dest_body.as_deref(),
                &export_body,
            );
            let engine_body = if strip_planted {
                crate::lfs::engine_gitattributes_body(dst_root).with_context(|| {
                    format!("failed to read engine LFS marker in {}", dst_root.display())
                })?
            } else {
                None
            };
            let merged = crate::lfs::merge_export_present_gitattributes(
                &export_body,
                dest_body.as_deref(),
                engine_body.as_deref(),
                strip_planted,
            );
            write_root_gitattributes_regular_file(dst_path, &merged)?;
            debug!(
                path = %dst_path.display(),
                "reconciled export-present .gitattributes"
            );
            return Ok(true);
        }
    }

    if let Some(body) = crate::lfs::engine_gitattributes_body(dst_root)
        .with_context(|| format!("failed to read engine LFS marker in {}", dst_root.display()))?
    {
        write_root_gitattributes_regular_file(dst_path, &body)?;
        debug!(
            path = %dst_path.display(),
            "rewrote .gitattributes to engine-recorded LFS patterns"
        );
        return Ok(true);
    }

    std::fs::remove_file(dst_path).with_context(|| {
        format!(
            "failed to remove non-engine .gitattributes: {}",
            dst_path.display()
        )
    })?;
    debug!(path = %dst_path.display(), "removed non-engine .gitattributes");
    Ok(true)
}

fn read_regular_file_no_follow(path: &Path) -> Result<String> {
    let mut file = open_no_follow_read(path)?;
    let mut body = String::new();
    file.read_to_string(&mut body)
        .with_context(|| format!("failed to read {} without following", path.display()))?;
    Ok(body)
}

fn write_root_gitattributes_regular_file(path: &Path, body: &str) -> Result<()> {
    crate::lfs::unlink_gitattributes_symlink(path).with_context(|| {
        format!(
            "failed to unlink planted .gitattributes symlink before write: {}",
            path.display()
        )
    })?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "failed to remove non-regular .gitattributes: {}",
                        path.display()
                    )
                });
            }
            Ok(()) => {}
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "failed to stat .gitattributes without following: {}",
                    path.display()
                )
            });
        }
    }
    write_regular_file_no_follow(path, body)
        .with_context(|| format!("failed to write engine .gitattributes: {}", path.display()))?;
    Ok(())
}

fn write_regular_file_no_follow(path: &Path, body: &str) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts
        .open(path)
        .with_context(|| format!("failed to open {} without following", path.display()))?;
    file.write_all(body.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
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
    /// Durable import operation id when `repo_id` is set (managed repos and setup wizard).
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
            if std::env::var_os("REPOSYNC_IMPORT_CANCEL_OBSERVE").is_some() {
                std::fs::write(dir.join(format!("{stage}.cancel_observed")), b"observed")
                    .expect("fixture cancel observation");
                while !dir.join(format!("{stage}.cancel_release")).exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            return true;
        }
        if dir.join(format!("{stage}.release")).exists() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(feature = "reliability-fixture")]
fn fixture_barrier_ready_only(stage: &str, repo: Option<&str>) -> bool {
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
    false
}

struct PublicationTarget<'a> {
    workdir: &'a Path,
    remote: &'a str,
    branch: &'a str,
    sha: &'a str,
    force: bool,
}

fn without_http_credentials(url: &str) -> String {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("https://", rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("http://", rest)
    } else {
        return url.into();
    };
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host_path = rest[..host_end]
        .rfind('@')
        .map_or(rest, |at| &rest[at + 1..]);
    format!("{scheme}{host_path}")
}

/// Read-only local proof for a held import. Reject missing/replaced managed
/// checkouts, changed origin targets, incomplete SHAs and absent commit trees.
pub fn verify_import_local_tip(
    workdir: &Path,
    reference: &str,
    sha: &str,
    expected_remote_url: &str,
) -> Result<String> {
    anyhow::ensure!(
        sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "recorded Git SHA is not full"
    );
    anyhow::ensure!(
        std::fs::symlink_metadata(workdir.join(".git"))
            .is_ok_and(|metadata| metadata.file_type().is_dir()),
        "managed Git checkout is missing"
    );
    let repo = git2::Repository::open(workdir).context("managed Git checkout cannot be opened")?;
    anyhow::ensure!(
        repo.workdir().and_then(|p| p.canonicalize().ok()) == workdir.canonicalize().ok(),
        "managed Git checkout path changed"
    );
    let origin = repo
        .find_remote("origin")
        .context("managed Git origin is missing")?;
    anyhow::ensure!(
        origin
            .url()
            .is_some_and(|url| without_http_credentials(url) == expected_remote_url),
        "managed Git origin differs from configured target"
    );
    let oid = git2::Oid::from_str(sha).context("recorded Git SHA is malformed")?;
    anyhow::ensure!(oid.to_string() == sha, "recorded Git SHA is not canonical");
    let branch = repo
        .find_reference(reference)
        .context("local import branch is missing")?;
    anyhow::ensure!(
        branch.target() == Some(oid),
        "local import branch differs from recorded tip"
    );
    let commit = repo
        .find_commit(oid)
        .context("recorded local Git commit is missing")?;
    let tree = commit
        .tree()
        .context("recorded local Git tree is missing")?;
    Ok(tree.id().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportInspect {
    UniqueMatch { git_sha: String, git_tree: String },
    Absent,
    Conflict { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone)]
pub struct ImportReconcileResult {
    pub operation: ImportOperation,
    pub inspect: ImportInspect,
    pub finalized: bool,
    pub resume_authorized: bool,
    pub publication_recorded: bool,
    pub observed_ref: Option<String>,
    pub observed_sha: Option<String>,
}

/// Bounded `git ls-remote --exit-code origin <exact-ref>` inspection for held
/// import reconciliation. Exit 2 means the ref is absent.
pub async fn fresh_import_remote_ref(
    workdir: &Path,
    reference: &str,
) -> Result<Option<String>, &'static str> {
    let mut inspect = crate::process::import_git_command()
        .map_err(|_| "remote inspection command unavailable")?;
    inspect
        .args(["ls-remote", "--exit-code", "origin", reference])
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = crate::process::run(inspect, Duration::from_secs(60), None)
        .await
        .map_err(|_| "remote inspection failed or timed out")?;
    match output.status.code() {
        Some(2) => Ok(None),
        Some(0) => {
            let stdout = String::from_utf8(output.stdout)
                .map_err(|_| "remote inspection returned malformed data")?;
            let mut lines = stdout.lines();
            let line = lines
                .next()
                .ok_or("remote inspection returned no exact ref")?;
            let (sha, found_ref) = line
                .split_once('\t')
                .ok_or("remote inspection returned malformed data")?;
            if lines.next().is_some()
                || found_ref != reference
                || sha.len() != 40
                || !sha.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("remote inspection did not return one exact full ref");
            }
            Ok(Some(sha.to_string()))
        }
        _ => Err("remote inspection unavailable (authentication or transport failure)"),
    }
}

fn import_reconcile_held_reasons() -> [&'static str; 10] {
    [
        "import target fingerprint changed",
        "repository checkpoint changed during held import",
        "missing import revision total",
        "remote SHA is not the recorded local import tip",
        "incomplete prior publication receipt",
        "remote SHA differs from publication evidence",
        "invalid publication counter",
        "snapshot import is missing its pin",
        "snapshot import is not a single verified baseline",
        "snapshot pin does not match the recorded local revision",
    ]
}

/// Observe-first reconciliation for held `import_operation_v1` journals.
///
/// Reuses the same inspect+finalize path as the admin reconcile endpoint.
/// Finalizes only on a unique verified remote match. A verified partial import
/// authorizes resume from the confirmed checkpoint but does not auto-resume.
pub async fn apply_import_reconciliation(
    db: &Database,
    repo: &Repository,
    op_id: &str,
    git_workdir: &Path,
) -> Result<ImportReconcileResult, DatabaseError> {
    let requested = db
        .get_import_operation(&repo.id, op_id)?
        .ok_or_else(|| DatabaseError::Other("import operation not found".into()))?;
    let active = db.active_import_operation(&repo.id)?;
    if active.is_none() && requested.state == ImportOperationState::Completed {
        let git_sha = requested.last_confirmed_git_sha.clone().unwrap_or_default();
        return Ok(ImportReconcileResult {
            operation: requested,
            inspect: ImportInspect::UniqueMatch {
                git_sha,
                git_tree: String::new(),
            },
            finalized: true,
            resume_authorized: false,
            publication_recorded: false,
            observed_ref: None,
            observed_sha: None,
        });
    }
    if active.as_ref().is_none_or(|op| op.id != op_id)
        || requested.state != ImportOperationState::ReconciliationRequired
        || !matches!(
            requested.operation_type.as_str(),
            "full_import" | "snapshot_import"
        )
    {
        return Err(DatabaseError::Other(
            "operation is not this repository's active reconciliation hold".into(),
        ));
    }
    if requested.target_fingerprint.is_empty()
        || requested.target_fingerprint != import_target_fingerprint(repo, git_workdir)
    {
        let operation = db.note_import_reconciliation_reason(
            &repo.id,
            op_id,
            "Import target configuration changed; review required",
        )?;
        return Ok(ImportReconcileResult {
            operation,
            inspect: ImportInspect::Conflict {
                reason: "Import target configuration changed; review required".into(),
            },
            finalized: false,
            resume_authorized: false,
            publication_recorded: false,
            observed_ref: None,
            observed_sha: None,
        });
    }
    let reference = format!("refs/heads/{}", repo.git_branch);
    let expected_sha = match (&requested.intended_ref, &requested.intended_git_sha) {
        (Some(intent_ref), Some(sha))
            if intent_ref == &reference && requested.last_local_git_sha.as_deref() == Some(sha) =>
        {
            sha.as_str()
        }
        (None, None)
            if requested.last_confirmed_svn_rev == requested.last_local_svn_rev
                && requested.last_confirmed_git_sha == requested.last_local_git_sha =>
        {
            requested.last_confirmed_git_sha.as_deref().unwrap_or("")
        }
        _ => {
            let operation = db.note_import_reconciliation_reason(
                &repo.id,
                op_id,
                "Publication evidence is incomplete or inconsistent",
            )?;
            return Ok(ImportReconcileResult {
                operation,
                inspect: ImportInspect::Conflict {
                    reason: "Publication evidence is incomplete or inconsistent".into(),
                },
                finalized: false,
                resume_authorized: false,
                publication_recorded: false,
                observed_ref: None,
                observed_sha: None,
            });
        }
    };
    let configured_url = remote_url::derive_git_remote_url(&repo.git_api_url, None, &repo.git_repo);
    let local_tree = match verify_import_local_tip(
        git_workdir,
        &reference,
        expected_sha,
        &configured_url,
    ) {
        Ok(tree) => tree,
        Err(_) => {
            let operation = db.note_import_reconciliation_reason(
                &repo.id,
                op_id,
                "Recorded local Git object, tree, branch or origin is unavailable or changed",
            )?;
            return Ok(ImportReconcileResult {
                operation,
                inspect: ImportInspect::Conflict {
                    reason: "Recorded local Git object, tree, branch or origin is unavailable or changed".into(),
                },
                finalized: false,
                resume_authorized: false,
                publication_recorded: false,
                observed_ref: None,
                observed_sha: None,
            });
        }
    };
    let observed = match fresh_import_remote_ref(git_workdir, &reference).await {
        Ok(Some(sha)) => sha,
        Ok(None) => {
            let operation = db.note_import_reconciliation_reason(
                &repo.id,
                op_id,
                "Configured remote ref is missing; no publication was inferred",
            )?;
            return Ok(ImportReconcileResult {
                operation,
                inspect: ImportInspect::Absent,
                finalized: false,
                resume_authorized: false,
                publication_recorded: false,
                observed_ref: Some(reference),
                observed_sha: None,
            });
        }
        Err(reason) => {
            let operation = db.note_import_reconciliation_reason(&repo.id, op_id, reason)?;
            return Ok(ImportReconcileResult {
                operation,
                inspect: ImportInspect::Unavailable {
                    reason: reason.into(),
                },
                finalized: false,
                resume_authorized: false,
                publication_recorded: false,
                observed_ref: None,
                observed_sha: None,
            });
        }
    };
    if observed != expected_sha {
        let operation = db.note_import_reconciliation_reason(
            &repo.id,
            op_id,
            "Remote ref differs from the recorded full Git SHA",
        )?;
        return Ok(ImportReconcileResult {
            operation,
            inspect: ImportInspect::Conflict {
                reason: "Remote ref differs from the recorded full Git SHA".into(),
            },
            finalized: false,
            resume_authorized: false,
            publication_recorded: false,
            observed_ref: Some(reference),
            observed_sha: Some(observed),
        });
    }
    if verify_import_local_tip(git_workdir, &reference, expected_sha, &configured_url)
        .ok()
        .as_deref()
        != Some(local_tree.as_str())
    {
        let operation = db.note_import_reconciliation_reason(
            &repo.id,
            op_id,
            "Managed local Git evidence changed during remote inspection",
        )?;
        return Ok(ImportReconcileResult {
            operation,
            inspect: ImportInspect::Conflict {
                reason: "Managed local Git evidence changed during remote inspection".into(),
            },
            finalized: false,
            resume_authorized: false,
            publication_recorded: false,
            observed_ref: Some(reference),
            observed_sha: Some(observed),
        });
    }
    match db.reconcile_verified_import(&repo.id, op_id, git_workdir, &reference, &observed) {
        Ok(reconciled) => Ok(ImportReconcileResult {
            operation: reconciled.operation,
            inspect: ImportInspect::UniqueMatch {
                git_sha: observed.clone(),
                git_tree: local_tree,
            },
            finalized: reconciled.completed,
            resume_authorized: reconciled.resume_authorized,
            publication_recorded: reconciled.publication_recorded,
            observed_ref: Some(reference),
            observed_sha: Some(observed),
        }),
        Err(DatabaseError::Other(reason))
            if import_reconcile_held_reasons().contains(&reason.as_str()) =>
        {
            let operation = db.note_import_reconciliation_reason(&repo.id, op_id, &reason)?;
            Ok(ImportReconcileResult {
                operation,
                inspect: ImportInspect::Conflict { reason },
                finalized: false,
                resume_authorized: false,
                publication_recorded: false,
                observed_ref: Some(reference),
                observed_sha: Some(observed),
            })
        }
        Err(error) => Err(error),
    }
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

enum ImportLfsPrep {
    NotRequired,
    Ready,
    PreflightFailed(String),
    InstallFailed(String),
    Cancelled,
}

/// Shared LFS preflight and `git lfs install --local`.
///
/// Success and failure are reported to the caller. Full import warns and may
/// continue without pointers. Snapshot import must fail closed on
/// [`ImportLfsPrep::PreflightFailed`] / [`ImportLfsPrep::InstallFailed`]
/// before it creates a commit.
async fn prepare_import_lfs(
    file_policy: &FilePolicy,
    repo_path: &Path,
    durable: bool,
    progress: &Arc<RwLock<ImportProgress>>,
    ws_broadcast: &Option<broadcast::Sender<String>>,
    cancel_signal: Option<&Arc<AtomicBool>>,
) -> Result<ImportLfsPrep> {
    if !file_policy.lfs_enabled() {
        return Ok(ImportLfsPrep::NotRequired);
    }
    let preflight = if durable {
        match import_lfs_command(&["lfs", "version"], repo_path, cancel_signal).await {
            Ok(output) if output.status.success() => {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
            Ok(output) => Err(format!(
                "git lfs version failed (exit {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(e) => {
                if crate::process::confirmed_cancelled(&e)
                    && stop_requested(progress, cancel_signal).await
                {
                    return Ok(ImportLfsPrep::Cancelled);
                }
                return Err(e).context("LFS preflight did not quiesce safely");
            }
        }
    } else {
        crate::lfs::preflight_check()
    };
    let version = match preflight {
        Ok(version) => version,
        Err(reason) => return Ok(ImportLfsPrep::PreflightFailed(reason)),
    };
    push_log_line(
        progress,
        ws_broadcast,
        format!("[info] Git LFS available: {version}"),
    )
    .await;
    let install = if durable {
        match import_lfs_command(&["lfs", "install", "--local"], repo_path, cancel_signal).await {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(format!(
                "git lfs install failed (exit {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(e) => {
                return Err(e).context("LFS hook installation outcome requires inspection");
            }
        }
    } else {
        crate::lfs::install_lfs_hooks(repo_path)
    };
    match install {
        Ok(()) => {
            push_log_line(
                progress,
                ws_broadcast,
                "[info] Git LFS installed in repo (filters active)".into(),
            )
            .await;
            Ok(ImportLfsPrep::Ready)
        }
        Err(reason) => Ok(ImportLfsPrep::InstallFailed(reason)),
    }
}

/// Materialize one pinned SVN snapshot, verify projected bytes, publish, and
/// record the verified baseline. The pin must already be stored; this never
/// re-resolves live HEAD.
pub async fn run_snapshot_import(
    svn_client: &SvnClient,
    git_client: &Arc<std::sync::Mutex<GitClient>>,
    db: &Database,
    file_policy: &FilePolicy,
    import_config: &ImportConfig,
    pin: &crate::db::import_operations::SnapshotPin,
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

    {
        let mut p = progress.write().await;
        p.phase = ImportPhase::Importing;
        p.current_rev = pin.operative_rev;
        p.total_revs = 1;
        p.push_log(format!(
            "[info] snapshot pin uuid={} url={} r{} ({})",
            pin.svn_uuid,
            pin.canonical_url,
            pin.operative_rev,
            pin.history_boundary()
        ));
    }

    let repo_path = {
        let git = git_client.lock().unwrap_or_else(|p| p.into_inner());
        git.repo_path().to_path_buf()
    };

    if crate::snapshot::snapshot_workdir_is_born(&repo_path)? {
        anyhow::bail!(
            "existing non-empty Git workdir refuses snapshot overwrite; choose a new target"
        );
    }

    match prepare_import_lfs(
        file_policy,
        &repo_path,
        operation_id.is_some(),
        &progress,
        &ws_broadcast,
        cancel_signal.as_ref(),
    )
    .await?
    {
        ImportLfsPrep::Cancelled => return Ok(ImportOutcome::Cancelled { commits: 0 }),
        ImportLfsPrep::NotRequired | ImportLfsPrep::Ready => {}
        ImportLfsPrep::PreflightFailed(reason) | ImportLfsPrep::InstallFailed(reason) => {
            push_log_line(
                &progress,
                &ws_broadcast,
                format!("[error] Git LFS required for snapshot import but unavailable: {reason}"),
            )
            .await;
            anyhow::bail!(
                "Git LFS is required for this snapshot import but is unavailable: {reason}"
            );
        }
    }

    // Observability only. No test coordinates this stage; waiting here would
    // swallow the ordinary full-import `after_first_local` barrier whenever
    // `REPOSYNC_IMPORT_BARRIER_DIR` is set on a snapshot worker.
    #[cfg(feature = "reliability-fixture")]
    {
        let _ = fixture_barrier_ready_only("before_snapshot_export", repo_id.as_deref());
    }
    if stop_requested(&progress, cancel_signal.as_ref()).await {
        return Ok(ImportOutcome::Cancelled { commits: 0 });
    }

    let stats =
        crate::snapshot::materialize_snapshot(svn_client, &repo_path, file_policy, db, pin).await?;
    {
        let mut p = progress.write().await;
        p.current_file_count = stats.copied as u64;
        p.files_skipped = stats.skipped as u64;
        p.push_log(format!(
            "[info] materialized pinned r{} (copied={} skipped={})",
            pin.operative_rev, stats.copied, stats.skipped
        ));
    }

    let workdir_verify =
        crate::snapshot::verify_projected_snapshot(svn_client, &repo_path, file_policy, db, pin)
            .await?;
    progress.write().await.verification = Some(workdir_verify.clone());

    if stop_requested(&progress, cancel_signal.as_ref()).await {
        return Ok(ImportOutcome::Cancelled { commits: 0 });
    }

    progress.write().await.phase = ImportPhase::Verifying;
    let message = crate::snapshot::snapshot_commit_message(pin);
    let sha = import_cli_commit(
        &repo_path,
        &message,
        &import_config.committer_name,
        &import_config.committer_email,
        &import_config.committer_name,
        &import_config.committer_email,
        cancel_signal.as_ref(),
    )
    .await
    .context("failed to create snapshot baseline Git commit")?
    .to_string();

    let projected =
        crate::snapshot::project_snapshot_tree(svn_client, file_policy, db, pin).await?;
    let commit_verify = crate::snapshot::verify_commit_matches_projection(
        projected.path(),
        &repo_path,
        &sha,
        file_policy,
    )?;
    {
        let mut p = progress.write().await;
        p.verification = Some(commit_verify);
        p.commits_created = 1;
        p.current_rev = pin.operative_rev;
        p.push_log(format!(
            "[ok] snapshot baseline {} at r{} (projected content verified)",
            &sha[..8.min(sha.len())],
            pin.operative_rev
        ));
    }

    db.insert_commit_map_with_repo(
        pin.operative_rev,
        &sha,
        "svn_to_git",
        "snapshot",
        &format!(
            "{} <{}>",
            import_config.committer_name, import_config.committer_email
        ),
        repo_id.as_deref(),
    )
    .context("failed to persist snapshot baseline mapping")?;

    if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
        db.note_import_local(repo, op, pin.operative_rev, &sha, 1, 1)
            .context("failed to persist local snapshot baseline")?;
        if stop_requested(&progress, cancel_signal.as_ref()).await {
            return Ok(ImportOutcome::Cancelled { commits: 1 });
        }
        progress.write().await.phase = ImportPhase::FinalPush;
        if let Err(reason) = publish_checked(
            db,
            repo,
            op,
            PublicationTarget {
                workdir: &repo_path,
                remote: &import_config.remote_name,
                branch: &import_config.branch,
                sha: &sha,
                // First-ref creation only. The empty lease
                // (`refs/heads/{branch}:`) creates the branch when that
                // remote ref is absent. The empty-target gate already
                // refused an existing branch before the worker started.
                force: true,
            },
            cancel_signal.as_ref(),
        )
        .await
        {
            return Ok(ImportOutcome::ReconciliationRequired { commits: 1, reason });
        }
        progress.write().await.batches_pushed = 1;
    }

    db.insert_audit_log(
        "import_snapshot",
        Some("svn_to_git"),
        Some(pin.operative_rev),
        Some(&sha),
        None,
        Some(&format!(
            "Snapshot import from SVN r{} ({})",
            pin.operative_rev,
            pin.history_boundary()
        )),
        true,
    )
    .ok();

    // The worker broadcasts phase completion only after
    // `complete_import_operation` succeeds. Emitting completed here would
    // claim success if finalization then fails.

    Ok(ImportOutcome::Completed {
        commits: 1,
        svn_rev: pin.operative_rev,
        git_sha: sha,
    })
}

/// Legacy personal-import tail: global watermarks when no durable operation journal exists.
fn persist_legacy_personal_import_watermarks(
    db: &Database,
    last_svn_rev: i64,
    git_sha: &str,
) -> Result<(), crate::errors::DatabaseError> {
    db.set_watermark("svn_rev", &last_svn_rev.to_string())?;
    db.set_legacy_import_git_sha_watermark(git_sha)?;
    Ok(())
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

    // Get SVN info / fixture handshake before LFS so a slow or hung git-lfs
    // preflight cannot prevent `after_first_local.ready` from being reachable
    // on the ordinary full-import browser path.
    #[cfg(feature = "reliability-fixture")]
    if fixture_barrier("connecting", repo_id.as_deref(), cancel_signal.as_ref()).await {
        return Ok(ImportOutcome::Cancelled { commits: 0 });
    }

    // LFS preflight: check availability and install hooks in the repo.
    // Full import still warns and may commit without pointers. Snapshot
    // import uses the same helper and fails closed instead.
    let lfs_available = if file_policy.lfs_enabled() {
        let lfs_repo = {
            let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
            git_guard.repo_workdir()
        };
        match prepare_import_lfs(
            file_policy,
            &lfs_repo,
            operation_id.is_some(),
            &progress,
            &ws_broadcast,
            cancel_signal.as_ref(),
        )
        .await?
        {
            ImportLfsPrep::Cancelled => return Ok(ImportOutcome::Cancelled { commits: 0 }),
            ImportLfsPrep::NotRequired => false,
            ImportLfsPrep::Ready => true,
            ImportLfsPrep::PreflightFailed(e) => {
                log(
                    &progress,
                    &ws_broadcast,
                    format!(
                        "[warn] Git LFS not available: {e} — large files will be committed directly"
                    ),
                )
                .await;
                false
            }
            ImportLfsPrep::InstallFailed(e) => {
                log(
                    &progress,
                    &ws_broadcast,
                    format!("[warn] git lfs install failed: {e} — LFS tracking will not work"),
                )
                .await;
                false
            }
        }
    } else {
        false
    };

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
        Err(crate::errors::SvnError::IoError(ref e))
            if crate::process::confirmed_cancelled(e)
                && stop_requested(&progress, cancel_signal.as_ref()).await =>
        {
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
        Err(crate::errors::SvnError::IoError(ref e))
            if crate::process::confirmed_cancelled(e)
                && stop_requested(&progress, cancel_signal.as_ref()).await =>
        {
            return Ok(ImportOutcome::Cancelled { commits: 0 });
        }
        Err(e) => return Err(e).context("failed to get SVN log"),
    };

    let resume_checkpoint = if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
        db.get_import_operation(repo, op)
            .ok()
            .flatten()
            .and_then(|operation| {
                crate::db::import_operations::import_resume_checkpoint(&operation)
            })
    } else {
        None
    };

    if let (Some(repo), Some(op)) = (&repo_id, &operation_id) {
        // Always persist the live SVN log length. On resume the journal total must
        // grow with the repository so processed_revisions cannot outrun total_revisions.
        db.note_import_total(repo, op, log_entries.len() as u64)?;
    }

    let total_revisions = log_entries.len();
    {
        let mut p = progress.write().await;
        p.total_revs = total_revisions as i64;
    }

    let pending_entries: Vec<_> = if let Some((from_svn_rev, _, _)) = resume_checkpoint {
        log_entries
            .iter()
            .filter(|entry| entry.revision > from_svn_rev)
            .collect()
    } else {
        log_entries.iter().collect()
    };

    if let Some((from_svn_rev, local_commits, confirmed_batches)) = resume_checkpoint {
        log(
            &progress,
            &ws_broadcast,
            format!(
                "[info] Resuming import after confirmed SVN r{} ({}/{} revisions already durable)",
                from_svn_rev, local_commits, total_revisions
            ),
        )
        .await;
        {
            let mut p = progress.write().await;
            p.commits_created = local_commits;
            p.batches_pushed = confirmed_batches;
            p.current_rev = local_commits as i64;
        }
    } else {
        log(
            &progress,
            &ws_broadcast,
            format!("[info] Found {} revisions to import", total_revisions),
        )
        .await;
    }

    let repo_path = {
        let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
        git_guard.repo_path().to_path_buf()
    };

    if let Some((_, local_commits, _)) = resume_checkpoint {
        let git_guard = git_client.lock().unwrap_or_else(|p| p.into_inner());
        let head = git_guard
            .get_head_sha()
            .context("missing local import tip during resume")?;
        let expected = db
            .get_import_operation(
                repo_id.as_deref().unwrap(),
                operation_id.as_deref().unwrap(),
            )?
            .and_then(|op| op.last_local_git_sha)
            .ok_or_else(|| anyhow::anyhow!("missing recorded local Git tip during resume"))?;
        if head != expected {
            return Err(anyhow::anyhow!(
                "local Git tip {head} does not match the durable checkpoint {expected}"
            ));
        }
        if local_commits == 0 {
            return Err(anyhow::anyhow!(
                "resume checkpoint recorded zero local commits"
            ));
        }
    }

    const PUSH_BATCH_SIZE: u64 = 50;
    let mut count = resume_checkpoint.map(|(_, local, _)| local).unwrap_or(0);
    let mut commits_since_push = resume_checkpoint
        .map(|(_, local, batches)| local.saturating_sub(batches * PUSH_BATCH_SIZE))
        .unwrap_or(0);

    for (idx, entry) in pending_entries.iter().enumerate() {
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
            p.current_rev = (count + idx as u64 + 1) as i64;
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
                                    if crate::process::confirmed_cancelled(io))
                                {
                                    return Ok(ImportOutcome::Cancelled { commits: count });
                                }
                                return Err(e)
                                    .context("Git apply stopped without confirmed quiescence");
                            }
                            if matches!(e, crate::errors::GitError::ApplyFailed(_)) {
                                debug!(rev, error = %e, "finished incremental apply failed, using full export");
                            } else {
                                return Err(e).context(
                                    "Git apply did not finish with a known failed-patch result",
                                );
                            }
                        }
                    }
                }
                Ok(_) => {
                    debug!(rev, "no diff available, using full export");
                }
                Err(e)
                    if matches!(e, crate::errors::SvnError::CommandFailed { .. })
                        && !stop_requested(&progress, cancel_signal.as_ref()).await =>
                {
                    debug!(rev, error = %e, "finished SVN diff failed, using full export");
                }
                Err(e) => {
                    if matches!(&e, crate::errors::SvnError::IoError(io)
                        if crate::process::confirmed_cancelled(io))
                        && stop_requested(&progress, cancel_signal.as_ref()).await
                    {
                        return Ok(ImportOutcome::Cancelled { commits: count });
                    }
                    return Err(e)
                        .context("SVN diff stopped without confirmed fallback eligibility");
                }
            }
        }

        if !used_incremental {
            if stop_requested(&progress, cancel_signal.as_ref()).await {
                return Ok(ImportOutcome::Cancelled { commits: count });
            }
            if let Err(e) = svn_client.export("", rev, export_dir.path()).await {
                if matches!(&e, crate::errors::SvnError::IoError(io)
                    if crate::process::confirmed_cancelled(io))
                    && stop_requested(&progress, cancel_signal.as_ref()).await
                {
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
                    db.note_import_local(repo, op, rev, &sha, count + 1, count + 1)
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
                let map_result = db.insert_commit_map_with_repo(
                    rev,
                    &sha,
                    "svn_to_git",
                    &entry.author,
                    &format!("{} <{}>", author_name, author_email),
                    repo_id.as_deref(),
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
        persist_legacy_personal_import_watermarks(db, last_rev, &sha)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_policy::FilePolicy;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::time::{Duration, Instant};

    const CANARY: &[u8] = b"OUTSIDE-ROOT-CANARY-SECRET-RS-C01";

    fn test_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    fn noop_policy() -> FilePolicy {
        FilePolicy::new(0, vec![])
    }

    fn dest_contains_canary(dest: &Path) -> bool {
        fn walk(dir: &Path) -> bool {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return false;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_dir() {
                    if walk(&path) {
                        return true;
                    }
                } else if meta.file_type().is_file() {
                    if let Ok(bytes) = std::fs::read(&path) {
                        if bytes.windows(CANARY.len()).any(|w| w == CANARY) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        walk(dest)
    }

    /// Historical follow-y copier used only to prove self-consistent verify
    /// cannot detect an outside-root double-copy.
    fn naive_follow_copy(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name == ".git" || name == ".svn" {
                continue;
            }
            let src_path = entry.path();
            let dst_path = dst.join(&name);
            if src_path.is_dir() {
                std::fs::create_dir_all(&dst_path).unwrap();
                naive_follow_copy(&src_path, &dst_path);
            } else {
                std::fs::copy(&src_path, &dst_path).unwrap();
            }
        }
    }

    fn hash_tree_follow(root: &Path) -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        fn walk(dir: &Path, root: &Path, files: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, files);
                } else {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    let bytes = std::fs::read(&path).unwrap();
                    files.insert(rel, hex::encode(Sha256::digest(&bytes)));
                }
            }
        }
        walk(root, root, &mut files);
        files
    }

    #[test]
    fn import_copy_preserves_ordinary_dotfiles_and_records_reserved() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(src.path().join(".editorconfig"), "root = true\n").unwrap();
        std::fs::create_dir_all(src.path().join(".github/workflows")).unwrap();
        std::fs::write(src.path().join(".github/workflows/ci.yml"), "on: push\n").unwrap();
        std::fs::create_dir(src.path().join(".svn")).unwrap();
        std::fs::write(src.path().join(".svn/entries"), "skip-me").unwrap();
        std::fs::create_dir(src.path().join(".git")).unwrap();
        std::fs::write(src.path().join(".git/HEAD"), "should-not-copy").unwrap();
        std::fs::create_dir(src.path().join("nested")).unwrap();
        std::fs::write(src.path().join("nested/.hidden"), "keep").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        let db = test_db();
        let stats = copy_tree_with_policy(src.path(), dst.path(), &noop_policy(), &db).unwrap();
        assert_eq!(stats.copied, 4);
        assert_eq!(stats.reserved_excluded, 2);
        assert!(stats
            .exclusions
            .iter()
            .any(|e| e == "reserved:.svn" || e == "reserved:.git"));

        let audits = db.list_audit_log(10, 0).unwrap();
        assert!(
            audits
                .iter()
                .any(|e| e.action == "reserved_metadata_exclude"),
            "reserved exclusion must be audited: {:?}",
            audits.iter().map(|e| &e.action).collect::<Vec<_>>()
        );

        assert_eq!(
            std::fs::read_to_string(dst.path().join(".gitignore")).unwrap(),
            "*.tmp\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".editorconfig")).unwrap(),
            "root = true\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".github/workflows/ci.yml")).unwrap(),
            "on: push\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("nested/.hidden")).unwrap(),
            "keep"
        );
        assert!(!dst.path().join(".svn").exists());
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main"
        );

        let manifest = independent_tree_manifest(src.path()).unwrap();
        verify_against_independent_manifest(dst.path(), &manifest, &noop_policy()).unwrap();
    }

    #[test]
    fn import_copy_preserves_executable_bit_and_ordinary_binary() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let script = src.path().join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let binary = [0u8, 1, 255, 0, 10, 13, 0x89, b'P', b'N', b'G'];
        std::fs::write(src.path().join("blob.bin"), binary).unwrap();

        let db = test_db();
        let stats = copy_tree_with_policy(src.path(), dst.path(), &noop_policy(), &db).unwrap();
        assert_eq!(stats.copied, 2);

        let dest_mode = std::fs::metadata(dst.path().join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(dest_mode & 0o111, 0, "executable bit must be preserved");
        assert_eq!(std::fs::read(dst.path().join("blob.bin")).unwrap(), binary);

        let manifest = independent_tree_manifest(src.path()).unwrap();
        match manifest.entries.get("run.sh") {
            Some(ManifestEntry::File { mode, .. }) => assert_ne!(mode & 0o111, 0),
            other => panic!("expected file manifest for run.sh, got {other:?}"),
        }
        verify_against_independent_manifest(dst.path(), &manifest, &noop_policy()).unwrap();
    }

    #[test]
    fn import_copy_rejects_file_symlink_before_publish() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("target.txt"), "inside").unwrap();
        symlink(src.join("target.txt"), src.join("link.txt")).unwrap();

        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(
            err.to_string().contains("unsupported symlink"),
            "unexpected error: {err}"
        );
        assert!(!dst.join("link.txt").exists());

        let manifest = independent_tree_manifest(&src).unwrap();
        assert!(matches!(
            manifest.entries.get("link.txt"),
            Some(ManifestEntry::Unsupported { kind, .. }) if kind == "symlink"
        ));
    }

    #[test]
    fn import_copy_rejects_dir_symlink_before_publish() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("realdir")).unwrap();
        std::fs::write(src.join("realdir/a.txt"), "a").unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        symlink(src.join("realdir"), src.join("alias")).unwrap();

        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(err.to_string().contains("unsupported symlink"), "{err}");
        assert!(!dst.join("alias").exists());
    }

    #[test]
    fn import_copy_rejects_dangling_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        symlink(src.join("missing.txt"), src.join("dangling")).unwrap();

        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(err.to_string().contains("unsupported symlink"), "{err}");
        assert!(!dst.join("dangling").exists());
        let manifest = independent_tree_manifest(&src).unwrap();
        assert!(matches!(
            manifest.entries.get("dangling"),
            Some(ManifestEntry::Unsupported { kind, .. }) if kind == "symlink"
        ));
    }

    #[test]
    fn import_copy_rejects_symlink_loop_without_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        symlink(src.join("loop_b"), src.join("loop_a")).unwrap();
        symlink(src.join("loop_a"), src.join("loop_b")).unwrap();

        let started = Instant::now();
        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "symlink loop hung the copier"
        );
        assert!(err.to_string().contains("unsupported symlink"), "{err}");
    }

    #[test]
    fn import_copy_rejects_fifo_special_without_opening() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("ok.txt"), "ok").unwrap();
        let fifo = src.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed");

        let started = Instant::now();
        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "FIFO open blocked — special file was read"
        );
        assert!(
            err.to_string().contains("unsupported special") || err.to_string().contains("fifo"),
            "{err}"
        );
        assert!(!dst.join("pipe").exists());
    }

    #[test]
    fn import_copy_outside_root_canary_unread_and_unpublished() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let src = tmp.path().join("export");
        let dst = tmp.path().join("dest");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(outside.join("secret.bin"), CANARY).unwrap();
        std::fs::write(src.join("ok.txt"), "inside").unwrap();
        symlink(outside.join("secret.bin"), src.join("escape")).unwrap();

        let manifest = independent_tree_manifest(&src).unwrap();
        assert!(
            !manifest.contains_file_digest_of(CANARY),
            "independent manifest ingested outside-root canary bytes"
        );
        assert!(matches!(
            manifest.entries.get("escape"),
            Some(ManifestEntry::Unsupported { kind, .. }) if kind == "symlink"
        ));

        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(err.to_string().contains("unsupported symlink"), "{err}");
        assert!(!dst.join("escape").exists());
        assert!(!dest_contains_canary(&dst));
        assert_eq!(std::fs::read(outside.join("secret.bin")).unwrap(), CANARY);
    }

    #[test]
    fn import_copy_outside_root_double_copy_canary_fails_independent_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let src = tmp.path().join("export");
        let naive = tmp.path().join("naive");
        let naive_expected = tmp.path().join("naive_expected");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&naive).unwrap();
        std::fs::create_dir_all(&naive_expected).unwrap();
        std::fs::write(outside.join("secret.bin"), CANARY).unwrap();
        std::fs::write(src.join("ok.txt"), "inside").unwrap();
        symlink(outside.join("secret.bin"), src.join("escape")).unwrap();

        naive_follow_copy(&src, &naive);
        naive_follow_copy(&src, &naive_expected);
        assert_eq!(
            hash_tree_follow(&naive),
            hash_tree_follow(&naive_expected),
            "self-consistent follow-copy verify must match (the bug #88 catches)"
        );
        assert_eq!(std::fs::read(naive.join("escape")).unwrap(), CANARY);

        let manifest = independent_tree_manifest(&src).unwrap();
        let err =
            verify_against_independent_manifest(&naive, &manifest, &noop_policy()).unwrap_err();
        assert!(
            err.to_string().contains("unsupported") || err.to_string().contains("escape"),
            "independent manifest must fail the double-copy canary: {err}"
        );
        assert!(!manifest.contains_file_digest_of(CANARY));
    }

    #[test]
    fn import_copy_rejects_hardlink_nlink_gt_1_before_publish() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let original = src.join("original.txt");
        std::fs::write(&original, b"in-tree-bytes").unwrap();
        std::fs::hard_link(&original, src.join("alias.txt")).unwrap();
        assert!(
            std::fs::symlink_metadata(&original).unwrap().nlink() > 1,
            "fixture must be a multi-linked regular file"
        );

        let db = test_db();
        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &db).unwrap_err();
        assert!(
            err.to_string().contains("unsupported hardlink") && err.to_string().contains("nlink>1"),
            "{err}"
        );
        assert!(!dst.join("alias.txt").exists());
        assert!(
            !dst.join("original.txt").exists(),
            "every nlink>1 name is rejected before publish"
        );

        let audits = db.list_audit_log(10, 0).unwrap();
        assert!(
            audits.iter().any(|e| {
                e.action == "unsupported_hardlink_reject"
                    && !e.success
                    && e.details.as_deref().unwrap_or("").contains("nlink>1")
            }),
            "hardlink rejection must be recorded: {:?}",
            audits
                .iter()
                .map(|e| (&e.action, e.success, &e.details))
                .collect::<Vec<_>>()
        );

        let manifest = independent_tree_manifest(&src).unwrap();
        for name in ["original.txt", "alias.txt"] {
            assert!(
                matches!(
                    manifest.entries.get(name),
                    Some(ManifestEntry::Unsupported { kind, detail })
                        if kind == "hardlink" && detail.contains("not opened")
                ),
                "{name}: {:?}",
                manifest.entries.get(name)
            );
        }
        assert!(
            !manifest.contains_file_digest_of(b"in-tree-bytes"),
            "independent manifest must not read a multi-linked file"
        );
    }

    #[test]
    fn import_copy_outside_root_hardlink_canary_unread_and_unpublished() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let src = tmp.path().join("export");
        let dst = tmp.path().join("dest");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let secret = outside.join("secret.bin");
        std::fs::write(&secret, CANARY).unwrap();
        std::fs::write(src.join("ok.txt"), "inside").unwrap();
        std::fs::hard_link(&secret, src.join("escape")).unwrap();
        assert!(
            std::fs::symlink_metadata(src.join("escape"))
                .unwrap()
                .nlink()
                > 1
        );

        let manifest = independent_tree_manifest(&src).unwrap();
        assert!(
            !manifest.contains_file_digest_of(CANARY),
            "independent manifest ingested outside-root hardlink canary bytes"
        );
        assert!(matches!(
            manifest.entries.get("escape"),
            Some(ManifestEntry::Unsupported { kind, detail })
                if kind == "hardlink" && detail.contains("nlink>1") && detail.contains("not opened")
        ));
        assert!(matches!(
            manifest.entries.get("ok.txt"),
            Some(ManifestEntry::File { .. })
        ));

        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(err.to_string().contains("unsupported hardlink"), "{err}");
        assert!(!dst.join("escape").exists());
        assert!(!dest_contains_canary(&dst));
        assert_eq!(std::fs::read(&secret).unwrap(), CANARY);
    }

    #[test]
    fn import_copy_outside_root_hardlink_canary_fails_independent_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let src = tmp.path().join("export");
        let published = tmp.path().join("published");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&published).unwrap();
        let secret = outside.join("secret.bin");
        std::fs::write(&secret, CANARY).unwrap();
        std::fs::write(src.join("ok.txt"), "inside").unwrap();
        std::fs::hard_link(&secret, src.join("escape")).unwrap();

        // Publisher reads the alias and writes a fresh regular file. Expectations
        // come from independent_tree_manifest, not copy_tree_with_policy.
        std::fs::copy(src.join("escape"), published.join("escape")).unwrap();
        std::fs::copy(src.join("ok.txt"), published.join("ok.txt")).unwrap();
        assert_eq!(std::fs::read(published.join("escape")).unwrap(), CANARY);
        assert_eq!(
            std::fs::symlink_metadata(published.join("escape"))
                .unwrap()
                .nlink(),
            1,
            "published canary must be a normal file so only the source manifest can catch it"
        );

        let manifest = independent_tree_manifest(&src).unwrap();
        assert!(!manifest.contains_file_digest_of(CANARY));
        let err =
            verify_against_independent_manifest(&published, &manifest, &noop_policy()).unwrap_err();
        assert!(
            err.to_string().contains("unsupported") && err.to_string().contains("escape"),
            "independent manifest must fail the hardlink canary: {err}"
        );
    }

    #[test]
    fn import_copy_symlink_to_outside_fifo_does_not_block() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let fifo = outside.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
        symlink(&fifo, src.join("escape")).unwrap();
        std::fs::write(src.join("ok.txt"), "ok").unwrap();

        let started = Instant::now();
        let err = copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "followed outside FIFO (blocked)"
        );
        assert!(err.to_string().contains("unsupported symlink"), "{err}");
        assert!(!dst.join("escape").exists());
    }

    #[test]
    fn import_copy_records_policy_exclusions() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(src.path().join("noise.log"), "drop").unwrap();
        let policy = FilePolicy::new(0, vec!["*.log".into()]);
        let db = test_db();
        let stats = copy_tree_with_policy(src.path(), dst.path(), &policy, &db).unwrap();
        assert_eq!(stats.copied, 1);
        assert_eq!(stats.skipped, 1);
        assert!(stats
            .exclusions
            .iter()
            .any(|e| e == "policy:ignored:noise.log"));
        assert!(dst.path().join("keep.txt").exists());
        assert!(!dst.path().join("noise.log").exists());
        let audits = db.list_audit_log(10, 0).unwrap();
        assert!(audits.iter().any(|e| e.action == "file_policy_skip"));
        let manifest = independent_tree_manifest(src.path()).unwrap();
        verify_against_independent_manifest(dst.path(), &manifest, &policy).unwrap();
    }

    #[test]
    fn import_copy_lfs_gitattributes_survives_stale_remove() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("model.bin"), vec![0u8; 200]).unwrap();
        std::fs::write(src.path().join("readme.txt"), "hello").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        let policy = FilePolicy::with_lfs(0, vec![], 100, &[]);
        let db = test_db();
        let stats = copy_tree_with_policy(src.path(), dst.path(), &policy, &db).unwrap();
        assert_eq!(stats.copied, 2);
        assert_eq!(stats.lfs_tracked, 1);
        let gitattr = dst.path().join(".gitattributes");
        assert!(
            gitattr.exists(),
            ".gitattributes should be created for LFS-tracked files"
        );
        let before = std::fs::read_to_string(&gitattr).unwrap();
        assert!(before.contains("filter=lfs"));

        remove_stale_files(src.path(), dst.path()).unwrap();
        assert!(
            gitattr.exists(),
            "stale-remove must not delete engine-written .gitattributes"
        );
        assert_eq!(std::fs::read_to_string(&gitattr).unwrap(), before);
        assert!(dst.path().join(".git").exists());
        assert!(!is_stale_remove_protected(std::ffi::OsStr::new(
            ".gitattributes"
        )));
        assert!(!is_reserved_vcs_metadata(std::ffi::OsStr::new(
            ".gitattributes"
        )));
    }

    #[test]
    fn import_stale_remove_drops_planted_gitattributes_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let outside = tmp.path().join("outside-attrs");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(&outside, "* filter=evil\n").unwrap();
        symlink(&outside, dst.join(".gitattributes")).unwrap();
        std::fs::create_dir(dst.join(".git")).unwrap();

        remove_stale_files(&src, &dst).unwrap();

        assert!(!dst.join(".gitattributes").exists());
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "* filter=evil\n",
            "removing a planted .gitattributes symlink must not follow it"
        );
    }

    #[test]
    fn import_stale_remove_drops_planted_gitattributes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("keep.txt"), "keep").unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/a.txt"), "a").unwrap();

        std::fs::write(dst.path().join("keep.txt"), "keep").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::write(dst.path().join(".gitattributes"), "* filter=evil\n").unwrap();
        std::fs::create_dir(dst.path().join("sub")).unwrap();
        std::fs::write(dst.path().join("sub/a.txt"), "a").unwrap();
        std::fs::write(dst.path().join("sub/.gitattributes"), "* filter=evil\n").unwrap();

        remove_stale_files(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(dst.path().join("sub/a.txt").exists());
        assert!(dst.path().join(".git/HEAD").exists());
        assert!(
            !dst.path().join(".gitattributes").exists(),
            "planted root .gitattributes must not survive when the export omits it"
        );
        assert!(
            !dst.path().join("sub/.gitattributes").exists(),
            "nested .gitattributes must not be name-protected"
        );
    }

    #[test]
    fn import_stale_remove_strips_planted_rules_keeps_engine_lfs() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("model.bin"), vec![0u8; 200]).unwrap();
        std::fs::write(src.path().join("readme.txt"), "hello").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(
            dst.path().join(".gitattributes"),
            "* filter=evil\n*.c filter=evil\n",
        )
        .unwrap();

        let policy = FilePolicy::with_lfs(0, vec![], 100, &[]);
        copy_tree_with_policy(src.path(), dst.path(), &policy, &test_db()).unwrap();
        let merged = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert!(
            merged.contains("filter=evil"),
            "ensure_lfs_tracked appends onto existing content before reconcile: {merged}"
        );
        assert!(merged.contains("filter=lfs"), "{merged}");

        remove_stale_files(src.path(), dst.path()).unwrap();
        let after = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert_eq!(after, "*.bin filter=lfs diff=lfs merge=lfs -text\n");
        assert!(
            !after.contains("evil"),
            "planted filter rules must not remain: {after}"
        );
    }

    #[test]
    fn import_stale_remove_keeps_exported_gitattributes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.path().join("readme.txt"), "hello").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        // Engine marker from an earlier LFS sync must not overwrite attributes
        // the current export still ships.
        crate::lfs::ensure_lfs_tracked(dst.path(), "*.bin").unwrap();

        copy_tree_with_policy(src.path(), dst.path(), &noop_policy(), &test_db()).unwrap();
        remove_stale_files(src.path(), dst.path()).unwrap();

        let after = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert_eq!(after, "* text=auto\n");
    }

    #[test]
    fn import_copy_export_present_gitattributes_symlink_outside_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let outside = tmp.path().join("outside-attrs");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(&outside, "* filter=evil\n").unwrap();
        std::fs::write(src.join(".gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.join("readme.txt"), "hello").unwrap();
        std::fs::create_dir(dst.join(".git")).unwrap();
        symlink(&outside, dst.join(".gitattributes")).unwrap();

        copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap();
        remove_stale_files(&src, &dst).unwrap();

        let meta = std::fs::symlink_metadata(dst.join(".gitattributes")).unwrap();
        assert!(
            meta.file_type().is_file(),
            "export-present copy must replace a planted .gitattributes symlink with a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join(".gitattributes")).unwrap(),
            "* text=auto\n"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "* filter=evil\n",
            "must not write through the planted .gitattributes symlink"
        );
    }

    #[test]
    fn import_export_present_strips_planted_filter_keeps_export_and_engine_lfs() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.path().join("model.bin"), vec![0u8; 200]).unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(
            dst.path().join(".gitattributes"),
            "* filter=evil\n*.c filter=evil\n",
        )
        .unwrap();

        let policy = FilePolicy::with_lfs(0, vec![], 100, &[]);
        copy_tree_with_policy(src.path(), dst.path(), &policy, &test_db()).unwrap();
        remove_stale_files(src.path(), dst.path()).unwrap();

        let after = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert_eq!(
            after,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
        assert!(
            !after.contains("evil"),
            "planted destination-only filter rules must not survive: {after}"
        );
    }

    #[test]
    fn import_stale_remove_export_present_dest_symlink_replaced_without_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let outside = tmp.path().join("outside-attrs");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(&outside, "* filter=evil\n").unwrap();
        std::fs::write(src.join(".gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.join("readme.txt"), "hello").unwrap();
        std::fs::create_dir(dst.join(".git")).unwrap();
        symlink(&outside, dst.join(".gitattributes")).unwrap();

        remove_stale_files(&src, &dst).unwrap();

        let meta = std::fs::symlink_metadata(dst.join(".gitattributes")).unwrap();
        assert!(
            meta.file_type().is_file(),
            "stale-remove must replace a planted .gitattributes symlink with a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join(".gitattributes")).unwrap(),
            "* text=auto\n"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "* filter=evil\n",
            "must not write through the planted .gitattributes symlink"
        );
    }

    #[test]
    fn import_stale_remove_export_present_export_symlink_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let outside = tmp.path().join("outside-export-canary");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(&outside, "OUTSIDE-EXPORT-CANARY filter=evil\n").unwrap();
        symlink(&outside, src.join(".gitattributes")).unwrap();
        std::fs::write(src.join("readme.txt"), "hello").unwrap();
        std::fs::write(dst.join(".gitattributes"), "dest-owned\n").unwrap();
        std::fs::create_dir(dst.join(".git")).unwrap();

        let err = remove_stale_files(&src, &dst).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported symlink at exported .gitattributes"),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join(".gitattributes")).unwrap(),
            "dest-owned\n",
            "must not publish export-side symlink target into destination"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "OUTSIDE-EXPORT-CANARY filter=evil\n"
        );
    }

    #[test]
    fn import_copy_nested_gitattributes_symlink_outside_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let outside = tmp.path().join("outside-nested-attrs");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(dst.join("sub")).unwrap();
        std::fs::write(&outside, "* filter=evil\n").unwrap();
        std::fs::write(src.join("sub/.gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.join("sub/readme.txt"), "hello").unwrap();
        symlink(&outside, dst.join("sub/.gitattributes")).unwrap();

        copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap();

        let meta = std::fs::symlink_metadata(dst.join("sub/.gitattributes")).unwrap();
        assert!(
            meta.file_type().is_file(),
            "nested copy must replace a planted .gitattributes symlink with a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/.gitattributes")).unwrap(),
            "* text=auto\n"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "* filter=evil\n",
            "must not write through the planted nested .gitattributes symlink"
        );
    }

    #[test]
    fn import_copy_dest_symlink_outside_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        let outside = tmp.path().join("outside-secret");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(&outside, "SECRET").unwrap();
        std::fs::write(src.join("payload.txt"), "copied").unwrap();
        symlink(&outside, dst.join("payload.txt")).unwrap();

        copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap();

        let meta = std::fs::symlink_metadata(dst.join("payload.txt")).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(
            std::fs::read_to_string(dst.join("payload.txt")).unwrap(),
            "copied"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "SECRET",
            "must not write through a planted destination symlink"
        );
    }

    #[test]
    fn import_copy_open_no_follow_write_refuses_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let dst = tmp.path().join("dst.txt");
        std::fs::write(&outside, "SECRET").unwrap();
        symlink(&outside, &dst).unwrap();

        let err = open_no_follow_write(&dst).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "unexpected error: {err}"
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "SECRET");
    }

    #[test]
    fn import_export_present_strips_dest_only_bare_filter_lfs() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.path().join("model.bin"), vec![0u8; 200]).unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".gitattributes"), "* filter=lfs\n").unwrap();

        let policy = FilePolicy::with_lfs(0, vec![], 100, &[]);
        copy_tree_with_policy(src.path(), dst.path(), &policy, &test_db()).unwrap();
        remove_stale_files(src.path(), dst.path()).unwrap();

        let after = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert_eq!(
            after,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
        assert!(
            !after.lines().any(|line| line.trim() == "* filter=lfs"),
            "destination-only bare filter=lfs must not survive: {after}"
        );
    }

    #[test]
    fn import_stale_remove_export_present_strips_planted_filter_without_copy() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitattributes"), "* text=auto\n").unwrap();
        std::fs::write(src.path().join("readme.txt"), "hello").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        crate::lfs::ensure_lfs_tracked(dst.path(), "*.bin").unwrap();
        std::fs::write(
            dst.path().join(".gitattributes"),
            "* filter=evil\n*.c filter=evil\n",
        )
        .unwrap();

        remove_stale_files(src.path(), dst.path()).unwrap();

        let after = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert_eq!(
            after,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
        assert!(
            !after.contains("evil"),
            "export-present reconcile must drop planted filter rules: {after}"
        );
    }

    #[test]
    fn legacy_personal_import_watermarks_use_helper_caller_path() {
        let db = test_db();
        assert!(db.list_repositories().unwrap().is_empty());
        let sha = "f".repeat(40);
        persist_legacy_personal_import_watermarks(&db, 42, &sha).unwrap();
        assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("42"));
        assert_eq!(
            db.get_watermark("git_sha").unwrap().as_deref(),
            Some(sha.as_str())
        );
    }

    #[test]
    fn import_full_history_writer_is_copy_tree_with_policy() {
        // run_full_import calls copy_tree_with_policy; this exercises that
        // same writer the way a full-export revision does.
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("README"), "full-import-path\n").unwrap();
        std::fs::write(src.path().join(".gitignore"), "target\n").unwrap();
        let stats =
            copy_tree_with_policy(src.path(), dst.path(), &noop_policy(), &test_db()).unwrap();
        assert_eq!(stats.copied, 2);
        let manifest = independent_tree_manifest(src.path()).unwrap();
        verify_against_independent_manifest(dst.path(), &manifest, &noop_policy()).unwrap();
    }
}
