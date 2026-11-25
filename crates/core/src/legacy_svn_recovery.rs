//! Read-only corroboration of retained legacy SVN-to-Git imports. An operator
//! must identify either a trusted historical restore or the known-good, stopped
//! original installation before migration. A digest alone establishes neither.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, ensure, Context, Result};
use git2::{Oid, Repository};
use reqwest::Url;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::db::historical_enrollment::LegacyPairPins;
use crate::db::lineage_checkpoints::{
    observed_svn_identity_from_history, observed_svn_identity_path, CanonicalLineage,
    LiveSvnIdentity,
};
use crate::pair_refresh::git_sources::require_historical_git_tip;
use crate::pair_refresh::publication::require_complete_git_graph;
use crate::pair_refresh::svn_preparation::{verify_refresh_svn_tree, visible_checkout_properties};
use crate::pair_refresh::svn_visibility::{observe_subtree, require_empty_visible_ancestors};
use crate::svn::{parser::parse_copy_history_entries, SvnClient, SvnInfo, SvnLogEntry};

pub enum HistoricalSvnSource {
    /// A separately trusted pre-upgrade backup restored privately. SVN does
    /// not store the original service URL in a dump; the custodian supplies it.
    Restored {
        repository: PathBuf,
        original_root_url: String,
    },
    /// The storage layer must establish physical pre-migration state, retain a
    /// reviewed receipt, and require it before installing the resulting pin.
    PlannedCutover,
}

pub(crate) struct HistoricalSvnWitness {
    pub source: HistoricalSvnSource,
    pub git_mirror: PathBuf,
    /// None selects the current active SVN revision for a fresh preview.
    /// Apply supplies the lower bound recorded when the operator reviewed it.
    pub minimum_live_revision: Option<i64>,
    /// A keyed digest of the selected registered credential context. Raw
    /// credentials never appear in receipts, including public provenance.
    pub credential_context_sha256: Option<String>,
    /// Derived from the stored Team registration by the credential owner.
    /// Independently checked against the exact registered URL before contact.
    pub configured_git_url: String,
}

/// Bounded operator diagnostics. VCS errors may contain private locations,
/// paths or output. Never cross the proof boundary with their raw text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HistoricalVerifyError {
    #[error("The registered endpoint or credential context could not be verified.")]
    Registration,
    #[error("The registered or mirror Git branch could not be read at the retained tip.")]
    GitRef,
    #[error("The retained mapping and Git commit ancestry do not form a complete linear history.")]
    GitAncestry,
    #[error("The registered SVN branch, incarnation or history through the watermark could not be verified.")]
    SvnHistory,
    #[error("A historical SVN checkout, its visible properties or its complete Git projection could not be verified.")]
    SvnTree,
    #[error("The trusted SVN restore or its association with the registered history could not be verified.")]
    Archive,
}

impl HistoricalVerifyError {
    pub fn classify(error: &anyhow::Error) -> Self {
        Self::specific_or(error, Self::Registration)
    }
    fn specific_or(error: &anyhow::Error, fallback: Self) -> Self {
        error.downcast_ref::<Self>().copied().unwrap_or(fallback)
    }
}

pub struct VerifiedHistoricalLineage {
    trees: Vec<Oid>,
    reviewed_floor: i64,
    branch_revisions: usize,
    pair: LegacyPairPins,
    lineage: CanonicalLineage,
    history: String,
    review: String,
    planned: bool,
    provenance: Value,
}

impl VerifiedHistoricalLineage {
    pub fn pair(&self) -> &LegacyPairPins {
        &self.pair
    }
    pub fn lineage(&self) -> &CanonicalLineage {
        &self.lineage
    }
    /// Verified trees in exactly the same order as the retained SVN/Git pairs.
    pub fn reviewed_svn_floor(&self) -> i64 {
        self.reviewed_floor
    }
    pub fn verified_branch_revisions(&self) -> usize {
        self.branch_revisions
    }
    pub fn projected_tree_ids(&self) -> &[Oid] {
        &self.trees
    }
    pub fn history_sha256(&self) -> &str {
        &self.history
    }
    pub fn review_sha256(&self) -> &str {
        &self.review
    }
    pub fn is_planned_cutover(&self) -> bool {
        self.planned
    }
    pub fn provenance(&self) -> Value {
        self.provenance.clone()
    }
}

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

