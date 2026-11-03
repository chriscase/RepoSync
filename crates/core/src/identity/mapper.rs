//! Core identity mapping logic.
//!
//! [`IdentityMapper`] coordinates lookups across the mapping file, LDAP, and
//! fallback strategies to translate between SVN usernames and Git identities.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::ldap::LdapResolver;
use super::mapping_file::{AuthorEntry, MappingFile};
use crate::config::IdentityConfig;
use crate::errors::IdentityError;

/// A Git author/committer identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitIdentity {
    /// Display name.
    pub name: String,
    /// Email address.
    pub email: String,
}

/// Bidirectional identity mapper for SVN username <-> Git identity.
///
/// Thread-safe: the internal mapping cache is wrapped in an `RwLock` so
/// multiple sync tasks can read concurrently and `reload()` can update it.
pub struct IdentityMapper {
    /// Cached SVN username -> Git identity mapping.
    cache: Arc<RwLock<HashMap<String, AuthorEntry>>>,
    /// Reverse cache: email -> SVN username.
    reverse_cache: Arc<RwLock<HashMap<String, String>>>,
    /// Path to the TOML mapping file (if any).
    mapping_file_path: Option<PathBuf>,
    /// Default email domain for fallback mapping.
    email_domain: Option<String>,
    /// LDAP resolver (if configured).
    ldap: Option<Arc<RwLock<LdapResolver>>>,
}

impl IdentityMapper {
    /// Create a new `IdentityMapper` from an [`IdentityConfig`].
    ///
    /// If a mapping file is specified, it is loaded immediately. LDAP is
    /// initialized if URL / base DN / bind DN are all provided.
    pub fn new(config: &IdentityConfig) -> Result<Self, IdentityError> {
        info!("initializing identity mapper");

        // Load mapping file if specified.
        let entries = match &config.mapping_file {
            Some(path) if path.exists() => {
                info!(path = %path.display(), "loading identity mapping file");
                MappingFile::load(path)?
            }
            Some(path) => {
                warn!(path = %path.display(), "mapping file not found, starting with empty map");
                HashMap::new()
            }
            None => {
                debug!("no mapping file configured");
                HashMap::new()
            }
        };

        // Build reverse cache.
        let reverse = build_reverse_cache(&entries);

        // Initialize LDAP if configured.
        let ldap = match (&config.ldap_url, &config.ldap_base_dn, &config.ldap_bind_dn) {
            (Some(url), Some(base_dn), Some(bind_dn)) => {
                let password = config.ldap_bind_password.as_deref().unwrap_or("");
                Some(Arc::new(RwLock::new(LdapResolver::new(
                    url.clone(),
                    base_dn.clone(),
                    bind_dn.clone(),
                    password.to_string(),
                ))))
            }
            _ => {
                debug!("LDAP not configured");
                None
            }
        };

        Ok(Self {
            cache: Arc::new(RwLock::new(entries)),
            reverse_cache: Arc::new(RwLock::new(reverse)),
            mapping_file_path: config.mapping_file.clone(),
            email_domain: config.email_domain.clone(),
            ldap,
        })
    }

    /// Map an SVN username to a Git identity.
    ///
    /// Lookup order:
    /// 1. In-memory cache (loaded from mapping file)
    /// 2. LDAP (if configured)
    /// 3. Fallback: derive from username + email_domain
    pub fn svn_to_git(&self, svn_username: &str) -> Result<GitIdentity, IdentityError> {
        // 1. Check mapping file cache.
        {
            let cache = self
                .cache
                .read()
                .map_err(|_| IdentityError::LdapError("cache lock poisoned".into()))?;
            if let Some(entry) = cache.get(svn_username) {
                debug!(svn_username, "found in mapping file cache");
                return Ok(GitIdentity {
                    name: entry.name.clone(),
                    email: entry.email.clone(),
                });
            }
        }

        // 2. Try LDAP.
        if let Some(ref ldap) = self.ldap {
            let mut resolver = ldap
                .write()
                .map_err(|_| IdentityError::LdapError("LDAP lock poisoned".into()))?;
            let ldap_result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(resolver.lookup_by_username(svn_username))
            })?;
            if let Some(identity) = ldap_result {
                debug!(svn_username, "found via LDAP");
                // Cache the LDAP result for future lookups.
                drop(resolver);
                self.add_to_cache(svn_username, &identity);
                return Ok(identity);
            }
        }

        // 3. Fallback: generate from username + email_domain.
        if let Some(ref domain) = self.email_domain {
            let identity = GitIdentity {
                name: svn_username.to_string(),
                email: format!("{}@{}", svn_username, domain),
            };
            debug!(svn_username, email = %identity.email, "using fallback identity");
            return Ok(identity);
        }

        Err(IdentityError::SvnUserNotFound(svn_username.to_string()))
    }

    /// Map a Git identity back to an SVN username.
    ///
    /// Lookup order:
    /// 1. Reverse cache (email -> SVN username)
    /// 2. LDAP reverse lookup
    /// 3. Derive from email local-part
    pub fn git_to_svn(&self, git_name: &str, git_email: &str) -> Result<String, IdentityError> {
        // 1. Check reverse cache.
        {
            let reverse = self
                .reverse_cache
                .read()
                .map_err(|_| IdentityError::LdapError("reverse cache lock poisoned".into()))?;
            if let Some(username) = reverse.get(git_email) {
                debug!(git_email, svn_username = %username, "found in reverse cache");
                return Ok(username.clone());
            }
        }

        // 2. Try LDAP reverse lookup.
        if let Some(ref ldap) = self.ldap {
            let mut resolver = ldap
                .write()
                .map_err(|_| IdentityError::LdapError("LDAP lock poisoned".into()))?;
            let ldap_result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(resolver.lookup_by_email(git_email))
            })?;
            if let Some(username) = ldap_result {
                debug!(git_email, svn_username = %username, "found via LDAP reverse");
                return Ok(username);
            }
        }

        // 3. Fallback: use the local part of the email.
        if let Some(local_part) = git_email.split('@').next() {
            if !local_part.is_empty() {
                debug!(
                    git_email,
                    svn_username = local_part,
                    "using email local-part as SVN username fallback"
                );
                return Ok(local_part.to_string());
            }
        }

        Err(IdentityError::GitIdentityNotFound {
            name: git_name.to_string(),
            email: git_email.to_string(),
        })
    }

    /// Reload the mapping file from disk. This is safe to call while the
    /// mapper is in use; existing lookups will see the old data until the
    /// write lock is acquired.
    pub fn reload(&self) -> Result<(), IdentityError> {
        let path = match &self.mapping_file_path {
            Some(p) => p,
            None => {
                debug!("no mapping file to reload");
                return Ok(());
            }
        };

        info!(path = %path.display(), "reloading identity mapping file");
        let entries = MappingFile::load(path)?;
        let reverse = build_reverse_cache(&entries);

        {
            let mut cache = self
                .cache
                .write()
                .map_err(|_| IdentityError::LdapError("cache lock poisoned".into()))?;
            *cache = entries;
        }
        {
            let mut rev = self
                .reverse_cache
                .write()
                .map_err(|_| IdentityError::LdapError("reverse cache lock poisoned".into()))?;
            *rev = reverse;
        }

        info!("identity mapping reloaded");
        Ok(())
    }

    /// Add an identity to the in-memory cache (does NOT persist to disk).
    fn add_to_cache(&self, svn_username: &str, identity: &GitIdentity) {
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(
                svn_username.to_string(),
                AuthorEntry {
                    name: identity.name.clone(),
                    email: identity.email.clone(),
                },
            );
        }
        if let Ok(mut reverse) = self.reverse_cache.write() {
            reverse.insert(identity.email.clone(), svn_username.to_string());
        }
    }
}

