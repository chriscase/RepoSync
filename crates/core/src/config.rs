//! TOML-based configuration system for RepoSync.
//!
//! All sensitive values (passwords, tokens, secrets) are stored as `_env`
//! fields that reference environment variable names. The actual secrets are
//! resolved at runtime via [`AppConfig::resolve_env_vars`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::db::Database;
use crate::errors::ConfigError;

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Top-level application configuration loaded from a TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Daemon / polling settings.
    pub daemon: DaemonConfig,

    /// SVN repository settings.
    pub svn: SvnConfig,

    /// GitHub repository and API settings.
    pub github: GitHubConfig,

    /// Identity mapping settings.
    #[serde(default)]
    pub identity: IdentityConfig,

    /// Web dashboard settings.
    #[serde(default)]
    pub web: WebConfig,

    /// Notification settings (Slack, email).
    #[serde(default)]
    pub notifications: NotificationConfig,

    /// Sync behaviour settings.
    #[serde(default)]
    pub sync: SyncConfig,

    /// Resolved secrets cache (not serialized).
    #[serde(skip)]
    pub resolved_secrets: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Daemon
// ---------------------------------------------------------------------------

/// Daemon / polling configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Seconds between polling cycles (default 60).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,

    /// Minimum tracing level: trace, debug, info, warn, error.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Directory for persistent data (database, working copies).
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Maximum daemon RSS in megabytes. If exceeded, sync cycles are skipped
    /// until memory drops below the limit. 0 = disabled. Default 512.
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,
}

fn default_poll_interval() -> u64 {
    60
}
fn default_log_level() -> String {
    "info".into()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/reposync")
}
fn default_memory_limit_mb() -> u64 {
    512
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            log_level: default_log_level(),
            data_dir: default_data_dir(),
            memory_limit_mb: default_memory_limit_mb(),
        }
    }
}

// ---------------------------------------------------------------------------
// SVN
// ---------------------------------------------------------------------------

/// SVN repository layout type.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SvnLayout {
    /// Standard trunk/branches/tags layout.
    #[default]
    Standard,
    /// Custom paths specified explicitly.
    Custom,
}

/// SVN repository connection and layout settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvnConfig {
    /// SVN repository root URL (e.g. `https://svn.example.com/repo`).
    pub url: String,

    /// SVN username for authentication.
    pub username: String,

    /// Environment variable holding the SVN password.
    #[serde(default)]
    pub password_env: String,

    /// Repository layout.
    #[serde(default)]
    pub layout: SvnLayout,

    /// Path to trunk (relative to repo root). Default `trunk`.
    #[serde(default = "default_trunk")]
    pub trunk_path: String,

    /// Path to branches (relative to repo root). Default `branches`.
    #[serde(default = "default_branches")]
    pub branches_path: String,

    /// Path to tags (relative to repo root). Default `tags`.
    #[serde(default = "default_tags")]
    pub tags_path: String,

    /// Environment variable holding a shared secret for SVN webhook authentication.
    #[serde(default)]
    pub webhook_secret_env: Option<String>,

    /// Resolved password (populated by `resolve_env_vars`).
    #[serde(skip)]
    pub password: Option<String>,

    /// Resolved SVN webhook secret.
    #[serde(skip)]
    pub webhook_secret: Option<String>,
}

fn default_trunk() -> String {
    "trunk".into()
}
fn default_branches() -> String {
    "branches".into()
}
fn default_tags() -> String {
    "tags".into()
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

/// Git hosting provider type.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GitProvider {
    #[default]
    GitHub,
    Gitea,
}

