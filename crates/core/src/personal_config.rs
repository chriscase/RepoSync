//! Configuration for RepoSync Personal Branch Mode.
//!
//! A simplified, single-developer configuration that reuses the existing
//! `SvnConfig` and `GitHubConfig` patterns from the team-mode [`AppConfig`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::errors::ConfigError;

// ---------------------------------------------------------------------------
// Top-level personal config
// ---------------------------------------------------------------------------

/// Configuration for Personal Branch Mode.
///
/// This mirrors the structure of [`crate::config::AppConfig`] but is tailored
/// for a single developer running the sync daemon on their own machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalConfig {
    /// Personal-mode-specific settings.
    pub personal: PersonalSection,

    /// SVN repository connection settings (reuses team-mode pattern).
    pub svn: PersonalSvnConfig,

    /// GitHub repository and API settings.
    pub github: PersonalGitHubConfig,

    /// Developer identity for Git commits.
    pub developer: DeveloperConfig,

    /// Commit message formatting templates.
    #[serde(default)]
    pub commit_format: CommitFormatConfig,

    /// Sync behaviour options.
    #[serde(default)]
    pub options: PersonalOptionsConfig,

    /// Optional identity mapping for preserving original SVN authors.
    /// When set, `import --full` will map SVN usernames to Git identities
    /// instead of using the single developer identity for all commits.
    #[serde(default)]
    pub identity: Option<crate::config::IdentityConfig>,
}

// ---------------------------------------------------------------------------
// Personal section
// ---------------------------------------------------------------------------

/// Top-level personal mode settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalSection {
    /// Seconds between polling cycles (default 30).
    #[serde(default = "default_personal_poll_interval")]
    pub poll_interval_secs: u64,

    /// Minimum tracing level: trace, debug, info, warn, error.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Directory for persistent data (database, working copies).
    /// Defaults to platform-appropriate location.
    #[serde(default = "default_personal_data_dir")]
    pub data_dir: PathBuf,

    /// Optional HTTP status endpoint port (localhost only).
    #[serde(default)]
    pub status_port: Option<u16>,
}

fn default_personal_poll_interval() -> u64 {
    30
}

fn default_log_level() -> String {
    "info".into()
}

fn default_personal_data_dir() -> PathBuf {
    // Platform-appropriate default; overridden at runtime via dirs crate
    PathBuf::from("~/.local/share/reposync")
}

// ---------------------------------------------------------------------------
// SVN (personal)
// ---------------------------------------------------------------------------

/// SVN repository connection settings for personal mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalSvnConfig {
    /// SVN repository URL (typically the trunk URL).
    pub url: String,

    /// SVN username for authentication.
    pub username: String,

    /// Environment variable holding the SVN password.
    pub password_env: String,

    /// Resolved password (populated by `resolve_env_vars`).
    #[serde(skip)]
    pub password: Option<String>,
}

// ---------------------------------------------------------------------------
// GitHub (personal)
// ---------------------------------------------------------------------------

/// GitHub repository and API settings for personal mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalGitHubConfig {
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
    pub token_env: String,

    /// Default branch name (e.g. `main`).
    #[serde(default = "default_branch")]
    pub default_branch: String,

    /// Whether to auto-create the repo if it doesn't exist.
    #[serde(default = "default_true")]
    pub auto_create: bool,

    /// Whether the auto-created repo should be private.
    #[serde(default = "default_true")]
    pub private: bool,

    /// Resolved token (populated by `resolve_env_vars`).
    #[serde(skip)]
    pub token: Option<String>,
}

impl PersonalGitHubConfig {
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

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Developer identity
// ---------------------------------------------------------------------------

/// The single developer's identity for Git commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeveloperConfig {
    /// Git author/committer name.
    pub name: String,

    /// Git author/committer email.
    pub email: String,

    /// SVN username (used for echo suppression and author attribution).
    pub svn_username: String,
}

// ---------------------------------------------------------------------------
// Commit format templates
// ---------------------------------------------------------------------------

