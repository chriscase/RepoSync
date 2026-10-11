//! Pegged SVN copy proof for the bounded first pair-refresh case.

use serde::{Deserialize, Serialize};

use crate::errors::SvnError;
use crate::svn::SvnClient;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvnLineagePins {
    pub repository_uuid: String,
    pub child_path: String,
    pub parent_path: String,
    pub child_revision: i64,
    pub child_mapping_revision: i64,
    pub parent_baseline_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvnCopyProof {
    pub repository_uuid: String,
    pub child_path: String,
    pub created_revision: i64,
    pub parent_path: String,
    pub copy_from_revision: i64,
    pub inspected_revision: i64,
}

/// Complete location and revision pins for the two-path lineage proof. These
/// fields are part of the later execution approval, including both clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvnRefreshLineagePins {
    pub child: SvnLineagePins,
    pub repository_root_url: String,
    pub child_url: String,
    pub parent_url: String,
    pub parent_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvnRefreshLineageProof {
    pub pins: SvnRefreshLineagePins,
    pub child_copy: SvnCopyProof,
    pub parent_originating_revision: i64,
}

fn refused(detail: &str) -> SvnError {
    SvnError::LineageRefused {
        reason: "unproved_pair_refresh_svn_lineage".into(),
        detail: detail.into(),
    }
}

fn canonical_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && path.split('/').skip(1).all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

/// Structural validation for durable evidence. This does not replace fresh
/// remote inspection, but contradictory or missing evidence cannot hold scopes.
pub(crate) fn validate_stored_refresh_proof(
    proof: &SvnRefreshLineageProof,
) -> Result<(), SvnError> {
    let pins = &proof.pins;
    let child = &pins.child;
    let copy = &proof.child_copy;
    let root = reqwest::Url::parse(&pins.repository_root_url)
        .map_err(|_| refused("invalid pinned repository root URL"))?;
    if child.repository_uuid.is_empty()
        || !canonical_path(&child.child_path)
        || !canonical_path(&child.parent_path)
        || child.child_path == child.parent_path
        || child.child_mapping_revision < 1
        || child.child_mapping_revision > child.child_revision
        || child.parent_baseline_revision < 1
        || pins.parent_revision < child.parent_baseline_revision
        || copy.repository_uuid != child.repository_uuid
        || copy.child_path != child.child_path
        || copy.parent_path != child.parent_path
        || copy.copy_from_revision < 1
        || copy.copy_from_revision > child.parent_baseline_revision
        || copy.created_revision <= copy.copy_from_revision
        || copy.created_revision > child.child_mapping_revision
        || copy.inspected_revision != child.child_revision
        || proof.parent_originating_revision < 1
        || proof.parent_originating_revision > copy.copy_from_revision
        || pins.repository_root_url.ends_with('/')
        || root.as_str() != pins.repository_root_url
        || !root.username().is_empty()
        || root.password().is_some()
        || root.query().is_some()
        || root.fragment().is_some()
        || pins.child_url != format!("{}{}", pins.repository_root_url, child.child_path)
        || pins.parent_url != format!("{}{}", pins.repository_root_url, child.parent_path)
    {
        return Err(refused("invalid or contradictory stored lineage proof"));
    }
    Ok(())
}

fn affects_path(changed: &str, target: &str) -> bool {
    changed == "/"
        || changed == target
        || target
            .strip_prefix(changed)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// Prove a direct copy from the configured parent and bind its incarnation to
/// the already verified child mapping. Replacements, parent-directory copies,
/// renamed paths and unsupported path spellings are refused in this first case.
pub async fn inspect_svn_copy_origin(
    client: &SvnClient,
    pins: &SvnLineagePins,
) -> Result<SvnCopyProof, SvnError> {
    if pins.repository_uuid.is_empty()
        || !canonical_path(&pins.child_path)
        || !canonical_path(&pins.parent_path)
        || pins.child_path == pins.parent_path
        || pins.child_mapping_revision < 1
        || pins.parent_baseline_revision < 1
        || pins.child_mapping_revision > pins.child_revision
    {
        return Err(refused("invalid lineage pins"));
    }
    let info = client.info_at_rev(pins.child_revision).await?;
    if info.uuid != pins.repository_uuid
        || info.latest_rev != pins.child_revision
        || info.url.strip_prefix(info.root_url.trim_end_matches('/'))
            != Some(pins.child_path.as_str())
    {
        return Err(refused("pegged UUID or path differs from approval"));
    }
    let history = client.copy_history_at_rev(pins.child_revision).await?;
    let mut previous_revision = None;
    let mut origin = None;
    for entry in &history {
        if entry.revision < 1
            || entry.revision > pins.child_revision
            || previous_revision.is_some_and(|previous| entry.revision >= previous)
        {
            return Err(refused("copy history is incomplete or unordered"));
        }
        previous_revision = Some(entry.revision);
        for change in &entry.changed_paths {
            if !affects_path(&change.path, &pins.child_path) {
                continue;
            }
            match change.action.as_str() {
                "M" if change.copy_from_path.is_none() && change.copy_from_rev.is_none() => {}
                "A" if change.path == pins.child_path => {
                    if origin.is_some() {
                        return Err(refused("multiple path origins"));
                    }
                    let source_revision = change
                        .copy_from_rev
                        .filter(|revision| {
                            *revision >= 1
                                && *revision < entry.revision
                                && *revision <= pins.parent_baseline_revision
                        })
                        .ok_or_else(|| refused("missing or incompatible copy-from revision"))?;
                    if change.copy_from_path.as_deref() != Some(pins.parent_path.as_str())
                        || entry.revision > pins.child_mapping_revision
                    {
                        return Err(refused("copy source or incarnation differs from baseline"));
                    }
                    origin = Some(SvnCopyProof {
                        repository_uuid: pins.repository_uuid.clone(),
                        child_path: pins.child_path.clone(),
                        created_revision: entry.revision,
                        parent_path: pins.parent_path.clone(),
                        copy_from_revision: source_revision,
                        inspected_revision: pins.child_revision,
                    });
                }
                _ => return Err(refused("replaced, renamed or unsupported path history")),
            }
        }
    }
    let origin = origin.ok_or_else(|| refused("originating direct copy not found"))?;
    if history.last().map(|entry| entry.revision) != Some(origin.created_revision) {
        return Err(refused("history did not stop at the originating copy"));
    }
    Ok(origin)
}

/// Prove that the configured parent at the child's copy-from revision remains
/// the same path incarnation through the mapped baseline and pinned source tip.
/// Content changes beneath the parent are allowed; replacing or renaming the
/// parent or an enclosing directory after the copy source is refused.
pub async fn inspect_svn_refresh_lineage(
    child_client: &SvnClient,
    parent_client: &SvnClient,
    pins: &SvnRefreshLineagePins,
) -> Result<SvnRefreshLineageProof, SvnError> {
    let root = reqwest::Url::parse(&pins.repository_root_url)
        .map_err(|_| refused("invalid pinned repository root URL"))?;
    if pins.repository_root_url.ends_with('/')
        || !root.username().is_empty()
        || root.password().is_some()
        || root.query().is_some()
        || root.fragment().is_some()
        || pins.parent_revision < pins.child.parent_baseline_revision
        || child_client.url() != pins.child_url
        || parent_client.url() != pins.parent_url
        || pins.child_url != format!("{}{}", pins.repository_root_url, pins.child.child_path)
        || pins.parent_url != format!("{}{}", pins.repository_root_url, pins.child.parent_path)
    {
        return Err(refused(
            "lineage clients or revision range differ from approval",
        ));
    }
    let child_copy = inspect_svn_copy_origin(child_client, &pins.child).await?;
    let child_info = child_client.info_at_rev(pins.child.child_revision).await?;
    if child_info.root_url != pins.repository_root_url || child_info.url != pins.child_url {
        return Err(refused("child repository location differs from approval"));
    }
    for revision in [
        child_copy.copy_from_revision,
        pins.child.parent_baseline_revision,
        pins.parent_revision,
    ] {
        let info = parent_client.info_at_rev(revision).await?;
        if info.uuid != pins.child.repository_uuid
            || info.root_url != pins.repository_root_url
            || info.url != pins.parent_url
            || info.latest_rev != revision
        {
            return Err(refused(
                "parent UUID or pegged location differs from approval",
            ));
        }
    }
    let history = parent_client
        .copy_history_at_rev(pins.parent_revision)
        .await?;
    let mut previous_revision = None;
    let mut originating_revision = None;
    for entry in &history {
        if entry.revision < 1
            || entry.revision > pins.parent_revision
            || previous_revision.is_some_and(|previous| entry.revision >= previous)
        {
            return Err(refused("parent history is incomplete or unordered"));
        }
        previous_revision = Some(entry.revision);
        for change in &entry.changed_paths {
            if !affects_path(&change.path, &pins.child.parent_path) {
                continue;
            }
            match change.action.as_str() {
                "M" if change.copy_from_path.is_none() && change.copy_from_rev.is_none() => {}
                "A" if entry.revision <= child_copy.copy_from_revision => {
                    if originating_revision.is_some_and(|revision| revision != entry.revision) {
                        return Err(refused("multiple parent path origins"));
                    }
                    originating_revision = Some(entry.revision);
                }
                _ => {
                    return Err(refused(
                        "parent path incarnation changed after the copy source",
                    ))
                }
            }
        }
    }
    let parent_originating_revision =
        originating_revision.ok_or_else(|| refused("parent path origin not found"))?;
    if history.last().map(|entry| entry.revision) != Some(parent_originating_revision) {
        return Err(refused("parent history did not stop at its path origin"));
    }
    Ok(SvnRefreshLineageProof {
        pins: pins.clone(),
        child_copy,
        parent_originating_revision,
    })
}