/// GitHub repository and API configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// GitHub API base URL (default `https://api.github.com`).
    #[serde(default = "default_github_api_url")]
    pub api_url: String,

    /// Explicit Git clone/push base URL.  When set, this overrides the
    /// automatic derivation from `api_url`.  Useful for non-standard
    /// enterprise setups where the Git host differs from the API host.
    /// Example: `https://github.company.com`
    #[serde(default)]
    pub git_base_url: Option<String>,

    /// Repository in `owner/repo` format.
    pub repo: String,

    /// Environment variable holding the GitHub personal access token.
    #[serde(default)]
    pub token_env: String,

    /// Environment variable holding the webhook secret.
    #[serde(default)]
    pub webhook_secret_env: Option<String>,

    /// Default branch name (e.g. `main`).
    #[serde(default = "default_branch")]
    pub default_branch: String,

    /// Git hosting provider (github or gitea). Default: github.
    #[serde(default)]
    pub provider: GitProvider,

    /// Resolved token (populated by `resolve_env_vars`).
    #[serde(skip)]
    pub token: Option<String>,

    /// Resolved webhook secret.
    #[serde(skip)]
    pub webhook_secret: Option<String>,
}

impl GitHubConfig {
    /// Derive the full HTTPS clone URL for the configured repository.
    ///
    /// Uses `git_base_url` if set, otherwise derives from `api_url`.
    /// See [`crate::git::remote_url::derive_git_remote_url`] for details.
    pub fn clone_url(&self) -> String {
        crate::git::remote_url::derive_git_remote_url(
            &self.api_url,
            self.git_base_url.as_deref(),
            &self.repo,
        )
    }
}

fn default_github_api_url() -> String {
    "https://api.github.com".into()
}
fn default_branch() -> String {
    "main".into()
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Identity mapping configuration for translating SVN usernames to/from Git
/// author information.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdentityConfig {
    /// Path to the TOML identity mapping file.
    #[serde(default)]
    pub mapping_file: Option<PathBuf>,

    /// Default email domain used when constructing emails from SVN usernames
    /// (e.g. `example.com` produces `jdoe@example.com`).
    #[serde(default)]
    pub email_domain: Option<String>,

    /// Optional LDAP server URL for on-the-fly lookups.
    #[serde(default)]
    pub ldap_url: Option<String>,

    /// LDAP search base DN.
    #[serde(default)]
    pub ldap_base_dn: Option<String>,

    /// LDAP bind DN for authenticated queries.
    #[serde(default)]
    pub ldap_bind_dn: Option<String>,

    /// Environment variable holding the LDAP bind password.
    #[serde(default)]
    pub ldap_bind_password_env: Option<String>,

    /// Resolved LDAP bind password.
    #[serde(skip)]
    pub ldap_bind_password: Option<String>,
}

// ---------------------------------------------------------------------------
// Web dashboard
// ---------------------------------------------------------------------------

/// Authentication mode for the web dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Simple password authentication.
    #[default]
    Simple,
    /// GitHub OAuth.
    #[serde(rename = "github_oauth")]
    GitHubOAuth,
    /// Both simple and GitHub OAuth accepted.
    Both,
}

/// Web dashboard configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// Listen address (default `127.0.0.1:3000`).
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Authentication mode.
    #[serde(default)]
    pub auth_mode: AuthMode,

    /// Environment variable holding the admin password (for simple auth).
    #[serde(default)]
    pub admin_password_env: Option<String>,

    /// GitHub OAuth client ID.
    #[serde(default)]
    pub oauth_client_id: Option<String>,

    /// Environment variable holding the GitHub OAuth client secret.
    #[serde(default)]
    pub oauth_client_secret_env: Option<String>,

    /// Allowed GitHub usernames / org membership for OAuth access.
    #[serde(default)]
    pub oauth_allowed_users: Vec<String>,

    /// Resolved admin password.
    #[serde(skip)]
    pub admin_password: Option<String>,

    /// Resolved OAuth client secret.
    #[serde(skip)]
    pub oauth_client_secret: Option<String>,

    /// Allowed CORS origins. If empty, derives from the listen address.
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

