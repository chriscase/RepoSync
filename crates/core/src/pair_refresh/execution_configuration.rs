//! Capture registered execution inputs without treating a client DTO as proof.
//! This read-only configuration boundary does not admit a job or publish.

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::pair_refresh_credentials::{resolve_refresh_credentials, RefreshCredentials};
use crate::db::pair_refresh_operations::{
    capture_refresh_registration, RefreshJobPins, RefreshRegistrationPin,
};
use crate::db::Database;
use crate::errors::DatabaseError;

use super::publication::{canonical_url, PinnedPushPolicy};

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!(
        "pair refresh execution configuration refused: {detail}"
    ))
}

/// Safe metadata for the versioned preview. No credential value is serialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionConfigurationPin {
    pub version: u8,
    pub child: RefreshRegistrationPin,
    pub parent: RefreshRegistrationPin,
    pub git_remote_url: String,
    pub child_git_branch: String,
    pub parent_git_branch: String,
    pub child_svn_url: String,
    pub parent_svn_url: String,
    pub child_svn_username: String,
    pub parent_svn_username: String,
    pub transport_policy: PinnedPushPolicy,
    pub credential_context_digest: String,
}

// Construction is private; credentials cannot be serialized or debug-printed.
pub struct LiveExecutionConfiguration {
    pin: ExecutionConfigurationPin,
    credentials: RefreshCredentials,
}

impl LiveExecutionConfiguration {
    pub fn pin(&self) -> &ExecutionConfigurationPin {
        &self.pin
    }
    pub fn credentials(&self) -> &RefreshCredentials {
        &self.credentials
    }

    /// Build both SVN observers from the captured registrations and explicit
    /// credentials, with independent private authentication directories.
    /// Construction performs no remote observation or publication.
    pub fn isolated_svn_clients(
        &self,
    ) -> Result<(crate::svn::SvnClient, crate::svn::SvnClient), crate::errors::SvnError> {
        Ok((
            crate::svn::SvnClient::new_isolated_refresh(
                &self.pin.child_svn_url,
                &self.pin.child_svn_username,
                self.credentials.child_svn_password(),
            )?,
            crate::svn::SvnClient::new_isolated_refresh(
                &self.pin.parent_svn_url,
                &self.pin.parent_svn_username,
                self.credentials.parent_svn_password(),
            )?,
        ))
    }

