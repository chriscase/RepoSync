//! Git LFS utilities for personal branch mode.
//!
//! Provides:
//! - LFS pointer detection and parsing
//! - LFS pointer creation (for SVN→Git when files exceed the LFS threshold)
//! - `.gitattributes` management for LFS-tracked patterns
//! - Preflight check that `git lfs` CLI is available
//! - LFS pointer resolution via `git lfs smudge` (for Git→SVN)

use std::io::Write;
use std::path::Path;
use std::process::Command;

use tracing::{debug, info};

// ---------------------------------------------------------------------------
// LFS pointer format
// ---------------------------------------------------------------------------

/// Magic prefix of every Git LFS pointer file.
const LFS_POINTER_PREFIX: &str = "version https://git-lfs.github.com/spec/v1\n";

/// Parsed content of a Git LFS pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsPointer {
    /// The SHA-256 OID of the blob in LFS storage.
    pub oid: String,
    /// Size in bytes of the actual file content.
    pub size: u64,
}

/// Check whether `content` is a Git LFS pointer file.
///
/// A pointer file is a small text file (typically under 200 bytes) starting
/// with `version https://git-lfs.github.com/spec/v1`.
pub fn is_lfs_pointer(content: &[u8]) -> bool {
    // LFS pointers are always small text files.
    if content.len() > 512 {
        return false;
    }
    match std::str::from_utf8(content) {
        Ok(text) => text.starts_with(LFS_POINTER_PREFIX),
        Err(_) => false,
    }
}

/// Parse a Git LFS pointer from bytes.
///
/// Returns `None` if the content is not a valid pointer.
pub fn parse_lfs_pointer(content: &[u8]) -> Option<LfsPointer> {
    let text = std::str::from_utf8(content).ok()?;
    if !text.starts_with(LFS_POINTER_PREFIX) {
        return None;
    }

    let mut oid = None;
    let mut size = None;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("oid sha256:") {
            oid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("size ") {
            size = rest.trim().parse::<u64>().ok();
        }
    }

    Some(LfsPointer {
        oid: oid?,
        size: size?,
    })
}

/// Create a Git LFS pointer file for a blob of the given size.
///
/// The OID is computed as the SHA-256 of the content.
pub fn create_lfs_pointer(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    let hash = hasher.finalize();
    let oid = hex::encode(hash);

    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        oid,
        content.len()
    )
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

/// Check whether `git lfs` is installed and available on PATH.
///
/// Returns `Ok(version_string)` on success, `Err(message)` on failure.
pub fn preflight_check() -> Result<String, String> {
    match Command::new("git").args(["lfs", "version"]).output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            debug!(version = %version, "git-lfs preflight passed");
            Ok(version)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!(
                "git lfs version failed (exit code {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ))
        }
        Err(e) => Err(format!(
            "git lfs not found on PATH. Install git-lfs: https://git-lfs.com — {}",
            e
        )),
    }
}

// ---------------------------------------------------------------------------
// .gitattributes management
// ---------------------------------------------------------------------------

/// Marker under the destination `.git/` directory listing LFS patterns this
/// engine has applied. Copy and stale-remove both skip reserved `.git/`, so
/// an SVN export cannot plant the marker. Absent from the export, root
/// `.gitattributes` is rewritten to exactly these patterns.
pub(crate) const ENGINE_LFS_PATTERN_MARKER: &str = "reposync-lfs-patterns";

/// Canonical attribute text for one engine LFS pattern.
pub(crate) fn lfs_tracking_line(pattern: &str) -> String {
    format!("{pattern} filter=lfs diff=lfs merge=lfs -text")
}

fn is_single_line_pattern(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.contains(['\n', '\r', '\0'])
}

