//! LDAP-based identity resolution.
//!
//! Provides [`LdapResolver`] backed by a real `ldap3` client. Each lookup
//! opens a fresh connection, binds with the service-account credentials, runs
//! a scoped search, then unbinds.

use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};

use crate::errors::IdentityError;
use crate::identity::mapper::GitIdentity;
use crate::ldap_auth::escape_ldap_filter_value;
use tracing::{debug, info};

/// LDAP resolver for looking up Git <-> SVN identities.
pub struct LdapResolver {
    url: String,
    base_dn: String,
    bind_dn: String,
    bind_password: String,
    /// Tracks whether at least one successful connection has been made.
    connected: bool,
}

impl LdapResolver {
    /// Create a new LDAP resolver.
    ///
    /// Connection is deferred until the first lookup.
    pub fn new(
        url: impl Into<String>,
        base_dn: impl Into<String>,
        bind_dn: impl Into<String>,
        bind_password: impl Into<String>,
    ) -> Self {
        let resolver = Self {
            url: url.into(),
            base_dn: base_dn.into(),
            bind_dn: bind_dn.into(),
            bind_password: bind_password.into(),
            connected: false,
        };
        info!(
            url = %resolver.url,
            base_dn = %resolver.base_dn,
            "created LdapResolver"
        );
        resolver
    }

    /// Open an LDAP connection and bind with the service-account credentials.
    async fn connect(&self) -> Result<ldap3::Ldap, IdentityError> {
        let settings = LdapConnSettings::new();
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &self.url)
            .await
            .map_err(|e| IdentityError::LdapError(format!("connection failed: {}", e)))?;
        ldap3::drive!(conn);
        ldap.simple_bind(&self.bind_dn, &self.bind_password)
            .await
            .map_err(|e| IdentityError::LdapError(format!("bind failed: {}", e)))?
            .success()
            .map_err(|e| IdentityError::LdapError(format!("bind rejected: {}", e)))?;
        Ok(ldap)
    }

    /// Look up a Git identity by SVN / LDAP username.
    ///
    /// Searches `(uid={username})` under `base_dn` and returns a
    /// [`GitIdentity`] built from the `cn` and `mail` attributes.
    pub async fn lookup_by_username(
        &mut self,
        username: &str,
    ) -> Result<Option<GitIdentity>, IdentityError> {
        debug!(username, "LDAP lookup by username");
        let mut ldap = self.connect().await?;
        self.connected = true;

        let escaped = escape_ldap_filter_value(username);
        let filter = format!("(uid={})", escaped);

        let (entries, _res) = ldap
            .search(&self.base_dn, Scope::Subtree, &filter, vec!["cn", "mail"])
            .await
            .map_err(|e| IdentityError::LdapError(format!("search failed: {}", e)))?
            .success()
            .map_err(|e| IdentityError::LdapError(format!("search error: {}", e)))?;

        let _ = ldap.unbind().await;

        let entry = match entries.into_iter().next() {
            Some(e) => SearchEntry::construct(e),
            None => return Ok(None),
        };

        let name = entry
            .attrs
            .get("cn")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| username.to_string());
        let email = match entry.attrs.get("mail").and_then(|v| v.first()).cloned() {
            Some(e) => e,
            None => return Ok(None),
        };

        debug!(username, name = %name, email = %email, "LDAP resolved identity");
        Ok(Some(GitIdentity { name, email }))
    }

    /// Reverse lookup: find an SVN username by Git email.
    ///
    /// Searches `(mail={email})` under `base_dn` and returns the `uid`
    /// attribute of the first matching entry.
    pub async fn lookup_by_email(
        &mut self,
        email: &str,
    ) -> Result<Option<String>, IdentityError> {
        debug!(email, "LDAP reverse lookup by email");
        let mut ldap = self.connect().await?;
        self.connected = true;

        let escaped = escape_ldap_filter_value(email);
        let filter = format!("(mail={})", escaped);

        let (entries, _res) = ldap
            .search(&self.base_dn, Scope::Subtree, &filter, vec!["uid"])
            .await
            .map_err(|e| IdentityError::LdapError(format!("search failed: {}", e)))?
            .success()
            .map_err(|e| IdentityError::LdapError(format!("search error: {}", e)))?;

        let _ = ldap.unbind().await;

        let entry = match entries.into_iter().next() {
            Some(e) => SearchEntry::construct(e),
            None => return Ok(None),
        };

        let uid = entry
            .attrs
            .get("uid")
            .and_then(|v| v.first())
            .cloned();

        debug!(email, uid = ?uid, "LDAP resolved uid");
        Ok(uid)
    }

    /// Return whether at least one successful connection has been made.
    pub fn is_connected(&self) -> bool {
        self.connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolver_construction() {
        let resolver = LdapResolver::new(
            "ldap://ldap.example.com",
            "dc=example,dc=com",
            "cn=admin,dc=example,dc=com",
            "password",
        );
        assert!(!resolver.is_connected());
    }
}
