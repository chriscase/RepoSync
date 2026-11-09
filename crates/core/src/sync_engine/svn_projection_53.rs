//! #53 SVN→Git filtering: a verified projection of genuine SVN revisions.
//!
//! Before Git fetch/reset and conflict detection, the engine prepares one
//! projected file-level plan per fetched SVN revision from the actual
//! `svn diff` of that revision, validated against the revision's changed-path
//! log (already mapped exactly once to the canonical branch-relative
//! namespace). Every revision, its identity, metadata and order are kept even
//! when its selected file view is empty.
//!
//! * A revision with selected file content feeds only its selected diff
//!   sections to the unchanged apply/LFS/commit/push/observe path.
//! * A revision whose selected Git-content effect is genuinely zero is
//!   finalized as `intentionally_filtered_no_target` in one journal
//!   transaction, with no Git commit, push, inbound Git cursor movement or
//!   applied count.
//! * Anything that cannot be proven (unparseable diff text, a changed path
//!   without diff evidence, an ancestor directory add/delete/copy whose
//!   pinned export shows a selected file missing from the diff, a selected
//!   property with Git meaning but no Git conversion, a read/export failure)
//!   holds that revision explicitly; later revisions stay pending.
//!
//! [`VerifiedSvnProjection`] is the only authority for filtered finalization.
//! It is crate-private, its fields are private, it does not implement
//! `Serialize`/`Deserialize`, and it has no Boolean, scalar or JSON
//! constructor: it is minted only from a prepared plan whose source was
//! observed in this cycle, and it borrows the actual ordinary writer window
//! and database for its whole life.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use super::{ChangedFile, SvnChangeSet, SyncEngine};
use crate::db::lineage_checkpoints::{
    load_canonical_lineage_tx, observe_live_svn_identity, LiveSvnIdentity,
};
use crate::db::nullable_commit_map::current_generation_read_only;
use crate::db::ordinary_writer_windows::OrdinaryWriterWindow;
use crate::db::queries::AuditLogInput;
use crate::db::Database;
use crate::errors::{DatabaseError, SyncError};
use crate::path_projection::{normalize_policy_path, path_is_projected, path_matches_prefix};
use crate::svn::client::SvnClient;

/// SVN properties whose Git representation (file mode, symlink) has no
/// conversion in the existing apply path. A selected change to one of them
/// with no selected file content is held, never treated as filtered.
const GIT_MEANING_PROPERTIES: [&str; 2] = ["svn:executable", "svn:special"];

// ---------------------------------------------------------------------------
// Diff evidence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Content,
    Property,
}

/// One node section of an `svn diff`, with its header line(s) included.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffSection {
    kind: SectionKind,
    path: String,
    text: String,
}

/// The branch root itself (property changes on the target URL).
fn is_root(path: &str) -> bool {
    path.is_empty() || path == "."
}

/// Split one `svn diff` into per-node sections. Every line belongs to the
/// section opened by the last `Index: ` or `Property changes on: ` header;
/// any non-blank text before the first header is refused. Paths are kept
/// literally as SVN printed them (no decoding or rewriting).
fn split_svn_diff(raw: &str) -> Result<Vec<DiffSection>, String> {
    let mut sections: Vec<DiffSection> = Vec::new();
    for line in raw.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        let opened = if let Some(path) = bare.strip_prefix("Index: ") {
            Some((SectionKind::Content, path))
        } else {
            bare.strip_prefix("Property changes on: ")
                .map(|path| (SectionKind::Property, path))
        };
        if let Some((kind, path)) = opened {
            sections.push(DiffSection {
                kind,
                path: path.to_owned(),
                text: String::new(),
            });
        }
        match sections.last_mut() {
            Some(section) => section.text.push_str(line),
            None if bare.trim().is_empty() => {}
            None => return Err("svn diff has text outside any node section".into()),
        }
    }
    Ok(sections)
}

/// Header facts of one content section, checked against its own path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ContentFacts {
    added: bool,
    deleted: bool,
    binary: bool,
    hunks: bool,
}

impl ContentFacts {
    /// A content section changes Git content only with a hunk, a binary
    /// marker, or an add/delete of the file. SVN prints a header-only
    /// `Index:` section (no hunk) for a property-only change; that is not
    /// file content.
    fn has_git_effect(self) -> bool {
        self.hunks || self.binary || self.added || self.deleted
    }
}

fn content_facts(section: &DiffSection) -> Result<ContentFacts, String> {
    let mut facts = ContentFacts::default();
    let old = format!("--- {}\t(", section.path);
    let new = format!("+++ {}\t(", section.path);
    for line in section.text.lines().skip(1) {
        if line.starts_with("@@") {
            facts.hunks = true;
            break;
        }
        if line.starts_with("Cannot display: file marked as a binary type.") {
            facts.binary = true;
        } else if let Some(rest) = line.strip_prefix("--- ") {
            if !line.starts_with(&old) {
                return Err(format!(
                    "diff header '--- {rest}' does not name section '{}'",
                    section.path
                ));
            }
            facts.added = line.ends_with("\t(nonexistent)");
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if !line.starts_with(&new) {
                return Err(format!(
                    "diff header '+++ {rest}' does not name section '{}'",
                    section.path
                ));
            }
            facts.deleted = line.ends_with("\t(nonexistent)");
        }
    }
    Ok(facts)
}