pub(crate) fn unlink_gitattributes_symlink(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn git_metadata_dir(repo_root: &Path) -> Option<std::path::PathBuf> {
    let git_dir = repo_root.join(".git");
    let meta = std::fs::symlink_metadata(&git_dir).ok()?;
    if meta.is_dir() {
        Some(git_dir)
    } else {
        None
    }
}

fn read_engine_lfs_patterns(path: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Ok(Vec::new()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let text = std::fs::read_to_string(path)?;
    let mut patterns = Vec::new();
    for line in text.lines() {
        if !is_single_line_pattern(line) {
            continue;
        }
        if !patterns.iter().any(|existing| existing == line) {
            patterns.push(line.to_string());
        }
    }
    Ok(patterns)
}

fn record_engine_lfs_pattern(repo_root: &Path, pattern: &str) -> std::io::Result<()> {
    let Some(git_dir) = git_metadata_dir(repo_root) else {
        return Ok(());
    };
    let path = git_dir.join(ENGINE_LFS_PATTERN_MARKER);
    let mut patterns = read_engine_lfs_patterns(&path)?;
    if patterns.iter().any(|existing| existing == pattern) {
        return Ok(());
    }
    patterns.push(pattern.to_string());
    let mut body = String::new();
    for recorded in &patterns {
        body.push_str(recorded);
        body.push('\n');
    }
    if std::fs::symlink_metadata(&path)
        .map(|meta| !meta.file_type().is_file())
        .unwrap_or(false)
    {
        std::fs::remove_file(&path)?;
    }
    std::fs::write(path, body)
}

fn gitattributes_line_attributes(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    trimmed
        .find(|c: char| c.is_whitespace())
        .map(|idx| trimmed[idx..].trim_start())
        .filter(|attrs| !attrs.is_empty())
}

fn gitattributes_line_has_exact_attribute(line: &str, attribute: &str) -> bool {
    gitattributes_line_attributes(line)
        .is_some_and(|attrs| attrs.split_whitespace().any(|token| token == attribute))
}

fn gitattributes_line_is_planted_filter(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    gitattributes_line_attributes(trimmed).is_some_and(|attrs| {
        attrs
            .split_whitespace()
            .any(|token| token.starts_with("filter=") && token != "filter=lfs")
    })
}

/// Engine-owned LFS lines always carry the full tracking attribute set.
fn gitattributes_line_is_full_engine_lfs(line: &str) -> bool {
    let trimmed = line.trim();
    gitattributes_line_has_exact_attribute(trimmed, "filter=lfs")
        && gitattributes_line_has_exact_attribute(trimmed, "diff=lfs")
        && gitattributes_line_has_exact_attribute(trimmed, "merge=lfs")
        && trimmed.contains("-text")
}

fn export_body_contains_line(export_body: &str, line: &str) -> bool {
    let trimmed = line.trim();
    export_body
        .lines()
        .any(|candidate| candidate.trim() == trimmed)
}

/// Destination-only bare `filter=lfs` rules (for example `* filter=lfs` without
/// the engine's `diff=lfs merge=lfs -text` bundle) are planted filter rules.
fn gitattributes_line_is_planted_dest_lfs(line: &str, export_body: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    if !gitattributes_line_has_exact_attribute(trimmed, "filter=lfs") {
        return false;
    }
    if gitattributes_line_is_full_engine_lfs(trimmed) {
        return false;
    }
    !export_body_contains_line(export_body, trimmed)
}

fn push_gitattributes_line(lines: &mut Vec<String>, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    if !lines.iter().any(|existing| existing == trimmed) {
        lines.push(trimmed.to_string());
    }
}

/// Merge export-shipped `.gitattributes` with engine LFS lines already on the
/// destination and, when `strip_planted` is set, lines recorded in
/// [`engine_gitattributes_body`].
///
/// Fail-closed rules when `strip_planted` is true:
/// - export lines always survive;
/// - destination-only planted `filter=` rules (including bare `* filter=lfs`)
///   are dropped;
/// - engine-recorded LFS lines from [`engine_gitattributes_body`] are merged in.
///
/// When `strip_planted` is false, legitimate destination engine LFS lines with
/// the full tracking attribute bundle may be kept alongside export lines.
pub(crate) fn merge_export_present_gitattributes(
    export_body: &str,
    dest_body: Option<&str>,
    engine_body: Option<&str>,
    strip_planted: bool,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in export_body.lines() {
        push_gitattributes_line(&mut lines, line);
    }

    if !strip_planted {
        if let Some(body) = dest_body {
            for line in body.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if gitattributes_line_has_exact_attribute(trimmed, "filter=lfs") {
                    push_gitattributes_line(&mut lines, trimmed);
                }
            }
        }
    }

    if strip_planted {
        if let Some(engine) = engine_body {
            for line in engine.lines() {
                push_gitattributes_line(&mut lines, line);
            }
        }
    }

    let mut body = String::new();
    for line in lines {
        body.push_str(&line);
        body.push('\n');
    }
    body
}

pub(crate) fn export_present_gitattributes_needs_strip(
    dest_was_symlink: bool,
    dest_body: Option<&str>,
    export_body: &str,
) -> bool {
    dest_was_symlink
        || dest_body
            .map(|body| {
                body.lines().any(|line| {
                    gitattributes_line_is_planted_filter(line)
                        || gitattributes_line_is_planted_dest_lfs(line, export_body)
                })
            })
            .unwrap_or(false)
}

/// `.gitattributes` body for LFS patterns recorded by [`ensure_lfs_tracked`].
///
/// `None` when this destination has no engine marker (no `.git/` directory,
/// or no patterns recorded). Callers must not treat an untracked
/// `.gitattributes` as engine-owned.
pub(crate) fn engine_gitattributes_body(repo_root: &Path) -> std::io::Result<Option<String>> {
    let Some(git_dir) = git_metadata_dir(repo_root) else {
        return Ok(None);
    };
    let patterns = read_engine_lfs_patterns(&git_dir.join(ENGINE_LFS_PATTERN_MARKER))?;
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut body = String::new();
    for pattern in patterns {
        body.push_str(&lfs_tracking_line(&pattern));
        body.push('\n');
    }
    Ok(Some(body))
}

/// Ensure that the given file extension or glob pattern is tracked by LFS
/// in the `.gitattributes` file at `repo_root`.
///
/// If the pattern is already present, this is a no-op. Otherwise, appends
/// the appropriate line. Returns `true` if a new line was added.
///
/// When `repo_root/.git` is a directory, the pattern is also recorded in
/// `.git/reposync-lfs-patterns`. Stale-remove uses that marker to keep only
/// engine-written LFS lines and to drop planted filter rules when the export
/// omits `.gitattributes`.
pub fn ensure_lfs_tracked(repo_root: &Path, pattern: &str) -> std::io::Result<bool> {
    if !is_single_line_pattern(pattern) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to record a .gitattributes LFS pattern that is empty or contains a newline",
        ));
    }
    record_engine_lfs_pattern(repo_root, pattern)?;

    let gitattr_path = repo_root.join(".gitattributes");
    let expected_line = lfs_tracking_line(pattern);

    // A planted symlink must not be followed: appending would write the LFS
    // line through to the target and leave the symlink in the work tree.
    unlink_gitattributes_symlink(&gitattr_path)?;

    // Read existing content. Only a regular file counts; do not follow links.
    let existing = match std::fs::symlink_metadata(&gitattr_path) {
        Ok(meta) if meta.file_type().is_file() => std::fs::read_to_string(&gitattr_path)?,
        Ok(_) => String::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    // Check if already tracked.
    for line in existing.lines() {
        let trimmed = line.trim();
        // Match on the pattern part before the attributes.
        if trimmed.starts_with(pattern) && trimmed.contains("filter=lfs") {
            debug!(pattern, "LFS pattern already tracked in .gitattributes");
            return Ok(false);
        }
    }

    // Append the new line.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitattr_path)?;

    // Ensure we start on a new line if the file doesn't end with one.
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }
    writeln!(file, "{}", expected_line)?;

    info!(pattern, path = %gitattr_path.display(), "added LFS tracking to .gitattributes");
    Ok(true)
}

