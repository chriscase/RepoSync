//! File-policy enforcement for sync operations.
//!
//! Provides [`FilePolicy`] which encapsulates `max_file_size` and
//! `ignore_patterns` from [`PersonalOptionsConfig`] and evaluates candidate
//! files before they are copied, committed, or replayed.
//!
//! # Decision model
//!
//! For each candidate file the policy returns a [`FilePolicyDecision`]:
//!
//! | Condition | Decision |
//! |-----------|----------|
//! | Path matches an ignore pattern | `Ignored` |
//! | Size exceeds `max_file_size` (when > 0) | `Oversize` |
//! | Size exceeds `lfs_threshold` and LFS enabled | `LfsTrack` |
//! | None of the above | `Allow` |
//!
//! All decisions carry the relative path and size for auditing.

use std::path::Path;

use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Decision enum
// ---------------------------------------------------------------------------

/// The outcome of evaluating a file against the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePolicyDecision {
    /// File passes all checks — sync it normally.
    Allow,
    /// File matches an ignore pattern — skip it.
    Ignored { pattern: String },
    /// File exceeds the configured `max_file_size` — skip it.
    Oversize { size: u64, limit: u64 },
    /// File qualifies for LFS tracking (above lfs_threshold, LFS enabled).
    LfsTrack { size: u64, threshold: u64 },
}

impl FilePolicyDecision {
    /// `true` if the file should be synced (either `Allow` or `LfsTrack`).
    pub fn should_sync(&self) -> bool {
        matches!(self, Self::Allow | Self::LfsTrack { .. })
    }

    /// `true` if the file is blocked (ignored or oversize).
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Ignored { .. } | Self::Oversize { .. })
    }

    /// Short human-readable label for audit/logging.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ignored { .. } => "ignored",
            Self::Oversize { .. } => "oversize",
            Self::LfsTrack { .. } => "lfs-track",
        }
    }
}

// ---------------------------------------------------------------------------
// FilePolicy
// ---------------------------------------------------------------------------

/// Evaluates candidate files against size limits and ignore patterns.
///
/// Constructed from the personal-mode `[options]` config section. Thread-safe
/// and cheap to clone (all data is owned strings/u64).
#[derive(Debug, Clone)]
pub struct FilePolicy {
    /// Maximum allowed file size in bytes. 0 = no limit.
    max_file_size: u64,
    /// Glob patterns to exclude. Matched against the *relative* path.
    ignore_patterns: Vec<String>,
    /// LFS size threshold in bytes. 0 = LFS disabled.
    lfs_threshold: u64,
    /// Whether LFS is enabled (threshold > 0 or patterns non-empty).
    lfs_enabled: bool,
    /// Glob patterns for files that should always be LFS-tracked regardless of size.
    lfs_patterns: Vec<String>,
}

impl FilePolicy {
    /// Create a new `FilePolicy` from config values.
    pub fn new(max_file_size: u64, ignore_patterns: Vec<String>) -> Self {
        Self {
            max_file_size,
            ignore_patterns,
            lfs_threshold: 0,
            lfs_enabled: false,
            lfs_patterns: Vec::new(),
        }
    }

    /// Create a `FilePolicy` with LFS support.
    ///
    /// `lfs_patterns` are glob patterns for files that should always be
    /// LFS-tracked regardless of size (e.g., `["*.psd", "*.bin"]`).
    pub fn with_lfs(
        max_file_size: u64,
        ignore_patterns: Vec<String>,
        lfs_threshold: u64,
        lfs_patterns: &[String],
    ) -> Self {
        let has_patterns = !lfs_patterns.is_empty();
        Self {
            max_file_size,
            ignore_patterns,
            lfs_threshold,
            lfs_enabled: lfs_threshold > 0 || has_patterns,
            lfs_patterns: lfs_patterns.to_vec(),
        }
    }