/// Templates for commit message formatting in each sync direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFormatConfig {
    /// Template for SVN→Git commit messages.
    /// Placeholders: `{original_message}`, `{svn_rev}`, `{svn_author}`, `{svn_date}`
    #[serde(default = "default_svn_to_git_template")]
    pub svn_to_git: String,

    /// Template for Git→SVN commit messages.
    /// Placeholders: `{original_message}`, `{git_sha}`, `{pr_number}`, `{pr_branch}`
    #[serde(default = "default_git_to_svn_template")]
    pub git_to_svn: String,
}

fn default_svn_to_git_template() -> String {
    r#"{original_message}

Synced-From: svn
SVN-Revision: r{svn_rev}
SVN-Author: {svn_author}
SVN-Date: {svn_date}
Sync-Marker: [reposync]"#
        .into()
}

fn default_git_to_svn_template() -> String {
    r#"{original_message}

[reposync] Synced from Git
Git-SHA: {git_sha}
PR-Number: #{pr_number}
PR-Branch: {pr_branch}"#
        .into()
}

impl Default for CommitFormatConfig {
    fn default() -> Self {
        Self {
            svn_to_git: default_svn_to_git_template(),
            git_to_svn: default_git_to_svn_template(),
        }
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Miscellaneous sync behaviour options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalOptionsConfig {
    /// SVN branch-relative prefixes selected by incremental Personal SVN→Git sync.
    #[serde(default)]
    pub allowed_paths: Vec<String>,

    /// Explicit opt-in to a fresh scoped full-history import and PR round trip.
    #[serde(default)]
    pub projected_history: bool,

    /// Block globs, evaluated in the original SVN path namespace.
    #[serde(default)]
    pub blocked_patterns: Vec<String>,

    /// At most one reversible rule, in the same shape as a Team rule.
    #[serde(default)]
    pub path_rewrite_rules: Vec<PersonalPathRewriteRule>,

    /// Explicit opt-in to a qualified stock Subversion checkout proof.
    #[serde(default)]
    pub network_directory_profile: Option<NetworkDirectoryProfile>,

    /// Explicit, independently provisioned CA PEM bundle for HTTPS proof.
    #[serde(default)]
    pub svn_tls_ca_file: Option<PathBuf>,

    /// Normalize CRLF to LF during sync.
    #[serde(default = "default_true")]
    pub normalize_line_endings: bool,

    /// Sync the executable bit from SVN `svn:executable` to Git.
    #[serde(default = "default_true")]
    pub sync_executable_bit: bool,

    /// Skip files larger than this (in bytes). 0 = no limit.
    #[serde(default)]
    pub max_file_size: u64,

    /// Glob patterns of files/directories to ignore during sync.
    #[serde(default)]
    pub ignore_patterns: Vec<String>,

    /// Whether to sync SVN externals (metadata only).
    #[serde(default)]
    pub sync_externals: bool,

    /// Whether to sync direct pushes to main (not just merged PRs).
    #[serde(default)]
    pub sync_direct_pushes: bool,

    /// Automatically merge conflicts when a clean 3-way merge is possible.
    #[serde(default = "default_true")]
    pub auto_merge: bool,

    /// Enable Git LFS tracking for files above this byte threshold.
    /// Files that exceed this threshold (but are within `max_file_size`) are
    /// stored via Git LFS instead of as regular blobs.
    /// 0 = LFS disabled (default).
    #[serde(default)]
    pub lfs_threshold: u64,

    /// Glob patterns for files that should always be LFS-tracked regardless
    /// of size. Example: `["*.psd", "*.bin", "*.iso"]`.
    #[serde(default)]
    pub lfs_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalPathRewriteRule {
    pub strip_prefix: String,
}

impl PersonalOptionsConfig {
    pub fn rewrite_rule(
        &self,
    ) -> Result<Option<crate::path_projection::TeamPrefixRewrite>, &'static str> {
        use crate::path_projection::TeamPrefixRewrite;
        match self.path_rewrite_rules.as_slice() {
            [] => Ok(None),
            [rule] => {
                let rewrite = TeamPrefixRewrite::new(&rule.strip_prefix)?;
                rewrite.validate_scope(&self.allowed_paths)?;
                Ok(Some(rewrite))
            }
            _ => Err("only one strip-prefix rule is supported"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkDirectoryProfile {
    ApacheSvnserveWc31,
    ApacheModDavSvnHttpsWc31,
}

impl NetworkDirectoryProfile {
    pub fn matches_scheme(self, scheme: &str) -> bool {
        matches!(
            (self, scheme),
            (Self::ApacheSvnserveWc31, "svn") | (Self::ApacheModDavSvnHttpsWc31, "https")
        )
    }

    pub fn revision_limit(self) -> i64 {
        match self {
            Self::ApacheSvnserveWc31 => 32,
            Self::ApacheModDavSvnHttpsWc31 => 8,
        }
    }
}

impl Default for PersonalOptionsConfig {
    fn default() -> Self {
        Self {
            allowed_paths: Vec::new(),
            projected_history: false,
            blocked_patterns: Vec::new(),
            path_rewrite_rules: Vec::new(),
            network_directory_profile: None,
            svn_tls_ca_file: None,
            normalize_line_endings: true,
            sync_executable_bit: true,
            max_file_size: 0,
            ignore_patterns: Vec::new(),
            sync_externals: false,
            sync_direct_pushes: false,
            auto_merge: true,
            lfs_threshold: 0,
            lfs_patterns: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading & resolving
// ---------------------------------------------------------------------------

impl PersonalConfig {
    /// Load a [`PersonalConfig`] from a TOML file.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        info!(path = %path.display(), "loading personal configuration");

        if !path.exists() {
            return Err(ConfigError::FileNotFound(path.display().to_string()));
        }

        let contents = std::fs::read_to_string(path)?;
        let config: PersonalConfig =
            toml::from_str(&contents).map_err(|e| ConfigError::ParseError(e.to_string()))?;

        debug!("personal configuration parsed successfully");
        Ok(config)
    }

    /// Resolve all `*_env` fields from environment variables.
    pub fn resolve_env_vars(&mut self) -> Result<(), ConfigError> {
        info!("resolving environment variable references in personal config");

        self.svn.password = resolve_optional_env(&self.svn.password_env, "svn.password_env");
        self.github.token = resolve_optional_env(&self.github.token_env, "github.token_env");

        debug!("personal config environment variable resolution complete");
        Ok(())
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
        if self.developer.name.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "developer.name".into(),
                detail: "Developer name must not be empty".into(),
            });
        }
        if self.developer.email.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "developer.email".into(),
                detail: "Developer email must not be empty".into(),
            });
        }
        if self.developer.svn_username.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "developer.svn_username".into(),
                detail: "Developer SVN username must not be empty".into(),
            });
        }
        if self.personal.poll_interval_secs == 0 {
            return Err(ConfigError::InvalidValue {
                field: "personal.poll_interval_secs".into(),
                detail: "poll interval must be > 0".into(),
            });
        }

