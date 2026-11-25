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
    MappingOutcome, MappingWrite,
};
use crate::db::nullable_commit_map::{
    current_generation_read_only, insert_proven_no_target_row_tx,
};
use crate::db::personal_scope::PERSONAL_SCOPE_KEY as SCOPE;
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::file_policy::FilePolicy;
use crate::import::{require_supported_export, ManifestEntry};
use crate::path_projection::path_is_projected;
use crate::personal_config::PersonalOptionsConfig;
use crate::svn::{SvnClient, SvnLogEntry};

const MODE_LATCH: &str = "personal_incremental_scope_mode_v1";

/// No implicit downgrade: TOML omission is not an instruction to republish the
/// full tree or replay previously held PRs. Keep unrestricted installs unchanged.
pub fn require_unrestricted(db: &Database) -> Result<()> {
    ensure!(db.get_state(MODE_LATCH)?.is_none(),
        "this mirror was opened in scoped Personal mode; restore allowed_paths. An unrestricted transition requires a separately prepared mirror");
    Ok(())
}

pub fn admit_sync(db: &Database, config: &crate::personal_config::PersonalConfig) -> Result<()> {
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
    serde_json::json!({
        "allowed_paths": options.allowed_paths,
        "blocked_patterns": [],
        "ignore_patterns": options.ignore_patterns,
        "max_file_size": options.max_file_size,
        "lfs_patterns": options.lfs_patterns,
        "lfs_threshold": options.lfs_threshold,
    })
    .to_string()
}

// Git records file bytes and owner execute, not empty directories, mtimes,
// or the remaining POSIX mode bits.
type Files = BTreeMap<String, (String, bool)>;

fn selected_files(root: &Path, options: &PersonalOptionsConfig) -> Result<Files> {
    let policy = FilePolicy::from(options);
    let mut files = Files::new();
    for (path, entry) in require_supported_export(root)?.entries {
        if let ManifestEntry::File { size, mode, sha256 } = entry {
            if path_is_projected(&path, &options.allowed_paths, &[])
                && policy.evaluate(&path, size).should_sync()
            {
                files.insert(path, (sha256, mode & 0o100 != 0));
            }
        }
    }
    Ok(files)
}

// Inventory/export only configured subtrees, never the rest of the monorepo.
// Prefixes can denote files, nested directories or paths absent at one end.
async fn export_scope(
    svn: &SvnClient,
    rev: i64,
    dest: &Path,
    options: &PersonalOptionsConfig,
    source: &crate::db::lineage_checkpoints::LiveSvnIdentity,
    changes: &SvnLogEntry,
    proves_absence: &str,
) -> Result<()> {
    let mut prefixes: Vec<_> = options
        .allowed_paths
        .iter()
        .map(|p| p.trim_end_matches('/'))
        .collect();
    prefixes.sort_unstable();
    let mut done: Vec<&str> = Vec::new();
    for prefix in prefixes {
        if done
            .iter()
            .any(|p| crate::path_projection::path_matches_prefix(prefix, p))
        {
            continue;
        }
        let node = svn.selected_node_at(prefix, rev).await?;
        let local = crate::svn::operand::checked_url(svn.url())?.scheme() == "file";
        let full_path = if source.branch_path.is_empty() {
            format!("/{prefix}")
        } else {
            format!("/{}/{prefix}", source.branch_path)
        };
        let affirmative_transition = changes
            .changed_paths
            .iter()
            .any(|p| p.action == proves_absence && p.path == full_path);
        require_visible_endpoint(node, local, affirmative_transition)?;
        if node != crate::svn::client::SelectedNode::Missing {
            let target = dest.join(prefix);
            std::fs::create_dir_all(target.parent().context("invalid selected prefix")?)?;
            svn.export(prefix, rev, &target).await?;
        }
        done.push(prefix);
    }
    Ok(())
}