fn checked_url(raw: &str, schemes: &[&str]) -> Result<Url> {
    let url = Url::parse(raw).context("registered endpoint is not a supported URL")?;
    ensure!(
        url.as_str() == raw
            && schemes.contains(&url.scheme())
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && !raw.chars().any(char::is_control),
        "unsupported or noncanonical registered endpoint"
    );
    ensure!(
        if url.scheme() == "file" {
            url.host_str().is_none()
        } else {
            url.host_str().is_some()
        },
        "unsupported endpoint authority"
    );
    Ok(url)
}

fn branch_url(pair: &LegacyPairPins) -> Result<String> {
    checked_url(&pair.svn_url, &["file", "https"])?;
    ensure!(
        !pair.svn_url.ends_with('/')
            && pair.svn_branch.split('/').all(|segment| {
                !matches!(segment, "" | "." | "..")
                    && segment
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            }),
        "unsupported registered SVN branch"
    );
    Ok(format!("{}/{}", pair.svn_url, pair.svn_branch))
}

fn oid(raw: &str) -> Result<Oid> {
    let id = Oid::from_str(raw).context("invalid retained Git object ID")?;
    ensure!(
        !id.is_zero() && id.to_string() == raw,
        "noncanonical retained Git object ID"
    );
    Ok(id)
}

fn open_mirror(path: &Path) -> Result<Repository> {
    let repo = Repository::open_bare(path).context("approved bare Git repository unavailable")?;
    ensure!(
        repo.is_bare() && repo.path().canonicalize()? == path.canonicalize()?,
        "Git repository identity differs"
    );
    require_complete_git_graph(&repo)?;
    for name in [
        "objects/info/alternates",
        "objects/info/http-alternates",
        "commondir",
    ] {
        match std::fs::symlink_metadata(path.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => anyhow::bail!("Git repository has unsupported external storage"),
        }
    }
    Ok(repo)
}

fn require_local_ref(path: &Path, reference: &str, tip: Oid) -> Result<()> {
    let repo = open_mirror(path)?;
    ensure!(
        repo.find_reference(reference)?.target() == Some(tip),
        "named Git branch differs from retained tip"
    );
    Ok(())
}

fn registered_ref(url: &Url, reference: &str, tip: Oid, token: Option<&str>) -> Result<()> {
    match url.scheme() {
        "file" => require_local_ref(
            &url.to_file_path()
                .map_err(|_| anyhow!("invalid Git file URL"))?,
            reference,
            tip,
        ),
        "https" => Ok(require_historical_git_tip(
            url.as_str(),
            reference,
            tip,
            token,
        )?),
        _ => anyhow::bail!("unsupported registered Git transport"),
    }
}

fn mapped_trees(pair: &LegacyPairPins, mirror: &Path) -> Result<Vec<Oid>> {
    let repo = open_mirror(mirror)?;
    let mut previous = None;
    let mut trees = Vec::with_capacity(pair.mappings.len());
    let mut last_rev = 0;
    for (rev, sha) in &pair.mappings {
        ensure!(
            *rev > last_rev,
            "retained SVN revisions are not strictly increasing"
        );
        let id = oid(sha)?;
        let commit = repo
            .find_commit(id)
            .context("retained Git commit unavailable")?;
        ensure!(
            commit.parent_ids().eq(previous.into_iter()),
            "retained Git ancestry is incomplete, nonlinear or discontinuous"
        );
        previous = Some(id);
        last_rev = *rev;
        trees.push(commit.tree_id());
    }
    ensure!(
        last_rev == pair.last_svn_rev && previous == Some(oid(&pair.last_git_sha)?),
        "retained mappings do not reach both cursors"
    );
    Ok(trees)
}

