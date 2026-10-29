//! Assemble independently observed inputs and both unreferenced/local results.
//! This preparation does not authorize an actor, admit a job or publish.

#[cfg(test)]
#[path = "authenticated_enrollment_tests.rs"]
mod authenticated_enrollment_tests;

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use crate::db::pair_refresh_operations::{PairRefreshOperation, RefreshJobPins};
use crate::db::Database;

use super::execution_configuration::LiveExecutionConfiguration;
use super::git_sources::{observe_registered_git_sources, GitSourceSnapshot};
use super::lineage::{
    inspect_svn_copy_origin, inspect_svn_refresh_lineage, SvnLineagePins, SvnRefreshLineagePins,
};
use super::preparation::{prepare_clean_child_git, CleanChildGitPins};
use super::svn_preparation::{prepare_clean_child_svn, CleanChildSvnPins};
use super::svn_sources::{observe_registered_svn_sources, SvnSourceSnapshot};
use super::workspace::RefreshWorkspace;

/// These resources belong only to this preparation. A durable execution caller
/// must arrange workspace retention/reconstruction before using the pins for
/// admission. Dropping this read-only preparation cleans its private resources.
/// There is intentionally no Debug/Serialize implementation for live clients.
pub struct LivePreparedRefresh {
    pins: RefreshJobPins,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    visibility: Option<Vec<super::svn_visibility::SvnPresencePin>>,
    _operation_directory: PreparationDirectory,
}

struct PreparationLocation {
    operation_id: Uuid,
    prepared_at: i64,
    workspace: Option<RefreshWorkspace>,
}

enum PreparationDirectory {
    Temporary(tempfile::TempDir),
    Workspace {
        path: std::path::PathBuf,
        owner: RefreshWorkspace,
    },
}

impl PreparationDirectory {
    fn path(&self) -> &std::path::Path {
        match self {
            Self::Temporary(directory) => directory.path(),
            Self::Workspace { path, .. } => path,
        }
    }
}

impl LivePreparedRefresh {
    pub(crate) fn reconstruct(
        candidate: &mut crate::db::pair_refresh_operations::VerifiedRecoveryCandidate,
        workspace: RefreshWorkspace,
    ) -> Result<Self> {
        candidate.require_native_proof()?;
        let configuration = candidate.take_configuration()?;
        let git = GitSourceSnapshot::reconstruct(candidate, &configuration, workspace.clone())?;
        let svn = SvnSourceSnapshot::reconstruct(candidate, &configuration)?;
        let visibility = candidate.original_visibility()?;
        let path = candidate
            .resources()
            .directories
            .get("svn")
            .context("retained SVN role is missing")?
            .path
            .clone();
        if candidate.operation().pins.svn.working_copy
            != candidate
                .resources()
                .directories
                .get("svn-working-copy")
                .context("retained working copy is missing")?
                .path
        {
            bail!("retained working copy differs from signed job");
        }
        workspace.require_current()?;
        Ok(Self {
            pins: candidate.operation().pins.clone(),
            configuration,
            git,
            svn,
            visibility: Some(visibility),
            _operation_directory: PreparationDirectory::Workspace {
                path,
                owner: workspace,
            },
        })
    }

    pub(crate) fn workspace(&self) -> Result<&RefreshWorkspace, crate::errors::DatabaseError> {
        match &self._operation_directory {
            PreparationDirectory::Workspace { owner, .. } => Ok(owner),
            PreparationDirectory::Temporary(_) => Err(crate::errors::DatabaseError::Other(
                "temporary preparation cannot enroll an authenticated job".into(),
            )),
        }
    }

    pub fn pins(&self) -> &RefreshJobPins {
        &self.pins
    }
    pub fn configuration(&self) -> &LiveExecutionConfiguration {
        &self.configuration
    }
    pub fn git(&self) -> &GitSourceSnapshot {
        &self.git
    }
    pub fn svn(&self) -> &SvnSourceSnapshot {
        &self.svn
    }

    pub fn visibility_pins(&self) -> Option<&[super::svn_visibility::SvnPresencePin]> {
        self.visibility.as_deref()
    }

