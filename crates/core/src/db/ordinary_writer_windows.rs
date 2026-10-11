//! Durable exclusion between an ordinary engine's local preparation and refresh
//! admission. This is not a publication receipt or permission to retry a write.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
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

/// Private identity fields make this a capability for one admitted run. Drop
/// releases only a still-preparing window. Possible legacy publication survives
/// failure/cancellation/drop, and abrupt process exit preserves every window.
pub struct OrdinaryWriterWindow<'db> {
    db: &'db Database,
    admitted: Document,
    completed: bool,
}

impl Database {
    pub fn begin_ordinary_writer_window(
        &self,
        repo_id: Option<&str>,
    ) -> Result<OrdinaryWriterWindow<'_>, DatabaseError> {
        let repo_id =
            repo_id.filter(|id| !id.is_empty() && *id != super::personal_scope::PERSONAL_SCOPE_KEY);
        let admitted=self.transaction(|tx| {
            require_no_pair_refresh_hold(tx,repo_id)?;
            let writer_epoch=crate::writer_fence::ordinary_current_epoch(tx)?;
            // Legacy engine migrations happen after admission. A human scope
            // still excludes refresh while the old schema lacks UUID columns.
            let has_scope:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('repositories') WHERE name='scope_uuid')",[],|r|r.get(0))?;
            let scope_uuid=if has_scope {
                repo_id.map(|id|tx.query_row("SELECT scope_uuid FROM repositories WHERE id=?1",[id],|row|row.get::<_,Option<String>>(0)).optional()).transpose()?.flatten().flatten()
            } else { None };
            let doc=Document {version:1,id:Uuid::new_v4(),repo_id:repo_id.map(str::to_owned),scope_uuid,writer_epoch,created_at:Utc::now().to_rfc3339(),phase:Phase::Preparing};
            if documents(tx)?.iter().any(|prior|overlaps(prior,&doc)) {
                return Err(refused("another run owns this writer scope"));
            }
            store(tx,&doc)?;
            // Validate newly emitted evidence before committing it too.
            documents(tx)?;
            Ok(doc)
        })?;
        Ok(OrdinaryWriterWindow {
            db: self,
            admitted,
            completed: false,
        })
    }
}

impl OrdinaryWriterWindow<'_> {
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
        self.db.transaction(|tx| {
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

    /// Only a completed normal cycle can release possible legacy publication.
    pub(crate) fn finish(mut self) -> Result<(), DatabaseError> {
        self.db.transaction(|tx| {
            self.owned(tx)?;
            self.release(tx)
        })?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for OrdinaryWriterWindow<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let result = self.db.transaction(|tx| {
            let doc = self.owned(tx)?;
            if doc.phase == Phase::Preparing {
                self.release(tx)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            tracing::warn!(window_id=%self.admitted.id,%error,"ordinary writer window retained after drop");
        }
    }
}
