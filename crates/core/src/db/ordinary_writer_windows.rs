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

/// Startup must refuse a retained preparation or uncertain publication before
/// opening ordinary storage for checkpoint or migration.
pub(crate) fn require_quiet_for_startup(tx: &Connection) -> Result<(), DatabaseError> {
    if !documents(tx)?.is_empty() {
        return Err(refused("another run owns this writer scope"));
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
    pub(crate) fn prepare_emitted_git_convergence(
        &self,
        repo_id: &str,
        branch: &str,
        origin: &str,
        checkpoint: &str,
        remote_tip: &str,
        projection: &str,
    ) -> Result<
        Option<impl FnOnce(&str, &str, &[(String, String)]) -> Result<(), DatabaseError> + '_>,
        DatabaseError,
    > {
        // Private capture lives in the returned single-use closure, not a
        // caller-constructible DTO. No credential values escape this method.
        #[derive(PartialEq, Eq)]
        struct Pin {
            configuration: Vec<String>,
            credentials: String,
            scope: String,
            generation: i64,
            copies: Vec<(String, String, String)>,
            inbound: Option<String>,
            emitted: String,
            svn_revision: i64,
        }
        fn pin(
            tx: &Connection,
            id: &str,
            branch: &str,
            checkpoint: &str,
        ) -> Result<Option<Pin>, DatabaseError> {
            let related: Option<(Option<String>, String, i64)> = tx.query_row(
                "SELECT parent_id,last_git_sha,last_svn_rev FROM repositories WHERE id=?1 AND enabled=1 AND sync_mode='team' AND git_branch=?2 AND (allowed_paths IS NULL OR allowed_paths='[]') AND (blocked_patterns IS NULL OR blocked_patterns='[]')",
                params![id, branch],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?;
            let Some((parent, emitted, svn_revision)) = related else {
                return Ok(None);
            };
            let pair = match parent {
                Some(parent) if parent != id => {
                    let enabled:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM repositories WHERE id=?1 AND enabled=1 AND sync_mode='team')",[&parent],|row|row.get(0))?;
                    if !enabled {return Ok(None);}
                    Some((id.to_owned(),parent))
                },
                Some(_) => return Err(refused("self-parent registration")),
                None => tx.query_row(
                    "SELECT id FROM repositories WHERE parent_id=?1 AND id<>?1 AND enabled=1 AND sync_mode='team' ORDER BY id LIMIT 1",
                    [id], |row| row.get::<_,String>(0),
                ).optional()?.map(|child| (child, id.to_owned())),
            };
            let Some((child, parent)) = pair else {
                return Ok(None); // Unrelated ordinary roots keep their protocol.
            };
            // Unsupported ordinary credential setups do not acquire this new
            // capability. Their existing engine protocol and strict reader stay.
            let credentials = match super::pair_refresh_credentials::resolve_refresh_credentials(
                tx, &child, &parent,
            ) {
                Ok(value) => value.context_digest().to_owned(),
                Err(_) => return Ok(None),
            };
            let mut query = tx.prepare(
                "SELECT json_object('id',id,'scope_uuid',scope_uuid,'parent_id',parent_id,'svn_url',svn_url,'svn_branch',svn_branch,'svn_username',svn_username,'git_provider',git_provider,'git_api_url',git_api_url,'git_repo',git_repo,'git_branch',git_branch,'sync_mode',sync_mode,'poll_interval_secs',poll_interval_secs,'lfs_threshold_mb',lfs_threshold_mb,'auto_merge',auto_merge,'enabled',enabled,'allowed_paths',allowed_paths,'blocked_patterns',blocked_patterns) FROM repositories WHERE id=?1 OR id=?2 OR parent_id=?1 ORDER BY id",
            )?;
            let configuration = query.query_map(params![id,parent], |row| row.get(0))?
                .collect::<Result<Vec<String>,_>>()?;
            let scope = super::repo_scope_identity::repository_scope_uuid(tx, id)?;
            if !Uuid::parse_str(&scope).is_ok_and(|value| !value.is_nil() && value.to_string()==scope) {
                return Err(refused("convergence requires durable scope"));
            }
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key IN (?1,?2,?3))",
                params![format!("svn_to_git_push_v1:active:{id}"),format!("git_to_svn_commit_v1:active:{id}"),format!("import_operation_v1:active:{id}")],
                |row| row.get(0),
            )?;
            if active {
                return Err(refused("convergence cannot cross an active publication or import"));
            }
            let scope_owners:i64=tx.query_row("SELECT count(*) FROM repositories WHERE scope_uuid=?1",[&scope],|row|row.get(0))?;
            if scope_owners!=1 {return Err(refused("convergence scope has multiple owners"));}
            let generation = crate::echo_receipt_scope::repo_echo_generation_tx(tx, id)?;
            if generation <= 0 {
                return Err(refused("convergence requires positive generation"));
            }
            let mut keys = tx.prepare(
                "SELECT key,value,updated_at FROM kv_state WHERE key IN (?1,?2,?3,?4,?5,?6) ORDER BY key",
            )?;
            let copies = keys.query_map(params![
                super::repo_scope_identity::last_git_sha_kv_key(&scope),
                super::repo_scope_identity::legacy_last_git_sha_kv_key(id),
                super::repo_scope_identity::repo_echo_generation_kv_key(&scope),
                super::repo_scope_identity::legacy_repo_echo_generation_kv_key(id),
                super::repo_scope_identity::LEGACY_REPO_ID_KV_READS_KEY,
                crate::echo_receipt_scope::handled_git_no_target_state_key(&scope,generation,checkpoint),
            ], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?
                .collect::<Result<Vec<(String,String,String)>,_>>()?;
            let inbound = crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv_tx(tx,id)?;
            let svn_copy: Option<String> = tx.query_row(
                "SELECT value FROM kv_state WHERE key=?1",[format!("last_svn_rev_{id}")],|row|row.get(0),
            ).optional()?;
            if svn_revision <= 0 || svn_copy.as_deref()!=Some(svn_revision.to_string().as_str()) {
                return Err(refused("convergence SVN checkpoint is not coherent"));
            }
            Ok(Some(Pin { configuration, credentials, scope, generation, copies, inbound, emitted, svn_revision }))
        }
        let id = repo_id.to_owned();
        let branch = branch.to_owned();
        let origin = origin.to_owned();
        let checkpoint = checkpoint.to_owned();
        let projection = projection.to_owned();
        if !crate::history_inspect::is_full_git_oid(&checkpoint)
            || !crate::history_inspect::is_full_git_oid(remote_tip) || origin.is_empty()
        {
            return Err(refused("convergence source observation is malformed"));
        }
        let before = self.db.immediate_transaction(|tx| {
            let doc = self.owned(tx)?;
            if doc.writer_epoch.is_none() { return Ok(None); }
            if doc.phase!=Phase::Preparing || doc.repo_id.as_deref()!=Some(id.as_str()) {
                return Err(refused("convergence requires current engine window"));
            }
            require_no_pair_refresh_hold(tx,Some(&id))?;
            let Some(before) = pin(tx,&id,&branch,&checkpoint)? else { return Ok(None) };
            // This attestation belongs to the optional convergence capability,
            // not to no-epoch or unrelated ordinary callers declined above.
            self.db.opened_file()?.require_unaliased_owner(tx)?;
            if doc.scope_uuid.as_deref()!=Some(before.scope.as_str()) || before.emitted!=remote_tip {
                return Err(refused("convergence registration or observed tip changed"));
            }
            let old_authority = match before.inbound.as_deref() {
                Some(handled) if handled==checkpoint => {
                    let direct: i64 = tx.query_row(
                        "SELECT count(*) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND status='applied' AND (direction='git_to_svn' OR (direction='svn_to_git' AND svn_rev>0 AND NOT EXISTS(SELECT 1 FROM sync_records WHERE repo_id=?1 AND direction='git_to_svn' AND status='applied')))",
                        params![id,checkpoint],|row|row.get(0),
                    )?;
                    let accepted = before.copies.iter().any(|(key,raw,_)| {
                        key==&crate::echo_receipt_scope::handled_git_no_target_state_key(&before.scope,before.generation,&checkpoint)
                            && serde_json::from_str(raw).is_ok_and(|receipt| {
                                crate::echo_suppression::verify_no_target_receipt(
                                    &receipt,&id,&checkpoint,&projection,before.generation,Some(&before.scope),
                                )==crate::echo_suppression::NoTargetReceiptVerdict::Accepted
                            })
                    });
                    if Self::emitted_frontier_recorded(tx,&id,&checkpoint)? {
                        if !Self::verified_emitted_git_frontier(tx,&self.db,&id,&checkpoint,&projection)? {
                            return Err(refused("recorded old emitted proof is stale or missing"));
                        }
                        use sha2::Digest;
                        let proof_key=format!("handled_git_emitted_frontier_v1:{}:g{}:{}",before.scope,before.generation,checkpoint);
                        let raw:String=tx.query_row("SELECT value FROM kv_state WHERE key=?1",[&proof_key],|row|row.get(0))?;
                        serde_json::json!({"kind":"convergence","key":proof_key,"sha256":hex::encode(sha2::Sha256::digest(raw.as_bytes()))})
                    } else if direct>0 && !Self::emitted_frontier_needs_retained_history(tx,&id,&checkpoint)? {
                        let mut rows=tx.prepare("SELECT json_array(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND status='applied' AND (direction='git_to_svn' OR (direction='svn_to_git' AND svn_rev>0 AND NOT EXISTS(SELECT 1 FROM sync_records WHERE repo_id=?1 AND direction='git_to_svn' AND status='applied'))) ORDER BY id")?;
                        let rows=rows.query_map(params![id,checkpoint],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
                        serde_json::json!({"kind":"direct","rows":rows})
                    } else if accepted {
                        let key=crate::echo_receipt_scope::handled_git_no_target_state_key(&before.scope,before.generation,&checkpoint);
                        let raw=before.copies.iter().find(|(name,_,_)|name==&key).map(|(_,raw,_)|raw.clone())
                            .ok_or_else(||refused("accepted old receipt disappeared"))?;
                        serde_json::json!({"kind":"no_target","key":key,"raw":raw})
                    } else {
                        // The ordinary inspector may admit an older supported
                        // no-target representation outside this narrow durable
                        // proof format. Decline the optional convergence route;
                        // never reinterpret it, mutate it, or break that route.
                        return Ok(None);
                    }
                }
                None if before.emitted==checkpoint => {
                    let applied: i64 = tx.query_row(
                        "SELECT count(*) FROM sync_records WHERE repo_id=?1 AND status='applied' AND direction IN ('svn_to_git','git_to_svn')",[&id],|row|row.get(0),
                    )?;
                    let retained:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM kv_state WHERE key=?1 OR key LIKE ?2)",params![format!("handled_git_baseline_{id}"),format!("handled_git_emitted_frontier_v1:{}:%",before.scope)],|row|row.get(0))?;
                    if applied!=0 || retained || super::repo_scope_identity::repository_has_stored_no_target_receipt_kv(tx,&id)? {
                        // No durable record proves an old absence after an
                        // interrupted publication. Retain evidence and refuse.
                        return Err(refused("absent inbound needs original pre-publication bootstrap capture"));
                    }
                    serde_json::json!({"kind":"bootstrap_absent"})
                }
                _ => return Err(refused("convergence old inbound changed")),
            };
            Ok(Some((before,old_authority)))
        })?;
        let Some((before,old_authority)) = before else { return Ok(None) };
        Ok(Some(move |observed: &str, observed_origin: &str, commits: &[(String,String)]| {
            self.db.immediate_transaction(|tx| {
                self.db.opened_file()?.require_unaliased_owner(tx)?;
                let doc = self.owned(tx)?;
                if doc.phase!=Phase::Preparing || doc.repo_id.as_deref()!=Some(id.as_str()) {
                    return Err(refused("convergence lost current engine window"));
                }
                require_no_pair_refresh_hold(tx,Some(&id))?;
                let current = pin(tx,&id,&branch,&checkpoint)?
                    .ok_or_else(||refused("convergence registration disappeared"))?;
                if doc.scope_uuid.as_deref()!=Some(current.scope.as_str())
                    || current.configuration!=before.configuration
                    || current.credentials!=before.credentials || current.scope!=before.scope
                    || current.generation!=before.generation || current.copies!=before.copies
                    || current.inbound!=before.inbound || observed_origin!=origin
                    || current.svn_revision<before.svn_revision
                    || current.emitted!=observed
                {
                    return Err(refused("convergence source or captured authority drifted"));
                }
                match old_authority["kind"].as_str() {
                    Some("convergence") => {
                        use sha2::Digest;
                        let key=old_authority["key"].as_str().ok_or_else(||refused("captured old proof key missing"))?;
                        let raw:Option<String>=tx.query_row("SELECT value FROM kv_state WHERE key=?1",[key],|row|row.get(0)).optional()?;
                        if raw.as_ref().is_none_or(|raw|old_authority["sha256"]!=hex::encode(sha2::Sha256::digest(raw.as_bytes()))) {
                            return Err(refused("captured old emitted proof changed"));
                        }
                        if !Self::verified_emitted_git_frontier(tx,&self.db,&id,&checkpoint,&projection)? {
                            return Err(refused("captured old emitted proof disappeared"));
                        }
                    }
                    Some("direct") => {
                        for raw in old_authority["rows"].as_array().ok_or_else(||refused("captured old rows missing"))? {
                            let raw=raw.as_str().ok_or_else(||refused("captured old row bytes missing"))?;
                            let row:serde_json::Value=serde_json::from_str(raw).map_err(|_|refused("captured old row corrupt"))?;
                            let stored:Option<String>=tx.query_row("SELECT json_array(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) FROM sync_records WHERE id=?1",[row[0].as_str().ok_or_else(||refused("captured old row ID missing"))?],|row|row.get(0)).optional()?;
                            if stored.as_deref()!=Some(raw) {return Err(refused("captured old row disappeared or changed"));}
                        }
                    }
                    Some("bootstrap_absent"|"no_target") => (),
                    _ => return Err(refused("unknown captured old authority")),
                }
                if commits.is_empty() {
                    if current.emitted!=checkpoint || current.svn_revision!=before.svn_revision {
                        return Err(refused("empty convergence range cannot claim progress"));
                    }
                    return Ok(()); // No evidence is synthesized for an empty range.
                }
                if commits.last().map(|value|value.0.as_str())!=Some(observed) {
                    return Err(refused("convergence range is incomplete"));
                }
                let mut documents_query=tx.prepare(
                    "SELECT key,value FROM kv_state WHERE substr(key,1,?1)=?2 ORDER BY key",
                )?;
                let prefix="svn_to_git_push_v1:document:";
                let documents=documents_query.query_map(params![prefix.len(),prefix],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
                    .collect::<Result<Vec<_>,_>>()?;
                let mut retained_chain=Vec::with_capacity(commits.len());
                let mut previous=checkpoint.clone();
                let mut previous_revision=0;
                let mut seen=std::collections::BTreeSet::new();
                for (sha,tree) in commits {
                    if !seen.insert(sha) || !crate::history_inspect::is_full_git_oid(sha)
                        || !crate::history_inspect::is_full_git_oid(tree) {
                        return Err(refused("convergence duplicate or malformed commit"));
                    }
                    let mut matching=Vec::new();
                    for (key,raw) in &documents {
                        let op: super::git_push_operations::GitPushOperation=serde_json::from_str(raw)
                            .map_err(|_|refused("corrupt producer evidence"))?;
                        if op.repo_id==id && op.intended_local_git_sha==*sha {
                            if serde_json::to_string(&op).map_err(|_|refused("producer encoding"))?!=*raw {
                                return Err(refused("noncanonical producer evidence"));
                            }
                            if *key!=format!("{prefix}{}",op.id) || op.version!=1
                                || op.operation_type!="svn_to_git_push" {
                                return Err(refused("unknown producer evidence"));
                            }
                            matching.push(op);
                        }
                    }
                    let [op]=matching.as_slice() else {
                        return Err(refused("convergence needs one exact producer receipt"));
                    };
                    if op.state!=super::git_push_operations::GitPushOperationState::Completed
                        || op.pre_push_git_remote!="origin" || op.pre_push_git_branch!=branch
                        || op.target_fingerprint!=super::git_push_operations::git_push_target_fingerprint(&id,"origin",&branch)
                        || op.pre_push_git_sha!=previous || op.intended_local_git_parent.as_deref()!=Some(previous.as_str())
                        || op.intended_local_git_tree!=*tree || op.last_confirmed_git_sha.as_deref()!=Some(sha.as_str())
                        || op.last_confirmed_git_tree.as_deref()!=Some(tree.as_str()) || op.resume_authorized
                        || op.source_svn_rev<=previous_revision || op.source_svn_rev>current.svn_revision
                    {
                        return Err(refused("convergence producer chain differs"));
                    }
                    let mut mappings=tx.prepare(
                        "SELECT repo_id,git_sha,svn_rev,direction,status FROM sync_records WHERE (repo_id=?1 OR repo_id IS NULL OR repo_id='') AND (git_sha=?2 OR svn_rev=?3)",
                    )?;
                    let rows=mappings.query_map(params![id,sha,op.source_svn_rev],|row|Ok((row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?)))?
                        .collect::<Result<Vec<_>,_>>()?;
                    if rows.len()!=1 || rows[0]!=(Some(id.clone()),Some(sha.clone()),Some(op.source_svn_rev),"svn_to_git".to_owned(),"applied".to_owned()) {
                        return Err(refused("convergence correspondence is missing or contradictory"));
                    }
                    let proof_key=format!("handled_git_emitted_frontier_v1:{}:g{}:{}",current.scope,current.generation,sha);
                    let stale:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM kv_state WHERE key LIKE ?1 AND key<>?2)",params![format!("handled_git_emitted_frontier_v1:{}:%:{}",current.scope,sha),proof_key],|row|row.get(0))?;
                    if stale {return Err(refused("stale emitted proof requires separate reconciliation"));}
                    let producer_key=format!("svn_to_git_push_v1:document:{}",op.id);
                    let producer_raw=documents.iter().find(|(key,_)|key==&producer_key).map(|(_,raw)|raw.clone())
                        .ok_or_else(||refused("completed producer bytes disappeared"))?;
                    let mapping_raw:String=tx.query_row("SELECT json_array(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND svn_rev=?3 AND direction='svn_to_git' AND status='applied'",params![id,sha,op.source_svn_rev],|row|row.get(0))?;
                    retained_chain.push(serde_json::json!({"sha":sha,"tree":tree,"producer_key":producer_key,"producer_raw":producer_raw,"mapping_raw":mapping_raw}));
                    previous=sha.clone();previous_revision=op.source_svn_rev;
                }
                if previous_revision!=current.svn_revision {
                    return Err(refused("convergence final SVN coordinate differs"));
                }
                use hmac::Mac;
                use sha2::Digest;
                let proof_key=format!("handled_git_emitted_frontier_v1:{}:g{}:{}",current.scope,current.generation,observed);
                let existing:Option<String>=tx.query_row("SELECT value FROM kv_state WHERE key=?1",[&proof_key],|row|row.get(0)).optional()?;
                if existing.is_some() {
                    if !Self::verified_emitted_git_frontier(tx,&self.db,&id,observed,&projection)? {
                        return Err(refused("existing emitted proof is not valid"));
                    }
                    // Keep the immutable historical proof. Fresh observation,
                    // captured old authority and current owner were rechecked.
                } else {
                    let native=self.db.opened_file()?;
                    let payload=serde_json::json!({
                        "version":1,"outcome":"complete_svn_emitted_frontier",
                        "repo_id":id,"scope_uuid":current.scope,"generation":current.generation,
                        "configuration_digest":hex::encode(sha2::Sha256::digest(serde_json::to_vec(&current.configuration).map_err(|_|refused("configuration encoding"))?)),
                        "projection":projection,"origin_digest":hex::encode(sha2::Sha256::digest(origin.as_bytes())),
                        "branch":branch,"native_database":{"path":native.path.to_str().ok_or_else(||refused("non-UTF8 native database path"))?,"device":native.device,"inode":native.inode,"owner":rustix::process::geteuid().as_raw()},
                        "writer_window":doc.id.to_string(),"writer_epoch":doc.writer_epoch,
                        "old_frontier":checkpoint,"old_inbound":before.inbound,"old_svn_revision":before.svn_revision,
                        "old_authority":old_authority,"new_git_sha":observed,
                        "new_git_tree":commits.last().map(|value|value.1.as_str()).ok_or_else(||refused("missing final emitted tree"))?,
                        "new_svn_revision":current.svn_revision,"chain":retained_chain,
                        "credentials_at_transition":before.credentials,
                    });
                    let key=super::pair_refresh_credentials::fingerprint_key(tx)?;
                    let mut mac=hmac::Hmac::<sha2::Sha256>::new_from_slice(&key).map_err(|_|refused("proof authority key"))?;
                    mac.update(&serde_json::to_vec(&("reposync_emitted_frontier_v1",&payload)).map_err(|_|refused("proof encoding"))?);
                    let record=serde_json::json!({"payload":payload,"mac":hex::encode(mac.finalize().into_bytes())});
                    tx.execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)",params![proof_key,serde_json::to_string(&record).map_err(|_|refused("proof encoding"))?,Utc::now().to_rfc3339()])?;
                }
                crate::echo_receipt_scope::write_scoped_last_git_sha_kv(tx,&id,observed,&Utc::now().to_rfc3339())?;
                if !Self::verified_emitted_git_frontier(tx,&self.db,&id,observed,&projection)? {
                    return Err(refused("persisted emitted proof is not consumable"));
                }
                Ok(())
            })
        }))
    }

    /// Authenticated same-incarnation refresh history remains authoritative
    /// even when disposable directional audit rows have been pruned. Such a
    /// later SVN-origin SHA cannot become an old-import baseline by that pruning.
    pub(crate) fn emitted_frontier_needs_retained_history(
        tx: &Connection,
        repo_id: &str,
        sha: &str,
    ) -> Result<bool,DatabaseError> {
        super::pair_refresh_operations::require_no_pair_refresh_hold(tx,Some(repo_id))?;
        let scope=super::repo_scope_identity::repository_scope_uuid(tx,repo_id)?;
        let generation=crate::echo_receipt_scope::repo_echo_generation_tx(tx,repo_id)?;
        let prefix="pair_refresh_execution_v3:document:";
        Ok(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_state WHERE substr(key,1,?1)=?2 AND json_extract(value,'$.phase')='completed' AND json_extract(value,'$.pins.child.repo_id')=?3 AND json_extract(value,'$.pins.child.scope_uuid')=?4 AND json_extract(value,'$.pins.child.echo_generation')=?5 AND json_extract(value,'$.pins.git.prepared_sha')<>?6) AND EXISTS(SELECT 1 FROM sync_records WHERE repo_id=?3 AND git_sha=?6 AND direction='svn_to_git' AND status='applied' AND svn_rev>0) AND NOT EXISTS(SELECT 1 FROM sync_records WHERE repo_id=?3 AND git_sha=?6 AND direction='git_to_svn' AND status='applied')",
            params![prefix.len(),prefix,repo_id,scope,generation,sha],|row|row.get(0),
        )?)
    }

    /// Once a frontier used this authority, dependency loss cannot silently
    /// reclassify it as an old import after diagnostic rows are pruned.
    pub(crate) fn emitted_frontier_recorded(
        tx: &Connection,
        repo_id: &str,
        sha: &str,
    ) -> Result<bool,DatabaseError> {
        let scope=match super::repo_scope_identity::repository_scope_uuid(tx,repo_id) {
            Ok(scope)=>scope,
            Err(DatabaseError::NotFound { .. })=>return Ok(false),
            Err(error)=>return Err(error),
        };
        Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM kv_state WHERE key LIKE ?1)",
            [format!("handled_git_emitted_frontier_v1:{scope}:%:{sha}")],|row|row.get(0))?)
    }

    /// A retained completed transition, not permission for a new publication.
    /// Validation is local/read-only; missing dependencies are never repaired.
    pub(crate) fn verified_emitted_git_frontier(
        tx: &Connection,
        db: &Database,
        repo_id: &str,
        sha: &str,
        projection: &str,
    ) -> Result<bool, DatabaseError> {
        use hmac::Mac;
        use sha2::Digest;
        let scope = match super::repo_scope_identity::repository_scope_uuid(tx, repo_id) {
            Ok(scope) => scope,
            Err(DatabaseError::NotFound { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let generation = crate::echo_receipt_scope::repo_echo_generation_tx(tx, repo_id)?;
        let key_for = |tip: &str| format!("handled_git_emitted_frontier_v1:{scope}:g{generation}:{tip}");
        let mut target = sha.to_owned();
        let mut visited = std::collections::BTreeSet::new();
        let mut expected_digest:Option<String>=None;
        loop {
            if !crate::history_inspect::is_full_git_oid(&target) || !visited.insert(target.clone()) {
                return Err(refused("malformed or cyclic emitted-frontier proof"));
            }
            let raw: Option<String> = tx.query_row(
                "SELECT value FROM kv_state WHERE key=?1",[key_for(&target)],|row|row.get(0),
            ).optional()?;
            let Some(raw) = raw else {
                return if visited.len()==1 { Ok(false) } else {
                    Err(refused("required prior emitted-frontier proof is missing"))
                };
            };
            if expected_digest.as_ref().is_some_and(|expected|*expected!=hex::encode(sha2::Sha256::digest(raw.as_bytes()))) {
                return Err(refused("prior emitted-frontier proof dependency changed"));
            }
            let native=db.opened_file()?;
            native.require_unaliased_owner(tx)?;
            let scope_owners:i64=tx.query_row("SELECT count(*) FROM repositories WHERE scope_uuid=?1",[&scope],|row|row.get(0))?;
            if scope_owners!=1 {return Err(refused("emitted proof scope has multiple owners"));}
            let record: serde_json::Value=serde_json::from_str(&raw)
                .map_err(|_|refused("malformed emitted-frontier proof"))?;
            if record.as_object().is_none_or(|fields| fields.len()!=2 || !fields.contains_key("payload") || !fields.contains_key("mac"))
                || serde_json::to_string(&record).map_err(|_|refused("proof encoding"))?!=raw {
                return Err(refused("noncanonical emitted-frontier proof"));
            }
            let payload=&record["payload"];
            let fields=["version","outcome","repo_id","scope_uuid","generation","configuration_digest","projection","origin_digest","branch","native_database","writer_window","writer_epoch","old_frontier","old_inbound","old_svn_revision","old_authority","new_git_sha","new_git_tree","new_svn_revision","chain","credentials_at_transition"];
            if payload.as_object().is_none_or(|value|value.len()!=fields.len() || fields.iter().any(|key|!value.contains_key(*key))) {
                return Err(refused("unknown emitted-frontier proof schema"));
            }
            let encoded=serde_json::to_vec(&("reposync_emitted_frontier_v1",payload))
                .map_err(|_|refused("proof encoding"))?;
            let key=super::pair_refresh_credentials::fingerprint_key(tx)?;
            let mut mac=hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
                .map_err(|_|refused("proof authority key"))?;
            mac.update(&encoded);
            let signature=record["mac"].as_str().and_then(|value|hex::decode(value).ok())
                .ok_or_else(||refused("malformed emitted-frontier MAC"))?;
            mac.verify_slice(&signature).map_err(|_|refused("emitted-frontier authority mismatch"))?;
            let native_pin=serde_json::json!({
                "path":native.path.to_str().ok_or_else(||refused("non-UTF8 native database path"))?,
                "device":native.device,"inode":native.inode,"owner":rustix::process::geteuid().as_raw(),
            });
            let (parent,branch,current_revision):(Option<String>,String,i64)=tx.query_row(
                "SELECT parent_id,git_branch,last_svn_rev FROM repositories WHERE id=?1 AND enabled=1 AND sync_mode='team'",
                [repo_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?;
            let parent=match parent {
                Some(value) if value!=repo_id=>{
                    let enabled:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM repositories WHERE id=?1 AND enabled=1 AND sync_mode='team')",[&value],|row|row.get(0))?;
                    if !enabled {return Err(refused("emitted proof parent is no longer enabled"));}
                    value
                },
                Some(_)=>return Err(refused("self-parent proof registration")),
                None=>{
                    let related:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM repositories WHERE parent_id=?1 AND id<>?1 AND enabled=1 AND sync_mode='team')",[repo_id],|row|row.get(0))?;
                    if !related {return Err(refused("emitted-frontier proof no longer belongs to a pair"));}
                    repo_id.to_owned()
                }
            };
            let mut query=tx.prepare(
                "SELECT json_object('id',id,'scope_uuid',scope_uuid,'parent_id',parent_id,'svn_url',svn_url,'svn_branch',svn_branch,'svn_username',svn_username,'git_provider',git_provider,'git_api_url',git_api_url,'git_repo',git_repo,'git_branch',git_branch,'sync_mode',sync_mode,'poll_interval_secs',poll_interval_secs,'lfs_threshold_mb',lfs_threshold_mb,'auto_merge',auto_merge,'enabled',enabled,'allowed_paths',allowed_paths,'blocked_patterns',blocked_patterns) FROM repositories WHERE id=?1 OR id=?2 OR parent_id=?1 ORDER BY id",
            )?;
            let configuration=query.query_map(params![repo_id,parent],|row|row.get::<_,String>(0))?
                .collect::<Result<Vec<_>,_>>()?;
            let configuration_digest=hex::encode(sha2::Sha256::digest(
                serde_json::to_vec(&configuration).map_err(|_|refused("configuration encoding"))?,
            ));
            let positive=|key:&str|payload[key].as_i64().is_some_and(|value|value>0);
            let full_digest=|value:&serde_json::Value|value.as_str().is_some_and(|value|value.len()==64 && value.bytes().all(|byte|byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
            if payload["version"]!=1 || payload["outcome"]!="complete_svn_emitted_frontier"
                || payload["repo_id"]!=repo_id || payload["scope_uuid"]!=scope
                || payload["generation"]!=generation || generation<=0
                || payload["configuration_digest"]!=configuration_digest || payload["projection"]!=projection
                || payload["branch"]!=branch || payload["native_database"]!=native_pin
                || payload["new_git_sha"]!=target || !positive("new_svn_revision")
                || payload["new_svn_revision"].as_i64().is_none_or(|revision|revision>current_revision)
                || !positive("old_svn_revision")
                || payload["old_svn_revision"].as_i64()>payload["new_svn_revision"].as_i64()
                || payload["writer_epoch"].as_u64().is_none_or(|epoch|epoch==0)
                || !full_digest(&payload["origin_digest"]) || !full_digest(&payload["credentials_at_transition"])
                || !payload["writer_window"].as_str().is_some_and(|raw|Uuid::parse_str(raw).is_ok_and(|id|!id.is_nil() && id.to_string()==raw)) {
                return Err(refused("stale or foreign emitted-frontier proof"));
            }
            let old=payload["old_frontier"].as_str().filter(|value|crate::history_inspect::is_full_git_oid(value))
                .ok_or_else(||refused("malformed old emitted frontier"))?;
            let chain=payload["chain"].as_array().filter(|chain|!chain.is_empty())
                .ok_or_else(||refused("empty retained producer chain"))?;
            let mut previous=old.to_owned();let mut revision=0;let mut final_tree=String::new();
            for entry in chain {
                if entry.as_object().is_none_or(|fields|fields.len()!=5 || ["sha","tree","producer_key","producer_raw","mapping_raw"].iter().any(|key|!fields.contains_key(*key))) {
                    return Err(refused("unknown retained chain entry"));
                }
                let tip=entry["sha"].as_str().filter(|value|crate::history_inspect::is_full_git_oid(value)).ok_or_else(||refused("malformed retained SHA"))?;
                let tree=entry["tree"].as_str().filter(|value|crate::history_inspect::is_full_git_oid(value)).ok_or_else(||refused("malformed retained tree"))?;
                let producer_key=entry["producer_key"].as_str().ok_or_else(||refused("missing producer dependency"))?;
                let stored:Option<String>=tx.query_row("SELECT value FROM kv_state WHERE key=?1",[producer_key],|row|row.get(0)).optional()?;
                let raw=entry["producer_raw"].as_str().ok_or_else(||refused("missing retained producer bytes"))?;
                if stored.as_deref()!=Some(raw) {return Err(refused("retained producer dependency changed or missing"));}
                let op:super::git_push_operations::GitPushOperation=serde_json::from_str(raw).map_err(|_|refused("corrupt retained producer"))?;
                if serde_json::to_string(&op).map_err(|_|refused("producer encoding"))?!=raw
                    || producer_key!=format!("svn_to_git_push_v1:document:{}",op.id)
                    || op.version!=1 || op.operation_type!="svn_to_git_push" || op.repo_id!=repo_id
                    || op.state!=super::git_push_operations::GitPushOperationState::Completed
                    || op.pre_push_git_remote!="origin" || op.pre_push_git_branch!=branch
                    || op.target_fingerprint!=super::git_push_operations::git_push_target_fingerprint(repo_id,"origin",&branch)
                    || op.pre_push_git_sha!=previous || op.intended_local_git_parent.as_deref()!=Some(previous.as_str())
                    || op.intended_local_git_sha!=tip || op.intended_local_git_tree!=tree
                    || op.last_confirmed_git_sha.as_deref()!=Some(tip) || op.last_confirmed_git_tree.as_deref()!=Some(tree)
                    || op.resume_authorized || op.source_svn_rev<=revision {
                    return Err(refused("retained producer chain contradicts proof"));
                }
                let mut mappings=tx.prepare("SELECT json_array(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) FROM sync_records WHERE (repo_id=?1 OR repo_id IS NULL OR repo_id='') AND (git_sha=?2 OR svn_rev=?3) ORDER BY id")?;
                let rows=mappings.query_map(params![repo_id,tip,op.source_svn_rev],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
                let raw_mapping=entry["mapping_raw"].as_str().ok_or_else(||refused("missing mapping dependency"))?;
                let mapping:serde_json::Value=serde_json::from_str(raw_mapping).map_err(|_|refused("invalid mapping dependency"))?;
                if rows.len()!=1 || rows[0]!=raw_mapping || mapping.as_array().is_none_or(|row|row.len()!=10)
                    || mapping[1]!=repo_id || mapping[2]!=op.source_svn_rev || mapping[3]!=tip
                    || mapping[4]!="svn_to_git" || mapping[9]!="applied" {
                    return Err(refused("retained correspondence changed or missing"));
                }
                previous=tip.to_owned();revision=op.source_svn_rev;final_tree=tree.to_owned();
            }
            if previous!=target || payload["new_git_tree"]!=final_tree || payload["new_svn_revision"]!=revision {
                return Err(refused("retained final coordinates differ"));
            }
            let authority=&payload["old_authority"];
            match authority["kind"].as_str() {
                Some("bootstrap_absent")=>{
                    if authority.as_object().is_none_or(|fields|fields.len()!=1)
                        || !payload["old_inbound"].is_null()
                        || chain[0]["producer_raw"].as_str().and_then(|raw|serde_json::from_str::<super::git_push_operations::GitPushOperation>(raw).ok()).is_none_or(|op|op.source_svn_rev<=payload["old_svn_revision"].as_i64().unwrap_or(i64::MAX)) {
                        return Err(refused("invalid retained bootstrap transition"));
                    }
                    return Ok(true); // MAC binds actual captured historical absence.
                }
                Some("direct")=>{
                    if authority.as_object().is_none_or(|fields|fields.len()!=2) || payload["old_inbound"]!=old {
                        return Err(refused("invalid retained direct authority"));
                    }
                    let rows=authority["rows"].as_array().filter(|rows|!rows.is_empty()).ok_or_else(||refused("missing old applied authority"))?;
                    for raw in rows {
                        let raw=raw.as_str().ok_or_else(||refused("invalid old applied bytes"))?;
                        let row:serde_json::Value=serde_json::from_str(raw).map_err(|_|refused("invalid old applied row"))?;
                        if row.as_array().is_none_or(|row|row.len()!=10) || row[1]!=repo_id || row[3]!=old || row[9]!="applied"
                            || !matches!(row[4].as_str(),Some("git_to_svn"|"svn_to_git")) {
                            return Err(refused("foreign old applied authority"));
                        }
                        let stored:Option<String>=tx.query_row("SELECT json_array(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) FROM sync_records WHERE id=?1",[row[0].as_str().ok_or_else(||refused("old row ID missing"))?],|row|row.get(0)).optional()?;
                        if stored.as_deref()!=Some(raw) {return Err(refused("old applied authority dependency missing or changed"));}
                    }
                    return Ok(true);
                }
                Some("no_target")=>{
                    if authority.as_object().is_none_or(|fields|fields.len()!=3) || payload["old_inbound"]!=old {return Err(refused("invalid retained no-target authority"));}
                    let expected=crate::echo_receipt_scope::handled_git_no_target_state_key(&scope,generation,old);
                    if authority["key"]!=expected {return Err(refused("foreign old no-target key"));}
                    let stored:Option<String>=tx.query_row("SELECT value FROM kv_state WHERE key=?1",[expected],|row|row.get(0)).optional()?;
                    let raw=authority["raw"].as_str().ok_or_else(||refused("old no-target bytes missing"))?;
                    let receipt=serde_json::from_str(raw).map_err(|_|refused("old no-target bytes corrupt"))?;
                    if stored.as_deref()!=Some(raw) || crate::echo_suppression::verify_no_target_receipt(&receipt,repo_id,old,projection,generation,Some(&scope))!=crate::echo_suppression::NoTargetReceiptVerdict::Accepted {return Err(refused("old no-target dependency changed"));}
                    return Ok(true);
                }
                Some("convergence")=>{
                    if authority.as_object().is_none_or(|fields|fields.len()!=3) || payload["old_inbound"]!=old || authority["key"]!=key_for(old) || !full_digest(&authority["sha256"]) {return Err(refused("foreign prior convergence authority"));}
                    expected_digest=authority["sha256"].as_str().map(str::to_owned);
                    target=old.to_owned(); // Iterative, finite dependency walk; cycles refuse.
                }
                _=>return Err(refused("unknown retained old authority")),
            }
        }
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