struct SvnEvidence {
    identity: LiveSvnIdentity,
    head: i64,
    log: String,
    snapshots: String,
    branch_revisions: usize,
}

struct ScopedHistory {
    digest: String,
    identity: LiveSvnIdentity,
    revisions: Vec<SvnLogEntry>,
}

async fn history_at_head(
    client: &SvnClient,
    info: &SvnInfo,
    branch: &str,
    pair: &LegacyPairPins,
) -> Result<ScopedHistory> {
    let head = info.latest_rev;
    let completion = pair.import_completion_svn_rev.unwrap_or(pair.last_svn_rev);
    ensure!(
        completion >= pair.last_svn_rev && head >= completion,
        "SVN does not cover the original import completion"
    );
    let xml = client.copy_history_xml_at_rev(head, true).await?;
    let mut log = parse_copy_history_entries(&xml, true)?;
    let identity =
        observed_svn_identity_from_history(info, observed_svn_identity_path(info)?, &log)?;
    log.reverse();
    let created = log.first().context("missing SVN origin")?.revision;
    let prefix = format!("{branch}/");
    let mut previous = 0;
    for entry in &mut log {
        ensure!(
            entry.revision > previous && entry.revision <= head,
            "invalid or incomplete SVN history order"
        );
        previous = entry.revision;
        entry.changed_paths.sort_by(|a, b| a.path.cmp(&b.path));
        ensure!(
            !entry
                .changed_paths
                .windows(2)
                .any(|p| p[0].path == p[1].path),
            "duplicate SVN changed path"
        );
        ensure!(
            entry
                .changed_paths
                .iter()
                .any(|p| p.path == branch || p.path.starts_with(&prefix)),
            "unproved SVN branch history"
        );
        for p in &entry.changed_paths {
            if p.path == branch {
                if entry.revision == created {
                    ensure!(
                        p.action == "A" && p.copy_from_path.is_none(),
                        "copied or replaced SVN branch origin"
                    );
                } else {
                    ensure!(p.action == "M", "SVN branch incarnation changed");
                }
            } else if branch
                .strip_prefix(&p.path)
                .is_some_and(|tail| tail.starts_with('/'))
                || p.path == "/"
            {
                ensure!(
                    p.action == "M"
                        || (entry.revision == created
                            && p.action == "A"
                            && p.copy_from_path.is_none()),
                    "SVN branch ancestor was copied or replaced"
                );
            }
        }
    }
    ensure!(
        log[0].changed_paths.iter().any(|p| p.path == branch),
        "SVN branch creation is not directly proved"
    );
    log.retain(|entry| entry.revision <= completion);
    ensure!(
        pair.mappings
            .iter()
            .all(|(rev, _)| log.binary_search_by_key(rev, |e| e.revision).is_ok()),
        "retained mapping names a revision outside this SVN branch history"
    );
    if let Some(expected) = pair.import_log_entry_count {
        ensure!(
            usize::try_from(expected).ok() == Some(log.len()),
            "old import progress and SVN log disagree"
        );
    }
    // The normal observer models actions and copy ancestry. Also preserve the
    // verbose node kinds, text/property flags and any future validated
    // attributes so two different changes cannot collapse into one receipt.
    let xml = roxmltree::Document::parse(&xml)?;
    let mut attributes = BTreeMap::new();
    let mut revprops = BTreeMap::new();
    for entry in xml.root_element().children().filter(|n| n.is_element()) {
        let rev: i64 = entry
            .attribute("revision")
            .context("missing SVN revision")?
            .parse()?;
        if rev > completion {
            continue;
        }
        let mut groups = entry
            .children()
            .filter(|node| node.has_tag_name("revprops"));
        if let Some(group) = groups.next() {
            ensure!(
                groups.next().is_none() && group.attributes().len() == 0,
                "ambiguous SVN revision properties"
            );
            let mut values = BTreeMap::new();
            for prop in group.children().filter(|n| n.is_element()) {
                let name = prop
                    .attribute("name")
                    .context("unnamed SVN revision property")?;
                let encoding = prop.attribute("encoding");
                ensure!(
                    prop.has_tag_name("property")
                        && prop.tag_name().namespace().is_none()
                        && !name.is_empty()
                        && encoding.is_none_or(|e| e == "base64")
                        && prop
                            .attributes()
                            .all(|a| a.namespace().is_none()
                                && matches!(a.name(), "name" | "encoding"))
                        && !prop.children().any(|n| n.is_element()),
                    "unsupported SVN revision property"
                );
                let value: String = prop
                    .children()
                    .filter(|n| n.is_text())
                    .filter_map(|n| n.text())
                    .collect();
                ensure!(
                    values.insert(name, (encoding, value)).is_none(),
                    "duplicate SVN revision property"
                );
            }
            revprops.insert(rev, values);
        }
        for node in entry.descendants().filter(|n| n.has_tag_name("path")) {
            let path = node.text().context("missing SVN changed path")?;
            let attrs: BTreeMap<_, _> = node.attributes().map(|a| (a.name(), a.value())).collect();
            attributes
                .entry(rev)
                .or_insert_with(BTreeMap::new)
                .insert(path, attrs);
        }
    }
    ensure!(
        identity.incarnation_created_rev == created
            && identity.branch_path == pair.svn_branch
            && identity.copy_from_path.is_none()
            && identity.copy_from_rev.is_none(),
        "historical SVN incarnation is ambiguous"
    );
    Ok(ScopedHistory {
        digest: digest(&(&log, attributes, revprops))?,
        identity,
        revisions: log,
    })
}