/// Build a reverse lookup map from email -> SVN username.
///
/// Warns if multiple SVN usernames map to the same email address (last-write-wins).
fn build_reverse_cache(entries: &HashMap<String, AuthorEntry>) -> HashMap<String, String> {
    let mut reverse = HashMap::new();
    for (username, entry) in entries {
        if let Some(existing_username) = reverse.insert(entry.email.clone(), username.clone()) {
            if existing_username != *username {
                warn!(
                    email = %entry.email,
                    user1 = %existing_username,
                    user2 = %username,
                    "duplicate email in identity mapping — '{}' overwrites '{}' for email '{}'",
                    username, existing_username, entry.email
                );
            }
        }
    }
    reverse
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IdentityConfig;

    fn config_with_file(path: &std::path::Path) -> IdentityConfig {
        IdentityConfig {
            mapping_file: Some(path.to_path_buf()),
            email_domain: Some("example.com".into()),
            ..Default::default()
        }
    }

    fn write_test_mapping(path: &std::path::Path) {
        let content = r#"
[authors]
[authors.jdoe]
name = "John Doe"
email = "john.doe@example.com"

[authors.alice]
name = "Alice Smith"
email = "alice@example.com"
"#;
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_svn_to_git_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authors.toml");
        write_test_mapping(&path);

        let mapper = IdentityMapper::new(&config_with_file(&path)).unwrap();
        let identity = mapper.svn_to_git("jdoe").unwrap();
        assert_eq!(identity.name, "John Doe");
        assert_eq!(identity.email, "john.doe@example.com");
    }

    #[test]
    fn test_svn_to_git_fallback() {
        let config = IdentityConfig {
            email_domain: Some("corp.example.com".into()),
            ..Default::default()
        };
        let mapper = IdentityMapper::new(&config).unwrap();
        let identity = mapper.svn_to_git("unknown_user").unwrap();
        assert_eq!(identity.name, "unknown_user");
        assert_eq!(identity.email, "unknown_user@corp.example.com");
    }

    #[test]
    fn test_svn_to_git_no_fallback() {
        let config = IdentityConfig::default();
        let mapper = IdentityMapper::new(&config).unwrap();
        let result = mapper.svn_to_git("nobody");
        assert!(matches!(result, Err(IdentityError::SvnUserNotFound(_))));
    }

    #[test]
    fn test_git_to_svn_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authors.toml");
        write_test_mapping(&path);

        let mapper = IdentityMapper::new(&config_with_file(&path)).unwrap();
        let username = mapper
            .git_to_svn("John Doe", "john.doe@example.com")
            .unwrap();
        assert_eq!(username, "jdoe");
    }

    #[test]
    fn test_git_to_svn_fallback() {
        let config = IdentityConfig::default();
        let mapper = IdentityMapper::new(&config).unwrap();
        let username = mapper
            .git_to_svn("Random User", "ruser@company.com")
            .unwrap();
        assert_eq!(username, "ruser");
    }

    #[test]
    fn test_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authors.toml");
        write_test_mapping(&path);

        let mapper = IdentityMapper::new(&config_with_file(&path)).unwrap();

        // Add new entry to file.
        let updated = r#"
[authors]
[authors.jdoe]
name = "John Doe"
email = "john.doe@example.com"

[authors.alice]
name = "Alice Smith"
email = "alice@example.com"

[authors.bob]
name = "Bob Builder"
email = "bob@example.com"
"#;
        std::fs::write(&path, updated).unwrap();

        mapper.reload().unwrap();
        let identity = mapper.svn_to_git("bob").unwrap();
        assert_eq!(identity.name, "Bob Builder");
    }
}