    /// Evaluate a file.
    ///
    /// `rel_path` is the file's path relative to the repo root (forward-slash
    /// separated). `size` is the file size in bytes.
    pub fn evaluate(&self, rel_path: &str, size: u64) -> FilePolicyDecision {
        // 1. Check ignore patterns first.
        for pattern in &self.ignore_patterns {
            if self.matches_pattern(rel_path, pattern) {
                debug!(
                    path = rel_path,
                    pattern = pattern.as_str(),
                    "file matches ignore pattern"
                );
                return FilePolicyDecision::Ignored {
                    pattern: pattern.clone(),
                };
            }
        }

        // 2. Check max_file_size (0 = unlimited).
        if self.max_file_size > 0 && size > self.max_file_size {
            warn!(
                path = rel_path,
                size,
                limit = self.max_file_size,
                "file exceeds max_file_size — skipping"
            );
            return FilePolicyDecision::Oversize {
                size,
                limit: self.max_file_size,
            };
        }

        // 3. Check LFS patterns (pattern-based LFS, regardless of size).
        for pattern in &self.lfs_patterns {
            if self.matches_pattern(rel_path, pattern) {
                info!(
                    path = rel_path,
                    pattern = pattern.as_str(),
                    size,
                    "file matches lfs_pattern — LFS tracking"
                );
                return FilePolicyDecision::LfsTrack {
                    size,
                    threshold: 0, // pattern-based, not threshold-based
                };
            }
        }

        // 4. Check LFS threshold.
        if self.lfs_threshold > 0 && size > self.lfs_threshold {
            info!(
                path = rel_path,
                size,
                threshold = self.lfs_threshold,
                "file qualifies for LFS tracking (size threshold)"
            );
            return FilePolicyDecision::LfsTrack {
                size,
                threshold: self.lfs_threshold,
            };
        }

        FilePolicyDecision::Allow
    }

    /// Evaluate a file on disk (reads metadata for size).
    ///
    /// `base_dir` is the repo/export root, `rel_path` is relative to it.
    /// If the file cannot be stat'd, returns `Allow` (let the copy/commit
    /// operation itself report the I/O error).
    pub fn evaluate_path(&self, base_dir: &Path, rel_path: &str) -> FilePolicyDecision {
        let full = base_dir.join(rel_path);
        let size = match std::fs::metadata(&full) {
            Ok(m) => m.len(),
            Err(_) => return FilePolicyDecision::Allow,
        };
        self.evaluate(rel_path, size)
    }

    /// Whether the policy has any constraints at all.
    pub fn has_constraints(&self) -> bool {
        self.max_file_size > 0
            || !self.ignore_patterns.is_empty()
            || self.lfs_enabled
            || !self.lfs_patterns.is_empty()
    }

    /// Max file size (for display/logging).
    pub fn max_file_size(&self) -> u64 {
        self.max_file_size
    }

    /// Whether LFS is enabled.
    pub fn lfs_enabled(&self) -> bool {
        self.lfs_enabled
    }

    /// LFS threshold (for display/logging).
    pub fn lfs_threshold(&self) -> u64 {
        self.lfs_threshold
    }

    // -----------------------------------------------------------------------
    // Pattern matching
    // -----------------------------------------------------------------------

    /// Test whether `rel_path` matches a glob `pattern`.
    ///
    /// Supports:
    /// - `*` — match any single path component segment
    /// - `**` — match zero or more path segments
    /// - `*.ext` — match by extension
    /// - `dir/**` — match everything under a directory
    fn matches_pattern(&self, rel_path: &str, pattern: &str) -> bool {
        // Normalize to forward slashes for consistent matching.
        let path = rel_path.replace('\\', "/");
        let pat = pattern.replace('\\', "/");

        // Use glob-match crate for proper glob semantics.
        glob_match::glob_match(&pat, &path)
    }
}

// ---------------------------------------------------------------------------
// Construct from PersonalOptionsConfig
// ---------------------------------------------------------------------------

impl From<&crate::personal_config::PersonalOptionsConfig> for FilePolicy {
    fn from(opts: &crate::personal_config::PersonalOptionsConfig) -> Self {
        if opts.lfs_threshold > 0 || !opts.lfs_patterns.is_empty() {
            Self::with_lfs(
                opts.max_file_size,
                opts.ignore_patterns.clone(),
                opts.lfs_threshold,
                &opts.lfs_patterns,
            )
        } else {
            Self::new(opts.max_file_size, opts.ignore_patterns.clone())
        }
    }
}