async fn observe_svn(
    client: &SvnClient,
    root: &str,
    pair: &LegacyPairPins,
    mirror: &Path,
    trees: &[Oid],
) -> Result<SvnEvidence> {
    let info = client.info().await?;
    let branch = format!("/{}", pair.svn_branch);
    ensure!(
        info.root_url == root
            && info.url == format!("{root}{branch}")
            && info.url == client.url()
            && info.latest_rev >= pair.last_svn_rev
            && uuid::Uuid::parse_str(&info.uuid)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == info.uuid),
        "SVN repository identity, location or revision differs"
    );
    let history = history_at_head(client, &info, &branch, pair).await?;
    let branch_revisions = history.revisions.len();
    let has_skipped = branch_revisions != pair.mappings.len();
    tracing::info!(
        mapping_count = trees.len(),
        branch_revisions,
        through_revision = pair.import_completion_svn_rev.unwrap_or(pair.last_svn_rev),
        "Verifying one complete SVN checkout for each historical branch revision"
    );
    let started = std::time::Instant::now();
    let mut snapshots = Vec::with_capacity(branch_revisions);
    let git = open_mirror(mirror)?;
    let mut previous_tree = None;
    for (i, entry) in history.revisions.iter().enumerate() {
        let rev = entry.revision;
        let mapped = pair.mappings.binary_search_by_key(&rev, |(r, _)| *r).ok();
        if let Some(index) = mapped {
            previous_tree = Some(trees[index]);
            if has_skipped {
                // With possible identical-tree gaps, corroborate the actual
                // shipped full import's revision attribution in the Git object.
                let commit = git.find_commit(oid(&pair.mappings[index].1)?)?;
                let message = format!(
                    "{}\n\n[reposync] imported from SVN r{}\nSVN-Author: {}\nSVN-Date: {}",
                    entry.message, rev, entry.author, entry.date
                );
                ensure!(
                    commit
                        .message_bytes()
                        .strip_suffix(b"\n")
                        .unwrap_or(commit.message_bytes())
                        == message.as_bytes(),
                    HistoricalVerifyError::GitAncestry
                );
            }
        }
        let checkout = observe_subtree(client, rev)
            .await
            .context(HistoricalVerifyError::SvnTree)?;
        let pinned = checkout.info();
        ensure!(
            pinned.uuid == info.uuid && pinned.url == info.url && pinned.root_url == info.root_url,
            "pegged SVN snapshot identity differs"
        );
        require_empty_visible_ancestors(client, pinned)
            .await
            .context(HistoricalVerifyError::SvnTree)?;
        let properties = visible_checkout_properties(client, checkout.path(), pinned)
            .await
            .context(HistoricalVerifyError::SvnTree)?;
        if let Some(tree) = previous_tree {
            verify_refresh_svn_tree(
                mirror,
                &tree.to_string(),
                checkout.path(),
                &properties,
                None,
                &branch,
            )
            .context(HistoricalVerifyError::SvnTree)?;
        } else {
            // Pre-first-commit SVN must be exactly a property-free, fully
            // visible empty branch. No nested directory, file or inherited
            // transformation can silently disappear from a missing mapping.
            ensure!(
                checkout.node_count() == 1 && properties.values().all(|values| values.is_empty()),
                HistoricalVerifyError::SvnTree
            );
        }
        snapshots.push((
            rev,
            mapped.is_some(),
            previous_tree.map(|id| id.to_string()),
            checkout.node_count(),
            digest(&properties)?,
        ));
        let completed = i + 1;
        if completed == 1 || completed % 100 == 0 || completed == branch_revisions {
            tracing::info!(
                completed,
                total = branch_revisions,
                revision = rev,
                elapsed_seconds = started.elapsed().as_secs(),
                "Verified historical SVN and Git snapshots"
            );
        }
    }
    Ok(SvnEvidence {
        identity: history.identity,
        head: info.latest_rev,
        log: history.digest,
        snapshots: digest(&snapshots)?,
        branch_revisions,
    })
}