fn default_listen() -> String {
    "127.0.0.1:3000".into()
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            auth_mode: AuthMode::default(),
            admin_password_env: None,
            oauth_client_id: None,
            oauth_client_secret_env: None,
            oauth_allowed_users: Vec::new(),
            admin_password: None,
            oauth_client_secret: None,
            cors_origins: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Notification channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationConfig {
    /// Environment variable holding the Slack incoming-webhook URL.
    #[serde(default)]
    pub slack_webhook_url_env: Option<String>,

    /// SMTP server address for email notifications (e.g. `smtp.example.com:587`).
    #[serde(default)]
    pub email_smtp: Option<String>,

    /// Sender email address.
    #[serde(default)]
    pub email_from: Option<String>,

    /// Recipient email addresses.
    #[serde(default)]
    pub email_recipients: Vec<String>,

    /// Resolved Slack webhook URL.
    #[serde(skip)]
    pub slack_webhook_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Sync behaviour
// ---------------------------------------------------------------------------

/// How commits are pushed to Git.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// Push directly to the target branch.
    #[default]
    Direct,
    /// Create a pull request for each sync batch.
    Pr,
}

/// Sub-configuration for PR-based sync mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrConfig {
    /// Title prefix for auto-generated PRs.
    #[serde(default = "default_pr_prefix")]
    pub title_prefix: String,

    /// Labels to apply to the PR.
    #[serde(default)]
    pub labels: Vec<String>,

    /// GitHub usernames to request review from.
    #[serde(default)]
    pub reviewers: Vec<String>,

    /// Automatically merge the PR if CI passes.
    #[serde(default)]
    pub auto_merge: bool,
}

fn default_pr_prefix() -> String {
    "[svn-sync]".into()
}

/// Sync behaviour configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Mode: direct push or pull request.
    #[serde(default)]
    pub mode: SyncMode,

    /// Automatically merge conflicts when a clean 3-way merge is possible.
    #[serde(default = "default_true")]
    pub auto_merge: bool,

    /// Branch names to synchronize (empty = all).
    #[serde(default)]
    pub sync_branches: Vec<String>,

    /// Whether to sync tags.
    #[serde(default = "default_true")]
    pub sync_tags: bool,

    /// PR-specific settings (used when `mode` is `Pr`).
    #[serde(default)]
    pub pr: PrConfig,

    /// Maximum file size in bytes (0 = no limit). Files larger are skipped.
    #[serde(default)]
    pub max_file_size: u64,

    /// LFS threshold in bytes (0 = disabled). Files larger are LFS-tracked.
    #[serde(default)]
    pub lfs_threshold: u64,

    /// File patterns that should always be LFS-tracked (e.g. `*.psd`).
    #[serde(default)]
    pub lfs_patterns: Vec<String>,

    /// Glob patterns for files to ignore during sync.
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            mode: SyncMode::default(),
            auto_merge: true,
            sync_branches: Vec::new(),
            sync_tags: true,
            pr: PrConfig::default(),
            max_file_size: 0,
            lfs_threshold: 0,
            lfs_patterns: Vec::new(),
            ignore_patterns: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading & resolving
// ---------------------------------------------------------------------------

impl AppConfig {
    /// Load an [`AppConfig`] from a TOML file at the given path.
    ///
    /// This does **not** resolve environment variables -- call
    /// [`resolve_env_vars`](Self::resolve_env_vars) afterwards.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        info!(path = %path.display(), "loading configuration");

        if !path.exists() {
            return Err(ConfigError::FileNotFound(path.display().to_string()));
        }

        let contents = std::fs::read_to_string(path)?;
        let config: AppConfig =
            toml::from_str(&contents).map_err(|e| ConfigError::ParseError(e.to_string()))?;