/// Derive a `.gitattributes` pattern for a file path.
///
/// For example, `assets/model.bin` produces `*.bin` (extension-based).
/// If the file has no extension, the exact path is used.
pub fn pattern_for_path(rel_path: &str) -> String {
    if let Some(ext) = Path::new(rel_path).extension() {
        format!("*.{}", ext.to_string_lossy())
    } else {
        rel_path.to_string()
    }
}

// ---------------------------------------------------------------------------
// LFS pointer resolution (smudge)
// ---------------------------------------------------------------------------

/// Resolve an LFS pointer to the actual file content.
///
/// This shells out to `git lfs smudge` which reads the pointer from stdin
/// and writes the actual content to stdout. Requires `git lfs install` to
/// have been run in the repo.
///
/// Returns `Ok(content_bytes)` on success, `Err(message)` on failure.
pub fn resolve_lfs_pointer(repo_root: &Path, pointer_content: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .args(["lfs", "smudge"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git lfs smudge: {}", e))?;

    // Write pointer to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(pointer_content)
            .map_err(|e| format!("failed to write to git lfs smudge stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for git lfs smudge: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git lfs smudge failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    Ok(output.stdout)
}

/// Run `git lfs install` in a repository to set up the LFS hooks.
pub fn install_lfs_hooks(repo_root: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["lfs", "install", "--local"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("failed to run git lfs install: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git lfs install failed: {}", stderr.trim()));
    }

    info!(path = %repo_root.display(), "git lfs install completed");
    Ok(())
}

/// Store a large file as an LFS object and return the pointer content.
///
/// Uses `git lfs clean` to convert file content into an LFS pointer and
/// store the object locally.
pub fn store_lfs_object(repo_root: &Path, content: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .args(["lfs", "clean"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git lfs clean: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content)
            .map_err(|e| format!("failed to write to git lfs clean stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for git lfs clean: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git lfs clean failed: {}", stderr.trim()));
    }

    Ok(output.stdout)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_lfs_pointer_valid() {
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123def456\nsize 12345\n";
        assert!(is_lfs_pointer(pointer));
    }

    #[test]
    fn test_is_lfs_pointer_too_large() {
        let mut large = Vec::from(&b"version https://git-lfs.github.com/spec/v1\n"[..]);
        large.extend(vec![b'x'; 600]);
        assert!(!is_lfs_pointer(&large));
    }

    #[test]
    fn test_is_lfs_pointer_binary() {
        let binary = vec![0xFF, 0xFE, 0x00, 0x01];
        assert!(!is_lfs_pointer(&binary));
    }

    #[test]
    fn test_is_lfs_pointer_regular_file() {
        let content = b"fn main() { println!(\"hello\"); }";
        assert!(!is_lfs_pointer(content));
    }

    #[test]
    fn test_parse_lfs_pointer_valid() {
        let pointer = "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";
        let parsed = parse_lfs_pointer(pointer.as_bytes()).unwrap();
        assert_eq!(
            parsed.oid,
            "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"
        );
        assert_eq!(parsed.size, 12345);
    }

    #[test]
    fn test_parse_lfs_pointer_invalid() {
        let content = b"not a pointer";
        assert!(parse_lfs_pointer(content).is_none());
    }

    #[test]
    fn test_parse_lfs_pointer_incomplete() {
        let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\n";
        // Missing size line.
        assert!(parse_lfs_pointer(pointer).is_none());
    }

    #[test]
    fn test_create_lfs_pointer() {
        let content = b"hello world";
        let pointer = create_lfs_pointer(content);
        assert!(pointer.starts_with("version https://git-lfs.github.com/spec/v1\n"));
        assert!(pointer.contains("oid sha256:"));
        assert!(pointer.contains(&format!("size {}", content.len())));

        // Parse it back.
        let parsed = parse_lfs_pointer(pointer.as_bytes()).unwrap();
        assert_eq!(parsed.size, content.len() as u64);
    }

    #[test]
    fn test_create_and_parse_roundtrip() {
        let content = b"some binary data \x00\x01\x02\x03";
        let pointer = create_lfs_pointer(content);
        let parsed = parse_lfs_pointer(pointer.as_bytes()).unwrap();
        assert_eq!(parsed.size, content.len() as u64);
        // OID should be deterministic.
        let pointer2 = create_lfs_pointer(content);
        assert_eq!(pointer, pointer2);
    }

    #[test]
    fn test_pattern_for_path_with_extension() {
        assert_eq!(pattern_for_path("assets/model.bin"), "*.bin");
        assert_eq!(pattern_for_path("images/photo.png"), "*.png");
        assert_eq!(pattern_for_path("deep/nested/file.tar.gz"), "*.gz");
    }

    #[test]
    fn test_pattern_for_path_without_extension() {
        assert_eq!(pattern_for_path("Makefile"), "Makefile");
        assert_eq!(pattern_for_path("scripts/build"), "scripts/build");
    }

    #[test]
    fn test_ensure_lfs_tracked_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let added = ensure_lfs_tracked(dir.path(), "*.bin").unwrap();
        assert!(added);

        let content = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        assert!(content.contains("*.bin filter=lfs diff=lfs merge=lfs -text"));
    }

    #[test]
    fn test_ensure_lfs_tracked_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure_lfs_tracked(dir.path(), "*.bin").unwrap());
        assert!(!ensure_lfs_tracked(dir.path(), "*.bin").unwrap());

        let content = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        // Should only appear once.
        assert_eq!(content.matches("*.bin filter=lfs").count(), 1);
    }

    #[test]
    fn test_ensure_lfs_tracked_multiple_patterns() {
        let dir = tempfile::tempdir().unwrap();
        ensure_lfs_tracked(dir.path(), "*.bin").unwrap();
        ensure_lfs_tracked(dir.path(), "*.psd").unwrap();

        let content = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        assert!(content.contains("*.bin filter=lfs"));
        assert!(content.contains("*.psd filter=lfs"));
    }

    #[test]
    fn test_ensure_lfs_tracked_records_pattern_in_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(ensure_lfs_tracked(dir.path(), "*.bin").unwrap());
        assert!(!ensure_lfs_tracked(dir.path(), "*.bin").unwrap());
        let marker =
            std::fs::read_to_string(dir.path().join(".git").join(ENGINE_LFS_PATTERN_MARKER))
                .unwrap();
        assert_eq!(marker, "*.bin\n");
        let body = engine_gitattributes_body(dir.path()).unwrap().unwrap();
        assert_eq!(body, "*.bin filter=lfs diff=lfs merge=lfs -text\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap(),
            body
        );
    }

    #[test]
    fn test_ensure_lfs_tracked_skips_marker_without_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        ensure_lfs_tracked(dir.path(), "*.bin").unwrap();
        assert!(engine_gitattributes_body(dir.path()).unwrap().is_none());
        assert!(!dir.path().join(".git").exists());
    }

    #[test]
    fn test_ensure_lfs_tracked_replaces_symlink_without_following() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let outside = dir.path().join("outside-attrs");
        std::fs::write(&outside, "* filter=evil\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join(".gitattributes")).unwrap();

        assert!(ensure_lfs_tracked(dir.path(), "*.bin").unwrap());

        let meta = std::fs::symlink_metadata(dir.path().join(".gitattributes")).unwrap();
        assert!(
            meta.file_type().is_file(),
            "engine .gitattributes must be a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap(),
            "*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "* filter=evil\n",
            "must not append through the planted symlink"
        );
    }

    #[test]
    fn test_merge_export_present_gitattributes_keeps_export_and_dest_engine_lfs() {
        let merged = merge_export_present_gitattributes(
            "* text=auto\n",
            Some("* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            None,
            false,
        );
        assert_eq!(
            merged,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
    }

    #[test]
    fn test_merge_export_present_gitattributes_adds_marker_when_stripping_planted_filters() {
        let merged = merge_export_present_gitattributes(
            "* text=auto\n",
            Some("* filter=evil\n*.c filter=evil\n"),
            Some("*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            true,
        );
        assert_eq!(
            merged,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
    }

    #[test]
    fn test_merge_export_present_gitattributes_ignores_marker_without_planted_filters() {
        let merged = merge_export_present_gitattributes(
            "* text=auto\n",
            Some("* text=auto\n"),
            Some("*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            false,
        );
        assert_eq!(merged, "* text=auto\n");
    }

    #[test]
    fn test_merge_export_present_gitattributes_treats_filter_lfsevil_as_planted() {
        let merged = merge_export_present_gitattributes(
            "* text=auto\n",
            Some("* filter=lfs\n*.evil filter=lfsevil\n* filter=evil\n"),
            Some("*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            true,
        );
        assert_eq!(
            merged,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
        assert!(
            !merged.contains("lfsevil"),
            "filter=lfsevil must not be treated as engine LFS: {merged}"
        );
        assert!(!merged.contains("filter=evil"), "{merged}");
        assert!(
            !merged.contains("* filter=lfs\n"),
            "destination-only bare filter=lfs must not survive: {merged}"
        );
    }

    #[test]
    fn test_merge_export_present_gitattributes_strips_dest_only_bare_filter_lfs() {
        let merged = merge_export_present_gitattributes(
            "* text=auto\n",
            Some("* filter=lfs\n"),
            Some("*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            true,
        );
        assert_eq!(
            merged,
            "* text=auto\n*.bin filter=lfs diff=lfs merge=lfs -text\n"
        );
    }

    #[test]
    fn test_export_present_gitattributes_needs_strip_ignores_commented_filter() {
        assert!(!export_present_gitattributes_needs_strip(
            false,
            Some("# don't use filter=ident here\n* text=auto\n"),
            "* text=auto\n"
        ));
    }

    #[test]
    fn test_export_present_gitattributes_needs_strip_detects_dest_only_bare_filter_lfs() {
        assert!(export_present_gitattributes_needs_strip(
            false,
            Some("* filter=lfs\n"),
            "* text=auto\n"
        ));
        assert!(!export_present_gitattributes_needs_strip(
            false,
            Some("*.bin filter=lfs diff=lfs merge=lfs -text\n"),
            "* text=auto\n"
        ));
    }

    #[test]
    fn test_ensure_lfs_tracked_rejects_multiline_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let err = ensure_lfs_tracked(dir.path(), "*.bin\n* filter=evil").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(engine_gitattributes_body(dir.path()).unwrap().is_none());
        assert!(!dir.path().join(".gitattributes").exists());
    }

    #[test]
    fn test_preflight_check() {
        // This test depends on the host environment. If git-lfs is
        // installed, it should pass. We only verify the function doesn't
        // panic.
        let result = preflight_check();
        // Either Ok or Err — both are acceptable in CI.
        match &result {
            Ok(v) => assert!(v.contains("git-lfs"), "version: {}", v),
            Err(e) => assert!(!e.is_empty(), "error should be descriptive: {}", e),
        }
    }
}
