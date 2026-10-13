//! Durable exclusion between an ordinary engine's local preparation and refresh
//! admission. This is not a publication receipt or permission to retry a write.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use super::{pair_refresh_operations::require_no_pair_refresh_hold, Database};
use crate::errors::DatabaseError;

const FAMILY: &str = "pair_refresh_writer_window_";
const PREFIX: &str = "pair_refresh_writer_window_v1:document:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preparing,
    UnjournalledPublicationPossible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    id: Uuid,
    repo_id: Option<String>,
    scope_uuid: Option<String>,
    writer_epoch: Option<u64>,
    created_at: String,
    phase: Phase,
}

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("ordinary writer window refused: {detail}"))
}

fn documents(tx: &Connection) -> Result<Vec<Document>, DatabaseError> {
    let mut query =
        tx.prepare("SELECT key,value FROM kv_state WHERE substr(key,1,?1)=?2 ORDER BY key")?;
    let rows = query
        .query_map(params![FAMILY.len(), FAMILY], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut found = Vec::<Document>::new();
    for (key, raw) in rows {
        let doc: Document =
            serde_json::from_str(&raw).map_err(|_| refused("corrupt durable document"))?;
        if doc.version != 1
            || doc.id.is_nil()
            || key != format!("{PREFIX}{}", doc.id)
            || doc.repo_id.as_ref().is_some_and(String::is_empty)
            || doc.writer_epoch == Some(0)
            || doc.scope_uuid.as_ref().is_some_and(|scope| {
                !Uuid::parse_str(scope).is_ok_and(|id| !id.is_nil() && id.to_string() == *scope)
            })
            || (doc.repo_id.is_none() && doc.scope_uuid.is_some())
            || doc.created_at.is_empty()
            || serde_json::to_string(&doc).map_err(|_| refused("document encoding"))? != raw
            || found.iter().any(|prior| overlaps(prior, &doc))
        {
            return Err(refused(
                "unknown, noncanonical or contradictory durable window",
            ));
        }
        found.push(doc);
    }
    Ok(found)
}

fn overlaps(a: &Document, b: &Document) -> bool {
    a.repo_id.is_none()
        || b.repo_id.is_none()
        || a.repo_id == b.repo_id
        || a.scope_uuid
            .as_ref()
            .is_some_and(|scope| b.scope_uuid.as_ref() == Some(scope))
}

fn store(tx: &Connection, doc: &Document) -> Result<(), DatabaseError> {
    tx.execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![format!("{PREFIX}{}",doc.id),serde_json::to_string(doc).map_err(|_|refused("document encoding"))?,Utc::now().to_rfc3339()])?;
    Ok(())
}

/// Called inside refresh admission's own transaction. An unrelated scoped
/// window can run; any global or matching identity window excludes admission.
pub fn require_quiet_for_refresh(
    tx: &Connection,
    repo_id: &str,
    scope_uuid: &str,
) -> Result<(), DatabaseError> {
    if documents(tx)?.iter().any(|doc| {
        doc.repo_id.is_none()
            || doc.repo_id.as_deref() == Some(repo_id)
            || doc.scope_uuid.as_deref() == Some(scope_uuid)
    }) {
        return Err(refused(
            "an ordinary writer is preparing or has uncertain publication",
        ));
    }
    Ok(())
}

pub(crate) fn require_globally_quiet_for_refresh(tx: &Connection) -> Result<(), DatabaseError> {
    if !documents(tx)?.is_empty() {
        return Err(refused("an ordinary writer owns a durable window"));
    }
    Ok(())
}

/// Private identity fields make this a capability for one admitted run. Drop
/// releases only a still-preparing window. Possible legacy publication survives
/// failure/cancellation/drop, and abrupt process exit preserves every window.
pub struct OrdinaryWriterWindow<'db> {
    db: WindowDatabase<'db>,
    admitted: Document,
    completed: bool,
}

enum WindowDatabase<'db> {
    Borrowed(&'db Database),
    Shared(Arc<Database>),
}

impl std::ops::Deref for WindowDatabase<'_> {
    type Target = Database;
    fn deref(&self) -> &Database {
        match self {
            Self::Borrowed(db) => db,
            Self::Shared(db) => db,
        }
    }
}

/// A scheduler owns this capability across local preparation and task handoff.
/// Sharing the connection does not clone the capability or admit another run.
pub fn begin_shared_ordinary_writer_window(
    db: Arc<Database>,
    repo_id: Option<&str>,
) -> Result<OrdinaryWriterWindow<'static>, DatabaseError> {
    let admitted = db.admit_ordinary_writer_window(repo_id)?;
    Ok(OrdinaryWriterWindow {
        db: WindowDatabase::Shared(db),
        admitted,
        completed: false,
    })
}

