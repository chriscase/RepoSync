//! LDAP authentication support.
//!
//! When configured, users can authenticate against a corporate LDAP/Active Directory
//! server. On first login, a local user account is auto-provisioned from LDAP attributes.

use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};
use native_tls::TlsConnector;
use serde::{Deserialize, Serialize};
use tracing::debug;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// LDAP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdapConfig {
    /// LDAP server URL (e.g. "ldaps://ldap.example.com:3269").
    pub url: String,
    /// Base DN for user searches (e.g. "dc=corp,dc=example,dc=com").
    pub base_dn: String,
    /// Search filter template. `{0}` is replaced with the username.
    pub search_filter: String,
    /// LDAP attribute containing the user's display name.
    pub display_name_attr: String,
    /// LDAP attribute containing the user's email address.
    pub email_attr: String,
    /// LDAP attribute containing group membership.
    pub group_attr: String,
    /// Optional service account DN used to search for users.
    pub bind_dn: Option<String>,
    /// Optional service account password.
    pub bind_password: Option<String>,
    /// Whether to verify TLS certificates (default: true).
    #[serde(default = "default_tls_verify")]
    pub tls_verify: bool,
}

fn default_tls_verify() -> bool {
    true
}

/// Escape special characters in an LDAP filter value per RFC 4515.
pub fn escape_ldap_filter_value(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' => escaped.push_str("\\5c"),
            '*' => escaped.push_str("\\2a"),
            '(' => escaped.push_str("\\28"),
            ')' => escaped.push_str("\\29"),
            '\0' => escaped.push_str("\\00"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// A user record returned after successful LDAP authentication.
#[derive(Debug, Clone)]
pub struct LdapUser {
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub groups: Vec<String>,
}

/// Errors from LDAP operations.
#[derive(Debug, thiserror::Error)]
pub enum LdapAuthError {
    #[error("LDAP connection failed: {0}")]
    ConnectionFailed(String),

    #[error("LDAP bind failed: {0}")]
    BindFailed(String),

    #[error("LDAP search failed: {0}")]
    SearchFailed(String),

    #[error("user not found in LDAP")]
    UserNotFound,

    #[error("invalid credentials")]
    InvalidCredentials,

    #[error("LDAP error: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl LdapConfig {
    /// Authenticate a user via LDAP bind.
    ///
    /// 1. Connect to the LDAP server.
    /// 2. If `bind_dn` is set, bind as service account and search for the user DN.
    /// 3. Otherwise, construct a DN from the username and base DN.
    /// 4. Rebind with the user's DN and password to verify credentials.
    /// 5. Search for the user's attributes.
    /// 6. Return an `LdapUser` on success.
    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
    ) -> Result<LdapUser, LdapAuthError> {
        // Build connection settings — optionally accept self-signed certificates.
        let mut tls_builder = TlsConnector::builder();
        if !self.tls_verify {
            tls_builder.danger_accept_invalid_certs(true);
            tls_builder.danger_accept_invalid_hostnames(true);
        }
        let tls_connector = tls_builder
            .build()
            .map_err(|e| LdapAuthError::ConnectionFailed(format!("TLS setup failed: {}", e)))?;
        let settings = LdapConnSettings::new()
            .set_connector(tls_connector);
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &self.url)
            .await
            .map_err(|e| LdapAuthError::ConnectionFailed(e.to_string()))?;

        // Drive the connection in the background.
        ldap3::drive!(conn);

        // Build the search filter by replacing `{0}` with the escaped username.
        let escaped_username = escape_ldap_filter_value(username);
        let search_filter = self.search_filter.replace("{0}", &escaped_username);

        // Step 1: Find the user DN.
        let user_dn = if let (Some(bind_dn), Some(bind_pw)) =
            (self.bind_dn.as_deref(), self.bind_password.as_deref())
        {
            // Bind as service account first.
            ldap.simple_bind(bind_dn, bind_pw)
                .await
                .map_err(|e| LdapAuthError::BindFailed(format!("service account bind: {}", e)))?
                .success()
                .map_err(|e| LdapAuthError::BindFailed(format!("service account bind rejected: {}", e)))?;

            debug!("LDAP: bound as service account, searching for user '{}'", username);

            let (entries, _result) = ldap
                .search(&self.base_dn, Scope::Subtree, &search_filter, vec!["dn"])
                .await
                .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?
                .success()
                .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?;

            if entries.is_empty() {
                return Err(LdapAuthError::UserNotFound);
            }

            let entry = SearchEntry::construct(entries.into_iter().next().unwrap());
            entry.dn
        } else {
            // No service account — try common AD bind formats.
            // AD supports UPN (user@domain), DOMAIN\user, and DN-based bind.
            // Extract domain from base_dn: dc=mgc,dc=mentorg,dc=com → mgc.mentorg.com
            let domain = self.base_dn
                .split(',')
                .filter_map(|part| part.trim().strip_prefix("dc=").or_else(|| part.trim().strip_prefix("DC=")))
                .collect::<Vec<_>>()
                .join(".");

            // Try UPN format first (most common for AD): user@domain
            let upn = format!("{}@{}", username, domain);
            debug!("LDAP: trying UPN bind: {}", upn);

            let bind_result = ldap
                .simple_bind(&upn, password)
                .await
                .map_err(|e| LdapAuthError::BindFailed(e.to_string()))?;

            if bind_result.rc == 0 {
                // UPN bind succeeded — now search for user attributes
                let (entries, _) = ldap
                    .search(&self.base_dn, Scope::Subtree, &search_filter, vec![
                        &self.display_name_attr,
                        &self.email_attr,
                        &self.group_attr,
                    ])
                    .await
                    .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?
                    .success()
                    .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?;

                if entries.is_empty() {
                    // Auth succeeded but user not found in search — return basic info
                    return Ok(LdapUser {
                        username: username.to_string(),
                        display_name: username.to_string(),
                        email: format!("{}@{}", username, domain),
                        groups: vec![],
                    });
                }

                let entry = SearchEntry::construct(entries.into_iter().next().unwrap());
                let display_name = entry.attrs.get(&self.display_name_attr)
                    .and_then(|v| v.first())
                    .cloned()
                    .unwrap_or_else(|| username.to_string());
                let email = entry.attrs.get(&self.email_attr)
                    .and_then(|v| v.first())
                    .cloned()
                    .unwrap_or_else(|| format!("{}@{}", username, domain));
                let groups = entry.attrs.get(&self.group_attr)
                    .cloned()
                    .unwrap_or_default();

                let _ = ldap.unbind().await;
                return Ok(LdapUser {
                    username: username.to_string(),
                    display_name,
                    email,
                    groups,
                });
            }

            // UPN failed — try DOMAIN\user format
            let netbios = domain.split('.').next().unwrap_or("DOMAIN").to_uppercase();
            let domain_user = format!("{}\\{}", netbios, username);
            debug!("LDAP: UPN failed (rc={}), trying DOMAIN\\user: {}", bind_result.rc, domain_user);

            // Need a fresh connection for the retry
            drop(ldap);
            let mut tls_builder2 = TlsConnector::builder();
            if !self.tls_verify {
                tls_builder2.danger_accept_invalid_certs(true);
                tls_builder2.danger_accept_invalid_hostnames(true);
            }
            let tls_connector2 = tls_builder2
                .build()
                .map_err(|e| LdapAuthError::ConnectionFailed(format!("TLS: {}", e)))?;
            let settings2 = LdapConnSettings::new().set_connector(tls_connector2);
            let (conn2, mut ldap2) = LdapConnAsync::with_settings(settings2, &self.url)
                .await
                .map_err(|e| LdapAuthError::ConnectionFailed(e.to_string()))?;
            ldap3::drive!(conn2);

            let bind_result2 = ldap2
                .simple_bind(&domain_user, password)
                .await
                .map_err(|e| LdapAuthError::BindFailed(e.to_string()))?;

            if bind_result2.rc != 0 {
                return Err(LdapAuthError::InvalidCredentials);
            }

            // DOMAIN\user bind succeeded — search for attributes
            let search_filter2 = self.search_filter.replace("{0}", &escaped_username);
            let (entries, _) = ldap2
                .search(&self.base_dn, Scope::Subtree, &search_filter2, vec![
                    &self.display_name_attr,
                    &self.email_attr,
                    &self.group_attr,
                ])
                .await
                .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?
                .success()
                .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?;

            let (display_name, email, groups) = if !entries.is_empty() {
                let entry = SearchEntry::construct(entries.into_iter().next().unwrap());
                let dn = entry.attrs.get(&self.display_name_attr)
                    .and_then(|v| v.first()).cloned().unwrap_or_else(|| username.to_string());
                let em = entry.attrs.get(&self.email_attr)
                    .and_then(|v| v.first()).cloned().unwrap_or_else(|| format!("{}@{}", username, domain));
                let gr = entry.attrs.get(&self.group_attr).cloned().unwrap_or_default();
                (dn, em, gr)
            } else {
                (username.to_string(), format!("{}@{}", username, domain), vec![])
            };

            let _ = ldap2.unbind().await;
            return Ok(LdapUser {
                username: username.to_string(),
                display_name,
                email,
                groups,
            });
        };

        debug!("LDAP: attempting bind for user DN '{}'", user_dn);

        // Step 2: Bind as the user to verify credentials (service account path).
        let bind_result = ldap
            .simple_bind(&user_dn, password)
            .await
            .map_err(|e| LdapAuthError::BindFailed(e.to_string()))?;

        if bind_result.rc != 0 {
            debug!("LDAP: user bind failed with rc={}", bind_result.rc);
            return Err(LdapAuthError::InvalidCredentials);
        }

        // Step 3: Search for user attributes.
        let attrs = vec![
            self.display_name_attr.as_str(),
            self.email_attr.as_str(),
            self.group_attr.as_str(),
        ];

        let (entries, _result) = ldap
            .search(&self.base_dn, Scope::Subtree, &search_filter, attrs)
            .await
            .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?
            .success()
            .map_err(|e| LdapAuthError::SearchFailed(e.to_string()))?;

        let _ = ldap.unbind().await;

        if entries.is_empty() {
            return Err(LdapAuthError::UserNotFound);
        }

        let entry = SearchEntry::construct(entries.into_iter().next().unwrap());

        let display_name = entry
            .attrs
            .get(&self.display_name_attr)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| username.to_string());

        let email = entry
            .attrs
            .get(&self.email_attr)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default();

        let groups = entry
            .attrs
            .get(&self.group_attr)
            .cloned()
            .unwrap_or_default();

        Ok(LdapUser {
            username: username.to_string(),
            display_name,
            email,
            groups,
        })
    }

    /// Test LDAP connectivity by attempting a bind (service account or anonymous).
    pub async fn test_connection(&self) -> Result<String, LdapAuthError> {
        let settings = LdapConnSettings::new();
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &self.url)
            .await
            .map_err(|e| LdapAuthError::ConnectionFailed(e.to_string()))?;

        ldap3::drive!(conn);

        if let (Some(bind_dn), Some(bind_pw)) =
            (self.bind_dn.as_deref(), self.bind_password.as_deref())
        {
            ldap.simple_bind(bind_dn, bind_pw)
                .await
                .map_err(|e| LdapAuthError::BindFailed(format!("service account bind: {}", e)))?
                .success()
                .map_err(|e| LdapAuthError::BindFailed(format!("service account bind rejected: {}", e)))?;
        }

        let _ = ldap.unbind().await;

        Ok("LDAP connection successful".to_string())
    }
}