async fn require_stable_source(
    client: &SvnClient,
    evidence: &SvnEvidence,
    pair: &LegacyPairPins,
    is_archive: bool,
) -> Result<()> {
    let after = client.info().await?;
    // Pending revisions on the active company SVN are normal while bridge
    // writers are stopped. Its HEAD may advance only if the current path is
    // still the original incarnation and the mapped prefix remains identical.
    let revision_ok = if is_archive {
        after.latest_rev == evidence.head
    } else {
        after.latest_rev >= evidence.head
    };
    ensure!(
        revision_ok
            && after.uuid == evidence.identity.uuid
            && after.url == evidence.identity.url
            && after.root_url == evidence.identity.root_url,
        "SVN source changed during review"
    );
    let history = history_at_head(client, &after, &format!("/{}", pair.svn_branch), pair).await?;
    ensure!(
        history.digest == evidence.log && history.identity == evidence.identity,
        "SVN history changed during review"
    );
    tracing::info!(
        start_head = evidence.head,
        end_head = after.latest_rev,
        through_revision = pair.last_svn_rev,
        "Verified current SVN incarnation against the retained history"
    );
    Ok(())
}

async fn verify_archive(
    repository: &Path,
    original_root_url: &str,
    pair: &LegacyPairPins,
    mirror: &Path,
    trees: &[Oid],
    active: &SvnEvidence,
) -> Result<Value> {
    ensure!(
        original_root_url == pair.svn_url,
        "trusted backup original location differs from registration"
    );
    let repository = repository.canonicalize()?;
    let original = Url::parse(original_root_url)?;
    if original.scheme() == "file" {
        let live_path = original
            .to_file_path()
            .map_err(|_| anyhow!("invalid original local SVN root"))?
            .canonicalize()?;
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::metadata(live_path)?;
        let b = std::fs::metadata(&repository)?;
        ensure!(
            (a.dev(), a.ino()) != (b.dev(), b.ino()),
            "trusted restore must be separate from the active repository"
        );
    }
    let root = Url::from_file_path(&repository)
        .map_err(|_| anyhow!("unusable restored repository path"))?
        .to_string();
    let archived =
        SvnClient::new_isolated_refresh(&format!("{root}/{}", pair.svn_branch), "", None)?;
    let old = observe_svn(&archived, &root, pair, mirror, trees).await?;
    ensure!(
        old.identity.uuid == active.identity.uuid
            && old.identity.branch_path == active.identity.branch_path
            && old.identity.incarnation_created_rev == active.identity.incarnation_created_rev
            && old.log == active.log
            && old.snapshots == active.snapshots,
        "trusted backup and current SVN history differ"
    );
    require_stable_source(&archived, &old, pair, true).await?;
    Ok(json!({"restore_location_sha256": digest(&repository)?, "restored_head": old.head}))
}