        debug!("configuration parsed successfully");
        Ok(config)
    }

    /// Resolve all `*_env` fields from environment variables and populate the
    /// corresponding resolved fields.
    ///
    /// Fields that reference a missing variable will log a warning but will
    /// **not** fail -- callers can check the `Option` fields and decide what
    /// is required for their execution mode.
    pub fn resolve_env_vars(&mut self) -> Result<(), ConfigError> {
        info!("resolving environment variable references in config");

        // SVN password
        self.svn.password = resolve_optional_env(&self.svn.password_env, "svn.password_env");

        // GitHub token (required for most operations)
        self.github.token = resolve_optional_env(&self.github.token_env, "github.token_env");

        // GitHub webhook secret
        if let Some(ref env_name) = self.github.webhook_secret_env {
            self.github.webhook_secret =
                resolve_optional_env(env_name, "github.webhook_secret_env");
        }

        // SVN webhook secret
        if let Some(ref env_name) = self.svn.webhook_secret_env {
            self.svn.webhook_secret = resolve_optional_env(env_name, "svn.webhook_secret_env");
        }

        // Web admin password
        if let Some(ref env_name) = self.web.admin_password_env {
            self.web.admin_password = resolve_optional_env(env_name, "web.admin_password_env");
        }

        // Web OAuth client secret
        if let Some(ref env_name) = self.web.oauth_client_secret_env {
            self.web.oauth_client_secret =
                resolve_optional_env(env_name, "web.oauth_client_secret_env");
        }

        // LDAP bind password
        if let Some(ref env_name) = self.identity.ldap_bind_password_env {
            self.identity.ldap_bind_password =
                resolve_optional_env(env_name, "identity.ldap_bind_password_env");
        }

        // Slack webhook URL
        if let Some(ref env_name) = self.notifications.slack_webhook_url_env {
            self.notifications.slack_webhook_url =
                resolve_optional_env(env_name, "notifications.slack_webhook_url_env");
        }

        debug!("environment variable resolution complete");
        Ok(())
    }

    /// Fall back to secrets stored in the database when env vars are absent.
    ///
    /// The wizard saves passwords / tokens into the `kv_state` table.  This
    /// method reads them back and fills any resolved field that is still
    /// `None` after [`resolve_env_vars`](Self::resolve_env_vars).  Env-var
    /// values therefore always take priority (they are set first).
    pub fn resolve_secrets_from_db(&mut self, db: &Database) {
        // Helper: try encrypted_secrets first, fall back to legacy kv_state plaintext
        let decrypt_secret = |db: &Database, key: &str, legacy_key: &str| -> Option<String> {
            // Try encrypted secrets table first
            if let Ok(Some((ct, nonce))) = db.get_encrypted_secret(key) {
                if let Ok(enc_key) = crate::crypto::get_or_create_encryption_key(db) {
                    if let Ok(plaintext) = crate::crypto::decrypt_credential(&ct, &nonce, &enc_key) {
                        return Some(plaintext);
                    }
                }
            }
            // Fall back to legacy plaintext in kv_state
            if let Ok(Some(val)) = db.get_state(legacy_key) {
                if !val.is_empty() {
                    // Auto-migrate: encrypt and store in encrypted_secrets
                    if let Ok(enc_key) = crate::crypto::get_or_create_encryption_key(db) {
                        if let Ok((ct, nonce)) = crate::crypto::encrypt_credential(&val, &enc_key) {
                            let _ = db.store_encrypted_secret(key, &ct, &nonce);
                            let _ = db.set_state(legacy_key, "");
                            info!("Migrated {} from plaintext to encrypted storage", key);
                        }
                    }
                    return Some(val);
                }
            }
            None
        };

        // SVN password
        if self.svn.password.is_some() {
            debug!("SVN password already set from env var, skipping DB lookup");
        } else if let Some(val) = decrypt_secret(db, "svn_password", "secret_svn_password") {
            self.svn.password = Some(val);
            info!("Loaded SVN password from database (encrypted)");
        } else {
            debug!("No SVN password found in database");
        }

        // Git token
        if self.github.token.is_some() {
            debug!("Git token already set from env var, skipping DB lookup");
        } else if let Some(val) = decrypt_secret(db, "git_token", "secret_git_token") {
            self.github.token = Some(val);
            info!("Loaded Git token from database (encrypted)");
        } else {
            debug!("No Git token found in database");
        }

        // Admin password (stored as bcrypt hash, not encrypted)
        if self.web.admin_password.is_some() {
            debug!("Admin password already set from env var, skipping DB lookup");
        } else {
            // Try bcrypt hash first, then legacy plaintext
            match db.get_state("secret_admin_password_hash") {
                Ok(Some(val)) if !val.is_empty() => {
                    self.web.admin_password = Some(val);
                    info!("Loaded admin password hash from database");
                }
                _ => match db.get_state("secret_admin_password") {
                    Ok(Some(val)) if !val.is_empty() => {
                        // Auto-migrate plaintext to bcrypt hash
                        if let Ok(hash) = crate::crypto::hash_password(&val) {
                            let _ = db.set_state("secret_admin_password_hash", &hash);
                            let _ = db.set_state("secret_admin_password", "");
                            self.web.admin_password = Some(hash);
                            info!("Migrated admin password from plaintext to bcrypt hash");
                        } else {
                            self.web.admin_password = Some(val);
                        }
                    }
                    Ok(_) => debug!("No admin password found in database"),
                    Err(e) => warn!("Failed to read admin password from database: {}", e),
                },
            }
        }
    }

    /// Validate that all required fields are present and sane.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.svn.url.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "svn.url".into(),
                detail: "SVN URL must not be empty".into(),
            });
        }
        if self.svn.username.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "svn.username".into(),
                detail: "SVN username must not be empty".into(),
            });
        }
        if self.github.repo.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "github.repo".into(),
                detail: "GitHub repo must not be empty".into(),
            });
        }
        if !self.github.repo.contains('/') {
            return Err(ConfigError::InvalidValue {
                field: "github.repo".into(),
                detail: "GitHub repo must be in 'owner/repo' format".into(),
            });
        }
        if self.daemon.poll_interval_secs == 0 {
            return Err(ConfigError::InvalidValue {
                field: "daemon.poll_interval_secs".into(),
                detail: "poll interval must be > 0".into(),
            });
        }

        // Validate log_level against known tracing levels.
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.daemon.log_level.to_lowercase().as_str()) {
            return Err(ConfigError::InvalidValue {
                field: "daemon.log_level".into(),
                detail: format!(
                    "invalid log level '{}', must be one of: {}",
                    self.daemon.log_level,
                    valid_levels.join(", ")
                ),
            });
        }

        Ok(())
    }

    /// Convenience: load, resolve, and validate in one call.
    pub fn load_and_resolve<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let mut config = Self::load_from_file(path)?;
        config.resolve_env_vars()?;
        config.validate()?;
        Ok(config)
    }
}