impl Database {
    /// Personal startup needs storage before clone, while existing storage must
    /// refuse a held writer before migrations or conflict attribution can run.
    /// Only an empty database may be initialized before admission.
    pub fn begin_personal_startup_writer_window(
        &self,
    ) -> Result<OrdinaryWriterWindow<'_>, DatabaseError> {
        let has_storage: bool = self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%')",
            [],
            |row| row.get(0),
        )?;
        if !has_storage {
            self.initialize()?;
        }
        let window = self.begin_ordinary_writer_window(None)?;
        self.initialize()?;
        window.require_engine_scope(self, None)?;
        Ok(window)
    }

    pub fn begin_ordinary_writer_window(
        &self,
        repo_id: Option<&str>,
    ) -> Result<OrdinaryWriterWindow<'_>, DatabaseError> {
        let admitted = self.admit_ordinary_writer_window(repo_id)?;
        Ok(OrdinaryWriterWindow {
            db: WindowDatabase::Borrowed(self),
            admitted,
            completed: false,
        })
    }

    /// A caller may queue behind a still-preparing ordinary run. None grants
    /// no capability and performs no mutation. Uncertain publication, corrupt
    /// evidence and refresh holds remain errors; waiting never releases them.
    pub fn try_begin_ordinary_writer_window(
        &self,
        repo_id: Option<&str>,
    ) -> Result<Option<OrdinaryWriterWindow<'_>>, DatabaseError> {
        Ok(self
            .try_admit_ordinary_writer_window(repo_id)?
            .map(|admitted| OrdinaryWriterWindow {
                db: WindowDatabase::Borrowed(self),
                admitted,
                completed: false,
            }))
    }

    fn admit_ordinary_writer_window(
        &self,
        repo_id: Option<&str>,
    ) -> Result<Document, DatabaseError> {
        self.try_admit_ordinary_writer_window(repo_id)?
            .ok_or_else(|| refused("another run owns this writer scope"))
    }

    fn try_admit_ordinary_writer_window(
        &self,
        repo_id: Option<&str>,
    ) -> Result<Option<Document>, DatabaseError> {
        let repo_id =
            repo_id.filter(|id| !id.is_empty() && *id != super::personal_scope::PERSONAL_SCOPE_KEY);
        // Reserve the SQLite writer before reading ownership. A deferred
        // read-to-write upgrade can fail immediately under WAL contention,
        // bypassing busy_timeout and turning a quiet scope into a refusal.
        self.immediate_transaction(|tx| {
            require_no_pair_refresh_hold(tx,repo_id)?;
            // Legacy engine migrations happen after admission. A human scope
            // still excludes refresh while the old schema lacks UUID columns.
            let has_scope:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('repositories') WHERE name='scope_uuid')",[],|r|r.get(0))?;
            let scope_uuid=if has_scope {
                repo_id.map(|id|tx.query_row("SELECT scope_uuid FROM repositories WHERE id=?1",[id],|row|row.get::<_,Option<String>>(0)).optional()).transpose()?.flatten().flatten()
            } else { None };
            let mut doc=Document {version:1,id:Uuid::new_v4(),repo_id:repo_id.map(str::to_owned),scope_uuid,writer_epoch:None,created_at:Utc::now().to_rfc3339(),phase:Phase::Preparing};
            let prior=documents(tx)?;
            if prior.iter().any(|prior|overlaps(prior,&doc) && prior.phase!=Phase::Preparing) {
                return Err(refused("another run owns this writer scope with possible publication"));
            }
            if prior.iter().any(|prior|overlaps(prior,&doc)) {
                return Ok(None);
            }
            // Even lease renewal waits until a real admission is possible.
            doc.writer_epoch=crate::writer_fence::ordinary_current_epoch(tx)?;
            store(tx,&doc)?;
            // Validate newly emitted evidence before committing it too.
            documents(tx)?;
            Ok(Some(doc))
        })
    }
}

