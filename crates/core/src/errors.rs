//! Comprehensive error types for the RepoSync core library.
//!
//! Each subsystem has its own error type derived with `thiserror`, and a
//! top-level [`CoreError`] enum unifies them all for callers that want a
//! single error type.

use thiserror::Error;

// ---------------------------------------------------------------------------
// Top-level error
// ---------------------------------------------------------------------------

/// Unified error type for the entire core library.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Svn(#[from] SvnError),

    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    GitHub(#[from] GitHubError),

    #[error(transparent)]
    Sync(#[from] SyncError),

    #[error(transparent)]
    Conflict(#[from] ConflictError),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Database(#[from] DatabaseError),

    #[error(transparent)]
    Identity(#[from] IdentityError),

    #[error(transparent)]
    Notification(#[from] NotificationError),
}

// ---------------------------------------------------------------------------
// SVN errors
// ---------------------------------------------------------------------------

/// Errors from SVN CLI operations.
#[derive(Debug, Error)]
pub enum SvnError {
    /// The `svn` binary was not found on `$PATH`.
    #[error("svn binary not found: {0}")]
    BinaryNotFound(String),

    /// An `svn` command exited with a non-zero status.
    #[error("svn command failed (exit {exit_code}): {stderr}")]
    CommandFailed { exit_code: i32, stderr: String },

    /// Could not parse the XML output produced by `svn`.
    #[error("failed to parse svn XML output: {0}")]
    XmlParseError(String),

    /// An authentication problem with the SVN server.
    #[error("svn authentication failed for user '{username}': {detail}")]
    AuthenticationFailed { username: String, detail: String },

    /// The requested revision does not exist.
    #[error("svn revision {0} not found")]
    RevisionNotFound(i64),

    /// A checkout / working-copy operation failed.
    #[error("svn working copy error at '{path}': {detail}")]
    WorkingCopyError { path: String, detail: String },

    /// Network / connectivity issue.
    #[error("svn network error: {0}")]
    NetworkError(String),

    /// Generic I/O wrapper.
    #[error("svn I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

impl SvnError {
    /// Returns true if this error is permanent and should NOT be retried.
    /// Permanent errors include: authentication failures, access denied by
    /// server hooks, and missing binaries.
    pub fn is_permanent(&self) -> bool {
        match self {
            SvnError::AuthenticationFailed { .. } => true,
            SvnError::BinaryNotFound(_) => true,
            SvnError::CommandFailed { stderr, .. } => {
                stderr.contains("E195023")   // forbidden by pre-commit hook
                    || stderr.contains("E175013") // access denied
                    || stderr.contains("E170001") // authentication failed
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Git errors
// ---------------------------------------------------------------------------

/// Errors from local Git (git2) operations.
#[derive(Debug, Error)]
pub enum GitError {
    /// The repository path does not exist or is not a git repo.
    #[error("git repository not found at '{0}'")]
    RepositoryNotFound(String),

    /// A `git2` library error.
    #[error("git2 error: {0}")]
    Git2Error(#[from] git2::Error),

    /// A ref (branch, tag, SHA) could not be resolved.
    #[error("git ref not found: {0}")]
    RefNotFound(String),

    /// Push was rejected (e.g. non-fast-forward).
    #[error("git push rejected for branch '{branch}': {detail}")]
    PushRejected { branch: String, detail: String },

    /// Merge conflict detected during a local merge.
    #[error("git merge conflict: {0}")]
    MergeConflict(String),

    /// Failed to apply a diff / patch.
    #[error("git apply failed: {0}")]
    ApplyFailed(String),

    /// Generic I/O wrapper.
    #[error("git I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

impl GitError {
    /// Returns true if this error is permanent and should NOT be retried.
    pub fn is_permanent(&self) -> bool {
        match self {
            GitError::PushRejected { detail, .. } => {
                // Large file rejection or pre-receive hook rejection
                detail.contains("pre-receive hook declined")
                    || detail.contains("exceeds GitHub")
                    || detail.contains("GH001")
            }
            _ => false,
        }
    }
}

/// Sanitize sensitive tokens from error messages before displaying to users.
/// Removes GitHub tokens and x-access-token URLs.
pub fn sanitize_error_message(msg: &str) -> String {
    let mut result = msg.to_string();

    // Redact x-access-token:TOKEN@ patterns in URLs
    while let Some(start) = result.find("x-access-token:") {
        if let Some(at_pos) = result[start..].find('@') {
            let end = start + at_pos + 1;
            result.replace_range(start..end, "x-access-token:[REDACTED]@");
        } else {
            break;
        }
    }

    // Redact GitHub tokens (ghp_, gho_, github_pat_)
    for prefix in &["ghp_", "gho_", "github_pat_"] {
        while let Some(start) = result.find(prefix) {
            let token_end = result[start..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .map(|i| start + i)
                .unwrap_or(result.len());
            result.replace_range(start..token_end, "[REDACTED_TOKEN]");
        }
    }

    result
}

// ---------------------------------------------------------------------------
// GitHub API errors
// ---------------------------------------------------------------------------

/// Errors from GitHub REST API interactions.
#[derive(Debug, Error)]
pub enum GitHubError {
    /// HTTP-level transport error (network, TLS, etc.).
    #[error("GitHub HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    /// The API returned a non-success status code.
    #[error("GitHub API error (HTTP {status}): {body}")]
    ApiError { status: u16, body: String },

    /// Authentication token is missing or invalid.
    #[error("GitHub authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Rate limit exceeded.
    #[error("GitHub rate limit exceeded, resets at {reset_at}")]
    RateLimited { reset_at: String },

    /// Webhook signature verification failed.
    #[error("webhook signature verification failed")]
    WebhookSignatureInvalid,

    /// JSON deserialization failure.
    #[error("GitHub response parse error: {0}")]
    ParseError(String),
}

// ---------------------------------------------------------------------------
// Sync engine errors
// ---------------------------------------------------------------------------

/// Errors from the bidirectional synchronization engine.
#[derive(Debug, Error)]
pub enum SyncError {
    /// Another sync cycle is already running.
    #[error("sync already in progress (started at {started_at})")]
    AlreadyRunning { started_at: String },

    /// The sync detected an unresolvable conflict.
    #[error("unresolvable conflict on '{file_path}': {detail}")]
    UnresolvableConflict { file_path: String, detail: String },

    /// Echo detection failure — unable to determine if a commit is ours.
    #[error("echo detection failed for commit {sha}: {detail}")]
    EchoDetectionFailed { sha: String, detail: String },

    /// A state-machine transition was invalid.
    #[error("invalid sync state transition from {from} to {to}")]
    InvalidStateTransition { from: String, to: String },

    /// Underlying SVN error during sync.
    #[error("sync SVN error: {0}")]
    SvnError(#[from] SvnError),

    /// Underlying Git error during sync.
    #[error("sync Git error: {0}")]
    GitError(#[from] GitError),

    /// Underlying GitHub error during sync.
    #[error("sync GitHub error: {0}")]
    GitHubError(#[from] GitHubError),

    /// Database error during sync.
    #[error("sync database error: {0}")]
    DatabaseError(#[from] DatabaseError),

    /// Identity mapping error during sync.
    #[error("sync identity error: {0}")]
    IdentityError(#[from] IdentityError),
}

impl SyncError {
    /// Returns true if this error is permanent and should NOT be retried.
    pub fn is_permanent(&self) -> bool {
        match self {
            SyncError::SvnError(e) => e.is_permanent(),
            SyncError::GitError(e) => e.is_permanent(),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Conflict errors
// ---------------------------------------------------------------------------

/// Errors from the conflict detection / resolution subsystem.
#[derive(Debug, Error)]
pub enum ConflictError {
    /// The requested conflict ID was not found.
    #[error("conflict not found: {0}")]
    NotFound(String),

    /// Attempted to resolve a conflict that is already resolved.
    #[error("conflict {0} is already resolved")]
    AlreadyResolved(String),

    /// The provided resolution content is invalid.
    #[error("invalid resolution for conflict {id}: {detail}")]
    InvalidResolution { id: String, detail: String },

    /// Three-way merge failed.
    #[error("three-way merge failed: {0}")]
    MergeFailed(String),

    /// Database error when persisting conflict data.
    #[error("conflict database error: {0}")]
    DatabaseError(#[from] DatabaseError),
}

// ---------------------------------------------------------------------------
// Configuration errors
// ---------------------------------------------------------------------------

/// Errors from configuration loading and validation.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Config file not found.
    #[error("configuration file not found: {0}")]
    FileNotFound(String),

    /// TOML parse error.
    #[error("configuration parse error: {0}")]
    ParseError(String),

    /// A required environment variable is not set.
    #[error(
        "required environment variable '{var}' is not set (referenced by config field '{field}')"
    )]
    EnvVarMissing { var: String, field: String },

    /// A config value is invalid.
    #[error("invalid configuration value for '{field}': {detail}")]
    InvalidValue { field: String, detail: String },

    /// Generic I/O error reading the config file.
    #[error("configuration I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Database errors
// ---------------------------------------------------------------------------

/// Errors from the SQLite persistence layer.
#[derive(Debug, Error)]
pub enum DatabaseError {
    /// Underlying rusqlite error.
    #[error("database error: {0}")]
    SqliteError(#[from] rusqlite::Error),

    /// A migration failed.
    #[error("database migration failed (version {version}): {detail}")]
    MigrationFailed { version: u32, detail: String },

    /// A record was not found.
    #[error("{entity} not found: {id}")]
    NotFound { entity: String, id: String },

    /// Generic I/O error (e.g. file permissions).
    #[error("database I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Generic error with a descriptive message.
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Identity errors
// ---------------------------------------------------------------------------

/// Errors from the identity mapping subsystem.
#[derive(Debug, Error)]
pub enum IdentityError {
    /// The mapping file could not be loaded.
    #[error("identity mapping file error at '{path}': {detail}")]
    MappingFileError { path: String, detail: String },

    /// No mapping exists for the given SVN user.
    #[error("no git identity mapping for svn user '{0}'")]
    SvnUserNotFound(String),

    /// No mapping exists for the given Git identity.
    #[error("no svn user mapping for git identity '{name} <{email}>'")]
    GitIdentityNotFound { name: String, email: String },

    /// LDAP connection or query error.
    #[error("LDAP error: {0}")]
    LdapError(String),

    /// TOML parse error when reading the mapping file.
    #[error("identity mapping parse error: {0}")]
    ParseError(String),

    /// Generic I/O error.
    #[error("identity I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Notification errors
// ---------------------------------------------------------------------------

/// Errors from the notification subsystem (Slack, email).
#[derive(Debug, Error)]
pub enum NotificationError {
    /// Slack webhook delivery failed.
    #[error("Slack notification failed: {0}")]
    SlackError(String),

    /// Email delivery failed.
    #[error("email notification failed: {0}")]
    EmailError(String),

    /// HTTP error during notification delivery.
    #[error("notification HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    /// All notification channels failed.
    #[error("all notification channels failed: {0}")]
    AllChannelsFailed(String),
}

// ---------------------------------------------------------------------------
// Convenience conversions
// ---------------------------------------------------------------------------

// CoreError implements `std::error::Error` via `thiserror`, which means
// `anyhow::Error: From<CoreError>` is already provided by the blanket impl
// in `anyhow`. No manual `From` impl is needed.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_messages() {
        let err = SvnError::RevisionNotFound(42);
        assert_eq!(err.to_string(), "svn revision 42 not found");

        let err = GitError::RepositoryNotFound("/tmp/repo".into());
        assert_eq!(err.to_string(), "git repository not found at '/tmp/repo'");

        let err = GitHubError::RateLimited {
            reset_at: "2025-01-01T00:00:00Z".into(),
        };
        assert!(err.to_string().contains("rate limit"));

        let err = ConfigError::EnvVarMissing {
            var: "SVN_PASSWORD".into(),
            field: "svn.password_env".into(),
        };
        assert!(err.to_string().contains("SVN_PASSWORD"));
    }

    #[test]
    fn test_core_error_from_subsystem() {
        let svn_err = SvnError::RevisionNotFound(1);
        let core_err: CoreError = svn_err.into();
        assert!(matches!(core_err, CoreError::Svn(_)));

        let db_err = DatabaseError::NotFound {
            entity: "commit".into(),
            id: "abc".into(),
        };
        let core_err: CoreError = CoreError::Database(db_err);
        assert!(matches!(core_err, CoreError::Database(_)));
    }
}