impl From<&crate::config::SyncConfig> for FilePolicy {
    fn from(sync: &crate::config::SyncConfig) -> Self {
        if sync.lfs_threshold > 0 || !sync.lfs_patterns.is_empty() {
            Self::with_lfs(
                sync.max_file_size,
                sync.ignore_patterns.clone(),
                sync.lfs_threshold,
                &sync.lfs_patterns,
            )
        } else {
            Self::new(sync.max_file_size, sync.ignore_patterns.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allow_when_no_constraints() {
        let policy = FilePolicy::new(0, vec![]);
        let decision = policy.evaluate("src/main.rs", 1024);
        assert_eq!(decision, FilePolicyDecision::Allow);
        assert!(decision.should_sync());
        assert!(!decision.is_blocked());
        assert_eq!(decision.label(), "allow");
    }

    #[test]
    fn test_oversize_blocks_file() {
        let policy = FilePolicy::new(1000, vec![]);
        let decision = policy.evaluate("bigfile.bin", 2000);
        assert!(matches!(
            decision,
            FilePolicyDecision::Oversize {
                size: 2000,
                limit: 1000
            }
        ));
        assert!(!decision.should_sync());
        assert!(decision.is_blocked());
        assert_eq!(decision.label(), "oversize");
    }

    #[test]
    fn test_under_limit_allowed() {
        let policy = FilePolicy::new(5000, vec![]);
        let decision = policy.evaluate("small.txt", 500);
        assert_eq!(decision, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_exact_limit_allowed() {
        let policy = FilePolicy::new(1000, vec![]);
        // Exactly at the limit should pass (only > limit is blocked).
        let decision = policy.evaluate("exact.txt", 1000);
        assert_eq!(decision, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_ignore_pattern_star_ext() {
        let policy = FilePolicy::new(0, vec!["*.log".into()]);
        let decision = policy.evaluate("app.log", 100);
        assert!(matches!(decision, FilePolicyDecision::Ignored { .. }));
        assert!(decision.is_blocked());
        assert_eq!(decision.label(), "ignored");
    }

    #[test]
    fn test_ignore_pattern_no_match() {
        let policy = FilePolicy::new(0, vec!["*.log".into()]);
        let decision = policy.evaluate("app.txt", 100);
        assert_eq!(decision, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_ignore_pattern_double_star() {
        let policy = FilePolicy::new(0, vec!["build/**".into()]);
        let decision = policy.evaluate("build/out/main.o", 100);
        assert!(matches!(decision, FilePolicyDecision::Ignored { .. }));
    }

    #[test]
    fn test_ignore_pattern_nested_ext() {
        let policy = FilePolicy::new(0, vec!["**/*.class".into()]);
        let d1 = policy.evaluate("com/example/Main.class", 100);
        assert!(matches!(d1, FilePolicyDecision::Ignored { .. }));
        let d2 = policy.evaluate("com/example/Main.java", 100);
        assert_eq!(d2, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_ignore_checked_before_size() {
        // If a file matches ignore AND is oversize, the decision should be Ignored
        // (ignore patterns take priority).
        let policy = FilePolicy::new(100, vec!["*.tmp".into()]);
        let decision = policy.evaluate("data.tmp", 5000);
        assert!(matches!(decision, FilePolicyDecision::Ignored { .. }));
    }

    #[test]
    fn test_multiple_ignore_patterns() {
        let policy = FilePolicy::new(
            0,
            vec!["*.log".into(), "build/**".into(), ".idea/**".into()],
        );
        assert!(policy.evaluate("server.log", 10).is_blocked());
        assert!(policy.evaluate("build/output.o", 10).is_blocked());
        assert!(policy.evaluate(".idea/workspace.xml", 10).is_blocked());
        assert!(!policy.evaluate("src/main.rs", 10).is_blocked());
    }

    #[test]
    fn test_has_constraints() {
        assert!(!FilePolicy::new(0, vec![]).has_constraints());
        assert!(FilePolicy::new(1000, vec![]).has_constraints());
        assert!(FilePolicy::new(0, vec!["*.log".into()]).has_constraints());
    }

    #[test]
    fn test_from_options_config() {
        let opts = crate::personal_config::PersonalOptionsConfig {
            max_file_size: 5000,
            ignore_patterns: vec!["*.tmp".into()],
            ..Default::default()
        };
        let policy = FilePolicy::from(&opts);
        assert_eq!(policy.max_file_size(), 5000);
        assert!(policy.evaluate("data.tmp", 100).is_blocked());
    }

    #[test]
    fn test_evaluate_path_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; 2000]).unwrap();

        let policy = FilePolicy::new(1000, vec![]);
        assert_eq!(
            policy.evaluate_path(dir.path(), "small.txt"),
            FilePolicyDecision::Allow
        );
        assert!(matches!(
            policy.evaluate_path(dir.path(), "big.bin"),
            FilePolicyDecision::Oversize { .. }
        ));
        // Non-existent file → Allow (let I/O error surface elsewhere).
        assert_eq!(
            policy.evaluate_path(dir.path(), "missing.txt"),
            FilePolicyDecision::Allow
        );
    }

    #[test]
    fn test_lfs_threshold() {
        let policy = FilePolicy::with_lfs(0, vec![], 1000, &[]);
        // Under threshold → Allow.
        assert_eq!(policy.evaluate("small.txt", 500), FilePolicyDecision::Allow);
        // Over threshold → LfsTrack.
        let decision = policy.evaluate("model.bin", 2000);
        assert!(matches!(
            decision,
            FilePolicyDecision::LfsTrack {
                size: 2000,
                threshold: 1000
            }
        ));
        assert!(decision.should_sync());
        assert!(!decision.is_blocked());
        assert_eq!(decision.label(), "lfs-track");
    }

    #[test]
    fn test_oversize_takes_precedence_over_lfs() {
        // If max_file_size is set AND lfs_threshold, oversize wins.
        let policy = FilePolicy::with_lfs(5000, vec![], 1000, &[]);
        // 3000 is above LFS threshold but under max_file_size.
        let d1 = policy.evaluate("med.bin", 3000);
        assert!(matches!(d1, FilePolicyDecision::LfsTrack { .. }));
        // 6000 is above both. max_file_size blocks it.
        let d2 = policy.evaluate("huge.bin", 6000);
        assert!(matches!(d2, FilePolicyDecision::Oversize { .. }));
    }

    #[test]
    fn test_lfs_patterns_match_regardless_of_size() {
        // lfs_patterns should trigger LfsTrack even for tiny files.
        let policy = FilePolicy::with_lfs(0, vec![], 0, &["*.psd".into(), "*.bin".into()]);
        assert!(policy.lfs_enabled());

        // A tiny .psd file → LfsTrack (pattern-based, not size-based).
        let d1 = policy.evaluate("design.psd", 100);
        assert!(matches!(
            d1,
            FilePolicyDecision::LfsTrack {
                size: 100,
                threshold: 0
            }
        ));
        assert!(d1.should_sync());
        assert_eq!(d1.label(), "lfs-track");

        // A .bin file → LfsTrack (root-level; *.bin matches single path segment).
        let d2 = policy.evaluate("model.bin", 50);
        assert!(matches!(d2, FilePolicyDecision::LfsTrack { .. }));

        // A .txt file → Allow (no pattern match, no threshold).
        let d3 = policy.evaluate("readme.txt", 100);
        assert_eq!(d3, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_lfs_patterns_nested_path() {
        let policy = FilePolicy::with_lfs(0, vec![], 0, &["assets/**/*.bin".into()]);

        // Match nested path.
        let d1 = policy.evaluate("assets/models/large.bin", 10);
        assert!(matches!(d1, FilePolicyDecision::LfsTrack { .. }));

        // No match outside the assets/ tree.
        let d2 = policy.evaluate("src/data.bin", 10);
        assert_eq!(d2, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_lfs_patterns_combined_with_threshold() {
        // Both patterns and threshold active.
        let policy = FilePolicy::with_lfs(0, vec![], 1000, &["*.psd".into()]);
        assert!(policy.lfs_enabled());

        // Small .psd → LfsTrack via pattern (even though under threshold).
        let d1 = policy.evaluate("art.psd", 500);
        assert!(matches!(d1, FilePolicyDecision::LfsTrack { .. }));

        // Large .txt → LfsTrack via threshold.
        let d2 = policy.evaluate("data.txt", 2000);
        assert!(matches!(
            d2,
            FilePolicyDecision::LfsTrack {
                size: 2000,
                threshold: 1000
            }
        ));

        // Small .txt → Allow.
        let d3 = policy.evaluate("small.txt", 100);
        assert_eq!(d3, FilePolicyDecision::Allow);
    }

    #[test]
    fn test_ignore_pattern_takes_precedence_over_lfs_pattern() {
        // If a file is both ignored and LFS-patterned, ignore wins.
        let policy = FilePolicy::with_lfs(0, vec!["*.psd".into()], 0, &["*.psd".into()]);

        let d = policy.evaluate("huge.psd", 100);
        assert!(matches!(d, FilePolicyDecision::Ignored { .. }));
    }

    #[test]
    fn test_oversize_takes_precedence_over_lfs_pattern() {
        // If max_file_size blocks a file, it should be Oversize even if lfs_patterns match.
        let policy = FilePolicy::with_lfs(500, vec![], 0, &["*.bin".into()]);

        // Under size limit → LfsTrack via pattern.
        let d1 = policy.evaluate("small.bin", 100);
        assert!(matches!(d1, FilePolicyDecision::LfsTrack { .. }));

        // Over size limit → Oversize (blocks).
        let d2 = policy.evaluate("huge.bin", 1000);
        assert!(matches!(d2, FilePolicyDecision::Oversize { .. }));
    }

    #[test]
    fn test_lfs_patterns_enable_lfs_without_threshold() {
        // lfs_patterns alone (without threshold) should enable LFS.
        let policy = FilePolicy::with_lfs(0, vec![], 0, &["*.iso".into()]);
        assert!(policy.lfs_enabled());
        assert!(policy.has_constraints());
        assert_eq!(policy.lfs_threshold(), 0);
    }

    #[test]
    fn test_from_options_config_patterns_only() {
        // Regression: From<&PersonalOptionsConfig> must call with_lfs when
        // lfs_patterns is non-empty even when lfs_threshold is 0.
        let opts = crate::personal_config::PersonalOptionsConfig {
            lfs_patterns: vec!["*.bin".into(), "*.psd".into()],
            ..Default::default()
        };
        assert_eq!(opts.lfs_threshold, 0, "precondition: threshold is zero");

        let policy = FilePolicy::from(&opts);
        assert!(
            policy.lfs_enabled(),
            "LFS must be enabled via patterns alone"
        );
        assert!(policy.has_constraints());

        // A .bin file should get LfsTrack.
        let d = policy.evaluate("model.bin", 42);
        assert!(
            matches!(d, FilePolicyDecision::LfsTrack { size: 42, threshold: 0 }),
            "expected LfsTrack from pattern-only config, got {:?}",
            d
        );

        // A .txt file should be Allow (no pattern match, no threshold).
        assert_eq!(policy.evaluate("readme.txt", 42), FilePolicyDecision::Allow);
    }

    #[test]
    fn test_from_options_config_patterns_and_threshold() {
        // Both lfs_patterns and lfs_threshold set via config.
        let opts = crate::personal_config::PersonalOptionsConfig {
            lfs_threshold: 1000,
            lfs_patterns: vec!["*.psd".into()],
            ..Default::default()
        };
        let policy = FilePolicy::from(&opts);
        assert!(policy.lfs_enabled());

        // Small .psd → LfsTrack via pattern (under threshold).
        let d1 = policy.evaluate("art.psd", 100);
        assert!(matches!(d1, FilePolicyDecision::LfsTrack { .. }));

        // Large .txt → LfsTrack via threshold.
        let d2 = policy.evaluate("data.txt", 2000);
        assert!(matches!(d2, FilePolicyDecision::LfsTrack { size: 2000, threshold: 1000 }));

        // Small .txt → Allow.
        assert_eq!(policy.evaluate("small.txt", 100), FilePolicyDecision::Allow);
    }
}