        let profile = self.options.network_directory_profile;
        let ca = self.options.svn_tls_ca_file.as_deref();
        if !self.options.allowed_paths.is_empty() || profile.is_some() || ca.is_some() {
            let transport = crate::svn::operand::checked_url(&self.svn.url).map_err(|e| {
                ConfigError::InvalidValue {
                    field: "svn.url".into(),
                    detail: e.to_string(),
                }
            })?;
            let profile_matches = match profile {
                None => transport.scheme() == "file",
                Some(profile) => profile.matches_scheme(transport.scheme()),
            };
            let trust_matches = match profile {
                Some(NetworkDirectoryProfile::ApacheModDavSvnHttpsWc31) => {
                    ca.is_some_and(Path::is_absolute)
                }
                _ => ca.is_none(),
            };
            if self.options.allowed_paths.is_empty() || !profile_matches || !trust_matches {
                return Err(ConfigError::InvalidValue {
                    field: "options.network_directory_profile".into(),
                    detail: "scoped Personal sync requires file://, explicit apache_svnserve_wc31 for svn://, or explicit apache_mod_dav_svn_https_wc31 plus an absolute svn_tls_ca_file for https://; unsupported configuration held".into(),
                });
            }
        }
        for prefix in &self.options.allowed_paths {
            crate::svn::operand::validate_relative_path(prefix.trim_end_matches('/')).map_err(
                |e| ConfigError::InvalidValue {
                    field: "options.allowed_paths".into(),
                    detail: e.to_string(),
                },
            )?;
        }