impl OrdinaryWriterWindow<'_> {
    /// The actual engine has selected this complete P..R graph after normal
    /// history admission. Settle only SVN-emitted commits, with current scoped
    /// receipt/checkpoint and writer ownership checks in the same transaction.
    pub(crate) fn settle_emitted_git_echoes(
        &self,
        repo_id: &str,
        checkpoint: &str,
        remote_tip: &str,
        commits: &[&str],
    ) -> Result<(), DatabaseError> {
        if commits.is_empty() || !commits.contains(&remote_tip) {
            return Ok(());
        }
        self.db.immediate_transaction(|tx| {
            let doc = self.owned(tx)?;
            if doc.phase != Phase::Preparing || doc.repo_id.as_deref() != Some(repo_id) {
                return Err(refused("echo settlement requires the current engine window"));
            }
            require_no_pair_refresh_hold(tx, Some(repo_id))?;
            // Preserve the legacy dual-cursor protocol for ordinary roots.
            // This convergence is needed only for a registered branch pair's
            // parent to become an eligible refresh source.
            let refresh_parent: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM repositories WHERE parent_id=?1 AND id<>?1 AND sync_mode='team' AND enabled=1)",
                [repo_id], |row| row.get(0),
            )?;
            if !refresh_parent {
                return Ok(());
            }
            let (scope, emitted, revision): (Option<String>, String, i64) = tx.query_row(
                "SELECT scope_uuid,last_git_sha,last_svn_rev FROM repositories WHERE id=?1",
                [repo_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?;
            let handled = crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv_tx(tx, repo_id)?;
            if scope.is_none() || scope != doc.scope_uuid || emitted != remote_tip
                || handled.as_deref() != Some(checkpoint)
            {
                return Ok(());
            }
            for sha in commits {
                let matching: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND direction='svn_to_git' AND status='applied' AND svn_rev>0 AND svn_rev<=?3",
                    rusqlite::params![repo_id,sha,revision], |row| row.get(0),
                )?;
                if matching != 1 {
                    return Ok(());
                }
            }
            crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
                tx, repo_id, remote_tip, &Utc::now().to_rfc3339(),
            )
        })
    }

    /// The real engine observed the complete branch log beyond this checkpoint
    /// and proved each mapped Git source is an ancestor of its admitted tip.
    /// Recheck every receipt and cursor under the current owned writer window.
    pub(crate) fn settle_emitted_svn_echoes(
        &self,
        repo_id: &str,
        checkpoint: i64,
        git_checkpoint: &str,
        receipts: &[(i64, String)],
    ) -> Result<(), DatabaseError> {
        let Some((latest, latest_git)) = receipts.last() else {
            return Ok(());
        };
        if checkpoint <= 0
            || latest_git != git_checkpoint
            || receipts.first().is_some_and(|(rev, _)| *rev <= checkpoint)
            || receipts.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return Ok(());
        }
        self.db.immediate_transaction(|tx| {
            let doc = self.owned(tx)?;
            if doc.phase != Phase::Preparing || doc.repo_id.as_deref() != Some(repo_id) {
                return Err(refused("SVN echo settlement requires the current engine window"));
            }
            require_no_pair_refresh_hold(tx, Some(repo_id))?;
            let eligible: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM repositories WHERE parent_id=?1 AND id<>?1 AND sync_mode='team' AND enabled=1)",
                [repo_id], |row| row.get(0),
            )?;
            let (scope, rev, sha): (Option<String>, i64, String) = tx.query_row(
                "SELECT scope_uuid,last_svn_rev,last_git_sha FROM repositories WHERE id=?1",
                [repo_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?;
            let svn_key = format!("last_svn_rev_{repo_id}");
            let inbound_svn: Option<String> = tx.query_row(
                "SELECT value FROM kv_state WHERE key=?1", [&svn_key], |row| row.get(0),
            ).optional()?;
            let inbound_git = crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv_tx(tx, repo_id)?;
            if !eligible || scope.is_none() || scope != doc.scope_uuid || rev != checkpoint
                || inbound_svn.as_deref() != Some(checkpoint.to_string().as_str())
                || sha != git_checkpoint || inbound_git.as_deref() != Some(git_checkpoint)
            {
                return Ok(());
            }
            for (revision, source) in receipts {
                let mut query = tx.prepare("SELECT git_sha FROM sync_records WHERE repo_id=?1 AND svn_rev=?2 AND direction='git_to_svn' AND status='applied'")?;
                let matches = query.query_map(params![repo_id, revision], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                if matches.as_slice() != std::slice::from_ref(source) {
                    return Ok(());
                }
            }
            tx.execute("UPDATE repositories SET last_svn_rev=?1 WHERE id=?2", params![latest, repo_id])?;
            tx.execute("UPDATE kv_state SET value=?1,updated_at=?2 WHERE key=?3", params![latest.to_string(), Utc::now().to_rfc3339(), svn_key])?;
            Ok(())
        })
    }

    /// Validate a preparation handoff before resolving credentials or touching
    /// a checkout. The capability remains bound to its original storage, scope,
    /// writer epoch and durable preparation phase.
    pub fn require_engine_scope(
        &self,
        db: &Database,
        repo_id: Option<&str>,
    ) -> Result<(), DatabaseError> {
        let repo_id =
            repo_id.filter(|id| !id.is_empty() && *id != super::personal_scope::PERSONAL_SCOPE_KEY);
        if self.admitted.repo_id.as_deref() != repo_id {
            return Err(refused("window handoff targets another engine scope"));
        }
        // A copied database can contain identical journal bytes and an epoch.
        // It is still not the storage whose window this capability releases.
        if !std::ptr::eq::<Database>(&*self.db, db) {
            let admitted_path = self.db.conn().path().map(std::path::PathBuf::from);
            let engine_path = db.conn().path().map(std::path::PathBuf::from);
            let same_file = admitted_path
                .zip(engine_path)
                .and_then(|(a, b)| {
                    Some((
                        std::fs::canonicalize(a).ok()?,
                        std::fs::canonicalize(b).ok()?,
                    ))
                })
                .is_some_and(|(a, b)| a == b);
            if !same_file {
                return Err(refused("window handoff targets another database"));
            }
        }
        db.immediate_transaction(|tx| {
            let doc = self.owned(tx)?;
            if doc.phase != Phase::Preparing {
                return Err(refused("window already has uncertain publication"));
            }
            require_no_pair_refresh_hold(tx, repo_id)
        })
    }

    fn owned(&self, tx: &Connection) -> Result<Document, DatabaseError> {
        if crate::writer_fence::ordinary_current_epoch(tx)? != self.admitted.writer_epoch {
            return Err(refused("writer epoch changed during the run"));
        }
        let doc = documents(tx)?
            .into_iter()
            .find(|doc| doc.id == self.admitted.id)
            .ok_or_else(|| refused("durable window disappeared"))?;
        let mut identity = doc.clone();
        identity.phase = Phase::Preparing;
        if identity != self.admitted {
            return Err(refused("durable window identity changed"));
        }
        Ok(doc)
    }

    /// Managed publication is covered by its existing durable journal. Legacy
    /// global push/commit must mark uncertainty before invoking the command.
    pub fn mark_unjournalled_publication_possible(&self) -> Result<(), DatabaseError> {
        if self.admitted.repo_id.is_some() {
            return Ok(());
        }
        self.db.immediate_transaction(|tx| {
            let mut doc = self.owned(tx)?;
            doc.phase = Phase::UnjournalledPublicationPossible;
            store(tx, &doc)
        })
    }

    fn release(&self, tx: &Connection) -> Result<(), DatabaseError> {
        if tx.execute(
            "DELETE FROM kv_state WHERE key=?1",
            [format!("{PREFIX}{}", self.admitted.id)],
        )? != 1
        {
            return Err(refused("durable window release lost ownership"));
        }
        Ok(())
    }

    /// Release local preparation only. Possible publication cannot be released
    /// through the public capability without a verified checkpoint transition.
    pub fn finish_preparation(mut self) -> Result<(), DatabaseError> {
        self.db.immediate_transaction(|tx| {
            if self.owned(tx)?.phase != Phase::Preparing {
                return Err(refused("publication still requires a verified checkpoint"));
            }
            self.release(tx)
        })?;
        self.completed = true;
        Ok(())
    }

    /// Only a completed normal core cycle can release possible publication.
    pub(crate) fn finish(mut self) -> Result<(), DatabaseError> {
        self.db.immediate_transaction(|tx| {
            self.owned(tx)?;
            self.release(tx)
        })?;
        self.completed = true;
        Ok(())
    }

    /// Import's independently verified batch and its global checkpoints become
    /// confirmed together. Later cancellation can release preparation without
    /// losing the last confirmed publication or adopting unpublished local work.
    pub(crate) fn confirm_unjournalled_import_checkpoint(
        &self,
        checkpoint: impl FnOnce(&Connection) -> Result<(), DatabaseError>,
    ) -> Result<(), DatabaseError> {
        self.db.immediate_transaction(|tx| {
            let mut doc = self.owned(tx)?;
            if doc.repo_id.is_some() || doc.phase != Phase::UnjournalledPublicationPossible {
                return Err(refused(
                    "no global import publication requires confirmation",
                ));
            }
            checkpoint(tx)?;
            doc.phase = Phase::Preparing;
            store(tx, &doc)
        })
    }
}

impl Drop for OrdinaryWriterWindow<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let result = self.db.immediate_transaction(|tx| {
            // Retaining uncertainty is read-only: it needs neither a lease
            // renewal nor authority to release the durable window.
            let doc = documents(tx)?
                .into_iter()
                .find(|doc| doc.id == self.admitted.id)
                .ok_or_else(|| refused("durable window disappeared"))?;
            if doc.phase == Phase::Preparing {
                self.owned(tx)?;
                self.release(tx)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            tracing::warn!(window_id=%self.admitted.id,%error,"ordinary writer window retained after drop");
        }
    }
}
