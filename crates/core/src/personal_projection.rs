//! Scoped incremental Personal SVN→Git. Compare two pinned SVN endpoints,
//! never infer "no change" from a NULL row, an empty checkout, or a log path.
//! No file in an excluded Git subtree is overwritten or removed.

use std::{collections::BTreeMap, path::Path};

use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::db::lineage_checkpoints::{
    load_mapping_outcome, observe_historical_svn_identity_at, observe_live_svn_identity,
    record_mapping_outcome, svn_identity_difference, svn_source_key, CheckpointDirection,
    LiveSvnIdentity, MappingOutcome, MappingWrite,
};
use crate::db::nullable_commit_map::{
    current_generation_read_only, insert_proven_no_target_row_tx,
};
use crate::db::personal_scope::PERSONAL_SCOPE_KEY as SCOPE;
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::file_policy::FilePolicy;
use crate::import::{require_supported_export, IndependentTreeManifest, ManifestEntry};
use crate::pair_refresh::svn_visibility::{
    observe_subtree, prove_absent_child, VisibleSvnCheckout,
};
use crate::path_projection::path_is_projected;
use crate::personal_config::PersonalOptionsConfig;
use crate::svn::{SvnClient, SvnLogEntry};

const MODE_LATCH: &str = "personal_incremental_scope_mode_v1";

const PROJECTED_BASELINE: &str = "personal_projected_history_v1";
const PROJECTED_SOURCE: &str = "personal_projected_history_source_v1";

fn projected_binding(config: &crate::personal_config::PersonalConfig) -> String {
    let target = serde_json::json!({
        "policy": policy_snapshot(&config.options),
        "svn": config.svn.url,
        "git": config.github.clone_url(),
        "branch": config.github.default_branch,
    });
    hex::encode(Sha256::digest(target.to_string().as_bytes()))
}

/// Checks before clone, SVN read, or per-revision mappings. An existing
/// checkout must never be automatically rewritten to fit a newly edited scope.
pub fn require_import_mode(
    db: &Database,
    config: &crate::personal_config::PersonalConfig,
    full: bool,
) -> Result<()> {
    if !config.options.projected_history {
        ensure!(config.options.allowed_paths.is_empty(),
            "scoped Personal import is not supported without explicit projected_history; unrestricted import refused");
        return require_unrestricted(db);
    }
    ensure!(
        full,
        "scoped Personal snapshot refused; use a fresh projected full import"
    );
    let has_partial_history: bool = db.conn().query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_records WHERE repo_id IN (?1,?2))
                OR EXISTS(SELECT 1 FROM commit_map WHERE repo_id IN (?1,?2))",
        params![SCOPE, crate::db::personal_scope::LEGACY_PERSONAL_REPO_ID],
        |row| row.get(0),
    )?;
    ensure!(
        !has_partial_history
            && db.get_state(MODE_LATCH)?.is_none()
            && db.get_state(PROJECTED_BASELINE)?.is_none()
            && db.get_state(PROJECTED_SOURCE)?.is_none()
            && db.get_watermark("git_sha")?.is_none()
            && db.get_watermark("svn_rev")?.is_none(),
        "projected Personal history requires a fresh mirror and data directory; preserve the existing checkout and re-import"
    );
    Ok(())
}