        if self.options.projected_history {
            if self.options.allowed_paths.is_empty()
                || self.options.lfs_threshold != 0
                || !self.options.lfs_patterns.is_empty()
            {
                return Err(ConfigError::InvalidValue {
                    field: "options.projected_history".into(),
                    detail: "projected history requires an explicit allowed scope and does not yet support Git LFS".into(),
                });
            }
        } else if !self.options.blocked_patterns.is_empty()
            || !self.options.path_rewrite_rules.is_empty()
        {
            return Err(ConfigError::InvalidValue {
                field: "options.projected_history".into(),
                detail: "blocked paths or rewrite rules require an explicit fresh projected history opt-in".into(),
            });
        }
        self.options
            .rewrite_rule()
            .map_err(|e| ConfigError::InvalidValue {
                field: "options.path_rewrite_rules".into(),
                detail: e.into(),
            })?;

        // Fail-fast: sync_direct_pushes is not yet implemented in personal mode.
        if self.options.sync_direct_pushes {
            return Err(ConfigError::InvalidValue {
                field: "options.sync_direct_pushes".into(),
                detail: "sync_direct_pushes is not yet implemented in personal mode; \
                         only merged PRs are synced from Git to SVN. \
                         Set to false or remove it."
                    .into(),
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

    /// Generate a default TOML config template string.
    pub fn default_template() -> &'static str {
        r#"# RepoSync Personal Branch Mode Configuration
# See: docs/personal-branch/configuration.md

[personal]
poll_interval_secs = 30
log_level = "info"
# data_dir = "~/.local/share/reposync"  # auto-detected

[svn]
url = "https://svn.example.com/repos/project/trunk"
username = "your_svn_username"
password_env = "REPOSYNC_SVN_PASSWORD"

[github]
api_url = "https://api.github.com"
repo = "yourname/project-mirror"
token_env = "REPOSYNC_GITHUB_TOKEN"
default_branch = "main"
auto_create = true
private = true

[developer]
name = "Your Name"
email = "you@example.com"
svn_username = "your_svn_username"

[commit_format]
# svn_to_git = "..."  # uses sensible defaults
# git_to_svn = "..."  # uses sensible defaults

[options]
normalize_line_endings = true
sync_executable_bit = true
# max_file_size = 0         # 0 = no limit
# ignore_patterns = []
# allowed_paths = []        # branch-relative original SVN path prefixes
# projected_history = false # true requires a fresh projected full import
# blocked_patterns = []     # globs in SVN path coordinates
# path_rewrite_rules = [{ strip_prefix = "team/example/" }]
# network_directory_profile = "apache_svnserve_wc31"  # qualified stock svn://
# network_directory_profile = "apache_mod_dav_svn_https_wc31"  # qualified Apache DAV pilot
# svn_tls_ca_file = "/absolute/path/to/trusted-ca.pem"
# sync_externals = false
# sync_direct_pushes = false
auto_merge = true
"#
    }
}

/// Try to read an environment variable by name.
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

    fn sample_personal_toml() -> &'static str {
        r#"
[personal]
poll_interval_secs = 30
log_level = "debug"
data_dir = "/tmp/reposync-personal"

[svn]
url = "https://svn.example.com/repo/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
repo = "jdoe/project-mirror"
token_env = "GITHUB_TOKEN"
default_branch = "main"
auto_create = true
private = true

[developer]
name = "John Doe"
email = "jdoe@example.com"
svn_username = "jdoe"

[commit_format]
svn_to_git = "{original_message}\n\nSVN-Revision: r{svn_rev}"
git_to_svn = "{original_message}\n\n[reposync] Git-SHA: {git_sha}"

[options]
normalize_line_endings = true
sync_executable_bit = true
max_file_size = 10485760
ignore_patterns = ["*.tmp", "build/"]
"#
    }