/// Property names a property section adds, modifies or deletes.
fn property_names(section: &DiffSection) -> Vec<&str> {
    section
        .text
        .lines()
        .skip(1)
        .filter_map(|line| {
            ["Added: ", "Modified: ", "Deleted: ", "Name: "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(str::trim)
        .collect()
}

/// True when `path` or any descendant of it can be selected by the policy.
/// Used only to decide which directory endpoints need pinned expansion.
fn selection_may_reach(path: &str, allowed: &[String], blocked: &[String]) -> bool {
    if allowed.is_empty() || path_is_projected(path, allowed, blocked) {
        return true;
    }
    let node = normalize_policy_path(path);
    let node = node.trim_end_matches('/');
    if node.is_empty() {
        return true;
    }
    allowed.iter().any(|prefix| {
        let prefix = normalize_policy_path(prefix);
        path_matches_prefix(node, &prefix) || prefix.starts_with(&format!("{node}/"))
    })
}

/// Every regular file or symlink below `root`, as `/`-joined relative paths.
fn exported_files(root: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read pinned export: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read pinned export: {e}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "pinned export contains a non-UTF-8 name".to_string())?;
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot read pinned export: {e}"))?;
            if kind.is_dir() {
                stack.push((entry.path(), rel));
            } else {
                found.push(rel);
            }
        }
    }
    found.sort();
    Ok(found)
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedEffect {
    /// Selected file content exists: only the selected sections are applied.
    Content,
    /// Genuinely zero selected Git-content effect: filtered finalization.
    Zero,
}

#[derive(Debug, Clone)]
enum PlanState {
    Ready {
        effect: SelectedEffect,
        selected_patch: String,
        files: Vec<ChangedFile>,
        counts: ProjectionCounts,
    },
    /// This revision's evidence could not be proven; it stops here.
    Held(String),
    /// After an earlier held revision: not prepared, stays pending.
    Pending,
}

#[derive(Debug, Clone)]
struct RevisionPlan {
    revision: i64,
    svn_author: String,
    state: PlanState,
}

/// The opaque plan batch for one cycle. It keeps the captured identity and
/// the borrows of the actual window and database the plans were made under.
pub(crate) struct PreparedSvnBatch<'a, 'db> {
    db: &'a Database,
    window: &'a OrdinaryWriterWindow<'db>,
    identity: CapturedIdentity,
    plans: Vec<RevisionPlan>,
}

impl<'a, 'db> PreparedSvnBatch<'a, 'db> {
    /// Mint the filtered-finalization capability for one zero-effect plan.
    fn verified_filtered(
        &self,
        plan: &RevisionPlan,
        counts: ProjectionCounts,
    ) -> VerifiedSvnProjection<'a, 'db> {
        VerifiedSvnProjection {
            db: self.db,
            window: self.window,
            identity: self.identity.clone(),
            revision: plan.revision,
            svn_author: plan.svn_author.clone(),
            counts,
        }
    }
}

/// Classify one revision's validated diff sections under the policy.
///
/// Any selected change to a property with Git meaning (`svn:executable`,
/// `svn:special`: mode or symlink) is refused, whatever other selected
/// content the revision has. The unchanged apply path has no conversion for
/// it, so applying the rest would lose selected semantics while advancing the
/// revision. Otherwise selected content gives `Content` (only the selected
/// sections are applied), and its absence gives `Zero`.
fn select_revision_sections(
    sections: &[DiffSection],
    facts: &[Option<ContentFacts>],
    allowed: &[String],
    blocked: &[String],
) -> Result<PlanState, String> {
    let mut source: Vec<&str> = Vec::new();
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut selected_patch = String::new();
    let mut selected_content = false;
    let mut git_meaning: Vec<String> = Vec::new();
    for (section, fact) in sections.iter().zip(facts) {
        if is_root(&section.path) {
            continue;
        }
        if !source.contains(&section.path.as_str()) {
            source.push(&section.path);
        }
        if !path_is_projected(&section.path, allowed, blocked) {
            continue;
        }
        if section.kind == SectionKind::Property {
            for name in property_names(section) {
                if GIT_MEANING_PROPERTIES.contains(&name) {
                    git_meaning.push(format!("{name} on '{}'", section.path));
                }
            }
        }
        selected_patch.push_str(&section.text);
        let action = match fact {
            Some(f) if f.added => "A",
            Some(f) if f.deleted => "D",
            _ => "M",
        };
        if let Some(f) = fact.filter(|f| f.has_git_effect()) {
            selected_content = true;
            files.retain(|file| file.path != section.path);
            files.push(ChangedFile {
                path: section.path.clone(),
                action: action.into(),
                content: None,
                is_binary: f.binary,
                rename_from: None,
            });
        } else if !files.iter().any(|file| file.path == section.path) {
            files.push(ChangedFile {
                path: section.path.clone(),
                action: "M".into(),
                content: None,
                is_binary: false,
                rename_from: None,
            });
        }
    }
    if !git_meaning.is_empty() {
        return Err(format!(
            "selected property change with Git meaning has no Git conversion: {}",
            git_meaning.join(", ")
        ));
    }
    let included = files.len();
    let counts = ProjectionCounts {
        source: source.len(),
        included,
        excluded: source.len().saturating_sub(included),
    };
    let effect = if selected_content {
        SelectedEffect::Content
    } else {
        SelectedEffect::Zero
    };
    Ok(PlanState::Ready {
        effect,
        selected_patch,
        files,
        counts,
    })
}

/// Observation-only capture for native fixtures: debug builds only, and only when
/// `REPOSYNC_RS53_OBSERVE_DIR` is set. It writes the raw `svn diff` the producer
/// actually read, the changed-path log it validated against, and the plan it
/// actually computed (selected patch, endpoints, counts, effect, or hold detail).
/// It never recomputes or alters a plan, and write failures are ignored.
#[cfg(debug_assertions)]
fn observe_plan_53(change: &SvnChangeSet, raw: &str, outcome: &Result<PlanState, String>) {
    let Some(dir) = std::env::var_os("REPOSYNC_RS53_OBSERVE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let rev = change.revision;
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("r{rev}.raw.diff")), raw);
    let log: Vec<(&str, &str, Option<&str>)> = change
        .changed_files
        .iter()
        .map(|f| (f.path.as_str(), f.action.as_str(), f.rename_from.as_deref()))
        .collect();
    let summary = match outcome {
        Ok(PlanState::Ready {
            effect,
            selected_patch,
            files,
            counts,
        }) => {
            let _ = std::fs::write(dir.join(format!("r{rev}.selected.patch")), selected_patch);
            let endpoints: Vec<(&str, &str, bool)> = files
                .iter()
                .map(|f| (f.path.as_str(), f.action.as_str(), f.is_binary))
                .collect();
            format!("effect={effect:?}\ncounts={counts:?}\nendpoints={endpoints:?}\n")
        }
        Ok(other) => format!("state={other:?}\n"),
        Err(detail) => format!("held={detail}\n"),
    };
    let _ = std::fs::write(
        dir.join(format!("r{rev}.plan.txt")),
        format!("revision={rev}\nlog={log:?}\n{summary}"),
    );
}

