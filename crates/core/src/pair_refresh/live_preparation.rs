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

/// Selector-free, conservative candidate review. Its private construction adds
/// exactly-one authenticated stored candidate to M2b's observed role facts;
/// semantic applicability, correspondence and execution remain unverified.
pub struct LiveChangedChildBaselineCandidateContext {
    candidate: crate::db::pair_refresh_operations::ChangedChildBaselineCandidateReview,
    preview: LiveChangedChildPreviewContext,
    context_digest: String,
}

impl LiveChangedChildBaselineCandidateContext {
    pub fn candidate_review(
        &self,
    ) -> &crate::db::pair_refresh_operations::ChangedChildBaselineCandidateReview {
        &self.candidate
    }

    pub fn preview_context(&self) -> &LiveChangedChildPreviewContext {
        &self.preview
    }

    pub fn context_digest(&self) -> &str {
        &self.context_digest
    }

    pub fn execution_eligible(&self) -> bool {
        false
    }

    pub fn verified_gates(&self) -> &'static [&'static str] {
        &[
            "single_authenticated_receipt_candidate",
            "registered_source_preview_recapture",
            "final_candidate_set_recapture",
        ]
    }

    pub fn unverified_gates(&self) -> &'static [&'static str] {
        self.preview.unverified_gates()
    }
}

/// No historical UUID, candidate list, source snapshot or digest is accepted
/// from the caller. All successful facts originate at registered DB/observers.
/// Existing selected-history and production preparation paths stay unchanged.
pub async fn capture_registered_changed_child_baseline_candidate_context(
    db: &Database,
    child_id: &str,
) -> Result<LiveChangedChildBaselineCandidateContext> {
    capture_changed_child_baseline_candidate_with_sources(db, child_id, None, |_| Ok(())).await
}

// Reuse the private M2b observer/fault seam. The callback supplies no authority
// and is only used by this module's disposable tests at the recapture boundary.
async fn capture_changed_child_baseline_candidate_with_sources<F>(
    db: &Database,
    child_id: &str,
    client: Option<reqwest::Client>,
    after_sources: F,
) -> Result<LiveChangedChildBaselineCandidateContext>
where
    F: FnOnce(&Database) -> Result<()>,
{
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let candidate = db.capture_changed_child_baseline_candidate_review(&configuration)?;
    let selected = candidate.selected_review();
    let preview = capture_changed_child_preview_with_sources(
        db,
        child_id,
        selected.historical_operation().id,
        client,
        after_sources,
    )
    .await?;
    if preview.current_configuration() != configuration.pin()
        || preview.historical_review().snapshot_digest() != selected.snapshot_digest()
    {
        bail!("baseline candidate changed before registered preview capture");
    }
    let current = db.capture_pair_refresh_execution_configuration(child_id)?;
    let repeated = db.capture_changed_child_baseline_candidate_review(&current)?;
    if current.pin() != configuration.pin()
        || repeated.candidate_set_digest() != candidate.candidate_set_digest()
    {
        bail!("authenticated baseline candidate set changed during preview");
    }
    use sha2::Digest;
    let context_digest = hex::encode(sha2::Sha256::digest(serde_json::to_vec(&(
        "changed_child_baseline_candidate_context_v1",
        candidate.candidate_set_digest(),
        preview.context_digest(),
        current.pin(),
    ))?));
    Ok(LiveChangedChildBaselineCandidateContext {
        candidate,
        preview,
        context_digest,
    })
}

/// Safe metadata from a fully inspected role, not a publication capability.
#[derive(serde::Serialize)]
pub struct ChangedChildSourceRoleProof {
    role: String,
    git_commit: String,
    git_tree: String,
    handled_revision: i64,
    presence: super::svn_visibility::SvnPresencePin,
    properties: super::svn_preparation::SvnPropertySnapshot,
    expected_mergeinfo: Option<String>,
}

/// Read-only source correspondence. Real private checkouts and their clients
/// stay owned with the result. No public constructor, Clone or Deserialize.
pub struct LiveChangedChildSourceCorrespondence {
    candidate: LiveChangedChildBaselineCandidateContext,
    mappings: crate::db::pair_refresh_operations::ChangedChildMappingReview,
    snapshots: Vec<super::svn_visibility::VisibleSvnCheckout>,
    roles: Vec<ChangedChildSourceRoleProof>,
    context_digest: String,
}

impl LiveChangedChildSourceCorrespondence {
    pub fn candidate_context(&self) -> &LiveChangedChildBaselineCandidateContext {
        &self.candidate
    }

    pub fn mapping_review(&self) -> &crate::db::pair_refresh_operations::ChangedChildMappingReview {
        &self.mappings
    }

    pub fn role_proofs(&self) -> &[ChangedChildSourceRoleProof] {
        &self.roles
    }

    pub fn context_digest(&self) -> &str {
        &self.context_digest
    }

    pub fn retained_snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    pub fn execution_eligible(&self) -> bool {
        false
    }

    pub fn verified_gates(&self) -> &'static [&'static str] {
        &[
            "single_authenticated_receipt_candidate",
            "current_scoped_applied_mapping_capture",
            "four_role_complete_visible_svn_correspondence",
            "supported_direct_and_empty_ancestor_properties",
            "handled_current_source_checkpoints",
            "final_mapping_candidate_configuration_and_source_recapture",
        ]
    }

    pub fn unverified_gates(&self) -> &'static [&'static str] {
        &[
            "semantic_baseline_applicability_beyond_singleton_subset",
            "complete_arbitrary_pending_work_enumeration",
            "authenticated_changed_child_resolution",
            "current_actor_session_authorization",
            "exclusive_changed_child_execution_admission",
            "atomic_remote_freshness_after_return",
        ]
    }
}

struct ChangedChildCorrespondenceRole<'a> {
    name: &'static str,
    git_commit: &'a str,
    git_tree: &'a str,
    revision: i64,
    handled_revision: i64,
    expected_mergeinfo: Option<&'a str>,
    client: &'a crate::svn::SvnClient,
}