    /// Check stored job endpoints against the current registered configuration.
    /// Live mappings/remotes, journal ownership and approval are separate gates.
    pub fn require_job_configuration(&self, job: &RefreshJobPins) -> Result<(), DatabaseError> {
        let lineage = &job.svn.pins.lineage.pins;
        if job.child != self.pin.child
            || job.parent != self.pin.parent
            || job.git_remote_url != self.pin.git_remote_url
            || job.git_branch != self.pin.child_git_branch
            || job.transport_policy != self.pin.transport_policy
            || job.credential_context_digest != self.pin.credential_context_digest
            || lineage.child_url != self.pin.child_svn_url
            || lineage.parent_url != self.pin.parent_svn_url
        {
            return Err(refused("stored job does not match registered sources"));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Source {
    parent_id: Option<String>,
    svn_url: String,
    svn_branch: String,
    svn_username: String,
    git_provider: String,
    git_api_url: String,
    git_repo: String,
    git_branch: String,
    sync_mode: String,
    enabled: bool,
    lfs_threshold_mb: i64,
    allowed_paths: Option<String>,
    blocked_patterns: Option<String>,
}

fn source(tx: &Connection, id: &str) -> Result<Source, DatabaseError> {
    let encoded: String = tx.query_row(
        "SELECT json_object('parent_id',parent_id,'svn_url',svn_url,'svn_branch',svn_branch,
            'svn_username',svn_username,'git_provider',git_provider,'git_api_url',git_api_url,
            'git_repo',git_repo,'git_branch',git_branch,'sync_mode',sync_mode,
            'enabled',json(CASE WHEN enabled=1 THEN 'true' ELSE 'false' END),
            'lfs_threshold_mb',lfs_threshold_mb,'allowed_paths',allowed_paths,'blocked_patterns',blocked_patterns)
         FROM repositories WHERE id=?1",
        [id], |row| row.get(0),
    ).optional()?.ok_or_else(|| refused("registration is missing"))?;
    serde_json::from_str(&encoded).map_err(|_| refused("registration fields are unreadable"))
}

fn no_projection(raw: Option<&str>) -> bool {
    raw.is_none_or(|raw| {
        serde_json::from_str::<Vec<String>>(raw).is_ok_and(|values| values.is_empty())
    })
}

fn require_supported(source: &Source) -> Result<(), DatabaseError> {
    if source.sync_mode != "team"
        || !source.enabled
        || source.lfs_threshold_mb != 0
        || !no_projection(source.allowed_paths.as_deref())
        || !no_projection(source.blocked_patterns.as_deref())
        || !matches!(source.git_provider.as_str(), "github" | "gitea")
        || !git2::Reference::is_valid_name(&format!("refs/heads/{}", source.git_branch))
        || source.git_branch.is_empty()
    {
        return Err(refused(
            "unsupported mode, projection, LFS or branch configuration",
        ));
    }
    Ok(())
}

fn svn_url(source: &Source) -> Result<String, DatabaseError> {
    let raw = super::branch_svn_url(&source.svn_url, &source.svn_branch);
    let url = reqwest::Url::parse(&raw).map_err(|_| refused("invalid SVN endpoint"))?;
    if !matches!(url.scheme(), "https" | "http" | "svn" | "file")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != raw
        || raw.contains('%')
        || raw
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(refused("unsupported or noncanonical SVN endpoint"));
    }
    Ok(raw)
}

pub(crate) fn capture_execution_configuration(
    tx: &Connection,
    child_id: &str,
) -> Result<LiveExecutionConfiguration, DatabaseError> {
    let child = source(tx, child_id)?;
    let parent_id = child
        .parent_id
        .as_deref()
        .filter(|id| !id.is_empty() && *id != child_id)
        .ok_or_else(|| refused("registration is not a distinct child pair"))?;
    let parent = source(tx, parent_id)?;
    require_supported(&child)?;
    require_supported(&parent)?;
    let git_remote_url =
        crate::git::derive_git_remote_url(&child.git_api_url, None, &child.git_repo);
    let parent_remote =
        crate::git::derive_git_remote_url(&parent.git_api_url, None, &parent.git_repo);
    let child_url =
        canonical_url(&git_remote_url).map_err(|_| refused("unsupported Git endpoint"))?;
    let parent_url =
        canonical_url(&parent_remote).map_err(|_| refused("unsupported parent Git endpoint"))?;
    if child_url.scheme() != "https" || parent_url.scheme() != "https" {
        return Err(refused("HTTPS Git endpoints are required"));
    }
    if git_remote_url != parent_remote || child.git_branch == parent.git_branch {
        return Err(refused("distinct branches on one Git remote are required"));
    }
    let child_svn_url = svn_url(&child)?;
    let parent_svn_url = svn_url(&parent)?;
    if child_svn_url == parent_svn_url {
        return Err(refused("distinct SVN branches are required"));
    }
    let credentials = resolve_refresh_credentials(tx, child_id, parent_id)?;
    for (username, password) in [
        (
            child.svn_username.as_str(),
            credentials.child_svn_password(),
        ),
        (
            parent.svn_username.as_str(),
            credentials.parent_svn_password(),
        ),
    ] {
        if password.is_some() == username.is_empty() {
            return Err(refused(
                "complete explicit SVN credentials or anonymous access required",
            ));
        }
    }
    let child_registration = capture_refresh_registration(tx, child_id)?;
    let parent_registration = capture_refresh_registration(tx, parent_id)?;
    for registration in [&child_registration, &parent_registration] {
        if !crate::history_inspect::is_full_git_oid(&registration.last_git_sha)
            || registration.last_svn_rev < 1
        {
            return Err(refused("synchronized checkpoints are not established"));
        }
    }
    let pin = ExecutionConfigurationPin {
        version: 2,
        child: child_registration,
        parent: parent_registration,
        git_remote_url,
        child_git_branch: child.git_branch,
        parent_git_branch: parent.git_branch,
        child_svn_url,
        parent_svn_url,
        child_svn_username: child.svn_username,
        parent_svn_username: parent.svn_username,
        transport_policy: PinnedPushPolicy::SmartHttpV1,
        credential_context_digest: credentials.context_digest().to_owned(),
    };
    Ok(LiveExecutionConfiguration { pin, credentials })
}

impl Database {
    /// Bind source configuration and both credential chains in one read
    /// transaction. Identities/checkpoint copies must already be established.
    pub fn capture_pair_refresh_execution_configuration(
        &self,
        child_id: &str,
    ) -> Result<LiveExecutionConfiguration, DatabaseError> {
        self.transaction(|tx| capture_execution_configuration(tx, child_id))
    }
}
