//! Publication primitive for an approved pair refresh. Admission and recovery
//! must pin identity, credentials, SVN evidence and supported content separately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use git2::{Cred, CredentialType, Direction, Oid, PushOptions, RemoteCallbacks, Repository};

use crate::errors::GitError;

/// Check before durable admission or SVN publication. libgit2's local transport
/// does not enforce the receive-pack expected-old-object-ID race check.
pub fn require_pinned_push_transport(remote_url: &str) -> Result<(), GitError> {
    let parsed = reqwest::Url::parse(remote_url).map_err(|_| {
        git2::Error::from_str("pair refresh requires an explicit smart transport URL")
    })?;
    // The initial proof uses git:// receive-pack. Other smart transports need
    // the same shipping-library race controls before admission is expanded.
    if parsed.scheme() != "git"
        || parsed.host_str().is_none()
        || parsed.password().is_some()
        || (parsed.scheme() != "ssh" && !parsed.username().is_empty())
        || parsed.fragment().is_some()
        || parsed.query().is_some()
        || remote_url.chars().any(char::is_control)
    {
        return Err(git2::Error::from_str("unsupported pair refresh publication transport").into());
    }
    Ok(())
}

fn credentials(token: Option<&str>) -> RemoteCallbacks<'static> {
    let token = token.map(str::to_owned);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username, allowed| {
        if (url.starts_with("https://") || url.starts_with("http://"))
            && allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
        {
            if let Some(token) = &token {
                return Cred::userpass_plaintext("x-access-token", token);
            }
        }
        if url.starts_with("ssh://") && allowed.contains(CredentialType::SSH_KEY) {
            return Cred::ssh_key_from_agent(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::DEFAULT) {
            return Cred::default();
        }
        Err(git2::Error::from_str(
            "no supported pair refresh credential",
        ))
    });
    callbacks
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
    require_pinned_push_transport(remote_url)?;
    let target = format!("refs/heads/{branch}");
    if !git2::Reference::is_valid_name(&target)
        || expected_old.is_zero()
        || prepared == expected_old
        || !repo.graph_descendant_of(prepared, expected_old)?
    {
        return Err(git2::Error::from_str("invalid pinned child publication inputs").into());
    }
    repo.find_commit(prepared)?;
    let mut remote = repo.remote_anonymous(remote_url)?;
    let accepted = Arc::new(AtomicBool::new(false));
    let negotiated = Arc::new(AtomicBool::new(false));
    let mut callbacks = credentials(http_auth_token);
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
    options.remote_callbacks(callbacks);
    remote.push(&[&format!("{prepared}:{target}")], Some(&mut options))?;
    if !negotiated.load(Ordering::Relaxed) || !accepted.load(Ordering::Relaxed) {
        return Err(git2::Error::from_str("missing pinned child publication receipt").into());
    }
    remote.disconnect()?;
    remote.connect_auth(Direction::Fetch, Some(credentials(http_auth_token)), None)?;
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