// A network server may silently omit unauthorized children even when list
// and export succeed. Local file:// has no server-side path authorization.
// A network file is directly read; absent files require an affirmative
// add/delete of that exact operand, never absence of changed-path entries.
fn require_visible_endpoint(
    node: crate::svn::client::SelectedNode,
    local: bool,
    transition: bool,
) -> Result<()> {
    use crate::svn::client::SelectedNode;
    ensure!(local || node == SelectedNode::File || (node == SelectedNode::Missing && transition),
        "network SVN cannot prove complete visibility of this directory or missing path; scoped revision held without publication or checkpoint");
    Ok(())
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

/// Only this endpoint comparison can originate the Personal canonical
/// no-target outcome. A held source revision stays pending on any I/O error.
pub async fn prepare(
    svn: &SvnClient,
    db: &Database,
    options: &PersonalOptionsConfig,
    entry: &SvnLogEntry,
    predecessor: i64,
) -> Result<Option<SelectedSnapshot>> {
    let rev = entry.revision;
    ensure!(
        rev > predecessor && predecessor >= 0 && !options.allowed_paths.is_empty(),
        "invalid scoped Personal revision range or empty scope"
    );
    let window = db.begin_ordinary_writer_window(None)?;
    let generation = current_generation_read_only(&db.conn(), SCOPE, SCOPE)?;
    let historical = observe_historical_svn_identity_at(svn, rev).await?;
    let live = observe_live_svn_identity(svn, &svn.info().await?).await?;
    ensure!(
        svn_identity_difference(&historical, &live).is_none(),
        "SVN revision belongs to a different branch incarnation"
    );
    let log = svn.strict_log_range(rev, rev).await?;
    ensure!(
        matches!(log.as_slice(), [one] if one.revision == rev && one.author.trim() == entry.author && one.message.trim() == entry.message),
        "scoped SVN revision was not independently observed"
    );
    let old_dir = tempfile::tempdir()?;
    let new_dir = tempfile::tempdir()?;
    // At the first revision of this branch incarnation its predecessor has
    // no subtree. Otherwise require an actual pinned previous endpoint.
    let added_branch = !historical.branch_path.is_empty()
        && log[0].changed_paths.iter().any(|p| {
            p.action == "A" && p.path.strip_prefix('/') == Some(historical.branch_path.as_str())
        });
    if rev > 1 && !added_branch {
        export_scope(
            svn,
            rev - 1,
            old_dir.path(),
            options,
            &historical,
            &log[0],
            "A",
        )
        .await?;
    }
    export_scope(svn, rev, new_dir.path(), options, &historical, &log[0], "D").await?;
    let before = selected_files(old_dir.path(), options)?;
    let after = selected_files(new_dir.path(), options)?;
    let observed = observe_live_svn_identity(svn, &svn.info().await?).await?;
    ensure!(
        svn_identity_difference(&historical, &observed).is_none(),
        "SVN branch identity changed during scoped projection"
    );
    window.require_engine_scope(db, None)?;

    if before != after {
        // Move only admitted files between our private temporary directories.
        // No untrusted entry is followed, and renaming preserves execute bits.
        let projection = tempfile::tempdir()?;
        for path in after.keys() {
            let target = projection.path().join(path);
            std::fs::create_dir_all(target.parent().context("missing projected parent")?)?;
            std::fs::rename(new_dir.path().join(path), target)?;
        }
        ensure!(
            selected_files(projection.path(), options)? == after,
            "materialized scope differs from independent SVN projection"
        );
        window.finish_preparation()?;
        return Ok(Some(SelectedSnapshot {
            directory: projection,
            before: old_dir,
        }));
    }

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
            insert_proven_no_target_row_tx(tx, rev, "svn_to_git", &entry.author, "", SCOPE)?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("INSERT INTO watermarks (source,value,updated_at) VALUES ('svn_rev',?1,?2)
            ON CONFLICT(source) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![rev.to_string(),now])?;
        tx.execute("INSERT INTO audit_log (action,direction,svn_rev,git_sha,author,details,created_at,success,repo_id)
            VALUES ('svn_to_git_filtered','svn_to_git',?1,NULL,?2,?3,?4,1,?5)",
            params![rev,entry.author,format!("SVN r{rev} synced (filtered, no commit)"),now,SCOPE])?;
        Ok(())
    })?;
    window.finish_preparation()?;
    Ok(None)
}