async fn observe_changed_child_correspondence_role(
    git_path: &std::path::Path,
    expected: &super::svn_sources::ObservedSvnSourcePins,
    role: ChangedChildCorrespondenceRole<'_>,
) -> Result<(
    super::svn_visibility::VisibleSvnCheckout,
    ChangedChildSourceRoleProof,
)> {
    let snapshot = super::svn_visibility::observe_subtree(role.client, role.revision).await?;
    snapshot.require_unchanged_since(role.handled_revision)?;
    let info = snapshot.info();
    if info.uuid != expected.repository_uuid
        || info.root_url != expected.repository_root_url
        || info.url != role.client.url()
        || info.latest_rev != role.revision
    {
        bail!("changed-child complete SVN role identity differs");
    }
    let remote_xml = role.client.tree_properties_at_rev(role.revision).await?;
    let properties = super::svn_preparation::bound_checkout_property_xml(
        &remote_xml,
        role.client.url(),
        snapshot.path(),
        info,
    )?;
    let local =
        super::svn_preparation::visible_checkout_properties(role.client, snapshot.path(), info)
            .await?;
    let meaningful = |mut values: super::svn_preparation::SvnPropertySnapshot| {
        values.retain(|path, properties| path.is_empty() || !properties.is_empty());
        values
    };
    if meaningful(local) != meaningful(properties.clone()) {
        bail!("changed-child local and remote property proofs differ");
    }
    super::svn_preparation::verify_refresh_svn_tree(
        git_path,
        role.git_tree,
        snapshot.path(),
        &properties,
        role.expected_mergeinfo,
        &expected.parent_path,
    )?;
    let mut presence = super::svn_visibility::presence_pin(role.name, snapshot.path(), info)?;
    presence.ancestors =
        super::svn_visibility::require_empty_visible_ancestors(role.client, info).await?;
    let proof = ChangedChildSourceRoleProof {
        role: role.name.to_owned(),
        git_commit: role.git_commit.to_owned(),
        git_tree: role.git_tree.to_owned(),
        handled_revision: role.handled_revision,
        presence,
        properties,
        expected_mergeinfo: role.expected_mergeinfo.map(str::to_owned),
    };
    Ok((snapshot, proof))
}

/// No caller-supplied history, mapping, tree, property or expected digest.
/// This adds correspondence to a review context; no default caller is wired.
pub async fn capture_registered_changed_child_source_correspondence(
    db: &Database,
    child_id: &str,
) -> Result<LiveChangedChildSourceCorrespondence> {
    capture_changed_child_correspondence_with_sources(db, child_id, None, |_| Ok(())).await
}

// Private fixture CA and post-proof fault seam, never an injected observation
// or a bypass of DB, WC31, property, ancestry or registered-source validation.
async fn capture_changed_child_correspondence_with_sources<F>(
    db: &Database,
    child_id: &str,
    client: Option<reqwest::Client>,
    after_proofs: F,
) -> Result<LiveChangedChildSourceCorrespondence>
where
    F: FnOnce(&Database) -> Result<()>,
{
    let candidate =
        capture_changed_child_baseline_candidate_with_sources(db, child_id, client.clone(), |_| {
            Ok(())
        })
        .await?;
    let mappings = db.capture_changed_child_mapping_review(
        &candidate.preview.configuration,
        &candidate.candidate,
    )?;
    let current_pin = candidate.preview.current_configuration().clone();
    let historical = candidate.preview.history.historical_operation().clone();
    let receipt = historical
        .svn_receipt
        .as_ref()
        .context("authenticated correspondence receipt is missing")?;
    let svn = candidate.preview.svn.pins().clone();
    let git_path = candidate.preview.git.repository().path().to_owned();
    let head = svn.inspected_head_revision;
    let parent = candidate.preview.svn.parent_client();
    let child = candidate.preview.svn.child_client();
    let mut snapshots = Vec::new();
    let mut roles = Vec::new();
    for role in [
        ChangedChildCorrespondenceRole {
            name: "P1",
            git_commit: &candidate.preview.roles.parent_anchor,
            git_tree: &candidate.preview.roles.parent_anchor_tree,
            revision: historical.pins.parent.last_svn_rev,
            handled_revision: historical.pins.parent.last_svn_rev,
            expected_mergeinfo: None,
            client: parent,
        },
        ChangedChildCorrespondenceRole {
            name: "R",
            git_commit: &candidate.preview.roles.integrated_child,
            git_tree: &candidate.preview.roles.integrated_child_tree,
            revision: receipt.revision,
            handled_revision: receipt.revision,
            expected_mergeinfo: Some(&receipt.mergeinfo),
            client: child,
        },
        ChangedChildCorrespondenceRole {
            name: "C",
            git_commit: &candidate.preview.roles.current_child,
            git_tree: &candidate.preview.roles.current_child_tree,
            revision: head,
            handled_revision: current_pin.child.last_svn_rev,
            expected_mergeinfo: Some(&receipt.mergeinfo),
            client: child,
        },
        ChangedChildCorrespondenceRole {
            name: "P2",
            git_commit: &candidate.preview.roles.current_parent,
            git_tree: &candidate.preview.roles.current_parent_tree,
            revision: head,
            handled_revision: current_pin.parent.last_svn_rev,
            expected_mergeinfo: None,
            client: parent,
        },
    ] {
        let (snapshot, proof) =
            observe_changed_child_correspondence_role(&git_path, &svn, role).await?;
        snapshots.push(snapshot);
        roles.push(proof);
    }
    after_proofs(db)?;
    let current = db.capture_pair_refresh_execution_configuration(child_id)?;
    if current.pin() != &current_pin {
        bail!("changed-child correspondence configuration changed");
    }
    let (current, repeated_git) = changed_child_git_observation(current, client).await?;
    let repeated_svn = observe_registered_svn_sources(&current).await?;
    if repeated_git.pins() != candidate.preview.git.pins()
        || repeated_svn.pins() != candidate.preview.svn.pins()
    {
        bail!("changed-child correspondence registered sources changed");
    }
    let final_configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let final_candidate =
        db.capture_changed_child_baseline_candidate_review(&final_configuration)?;
    let final_mappings =
        db.capture_changed_child_mapping_review(&final_configuration, &final_candidate)?;
    if final_configuration.pin() != &current_pin
        || final_candidate.candidate_set_digest() != candidate.candidate.candidate_set_digest()
        || final_mappings.snapshot_digest() != mappings.snapshot_digest()
    {
        bail!("changed-child correspondence stored evidence changed");
    }
    use sha2::Digest;
    let context_digest = hex::encode(sha2::Sha256::digest(serde_json::to_vec(&(
        "changed_child_source_correspondence_v1",
        candidate.context_digest(),
        mappings.snapshot_digest(),
        &roles,
        &current_pin,
    ))?));
    Ok(LiveChangedChildSourceCorrespondence {
        candidate,
        mappings,
        snapshots,
        roles,
        context_digest,
    })
}

