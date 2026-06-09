//! Classify common sync errors into human-actionable hints.
//!
//! The raw error string returned when a sync cycle fails (a wrapped
//! libgit2, SVN, or network error) is rarely useful to an operator. This
//! module pattern-matches a small set of error families that have
//! well-known fixes and emits a categorised hint that the daemon can
//! both log structurally and persist to the audit log. The UI / logs
//! then teach the operator how to resolve the issue rather than just
//! showing the underlying error string.
//!
//! The classifier is intentionally conservative: it only matches
//! well-known patterns where the suggested fix is very likely correct.
//! Unrecognised errors return `None` and continue to surface as-is.

use std::path::Path;

/// A categorised, operator-facing hint about a sync error.
#[derive(Debug, Clone)]
pub struct OperatorHint {
    /// Short stable category identifier (e.g. `"filesystem_permission"`).
    /// Stable so the UI can apply display rules per category.
    pub category: &'static str,
    /// Severity of the issue.
    pub severity: Severity,
    /// One-sentence description of what's wrong, in human terms.
    pub message: String,
    /// A concrete suggested fix the operator can act on. May contain a
    /// shell command, with placeholders substituted where possible.
    pub suggested_fix: String,
}

/// How urgent the issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Auto-resolves on retry; no operator action needed.
    Transient,
    /// Operator action is required for the repo to recover.
    NeedsAttention,
    /// Service may be unable to continue at all (e.g. disk full).
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Transient => "transient",
            Severity::NeedsAttention => "needs_attention",
            Severity::Critical => "critical",
        }
    }
}

impl OperatorHint {
    /// Render the hint as a single multi-line string suitable for the
    /// audit_log `details` column or any human-facing message field.
    pub fn render_details(&self) -> String {
        format!(
            "[{}] {}\n\nSuggested fix: {}",
            self.category, self.message, self.suggested_fix
        )
    }
}

