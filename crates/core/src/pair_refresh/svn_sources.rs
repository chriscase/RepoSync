//! Observe both registered SVN inputs without treating directory metadata as
//! proof that there is no pending work beneath the branch.

use serde::Serialize;
use uuid::Uuid;

use crate::errors::SvnError;
use crate::svn::{SvnClient, SvnInfo};

use super::execution_configuration::LiveExecutionConfiguration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedSvnSourcePins {
    pub repository_uuid: String,
    pub repository_root_url: String,
    pub inspected_head_revision: i64,
    pub child_url: String,
    pub parent_url: String,
    pub child_path: String,
    pub parent_path: String,
    pub child_checkpoint_revision: i64,
    pub parent_checkpoint_revision: i64,
}

/// Private authentication directories remain alive with these clients.
/// The snapshot proves input locations/history only, not lineage or trees.
pub struct SvnSourceSnapshot {
    pins: ObservedSvnSourcePins,
    child: SvnClient,
    parent: SvnClient,
}

impl SvnSourceSnapshot {
    pub(crate) fn reconstruct(
        candidate: &crate::db::pair_refresh_operations::VerifiedRecoveryCandidate,
        configuration: &LiveExecutionConfiguration,
    ) -> Result<Self, SvnError> {
        candidate
            .require_native_proof()
            .map_err(|_| refused("native recovery proof changed"))?;
        let operation = candidate.operation();
        let lineage = &operation.pins.svn.pins.lineage.pins;
        let visibility = candidate
            .original_visibility()
            .map_err(|_| refused("stored visibility is unavailable"))?;
        let heads = visibility
            .iter()
            .filter(|pin| matches!(pin.role.as_str(), "current_child" | "current_parent"))
            .map(|pin| pin.revision)
            .collect::<Vec<_>>();
        if visibility.len() != 5 || heads.len() != 2 || heads[0] != heads[1] {
            return Err(refused("stored source HEAD is contradictory"));
        }
        let (child, parent) = configuration.isolated_svn_clients()?;
        Ok(Self {
            child,
            parent,
            pins: ObservedSvnSourcePins {
                repository_uuid: lineage.child.repository_uuid.clone(),
                repository_root_url: lineage.repository_root_url.clone(),
                inspected_head_revision: heads[0],
                child_url: lineage.child_url.clone(),
                parent_url: lineage.parent_url.clone(),
                child_path: lineage.child.child_path.clone(),
                parent_path: lineage.child.parent_path.clone(),
                child_checkpoint_revision: operation.pins.child.last_svn_rev,
                parent_checkpoint_revision: operation.pins.parent.last_svn_rev,
            },
        })
    }

    pub fn pins(&self) -> &ObservedSvnSourcePins {
        &self.pins
    }
    pub fn child_client(&self) -> &SvnClient {
        &self.child
    }
    pub fn parent_client(&self) -> &SvnClient {
        &self.parent
    }
}

fn refused(detail: &str) -> SvnError {
    SvnError::LineageRefused {
        reason: "unproved_registered_svn_inputs".into(),
        detail: detail.into(),
    }
}

fn path(info: &SvnInfo, expected_url: &str) -> Result<String, SvnError> {
    let root = reqwest::Url::parse(&info.root_url)
        .map_err(|_| refused("invalid observed SVN repository root"))?;
    if info.url != expected_url
        || root.as_str() != info.root_url
        || info.root_url.ends_with('/')
        || info.latest_rev < 1
        || !Uuid::parse_str(&info.uuid)
            .is_ok_and(|uuid| !uuid.is_nil() && uuid.to_string() == info.uuid)
    {
        return Err(refused(
            "observed SVN location or identity differs from registered input",
        ));
    }
    let path = info
        .url
        .strip_prefix(&info.root_url)
        .filter(|path| path.starts_with('/') && path.len() > 1 && !path.ends_with('/'))
        .ok_or_else(|| refused("registered SVN branch is outside the observed repository root"))?;
    if !path.split('/').skip(1).all(|segment| {
        !matches!(segment, "" | "." | "..")
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    }) {
        return Err(refused("unsupported observed SVN branch path"));
    }
    Ok(path.to_owned())
}