/// A pinned read-only manifest, never an authenticated resolution or job. The
/// source capability is retained separately and cannot be reconstructed from it.
#[derive(serde::Serialize)]
pub struct ChangedChildApplicabilityManifest {
    pub schema: u8,
    pub policy: String,
    pub source_correspondence_digest: String,
    pub source_role_proofs_digest: String,
    pub candidate_set_digest: String,
    pub historical_snapshot_digest: String,
    pub mapping_snapshot_digest: String,
    pub configuration: super::execution_configuration::ExecutionConfigurationPin,
    pub observed_git: super::git_sources::ObservedGitSourcePins,
    pub observed_svn: super::svn_sources::ObservedSvnSourcePins,
    pub graph: super::preparation::ChangedChildApplicableGraphReview,
    pub unverified: Vec<String>,
    pub execution_eligible: bool,
    pub manifest_digest: String,
}

/// Private construction, no Clone/Deserialize. The actual source observations,
/// authenticated history, native mapping capture and four checkouts stay owned.
pub struct LiveChangedChildApplicabilityReview {
    source: LiveChangedChildSourceCorrespondence,
    manifest: ChangedChildApplicabilityManifest,
}

impl LiveChangedChildApplicabilityReview {
    pub fn manifest(&self) -> &ChangedChildApplicabilityManifest {
        &self.manifest
    }

    pub fn source_correspondence(&self) -> &LiveChangedChildSourceCorrespondence {
        &self.source
    }

    pub fn execution_eligible(&self) -> bool {
        false
    }

    /// Compare a review pin only after fresh capture. This accepts no choices,
    /// actor assertion or serialized manifest, and does not grant execution.
    pub fn require_manifest_digest(&self, expected: &str) -> Result<()> {
        if expected != self.manifest.manifest_digest {
            bail!("changed-child applicability review pin is stale or different");
        }
        Ok(())
    }
}

fn changed_child_applicability_digest<T: serde::Serialize>(
    domain: &str,
    value: &T,
) -> Result<String> {
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(serde_json::to_vec(&(
        domain, value,
    ))?)))
}

