//! Git object preparation for the bounded clean-child refresh case. The caller
//! must separately prove mappings, SVN state, credentials, identities and holds.

use git2::{ObjectType, Oid, Repository, Signature, Time, Tree, TreeWalkMode, TreeWalkResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::publication::require_complete_git_graph;
use crate::errors::GitError;
use crate::history_inspect::is_full_git_oid;

const MAX_PARENT_ADVANCE: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanChildGitPins {
    pub child_tip: String,
    pub parent_tip: String,
    pub parent_baseline_tip: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedCleanChildGit {
    pub pins: CleanChildGitPins,
    pub operation_id: Uuid,
    pub prepared_at: i64,
    pub prepared_sha: String,
    pub tree_sha: String,
    pub parent_advance_commits: usize,
}

fn refused(detail: &str) -> GitError {
    git2::Error::from_str(detail).into()
}

fn pin(raw: &str) -> Result<Oid, GitError> {
    if !is_full_git_oid(raw) {
        return Err(refused("pair refresh requires complete Git object IDs"));
    }
    Ok(Oid::from_str(raw)?)
}

/// Check pinned objects, not a mutable working copy or inherited attribute
/// configuration. This first case supports regular files and executable bits.
/// Attribute rules, LFS pointers, symlinks and submodules need a later policy.
pub fn require_supported_refresh_tree(repo: &Repository, tree: &Tree<'_>) -> Result<(), GitError> {
    let mut failure = None;
    tree.walk(TreeWalkMode::PreOrder, |_, entry| {
        let result = (|| {
            match (entry.kind(), entry.filemode()) {
                (Some(ObjectType::Tree), 0o040000) => return Ok(()),
                (Some(ObjectType::Blob), 0o100644 | 0o100755) => {}
                _ => return Err(refused("unsupported pair refresh file semantics")),
            }
            let blob = repo.find_blob(entry.id())?;
            let body = blob.content();
            if crate::lfs::is_lfs_pointer(body)
                || body.starts_with(b"version https://git-lfs.github.com/spec/v1")
            {
                return Err(refused("pair refresh cannot publish Git LFS pointers"));
            }
            if entry.name_bytes() == b".gitattributes" {
                let rules = std::str::from_utf8(body)
                    .map_err(|_| refused("unsupported pair refresh attribute rules"))?;
                if rules
                    .lines()
                    .map(str::trim)
                    .any(|line| !line.is_empty() && !line.starts_with('#'))
                {
                    return Err(refused("unsupported pair refresh attribute rules"));
                }
            }
            Ok::<(), GitError>(())
        })();
        match result {
            Ok(()) => TreeWalkResult::Ok,
            Err(error) => {
                failure = Some(error);
                TreeWalkResult::Abort
            }
        }
    })
    .or_else(|error| {
        if failure.is_some() {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Create an unreferenced, reproducible two-parent commit whose tree is exactly
/// the pinned parent tree. Never update HEAD or any branch here. Job recovery
/// pins the returned SHA before publishing either external side.
pub fn prepare_clean_child_git(
    repo: &Repository,
    pins: &CleanChildGitPins,
    operation_id: Uuid,
    prepared_at: i64,
) -> Result<PreparedCleanChildGit, GitError> {
    require_complete_git_graph(repo)?;
    if operation_id.is_nil() || prepared_at < 0 {
        return Err(refused("invalid pair refresh preparation identity"));
    }
    let child_id = pin(&pins.child_tip)?;
    let parent_id = pin(&pins.parent_tip)?;
    let baseline_id = pin(&pins.parent_baseline_tip)?;
    let child = repo.find_commit(child_id)?;
    let parent = repo.find_commit(parent_id)?;
    let baseline = repo.find_commit(baseline_id)?;
    if parent_id == baseline_id {
        return Err(refused("pair refresh has no parent advance"));
    }
    if !repo.graph_descendant_of(parent_id, baseline_id)?
        || (child_id != baseline_id && !repo.graph_descendant_of(child_id, baseline_id)?)
        || child.tree_id() != baseline.tree_id()
    {
        return Err(refused(
            "pair refresh requires unchanged child and shared baseline",
        ));
    }
    let mut current = parent_id;
    let mut parent_advance_commits = 0;
    while current != baseline_id {
        let commit = repo.find_commit(current)?;
        if commit.parent_count() != 1 || parent_advance_commits == MAX_PARENT_ADVANCE {
            return Err(refused("unsupported nonlinear or oversized parent advance"));
        }
        require_supported_refresh_tree(repo, &commit.tree()?)?;
        current = commit.parent_id(0)?;
        parent_advance_commits += 1;
    }
    // Also scan the unchanged input trees. A deleted LFS pointer or attribute
    // rule in the baseline must not bypass the admitted content policy.
    for tree in [child.tree()?, baseline.tree()?, parent.tree()?] {
        require_supported_refresh_tree(repo, &tree)?;
    }
    let tree = parent.tree()?;
    let signature = Signature::new("RepoSync", "reposync@localhost", &Time::new(prepared_at, 0))?;
    let message =
        format!("[reposync] Update pair from parent\n\nRepoSync-Pair-Refresh: {operation_id}\n");
    let prepared = repo.commit(
        None,
        &signature,
        &signature,
        &message,
        &tree,
        &[&child, &parent],
    )?;
    Ok(PreparedCleanChildGit {
        pins: pins.clone(),
        operation_id,
        prepared_at,
        prepared_sha: prepared.to_string(),
        tree_sha: tree.id().to_string(),
        parent_advance_commits,
    })
}

/// Read the exact retained object without manufacturing a missing commit during
/// recovery. The immutable OID, parents, tree, author and operation marker agree.
pub(crate) fn require_prepared_refresh_object(
    repo: &Repository,
    prepared: &PreparedCleanChildGit,
) -> Result<(), GitError> {
    require_complete_git_graph(repo)?;
    let commit = repo.find_commit(pin(&prepared.prepared_sha)?)?;
    let expected = [
        pin(&prepared.pins.child_tip)?,
        pin(&prepared.pins.parent_tip)?,
    ];
    let message = format!(
        "[reposync] Update pair from parent\n\nRepoSync-Pair-Refresh: {}\n",
        prepared.operation_id
    );
    if commit.parent_ids().collect::<Vec<_>>() != expected
        || commit.tree_id() != pin(&prepared.tree_sha)?
        || commit.message_bytes() != message.as_bytes()
    {
        return Err(refused("retained prepared commit differs from signed job"));
    }
    for signature in [commit.author(), commit.committer()] {
        if signature.name_bytes() != b"RepoSync"
            || signature.email_bytes() != b"reposync@localhost"
            || signature.when().seconds() != prepared.prepared_at
            || signature.when().offset_minutes() != 0
        {
            return Err(refused("retained prepared commit signature differs"));
        }
    }
    require_supported_refresh_tree(repo, &commit.tree()?)?;
    let baseline = repo.find_commit(pin(&prepared.pins.parent_baseline_tip)?)?;
    let child = repo.find_commit(expected[0])?;
    if child.tree_id() != baseline.tree_id()
        || (child.id() != baseline.id() && !repo.graph_descendant_of(child.id(), baseline.id())?)
    {
        return Err(refused("retained unchanged child baseline differs"));
    }
    let mut current = expected[1];
    let mut count = 0;
    while current != baseline.id() {
        let commit = repo.find_commit(current)?;
        if commit.parent_count() != 1 || count >= MAX_PARENT_ADVANCE {
            return Err(refused("retained parent advance is unsupported"));
        }
        require_supported_refresh_tree(repo, &commit.tree()?)?;
        current = commit.parent_id(0)?;
        count += 1;
    }
    if count == 0 || count != prepared.parent_advance_commits {
        return Err(refused("retained parent advance count differs"));
    }
    require_supported_refresh_tree(repo, &baseline.tree()?)?;
    require_supported_refresh_tree(repo, &child.tree()?)
}

/// Separate, inspection-only policy. Its digests never authorize publication.
pub const CHANGED_CHILD_REVIEW_POLICY: &str = "issue69_changed_child_review_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildGitInput {
    pub commit: String,
    pub tree: String,
}

/// A caller-supplied evidence reference, not a verified capability. This module
/// binds the reference but does not load it or establish its authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildEvidence {
    pub reference: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildProjection {
    pub policy: String,
    pub allowed_paths: Vec<String>,
    pub blocked_patterns: Vec<String>,
    pub lfs_threshold_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildSvnContext {
    pub repository_uuid: String,
    pub repository_root_url: String,
    pub child_path: String,
    pub parent_path: String,
    pub child_revision: i64,
    pub parent_revision: i64,
    pub baseline_revision: i64,
    pub inspected_head_revision: i64,
    pub lineage_evidence: Option<ChangedChildEvidence>,
    pub property_evidence: Option<ChangedChildEvidence>,
}

/// Safe metadata only. Even complete metadata is not authenticated or remotely
/// revalidated by this object-level inspector. No secret belongs in these fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildReviewContext {
    pub child: crate::db::pair_refresh_operations::RefreshRegistrationPin,
    pub parent: crate::db::pair_refresh_operations::RefreshRegistrationPin,
    pub git_remote_url: String,
    pub child_git_branch: String,
    pub parent_git_branch: String,
    pub transport_policy: super::publication::PinnedPushPolicy,
    pub svn: ChangedChildSvnContext,
    pub baseline_evidence: Option<ChangedChildEvidence>,
    pub actor_policy: Option<super::actor_policy::RefreshActorPolicyPin>,
    pub credential_context_digest: Option<String>,
    pub projection: ChangedChildProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildReviewInputs {
    pub schema: u8,
    pub policy: String,
    pub baseline: ChangedChildGitInput,
    pub child: ChangedChildGitInput,
    pub parent: ChangedChildGitInput,
    pub context: ChangedChildReviewContext,
}

/// Absence is distinct from an empty blob. Directory OIDs retain topology
/// evidence; result directories are derived from selected leaves, never copied
/// over another side's changed subtree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChangedChildEntry {
    Absent {},
    File { blob: String, mode: u32 },
    Directory { tree: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedChildChoice {
    Base,
    Child,
    Parent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangedChildDisposition {
    Directory,
    Selected { entry: ChangedChildEntry },
    Conflict,
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedChildPathReview {
    pub path: String,
    pub baseline: ChangedChildEntry,
    pub child: ChangedChildEntry,
    pub parent: ChangedChildEntry,
    pub disposition: ChangedChildDisposition,
}

/// Not deserializable as a capability. Resolution always recomputes this from
/// the supplied current inputs and immutable objects, not a client manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedChildReviewManifest {
    pub schema: u8,
    pub policy: String,
    pub inputs: ChangedChildReviewInputs,
    pub paths: Vec<ChangedChildPathReview>,
    pub child_changes: Vec<String>,
    pub parent_changes: Vec<String>,
    pub unverified: Vec<String>,
    pub candidate_tree: Option<String>,
    pub execution_eligible: bool,
    pub manifest_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildConflictChoice {
    pub path: String,
    pub baseline: ChangedChildEntry,
    pub child: ChangedChildEntry,
    pub parent: ChangedChildEntry,
    pub choice: ChangedChildChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedChildResolution {
    pub schema: u8,
    pub policy: String,
    pub inputs: ChangedChildReviewInputs,
    pub manifest_digest: String,
    pub choices: Vec<ChangedChildConflictChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedChildResolvedReview {
    pub schema: u8,
    pub policy: String,
    pub inputs: ChangedChildReviewInputs,
    pub original_manifest_digest: String,
    pub choices: Vec<ChangedChildConflictChoice>,
    pub paths: Vec<ChangedChildPathReview>,
    pub candidate_tree: String,
    pub unverified: Vec<String>,
    pub execution_eligible: bool,
    pub resolution_digest: String,
}

fn review_error(detail: &str) -> GitError {
    refused(&format!("changed-child inspection refused: {detail}"))
}

fn review_text(raw: &str) -> bool {
    !raw.is_empty() && raw.len() <= 4096 && raw.trim() == raw && !raw.chars().any(char::is_control)
}

fn review_sha256(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn review_oid(raw: &str) -> Result<Oid, GitError> {
    let oid = pin(raw)?;
    if oid.to_string() != raw {
        return Err(review_error("noncanonical Git object ID"));
    }
    Ok(oid)
}

fn review_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !matches!(part, "" | "." | ".." | ".svn" | ".git")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn review_evidence(value: Option<&ChangedChildEvidence>) -> Result<(), GitError> {
    if value.is_some_and(|value| !review_text(&value.reference) || !review_sha256(&value.sha256)) {
        return Err(review_error("malformed evidence reference"));
    }
    Ok(())
}

fn review_registration(
    registration: &crate::db::pair_refresh_operations::RefreshRegistrationPin,
    input: &ChangedChildGitInput,
    svn_revision: i64,
) -> Result<(), GitError> {
    let canonical_scope = Uuid::parse_str(&registration.scope_uuid)
        .is_ok_and(|id| !id.is_nil() && id.to_string() == registration.scope_uuid);
    if !review_text(&registration.repo_id)
        || !canonical_scope
        || !review_sha256(&registration.configuration_digest)
        || registration.echo_generation < 1
        || registration.last_git_sha != input.commit
        || registration.last_svn_rev != svn_revision
        || registration.scoped_git_checkpoint.as_deref() != Some(input.commit.as_str())
        || registration.scoped_svn_checkpoint.as_deref() != Some(svn_revision.to_string().as_str())
    {
        return Err(review_error(
            "contradictory registration/checkpoint metadata",
        ));
    }
    Ok(())
}

fn review_inputs(inputs: &ChangedChildReviewInputs) -> Result<(), GitError> {
    if inputs.schema != 1 || inputs.policy != CHANGED_CHILD_REVIEW_POLICY {
        return Err(review_error("unknown inspection schema or policy"));
    }
    for role in [&inputs.baseline, &inputs.child, &inputs.parent] {
        review_oid(&role.commit)?;
        review_oid(&role.tree)?;
    }
    let context = &inputs.context;
    let svn = &context.svn;
    review_registration(&context.child, &inputs.child, svn.child_revision)?;
    review_registration(&context.parent, &inputs.parent, svn.parent_revision)?;
    if context.child.repo_id == context.parent.repo_id
        || context.child.scope_uuid == context.parent.scope_uuid
        || context.child_git_branch == context.parent_git_branch
        || [&context.child_git_branch, &context.parent_git_branch]
            .iter()
            .any(|branch| {
                branch.is_empty()
                    || !git2::Reference::is_valid_name(&format!("refs/heads/{branch}"))
            })
    {
        return Err(review_error("ambiguous source identity"));
    }
    let git_url = super::publication::canonical_url(&context.git_remote_url)?;
    let git_scheme = match context.transport_policy {
        super::publication::PinnedPushPolicy::GitProtocolV1 => "git",
        super::publication::PinnedPushPolicy::SmartHttpV1 => "https",
    };
    if git_url.scheme() != git_scheme {
        return Err(review_error("Git endpoint differs from transport policy"));
    }
    let root = reqwest::Url::parse(&svn.repository_root_url)
        .map_err(|_| review_error("malformed SVN root metadata"))?;
    if !review_text(&svn.repository_uuid)
        || root.as_str() != svn.repository_root_url
        || !matches!(root.scheme(), "https" | "http" | "svn" | "file")
        || !root.username().is_empty()
        || root.password().is_some()
        || root.query().is_some()
        || root.fragment().is_some()
        || svn.repository_root_url.ends_with('/')
        || svn.repository_root_url.contains('%')
        || svn.repository_root_url.chars().any(char::is_control)
        || !root.path().starts_with('/')
        || !review_path(&root.path()[1..])
        || [&svn.child_path, &svn.parent_path]
            .iter()
            .any(|path| !path.starts_with('/') || !review_path(&path[1..]))
        || svn.child_path == svn.parent_path
        || svn.child_revision < 1
        || svn.baseline_revision < 1
        || svn.baseline_revision > svn.child_revision
        || svn.parent_revision <= svn.baseline_revision
        || svn.inspected_head_revision < svn.child_revision.max(svn.parent_revision)
    {
        return Err(review_error("contradictory SVN location/revision metadata"));
    }
    let projection = &context.projection;
    if projection.policy != "full_tree_no_lfs_v1"
        || !projection.allowed_paths.is_empty()
        || !projection.blocked_patterns.is_empty()
        || projection.lfs_threshold_mb != 0
    {
        return Err(review_error("unsupported projection metadata"));
    }
    review_evidence(context.baseline_evidence.as_ref())?;
    review_evidence(svn.lineage_evidence.as_ref())?;
    review_evidence(svn.property_evidence.as_ref())?;
    if context.actor_policy.as_ref().is_some_and(|actor| {
        actor.version != 2
            || !review_text(&actor.actor_id)
            || !review_sha256(&actor.session_context_digest)
            || !review_sha256(&actor.policy_context_digest)
    }) || context
        .credential_context_digest
        .as_deref()
        .is_some_and(|digest| !review_sha256(digest))
    {
        return Err(review_error(
            "malformed actor or credential context metadata",
        ));
    }
    Ok(())
}

fn review_tree_entries(
    repo: &Repository,
    root: Oid,
) -> Result<std::collections::BTreeMap<String, ChangedChildEntry>, GitError> {
    let tree = repo.find_tree(root)?;
    require_supported_refresh_tree(repo, &tree)?;
    let mut result = std::collections::BTreeMap::new();
    let mut pending = vec![(String::new(), root)];
    while let Some((prefix, id)) = pending.pop() {
        let tree = repo.find_tree(id)?;
        if !prefix.is_empty() && tree.is_empty() {
            return Err(review_error(
                "empty subtree has unsupported SVN directory semantics",
            ));
        }
        for entry in &tree {
            let name = entry.name().ok_or_else(|| review_error("non-UTF8 path"))?;
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            if !review_path(&path) {
                return Err(review_error("unsupported canonical relative path"));
            }
            let value = if entry.filemode() == 0o040000 {
                pending.push((path.clone(), entry.id()));
                ChangedChildEntry::Directory {
                    tree: entry.id().to_string(),
                }
            } else {
                ChangedChildEntry::File {
                    blob: entry.id().to_string(),
                    mode: entry.filemode() as u32,
                }
            };
            if result.insert(path, value).is_some() {
                return Err(review_error("duplicate tree path"));
            }
        }
    }
    Ok(result)
}

/// Walk the explicit baseline; never choose a replacement via merge-base or
/// equal content. Scan every intermediate tree, including deleted attributes/LFS.
fn review_side(repo: &Repository, tip: Oid, baseline: Oid) -> Result<(), GitError> {
    let mut current = tip;
    let mut count = 0;
    while current != baseline {
        let commit = repo.find_commit(current)?;
        if commit.parent_count() != 1 || count >= MAX_PARENT_ADVANCE {
            return Err(review_error(
                "nonlinear, unrelated or oversized review history",
            ));
        }
        review_tree_entries(repo, commit.tree_id())?;
        current = commit.parent_id(0)?;
        count += 1;
    }
    Ok(())
}

fn review_digest<T: Serialize>(domain: &str, payload: &T) -> Result<String, GitError> {
    use sha2::Digest;
    let bytes = serde_json::to_vec(&(domain, payload))
        .map_err(|_| review_error("inspection identity encoding failed"))?;
    Ok(hex::encode(sha2::Sha256::digest(bytes)))
}

/// Hash canonical Git tree bytes in memory. git2::Oid::hash_object calls
/// git_odb_hash, not an ODB write. Only selected opaque blob identities are used.
fn review_result_tree(paths: &[ChangedChildPathReview]) -> Result<String, GitError> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut files = BTreeMap::new();
    for path in paths {
        match &path.disposition {
            ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::File { blob, mode },
            } => {
                if !matches!(*mode, 0o100644 | 0o100755) {
                    return Err(review_error("unsupported result mode"));
                }
                files.insert(path.path.clone(), (*mode, review_oid(blob)?));
            }
            ChangedChildDisposition::Directory
            | ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::Absent {},
            } => {}
            _ => return Err(review_error("result still has a conflict or refusal")),
        }
    }
    let mut directories = BTreeSet::from([String::new()]);
    type TreeEntries = Vec<(String, bool, u32, Oid)>;
    let mut nodes: BTreeMap<String, TreeEntries> = BTreeMap::new();
    for (path, (mode, oid)) in &files {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        nodes
            .entry(parent.into())
            .or_default()
            .push((name.into(), false, *mode, *oid));
        let mut directory = parent;
        while !directory.is_empty() {
            if files.contains_key(directory) {
                return Err(review_error("result file/directory collision"));
            }
            directories.insert(directory.to_owned());
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.len()));
    for directory in directories {
        let mut entries = nodes.remove(&directory).unwrap_or_default();
        entries.sort_by_cached_key(|(name, is_directory, _, _)| {
            let mut key = name.as_bytes().to_vec();
            if *is_directory {
                key.push(b'/');
            }
            key
        });
        let mut bytes = Vec::new();
        for (name, _, mode, oid) in entries {
            bytes.extend_from_slice(format!("{mode:o} {name}\0").as_bytes());
            bytes.extend_from_slice(oid.as_bytes());
        }
        let oid = Oid::hash_object(ObjectType::Tree, &bytes)?;
        if directory.is_empty() {
            return Ok(oid.to_string());
        }
        let (parent, name) = directory
            .rsplit_once('/')
            .unwrap_or(("", directory.as_str()));
        nodes
            .entry(parent.into())
            .or_default()
            .push((name.into(), true, 0o040000, oid));
    }
    Err(review_error("missing result root"))
}

fn review_unverified(context: &ChangedChildReviewContext) -> Vec<String> {
    let mut reasons = vec![
        "baseline_provenance_not_authenticated",
        "actor_policy_not_revalidated",
        "credentials_not_revalidated",
        "svn_correspondence_not_verified",
        "live_sources_not_revalidated",
        "pending_work_not_independently_observed",
    ];
    for (missing, reason) in [
        (
            context.baseline_evidence.is_none(),
            "baseline_evidence_missing",
        ),
        (
            context.actor_policy.is_none(),
            "actor_policy_metadata_missing",
        ),
        (
            context.credential_context_digest.is_none(),
            "credential_context_metadata_missing",
        ),
        (
            context.svn.lineage_evidence.is_none(),
            "svn_lineage_evidence_missing",
        ),
        (
            context.svn.property_evidence.is_none(),
            "svn_property_evidence_missing",
        ),
    ] {
        if missing {
            reasons.push(reason);
        }
    }
    reasons.into_iter().map(str::to_owned).collect()
}

/// Inspect existing immutable objects only. This does not collect live inputs,
/// establish baseline authority, prepare a commit, or grant execution approval.
pub fn inspect_changed_child_review(
    repo: &Repository,
    inputs: &ChangedChildReviewInputs,
) -> Result<ChangedChildReviewManifest, GitError> {
    use std::collections::BTreeSet;
    review_inputs(inputs)?;
    require_complete_git_graph(repo)?;
    let baseline = review_oid(&inputs.baseline.commit)?;
    if inputs.parent.commit == inputs.baseline.commit {
        return Err(review_error("no parent advance"));
    }
    for role in [&inputs.baseline, &inputs.child, &inputs.parent] {
        if repo.find_commit(review_oid(&role.commit)?)?.tree_id() != review_oid(&role.tree)? {
            return Err(review_error("commit/tree pin mismatch"));
        }
    }
    review_side(repo, review_oid(&inputs.child.commit)?, baseline)?;
    review_side(repo, review_oid(&inputs.parent.commit)?, baseline)?;
    let base = review_tree_entries(repo, review_oid(&inputs.baseline.tree)?)?;
    let child = review_tree_entries(repo, review_oid(&inputs.child.tree)?)?;
    let parent = review_tree_entries(repo, review_oid(&inputs.parent.tree)?)?;
    let names: BTreeSet<_> = base
        .keys()
        .chain(child.keys())
        .chain(parent.keys())
        .cloned()
        .collect();
    let mut paths = Vec::new();
    let mut child_changes = Vec::new();
    let mut parent_changes = Vec::new();
    let mut unresolved = false;
    for path in names {
        let baseline = base
            .get(&path)
            .cloned()
            .unwrap_or(ChangedChildEntry::Absent {});
        let child = child
            .get(&path)
            .cloned()
            .unwrap_or(ChangedChildEntry::Absent {});
        let parent = parent
            .get(&path)
            .cloned()
            .unwrap_or(ChangedChildEntry::Absent {});
        if child != baseline {
            child_changes.push(path.clone());
        }
        if parent != baseline {
            parent_changes.push(path.clone());
        }
        let entries = [&baseline, &child, &parent];
        let has_directory = entries
            .iter()
            .any(|entry| matches!(entry, ChangedChildEntry::Directory { .. }));
        let has_file = entries
            .iter()
            .any(|entry| matches!(entry, ChangedChildEntry::File { .. }));
        let deleted_modified_directory = matches!(baseline, ChangedChildEntry::Directory { .. })
            && ((matches!(child, ChangedChildEntry::Absent {})
                && matches!(parent, ChangedChildEntry::Directory { .. })
                && parent != baseline)
                || (matches!(parent, ChangedChildEntry::Absent {})
                    && matches!(child, ChangedChildEntry::Directory { .. })
                    && child != baseline));
        let disposition = if (has_directory && has_file) || deleted_modified_directory {
            unresolved = true;
            ChangedChildDisposition::Refused {
                reason: "unsupported_file_directory_topology".into(),
            }
        } else if has_directory {
            ChangedChildDisposition::Directory
        } else if child == parent || parent == baseline {
            ChangedChildDisposition::Selected {
                entry: child.clone(),
            }
        } else if child == baseline {
            ChangedChildDisposition::Selected {
                entry: parent.clone(),
            }
        } else {
            unresolved = true;
            ChangedChildDisposition::Conflict
        };
        paths.push(ChangedChildPathReview {
            path,
            baseline,
            child,
            parent,
            disposition,
        });
    }
    let candidate_tree = if unresolved {
        None
    } else {
        Some(review_result_tree(&paths)?)
    };
    let mut manifest = ChangedChildReviewManifest {
        schema: 1,
        policy: CHANGED_CHILD_REVIEW_POLICY.into(),
        inputs: inputs.clone(),
        paths,
        child_changes,
        parent_changes,
        unverified: review_unverified(&inputs.context),
        candidate_tree,
        execution_eligible: false,
        manifest_digest: String::new(),
    };
    // The field is empty during hashing. Every other field, including refusal
    // and verification status, participates. Never hash a caller's manifest.
    manifest.manifest_digest = review_digest("changed_child_manifest_v1", &manifest)?;
    Ok(manifest)
}

/// Recompute before interpreting any choices. Freshness here means equality to
/// supplied current inputs, not observation of a remote or current DB authority.
pub fn resolve_changed_child_review(
    repo: &Repository,
    current_inputs: &ChangedChildReviewInputs,
    resolution: &ChangedChildResolution,
) -> Result<ChangedChildResolvedReview, GitError> {
    let manifest = inspect_changed_child_review(repo, current_inputs)?;
    if resolution.schema != 1
        || resolution.policy != CHANGED_CHILD_REVIEW_POLICY
        || resolution.inputs != *current_inputs
        || resolution.manifest_digest != manifest.manifest_digest
    {
        return Err(review_error("stale resolution identity"));
    }
    let mut choices = std::collections::BTreeMap::new();
    for choice in &resolution.choices {
        if choices.insert(choice.path.as_str(), choice).is_some() {
            return Err(review_error("duplicate resolution path"));
        }
    }
    let mut paths = manifest.paths.clone();
    for path in &mut paths {
        match &path.disposition {
            ChangedChildDisposition::Refused { .. } => {
                return Err(review_error(
                    "unsupported topology cannot be resolved by side choice",
                ));
            }
            ChangedChildDisposition::Conflict => {
                let choice = choices
                    .remove(path.path.as_str())
                    .ok_or_else(|| review_error("missing conflict resolution"))?;
                if choice.baseline != path.baseline
                    || choice.child != path.child
                    || choice.parent != path.parent
                {
                    return Err(review_error(
                        "resolution triple differs from current conflict",
                    ));
                }
                let entry = match choice.choice {
                    ChangedChildChoice::Base => path.baseline.clone(),
                    ChangedChildChoice::Child => path.child.clone(),
                    ChangedChildChoice::Parent => path.parent.clone(),
                };
                path.disposition = ChangedChildDisposition::Selected { entry };
            }
            _ => {}
        }
    }
    if !choices.is_empty() {
        return Err(review_error(
            "resolution names an unknown or non-conflict path",
        ));
    }
    let mut choices = resolution.choices.clone();
    choices.sort_by(|left, right| left.path.cmp(&right.path));
    let mut result = ChangedChildResolvedReview {
        schema: 1,
        policy: CHANGED_CHILD_REVIEW_POLICY.into(),
        inputs: current_inputs.clone(),
        original_manifest_digest: manifest.manifest_digest,
        choices,
        candidate_tree: review_result_tree(&paths)?,
        paths,
        unverified: manifest.unverified,
        execution_eligible: false,
        resolution_digest: String::new(),
    };
    result.resolution_digest = review_digest("changed_child_resolution_v1", &result)?;
    Ok(result)
}

/// The baseline is justified by an exact authenticated retained merge, not by
/// a merge-base query or a current equal-content commit. This policy is separate
/// from M1's unchanged object-only linear-history inspector.
pub const CHANGED_CHILD_APPLICABILITY_POLICY: &str =
    "issue69_retained_merge_linear_sides_review_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedChildApplicabilityRoles {
    pub parent_anchor: ChangedChildGitInput,
    pub integrated_child: ChangedChildGitInput,
    pub current_child: ChangedChildGitInput,
    pub current_parent: ChangedChildGitInput,
}

/// Serializable inspection metadata only. Constructing or changing these bytes
/// cannot construct the live source capability or authorize a resolution/job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedChildApplicableGraphReview {
    pub schema: u8,
    pub policy: String,
    pub historical_merge: PreparedCleanChildGit,
    pub roles: ChangedChildApplicabilityRoles,
    pub context: ChangedChildReviewContext,
    pub baseline_rule: String,
    pub paths: Vec<ChangedChildPathReview>,
    pub child_changes: Vec<String>,
    pub parent_changes: Vec<String>,
    pub candidate_tree: Option<String>,
    pub execution_eligible: bool,
    pub inspection_digest: String,
}

/// Object-level implementation seam, not a provenance factory. Only the live
/// adapter binds its output to opaque M2d correspondence. No ODB/ref/DB write.
pub(crate) fn inspect_changed_child_applicable_graph(
    repo: &Repository,
    historical: &PreparedCleanChildGit,
    roles: &ChangedChildApplicabilityRoles,
    context: &ChangedChildReviewContext,
) -> Result<ChangedChildApplicableGraphReview, GitError> {
    use std::collections::BTreeSet;

    // Reuse metadata validation, not M1's linear-history admission. Its complete
    // public inspector and resolution path remain unchanged and still refuse R.
    review_inputs(&ChangedChildReviewInputs {
        schema: 1,
        policy: CHANGED_CHILD_REVIEW_POLICY.into(),
        baseline: roles.parent_anchor.clone(),
        child: roles.current_child.clone(),
        parent: roles.current_parent.clone(),
        context: context.clone(),
    })?;
    require_prepared_refresh_object(repo, historical)?;
    if roles.parent_anchor.commit != historical.pins.parent_tip
        || roles.integrated_child.commit != historical.prepared_sha
        || roles.parent_anchor.tree != historical.tree_sha
        || roles.integrated_child.tree != historical.tree_sha
    {
        return Err(review_error(
            "retained P1/R roles differ from the exact signed merge",
        ));
    }
    for role in [
        &roles.parent_anchor,
        &roles.integrated_child,
        &roles.current_child,
        &roles.current_parent,
    ] {
        if repo.find_commit(review_oid(&role.commit)?)?.tree_id() != review_oid(&role.tree)? {
            return Err(review_error(
                "applicability role commit/tree pin mismatch",
            ));
        }
    }
    if roles.current_child.commit == roles.integrated_child.commit
        || roles.current_parent.commit == roles.parent_anchor.commit
        || roles.current_child.tree == roles.integrated_child.tree
        || roles.current_parent.tree == roles.parent_anchor.tree
    {
        return Err(review_error(
            "this inspection subset requires both sides to advance content",
        ));
    }
    // Each side has its own explicit anchor. Only the exact retained R merge is
    // admitted; additional merges, foreign roots and oversize paths refuse.
    review_side(
        repo,
        review_oid(&roles.current_child.commit)?,
        review_oid(&roles.integrated_child.commit)?,
    )?;
    review_side(
        repo,
        review_oid(&roles.current_parent.commit)?,
        review_oid(&roles.parent_anchor.commit)?,
    )?;
    let base = review_tree_entries(repo, review_oid(&roles.integrated_child.tree)?)?;
    let child = review_tree_entries(repo, review_oid(&roles.current_child.tree)?)?;
    let parent = review_tree_entries(repo, review_oid(&roles.current_parent.tree)?)?;
    let names: BTreeSet<_> = base
        .keys()
        .chain(child.keys())
        .chain(parent.keys())
        .cloned()
        .collect();
    let mut paths = Vec::new();
    let mut child_changes = Vec::new();
    let mut parent_changes = Vec::new();
    let mut unresolved = false;
    for path in names {
        let baseline = base
            .get(&path)
            .cloned()
            .unwrap_or(ChangedChildEntry::Absent {});
        let child = child
            .get(&path)
            .cloned()
            .unwrap_or(ChangedChildEntry::Absent {});
        let parent = parent
            .get(&path)
            .cloned()
            .unwrap_or(ChangedChildEntry::Absent {});
        if child != baseline {
            child_changes.push(path.clone());
        }
        if parent != baseline {
            parent_changes.push(path.clone());
        }
        let entries = [&baseline, &child, &parent];
        let has_directory = entries
            .iter()
            .any(|entry| matches!(entry, ChangedChildEntry::Directory { .. }));
        let has_file = entries
            .iter()
            .any(|entry| matches!(entry, ChangedChildEntry::File { .. }));
        let deleted_modified_directory = matches!(baseline, ChangedChildEntry::Directory { .. })
            && ((matches!(child, ChangedChildEntry::Absent {})
                && matches!(parent, ChangedChildEntry::Directory { .. })
                && parent != baseline)
                || (matches!(parent, ChangedChildEntry::Absent {})
                    && matches!(child, ChangedChildEntry::Directory { .. })
                    && child != baseline));
        let disposition = if (has_directory && has_file) || deleted_modified_directory {
            unresolved = true;
            ChangedChildDisposition::Refused {
                reason: "unsupported_file_directory_topology".into(),
            }
        } else if has_directory {
            ChangedChildDisposition::Directory
        } else if child == parent || parent == baseline {
            ChangedChildDisposition::Selected {
                entry: child.clone(),
            }
        } else if child == baseline {
            ChangedChildDisposition::Selected {
                entry: parent.clone(),
            }
        } else {
            unresolved = true;
            ChangedChildDisposition::Conflict
        };
        paths.push(ChangedChildPathReview {
            path,
            baseline,
            child,
            parent,
            disposition,
        });
    }
    let candidate_tree = if unresolved {
        None
    } else {
        Some(review_result_tree(&paths)?)
    };
    let mut review = ChangedChildApplicableGraphReview {
        schema: 1,
        policy: CHANGED_CHILD_APPLICABILITY_POLICY.into(),
        historical_merge: historical.clone(),
        roles: roles.clone(),
        context: context.clone(),
        baseline_rule: "exact_authenticated_R_tree_equals_signed_P1_tree".into(),
        paths,
        child_changes,
        parent_changes,
        candidate_tree,
        execution_eligible: false,
        inspection_digest: String::new(),
    };
    review.inspection_digest = review_digest("changed_child_applicable_graph_v1", &review)?;
    Ok(review)
}