fn policy_identity(policy_snapshot: &str) -> String {
    hex::encode(Sha256::digest(policy_snapshot.as_bytes()))[..16].to_owned()
}

fn held_error(revision: i64, detail: &str) -> SyncError {
    SyncError::HistoryBlocked {
        reason: "svn_projection_held".into(),
        detail: format!("SVN r{revision} is held: {detail}; it and later revisions remain pending"),
    }
}

impl SyncEngine {
    /// E01: the actual fetch, then the projected file-level view and plan
    /// batch. Empty allowed and blocked rules, an empty fetch, or no managed
    /// repository scope keep the old behavior exactly (no batch).
    pub(super) async fn prepare_svn_projection_53<'a, 'db>(
        &'a self,
        writer_window: &'a OrdinaryWriterWindow<'db>,
    ) -> Result<(Vec<SvnChangeSet>, Option<PreparedSvnBatch<'a, 'db>>), SyncError> {
        let fetched = self.fetch_svn_changes().await?;
        if self.allowed_paths.is_empty() && self.blocked_patterns.is_empty() {
            return Ok((fetched, None));
        }
        let Some(repo_id) = self.effective_repo_id() else {
            return Ok((fetched, None));
        };
        if fetched.is_empty() {
            return Ok((fetched, None));
        }
        let svn = self
            .svn_client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let info = svn.info().await.map_err(SyncError::SvnError)?;
        let live = observe_live_svn_identity(&svn, &info)
            .await
            .map_err(SyncError::SvnError)?;
        let identity = CapturedIdentity::capture(
            &self.db,
            writer_window,
            repo_id,
            &live,
            &self.no_target_projection(),
        )
        .map_err(SyncError::DatabaseError)?;
        let mut projected = Vec::with_capacity(fetched.len());
        let mut plans = Vec::with_capacity(fetched.len());
        let mut held = false;
        for change in &fetched {
            let state = if held {
                PlanState::Pending
            } else {
                match self.plan_svn_revision_53(&svn, change).await {
                    Ok(state) => state,
                    Err(detail) => {
                        held = true;
                        PlanState::Held(detail)
                    }
                }
            };
            // Unproven revisions keep their complete log view, so conflict
            // detection stays conservative for them.
            let changed_files = match &state {
                PlanState::Ready { files, .. } => files.clone(),
                PlanState::Held(_) | PlanState::Pending => change.changed_files.clone(),
            };
            projected.push(SvnChangeSet {
                changed_files,
                ..change.clone()
            });
            plans.push(RevisionPlan {
                revision: change.revision,
                svn_author: change.author.clone(),
                state,
            });
        }
        Ok((
            projected,
            Some(PreparedSvnBatch {
                db: &self.db,
                window: writer_window,
                identity,
                plans,
            }),
        ))
    }

    /// Build one revision's plan from its actual diff and changed-path log.
    async fn plan_svn_revision_53(
        &self,
        svn: &SvnClient,
        change: &SvnChangeSet,
    ) -> Result<PlanState, String> {
        let raw = svn
            .diff_full(change.revision)
            .await
            .map_err(|e| format!("svn diff read failed: {e}"))?;
        let outcome = self.plan_from_diff_53(svn, change, &raw).await;
        #[cfg(debug_assertions)]
        observe_plan_53(change, &raw, &outcome);
        outcome
    }

    /// The plan for one revision from the diff text actually read for it.
    async fn plan_from_diff_53(
        &self,
        svn: &SvnClient,
        change: &SvnChangeSet,
        raw: &str,
    ) -> Result<PlanState, String> {
        let rev = change.revision;
        let allowed = &self.allowed_paths;
        let blocked = &self.blocked_patterns;
        let sections = split_svn_diff(raw)?;
        let mut facts = Vec::with_capacity(sections.len());
        for section in &sections {
            // The branch root has no Git file. A header-only root section
            // (a root property change, e.g. merge metadata) is allowed.
            if is_root(&section.path)
                && section.kind == SectionKind::Content
                && content_facts(section)?.has_git_effect()
            {
                return Err("svn diff shows file content for the branch root".into());
            }
            if !is_root(&section.path)
                && !change.changed_files.iter().any(|entry| {
                    entry.path == section.path
                        || section.path.starts_with(&format!("{}/", entry.path))
                })
            {
                return Err(format!(
                    "diff section '{}' has no matching changed path in the revision log",
                    section.path
                ));
            }
            facts.push(match section.kind {
                SectionKind::Content => Some(content_facts(section)?),
                SectionKind::Property => None,
            });
        }
        for entry in &change.changed_files {
            if !selection_may_reach(&entry.path, allowed, blocked) {
                continue;
            }
            let exact_content = sections
                .iter()
                .any(|s| s.kind == SectionKind::Content && s.path == entry.path);
            match entry.action.as_str() {
                "M" => {
                    if !sections.iter().any(|s| s.path == entry.path) {
                        return Err(format!(
                            "modified path '{}' has no diff evidence",
                            entry.path
                        ));
                    }
                }
                "A" | "R" | "D" if !exact_content => {
                    // A directory (or a file without evidence): expand its
                    // descendants at the pinned revisions it existed at.
                    let mut pins = Vec::new();
                    if entry.action != "D" {
                        pins.push(rev);
                    }
                    if entry.action == "D" || (entry.action == "R" && entry.rename_from.is_none()) {
                        pins.push(rev - 1);
                    }
                    for pin in pins {
                        self.expand_node_53(svn, &entry.path, pin, &sections)
                            .await?;
                    }
                }
                "A" | "R" | "D" => {}
                other => {
                    return Err(format!(
                        "unsupported changed-path action '{other}' for '{}'",
                        entry.path
                    ));
                }
            }
        }
        select_revision_sections(&sections, &facts, allowed, blocked)
    }