fn inspect_changed_child_applicability_source(
    source: &LiveChangedChildSourceCorrespondence,
) -> Result<ChangedChildApplicabilityManifest> {
    use super::preparation::{
        ChangedChildApplicabilityRoles, ChangedChildEvidence, ChangedChildGitInput,
        ChangedChildProjection, ChangedChildReviewContext, ChangedChildSvnContext,
    };

    let preview = &source.candidate.preview;
    let configuration = preview.current_configuration();
    let history = preview.historical_review();
    let historical = history.historical_operation();
    let svn = preview.svn_sources();
    let expected = &preview.roles;
    if source.roles.len() != 4 || source.snapshots.len() != 4 {
        bail!("changed-child applicability requires all four retained source roles");
    }
    for (proof, (name, commit, tree)) in source.roles.iter().zip([
        ("P1", &expected.parent_anchor, &expected.parent_anchor_tree),
        (
            "R",
            &expected.integrated_child,
            &expected.integrated_child_tree,
        ),
        ("C", &expected.current_child, &expected.current_child_tree),
        (
            "P2",
            &expected.current_parent,
            &expected.current_parent_tree,
        ),
    ]) {
        if proof.role != name || &proof.git_commit != commit || &proof.git_tree != tree {
            bail!("changed-child applicability source-role partition differs");
        }
    }
    let lineage_digest =
        changed_child_applicability_digest("changed_child_lineage_v1", &preview.lineage)?;
    let roles_digest =
        changed_child_applicability_digest("changed_child_correspondence_roles_v1", &source.roles)?;
    let context = ChangedChildReviewContext {
        child: configuration.child.clone(),
        parent: configuration.parent.clone(),
        git_remote_url: configuration.git_remote_url.clone(),
        child_git_branch: configuration.child_git_branch.clone(),
        parent_git_branch: configuration.parent_git_branch.clone(),
        transport_policy: configuration.transport_policy,
        svn: ChangedChildSvnContext {
            repository_uuid: svn.repository_uuid.clone(),
            repository_root_url: svn.repository_root_url.clone(),
            child_path: svn.child_path.clone(),
            parent_path: svn.parent_path.clone(),
            child_revision: configuration.child.last_svn_rev,
            parent_revision: configuration.parent.last_svn_rev,
            baseline_revision: history.historical_parent_anchor().1,
            inspected_head_revision: svn.inspected_head_revision,
            lineage_evidence: Some(ChangedChildEvidence {
                reference: format!("registered-svn-lineage:{}", historical.id),
                sha256: lineage_digest,
            }),
            property_evidence: Some(ChangedChildEvidence {
                reference: "complete-four-role-correspondence".into(),
                sha256: roles_digest.clone(),
            }),
        },
        baseline_evidence: Some(ChangedChildEvidence {
            reference: format!("authenticated-refresh-receipt:{}", historical.id),
            sha256: history.snapshot_digest().to_owned(),
        }),
        // Stored execution policy is not the current actor's authorization.
        actor_policy: None,
        credential_context_digest: Some(configuration.credential_context_digest.clone()),
        // capture_execution_configuration has already independently refused
        // projection and LFS; these values do not come from a preview caller.
        projection: ChangedChildProjection {
            policy: "full_tree_no_lfs_v1".into(),
            allowed_paths: Vec::new(),
            blocked_patterns: Vec::new(),
            lfs_threshold_mb: 0,
        },
    };
    let roles = ChangedChildApplicabilityRoles {
        parent_anchor: ChangedChildGitInput {
            commit: expected.parent_anchor.clone(),
            tree: expected.parent_anchor_tree.clone(),
        },
        integrated_child: ChangedChildGitInput {
            commit: expected.integrated_child.clone(),
            tree: expected.integrated_child_tree.clone(),
        },
        current_child: ChangedChildGitInput {
            commit: expected.current_child.clone(),
            tree: expected.current_child_tree.clone(),
        },
        current_parent: ChangedChildGitInput {
            commit: expected.current_parent.clone(),
            tree: expected.current_parent_tree.clone(),
        },
    };
    let graph = super::preparation::inspect_changed_child_applicable_graph(
        preview.git.repository(),
        &historical.pins.git,
        &roles,
        &context,
    )?;
    let mut manifest = ChangedChildApplicabilityManifest {
        schema: 1,
        policy: super::preparation::CHANGED_CHILD_APPLICABILITY_POLICY.into(),
        source_correspondence_digest: source.context_digest().to_owned(),
        source_role_proofs_digest: roles_digest,
        candidate_set_digest: source.candidate.candidate.candidate_set_digest().to_owned(),
        historical_snapshot_digest: history.snapshot_digest().to_owned(),
        mapping_snapshot_digest: source.mappings.snapshot_digest().to_owned(),
        configuration: configuration.clone(),
        observed_git: preview.git_sources().clone(),
        observed_svn: svn.clone(),
        graph,
        unverified: [
            "complete_arbitrary_pending_work_and_local_bridge_enumeration",
            "authenticated_changed_child_resolution",
            "current_actor_session_authorization",
            "exclusive_changed_child_execution_admission",
            "atomic_remote_freshness_after_return",
            "other_graphs_properties_projection_and_reanchor",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        execution_eligible: false,
        manifest_digest: String::new(),
    };
    manifest.manifest_digest =
        changed_child_applicability_digest("changed_child_applicability_manifest_v1", &manifest)?;
    Ok(manifest)
}

/// Factory-only inspection for exactly the documented retained-merge/linear-side
/// subset. No supplied receipt UUID, baseline, mapping, manifest or choices.
/// All default preparation/execution callers remain unchanged.
pub async fn capture_registered_changed_child_applicability_review(
    db: &Database,
    child_id: &str,
) -> Result<LiveChangedChildApplicabilityReview> {
    capture_changed_child_applicability_with_sources(db, child_id, None, |_| Ok(())).await
}

// Existing private CA plus a post-inspection fault boundary; not a source-fact
// injection seam. Production supplies neither the client nor callback.
async fn capture_changed_child_applicability_with_sources<F>(
    db: &Database,
    child_id: &str,
    client: Option<reqwest::Client>,
    after_inspection: F,
) -> Result<LiveChangedChildApplicabilityReview>
where
    F: FnOnce(&Database) -> Result<()>,
{
    let source =
        capture_changed_child_correspondence_with_sources(db, child_id, client.clone(), |_| Ok(()))
            .await?;
    let manifest = inspect_changed_child_applicability_source(&source)?;
    after_inspection(db)?;
    // Analysis of immutable objects is not an ongoing source lease. Repeat the
    // actual endpoints and final DB closure before returning the pinned review.
    let current = db.capture_pair_refresh_execution_configuration(child_id)?;
    if current.pin() != source.candidate.preview.current_configuration() {
        bail!("changed-child applicability configuration changed");
    }
    let (current, repeated_git) = changed_child_git_observation(current, client).await?;
    let repeated_svn = observe_registered_svn_sources(&current).await?;
    if repeated_git.pins() != source.candidate.preview.git_sources()
        || repeated_svn.pins() != source.candidate.preview.svn_sources()
    {
        bail!("changed-child applicability source visibility or tips changed");
    }
    let final_configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let final_candidate =
        db.capture_changed_child_baseline_candidate_review(&final_configuration)?;
    let final_mapping =
        db.capture_changed_child_mapping_review(&final_configuration, &final_candidate)?;
    if final_configuration.pin() != source.candidate.preview.current_configuration()
        || final_candidate.candidate_set_digest()
            != source.candidate.candidate.candidate_set_digest()
        || final_mapping.snapshot_digest() != source.mappings.snapshot_digest()
    {
        bail!("changed-child applicability stored provenance changed");
    }
    Ok(LiveChangedChildApplicabilityReview { source, manifest })
}

/// An unauthenticated choice candidate bound to a fresh applicability manifest.
/// These serializable bytes are neither the live source nor an approved job.
#[derive(serde::Serialize)]
pub struct ChangedChildResolutionCandidateManifest {
    pub schema: u8,
    pub policy: String,
    pub applicability_manifest_digest: String,
    pub source_correspondence_digest: String,
    pub graph: super::preparation::ChangedChildResolvedGraphCandidate,
    pub unverified: Vec<String>,
    pub execution_eligible: bool,
    pub candidate_digest: String,
}

/// Keeps the actual source observations alive. No public construction, Clone
/// or Deserialize; mutable caller JSON cannot replace this owned capture.
pub struct LiveChangedChildResolutionCandidateReview {
    applicability: LiveChangedChildApplicabilityReview,
    manifest: ChangedChildResolutionCandidateManifest,
}

impl LiveChangedChildResolutionCandidateReview {
    pub fn applicability_review(&self) -> &LiveChangedChildApplicabilityReview {
        &self.applicability
    }

    pub fn manifest(&self) -> &ChangedChildResolutionCandidateManifest {
        &self.manifest
    }

    pub fn execution_eligible(&self) -> bool {
        false
    }
}

/// Accept choices as untrusted intent. Only a fresh opaque capture and exact
/// manifest/triple comparisons bind a result; no actor approval is accepted.
/// This does not publish a Git/SVN result, write a job or change default callers.
pub async fn capture_registered_changed_child_resolution_candidate_review(
    db: &Database,
    child_id: &str,
    request: &super::preparation::ChangedChildResolutionCandidateRequest,
) -> Result<LiveChangedChildResolutionCandidateReview> {
    capture_changed_child_resolution_candidate_with_sources(db, child_id, request, None, |_| Ok(()))
        .await
}

// Private fixture CA and post-resolution fault seam. Neither supplies a source
// observation, manifest, selected baseline or execution/actor capability.
async fn capture_changed_child_resolution_candidate_with_sources<F>(
    db: &Database,
    child_id: &str,
    request: &super::preparation::ChangedChildResolutionCandidateRequest,
    client: Option<reqwest::Client>,
    after_resolution: F,
) -> Result<LiveChangedChildResolutionCandidateReview>
where
    F: FnOnce(&Database) -> Result<()>,
{
    // Malformed headers refuse before any new registered-source read. The DTO
    // and its nested choices/entries deny unknown fields and unknown choices.
    super::preparation::require_changed_child_resolution_request_header(request)?;
    let applicability =
        capture_changed_child_applicability_with_sources(db, child_id, client.clone(), |_| Ok(()))
            .await?;
    applicability.require_manifest_digest(&request.manifest_digest)?;
    let graph = {
        let captured = applicability.manifest();
        super::preparation::resolve_changed_child_applicable_graph(
            applicability.source.candidate.preview.git.repository(),
            &captured.graph.historical_merge,
            &captured.graph.roles,
            &captured.graph.context,
            &captured.graph.inspection_digest,
            &request.choices,
        )?
    };
    let mut unverified = applicability.manifest().unverified.clone();
    unverified.push("resolution_choices_not_actor_authorized".into());
    unverified.push("candidate_has_no_durable_execution_or_svn_result".into());
    let mut manifest = ChangedChildResolutionCandidateManifest {
        schema: 1,
        policy: super::preparation::CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY.into(),
        applicability_manifest_digest: applicability.manifest().manifest_digest.clone(),
        source_correspondence_digest: applicability
            .manifest()
            .source_correspondence_digest
            .clone(),
        graph,
        unverified,
        execution_eligible: false,
        candidate_digest: String::new(),
    };
    manifest.candidate_digest = changed_child_applicability_digest(
        "changed_child_live_resolution_candidate_v1",
        &manifest,
    )?;
    after_resolution(db)?;

    // Selection/hash computation does not extend the observation interval or
    // lock a remote. Repeat actual endpoints and final DB closure once more.
    let current = db.capture_pair_refresh_execution_configuration(child_id)?;
    if current.pin()
        != applicability
            .source
            .candidate
            .preview
            .current_configuration()
    {
        bail!("changed-child resolution candidate configuration changed");
    }
    let (current, repeated_git) = changed_child_git_observation(current, client).await?;
    let repeated_svn = observe_registered_svn_sources(&current).await?;
    if repeated_git.pins() != applicability.source.candidate.preview.git_sources()
        || repeated_svn.pins() != applicability.source.candidate.preview.svn_sources()
    {
        bail!("changed-child resolution candidate source visibility or tips changed");
    }
    let final_configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let final_candidate =
        db.capture_changed_child_baseline_candidate_review(&final_configuration)?;
    let final_mapping =
        db.capture_changed_child_mapping_review(&final_configuration, &final_candidate)?;
    if final_configuration.pin()
        != applicability
            .source
            .candidate
            .preview
            .current_configuration()
        || final_candidate.candidate_set_digest()
            != applicability
                .source
                .candidate
                .candidate
                .candidate_set_digest()
        || final_mapping.snapshot_digest() != applicability.source.mappings.snapshot_digest()
    {
        bail!("changed-child resolution candidate stored provenance changed");
    }
    Ok(LiveChangedChildResolutionCandidateReview {
        applicability,
        manifest,
    })
}

// Authenticated HTTP inspection intake. These are observations, not capabilities.
// The existing strict factories above still decide whether a graph is supported.
const REVIEW_OBSERVATION_LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewGitPending {
    pub checkpoint: String,
    pub observed_tip: Option<String>,
    pub complete: bool,
    pub pending_count: Option<usize>,
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewSvnPending {
    pub checkpoint: i64,
    pub observed_head: Option<i64>,
    pub observed_pending_revisions: Vec<i64>,
    pub complete: bool,
    pub blocked: Option<String>,
    /// An empty visible log is not this evidence. This pin comes only from
    /// the existing isolated, full WC31 presence/ancestor verification.
    pub visibility: Option<super::svn_visibility::SvnPresencePin>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewLocalPath {
    /// Git path bytes, never interpreted as a command or a filesystem selector.
    pub path: Vec<u8>,
    pub status: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewLocalPending {
    pub observed_tip: Option<String>,
    pub paths: Vec<ReviewLocalPath>,
    pub complete: bool,
    pub blocked: Option<String>,
    pub snapshot_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewPendingSide {
    pub repository_id: String,
    pub git: ReviewGitPending,
    pub svn: ReviewSvnPending,
    pub local_bridge: ReviewLocalPending,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegisteredReviewIntake {
    pub schema: u8,
    pub configuration: super::execution_configuration::ExecutionConfigurationPin,
    pub child: ReviewPendingSide,
    pub parent: ReviewPendingSide,
    pub observation_digest: String,
}

impl RegisteredReviewIntake {
    pub fn blocked_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        for (role, side) in [("child", &self.child), ("parent", &self.parent)] {
            for (source, complete, reason) in [
                ("git", side.git.complete, side.git.blocked.as_deref()),
                ("svn", side.svn.complete, side.svn.blocked.as_deref()),
                (
                    "local_bridge",
                    side.local_bridge.complete,
                    side.local_bridge.blocked.as_deref(),
                ),
            ] {
                if let Some(reason) = reason {
                    reasons.push(format!("{role}.{source}.{reason}"));
                } else if !complete {
                    reasons.push(format!("{role}.{source}.unknown"));
                }
            }
        }
        reasons
    }

    /// Permission to attempt the existing strict source factory, not to return
    /// a result. Empty visible history remains incomplete until that succeeds.
    pub(super) fn may_attempt_correspondence(&self) -> bool {
        [&self.child, &self.parent].iter().all(|side| {
            side.git.complete
                && side.git.blocked.is_none()
                && side.local_bridge.complete
                && side.local_bridge.blocked.is_none()
                && !side.svn.complete
                && side.svn.visibility.is_none()
                && side.svn.observed_head.is_some()
                && side.svn.observed_pending_revisions.is_empty()
                && side.svn.blocked.as_deref() == Some("subtree_visibility_unproved")
        })
    }

    /// Only this live factory result can promote per-role completeness. A
    /// serialized pin, consistent log or client assertion is not a substitute.
    pub(super) fn bind_correspondence_presence(
        &mut self,
        review: &LiveChangedChildApplicabilityReview,
    ) -> Result<()> {
        anyhow::ensure!(
            self.may_attempt_correspondence(),
            "review intake is not ready for source proof"
        );
        let manifest = review.manifest();
        anyhow::ensure!(
            manifest.configuration == self.configuration,
            "review source configuration changed"
        );
        let expected = self.configuration.clone();
        for (role, side, url, tip) in [
            (
                "C",
                &mut self.child,
                &expected.child_svn_url,
                &expected.child.last_git_sha,
            ),
            (
                "P2",
                &mut self.parent,
                &expected.parent_svn_url,
                &expected.parent.last_git_sha,
            ),
        ] {
            let proofs = review
                .source_correspondence()
                .roles
                .iter()
                .filter(|proof| proof.role == role)
                .collect::<Vec<_>>();
            let [proof] = proofs.as_slice() else {
                bail!("review role presence is ambiguous");
            };
            let pin = &proof.presence;
            anyhow::ensure!(
                proof.git_commit == *tip
                    && proof.handled_revision == side.svn.checkpoint
                    && side.git.observed_tip.as_deref() == Some(tip.as_str())
                    && pin.role == role
                    && pin.profile == "svn_wc31_full_subtree_presence_v1"
                    && pin.url == *url
                    && pin.repository_uuid == manifest.observed_svn.repository_uuid
                    && pin.repository_root_url == manifest.observed_svn.repository_root_url
                    && Some(pin.revision) == side.svn.observed_head
                    && pin.revision == manifest.observed_svn.inspected_head_revision
                    && pin.max_changed_revision <= side.svn.checkpoint
                    && !pin.ancestors.is_empty(),
                "review role presence differs from the observed interval"
            );
            side.svn.visibility = Some(pin.clone());
            side.svn.complete = true;
            side.svn.blocked = None;
        }
        self.observation_digest.clear();
        self.observation_digest =
            changed_child_applicability_digest("registered_review_intake_v1", self)?;
        Ok(())
    }
}

fn review_git_unknown(checkpoint: &str) -> ReviewGitPending {
    ReviewGitPending {
        checkpoint: checkpoint.to_owned(),
        observed_tip: None,
        complete: false,
        pending_count: None,
        blocked: Some("observation_unavailable".into()),
    }
}

fn review_local_unknown() -> ReviewLocalPending {
    ReviewLocalPending {
        observed_tip: None,
        paths: Vec::new(),
        complete: false,
        blocked: Some("identity_or_work_unknown".into()),
        snapshot_digest: None,
    }
}

/// Advertisements only: no fetch into a shared bridge, no ref/FETCH_HEAD write.
/// A different tip is reported, not guessed to be an applicable handled mapping.
fn review_advertised_git(
    configuration: &LiveExecutionConfiguration,
    client: &reqwest::Client,
    child: bool,
) -> Result<ReviewGitPending> {
    let pin = configuration.pin();
    let (branch, checkpoint, token) = if child {
        (
            &pin.child_git_branch,
            &pin.child.last_git_sha,
            configuration.credentials().child_git_token(),
        )
    } else {
        (
            &pin.parent_git_branch,
            &pin.parent.last_git_sha,
            configuration.credentials().parent_git_token(),
        )
    };
    let scratch = tempfile::tempdir()?;
    let mut options = git2::RepositoryInitOptions::new();
    options.bare(true).external_template(false).no_reinit(true);
    let repository = git2::Repository::init_opts(scratch.path(), &options)?;
    super::publication::require_pinned_push_endpoint_with_policy(
        &repository,
        &pin.git_remote_url,
        super::publication::PinnedPushPolicy::SmartHttpV1,
    )?;
    let transport =
        super::http_transport::HttpTransport::new(&pin.git_remote_url, token, client.clone())?;
    let mut remote =
        super::publication::checked_anonymous_remote(&repository, &transport.endpoint)?;
    let connection = remote.connect_auth(
        git2::Direction::Fetch,
        Some(super::publication::no_credentials()),
        None,
    )?;
    let reference = format!("refs/heads/{branch}");
    let tips = connection
        .list()?
        .iter()
        .filter(|head| head.name() == reference)
        .map(|head| head.oid().to_string())
        .collect::<Vec<_>>();
    anyhow::ensure!(tips.len() == 1, "registered review branch unavailable");
    let tip = tips[0].clone();
    let current = tip == *checkpoint;
    Ok(ReviewGitPending {
        checkpoint: checkpoint.clone(),
        observed_tip: Some(tip),
        complete: current,
        pending_count: current.then_some(0),
        blocked: (!current).then(|| "unhandled_or_rewritten_tip_count_unknown".into()),
    })
}

/// Inspect only the daemon's registered bridge location. Missing/linked/foreign
/// bridges are unknown, never evidence that local work can be discarded.
fn review_local_bridge(
    root: &std::path::Path,
    id: &str,
    branch: &str,
    remote_url: &str,
    remote_tip: Option<&str>,
) -> Result<ReviewLocalPending> {
    anyhow::ensure!(
        !id.is_empty()
            && !matches!(id, "." | "..")
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "unsupported review bridge identity"
    );
    let root = root.canonicalize()?;
    let mut path = root.clone();
    for part in ["repos", id, "git-repo", ".git"] {
        path.push(part);
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "review bridge link or directory changed"
        );
    }
    let bridge = path.parent().context("review bridge parent missing")?;
    let repository = git2::Repository::open(bridge)?;
    anyhow::ensure!(
        !repository.is_bare()
            && !repository.is_worktree()
            && repository.path().canonicalize()? == path
            && repository
                .workdir()
                .context("review bridge worktree missing")?
                .canonicalize()?
                == bridge,
        "review bridge is not the registered worktree"
    );
    super::publication::require_complete_git_graph(&repository)?;
    anyhow::ensure!(
        repository.state() == git2::RepositoryState::Clean,
        "review bridge has an unfinished operation"
    );
    anyhow::ensure!(
        repository.find_remote("origin")?.url() == Some(remote_url),
        "review bridge origin differs"
    );
    let head = repository.head()?;
    let expected_ref = format!("refs/heads/{branch}");
    anyhow::ensure!(
        !repository.head_detached()? && head.name() == Some(expected_ref.as_str()),
        "review bridge branch differs"
    );
    let branches = repository
        .branches(Some(git2::BranchType::Local))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        branches.len() == 1,
        "additional local branch work is unknown"
    );
    let tip = head
        .target()
        .context("review bridge tip unavailable")?
        .to_string();
    let index_path = path.join("index");
    let (index_before, _) = read_review_regular_file(&index_path, 16 * 1024 * 1024)?;
    let index = repository.index()?;
    anyhow::ensure!(
        index
            .path()
            .context("review index location unknown")?
            .canonicalize()?
            == index_path,
        "review index is not the registered index"
    );
    for key in [
        "core.sparseCheckout",
        "core.sparseCheckoutCone",
        "index.sparse",
    ] {
        match repository.config()?.get_bool(key) {
            Ok(false) => {}
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            _ => bail!("sparse or unsupported local index policy"),
        }
    }
    anyhow::ensure!(
        !index.has_conflicts()
            && index.len() <= REVIEW_OBSERVATION_LIMIT
            && index
                .iter()
                .all(|entry| review_supported_index_entry(&entry)),
        "local index has suppressed, sparse, unmerged or unsupported entries"
    );
    require_review_tracked_bytes(bridge, &index)?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(true)
        .include_unreadable(true)
        .no_refresh(true)
        .update_index(false);
    let statuses = repository.statuses(Some(&mut options))?;
    anyhow::ensure!(
        statuses.len() <= REVIEW_OBSERVATION_LIMIT,
        "review local work exceeds bounded inventory"
    );
    let mut paths = statuses
        .iter()
        .map(|entry| ReviewLocalPath {
            path: entry.path_bytes().to_vec(),
            status: entry.status().bits(),
        })
        .collect::<Vec<_>>();
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    let clean = paths.is_empty() && remote_tip == Some(tip.as_str());
    // Neither an unchanged index nor an empty status list proves tracked bytes.
    // Re-read actual regular files without filters/stat-cache shortcuts, then
    // require that the exact shared index also remained unchanged. No repair.
    require_review_tracked_bytes(bridge, &index)?;
    let (index_bytes, _) = read_review_regular_file(&index_path, 16 * 1024 * 1024)?;
    anyhow::ensure!(index_bytes == index_before, "review shared index changed");
    let snapshot_digest = changed_child_applicability_digest(
        "registered_review_local_bridge_v1",
        &(&tip, &paths, &index_bytes),
    )?;
    Ok(ReviewLocalPending {
        observed_tip: Some(tip),
        paths,
        complete: true,
        blocked: (!clean).then(|| "local_work_or_tip_differs".into()),
        snapshot_digest: Some(snapshot_digest),
    })
}

fn review_supported_index_entry(entry: &git2::IndexEntry) -> bool {
    entry.flags & (git2::IndexEntryFlag::VALID.bits() | 0x3000) == 0
        && entry.flags_extended & !git2::IndexEntryExtendedFlag::UPTODATE.bits() == 0
        && matches!(entry.mode, 0o100644 | 0o100755)
}

fn read_review_regular_file(
    path: &std::path::Path,
    limit: usize,
) -> Result<(Vec<u8>, std::fs::Metadata)> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "review input is not a regular file"
    );
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW);
    }
    let file = open.open(path)?;
    let before = file.metadata()?;
    anyhow::ensure!(before.is_file(), "review input file changed");
    let mut bytes = Vec::new();
    {
        use std::io::Read;
        (&file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    }
    let after = file.metadata()?;
    anyhow::ensure!(
        bytes.len() <= limit && after.len() == bytes.len() as u64,
        "review input exceeds bound or changed length"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let identity = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        anyhow::ensure!(
            identity(&metadata) == identity(&before) && identity(&before) == identity(&after),
            "review input identity changed"
        );
    }
    Ok((bytes, after))
}

