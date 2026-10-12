//! Read registered Git branches into a private, unreferenced object repository.
//! This proves remote Git inputs only; mappings, SVN and job admission are separate.

use git2::{FetchOptions, Oid, RemoteRedirect, Repository, RepositoryInitOptions};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::errors::GitError;

use super::execution_configuration::LiveExecutionConfiguration;
use super::http_transport::HttpTransport;
use super::publication::{
    checked_anonymous_remote, no_credentials, require_complete_git_graph,
    require_pinned_push_endpoint_with_policy, PinnedPushPolicy,
};
use super::workspace::RefreshWorkspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedGitSourcePins {
    pub remote_url: String,
    pub child_branch: String,
    pub parent_branch: String,
    pub child_tip: String,
    pub parent_tip: String,
}

/// This repository belongs solely to this observation. No shared worktree,
/// ref or FETCH_HEAD is updated. Its objects remain alive with the snapshot.
pub struct GitSourceSnapshot {
    repository: Repository,
    pins: ObservedGitSourcePins,
    _directory: SourceDirectory,
}

enum SourceDirectory {
    Temporary(tempfile::TempDir),
    Workspace {
        path: PathBuf,
        _owner: RefreshWorkspace,
    },
}

impl SourceDirectory {
    fn path(&self) -> &Path {
        match self {
            Self::Temporary(directory) => directory.path(),
            Self::Workspace { path, .. } => path,
        }
    }
}

impl GitSourceSnapshot {
    pub fn repository(&self) -> &Repository {
        &self.repository
    }
    pub fn pins(&self) -> &ObservedGitSourcePins {
        &self.pins
    }
}

fn refused(detail: &str) -> GitError {
    git2::Error::from_str(detail).into()
}

/// The authenticated caller supplies a freshly captured registration snapshot.
/// Each advertisement must show both registered checkpoint refs unchanged.
/// This is read-only evidence, not an atomic lock across Git refs or SVN.
/// Parent/child may use different explicit tokens, with no fallback between them.
pub fn observe_registered_git_sources(
    configuration: &LiveExecutionConfiguration,
) -> Result<GitSourceSnapshot, GitError> {
    observe_with_client(configuration, source_client()?)
}

fn source_client() -> Result<reqwest::Client, GitError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|_| refused("could not initialize pinned Git source client"))?;
    Ok(client)
}

pub(crate) fn observe_registered_git_sources_in_workspace(
    configuration: &LiveExecutionConfiguration,
    workspace: RefreshWorkspace,
) -> Result<GitSourceSnapshot, GitError> {
    observe_in_workspace_with_client(configuration, workspace, source_client()?)
}

fn observe_in_workspace_with_client(
    configuration: &LiveExecutionConfiguration,
    workspace: RefreshWorkspace,
    client: reqwest::Client,
) -> Result<GitSourceSnapshot, GitError> {
    let path = workspace
        .create_directory("git")
        .map_err(|_| refused("owned Git resource directory is unavailable"))?;
    let snapshot = observe_at_directory(
        configuration,
        client,
        SourceDirectory::Workspace {
            path,
            _owner: workspace.clone(),
        },
    )?;
    workspace
        .require_current()
        .map_err(|_| refused("owned Git resource identity changed"))?;
    Ok(snapshot)
}

fn observe_with_client(
    configuration: &LiveExecutionConfiguration,
    client: reqwest::Client,
) -> Result<GitSourceSnapshot, GitError> {
    let directory = tempfile::Builder::new()
        .prefix("reposync-refresh-git-input-")
        .tempdir()
        .map_err(|_| refused("could not create private Git input directory"))?;
    observe_at_directory(configuration, client, SourceDirectory::Temporary(directory))
}