    /// Export one node at a pinned revision into an owned temporary
    /// directory and require diff evidence for every selected file below it.
    async fn expand_node_53(
        &self,
        svn: &SvnClient,
        node: &str,
        pin: i64,
        sections: &[DiffSection],
    ) -> Result<(), String> {
        let allowed = &self.allowed_paths;
        let blocked = &self.blocked_patterns;
        let scratch =
            tempfile::tempdir().map_err(|e| format!("cannot create export scratch: {e}"))?;
        let dest = scratch.path().join("node");
        svn.export(node, pin, &dest)
            .await
            .map_err(|e| format!("pinned export of '{node}' at r{pin} failed: {e}"))?;
        let metadata = std::fs::symlink_metadata(&dest)
            .map_err(|e| format!("pinned export of '{node}' at r{pin} is unreadable: {e}"))?;
        if !metadata.is_dir() {
            if path_is_projected(node, allowed, blocked) {
                return Err(format!(
                    "file '{node}' changed at r{} without diff evidence",
                    pin.max(1)
                ));
            }
            return Ok(());
        }
        for rel in exported_files(&dest)? {
            let path = format!("{node}/{rel}");
            if path_is_projected(&path, allowed, blocked)
                && !sections
                    .iter()
                    .any(|s| s.kind == SectionKind::Content && s.path == path)
            {
                return Err(format!(
                    "selected file '{path}' under changed directory '{node}' (r{pin}) has no diff evidence"
                ));
            }
        }
        Ok(())
    }

    /// E04: consume the matching plan. `Ok(Some(patch))` feeds only the
    /// selected sections to the unchanged apply path; `Ok(None)` means the
    /// revision was finalized as filtered with no Git effect.
    pub(super) fn consume_svn_projection_plan_53(
        &self,
        batch: &PreparedSvnBatch<'_, '_>,
        change: &SvnChangeSet,
    ) -> Result<Option<String>, SyncError> {
        let rev = change.revision;
        let Some(plan) = batch.plans.iter().find(|plan| plan.revision == rev) else {
            return Err(held_error(rev, "no prepared projection plan matches it"));
        };
        let policy = policy_identity(&batch.identity.policy_snapshot);
        match &plan.state {
            PlanState::Held(detail) => {
                warn!(rev, repo_id = %batch.identity.repo_id, %detail, "SVN projection held; stopping at durable frontier");
                let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                    action: "svn_to_git_projection_held",
                    direction: Some("svn_to_git"),
                    svn_rev: Some(rev),
                    git_sha: None,
                    author: Some(&change.author),
                    details: Some(detail),
                    success: false,
                    repo_id: Some(&batch.identity.repo_id),
                });
                Err(held_error(rev, detail))
            }
            PlanState::Pending => Err(held_error(rev, "an earlier revision is held")),
            PlanState::Ready {
                effect: SelectedEffect::Content,
                selected_patch,
                counts,
                ..
            } => {
                info!(
                    rev,
                    repo_id = %batch.identity.repo_id,
                    source = counts.source,
                    included = counts.included,
                    excluded = counts.excluded,
                    policy = %policy,
                    "SVN revision projected; applying selected content only"
                );
                Ok(Some(selected_patch.clone()))
            }
            PlanState::Ready {
                effect: SelectedEffect::Zero,
                counts,
                ..
            } => {
                let proof = batch.verified_filtered(plan, *counts);
                let (pre_svn_rev, pre_git_sha) =
                    self.cycle_mapping_pre_write_watermarks(proof.repo_id())?;
                self.db
                    .record_svn_filtered_mapping(&proof, pre_svn_rev, &pre_git_sha)
                    .map_err(SyncError::DatabaseError)?;
                let counts = proof.counts();
                let details = format!(
                    "All changes outside the active path policy (policy {policy}): \
                     {} source, {} included, {} excluded file endpoints",
                    counts.source, counts.included, counts.excluded
                );
                self.db
                    .insert_audit_log_with_repo(AuditLogInput {
                        action: "svn_to_git_filtered",
                        direction: Some("svn_to_git"),
                        svn_rev: Some(rev),
                        git_sha: None,
                        author: Some(&change.author),
                        details: Some(&details),
                        success: true,
                        repo_id: Some(proof.repo_id()),
                    })
                    .map_err(SyncError::DatabaseError)?;
                info!(
                    rev,
                    repo_id = %proof.repo_id(),
                    source = counts.source,
                    included = counts.included,
                    excluded = counts.excluded,
                    policy = %policy,
                    "SVN revision fully filtered: no Git commit, push or applied count"
                );
                Ok(None)
            }
        }
    }
}

/// Identity read from one current observation of the database, the writer
/// fence and the live SVN repository. Compared again, field by field, inside
/// the journal transaction that consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedIdentity {
    repo_id: String,
    scope_uuid: String,
    generation: i64,
    writer_epoch: Option<u64>,
    svn_uuid: String,
    svn_root_url: String,
    svn_branch_path: String,
    incarnation_created_rev: i64,
    policy_snapshot: String,
}

/// Source/included/excluded file-endpoint counts for one revision, for
/// structured diagnostics only. They carry no authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ProjectionCounts {
    pub(crate) source: usize,
    pub(crate) included: usize,
    pub(crate) excluded: usize,
}

/// Opaque proof that one genuine SVN revision has zero selected Git-content
/// effect under the captured policy, scope, generation, writer epoch and SVN
/// lineage. Holding it borrows the actual writer window and database.
pub(crate) struct VerifiedSvnProjection<'a, 'db> {
    db: &'a Database,
    window: &'a OrdinaryWriterWindow<'db>,
    identity: CapturedIdentity,
    revision: i64,
    svn_author: String,
    counts: ProjectionCounts,
}

impl std::fmt::Debug for VerifiedSvnProjection<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedSvnProjection")
            .field("repo_id", &self.identity.repo_id)
            .field("revision", &self.revision)
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

fn refused(detail: impl Into<String>) -> DatabaseError {
    DatabaseError::Other(format!(
        "svn filtered projection refused: {}",
        detail.into()
    ))
}