pub(crate) async fn verify_registered(
    pair: &LegacyPairPins,
    witness: &HistoricalSvnWitness,
    svn_username: &str,
    svn_password: Option<&str>,
    git_token: Option<&str>,
) -> Result<VerifiedHistoricalLineage> {
    let url = branch_url(pair).map_err(|_| HistoricalVerifyError::Registration)?;
    let live = SvnClient::new_isolated_refresh(&url, svn_username, svn_password)
        .map_err(|_| HistoricalVerifyError::Registration)?;
    verify_isolated(pair, witness, &live, git_token)
        .await
        .map_err(|error| HistoricalVerifyError::classify(&error).into())
}

async fn verify_isolated(
    pair: &LegacyPairPins,
    witness: &HistoricalSvnWitness,
    live: &SvnClient,
    token: Option<&str>,
) -> Result<VerifiedHistoricalLineage> {
    ensure!(
        live.url() == branch_url(pair)?,
        "SVN source differs from registration"
    );
    ensure!(
        pair.svn_url.starts_with("file://") || live.source_username() == pair.svn_username,
        "SVN identity differs from registration"
    );
    let expected_git_url =
        crate::git::derive_git_remote_url(&pair.git_api_url, None, &pair.git_repo);
    ensure!(
        witness.configured_git_url == expected_git_url,
        "Git endpoint differs from registered repository"
    );
    let git_url = checked_url(&expected_git_url, &["file", "https"])?;
    ensure!(
        pair.git_repo.split('/').count() == 2
            && pair
                .git_repo
                .split('/')
                .all(|s| !matches!(s, "" | "." | "..")
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))),
        "unsupported Git repository registration"
    );
    let remote = git_url.scheme() != "file" || !pair.svn_url.starts_with("file://");
    ensure!(
        !remote
            || witness
                .credential_context_sha256
                .as_ref()
                .is_some_and(|v| v.len() == 64
                    && v.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "remote credential context is not bound to this review"
    );
    let reference = format!("refs/heads/{}", pair.git_branch);
    ensure!(
        !pair.git_branch.starts_with("refs/") && git2::Reference::is_valid_name(&reference),
        "unsupported registered Git branch"
    );
    let tip = oid(&pair.last_git_sha).context(HistoricalVerifyError::GitAncestry)?;
    let mirror = witness
        .git_mirror
        .canonicalize()
        .context(HistoricalVerifyError::GitRef)?;
    require_local_ref(&mirror, &reference, tip).context(HistoricalVerifyError::GitRef)?;
    registered_ref(&git_url, &reference, tip, token).context(HistoricalVerifyError::GitRef)?;
    let trees = mapped_trees(pair, &mirror).context(HistoricalVerifyError::GitAncestry)?;
    let active = observe_svn(live, &pair.svn_url, pair, &mirror, &trees)
        .await
        .map_err(|e| HistoricalVerifyError::specific_or(&e, HistoricalVerifyError::SvnHistory))?;
    let reviewed_floor = witness.minimum_live_revision.unwrap_or(active.head);
    ensure!(
        reviewed_floor >= pair.import_completion_svn_rev.unwrap_or(pair.last_svn_rev)
            && active.head >= reviewed_floor,
        HistoricalVerifyError::SvnHistory
    );
    let (planned, archive) = match &witness.source {
        HistoricalSvnSource::Restored {
            repository,
            original_root_url,
        } => {
            let archive = verify_archive(
                repository,
                original_root_url,
                pair,
                &mirror,
                &trees,
                &active,
            )
            .await
            .map_err(|e| HistoricalVerifyError::specific_or(&e, HistoricalVerifyError::Archive))?;
            (false, Some(archive))
        }
        HistoricalSvnSource::PlannedCutover => (true, None),
    };
    require_stable_source(live, &active, pair, false)
        .await
        .context(HistoricalVerifyError::SvnHistory)?;
    require_local_ref(&mirror, &reference, tip).context(HistoricalVerifyError::GitRef)?;
    registered_ref(&git_url, &reference, tip, token).context(HistoricalVerifyError::GitRef)?;
    let id = active.identity;
    let history = digest(&json!({
        "profile": "legacy_import_history_v1",
        "credential_context_sha256": witness.credential_context_sha256,
        "repo_id": pair.repo_id,
        "svn_root": id.root_url,
        "svn_branch": id.branch_path,
        "svn_url": id.url,
        "svn_uuid": id.uuid,
        "created": id.incarnation_created_rev,
        "git_provider": pair.git_provider,
        "git_api_url": pair.git_api_url,
        "git_repo": pair.git_repo,
        "git_url": git_url.as_str(),
        "git_ref": reference,
        "last_git_sha": pair.last_git_sha,
        "last_svn_rev": pair.last_svn_rev,
        "mappings": pair.mappings,
        "original_import_completion_revision": pair.import_completion_svn_rev,
        "original_import_log_entries": pair.import_log_entry_count,
        "branch_log_sha256": active.log,
        "projected_trees_sha256": active.snapshots
    }))?;
    let provenance = json!({
        "profile": "legacy_import_history_v1",
        "source": if planned { "planned_cutover" } else { "trusted_restore" },
        "svn_uuid": id.uuid,
        "svn_root_url": id.root_url,
        "svn_branch": id.branch_path,
        "creation_revision": id.incarnation_created_rev,
        "through_revision": pair.last_svn_rev,
        "reviewed_svn_floor": reviewed_floor,
        "verified_branch_revisions": active.branch_revisions,
        "original_import_completion_revision": pair.import_completion_svn_rev,
        "mapping_count": pair.mappings.len(),
        "git_url": git_url.as_str(),
        "git_ref": reference,
        "git_tip": pair.last_git_sha,
        "git_mirror_location_sha256": digest(&mirror)?,
        "archive": archive,
        "branch_log_sha256": active.log,
        "projected_trees_sha256": active.snapshots,
        "history_sha256": history
    });
    let review = digest(&("legacy_import_review_v1", pair, &provenance))?;
    let lineage = CanonicalLineage {
        scope_uuid: pair.scope_uuid.clone(),
        generation: pair.generation,
        repo_id: pair.repo_id.clone(),
        svn_uuid: id.uuid,
        svn_root_url: id.root_url,
        svn_url: id.url,
        svn_branch_path: id.branch_path,
        incarnation_created_rev: id.incarnation_created_rev,
        copy_from_path: None,
        copy_from_rev: None,
        git_baseline_sha: Some(pair.last_git_sha.clone()),
        git_ref: Some(pair.git_branch.clone()),
        projection_version: String::new(),
        policy_hash: String::new(),
    };
    Ok(VerifiedHistoricalLineage {
        trees,
        reviewed_floor,
        branch_revisions: active.branch_revisions,
        pair: pair.clone(),
        lineage,
        history,
        review,
        planned,
        provenance,
    })
}

#[cfg(test)]
#[path = "legacy_svn_recovery_tests.rs"]
mod tests;