/// Classify the given error message into an [`OperatorHint`] if it
/// matches one of the known error families. Returns `None` for any
/// unrecognised error.
///
/// `mirror_path`, when provided, is substituted into commands like
/// `chown` so the suggested fix is directly copy-pasteable.
pub fn classify_sync_error(error_text: &str, mirror_path: Option<&Path>) -> Option<OperatorHint> {
    let text = error_text;

    // -- Filesystem permission on the mirror.
    //
    // Symptom: "Permission denied" referencing something inside `.git/`,
    // a `.lock` file, or the repos data directory. Typically caused by a
    // file under the mirror being owned by root rather than the daemon
    // user — e.g. after a manual `sudo git` rescue command leaves a few
    // refs owned by root and the daemon can no longer write its locks.
    if text.contains("Permission denied")
        && (text.contains(".git/") || text.contains(".lock") || text.contains("/repos/"))
    {
        let mirror_str = mirror_path
            .and_then(|p| p.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "/opt/reposync/repos/<repo-id>/git-repo".to_string());
        let daemon_user = std::env::var("USER").unwrap_or_else(|_| "the daemon user".to_string());
        return Some(OperatorHint {
            category: "filesystem_permission",
            severity: Severity::NeedsAttention,
            message: format!(
                "The daemon (running as {}) cannot write inside its own mirror directory. \
                 Some files under the mirror's `.git/` are owned by a different user, \
                 typically as a result of running a manual `sudo git` command in this mirror.",
                daemon_user
            ),
            suggested_fix: format!(
                "Restore ownership recursively: `sudo chown -R {0}:$(id -gn {0}) {1}` \
                 (no service restart needed — the next sync cycle will succeed).",
                daemon_user, mirror_str
            ),
        });
    }

    // -- SVN authentication failure.
    if text.contains("E170001")
        || text.contains("svn authentication failed")
        || text.contains("Can't get password")
        || text.contains("E215004")
    {
        return Some(OperatorHint {
            category: "svn_auth",
            severity: Severity::NeedsAttention,
            message: "SVN authentication is failing. The stored credentials are missing, \
                      have expired, or have been changed in the SVN server."
                .to_string(),
            suggested_fix: "Update the SVN credentials on the repository's Credentials panel \
                            in the web UI, or confirm that the SVN service account is still \
                            active and unlocked in the directory."
                .to_string(),
        });
    }

    // -- Git HTTPS authentication failure (push/pull against the
    //    destination provider, e.g. Gitea or GitHub).
    if text.contains("Authentication failed for 'https://")
        || text.contains("Authentication failed for 'http://")
        || text.contains("401 Unauthorized")
        || text.contains("remote: Invalid username or password")
    {
        return Some(OperatorHint {
            category: "git_auth",
            severity: Severity::NeedsAttention,
            message: "The git destination rejected the daemon's credentials. The Personal \
                      Access Token (PAT) used to push to the destination has expired, been \
                      revoked, or had its scope changed."
                .to_string(),
            suggested_fix: "Generate a fresh PAT in the destination provider (Gitea / GitHub) \
                            with the `write:repository` scope, then update it via the \
                            repository's Credentials panel in the web UI."
                .to_string(),
        });
    }

    // -- Push race (non-fast-forward) — transient and self-healing.
    if text.contains("non-fast-forward")
        || text.contains("fetch first")
        || text.contains("Updates were rejected because the remote contains work")
    {
        return Some(OperatorHint {
            category: "git_push_race",
            severity: Severity::Transient,
            message: "The git destination's branch advanced between the daemon's fetch and \
                      its push. This is a benign race and auto-resolves on the next sync \
                      cycle after the daemon pulls the remote changes."
                .to_string(),
            suggested_fix: "No action required.".to_string(),
        });
    }

    // -- Network unreachable / connection refused / DNS failure.
    if text.contains("Connection refused")
        || text.contains("Could not resolve host")
        || text.contains("Network is unreachable")
        || text.contains("Connection timed out")
        || text.contains("Couldn't connect to server")
    {
        return Some(OperatorHint {
            category: "network",
            severity: Severity::NeedsAttention,
            message: "The daemon cannot reach a required remote (SVN, the git destination, \
                      or LFS storage). This is a network or service-availability issue, not \
                      a reposync configuration problem."
                .to_string(),
            suggested_fix: "Verify the remote is reachable from this host (e.g. \
                            `curl`, `svn info`, or `git ls-remote` against the configured \
                            URL). If the remote is up, check firewalls and DNS."
                .to_string(),
        });
    }

    // -- Disk exhaustion.
    if text.contains("No space left on device")
        || text.contains("ENOSPC")
        || text.contains("disk full")
    {
        return Some(OperatorHint {
            category: "disk_full",
            severity: Severity::Critical,
            message: "The daemon cannot write because the host has no free disk space. The \
                      sync cycle cannot continue until space is freed."
                .to_string(),
            suggested_fix: "Free space on the volume holding `/opt/reposync` (check \
                            `df -h`). Prune old logs and mirrors of removed repositories. \
                            LFS objects can be reclaimed with `git lfs prune` per mirror."
                .to_string(),
        });
    }

    // -- Pre-receive hook rejection (often size limits, branch protection, etc.)
    if text.contains("pre-receive hook declined")
        || text.contains("exceeds GitHub's file size limit")
        || text.contains("File size exceeds")
    {
        return Some(OperatorHint {
            category: "destination_policy_rejected",
            severity: Severity::NeedsAttention,
            message: "The git destination rejected the push via its pre-receive policy. \
                      Common causes: a file is over the provider's size limit, or branch \
                      protection rules require a PR / a specific signer."
                .to_string(),
            suggested_fix: "Look at the rejection text in the error for the specific reason. \
                            For size limits, add the offending pattern to LFS tracking and \
                            re-sync. For branch protection, relax the rule for the daemon's \
                            user or push to a different branch and merge via PR."
                .to_string(),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_with_lock_classified() {
        let err = "sync Git error: git2 error: git fetch failed: error: cannot lock ref \
                   'refs/remotes/origin/main': Unable to create \
                   '/opt/reposync/repos/abc/git-repo/.git/refs/remotes/origin/main.lock': \
                   Permission denied";
        let hint = classify_sync_error(err, Some(Path::new("/opt/reposync/repos/abc/git-repo")))
            .expect("should classify");
        assert_eq!(hint.category, "filesystem_permission");
        assert_eq!(hint.severity, Severity::NeedsAttention);
        assert!(hint.suggested_fix.contains("chown"));
        assert!(hint.suggested_fix.contains("/opt/reposync/repos/abc/git-repo"));
    }

    #[test]
    fn nfwd_push_classified_transient() {
        let err = "git push rejected for branch 'main': non-fast-forward";
        let hint = classify_sync_error(err, None).expect("should classify");
        assert_eq!(hint.category, "git_push_race");
        assert_eq!(hint.severity, Severity::Transient);
    }

    #[test]
    fn svn_auth_classified() {
        let err = "svn: E170001: Authentication failed: Can't get password";
        let hint = classify_sync_error(err, None).expect("should classify");
        assert_eq!(hint.category, "svn_auth");
    }

    #[test]
    fn git_auth_classified() {
        let err = "fatal: Authentication failed for 'https://github.example.com/o/r.git/'";
        let hint = classify_sync_error(err, None).expect("should classify");
        assert_eq!(hint.category, "git_auth");
    }

    #[test]
    fn network_classified() {
        let err = "fatal: Could not resolve host: example.invalid";
        let hint = classify_sync_error(err, None).expect("should classify");
        assert_eq!(hint.category, "network");
    }

    #[test]
    fn disk_full_classified_critical() {
        let err = "fatal: write error: No space left on device";
        let hint = classify_sync_error(err, None).expect("should classify");
        assert_eq!(hint.category, "disk_full");
        assert_eq!(hint.severity, Severity::Critical);
    }

    #[test]
    fn unknown_returns_none() {
        let hint = classify_sync_error("some random error", None);
        assert!(hint.is_none());
    }

    #[test]
    fn render_details_is_multiline_and_contains_fix() {
        let hint = OperatorHint {
            category: "filesystem_permission",
            severity: Severity::NeedsAttention,
            message: "msg".into(),
            suggested_fix: "fix".into(),
        };
        let d = hint.render_details();
        assert!(d.contains("filesystem_permission"));
        assert!(d.contains("msg"));
        assert!(d.contains("Suggested fix: fix"));
    }
}