/// Read-only snapshot of the scope identity inside one SQLite transaction.
/// No I/O and no nested database call happen under it.
fn read_scope_snapshot(
    tx: &Connection,
    repo_id: &str,
) -> Result<(String, i64, Option<u64>), DatabaseError> {
    let writer_epoch = crate::writer_fence::ordinary_current_epoch(tx)?;
    let scope: Option<String> = tx
        .query_row(
            "SELECT scope_uuid FROM repositories WHERE id = ?1",
            [repo_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|value| !value.is_empty());
    let Some(scope) = scope else {
        return Err(refused(
            "repository registration or its scope identity is missing",
        ));
    };
    let generation = current_generation_read_only(tx, repo_id, &scope)?;
    Ok((scope, generation, writer_epoch))
}

/// The repository's admitted path policy, read from its registration row
/// exactly as the scheduler builds the engine's rules (each column is a JSON
/// array of strings; NULL is empty; anything else is refused), rendered in the
/// same shape as the engine's policy snapshot (`no_target_projection`). This
/// is the authority both capture and revalidation compare against; no second
/// policy store exists.
fn read_admitted_policy_tx(tx: &Connection, repo_id: &str) -> Result<String, DatabaseError> {
    let row: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT allowed_paths, blocked_patterns FROM repositories WHERE id = ?1",
            [repo_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((allowed, blocked)) = row else {
        return Err(refused("repository registration is missing"));
    };
    let parse = |raw: Option<String>, setting: &str| -> Result<Vec<String>, DatabaseError> {
        match raw {
            None => Ok(Vec::new()),
            Some(raw) => serde_json::from_str::<Vec<String>>(&raw).map_err(|_| {
                refused(format!(
                    "repository {setting} is not a JSON array of strings"
                ))
            }),
        }
    };
    let allowed = parse(allowed, "allowed_paths")?;
    let blocked = parse(blocked, "blocked_patterns")?;
    Ok(serde_json::json!({"allowed_paths": allowed, "blocked_patterns": blocked}).to_string())
}

impl CapturedIdentity {
    /// Capture outside any SQLite reservation, except one read-only snapshot.
    /// `require_engine_scope` runs before and after the snapshot, so the
    /// recorded epoch is the one the actual window was admitted under.
    fn capture(
        db: &Database,
        window: &OrdinaryWriterWindow<'_>,
        repo_id: &str,
        live: &LiveSvnIdentity,
        policy_snapshot: &str,
    ) -> Result<Self, DatabaseError> {
        window.require_engine_scope(db, Some(repo_id))?;
        let (scope_uuid, generation, writer_epoch, lineage, admitted) = db.transaction(|tx| {
            let (scope, generation, epoch) = read_scope_snapshot(tx, repo_id)?;
            let lineage = load_canonical_lineage_tx(tx, &scope, generation)?;
            let admitted = read_admitted_policy_tx(tx, repo_id)?;
            Ok((scope, generation, epoch, lineage, admitted))
        })?;
        window.require_engine_scope(db, Some(repo_id))?;
        if admitted != policy_snapshot {
            return Err(refused(
                "engine path policy differs from the repository's admitted policy",
            ));
        }
        let Some(lineage) = lineage else {
            return Err(refused(
                "no canonical SVN lineage is pinned for the current scope generation",
            ));
        };
        if lineage.repo_id != repo_id
            || lineage.svn_uuid != live.uuid
            || lineage.svn_root_url != live.root_url
            || lineage.svn_branch_path != live.branch_path
            || lineage.incarnation_created_rev != live.incarnation_created_rev
        {
            return Err(refused(
                "live SVN identity differs from the pinned canonical lineage",
            ));
        }
        Ok(Self {
            repo_id: repo_id.to_owned(),
            scope_uuid,
            generation,
            writer_epoch,
            svn_uuid: live.uuid.clone(),
            svn_root_url: live.root_url.clone(),
            svn_branch_path: live.branch_path.clone(),
            incarnation_created_rev: live.incarnation_created_rev,
            policy_snapshot: policy_snapshot.to_owned(),
        })
    }

    /// Compare every captured field (epoch, scope, generation, admitted path
    /// policy, canonical SVN lineage) with the current database state inside
    /// the caller's journal transaction.
    fn revalidate_tx(&self, tx: &Connection) -> Result<(), DatabaseError> {
        let (scope, generation, epoch) = read_scope_snapshot(tx, &self.repo_id)?;
        if epoch != self.writer_epoch {
            return Err(refused("writer epoch changed since the projection"));
        }
        if scope != self.scope_uuid {
            return Err(refused("repository scope changed since the projection"));
        }
        if generation != self.generation {
            return Err(refused("scope generation changed since the projection"));
        }
        if read_admitted_policy_tx(tx, &self.repo_id)? != self.policy_snapshot {
            return Err(refused(
                "repository admitted path policy changed since the projection",
            ));
        }
        let lineage = load_canonical_lineage_tx(tx, &scope, generation)?
            .ok_or_else(|| refused("canonical SVN lineage disappeared"))?;
        if lineage.repo_id != self.repo_id
            || lineage.svn_uuid != self.svn_uuid
            || lineage.svn_root_url != self.svn_root_url
            || lineage.svn_branch_path != self.svn_branch_path
            || lineage.incarnation_created_rev != self.incarnation_created_rev
        {
            return Err(refused(
                "canonical SVN lineage changed since the projection",
            ));
        }
        Ok(())
    }
}

impl<'a, 'db> VerifiedSvnProjection<'a, 'db> {
    pub(crate) fn repo_id(&self) -> &str {
        &self.identity.repo_id
    }

    pub(crate) fn revision(&self) -> i64 {
        self.revision
    }

    pub(crate) fn svn_author(&self) -> &str {
        &self.svn_author
    }

    pub(crate) fn policy_snapshot(&self) -> &str {
        &self.identity.policy_snapshot
    }

    /// Scope captured at projection time; `revalidate_tx` proves it current.
    pub(crate) fn scope_uuid(&self) -> &str {
        &self.identity.scope_uuid
    }

    /// Generation captured at projection time; `revalidate_tx` proves it current.
    pub(crate) fn generation(&self) -> i64 {
        self.identity.generation
    }

    pub(crate) fn counts(&self) -> ProjectionCounts {
        self.counts
    }

    /// The actual owner check, outside any SQLite reservation: the journal
    /// database must be the capability's database and the borrowed window
    /// must still own this engine scope.
    pub(crate) fn require_owner(&self, db: &Database) -> Result<(), DatabaseError> {
        if !std::ptr::eq::<Database>(self.db, db) {
            return Err(refused("journal database is not the projection's database"));
        }
        self.window
            .require_engine_scope(db, Some(&self.identity.repo_id))
    }

    /// Revalidate the captured identity inside the journal transaction.
    pub(crate) fn revalidate_tx(&self, tx: &Connection) -> Result<(), DatabaseError> {
        self.identity.revalidate_tx(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = "Index: team-a/new file.txt\n===================================================================\n--- team-a/new file.txt\t(nonexistent)\n+++ team-a/new file.txt\t(revision 4)\n@@ -0,0 +1 @@\n+admitted\nIndex: team-b/secret.txt\n===================================================================\n--- team-b/secret.txt\t(revision 3)\n+++ team-b/secret.txt\t(revision 4)\n@@ -1 +1 @@\n-old\n+--- Index: not a header\nIndex: team-a/gone.txt\n===================================================================\n--- team-a/gone.txt\t(revision 3)\n+++ team-a/gone.txt\t(nonexistent)\n@@ -1 +0,0 @@\n-bye\n\nProperty changes on: team-a/tool.sh\n___________________________________________________________________\nAdded: svn:executable\n## -0,0 +1 ##\n+*\n\\ No newline at end of property\n";

    #[test]
    fn splits_sections_literally_and_keeps_hunk_lines_inside() {
        let sections = split_svn_diff(MIXED).unwrap();
        let paths: Vec<_> = sections.iter().map(|s| (s.kind, s.path.as_str())).collect();
        assert_eq!(
            paths,
            vec![
                (SectionKind::Content, "team-a/new file.txt"),
                (SectionKind::Content, "team-b/secret.txt"),
                (SectionKind::Content, "team-a/gone.txt"),
                (SectionKind::Property, "team-a/tool.sh"),
            ]
        );
        assert!(sections[1].text.contains("+--- Index: not a header\n"));
        let joined: String = sections.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, MIXED, "sections partition the diff byte for byte");
    }

    #[test]
    fn refuses_text_outside_sections_and_mismatched_headers() {
        assert!(split_svn_diff("svn: E170000: something\nIndex: a\n").is_err());
        assert!(split_svn_diff("\n\nIndex: a\n").is_ok());
        let wrong = split_svn_diff(
            "Index: a.txt\n====\n--- b.txt\t(revision 1)\n+++ a.txt\t(revision 2)\n",
        )
        .unwrap();
        assert!(content_facts(&wrong[0]).is_err());
    }

    #[test]
    fn content_facts_classify_add_delete_binary() {
        let sections = split_svn_diff(MIXED).unwrap();
        assert!(content_facts(&sections[0]).unwrap().added);
        assert!(content_facts(&sections[2]).unwrap().deleted);
        let binary = split_svn_diff(
            "Index: img.png\n====\nCannot display: file marked as a binary type.\nsvn:mime-type = application/octet-stream\n",
        )
        .unwrap();
        assert!(content_facts(&binary[0]).unwrap().binary);
    }

    #[test]
    fn header_only_section_of_a_property_change_has_no_git_effect() {
        let raw = "Index: team-a/a.txt\n===================================================================\n--- team-a/a.txt\t(revision 2)\n+++ team-a/a.txt\t(revision 3)\n\nProperty changes on: team-a/a.txt\n___________________________________________________________________\nAdded: custom:note\n## -0,0 +1 ##\n+value\n\\ No newline at end of property\n";
        let sections = split_svn_diff(raw).unwrap();
        assert_eq!(sections.len(), 2);
        let facts = content_facts(&sections[0]).unwrap();
        assert!(!facts.has_git_effect(), "{facts:?}");
        assert_eq!(property_names(&sections[1]), vec!["custom:note"]);
        let empty_add = split_svn_diff(
            "Index: e.txt\n====\n--- e.txt\t(nonexistent)\n+++ e.txt\t(revision 2)\n",
        )
        .unwrap();
        assert!(content_facts(&empty_add[0]).unwrap().has_git_effect());
        let sections = split_svn_diff(MIXED).unwrap();
        assert!(content_facts(&sections[1]).unwrap().has_git_effect());
    }

    #[test]
    fn root_property_header_has_no_git_effect() {
        let raw = "Index: .\n====\n--- .\t(revision 4)\n+++ .\t(revision 5)\n\nProperty changes on: .\n___\nModified: svn:mergeinfo\n";
        let sections = split_svn_diff(raw).unwrap();
        assert!(is_root(&sections[0].path) && is_root(&sections[1].path));
        assert!(!content_facts(&sections[0]).unwrap().has_git_effect());
    }

    #[test]
    fn property_names_and_git_meaning() {
        let sections = split_svn_diff(MIXED).unwrap();
        assert_eq!(property_names(&sections[3]), vec!["svn:executable"]);
        assert!(GIT_MEANING_PROPERTIES.contains(&"svn:executable"));
    }

    #[test]
    fn selection_reach_is_component_aware() {
        let allowed = vec!["team-a/".to_string()];
        assert!(selection_may_reach("team-a", &allowed, &[]));
        assert!(selection_may_reach("team-a/x", &allowed, &[]));
        assert!(!selection_may_reach("team-ab", &allowed, &[]));
        assert!(!selection_may_reach("team-b", &allowed, &[]));
        let nested = vec!["src/team-a".to_string()];
        assert!(selection_may_reach("src", &nested, &[]));
        assert!(!selection_may_reach("srcx", &nested, &[]));
        assert!(selection_may_reach("anything", &[], &["*.log".to_string()]));
    }

    #[test]
    fn exported_files_lists_nested_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        std::fs::write(dir.path().join("a/b/c.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("top @ -x.txt"), b"y").unwrap();
        assert_eq!(
            exported_files(dir.path()).unwrap(),
            vec!["a/b/c.txt".to_string(), "top @ -x.txt".to_string()]
        );
    }

    // -- RS53-R1: Git-meaning properties hold regardless of other content --

    fn classify(raw: &str, allowed: &[&str]) -> Result<PlanState, String> {
        let sections = split_svn_diff(raw).unwrap();
        let facts: Vec<_> = sections
            .iter()
            .map(|s| match s.kind {
                SectionKind::Content => Some(content_facts(s).unwrap()),
                SectionKind::Property => None,
            })
            .collect();
        let allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        select_revision_sections(&sections, &facts, &allowed, &[])
    }

    const CONTENT_A: &str = "Index: team-a/a.txt\n====\n--- team-a/a.txt\t(revision 1)\n+++ team-a/a.txt\t(revision 2)\n@@ -1 +1 @@\n-old\n+new\n";

    #[test]
    fn selected_executable_on_another_file_holds_despite_selected_content() {
        let raw = format!(
            "{CONTENT_A}Index: team-a/tool.sh\n====\n--- team-a/tool.sh\t(revision 1)\n+++ team-a/tool.sh\t(revision 2)\n\nProperty changes on: team-a/tool.sh\n___\nAdded: svn:executable\n## -0,0 +1 ##\n+*\n"
        );
        let held = classify(&raw, &["team-a/"]).unwrap_err();
        assert!(
            held.contains("svn:executable on 'team-a/tool.sh'"),
            "{held}"
        );
        // The same Git-meaning change outside the selection does not hold.
        assert!(matches!(
            classify(
                &raw.replace("team-a/tool.sh", "team-b/tool.sh"),
                &["team-a/"]
            ),
            Ok(PlanState::Ready {
                effect: SelectedEffect::Content,
                ..
            })
        ));
    }

    #[test]
    fn selected_symlink_property_holds_even_when_its_own_content_is_selected() {
        let raw = format!(
            "{CONTENT_A}Index: team-a/link\n====\n--- team-a/link\t(nonexistent)\n+++ team-a/link\t(revision 2)\n@@ -0,0 +1 @@\n+link a.txt\n\\ No newline at end of file\n\nProperty changes on: team-a/link\n___\nAdded: svn:special\n## -0,0 +1 ##\n+*\n"
        );
        let held = classify(&raw, &["team-a/"]).unwrap_err();
        assert!(held.contains("svn:special on 'team-a/link'"), "{held}");
        assert!(matches!(
            classify(CONTENT_A, &["team-a/"]),
            Ok(PlanState::Ready {
                effect: SelectedEffect::Content,
                ..
            })
        ));
    }

    // -- RS53-R2: policy/scope/generation/lineage/owner drift refuses -------

    use crate::db::lineage_checkpoints::{pin_canonical_lineage, CanonicalLineage};
    use crate::db::repo_scope_identity::repo_echo_generation_kv_key;

    /// Window documents require a canonical UUID scope.
    const SCOPE: &str = "00000000-0000-4000-8000-000000000053";

    fn owned_pair_db(dir: &Path) -> Database {
        let db = Database::new(dir.join("state.db")).unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,scope_uuid,allowed_paths,last_svn_rev,last_git_sha)
                 VALUES ('pair','pair','file:///repo','','','','r','main',1,'t','t',?1,'[\"team-a/\"]',1,?2)",
                rusqlite::params![SCOPE, "a".repeat(40)],
            )
            .unwrap();
        let generation = {
            let conn = db.conn();
            current_generation_read_only(&conn, "pair", SCOPE).unwrap()
        };
        pin_canonical_lineage(
            &db.conn(),
            &CanonicalLineage {
                scope_uuid: SCOPE.into(),
                generation,
                repo_id: "pair".into(),
                svn_uuid: "uuid-53".into(),
                svn_root_url: "file:///repo".into(),
                svn_url: "file:///repo".into(),
                svn_branch_path: String::new(),
                incarnation_created_rev: 1,
                copy_from_path: None,
                copy_from_rev: None,
                git_baseline_sha: None,
                git_ref: None,
                projection_version: String::new(),
                policy_hash: String::new(),
            },
        )
        .unwrap();
        db
    }

    fn live() -> LiveSvnIdentity {
        LiveSvnIdentity {
            uuid: "uuid-53".into(),
            root_url: "file:///repo".into(),
            url: "file:///repo".into(),
            branch_path: String::new(),
            incarnation_created_rev: 1,
            copy_from_path: None,
            copy_from_rev: None,
        }
    }

    fn engine_policy(allowed: &[&str]) -> String {
        serde_json::json!({"allowed_paths": allowed, "blocked_patterns": Vec::<String>::new()})
            .to_string()
    }

    fn revalidates(db: &Database, identity: &CapturedIdentity) -> Result<(), DatabaseError> {
        db.transaction(|tx| identity.revalidate_tx(tx))
    }

    #[test]
    fn capture_refuses_an_engine_policy_that_is_not_the_admitted_policy() {
        let _lock = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let db = owned_pair_db(dir.path());
        let window = db.begin_ordinary_writer_window(Some("pair")).unwrap();
        let ok =
            CapturedIdentity::capture(&db, &window, "pair", &live(), &engine_policy(&["team-a/"]));
        assert!(ok.is_ok(), "{ok:?}");
        let stale =
            CapturedIdentity::capture(&db, &window, "pair", &live(), &engine_policy(&["team-b/"]))
                .unwrap_err();
        assert!(
            stale
                .to_string()
                .contains("differs from the repository's admitted policy"),
            "{stale}"
        );
    }

    #[test]
    fn revalidation_refuses_policy_scope_generation_and_lineage_drift() {
        let _lock = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let db = owned_pair_db(dir.path());
        let window = db.begin_ordinary_writer_window(Some("pair")).unwrap();
        let identity =
            CapturedIdentity::capture(&db, &window, "pair", &live(), &engine_policy(&["team-a/"]))
                .unwrap();
        revalidates(&db, &identity).unwrap();
        let generation = identity.generation;
        let drifts: [(&str, String, String, &str); 5] = [
            (
                "policy",
                "UPDATE repositories SET allowed_paths='[\"team-b/\"]' WHERE id='pair'".into(),
                "UPDATE repositories SET allowed_paths='[\"team-a/\"]' WHERE id='pair'".into(),
                "admitted path policy changed",
            ),
            (
                "malformed policy",
                "UPDATE repositories SET allowed_paths='team-a/' WHERE id='pair'".into(),
                "UPDATE repositories SET allowed_paths='[\"team-a/\"]' WHERE id='pair'".into(),
                "not a JSON array of strings",
            ),
            (
                "scope",
                "UPDATE repositories SET scope_uuid='00000000-0000-4000-8000-0000000000aa' WHERE id='pair'".into(),
                format!("UPDATE repositories SET scope_uuid='{SCOPE}' WHERE id='pair'"),
                "repository scope changed",
            ),
            (
                "generation",
                format!(
                    "INSERT INTO kv_state(key,value,updated_at) VALUES('{}','{}','t') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    repo_echo_generation_kv_key(SCOPE),
                    generation + 1
                ),
                format!(
                    "UPDATE kv_state SET value='{}' WHERE key='{}'",
                    generation,
                    repo_echo_generation_kv_key(SCOPE)
                ),
                "scope generation changed",
            ),
            (
                "lineage",
                format!("UPDATE canonical_lineage SET svn_uuid='uuid-other' WHERE scope_uuid='{SCOPE}'"),
                format!("UPDATE canonical_lineage SET svn_uuid='uuid-53' WHERE scope_uuid='{SCOPE}'"),
                "canonical SVN lineage changed",
            ),
        ];
        for (name, drift, undo, needle) in drifts {
            db.conn().execute_batch(&drift).unwrap();
            let error = revalidates(&db, &identity).unwrap_err();
            assert!(error.to_string().contains(needle), "{name}: {error}");
            db.conn().execute_batch(&undo).unwrap();
            revalidates(&db, &identity).unwrap_or_else(|e| panic!("{name} undo: {e}"));
        }
    }

    #[test]
    fn owner_checks_refuse_another_database_epoch_drift_and_an_expired_owner() {
        let _lock = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let db = owned_pair_db(dir.path());
        let _fence = crate::writer_fence::claim(&db, dir.path()).unwrap();
        let window = db.begin_ordinary_writer_window(Some("pair")).unwrap();
        let identity =
            CapturedIdentity::capture(&db, &window, "pair", &live(), &engine_policy(&["team-a/"]))
                .unwrap();
        assert!(
            identity.writer_epoch.is_some(),
            "the claimed lease epoch is captured"
        );
        let proof = VerifiedSvnProjection {
            db: &db,
            window: &window,
            identity,
            revision: 2,
            svn_author: "alice".into(),
            counts: ProjectionCounts::default(),
        };
        proof.require_owner(&db).unwrap();
        // Another database with identical bytes is not the owned resource.
        let other_dir = tempfile::tempdir().unwrap();
        let other = owned_pair_db(other_dir.path());
        let error = proof.require_owner(&other).unwrap_err();
        assert!(
            error.to_string().contains("not the projection's database"),
            "{error}"
        );
        // A takeover moves the lease epoch under the live window.
        let lease: String = db
            .conn()
            .query_row(
                "SELECT value FROM kv_state WHERE key='writer_fence_v1:lease'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut moved: serde_json::Value = serde_json::from_str(&lease).unwrap();
        moved["epoch"] = serde_json::json!(moved["epoch"].as_u64().unwrap() + 1);
        db.conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key='writer_fence_v1:lease'",
                [moved.to_string()],
            )
            .unwrap();
        assert!(
            proof.require_owner(&db).is_err(),
            "epoch drift refuses the owner"
        );
        assert!(
            db.transaction(|tx| proof.revalidate_tx(tx)).is_err(),
            "epoch drift refuses revalidation"
        );
        db.conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key='writer_fence_v1:lease'",
                [lease],
            )
            .unwrap();
        proof.require_owner(&db).unwrap();
        // Expired owner: the window's durable ownership document is gone (as
        // after startup recovery cleared it) while the capability is held.
        let documents: Vec<(String, String)> = {
            let conn = db.conn();
            let mut query = conn
                .prepare(
                    "SELECT key, value FROM kv_state \
                     WHERE key LIKE 'pair_refresh_writer_window_v1:document:%'",
                )
                .unwrap();
            let rows = query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert_eq!(documents.len(), 1, "exactly the live window's document");
        db.conn()
            .execute("DELETE FROM kv_state WHERE key = ?1", [&documents[0].0])
            .unwrap();
        assert!(proof.require_owner(&db).is_err(), "expired owner refuses");
        assert!(
            db.record_svn_filtered_mapping(&proof, 1, &"a".repeat(40))
                .is_err(),
            "no filtered record without the live owner"
        );
        db.conn()
            .execute(
                "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'t')",
                [&documents[0].0, &documents[0].1],
            )
            .unwrap();
        proof.require_owner(&db).unwrap();
    }

    #[test]
    fn filtered_record_refuses_drift_with_no_effect_then_commits_once() {
        let _lock = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let db = owned_pair_db(dir.path());
        let window = db.begin_ordinary_writer_window(Some("pair")).unwrap();
        let identity =
            CapturedIdentity::capture(&db, &window, "pair", &live(), &engine_policy(&["team-a/"]))
                .unwrap();
        let proof = VerifiedSvnProjection {
            db: &db,
            window: &window,
            identity,
            revision: 2,
            svn_author: "alice".into(),
            counts: ProjectionCounts::default(),
        };
        let pre_git = "a".repeat(40);
        let null_rows = |db: &Database| -> i64 {
            db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM commit_map WHERE git_sha IS NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        db.conn()
            .execute_batch("UPDATE repositories SET allowed_paths='[\"team-b/\"]' WHERE id='pair'")
            .unwrap();
        let error = db
            .record_svn_filtered_mapping(&proof, 1, &pre_git)
            .unwrap_err();
        assert!(
            error.to_string().contains("admitted path policy changed"),
            "{error}"
        );
        assert_eq!(null_rows(&db), 0);
        assert_eq!(db.get_repo_watermark("pair").unwrap().0, 1);
        assert!(db
            .get_state("team_cycle_mapping_v1:latest:pair")
            .unwrap()
            .is_none());
        assert!(
            crate::echo_receipt_scope::read_svn_no_target_receipt(&db, "pair", 2)
                .unwrap()
                .is_none()
        );
        db.conn()
            .execute_batch("UPDATE repositories SET allowed_paths='[\"team-a/\"]' WHERE id='pair'")
            .unwrap();
        let op = db.record_svn_filtered_mapping(&proof, 1, &pre_git).unwrap();
        assert_eq!(
            op.outcome,
            crate::db::team_cycle_mapping_operations::TeamCycleMappingOutcome::SvnFiltered
        );
        assert_eq!(
            op.state,
            crate::db::team_cycle_mapping_operations::TeamCycleMappingState::Completed
        );
        assert_eq!(null_rows(&db), 1);
        assert_eq!(db.get_repo_watermark("pair").unwrap().0, 2);
        let listed = db.list_commit_map_rows(Some("pair"), 10).unwrap();
        assert_eq!(
            listed[0].target,
            crate::db::nullable_commit_map::CommitMapTarget::FilteredNoTarget
        );
        assert!(db
            .active_team_cycle_mapping_operation("pair")
            .unwrap()
            .is_none());
    }
}
