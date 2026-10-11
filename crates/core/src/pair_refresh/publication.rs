//! Publication primitive for an approved pair refresh. Admission and recovery
//! must pin identity, credentials, SVN evidence and supported content separately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use git2::{FetchOptions, Oid, PushOptions, RemoteCallbacks, RemoteRedirect, Repository};
use serde::{Deserialize, Serialize};

use super::http_transport::HttpTransport;
use crate::errors::GitError;

/// Execution approval binds this explicit publication policy. The original
/// Git-only primitive does not silently acquire additional transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinnedPushPolicy {
    GitProtocolV1,
    SmartHttpV1,
}

/// Validate URL syntax. Admission must also use [`require_pinned_push_endpoint`]
/// to reject Git configuration rewrites before any durable intent or SVN write.
pub fn require_pinned_push_transport(remote_url: &str) -> Result<(), GitError> {
    require_transport(remote_url, PinnedPushPolicy::GitProtocolV1)
}

fn canonical_url(remote_url: &str) -> Result<reqwest::Url, GitError> {
    let parsed = reqwest::Url::parse(remote_url).map_err(|_| {
        git2::Error::from_str("pair refresh requires an explicit smart transport URL")
    })?;
    if parsed.as_str() != remote_url
        || parsed.host_str().is_none_or(str::is_empty)
        || parsed.password().is_some()
        || !parsed.username().is_empty()
        || parsed.fragment().is_some()
        || parsed.query().is_some()
        || remote_url.chars().any(char::is_control)
        || parsed
            .path()
            .split('/')
            .skip(1)
            .any(|segment| matches!(segment, "" | "." | ".."))
        || parsed.path().is_empty()
    {
        return Err(git2::Error::from_str("noncanonical pair refresh publication URL").into());
    }
    Ok(parsed)
}

fn require_transport(remote_url: &str, policy: PinnedPushPolicy) -> Result<(), GitError> {
    let parsed = canonical_url(remote_url)?;
    let supported = match policy {
        PinnedPushPolicy::GitProtocolV1 => parsed.scheme() == "git",
        PinnedPushPolicy::SmartHttpV1 => parsed.scheme() == "https",
    };
    if !supported {
        return Err(git2::Error::from_str("unsupported pair refresh publication transport").into());
    }
    Ok(())
}

fn pinned_remote<'repo>(
    repo: &'repo Repository,
    remote_url: &str,
    policy: PinnedPushPolicy,
) -> Result<git2::Remote<'repo>, GitError> {
    require_transport(remote_url, policy)?;
    checked_anonymous_remote(repo, remote_url)
}

fn checked_anonymous_remote<'repo>(
    repo: &'repo Repository,
    remote_url: &str,
) -> Result<git2::Remote<'repo>, GitError> {
    let remote = repo.remote_anonymous(remote_url)?;
    if remote.url() != Some(remote_url) || remote.pushurl().is_some_and(|url| url != remote_url) {
        return Err(git2::Error::from_str("Git configuration rewrites the pinned endpoint").into());
    }
    Ok(remote)
}

/// Resolve both effective URLs without network I/O. Git insteadOf and
/// pushInsteadOf rules must not substitute another transport or destination.
pub fn require_pinned_push_endpoint(repo: &Repository, remote_url: &str) -> Result<(), GitError> {
    require_pinned_push_endpoint_with_policy(repo, remote_url, PinnedPushPolicy::GitProtocolV1)
}

/// Perform the same effective-endpoint check under the approval's explicit
/// transport policy before creating a job or publishing to SVN.
pub fn require_pinned_push_endpoint_with_policy(
    repo: &Repository,
    remote_url: &str,
    policy: PinnedPushPolicy,
) -> Result<(), GitError> {
    pinned_remote(repo, remote_url, policy)?;
    Ok(())
}

fn no_credentials() -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|_, _, _| {
        Err(git2::Error::from_str(
            "publication transport cannot request ambient credentials",
        ))
    });
    callbacks
}

/// Ancestry must describe the complete stored commit graph. Admission and
/// publication both refuse local overlays or shallow boundaries.
pub fn require_complete_git_graph(repo: &Repository) -> Result<(), GitError> {
    // git2 0.19 does not expose the common directory accessor. Operation-local
    // repositories need no linked worktree; refuse it rather than inspect the
    // wrong grafts path under .git/worktrees/<name>.
    if repo.is_worktree() {
        return Err(git2::Error::from_str("pair refresh does not support linked worktrees").into());
    }
    let grafts = match std::fs::symlink_metadata(repo.path().join("info/grafts")) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(git2::Error::from_str("could not inspect Git graph metadata").into());
        }
    };
    if repo.is_shallow() || grafts || repo.references_glob("refs/replace/*")?.next().is_some() {
        return Err(git2::Error::from_str(
            "pair refresh requires an unmodified complete Git graph",
        )
        .into());
    }
    Ok(())
}

/// Publish exactly one prepared descendant to the pinned child branch using a
/// normal refspec. An error after negotiation is an uncertain external outcome:
/// the durable caller must inspect the remote before deciding how to recover.
/// This bypasses CLI LFS hooks; admission must refuse LFS content beforehand.
pub fn publish_pinned_child(
    repo: &Repository,
    remote_url: &str,
    branch: &str,
    expected_old: Oid,
    prepared: Oid,
    http_auth_token: Option<&str>,
) -> Result<Oid, GitError> {
    publish_pinned_child_with_policy(
        repo,
        remote_url,
        branch,
        expected_old,
        prepared,
        http_auth_token,
        PinnedPushPolicy::GitProtocolV1,
    )
}