    #[test]
    fn test_parse_full_personal_config() {
        let config: PersonalConfig =
            toml::from_str(sample_personal_toml()).expect("failed to parse toml");
        assert_eq!(config.personal.poll_interval_secs, 30);
        assert_eq!(config.svn.url, "https://svn.example.com/repo/trunk");
        assert_eq!(config.github.repo, "jdoe/project-mirror");
        assert!(config.github.auto_create);
        assert!(config.github.private);
        assert_eq!(config.developer.name, "John Doe");
        assert_eq!(config.developer.svn_username, "jdoe");
        assert_eq!(config.options.max_file_size, 10_485_760);
        assert_eq!(config.options.ignore_patterns.len(), 2);
    }

    #[test]
    fn test_load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal.toml");
        std::fs::write(&path, sample_personal_toml()).unwrap();

        let config = PersonalConfig::load_from_file(&path).expect("load failed");
        assert_eq!(config.personal.log_level, "debug");
    }

    #[test]
    fn test_file_not_found() {
        let result = PersonalConfig::load_from_file("/nonexistent/personal.toml");
        assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
    }

    #[test]
    fn test_validate_rejects_empty_url() {
        let mut config: PersonalConfig = toml::from_str(sample_personal_toml()).unwrap();
        config.svn.url = String::new();
        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigError::InvalidValue { ref field, .. }) if field == "svn.url"
        ));
    }

    #[test]
    fn test_validate_rejects_bad_repo_format() {
        let mut config: PersonalConfig = toml::from_str(sample_personal_toml()).unwrap();
        config.github.repo = "noslash".into();
        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigError::InvalidValue { ref field, .. }) if field == "github.repo"
        ));
    }

    #[test]
    fn test_validate_rejects_empty_developer() {
        let mut config: PersonalConfig = toml::from_str(sample_personal_toml()).unwrap();
        config.developer.name = String::new();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_poll() {
        let mut config: PersonalConfig = toml::from_str(sample_personal_toml()).unwrap();
        config.personal.poll_interval_secs = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_sync_direct_pushes() {
        let mut config: PersonalConfig = toml::from_str(sample_personal_toml()).unwrap();
        config.options.sync_direct_pushes = true;
        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigError::InvalidValue { ref field, .. }) if field == "options.sync_direct_pushes"
        ));
    }

    #[test]
    fn test_defaults() {
        let minimal = r#"
[personal]
[svn]
url = "https://svn.example.com/repo/trunk"
username = "user"
password_env = "SVN_PW"
[github]
repo = "user/repo"
token_env = "GH_TOKEN"
[developer]
name = "User"
email = "user@example.com"
svn_username = "user"
"#;
        let config: PersonalConfig = toml::from_str(minimal).unwrap();
        assert_eq!(config.personal.poll_interval_secs, 30);
        assert_eq!(config.personal.log_level, "info");
        assert_eq!(config.github.default_branch, "main");
        assert!(config.github.auto_create);
        assert!(config.options.normalize_line_endings);
        assert!(config.options.auto_merge);
        assert!(!config.options.sync_direct_pushes);
    }

    #[test]
    fn test_resolve_env_vars() {
        std::env::set_var("TEST_PERSONAL_SVN_PW", "s3cret");
        std::env::set_var("TEST_PERSONAL_GH_TOKEN", "ghp_abc");

        let toml_str = r#"
[personal]
[svn]
url = "https://svn.example.com/repo/trunk"
username = "user"
password_env = "TEST_PERSONAL_SVN_PW"
[github]
repo = "user/repo"
token_env = "TEST_PERSONAL_GH_TOKEN"
[developer]
name = "User"
email = "user@example.com"
svn_username = "user"
"#;
        let mut config: PersonalConfig = toml::from_str(toml_str).unwrap();
        config.resolve_env_vars().unwrap();

        assert_eq!(config.svn.password.as_deref(), Some("s3cret"));
        assert_eq!(config.github.token.as_deref(), Some("ghp_abc"));

        std::env::remove_var("TEST_PERSONAL_SVN_PW");
        std::env::remove_var("TEST_PERSONAL_GH_TOKEN");
    }

    #[test]
    fn test_default_template_is_valid() {
        let _config: PersonalConfig = toml::from_str(PersonalConfig::default_template())
            .expect("default template should be valid TOML");
    }
}