fn related(changed: &str, branch: &str) -> bool {
    changed == "/"
        || changed == branch
        || changed
            .strip_prefix(branch)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || branch
            .strip_prefix(changed)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

async fn require_checkpoint_current(
    client: &SvnClient,
    info: &SvnInfo,
    branch: &str,
    checkpoint: i64,
) -> Result<(), SvnError> {
    if checkpoint < 1 || checkpoint > info.latest_rev {
        return Err(refused(
            "registered SVN checkpoint lies outside observed history",
        ));
    }
    let pegged = client.info_at_rev(checkpoint).await?;
    if pegged.uuid != info.uuid
        || pegged.root_url != info.root_url
        || pegged.url != info.url
        || pegged.latest_rev != checkpoint
    {
        return Err(refused(
            "pegged SVN checkpoint location or identity changed",
        ));
    }
    // A directory's last-changed revision can miss file/property changes in its
    // subtree. Inspect the complete path incarnation at the pinned HEAD instead.
    let history = client.copy_history_at_rev(info.latest_rev).await?;
    if history.is_empty() {
        return Err(refused("registered SVN branch history is unavailable"));
    }
    let mut previous = None;
    for entry in history {
        if entry.revision < 1
            || entry.revision > info.latest_rev
            || previous.is_some_and(|prior| entry.revision >= prior)
            || !entry
                .changed_paths
                .iter()
                .any(|change| related(&change.path, branch))
        {
            return Err(refused(
                "registered SVN history is incomplete or contradictory",
            ));
        }
        if entry.revision > checkpoint {
            return Err(refused("registered SVN branch has pending changes"));
        }
        previous = Some(entry.revision);
    }
    Ok(())
}

/// Read only explicit registered endpoints using isolated credentials. Both
/// branch histories must be handled through their stored checkpoints. Unrelated
/// repository revisions may advance HEAD. Fresh revalidation at publication
/// intent, copy lineage, correspondence and complete tree proof are separate.
pub async fn observe_registered_svn_sources(
    configuration: &LiveExecutionConfiguration,
) -> Result<SvnSourceSnapshot, SvnError> {
    let (child, parent) = configuration.isolated_svn_clients()?;
    let pin = configuration.pin();
    let child_info = child.info().await?;
    let parent_info = parent.info().await?;
    let child_path = path(&child_info, &pin.child_svn_url)?;
    let parent_path = path(&parent_info, &pin.parent_svn_url)?;
    if child_info.uuid != parent_info.uuid
        || child_info.root_url != parent_info.root_url
        || child_info.latest_rev != parent_info.latest_rev
        || child_path == parent_path
    {
        return Err(refused(
            "registered SVN inputs do not share one pinned repository snapshot",
        ));
    }
    require_checkpoint_current(&child, &child_info, &child_path, pin.child.last_svn_rev).await?;
    require_checkpoint_current(&parent, &parent_info, &parent_path, pin.parent.last_svn_rev)
        .await?;
    for (client, initial) in [(&child, &child_info), (&parent, &parent_info)] {
        let latest = client.info().await?;
        if latest.uuid != initial.uuid
            || latest.root_url != initial.root_url
            || latest.url != initial.url
            || latest.latest_rev != initial.latest_rev
        {
            return Err(refused("SVN repository changed during input observation"));
        }
    }
    Ok(SvnSourceSnapshot {
        pins: ObservedSvnSourcePins {
            repository_uuid: child_info.uuid,
            repository_root_url: child_info.root_url,
            inspected_head_revision: child_info.latest_rev,
            child_url: pin.child_svn_url.clone(),
            parent_url: pin.parent_svn_url.clone(),
            child_path,
            parent_path,
            child_checkpoint_revision: pin.child.last_svn_rev,
            parent_checkpoint_revision: pin.parent.last_svn_rev,
        },
        child,
        parent,
    })
}