fn observe_at_directory(
    configuration: &LiveExecutionConfiguration,
    client: reqwest::Client,
    directory: SourceDirectory,
) -> Result<GitSourceSnapshot, GitError> {
    let pin = configuration.pin();
    let mut init = RepositoryInitOptions::new();
    init.bare(true).external_template(false).no_reinit(true);
    let repository = Repository::init_opts(directory.path(), &init)?;
    require_pinned_push_endpoint_with_policy(
        &repository,
        &pin.git_remote_url,
        PinnedPushPolicy::SmartHttpV1,
    )?;
    let credentials = configuration.credentials();
    let expected_refs = [
        (
            format!("refs/heads/{}", pin.parent_git_branch),
            Oid::from_str(&pin.parent.last_git_sha)?,
        ),
        (
            format!("refs/heads/{}", pin.child_git_branch),
            Oid::from_str(&pin.child.last_git_sha)?,
        ),
    ];
    for (branch, expected, token) in [
        (
            pin.parent_git_branch.as_str(),
            pin.parent.last_git_sha.as_str(),
            credentials.parent_git_token(),
        ),
        (
            pin.child_git_branch.as_str(),
            pin.child.last_git_sha.as_str(),
            credentials.child_git_token(),
        ),
    ] {
        let transport = HttpTransport::new(&pin.git_remote_url, token, client.clone())?;
        download_pinned_branch(
            &repository,
            &transport,
            branch,
            Oid::from_str(expected)?,
            &expected_refs,
        )?;
    }
    require_complete_git_graph(&repository)?;
    Ok(GitSourceSnapshot {
        repository,
        pins: ObservedGitSourcePins {
            remote_url: pin.git_remote_url.clone(),
            child_branch: pin.child_git_branch.clone(),
            parent_branch: pin.parent_git_branch.clone(),
            child_tip: pin.child.last_git_sha.clone(),
            parent_tip: pin.parent.last_git_sha.clone(),
        },
        _directory: directory,
    })
}

#[cfg(test)]
pub(super) fn observe_registered_git_sources_in_workspace_for_test(
    configuration: &LiveExecutionConfiguration,
    workspace: RefreshWorkspace,
    client: reqwest::Client,
) -> Result<GitSourceSnapshot, GitError> {
    observe_in_workspace_with_client(configuration, workspace, client)
}

// Private test CA injection does not alter the production client or its TLS
// verification, proxy, redirect or credential behavior.
#[cfg(test)]
pub(super) fn observe_registered_git_sources_for_test(
    configuration: &LiveExecutionConfiguration,
    client: reqwest::Client,
) -> Result<GitSourceSnapshot, GitError> {
    observe_with_client(configuration, client)
}

fn download_pinned_branch(
    repository: &Repository,
    transport: &HttpTransport,
    branch: &str,
    expected: Oid,
    expected_refs: &[(String, Oid)],
) -> Result<(), GitError> {
    let target = format!("refs/heads/{branch}");
    if expected.is_zero() || !git2::Reference::is_valid_name(&target) {
        return Err(refused("invalid registered Git source pin"));
    }
    let mut remote = checked_anonymous_remote(repository, &transport.endpoint)?;
    let mut options = FetchOptions::new();
    options.follow_redirects(RemoteRedirect::None);
    options.remote_callbacks(no_credentials());
    remote.download(&[&target], Some(&mut options))?;
    for (name, expected) in expected_refs {
        let advertised = remote
            .list()?
            .iter()
            .filter(|head| head.name() == name)
            .map(|head| head.oid())
            .collect::<Vec<_>>();
        if advertised.as_slice() != [*expected] {
            return Err(refused("registered Git branch moved or is unavailable"));
        }
    }
    remote.disconnect()?;
    // Do not accept a truncated or missing ancestry merely because its tip
    // object arrived. Preparation later proves the supported merge/tree policy.
    let mut walk = repository.revwalk()?;
    walk.push(expected)?;
    for commit in walk {
        repository.find_commit(commit?)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "git_sources_tests.rs"]
mod tests;