fn require_review_tracked_bytes(bridge: &std::path::Path, index: &git2::Index) -> Result<()> {
    let mut remaining = 16 * 1024 * 1024;
    for entry in index.iter() {
        let name = std::str::from_utf8(&entry.path)?;
        anyhow::ensure!(
            !name.is_empty()
                && !name.contains(['\\', '\0'])
                && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
                && matches!(entry.mode, 0o100644 | 0o100755),
            "unsupported review tracked path or mode"
        );
        let mut path = bridge.to_owned();
        let mut parts = name.split('/').peekable();
        while let Some(part) = parts.next() {
            path.push(part);
            if parts.peek().is_some() {
                let metadata = std::fs::symlink_metadata(&path)?;
                anyhow::ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "review tracked directory changed"
                );
            }
        }
        let (bytes, metadata) = read_review_regular_file(&path, remaining)?;
        remaining -= bytes.len();
        anyhow::ensure!(
            git2::Oid::hash_object(git2::ObjectType::Blob, &bytes)? == entry.id,
            "tracked worktree bytes differ from the index"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                (metadata.permissions().mode() & 0o100 != 0) == (entry.mode == 0o100755),
                "tracked executable mode differs"
            );
        }
        #[cfg(not(unix))]
        let _ = metadata;
    }
    Ok(())
}