/// Try to read an environment variable by name. Returns `Some(value)` on
/// success; logs a warning and returns `None` if the variable is unset.
fn resolve_optional_env(env_name: &str, field: &str) -> Option<String> {
    match std::env::var(env_name) {
        Ok(val) if !val.is_empty() => {
            debug!(field, env_name, "resolved env var");
            Some(val)
        }
        Ok(_) => {
            warn!(field, env_name, "env var is set but empty");
            None
        }
        Err(_) => {
            warn!(field, env_name, "env var not set");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_toml() -> &'static str {
        r#"
[daemon]
poll_interval_secs = 30
log_level = "debug"
data_dir = "/tmp/reposync"

[svn]
url = "https://svn.example.com/repo"
username = "svnuser"
password_env = "SVN_PASSWORD"
layout = "standard"
trunk_path = "trunk"
branches_path = "branches"
tags_path = "tags"

[github]
api_url = "https://api.github.com"
repo = "acme/myrepo"
token_env = "GITHUB_TOKEN"
webhook_secret_env = "GITHUB_WEBHOOK_SECRET"
default_branch = "main"

[identity]
mapping_file = "/etc/reposync/authors.toml"
email_domain = "example.com"

[web]
listen = "0.0.0.0:8080"
auth_mode = "both"
admin_password_env = "ADMIN_PASSWORD"
oauth_client_id = "abc123"
oauth_client_secret_env = "OAUTH_SECRET"
oauth_allowed_users = ["alice", "bob"]

[notifications]
slack_webhook_url_env = "SLACK_URL"
email_smtp = "smtp.example.com:587"
email_from = "sync@example.com"
email_recipients = ["admin@example.com"]

[sync]
mode = "pr"
auto_merge = true
sync_branches = ["develop", "release/*"]
sync_tags = true

[sync.pr]
title_prefix = "[svn-sync]"
labels = ["automated"]
reviewers = ["charlie"]
auto_merge = false
"#
    }

    #[test]
    fn test_parse_full_config() {
        let config: AppConfig = toml::from_str(sample_toml()).expect("failed to parse toml");
        assert_eq!(config.daemon.poll_interval_secs, 30);
        assert_eq!(config.svn.url, "https://svn.example.com/repo");
        assert_eq!(config.github.repo, "acme/myrepo");
        assert_eq!(config.web.auth_mode, AuthMode::Both);
        assert_eq!(config.sync.mode, SyncMode::Pr);
        assert_eq!(config.sync.pr.reviewers, vec!["charlie"]);
    }

    #[test]
    fn test_load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(sample_toml().as_bytes()).unwrap();

        let config = AppConfig::load_from_file(&path).expect("load_from_file failed");
        assert_eq!(config.daemon.log_level, "debug");
    }

    #[test]
    fn test_file_not_found() {
        let result = AppConfig::load_from_file("/nonexistent/config.toml");
        assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
    }

    #[test]
    fn test_validate_rejects_empty_url() {
        let mut config: AppConfig = toml::from_str(sample_toml()).unwrap();
        config.svn.url = String::new();
        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigError::InvalidValue { ref field, .. }) if field == "svn.url"
        ));
    }

    #[test]
    fn test_validate_rejects_bad_repo_format() {
        let mut config: AppConfig = toml::from_str(sample_toml()).unwrap();
        config.github.repo = "noslash".into();
        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigError::InvalidValue { ref field, .. }) if field == "github.repo"
        ));
    }

    #[test]
    fn test_resolve_env_vars() {
        std::env::set_var("TEST_SVN_PW", "s3cret");
        std::env::set_var("TEST_GH_TOKEN", "ghp_abc");

        let toml_str = r#"
[daemon]
[svn]
url = "https://svn.example.com/repo"
username = "user"
password_env = "TEST_SVN_PW"
[github]
repo = "acme/repo"
token_env = "TEST_GH_TOKEN"
"#;
        let mut config: AppConfig = toml::from_str(toml_str).unwrap();
        config.resolve_env_vars().unwrap();

        assert_eq!(config.svn.password.as_deref(), Some("s3cret"));
        assert_eq!(config.github.token.as_deref(), Some("ghp_abc"));

        // Clean up
        std::env::remove_var("TEST_SVN_PW");
        std::env::remove_var("TEST_GH_TOKEN");
    }

    #[test]
    fn test_defaults() {
        let minimal = r#"
[daemon]
[svn]
url = "https://svn.example.com/repo"
username = "user"
password_env = "SVN_PW"
[github]
repo = "acme/repo"
token_env = "GH_TOKEN"
"#;
        let config: AppConfig = toml::from_str(minimal).unwrap();
        assert_eq!(config.daemon.poll_interval_secs, 60);
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.svn.trunk_path, "trunk");
        assert_eq!(config.github.default_branch, "main");
        assert_eq!(config.web.listen, "127.0.0.1:3000");
        assert_eq!(config.sync.mode, SyncMode::Direct);
        assert!(config.sync.auto_merge);
    }
}