/// Called only after exact remote full-import publication and watermarks were
/// verified. An interruption before this marker cannot enable reverse writes.
pub(crate) fn bind_projected_history_tx(
    tx: &rusqlite::Connection,
    config: &crate::personal_config::PersonalConfig,
    source: &LiveSvnIdentity,
) -> Result<(), DatabaseError> {
    if !config.options.projected_history {
        return Err(DatabaseError::Other(
            "projected import was not requested".into(),
        ));
    }
    tx.execute(
        "INSERT INTO kv_state (key,value,updated_at) VALUES (?1,?2,?3)",
        params![
            PROJECTED_BASELINE,
            projected_binding(config),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    tx.execute(
        "INSERT INTO kv_state (key,value,updated_at) VALUES (?1,?2,?3)",
        params![
            PROJECTED_SOURCE,
            projected_source_stamp(source),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

fn projected_source_stamp(source: &LiveSvnIdentity) -> String {
    serde_json::json!({
        "uuid": source.uuid, "root": source.root_url, "url": source.url,
        "branch": source.branch_path, "created_rev": source.incarnation_created_rev,
        "copy_from_path": source.copy_from_path, "copy_from_rev": source.copy_from_rev,
    })
    .to_string()
}

pub async fn require_projected_source(db: &Database, svn: &SvnClient) -> Result<()> {
    let prior = db
        .get_state(PROJECTED_SOURCE)?
        .context("Personal projected SVN source has not been verified")?;
    let now = observe_live_svn_identity(svn, &svn.info().await?).await?;
    ensure!(
        prior == projected_source_stamp(&now),
        "Personal projected SVN origin or branch incarnation changed; preserve the mirror"
    );
    Ok(())
}

pub fn require_matching_projected_history(
    db: &Database,
    config: &crate::personal_config::PersonalConfig,
) -> Result<()> {
    let prior = db.get_state(PROJECTED_BASELINE)?;
    if config.options.projected_history || prior.is_some() {
        ensure!(
            config.options.projected_history
                && prior.as_deref() == Some(projected_binding(config).as_str())
                && db.get_state(PROJECTED_SOURCE)?.is_some(),
            "Personal projection is unverified or changed; preserve this mirror and re-import into a fresh mirror and data directory"
        );
    }
    Ok(())
}

pub fn admit_reverse(db: &Database, config: &crate::personal_config::PersonalConfig) -> Result<()> {
    if config.options.projected_history {
        admit_sync(db, config)
    } else {
        ensure!(
            config.options.allowed_paths.is_empty(),
            "older scoped Personal mode is SVN-to-Git only"
        );
        require_unrestricted(db)
    }
}

pub fn policy_description(options: &PersonalOptionsConfig) -> String {
    let mode = if options.projected_history {
        "projected history (verified baseline required; merged PRs only)"
    } else if options.allowed_paths.is_empty() {
        "unrestricted legacy mode"
    } else {
        "existing scoped incremental SVN→Git only"
    };
    format!(
        "Personal path policy: {mode}; SVN allow {:?}; SVN block {:?}; rewrite {:?}",
        options.allowed_paths,
        options.blocked_patterns,
        options
            .path_rewrite_rules
            .iter()
            .map(|r| &r.strip_prefix)
            .collect::<Vec<_>>()
    )
}

/// Inspect the local checkout and its fetched mirror tip without fetching,
/// modifying refs, or resetting the checkout.
pub fn git_path_divergence(
    workdir: &Path,
    config: &crate::personal_config::PersonalConfig,
) -> Result<Vec<String>> {
    if config.options.allowed_paths.is_empty() {
        return Ok(Vec::new());
    }
    let repo = git2::Repository::open(workdir)?;
    let head = match repo.head() {
        Ok(head) => head.peel_to_tree()?,
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let cached_ref = format!("refs/remotes/origin/{}", config.github.default_branch);
    let mut trees = vec![head];
    match repo.find_reference(&cached_ref) {
        Ok(reference) => trees.push(reference.peel_to_tree()?),
        Err(e) if e.code() == git2::ErrorCode::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    projected_git_divergence(trees, config)
}

pub fn git_path_divergence_at(
    git_path: &Path,
    sha: &str,
    config: &crate::personal_config::PersonalConfig,
) -> Result<Vec<String>> {
    let repo = git2::Repository::open(git_path)?;
    let tree = repo.find_commit(git2::Oid::from_str(sha)?)?.tree()?;
    projected_git_divergence(vec![tree], config)
}

fn projected_git_divergence(
    trees: Vec<git2::Tree<'_>>,
    config: &crate::personal_config::PersonalConfig,
) -> Result<Vec<String>> {
    use git2::{ObjectType, TreeWalkMode, TreeWalkResult};
    let rule = config.options.rewrite_rule().map_err(anyhow::Error::msg)?;
    let mut divergent = std::collections::BTreeSet::new();
    for tree in trees {
        tree.walk(TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(ObjectType::Tree) {
                return TreeWalkResult::Ok;
            }
            if let Some(name) = entry.name() {
                let git = format!("{root}{name}");
                let svn = rule.as_ref().map_or_else(|| Ok(git.clone()), |r| r.to_svn(&git));
                if !matches!(svn, Ok(ref path) if path_is_projected(path, &config.options.allowed_paths, &config.options.blocked_patterns)) {
                    divergent.insert(git);
                }
            } else {
                divergent.insert(format!("{root}<non-UTF-8 path>"));
            }
            TreeWalkResult::Ok
        })?;
    }
    Ok(divergent.into_iter().collect())
}

/// No implicit downgrade: TOML omission is not an instruction to republish the
/// full tree or replay previously held PRs. Keep unrestricted installs unchanged.
pub fn require_unrestricted(db: &Database) -> Result<()> {
    ensure!(db.get_state(MODE_LATCH)?.is_none() && db.get_state(PROJECTED_BASELINE)?.is_none(),
        "this mirror was opened in scoped Personal mode; restore allowed_paths. An unrestricted transition requires a separately prepared mirror");
    Ok(())
}

pub fn admit_sync(db: &Database, config: &crate::personal_config::PersonalConfig) -> Result<()> {
    require_matching_projected_history(db, config)?;
    if config.options.allowed_paths.is_empty() {
        return require_unrestricted(db);
    }
    let target = serde_json::json!({"version": 1, "svn": config.svn.url,
        "git_base": config.github.clone_url(), "branch": config.github.default_branch})
    .to_string();
    let binding = hex::encode(Sha256::digest(target.as_bytes()));
    db.immediate_transaction(|tx| {
        Database::require_legacy_import_watermarks_available_tx(tx)?;
        let refused = || DatabaseError::Other("scoped Personal sync requires an existing proven SVN and Git checkpoint; refusing bootstrap".into());
        let mark = |name: &str| -> Result<String,DatabaseError> {
            tx.query_row("SELECT value FROM watermarks WHERE source=?1",[name],|r|r.get(0)).optional()?.ok_or_else(refused)
        };
        let rev = mark("svn_rev")?.parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(refused)?;
        let sha = mark("git_sha")?;
        // The two watermarks need not name the same publication. Both must
        // have Personal evidence; a global scalar or a Team receipt is not enough.
        let legacy = crate::db::personal_scope::LEGACY_PERSONAL_REPO_ID;
        let git_proven: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_records WHERE repo_id IN (?1,?2) AND status='applied' AND git_sha=?3)",params![SCOPE,legacy,sha],|r|r.get(0))?;
        let svn_applied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_records WHERE repo_id IN (?1,?2) AND status='applied' AND svn_rev=?3 AND git_sha IS NOT NULL AND length(git_sha)>0)",params![SCOPE,legacy,rev],|r|r.get(0))?;
        if !crate::history_inspect::is_full_git_oid(&sha) || !git_proven ||
            !(svn_applied || crate::db::nullable_commit_map::any_proven_no_target_row(tx,rev,Some(&[SCOPE]))?) {
            return Err(refused());
        }
        let prior: Option<String> = tx.query_row("SELECT value FROM kv_state WHERE key=?1",[MODE_LATCH],|r|r.get(0)).optional()?;
        match prior {
            Some(stored) if stored != binding => return Err(DatabaseError::Other("scoped Personal mirror source or destination binding changed; preserve this mirror".into())),
            Some(_) => {},
            None => {
                tx.execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)", params![MODE_LATCH,binding,chrono::Utc::now().to_rfc3339()])?;
            }
        }
        Ok(())
    })?;
    Ok(())
}

pub fn policy_snapshot(options: &PersonalOptionsConfig) -> String {
    let mut value = serde_json::json!({
        "allowed_paths": options.allowed_paths,
        "blocked_patterns": options.blocked_patterns,
        "network_directory_profile": options.network_directory_profile,
        "ignore_patterns": options.ignore_patterns,
        "max_file_size": options.max_file_size,
        "lfs_patterns": options.lfs_patterns,
        "lfs_threshold": options.lfs_threshold,
    });
    // Preserve existing #55 receipt serialization byte-for-byte.
    if options.projected_history {
        value["projected_history"] = true.into();
        value["path_rewrite_rules"] = serde_json::json!(options.path_rewrite_rules);
    }
    value.to_string()
}

// Git records file bytes and owner execute, not empty directories, mtimes,
// or the remaining POSIX mode bits.
type Files = BTreeMap<String, (String, bool)>;

fn selected_files(root: &Path, options: &PersonalOptionsConfig) -> Result<Files> {
    let policy = FilePolicy::from(options);
    let mut files = Files::new();
    for (path, entry) in require_supported_export(root)?.entries {
        if let ManifestEntry::File { size, mode, sha256 } = entry {
            if path_is_projected(&path, &options.allowed_paths, &options.blocked_patterns)
                && policy.evaluate(&path, size).should_sync()
            {
                files.insert(path, (sha256, mode & 0o100 != 0));
            }
        }
    }
    Ok(files)
}

/// Scoped network read evidence must never switch to ambient SVN credentials.
pub fn scoped_source(svn: &SvnClient, options: &PersonalOptionsConfig) -> Result<SvnClient> {
    let transport = crate::svn::operand::checked_url(svn.url())?;
    match options.network_directory_profile {
        None if transport.scheme() == "file" => Ok(svn.clone()),
        Some(profile) if profile.matches_scheme(transport.scheme()) => {
            let ca = options.svn_tls_ca_file.as_deref();
            ensure!(
                (transport.scheme() == "https") == ca.is_some(),
                "scoped HTTPS needs an explicitly provisioned CA bundle"
            );
            Ok(svn.isolated_personal_visibility(ca)?)
        }
        _ => anyhow::bail!(
            "scoped network SVN visibility is unqualified for this transport; SVN revision held"
        ),
    }
}

const HTTPS_TRUST: &str = "personal_https_wc31_trust_sha256_v1";
const HTTPS_SOURCE: &str = "personal_https_wc31_source_sha256_v1";

/// A previously admitted CA cannot be silently replaced. Check before
/// starting an SVN command so a stale trust file grants no new read.
pub fn require_https_trust(db: &Database, svn: &SvnClient) -> Result<()> {
    if let Some(current) = svn.https_trust_fingerprint() {
        match (db.get_state(HTTPS_TRUST)?, db.get_state(HTTPS_SOURCE)?) {
            (None, None) => {}
            (Some(previous), Some(source)) if previous == current
                && source.len() == 64 && source.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => anyhow::bail!("scoped HTTPS SVN CA bundle changed or binding is incomplete; preserve the mirror and restore its admitted trust"),
        }
    }
    Ok(())
}

/// Bind trust only after an authenticated, CA-verified direct SVN response.
/// No first failure can seed durable trust or advance a source checkpoint.
pub fn bind_https_source(db: &Database, svn: &SvnClient, info: &crate::svn::SvnInfo) -> Result<()> {
    let Some(trust) = svn.https_trust_fingerprint() else {
        return Ok(());
    };
    let expected = crate::svn::operand::checked_url(svn.url())?;
    let seen = crate::svn::operand::checked_url(&info.url)?;
    let root = crate::svn::operand::checked_url(&info.root_url)?;
    ensure!(
        expected.as_str().trim_end_matches('/') == seen.as_str().trim_end_matches('/')
            && root.scheme() == "https"
            && root.host_str() == seen.host_str()
            && root.port_or_known_default() == seen.port_or_known_default()
            && crate::path_projection::svn_path_identity(&info.root_url, &info.url).is_ok()
            && !info.uuid.is_empty(),
        "scoped HTTPS SVN endpoint or repository identity is not direct and unambiguous"
    );
    let source = serde_json::json!({"profile":"apache_mod_dav_svn_https_wc31","url":seen.as_str(),"root":root.as_str(),"uuid":info.uuid});
    let source = hex::encode(Sha256::digest(source.to_string().as_bytes()));
    db.immediate_transaction(|tx| {
        Database::require_legacy_import_watermarks_available_tx(tx)?;
        let refused = || {
            DatabaseError::Other(
                "scoped HTTPS SVN trust or repository identity changed; preserving the mirror"
                    .into(),
            )
        };
        let get = |key| {
            tx.query_row("SELECT value FROM kv_state WHERE key=?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
        };
        match (get(HTTPS_TRUST)?, get(HTTPS_SOURCE)?) {
            (None, None) => {
                for (key, value) in [(HTTPS_TRUST, trust), (HTTPS_SOURCE, source.as_str())] {
                    tx.execute(
                        "INSERT INTO kv_state(key,value,updated_at) VALUES (?1,?2,?3)",
                        params![key, value, chrono::Utc::now().to_rfc3339()],
                    )?;
                }
            }
            (Some(old_trust), Some(old_source)) if old_trust == trust && old_source == source => {}
            _ => return Err(refused()),
        }
        Ok(())
    })?;
    Ok(())
}

async fn observe_directory(
    svn: &SvnClient,
    prefix: &str,
    rev: i64,
    source: &crate::db::lineage_checkpoints::LiveSvnIdentity,
) -> Result<VisibleSvnCheckout> {
    let client = svn.personal_visibility_operand(prefix)?;
    let checkout = observe_subtree(&client, rev).await?;
    ensure!(
        checkout.info().uuid == source.uuid && checkout.info().root_url == source.root_url,
        "scoped directory checkout differs from the pinned SVN repository"
    );
    Ok(checkout)
}

fn content_entries(tree: IndependentTreeManifest) -> BTreeMap<String, Option<(String, bool)>> {
    tree.entries
        .into_iter()
        .map(|(path, entry)| {
            (
                path,
                match entry {
                    ManifestEntry::File { sha256, mode, .. } => Some((sha256, mode & 0o100 != 0)),
                    _ => None,
                },
            )
        })
        .collect()
}

/// A listing only routes the proof. If the selected endpoint is absent, the
/// nearest successfully seen parent supplies a depth-empty anchor. Prove
/// only its immediate child, never read the siblings to prove absence.
async fn prove_absence(
    svn: &SvnClient,
    prefix: &str,
    rev: i64,
    source: &crate::db::lineage_checkpoints::LiveSvnIdentity,
) -> Result<()> {
    use crate::svn::client::SelectedNode;
    let mut parent = prefix;
    loop {
        parent = parent.rsplit_once('/').map_or("", |(p, _)| p);
        if parent.is_empty() || svn.selected_node_at(parent, rev).await? == SelectedNode::Directory
        {
            let relative = if parent.is_empty() {
                prefix
            } else {
                &prefix[parent.len() + 1..]
            };
            let name = relative
                .split('/')
                .next()
                .context("invalid missing SVN prefix")?;
            let client = svn.personal_visibility_operand(parent)?;
            let proof = prove_absent_child(&client, rev, name)
                .await
                .context("cannot prove absence of the selected path from its pinned SVN parent")?;
            ensure!(
                proof.uuid == source.uuid && proof.root_url == source.root_url,
                "scoped absence parent differs from the pinned SVN repository"
            );
            return Ok(());
        }
    }
}

// Only selected files enter staging. A directory needs actual WC31
// completeness in the chosen profile and agreement between the proof's
// materialized tree and the pinned projection consumed by the Git writer.
async fn export_scope(
    svn: &SvnClient,
    rev: i64,
    dest: &Path,
    options: &PersonalOptionsConfig,
    source: &crate::db::lineage_checkpoints::LiveSvnIdentity,
) -> Result<()> {
    use crate::svn::client::SelectedNode;
    let mut prefixes: Vec<_> = options
        .allowed_paths
        .iter()
        .map(|p| p.trim_end_matches('/'))
        .collect();
    prefixes.sort_unstable();
    let mut done: Vec<&str> = Vec::new();
    let local = crate::svn::operand::checked_url(svn.url())?.scheme() == "file";
    for prefix in prefixes {
        if done
            .iter()
            .any(|p| crate::path_projection::path_matches_prefix(prefix, p))
        {
            continue;
        }
        done.push(prefix);
        let node = svn.selected_node_at(prefix, rev).await?;
        if node == SelectedNode::Missing {
            if !local {
                prove_absence(svn, prefix, rev, source).await?;
            }
            continue;
        }
        let attested = if !local && node == SelectedNode::Directory {
            let observed = observe_directory(svn, prefix, rev, source).await.context(
                "network SVN cannot prove complete visibility of the selected directory",
            )?;
            Some(observed.attest_materialized_tree()?)
        } else {
            None
        };
        let target = dest.join(prefix);
        std::fs::create_dir_all(target.parent().context("invalid selected prefix")?)?;
        svn.export(prefix, rev, &target).await?;
        if let Some(proved) = attested {
            ensure!(
                content_entries(proved) == content_entries(require_supported_export(&target)?),
                "pinned export differs from the complete network checkout"
            );
        } else if node == SelectedNode::File {
            ensure!(
                std::fs::symlink_metadata(&target)?.file_type().is_file(),
                "selected SVN file export is not a regular file"
            );
        }
    }
    Ok(())
}

/// Materialize an already verified selected SVN tree into Git coordinates.
fn projected_export(raw: &Path, options: &PersonalOptionsConfig) -> Result<TempDir> {
    let selected = selected_files(raw, options)?;
    let rewrite = options.rewrite_rule().map_err(anyhow::Error::msg)?;
    let stage = tempfile::tempdir()?;
    let mut expected = Files::new();
    for (name, bytes) in selected {
        let destination = match &rewrite {
            Some(rule) => rule
                .to_git(&name)
                .map_err(anyhow::Error::msg)?
                .context("selected SVN file falls outside the rewrite namespace")?,
            None => &name,
        };
        ensure!(
            expected.insert(destination.to_owned(), bytes).is_none(),
            "two selected SVN paths map to the same Git destination"
        );
        let target = stage.path().join(destination);
        std::fs::create_dir_all(target.parent().context("missing Git path parent")?)?;
        let source = raw.join(&name);
        let metadata = std::fs::symlink_metadata(&source)?;
        crate::import::copy_regular_file_no_follow(&source, stage.path(), &target, &metadata)?;
    }
    let observed: Files = content_entries(require_supported_export(stage.path())?)
        .into_iter()
        .filter_map(|(name, bytes)| bytes.map(|bytes| (name, bytes)))
        .collect();
    ensure!(
        observed == expected,
        "materialized Git tree differs from pinned SVN projection"
    );
    Ok(stage)
}

pub async fn projected_history_entries(
    svn: &SvnClient,
    first_rev: i64,
    head_rev: i64,
) -> Result<Vec<SvnLogEntry>> {
    let exact = svn.strict_log_range(first_rev, head_rev).await?;
    let entries = svn.log(first_rev, head_rev).await?;
    ensure!(
        exact
            .iter()
            .map(|e| e.revision)
            .eq(entries.iter().map(|e| e.revision)),
        "scoped full-history SVN log differs from strict numbered evidence"
    );
    ensure!(
        entries
            .first()
            .is_some_and(|entry| entry.revision == first_rev)
            && entries.windows(2).all(|w| w[0].revision < w[1].revision),
        "scoped full-history log does not begin at the proved SVN incarnation"
    );
    Ok(entries)
}

pub async fn require_projected_source_unchanged(
    svn: &SvnClient,
    prior: &LiveSvnIdentity,
) -> Result<()> {
    let live = observe_live_svn_identity(svn, &svn.info().await?).await?;
    ensure!(
        svn_identity_difference(prior, &live).is_none(),
        "SVN source changed during projected import"
    );
    Ok(())
}

/// Equality witness for a historical revision that changes no projected
/// file bytes or Git mode. Neither SVN author nor log text belongs here.
pub struct ProjectedHistoryNoTarget {
    rev: i64,
    digest: String,
}

/// Track the last independently projected Git tree, beginning at the unborn
/// empty tree. Only a change may enter the Git history.
#[derive(Default)]
pub struct ProjectedHistoryTrees(Files);

impl ProjectedHistoryTrees {
    pub fn observe(
        &mut self,
        rev: i64,
        verified: &Path,
    ) -> Result<Option<ProjectedHistoryNoTarget>> {
        let files: Files = content_entries(require_supported_export(verified)?)
            .into_iter()
            .filter_map(|(name, bytes)| bytes.map(|bytes| (name, bytes)))
            .collect();
        if self.0 == files {
            let digest = hex::encode(Sha256::digest(serde_json::to_vec(&files)?));
            Ok(Some(ProjectedHistoryNoTarget { rev, digest }))
        } else {
            self.0 = files;
            Ok(None)
        }
    }
}

pub(crate) fn persist_projected_history_no_targets_tx(
    tx: &rusqlite::Connection,
    frontier: i64,
    config: &crate::personal_config::PersonalConfig,
    proofs: &[ProjectedHistoryNoTarget],
) -> Result<(), DatabaseError> {
    let generation = current_generation_read_only(tx, SCOPE, SCOPE)?;
    let projection = policy_snapshot(&config.options);
    let mut previous = 0;
    for proof in proofs {
        if proof.rev <= previous || proof.rev > frontier {
            return Err(DatabaseError::Other(
                "invalid historical Personal no-target evidence order".into(),
            ));
        }
        let key = svn_source_key(proof.rev);
        let reason = format!(
            "equal verified historical Personal Git projection (SHA-256 {})",
            proof.digest
        );
        record_mapping_outcome(
            tx,
            SCOPE,
            generation,
            MappingWrite {
                direction: CheckpointDirection::SvnToGit,
                source_key: &key,
                outcome: MappingOutcome::IntentionallyFilteredNoTarget,
                target_git_sha: None,
                target_svn_rev: None,
                reason: Some(&reason),
                policy_snapshot: Some(&projection),
            },
        )?;
        insert_proven_no_target_row_tx(tx, proof.rev, "svn_to_git", "", "", SCOPE)?;
        previous = proof.rev;
    }
    Ok(())
}

/// Full historical import and ordinary incremental sync share the same
/// selected-endpoint visibility, SVN-coordinate filtering and Git mapping.
pub async fn export_projected_history_revision(
    svn: &SvnClient,
    options: &PersonalOptionsConfig,
    rev: i64,
    live: &LiveSvnIdentity,
) -> Result<TempDir> {
    let network = options.network_directory_profile.is_some();
    let historical = if network {
        observe_numbered_network_identity(svn, rev).await?
    } else {
        observe_historical_svn_identity_at(svn, rev).await?
    };
    ensure!(
        svn_identity_difference(&historical, live).is_none(),
        "SVN historical revision belongs to another branch incarnation"
    );
    let source = svn.visibility_tempdir("history-")?;
    export_scope(svn, rev, source.path(), options, &historical).await?;
    if network {
        require_network_live_endpoint(svn, &historical).await?;
    }
    svn.require_visibility_budget()?;
    projected_export(source.path(), options)
}

pub struct SelectedSnapshot {
    directory: TempDir,
    before: TempDir,
}

impl SelectedSnapshot {
    /// This narrow increment cannot safely migrate an existing full checkout.
    /// Check all actual files (including untracked/ignored files) before giving
    /// the legacy whole-tree publisher the projected snapshot. This also stops
    /// a widened scope from silently backfilling previously skipped history.
    pub fn into_export(self, destination: &Path, policy: &FilePolicy) -> Result<TempDir> {
        let expected =
            crate::import::independent_export_projection(self.before.path(), destination, policy)?;
        let files = |m: crate::import::IndependentTreeManifest| -> Files {
            m.entries
                .into_iter()
                .filter_map(|(path, entry)| match entry {
                    ManifestEntry::File { sha256, mode, .. } => {
                        Some((path, (sha256, mode & 0o100 != 0)))
                    }
                    _ => None,
                })
                .collect()
        };
        let actual = require_supported_export(destination)?;
        ensure!(files(actual) == files(expected),
            "Personal Git checkout differs from the previous scoped SVN snapshot; preserve it and reconcile before syncing");
        Ok(self.directory)
    }
}

async fn observe_numbered_network_identity(svn: &SvnClient, rev: i64) -> Result<LiveSvnIdentity> {
    let info = svn.info_at_rev(rev).await?;
    let path = crate::path_projection::svn_path_identity(&info.root_url, &info.url)?;
    ensure!(
        rev == info.latest_rev && !info.uuid.is_empty(),
        "SVN identity differs from numbered revision"
    );
    // The repository root cannot be replaced by an SVN path copy. A
    // server-redacted log of an unrelated later revision cannot change its
    // identity. A nested target still needs the actual incarnation proof.
    if path.branch_path.is_empty() {
        Ok(LiveSvnIdentity {
            uuid: info.uuid,
            root_url: path.root_url,
            url: info.url.trim_end_matches('/').to_owned(),
            branch_path: path.branch_path,
            incarnation_created_rev: 1,
            copy_from_path: None,
            copy_from_rev: None,
        })
    } else {
        Ok(observe_historical_svn_identity_at(svn, rev).await?)
    }
}

async fn require_network_live_endpoint(
    svn: &SvnClient,
    historical: &LiveSvnIdentity,
) -> Result<()> {
    let info = svn.info().await?;
    ensure!(
        historical.uuid == info.uuid
            && historical.root_url == info.root_url.trim_end_matches('/')
            && historical.url == info.url.trim_end_matches('/'),
        "SVN endpoint identity changed during scoped projection"
    );
    if !historical.branch_path.is_empty() {
        let current = observe_numbered_network_identity(svn, info.latest_rev)
            .await
            .context("current nested SVN incarnation could not be proved")?;
        ensure!(
            svn_identity_difference(historical, &current).is_none(),
            "current nested SVN incarnation differs from the Personal checkpoint"
        );
    }
    Ok(())
}

/// Check the imported predecessor against the actual live origin before the
/// cursor can move, including on a proved receipt-only retry.
pub async fn require_live_network_checkpoint(svn: &SvnClient, watermark: i64) -> Result<()> {
    let prior = observe_numbered_network_identity(svn, watermark).await?;
    require_network_live_endpoint(svn, &prior).await
}

/// Only this endpoint comparison can originate the Personal canonical
/// no-target outcome. A held source revision stays pending on any I/O error.
pub async fn prepare(
    svn: &SvnClient,
    db: &Database,
    options: &PersonalOptionsConfig,
    entry: Option<&SvnLogEntry>,
    rev: i64,
    predecessor: i64,
) -> Result<Option<SelectedSnapshot>> {
    ensure!(
        rev > predecessor && predecessor >= 0 && !options.allowed_paths.is_empty(),
        "invalid scoped Personal revision range or empty scope"
    );
    let window = db.begin_ordinary_writer_window(None)?;
    let generation = current_generation_read_only(&db.conn(), SCOPE, SCOPE)?;
    let transport = crate::svn::operand::checked_url(svn.url())?;
    let network = matches!(transport.scheme(), "svn" | "https");
    ensure!(
        transport.scheme() == "file"
            || (network
                && svn.has_refresh_isolation()
                && options
                    .network_directory_profile
                    .is_some_and(|profile| profile.matches_scheme(transport.scheme()))
                && (transport.scheme() == "https") == svn.https_trust_fingerprint().is_some()),
        "scoped source visibility profile is not qualified"
    );
    let historical = if network {
        let pinned = observe_numbered_network_identity(svn, rev).await?;
        let prior = observe_numbered_network_identity(svn, rev - 1).await?;
        ensure!(
            svn_identity_difference(&pinned, &prior).is_none(),
            "SVN source incarnation changed across adjacent Personal revisions"
        );
        require_network_live_endpoint(svn, &pinned).await?;
        pinned
    } else {
        let historical = observe_historical_svn_identity_at(svn, rev).await?;
        let live = observe_live_svn_identity(svn, &svn.info().await?).await?;
        ensure!(
            svn_identity_difference(&historical, &live).is_none(),
            "SVN revision belongs to a different branch incarnation"
        );
        historical
    };
    // The log supplies commit metadata; it is not a directory completeness
    // proof. Missing entries do not authorize a positive Git effect.
    let strict = if let Some(entry) = entry.filter(|e| !network || !e.changed_paths.is_empty()) {
        let log = svn.strict_log_range(rev, rev).await?;
        ensure!(
            matches!(log.as_slice(), [one] if one.revision == rev && one.author.trim() == entry.author && one.message.trim() == entry.message),
            "scoped SVN revision was not independently observed"
        );
        log.into_iter().next()
    } else {
        None
    };
    let old_dir = svn.visibility_tempdir("prior-")?;
    let new_dir = svn.visibility_tempdir("next-")?;
    let added_branch = !historical.branch_path.is_empty()
        && strict.as_ref().is_some_and(|log| {
            log.changed_paths.iter().any(|p| {
                p.action == "A" && p.path.strip_prefix('/') == Some(historical.branch_path.as_str())
            })
        });
    if rev > 1 && !added_branch {
        export_scope(svn, rev - 1, old_dir.path(), options, &historical).await?;
    }
    export_scope(svn, rev, new_dir.path(), options, &historical).await?;
    let before = selected_files(old_dir.path(), options)?;
    let after = selected_files(new_dir.path(), options)?;
    if network {
        require_network_live_endpoint(svn, &historical).await?;
    } else {
        let observed = observe_live_svn_identity(svn, &svn.info().await?).await?;
        ensure!(
            svn_identity_difference(&historical, &observed).is_none(),
            "SVN branch identity changed during scoped projection"
        );
    }
    svn.require_visibility_budget()?;
    window.require_engine_scope(db, None)?;

    if before != after {
        ensure!(
            strict.is_some(),
            "SVN log omitted a revision with a selected Git effect; holding the revision"
        );
        if options.projected_history {
            let directory = projected_export(new_dir.path(), options)?;
            let before = projected_export(old_dir.path(), options)?;
            svn.require_visibility_budget()?;
            window.finish_preparation()?;
            return Ok(Some(SelectedSnapshot { directory, before }));
        }
        // Move only admitted files between our private temporary directories.
        // No untrusted entry is followed, and renaming preserves execute bits.
        let projection = svn.visibility_tempdir("projected-")?;
        for path in after.keys() {
            let target = projection.path().join(path);
            std::fs::create_dir_all(target.parent().context("missing projected parent")?)?;
            std::fs::rename(new_dir.path().join(path), target)?;
        }
        ensure!(
            selected_files(projection.path(), options)? == after,
            "materialized scope differs from independent SVN projection"
        );
        svn.require_visibility_budget()?;
        window.finish_preparation()?;
        return Ok(Some(SelectedSnapshot {
            directory: projection,
            before: old_dir,
        }));
    }

    let author = entry.map_or("", |e| e.author.as_str());
    let snapshot = policy_snapshot(options);
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&before)?));
    let reason = format!(
        "equal pinned selected file manifests (SHA-256 {digest}); SVN UUID {}, branch {}, incarnation r{}",
        historical.uuid, historical.branch_path, historical.incarnation_created_rev
    );
    db.immediate_transaction(|tx| {
        Database::require_legacy_import_watermarks_available_tx(tx)?;
        let bookmark: Option<String> = tx.query_row(
            "SELECT value FROM watermarks WHERE source='svn_rev'", [], |r| r.get(0)
        ).optional()?;
        let expected = predecessor.to_string();
        if bookmark.as_deref().unwrap_or("0") != expected
            || current_generation_read_only(tx, SCOPE, SCOPE)? != generation {
            return Err(DatabaseError::Other("Personal SVN checkpoint or generation changed during projection".into()));
        }
        let key = svn_source_key(rev);
        let direction = CheckpointDirection::SvnToGit;
        if load_mapping_outcome(tx, SCOPE, generation, direction, &key)?.is_some() {
            return Err(DatabaseError::Other("existing Personal canonical outcome would be overwritten".into()));
        }
        record_mapping_outcome(tx, SCOPE, generation, MappingWrite {
            direction, source_key: &key, outcome: MappingOutcome::IntentionallyFilteredNoTarget,
            target_git_sha: None, target_svn_rev: None, reason: Some(&reason), policy_snapshot: Some(&snapshot),
        })?;
        // A legacy orphan NULL row is not evidence; once the outcome exists
        // it becomes its display projection. Do not manufacture a duplicate.
        let rows: i64 = tx.query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1 AND direction='svn_to_git' AND svn_rev=?2 AND git_sha IS NULL",
            params![SCOPE, rev], |r| r.get(0))?;
        if rows == 0 {
            insert_proven_no_target_row_tx(tx, rev, "svn_to_git", author, "", SCOPE)?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("INSERT INTO watermarks (source,value,updated_at) VALUES ('svn_rev',?1,?2)
            ON CONFLICT(source) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![rev.to_string(),now])?;
        tx.execute("INSERT INTO audit_log (action,direction,svn_rev,git_sha,author,details,created_at,success,repo_id)
            VALUES ('svn_to_git_filtered','svn_to_git',?1,NULL,?2,?3,?4,1,?5)",
            params![rev,author,format!("SVN r{rev} synced (filtered, no commit)"),now,SCOPE])?;
        Ok(())
    })?;
    window.finish_preparation()?;
    Ok(None)
}