async fn review_svn_pending(
    client: &crate::svn::SvnClient,
    expected_url: &str,
    checkpoint: i64,
) -> Result<ReviewSvnPending> {
    let info = client.info().await?;
    let pegged = client.info_at_rev(checkpoint).await?;
    anyhow::ensure!(
        checkpoint > 0
            && info.latest_rev >= checkpoint
            && info.url == expected_url
            && pegged.url == info.url
            && pegged.uuid == info.uuid
            && pegged.root_url == info.root_url
            && pegged.latest_rev == checkpoint,
        "review SVN checkpoint identity differs"
    );
    let path = info
        .url
        .strip_prefix(&info.root_url)
        .context("review SVN path missing")?;
    anyhow::ensure!(
        path.starts_with('/') && path.len() > 1,
        "review SVN path invalid"
    );
    let history = client.copy_history_at_rev(info.latest_rev).await?;
    anyhow::ensure!(
        !history.is_empty() && history.len() <= REVIEW_OBSERVATION_LIMIT,
        "review SVN history unavailable or exceeds bound"
    );
    let mut previous = None;
    let mut pending = Vec::new();
    for entry in history {
        anyhow::ensure!(
            entry.revision > 0
                && entry.revision <= info.latest_rev
                && previous.is_none_or(|prior| entry.revision < prior)
                && entry.changed_paths.iter().any(|change| {
                    change.path == "/"
                        || change.path == path
                        || change
                            .path
                            .strip_prefix(path)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                        || path
                            .strip_prefix(change.path.as_str())
                            .is_some_and(|suffix| suffix.starts_with('/'))
                }),
            "review SVN history contradictory"
        );
        previous = Some(entry.revision);
        if entry.revision > checkpoint {
            pending.push(entry.revision);
        }
    }
    let repeated = client.info().await?;
    anyhow::ensure!(
        repeated.url == info.url
            && repeated.uuid == info.uuid
            && repeated.root_url == info.root_url
            && repeated.latest_rev == info.latest_rev,
        "review SVN observation changed"
    );
    Ok(ReviewSvnPending {
        checkpoint,
        observed_head: Some(info.latest_rev),
        complete: false,
        blocked: if !pending.is_empty() {
            Some("visible_unhandled_revisions_remaining_visibility_unproven".into())
        } else {
            Some("subtree_visibility_unproved".into())
        },
        observed_pending_revisions: pending,
        visibility: None,
    })
}