/// Explicit-policy publisher used by the approved execution workflow. HTTP
/// requests use ordinary certificate verification and never follow redirects.
pub fn publish_pinned_child_with_policy(
    repo: &Repository,
    remote_url: &str,
    branch: &str,
    expected_old: Oid,
    prepared: Oid,
    http_auth_token: Option<&str>,
    policy: PinnedPushPolicy,
) -> Result<Oid, GitError> {
    match policy {
        PinnedPushPolicy::GitProtocolV1 => publish_with_callbacks(
            repo,
            pinned_remote(repo, remote_url, policy)?,
            branch,
            expected_old,
            prepared,
            no_credentials,
        ),
        PinnedPushPolicy::SmartHttpV1 => {
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .map_err(|_| git2::Error::from_str("could not initialize pinned HTTP client"))?;
            publish_http_with_client(
                repo,
                remote_url,
                branch,
                expected_old,
                prepared,
                http_auth_token,
                client,
            )
        }
    }
}

fn publish_http_with_client(
    repo: &Repository,
    remote_url: &str,
    branch: &str,
    expected_old: Oid,
    prepared: Oid,
    token: Option<&str>,
    client: reqwest::Client,
) -> Result<Oid, GitError> {
    require_pinned_push_endpoint_with_policy(repo, remote_url, PinnedPushPolicy::SmartHttpV1)?;
    publish_via_http_transport(
        repo,
        remote_url,
        branch,
        expected_old,
        prepared,
        token,
        client,
    )
}

fn publish_via_http_transport(
    repo: &Repository,
    remote_url: &str,
    branch: &str,
    expected_old: Oid,
    prepared: Oid,
    token: Option<&str>,
    client: reqwest::Client,
) -> Result<Oid, GitError> {
    let transport = HttpTransport::new(remote_url, token, client)?;
    publish_with_callbacks(
        repo,
        // This endpoint is generated by the operation-local listener. Check
        // effective URL rewrites but never treat it as the approved upstream.
        checked_anonymous_remote(repo, &transport.endpoint)?,
        branch,
        expected_old,
        prepared,
        no_credentials,
    )
}

fn publish_with_callbacks<'cb>(
    repo: &Repository,
    mut remote: git2::Remote<'_>,
    branch: &str,
    expected_old: Oid,
    prepared: Oid,
    mut make_callbacks: impl FnMut() -> RemoteCallbacks<'cb>,
) -> Result<Oid, GitError> {
    require_complete_git_graph(repo)?;
    let target = format!("refs/heads/{branch}");
    if !git2::Reference::is_valid_name(&target)
        || expected_old.is_zero()
        || prepared == expected_old
        || !repo.graph_descendant_of(prepared, expected_old)?
    {
        return Err(git2::Error::from_str("invalid pinned child publication inputs").into());
    }
    repo.find_commit(prepared)?;
    let accepted = Arc::new(AtomicBool::new(false));
    let negotiated = Arc::new(AtomicBool::new(false));
    let mut callbacks = make_callbacks();
    let negotiation_target = target.clone();
    let negotiation_receipt = negotiated.clone();
    callbacks.push_negotiation(move |updates| {
        if updates.len() != 1
            || updates[0].dst_refname() != Some(negotiation_target.as_str())
            || updates[0].src() != expected_old
            || updates[0].dst() != prepared
        {
            return Err(git2::Error::from_str(
                "pinned child tip changed before publication",
            ));
        }
        negotiation_receipt.store(true, Ordering::Relaxed);
        Ok(())
    });
    let update_target = target.clone();
    let update_receipt = accepted.clone();
    callbacks.push_update_reference(move |name, status| {
        if name != update_target || status.is_some() {
            return Err(git2::Error::from_str(
                "pinned child reference update rejected",
            ));
        }
        update_receipt.store(true, Ordering::Relaxed);
        Ok(())
    });
    let mut options = PushOptions::new();
    options.follow_redirects(RemoteRedirect::None);
    options.remote_callbacks(callbacks);
    remote.push(&[&format!("{prepared}:{target}")], Some(&mut options))?;
    if !negotiated.load(Ordering::Relaxed) || !accepted.load(Ordering::Relaxed) {
        return Err(git2::Error::from_str("missing pinned child publication receipt").into());
    }
    remote.disconnect()?;
    let mut fetch = FetchOptions::new();
    fetch.follow_redirects(RemoteRedirect::None);
    fetch.remote_callbacks(make_callbacks());
    // A fetch connection through download applies the explicit no-redirect
    // policy too. It writes only operation-local objects, never remote refs.
    remote.download(&[&target], Some(&mut fetch))?;
    let observed = remote
        .list()?
        .iter()
        .find(|head| head.name() == target)
        .map(|head| head.oid());
    remote.disconnect()?;
    if observed != Some(prepared) {
        return Err(git2::Error::from_str("child tip differs from published result").into());
    }
    Ok(prepared)
}

#[cfg(all(test, unix))]
#[path = "publication_http_tests.rs"]
mod http_tests;
