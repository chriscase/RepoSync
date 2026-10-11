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

fn refused(detail: &str) -> SvnError {
    SvnError::XmlParseError(format!("unproved pair refresh SVN lineage: {detail}"))
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

fn affects_path(changed: &str, target: &str) -> bool {
    changed == target
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