/// The actor/engine wrapper supplies the engine's data root, never an HTTP path.
/// Errors are converted into fixed unknown codes, not leaked credential/path text.
pub(super) async fn capture_registered_review_intake(
    db: &Database,
    child_id: &str,
    root: &std::path::Path,
    client: Option<reqwest::Client>,
) -> Result<RegisteredReviewIntake> {
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let initial_pin = configuration.pin().clone();
    let client = match client {
        Some(client) => client,
        None => super::git_sources::source_client()?,
    };
    let root = root.to_owned();
    let (configuration, child_git, parent_git, child_local, parent_local) =
        tokio::task::spawn_blocking(move || {
            let pin = configuration.pin();
            let child_git = review_advertised_git(&configuration, &client, true)
                .unwrap_or_else(|_| review_git_unknown(&pin.child.last_git_sha));
            let parent_git = review_advertised_git(&configuration, &client, false)
                .unwrap_or_else(|_| review_git_unknown(&pin.parent.last_git_sha));
            let child_local = review_local_bridge(
                &root,
                &pin.child.repo_id,
                &pin.child_git_branch,
                &pin.git_remote_url,
                child_git.observed_tip.as_deref(),
            )
            .unwrap_or_else(|_| review_local_unknown());
            let parent_local = review_local_bridge(
                &root,
                &pin.parent.repo_id,
                &pin.parent_git_branch,
                &pin.git_remote_url,
                parent_git.observed_tip.as_deref(),
            )
            .unwrap_or_else(|_| review_local_unknown());
            (
                configuration,
                child_git,
                parent_git,
                child_local,
                parent_local,
            )
        })
        .await
        .context("review input observation worker failed")?;
    let (child_client, parent_client) = configuration.isolated_svn_clients()?;
    let unknown = |checkpoint| ReviewSvnPending {
        checkpoint,
        observed_head: None,
        observed_pending_revisions: Vec::new(),
        complete: false,
        blocked: Some("visibility_or_history_unknown".into()),
        visibility: None,
    };
    let child_svn = review_svn_pending(
        &child_client,
        &initial_pin.child_svn_url,
        initial_pin.child.last_svn_rev,
    )
    .await
    .unwrap_or_else(|_| unknown(initial_pin.child.last_svn_rev));
    let parent_svn = review_svn_pending(
        &parent_client,
        &initial_pin.parent_svn_url,
        initial_pin.parent.last_svn_rev,
    )
    .await
    .unwrap_or_else(|_| unknown(initial_pin.parent.last_svn_rev));
    anyhow::ensure!(
        db.capture_pair_refresh_execution_configuration(child_id)?
            .pin()
            == &initial_pin,
        "registered review configuration changed"
    );
    let mut intake = RegisteredReviewIntake {
        schema: 1,
        child: ReviewPendingSide {
            repository_id: initial_pin.child.repo_id.clone(),
            git: child_git,
            svn: child_svn,
            local_bridge: child_local,
        },
        parent: ReviewPendingSide {
            repository_id: initial_pin.parent.repo_id.clone(),
            git: parent_git,
            svn: parent_svn,
            local_bridge: parent_local,
        },
        configuration: initial_pin,
        observation_digest: String::new(),
    };
    intake.observation_digest =
        changed_child_applicability_digest("registered_review_intake_v1", &intake)?;
    Ok(intake)
}

pub(super) fn capture_review_applicability<'a>(
    db: &'a Database,
    child: &'a str,
    client: Option<reqwest::Client>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<LiveChangedChildApplicabilityReview>> + 'a>,
> {
    // Store the large native observation future on the owned worker's heap;
    // do not duplicate it inline in each orchestration state machine.
    Box::pin(capture_changed_child_applicability_with_sources(
        db,
        child,
        client,
        |_| Ok(()),
    ))
}

pub(super) fn capture_review_resolution<'a>(
    db: &'a Database,
    child: &'a str,
    request: &'a super::preparation::ChangedChildResolutionCandidateRequest,
    client: Option<reqwest::Client>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<LiveChangedChildResolutionCandidateReview>> + 'a>,
> {
    Box::pin(capture_changed_child_resolution_candidate_with_sources(
        db,
        child,
        request,
        client,
        |_| Ok(()),
    ))
}
