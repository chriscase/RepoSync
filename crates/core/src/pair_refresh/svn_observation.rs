//! Read-only recovery evidence for a prepared normal SVN merge. No absence
//! observation authorizes another write: a durable caller must keep its holds
//! after an uncertain commit and reconcile its journal before proceeding.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lineage::{inspect_svn_refresh_lineage, SvnCopyProof};
use super::svn_preparation::{
    expected_refresh_mergeinfo, parse_refresh_properties, verify_refresh_svn_tree,
    PreparedCleanChildSvn, SvnPropertySnapshot,
};
use crate::errors::SvnError;
use crate::svn::{SvnClient, SvnLogEntry};

const MARKER: &str = "RepoSync-Pair-Refresh-Operation: ";

fn refused(detail: &str) -> SvnError {
    SvnError::LineageRefused {
        reason: "unproved_pair_refresh_svn_publication".into(),
        detail: detail.into(),
    }
}

/// Stable, exact message used by the later journaled publication caller.
pub fn refresh_svn_commit_message(operation_id: Uuid) -> Result<String, SvnError> {
    if operation_id.is_nil() {
        return Err(refused("missing publication operation identity"));
    }
    Ok(format!(
        "RepoSync update pair from parent\n\n{MARKER}{operation_id}"
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvnRefreshPublicationReceipt {
    pub operation_id: Uuid,
    pub revision: i64,
    pub child_url: String,
    pub child_copy: SvnCopyProof,
    pub git_tree: String,
    pub mergeinfo: String,
    pub properties: SvnPropertySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvnPublicationObservation {
    /// The marker was not seen in this snapshot. This is not proof that an
    /// uncertain commit failed, and must never trigger a blind retry.
    NotObserved,
    Observed(Box<SvnRefreshPublicationReceipt>),
}

fn relative_path<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    path.strip_prefix(root)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .filter(|path| {
            !path.is_empty()
                && path.split('/').all(|part| {
                    !matches!(part, "" | "." | ".." | ".svn" | ".git")
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                })
        })
}

fn require_candidate_paths(
    entry: &SvnLogEntry,
    prepared: &PreparedCleanChildSvn,
) -> Result<(), SvnError> {
    let pins = &prepared.pins.lineage.pins;
    let child = &pins.child.child_path;
    let mut root_modified = false;
    let mut paths = BTreeSet::new();
    for change in &entry.changed_paths {
        if !paths.insert(change.path.as_str()) {
            return Err(refused("duplicate publication changed path"));
        }
        if &change.path == child {
            if change.action != "M"
                || change.copy_from_path.is_some()
                || change.copy_from_rev.is_some()
            {
                return Err(refused("publication replaced its child root"));
            }
            root_modified = true;
        } else {
            let relative = relative_path(&change.path, child)
                .ok_or_else(|| refused("publication also changed an unapproved path"))?;
            if !matches!(change.action.as_str(), "A" | "M" | "D" | "R") {
                return Err(refused("unsupported publication path action"));
            }
            match (&change.copy_from_path, change.copy_from_rev) {
                (None, None) if matches!(change.action.as_str(), "M" | "D") => {}
                (Some(source), Some(revision))
                    if matches!(change.action.as_str(), "A" | "R")
                        && source == &format!("{}/{relative}", pins.child.parent_path)
                        && revision == pins.parent_revision => {}
                _ => {
                    return Err(refused(
                        "publication copy source differs from pinned parent",
                    ))
                }
            }
        }
    }
    if !root_modified {
        return Err(refused("publication omitted the root mergeinfo change"));
    }
    Ok(())
}

fn publication_revision(
    history: &[SvnLogEntry],
    prepared: &PreparedCleanChildSvn,
    inspected_revision: i64,
) -> Result<Option<i64>, SvnError> {
    let expected = refresh_svn_commit_message(prepared.operation_id)?;
    let marker = format!("{MARKER}{}", prepared.operation_id);
    let input_revision = prepared.pins.lineage.pins.child.child_revision;
    let mut previous = None;
    let mut candidate = None;
    let mut new_revisions = 0;
    for entry in history {
        if entry.revision < 1
            || entry.revision > inspected_revision
            || previous.is_some_and(|rev| entry.revision >= rev)
        {
            return Err(refused("publication history is unordered or duplicate"));
        }
        previous = Some(entry.revision);
        if entry.revision > input_revision {
            new_revisions += 1;
        }
        if !entry.message.contains(&marker) {
            continue;
        }
        if entry.message != expected || entry.revision <= input_revision || candidate.is_some() {
            return Err(refused(
                "publication marker is ambiguous, reused or malformed",
            ));
        }
        require_candidate_paths(entry, prepared)?;
        candidate = Some(entry.revision);
    }
    if history.is_empty() {
        return Err(refused("publication history is empty"));
    }
    if new_revisions != usize::from(candidate.is_some()) {
        return Err(refused(
            "child changed outside the one approved publication",
        ));
    }
    Ok(candidate)
}

fn normalized(mut properties: SvnPropertySnapshot) -> SvnPropertySnapshot {
    properties.retain(|path, values| path.is_empty() || !values.is_empty());
    properties
}

/// Find the one exact operation marker, prove its path incarnation, full result
/// tree/properties/mergeinfo and current branch history. Repository HEAD may
/// advance on other paths. Neither HEAD nor input_revision + 1 identifies the
/// publication. Local observation uses a fresh, non-reused checkout directory.
/// Identity, approval, credential freshness and shared holds remain prerequisites
/// enforced by the later durable caller; this function performs no remote write.
pub async fn observe_prepared_child_svn(
    child: &SvnClient,
    parent: &SvnClient,
    git_path: &Path,
    observation_root: &Path,
    prepared: &PreparedCleanChildSvn,
) -> Result<SvnPublicationObservation, SvnError> {
    let pins = &prepared.pins.lineage.pins;
    let mergeinfo = expected_refresh_mergeinfo(
        &pins.child.parent_path,
        pins.child.parent_baseline_revision,
        pins.parent_revision,
        prepared.pins.previous_mergeinfo.as_deref(),
    )?;
    refresh_svn_commit_message(prepared.operation_id)?;
    if prepared.result_git_tree != prepared.pins.parent_git_tree || prepared.mergeinfo != mergeinfo
    {
        return Err(refused(
            "prepared publication receipt differs from its pins",
        ));
    }
    if inspect_svn_refresh_lineage(child, parent, pins).await? != prepared.pins.lineage {
        return Err(refused(
            "prepared lineage receipt differs from repository proof",
        ));
    }
    let mut expected_properties = parse_refresh_properties(
        &parent.tree_properties_at_rev(pins.parent_revision).await?,
        parent.url(),
    )?;
    expected_properties
        .entry(String::new())
        .or_default()
        .insert("svn:mergeinfo".into(), mergeinfo.clone());
    let expected_properties = normalized(expected_properties);
    if normalized(prepared.result_properties.clone()) != expected_properties {
        return Err(refused(
            "prepared properties differ from pinned parent and mergeinfo",
        ));
    }
    let current = child.info().await?;
    if current.uuid != pins.child.repository_uuid
        || current.root_url != pins.repository_root_url
        || current.url != pins.child_url
        || current.latest_rev < pins.child.child_revision
    {
        return Err(refused("publication repository or path identity drifted"));
    }
    let mut current_pins = pins.clone();
    current_pins.child.child_revision = current.latest_rev;
    let current_lineage = inspect_svn_refresh_lineage(child, parent, &current_pins).await?;
    let mut expected_copy = prepared.pins.lineage.child_copy.clone();
    expected_copy.inspected_revision = current.latest_rev;
    if current_lineage.child_copy != expected_copy {
        return Err(refused("publication child copy origin drifted"));
    }
    let history = child.copy_history_at_rev(current.latest_rev).await?;
    let Some(revision) = publication_revision(&history, prepared, current.latest_rev)? else {
        return Ok(SvnPublicationObservation::NotObserved);
    };
    let properties = normalized(parse_refresh_properties(
        &child.tree_properties_at_rev(revision).await?,
        child.url(),
    )?);
    if properties != expected_properties {
        return Err(refused("published properties differ from prepared result"));
    }
    let root = observation_root.canonicalize()?;
    let directory = root.join(format!(
        "observed-{}-{}",
        prepared.operation_id,
        Uuid::new_v4()
    ));
    std::fs::create_dir(&directory)?;
    let working_copy = directory.join("child");
    child
        .checkout_pinned_without_externals(&working_copy, revision)
        .await?;
    verify_refresh_svn_tree(
        git_path,
        &prepared.result_git_tree,
        &working_copy,
        &properties,
        Some(&mergeinfo),
        &pins.child.parent_path,
    )?;
    // Root info alone cannot identify the operation or its complete path set.
    // Re-read branch history to account for every post-approval child revision.
    let latest = child.info().await?;
    if latest.uuid != current.uuid
        || latest.root_url != current.root_url
        || latest.url != current.url
        || latest.latest_rev < current.latest_rev
        || publication_revision(
            &child.copy_history_at_rev(latest.latest_rev).await?,
            prepared,
            latest.latest_rev,
        )? != Some(revision)
    {
        return Err(refused("child changed while observing publication"));
    }
    expected_copy.inspected_revision = revision;
    Ok(SvnPublicationObservation::Observed(Box::new(
        SvnRefreshPublicationReceipt {
            operation_id: prepared.operation_id,
            revision,
            child_url: pins.child_url.clone(),
            child_copy: expected_copy,
            git_tree: prepared.result_git_tree.clone(),
            mergeinfo,
            properties,
        },
    )))
}