    pub fn resource_pin(
        &self,
    ) -> Result<super::workspace::RefreshResourcePin, crate::errors::DatabaseError> {
        match &self._operation_directory {
            PreparationDirectory::Workspace { owner, .. } => owner.resource_pin(),
            PreparationDirectory::Temporary(_) => Err(crate::errors::DatabaseError::Other(
                "temporary preparation has no durable resource pin".into(),
            )),
        }
    }
}

/// Prepare the bounded first/next refresh from actual registered sources.
/// Read-only Git observation runs on a blocking thread, separately from the
/// asynchronous SVN commands. No shared worktree, ref or checkpoint changes.
pub async fn prepare_registered_pair_refresh(
    db: &Database,
    child_id: &str,
    operation_id: Uuid,
    prepared_at: i64,
) -> Result<LivePreparedRefresh> {
    if operation_id.is_nil() || prepared_at < 0 {
        bail!("pair refresh preparation requires a valid operation identity and time");
    }
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let baseline = db.capture_completed_pair_refresh_baseline(&configuration)?;
    let (configuration, git) = tokio::task::spawn_blocking(move || {
        let git = observe_registered_git_sources(&configuration)?;
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("pair refresh Git observation task failed")??;
    let svn = observe_registered_svn_sources(&configuration).await?;
    prepare_observed_sources(
        db,
        configuration,
        git,
        svn,
        baseline,
        operation_id,
        prepared_at,
    )
    .await
}

async fn prepare_observed_sources(
    db: &Database,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    baseline: Option<PairRefreshOperation>,
    operation_id: Uuid,
    prepared_at: i64,
) -> Result<LivePreparedRefresh> {
    prepare_observed_sources_at(
        db,
        configuration,
        git,
        svn,
        baseline,
        PreparationLocation {
            operation_id,
            prepared_at,
            workspace: None,
        },
    )
    .await
}

pub(crate) async fn prepare_registered_in_workspace(
    db: &Database,
    child_id: &str,
    workspace: RefreshWorkspace,
    prepared_at: i64,
) -> Result<LivePreparedRefresh> {
    prepare_registered_with_git_observer(
        db,
        child_id,
        workspace,
        prepared_at,
        super::git_sources::observe_registered_git_sources_in_workspace,
    )
    .await
}

pub(crate) async fn prepare_registered_in_workspace_with_client(
    db: &Database,
    child_id: &str,
    workspace: RefreshWorkspace,
    client: reqwest::Client,
) -> Result<LivePreparedRefresh> {
    prepare_registered_with_git_observer(
        db,
        child_id,
        workspace,
        chrono::Utc::now().timestamp(),
        move |configuration, workspace| {
            super::git_sources::observe_in_workspace_with_client(configuration, workspace, client)
        },
    )
    .await
}

async fn prepare_registered_with_git_observer(
    db: &Database,
    child_id: &str,
    workspace: RefreshWorkspace,
    prepared_at: i64,
    observer: impl FnOnce(
            &LiveExecutionConfiguration,
            RefreshWorkspace,
        ) -> Result<GitSourceSnapshot, crate::errors::GitError>
        + Send
        + 'static,
) -> Result<LivePreparedRefresh> {
    workspace.require_current()?;
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let baseline = db.capture_completed_pair_refresh_baseline(&configuration)?;
    let operation_id = workspace.pin().operation_id;
    let source_workspace = workspace.clone();
    let (configuration, git) = tokio::task::spawn_blocking(move || {
        let git = observer(&configuration, source_workspace)?;
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("owned Git preparation task failed")??;
    let svn = observe_registered_svn_sources(&configuration).await?;
    prepare_observed_sources_at(
        db,
        configuration,
        git,
        svn,
        baseline,
        PreparationLocation {
            operation_id,
            prepared_at,
            workspace: Some(workspace),
        },
    )
    .await
}

#[cfg(test)]
async fn prepare_registered_in_workspace_for_test(
    db: &Database,
    child_id: &str,
    workspace: RefreshWorkspace,
    client: reqwest::Client,
) -> Result<LivePreparedRefresh> {
    prepare_registered_with_git_observer(
        db,
        child_id,
        workspace,
        chrono::Utc::now().timestamp(),
        move |configuration, workspace| {
            super::git_sources::observe_registered_git_sources_in_workspace_for_test(
                configuration,
                workspace,
                client,
            )
        },
    )
    .await
}

async fn prepare_observed_sources_at(
    db: &Database,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    baseline: Option<PairRefreshOperation>,
    location: PreparationLocation,
) -> Result<LivePreparedRefresh> {
    let PreparationLocation {
        operation_id,
        prepared_at,
        workspace,
    } = location;
    if let Some(workspace) = workspace.as_ref() {
        workspace.require_current()?;
        if workspace.pin().operation_id != operation_id {
            bail!("workspace operation identity differs");
        }
    }
    let source = configuration.pin();
    let observed = svn.pins();
    let probe = SvnLineagePins {
        repository_uuid: observed.repository_uuid.clone(),
        child_path: observed.child_path.clone(),
        parent_path: observed.parent_path.clone(),
        child_revision: source.child.last_svn_rev,
        child_mapping_revision: source.child.last_svn_rev,
        parent_baseline_revision: source.parent.last_svn_rev,
    };
    let copy = inspect_svn_copy_origin(svn.child_client(), &probe).await?;
    let (baseline_sha, baseline_revision, previous_mergeinfo) = match baseline.as_ref() {
        Some(prior) => {
            let receipt = prior
                .svn_receipt
                .as_ref()
                .context("completed refresh baseline lacks its receipt")?;
            let mut expected_copy = receipt.child_copy.clone();
            expected_copy.inspected_revision = source.child.last_svn_rev;
            if copy != expected_copy {
                bail!("current copy origin differs from completed refresh evidence");
            }
            (
                prior.pins.parent.last_git_sha.clone(),
                prior.pins.parent.last_svn_rev,
                Some(receipt.mergeinfo.clone()),
            )
        }
        None => {
            // The first supported case has no SVN work after the original copy.
            // Its CURRENT scoped Git checkpoint must itself be a shared parent
            // commit. No historical human-ID mapping or arbitrary equal-tree
            // commit is selected as a replacement baseline.
            if source.child.last_svn_rev != copy.created_revision {
                bail!("first refresh requires the unchanged original SVN copy checkpoint");
            }
            (
                source.child.last_git_sha.clone(),
                copy.copy_from_revision,
                None,
            )
        }
    };
    let lineage = inspect_svn_refresh_lineage(
        svn.child_client(),
        svn.parent_client(),
        &SvnRefreshLineagePins {
            child: SvnLineagePins {
                parent_baseline_revision: baseline_revision,
                ..probe
            },
            repository_root_url: observed.repository_root_url.clone(),
            child_url: source.child_svn_url.clone(),
            parent_url: source.parent_svn_url.clone(),
            parent_revision: source.parent.last_svn_rev,
        },
    )
    .await?;
    if let Some(prior) = baseline.as_ref() {
        if lineage.parent_originating_revision
            != prior.pins.svn.pins.lineage.parent_originating_revision
        {
            bail!("parent incarnation differs from completed refresh evidence");
        }
    }
    let prepared_git = prepare_clean_child_git(
        git.repository(),
        &CleanChildGitPins {
            child_tip: source.child.last_git_sha.clone(),
            parent_tip: source.parent.last_git_sha.clone(),
            parent_baseline_tip: baseline_sha.clone(),
        },
        operation_id,
        prepared_at,
    )?;
    let baseline_tree = git
        .repository()
        .find_commit(git2::Oid::from_str(&baseline_sha)?)?
        .tree_id()
        .to_string();
    if let Some(prior) = baseline.as_ref() {
        if baseline_tree != prior.pins.git.tree_sha {
            bail!("completed baseline Git tree differs from its publication evidence");
        }
    }
    let operation_directory = match workspace {
        Some(owner) => PreparationDirectory::Workspace {
            path: owner.create_directory("svn")?,
            owner,
        },
        None => PreparationDirectory::Temporary(
            tempfile::Builder::new()
                .prefix("reposync-refresh-preparation-")
                .tempdir()?,
        ),
    };
    let svn_pins = CleanChildSvnPins {
        lineage,
        baseline_git_tree: baseline_tree,
        parent_git_tree: prepared_git.tree_sha.clone(),
        previous_mergeinfo,
    };
    let (prepared_svn, visibility) =
        if matches!(&operation_directory, PreparationDirectory::Workspace { .. }) {
            let (prepared, presence) = super::svn_preparation::prepare_clean_child_svn_visible(
                svn.child_client(),
                svn.parent_client(),
                git.repository().path(),
                operation_directory.path(),
                operation_id,
                &svn_pins,
                observed.inspected_head_revision,
            )
            .await?;
            (prepared, Some(presence))
        } else {
            (
                prepare_clean_child_svn(
                    svn.child_client(),
                    svn.parent_client(),
                    git.repository().path(),
                    operation_directory.path(),
                    operation_id,
                    &svn_pins,
                )
                .await?,
                None,
            )
        };
    if let PreparationDirectory::Workspace { owner, .. } = &operation_directory {
        owner.track_working_copy(&prepared_svn.working_copy)?;
    }
    let pins = RefreshJobPins {
        child: source.child.clone(),
        parent: source.parent.clone(),
        git_remote_url: source.git_remote_url.clone(),
        git_branch: source.child_git_branch.clone(),
        transport_policy: source.transport_policy,
        credential_context_digest: source.credential_context_digest.clone(),
        git: prepared_git,
        svn: prepared_svn,
    };
    // Preparation takes time. Drift or a newly admitted hold cannot be hidden
    // behind the earlier read transaction. Fresh remote checks at publication
    // intent, actor approval, effective policy and durable retention are still
    // mandatory responsibilities of the unfinished execution caller.
    let current = db.capture_pair_refresh_execution_configuration(&source.child.repo_id)?;
    if current.pin() != source || db.capture_completed_pair_refresh_baseline(&current)? != baseline
    {
        bail!("registered inputs or baseline evidence changed during preparation");
    }
    db.require_registered_pair_refresh_preparation(&current, &pins)?;
    if let PreparationDirectory::Workspace { owner, .. } = &operation_directory {
        owner.require_current()?;
    }
    Ok(LivePreparedRefresh {
        pins,
        configuration,
        git,
        svn,
        visibility,
        _operation_directory: operation_directory,
    })
}

#[cfg(test)]
#[path = "live_preparation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "workspace_preparation_tests.rs"]
mod workspace_tests;

/// Factory-only, read-only source context. This is neither an applicable-baseline
/// certificate nor an approved changed-child result. In particular, registered
/// checkpoints do not themselves prove Git/SVN content or mapping correspondence.
/// There is deliberately no public constructor, Clone, Serialize or Deserialize.
pub struct LiveChangedChildPreviewContext {
    history: crate::db::pair_refresh_operations::ChangedChildProvenanceReview,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    lineage: super::lineage::SvnRefreshLineageProof,
    roles: ChangedChildRoleFacts,
    context_digest: String,
}

#[derive(serde::Serialize)]
struct ChangedChildRoleFacts {
    parent_anchor: String,
    integrated_child: String,
    current_child: String,
    current_parent: String,
    parent_anchor_tree: String,
    integrated_child_tree: String,
    current_child_tree: String,
    current_parent_tree: String,
    parent_contains_integrated_child: bool,
}

impl LiveChangedChildPreviewContext {
    pub fn historical_review(
        &self,
    ) -> &crate::db::pair_refresh_operations::ChangedChildProvenanceReview {
        &self.history
    }

    pub fn current_configuration(
        &self,
    ) -> &super::execution_configuration::ExecutionConfigurationPin {
        self.configuration.pin()
    }

    pub fn git_sources(&self) -> &super::git_sources::ObservedGitSourcePins {
        self.git.pins()
    }

    pub fn svn_sources(&self) -> &super::svn_sources::ObservedSvnSourcePins {
        self.svn.pins()
    }

    pub fn svn_lineage(&self) -> &super::lineage::SvnRefreshLineageProof {
        &self.lineage
    }

    /// Tree identities in the fixed order P1, R, C, P2. Equal trees never stand
    /// in for a role's commit identity or for SVN correspondence.
    pub fn role_tree_ids(&self) -> [&str; 4] {
        [
            &self.roles.parent_anchor_tree,
            &self.roles.integrated_child_tree,
            &self.roles.current_child_tree,
            &self.roles.current_parent_tree,
        ]
    }

    /// This may legitimately be false: P2 advances from P1, not necessarily R.
    pub fn parent_contains_integrated_child(&self) -> bool {
        self.roles.parent_contains_integrated_child
    }

    /// Unkeyed identity of captured facts, not an actor's approval or a job pin.
    pub fn context_digest(&self) -> &str {
        &self.context_digest
    }

    pub fn execution_eligible(&self) -> bool {
        false
    }

    pub fn verified_gates(&self) -> &'static [&'static str] {
        &[
            "authenticated_stored_receipt",
            "current_registration_and_credential_capture",
            "registered_git_tip_observations",
            "historical_git_object_identity",
            "role_specific_git_ancestry",
            "registered_svn_identity_and_checkpoint_history",
            "svn_copy_and_parent_incarnation_continuity",
            "repeated_git_svn_observation_pins",
            "final_stored_context_recapture",
        ]
    }

    pub fn unverified_gates(&self) -> &'static [&'static str] {
        &[
            "unique_applicable_historical_baseline",
            "current_mapping_provenance",
            "svn_visibility_content_properties_mergeinfo_correspondence",
            "current_actor_session_authorization",
            "authenticated_resolution_freshness",
            "exclusive_changed_child_execution_admission",
            "atomic_remote_freshness_after_return",
        ]
    }
}

/// Capture sources for review using registered observers, never caller-supplied
/// tips, object repositories, mapping assertions or inspection digests. Reads
/// create private observation scratch and make configured Git/SVN read requests.
/// No prepared merge, checkout result, DB write, ref update or admission is made.
pub async fn capture_registered_changed_child_preview_context(
    db: &Database,
    child_id: &str,
    historical_operation_id: Uuid,
) -> Result<LiveChangedChildPreviewContext> {
    capture_changed_child_preview_with_sources(db, child_id, historical_operation_id, None, |_| {
        Ok(())
    })
    .await
}

async fn changed_child_git_observation(
    configuration: LiveExecutionConfiguration,
    client: Option<reqwest::Client>,
) -> Result<(LiveExecutionConfiguration, GitSourceSnapshot)> {
    tokio::task::spawn_blocking(move || {
        let git = match client {
            Some(client) => super::git_sources::observe_with_client(&configuration, client)?,
            None => observe_registered_git_sources(&configuration)?,
        };
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("changed-child Git source observation task failed")?
}

fn changed_child_role_facts(
    git: &GitSourceSnapshot,
    history: &crate::db::pair_refresh_operations::ChangedChildProvenanceReview,
) -> Result<ChangedChildRoleFacts> {
    let repository = git.repository();
    let operation = history.historical_operation();
    // Reuse the existing exact signed-object validator. Do not manufacture an
    // absent R or weaken the old clean-child policy that created this receipt.
    super::preparation::require_prepared_refresh_object(repository, &operation.pins.git)?;
    let p1 = git2::Oid::from_str(history.historical_parent_anchor().0)?;
    let r = git2::Oid::from_str(history.historical_child_anchor().0)?;
    let c = git2::Oid::from_str(&git.pins().child_tip)?;
    let p2 = git2::Oid::from_str(&git.pins().parent_tip)?;
    if (c != r && !repository.graph_descendant_of(c, r)?)
        || (p2 != p1 && !repository.graph_descendant_of(p2, p1)?)
    {
        bail!("changed-child sources do not descend from their distinct receipt roles");
    }
    let mut trees = Vec::new();
    for id in [p1, r, c, p2] {
        let tree = repository.find_commit(id)?.tree()?;
        super::preparation::require_supported_refresh_tree(repository, &tree)?;
        trees.push(tree.id().to_string());
    }
    if trees[0] != operation.pins.git.tree_sha || trees[1] != operation.pins.git.tree_sha {
        bail!("historical Git tree differs from its authenticated receipt");
    }
    Ok(ChangedChildRoleFacts {
        parent_anchor: p1.to_string(),
        integrated_child: r.to_string(),
        current_child: c.to_string(),
        current_parent: p2.to_string(),
        parent_anchor_tree: trees[0].clone(),
        integrated_child_tree: trees[1].clone(),
        current_child_tree: trees[2].clone(),
        current_parent_tree: trees[3].clone(),
        parent_contains_integrated_child: p2 == r || repository.graph_descendant_of(p2, r)?,
    })
}

// Private composition seam: production passes no injected client or callback.
// Tests use the existing private fixture CA and a fault at the recapture boundary;
// neither supplies a source snapshot or bypasses the registered observers.
async fn capture_changed_child_preview_with_sources<F>(
    db: &Database,
    child_id: &str,
    historical_operation_id: Uuid,
    client: Option<reqwest::Client>,
    after_sources: F,
) -> Result<LiveChangedChildPreviewContext>
where
    F: FnOnce(&Database) -> Result<()>,
{
    if historical_operation_id.is_nil() {
        bail!("changed-child review requires an explicit historical operation");
    }
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let history =
        db.capture_changed_child_provenance_review(&configuration, historical_operation_id)?;
    let source = configuration.pin().clone();
    let (configuration, git) = changed_child_git_observation(configuration, client.clone()).await?;
    let svn = observe_registered_svn_sources(&configuration).await?;
    let roles = changed_child_role_facts(&git, &history)?;
    let historical = history.historical_operation();
    let old_lineage = &historical.pins.svn.pins.lineage;
    let receipt = historical
        .svn_receipt
        .as_ref()
        .context("authenticated historical SVN receipt is missing")?;
    let observed = svn.pins();
    if observed.repository_uuid != old_lineage.pins.child.repository_uuid
        || observed.repository_root_url != old_lineage.pins.repository_root_url
        || observed.child_path != receipt.child_copy.child_path
        || observed.parent_path != receipt.child_copy.parent_path
        || observed.child_url != receipt.child_url
        || source.child.last_svn_rev < receipt.revision
        || source.parent.last_svn_rev < historical.pins.parent.last_svn_rev
    {
        bail!("current SVN source identity or role range differs from signed history");
    }
    let lineage = inspect_svn_refresh_lineage(
        svn.child_client(),
        svn.parent_client(),
        &SvnRefreshLineagePins {
            child: SvnLineagePins {
                repository_uuid: observed.repository_uuid.clone(),
                child_path: observed.child_path.clone(),
                parent_path: observed.parent_path.clone(),
                child_revision: source.child.last_svn_rev,
                child_mapping_revision: receipt.revision,
                parent_baseline_revision: historical.pins.parent.last_svn_rev,
            },
            repository_root_url: observed.repository_root_url.clone(),
            child_url: source.child_svn_url.clone(),
            parent_url: source.parent_svn_url.clone(),
            parent_revision: source.parent.last_svn_rev,
        },
    )
    .await?;
    let mut expected_copy = receipt.child_copy.clone();
    expected_copy.inspected_revision = source.child.last_svn_rev;
    if lineage.child_copy != expected_copy
        || lineage.parent_originating_revision != old_lineage.parent_originating_revision
    {
        bail!("current SVN incarnation differs from the historical receipt");
    }

    after_sources(db)?;
    // Re-observe actual endpoints after the asynchronous lineage reads. A DB
    // recapture alone cannot detect a ref or SVN HEAD change with unchanged rows.
    let (configuration, repeated_git) =
        changed_child_git_observation(configuration, client).await?;
    let repeated_svn = observe_registered_svn_sources(&configuration).await?;
    if repeated_git.pins() != git.pins() || repeated_svn.pins() != svn.pins() {
        bail!("registered sources changed during changed-child review");
    }
    let current = db.capture_pair_refresh_execution_configuration(child_id)?;
    let repeated_history =
        db.capture_changed_child_provenance_review(&current, historical_operation_id)?;
    if current.pin() != &source || repeated_history.snapshot_digest() != history.snapshot_digest() {
        bail!("registered context or authenticated history changed during changed-child review");
    }
    use sha2::Digest;
    let context_digest = hex::encode(sha2::Sha256::digest(serde_json::to_vec(&(
        "changed_child_preview_context_v1",
        history.snapshot_digest(),
        current.pin(),
        git.pins(),
        svn.pins(),
        &lineage,
        &roles,
    ))?));
    Ok(LiveChangedChildPreviewContext {
        history,
        configuration,
        git,
        svn,
        lineage,
        roles,
        context_digest,
    })
}
