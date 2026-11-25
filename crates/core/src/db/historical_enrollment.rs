//! Historical Team root enrollment after an installed database has been migrated.
use serde::Serialize;

/// Existing registered ownership and retained evidence for a single legacy root.
/// The verifier binds these pins to the SVN and Git observations in its review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyPairPins {
    pub repo_id: String,
    pub scope_uuid: String,
    pub generation: i64,
    pub svn_url: String,
    pub svn_branch: String,
    pub svn_username: String,
    pub git_provider: String,
    pub git_api_url: String,
    pub git_repo: String,
    pub git_branch: String,
    pub last_svn_rev: i64,
    pub import_completion_svn_rev: Option<i64>,
    pub import_log_entry_count: Option<i64>,
    pub last_git_sha: String,
    pub mappings: Vec<(i64, String)>,
    pub storage_sha256: String,
}

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::historical_transport::{read_registered_transport, RegisteredTransport};
use super::lineage_checkpoints::{self, CanonicalLineage};
use super::{schema, Database};
use crate::errors::DatabaseError;
use crate::legacy_svn_recovery::VerifiedHistoricalLineage;

const ACTION: &str = "historical_lineage_enrolled";

fn refused(reason: &'static str) -> DatabaseError {
    DatabaseError::Other(format!("historical lineage enrollment refused: {reason}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentOutcome {
    Enrolled,
    AlreadyEnrolled,
}

fn is_installed(conn: &Connection) -> Result<bool, DatabaseError> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let installed = schema::installed_team::pending(conn, version)?;
    if !installed
        && (version != schema::CURRENT_SCHEMA_VERSION || !schema::installed_team::converted(conn)?)
    {
        return Err(refused(
            "a supported installed or current Team catalog is required",
        ));
    }
    Ok(installed)
}

fn lower_hex(value: &str, width: usize) -> bool {
    value.len() == width
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// Hash complete selected rows with boundaries. No message, author or login is written to the receipt.
fn hash_rows(
    conn: &Connection,
    id: &str,
    query: &str,
    hash: &mut Sha256,
) -> Result<(), DatabaseError> {
    let mut stmt = conn.prepare(query)?;
    let mut rows = stmt.query([id])?;
    hash.update([0]);
    while let Some(row) = rows.next()? {
        let value = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| refused("invalid stored row"))?;
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    Ok(())
}

fn stored_pair(
    conn: &Connection,
    id: &str,
    installed: bool,
) -> Result<LegacyPairPins, DatabaseError> {
    // Require the original direct, unfiltered root and a stable paused registration.
    let mut pair = conn.query_row(
        "SELECT id, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch,
                last_svn_rev,last_git_sha FROM repositories WHERE id=?1 AND parent_id IS NULL
         AND sync_mode='direct' AND ((enabled=0 AND sync_status='idle') OR sync_status='error_paused') AND COALESCE(allowed_paths,'[]')='[]'
         AND COALESCE(blocked_patterns,'[]')='[]'",
        [id], |r| Ok(LegacyPairPins {
            repo_id:r.get(0)?, svn_url:r.get(1)?, svn_branch:r.get(2)?, svn_username:r.get(3)?,
            git_provider:r.get(4)?, git_api_url:r.get(5)?, git_repo:r.get(6)?, git_branch:r.get(7)?,
            last_svn_rev:r.get(8)?, import_completion_svn_rev:None, import_log_entry_count:None, last_git_sha:r.get(9)?, scope_uuid:String::new(),
            generation:0, mappings:vec![], storage_sha256:String::new(),
        })
    ).optional()?.ok_or_else(|| refused("the requested paused full-import root was not found"))?;
    if !installed {
        pair.scope_uuid = conn
            .query_row(
                "SELECT scope_uuid FROM repositories WHERE id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )?
            .ok_or_else(|| refused("missing registered scope"))?;
        let scope = uuid::Uuid::parse_str(&pair.scope_uuid)
            .map_err(|_| refused("invalid registered scope"))?;
        if scope.is_nil() || scope.to_string() != pair.scope_uuid {
            return Err(refused("invalid registered scope"));
        }
        pair.generation = crate::echo_receipt_scope::repo_echo_generation_tx(conn, id)?;
        if pair.generation < 1 {
            return Err(refused("invalid active scope or generation"));
        }
    }
    super::ordinary_writer_windows::require_quiet_for_refresh(conn, id, &pair.scope_uuid)?;
    super::pair_refresh_operations::require_no_pair_refresh_hold(conn, Some(id))?;
    super::pair_refresh_competing_journals::require_globally_quiet(conn)?;
    let uncertain: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM commit_map m WHERE NOT EXISTS(SELECT 1 FROM repositories r WHERE r.id=m.repo_id) OR (repo_id=?1 AND direction!='svn_to_git'))
          OR EXISTS(SELECT 1 FROM sync_records s WHERE NOT EXISTS(SELECT 1 FROM repositories r WHERE r.id=s.repo_id) OR (repo_id=?1 AND
          (direction!='svn_to_git' OR status!='applied' OR svn_rev IS NULL OR git_sha IS NULL)))
          OR EXISTS(SELECT 1 FROM conflicts c WHERE (repo_id=?1 OR NOT EXISTS(SELECT 1 FROM repositories r WHERE r.id=c.repo_id)) AND status NOT IN ('resolved','dismissed'))
          OR EXISTS(SELECT 1 FROM import_progress WHERE (repo_id=?1 OR repo_id IS NULL) AND phase NOT IN ('idle','completed'))",
        [id], |r|r.get(0))?;
    if uncertain {
        return Err(refused(
            "unattributed, incomplete or conflicting published history",
        ));
    }
    let read_maps = |sql| -> Result<Vec<(i64, String)>, DatabaseError> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    };
    pair.mappings =
        read_maps("SELECT svn_rev,git_sha FROM commit_map WHERE repo_id=?1 ORDER BY svn_rev,id")?;
    let records =
        read_maps("SELECT svn_rev,git_sha FROM sync_records WHERE repo_id=?1 ORDER BY svn_rev,id")?;
    if pair.mappings != records
        || pair.mappings.is_empty()
        || pair
            .mappings
            .iter()
            .any(|(rev, sha)| *rev < 1 || !lower_hex(sha, 40))
        || pair.mappings.windows(2).any(|w| w[0].0 >= w[1].0)
        || pair.mappings.last() != Some(&(pair.last_svn_rev, pair.last_git_sha.clone()))
    {
        return Err(refused(
            "both retained mapping tables and the registered terminal watermarks must agree",
        ));
    }
    let mut hash = Sha256::new();
    for sql in [
        "SELECT json_array(id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,enabled,sync_status,created_at,updated_at,last_svn_rev,last_git_sha,parent_id,allowed_paths,blocked_patterns) FROM repositories WHERE id=?1",
        "SELECT json_array(id,svn_rev,git_sha,direction,synced_at,svn_author,git_author) FROM commit_map WHERE repo_id=?1 ORDER BY svn_rev,id",
        "SELECT json_array(id,svn_rev,git_sha,direction,status,author,message,timestamp,synced_at) FROM sync_records WHERE repo_id=?1 ORDER BY svn_rev,id",
        "SELECT json_array(id,repo_id,direction,svn_rev,git_sha,details,created_at,success) FROM audit_log WHERE action='import_full' AND (repo_id=?1 OR repo_id IS NULL) ORDER BY id",
        "SELECT json_array(id,repo_id,phase,current_rev,total_revs,commits_created,batches_pushed,updated_at) FROM import_progress WHERE repo_id=?1 OR repo_id IS NULL ORDER BY id",
    ] {hash_rows(conn,id,sql,&mut hash)?;}
    // Scope migration moves the legacy Git key. Bind its logical presence/value in either spelling.
    for (prefix, expected) in [
        ("last_git_sha_", &pair.last_git_sha),
        ("last_svn_rev_", &pair.last_svn_rev.to_string()),
    ] {
        let mut present = false;
        for token in [id, pair.scope_uuid.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
        {
            let value: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_state WHERE key=?1",
                    [format!("{prefix}{token}")],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(value) = value {
                if &value != expected {
                    return Err(refused(
                        "a retained pair cursor contradicts its registered watermark",
                    ));
                }
                present = true;
            }
        }
        hash.update([u8::from(present)]);
    }
    let sole_publisher = !conn.query_row("SELECT EXISTS(SELECT 1 FROM commit_map WHERE repo_id<>?1)
        OR EXISTS(SELECT 1 FROM sync_records WHERE repo_id<>?1) OR EXISTS(SELECT 1 FROM repositories WHERE id<>?1 AND (last_svn_rev>0 OR last_git_sha!=''))",
        [id],|r|r.get::<_,bool>(0))?;
    (pair.import_completion_svn_rev, pair.import_log_entry_count) =
        import_completion(conn, &pair, sole_publisher)?;
    if sole_publisher {
        for (source, expected) in [
            ("git_sha", &pair.last_git_sha),
            ("svn_rev", &pair.last_svn_rev.to_string()),
        ] {
            let value: Option<String> = conn
                .query_row(
                    "SELECT value FROM watermarks WHERE source=?1",
                    [source],
                    |r| r.get(0),
                )
                .optional()?;
            if value.as_ref().is_some_and(|v| v != expected) {
                return Err(refused(
                    "a legacy import watermark contradicts its only published pair",
                ));
            }
            hash.update([u8::from(value.is_some())]);
        }
    }
    pair.storage_sha256 = hex::encode(hash.finalize());
    Ok(pair)
}

pub fn read_legacy_pair_pins(conn: &Connection, id: &str) -> Result<LegacyPairPins, DatabaseError> {
    if is_installed(conn)? {
        return Err(refused(
            "retrospective enrollment requires an already migrated catalog",
        ));
    }
    stored_pair(conn, id, false)
}

fn receipt(conn: &Connection, id: &str) -> Result<Option<Value>, DatabaseError> {
    let mut stmt = conn.prepare(
        "SELECT details,success FROM audit_log WHERE repo_id=?1 AND action=?2 ORDER BY id LIMIT 2",
    )?;
    let mut rows = stmt.query(params![id, ACTION])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let raw: String = row.get(0)?;
    if row.get::<_, i64>(1)? != 1 || rows.next()?.is_some() {
        return Err(refused("contradictory enrollment audit records"));
    }
    Ok(Some(
        serde_json::from_str(&raw).map_err(|_| refused("malformed enrollment receipt"))?,
    ))
}

// Migration assigns scopes to the whole database. A single-pair cutover must first
// demonstrate that it will not strand a different original published registration.
fn require_single_published_pair(
    conn: &Connection,
    pair: &LegacyPairPins,
) -> Result<(), DatabaseError> {
    let other:Option<String>=conn.query_row(
        "SELECT id FROM repositories AS r WHERE id<>?1 AND (
          last_svn_rev<>0 OR last_git_sha<>'' OR last_sync_at IS NOT NULL OR total_syncs<>0
          OR EXISTS(SELECT 1 FROM commit_map WHERE repo_id=r.id)
          OR EXISTS(SELECT 1 FROM sync_records WHERE repo_id=r.id)
          OR EXISTS(SELECT 1 FROM conflicts WHERE repo_id=r.id)
          OR EXISTS(SELECT 1 FROM audit_log WHERE repo_id=r.id AND (svn_rev IS NOT NULL OR COALESCE(git_sha,'')<>''))
          OR EXISTS(SELECT 1 FROM import_progress WHERE repo_id=r.id AND (phase<>'idle' OR current_rev<>0 OR commits_created<>0 OR batches_pushed<>0))
          OR EXISTS(SELECT 1 FROM kv_state WHERE (key='last_git_sha_'||r.id AND value<>'') OR (key='last_svn_rev_'||r.id AND value NOT IN ('','0'))))
          ORDER BY id LIMIT 1", [&pair.repo_id], |r|r.get(0)).optional()?;
    if let Some(id) = other {
        let label = uuid::Uuid::parse_str(&id)
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "<unrecognized ID>".into());
        return Err(DatabaseError::Other(format!("historical lineage enrollment refused: another registered pair ({label}) has retained history and needs a coordinated cutover")));
    }
    let unexplained:bool=conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM (
           SELECT repo_id FROM commit_map UNION ALL SELECT repo_id FROM sync_records
           UNION ALL SELECT repo_id FROM conflicts
           UNION ALL SELECT repo_id FROM audit_log WHERE repo_id IS NOT NULL AND (svn_rev IS NOT NULL OR COALESCE(git_sha,'')<>'')
           UNION ALL SELECT repo_id FROM import_progress WHERE repo_id IS NOT NULL AND (phase<>'idle' OR current_rev<>0 OR commits_created<>0 OR batches_pushed<>0)
         ) h WHERE NOT EXISTS(SELECT 1 FROM repositories r WHERE r.id=h.repo_id))
        OR EXISTS(SELECT 1 FROM sync_state) OR EXISTS(SELECT 1 FROM pr_sync_log)
        OR EXISTS(SELECT 1 FROM audit_log WHERE repo_id IS NULL AND action<>'import_full' AND (svn_rev IS NOT NULL OR COALESCE(git_sha,'')<>''))
        OR EXISTS(SELECT 1 FROM kv_state WHERE key GLOB 'independently_established_svn_uuid:*' OR key GLOB 'import_operation_*'
            OR (key GLOB 'last_git_sha_*' AND value<>'' AND key<>'last_git_sha_'||?1)
            OR (key GLOB 'last_svn_rev_*' AND value NOT IN ('','0') AND key<>'last_svn_rev_'||?1)
            OR (key IN ('last_git_hash','last_git_sha') AND value NOT IN ('',?2))
            OR (key='last_svn_rev' AND value NOT IN ('','0',CAST(?3 AS TEXT))))
        OR EXISTS(SELECT 1 FROM watermarks WHERE value NOT IN ('','0') AND source NOT IN ('svn_rev','git_sha'))",
        params![pair.repo_id,pair.last_git_sha,pair.last_svn_rev],|r|r.get(0))?;
    if unexplained {
        return Err(refused(
            "unattributed history needs review before migrating this database",
        ));
    }
    Ok(())
}

fn import_completion(
    conn: &Connection,
    pair: &LegacyPairPins,
    include_singleton: bool,
) -> Result<(Option<i64>, Option<i64>), DatabaseError> {
    // Both original daemons emitted these two singleton records without repo_id.
    // Corroborate the one documented full import against this pair's real keyed rows.
    let ambiguous:bool=conn.query_row(
        "WITH progress AS (
           SELECT * FROM import_progress WHERE (repo_id=?1 OR (?4 AND repo_id IS NULL)) AND (phase<>'idle' OR current_rev<>0 OR commits_created<>0 OR batches_pushed<>0)
         ), audit AS (SELECT * FROM audit_log WHERE action='import_full' AND (repo_id=?1 OR (?4 AND repo_id IS NULL)))
         SELECT (SELECT COUNT(*) FROM progress)>1 OR (SELECT COUNT(*) FROM audit)>1
         OR EXISTS(SELECT 1 FROM progress p WHERE p.phase<>'completed' OR p.current_rev<>p.total_revs OR p.total_revs<p.commits_created OR p.commits_created<>?3
            OR julianday(p.started_at) IS NULL OR julianday(p.completed_at) IS NULL OR julianday(p.started_at)>julianday(p.completed_at)
            OR EXISTS(SELECT 1 FROM (SELECT synced_at FROM commit_map WHERE repo_id=?1 UNION ALL SELECT synced_at FROM sync_records WHERE repo_id=?1) m
                 WHERE julianday(m.synced_at) IS NULL OR julianday(m.synced_at) NOT BETWEEN julianday(p.started_at) AND julianday(p.completed_at)))
         OR EXISTS(SELECT 1 FROM audit a WHERE a.action<>'import_full' OR a.success<>1 OR a.direction IS NOT 'svn_to_git'
            OR a.svn_rev IS NULL OR a.svn_rev<?2 OR COALESCE(a.git_sha,'')<>'' OR NOT EXISTS(SELECT 1 FROM progress p
                WHERE julianday(a.created_at) BETWEEN julianday(p.started_at) AND julianday(p.completed_at)
                AND a.details='Full history import: '||p.commits_created||' commits from '||p.total_revs||' revisions'))",
        params![pair.repo_id,pair.last_svn_rev,pair.mappings.len() as i64,include_singleton],|r|r.get(0))?;
    if ambiguous {
        return Err(refused(
            "unassigned older import evidence cannot be correlated to the selected mapped history",
        ));
    }
    let head=conn.query_row("SELECT svn_rev FROM audit_log WHERE action='import_full' AND (repo_id=?1 OR (?2 AND repo_id IS NULL))",
        params![pair.repo_id,include_singleton],|r|r.get(0)).optional()?;
    let count=conn.query_row("SELECT total_revs FROM import_progress WHERE phase='completed' AND (repo_id=?1 OR (?2 AND repo_id IS NULL))",
        params![pair.repo_id,include_singleton],|r|r.get(0)).optional()?;
    Ok((head, count))
}

fn planned_view(mut pins: LegacyPairPins) -> LegacyPairPins {
    pins.scope_uuid.clear();
    pins.generation = 0;
    pins
}

pub fn read_planned_cutover_pins(
    conn: &Connection,
    id: &str,
) -> Result<LegacyPairPins, DatabaseError> {
    if is_installed(conn)? {
        if receipt(conn, id)?.is_some() {
            return Err(refused(
                "unexpected enrollment evidence on the original catalog",
            ));
        }
        let pins = stored_pair(conn, id, true)?;
        require_single_published_pair(conn, &pins)?;
        return Ok(pins);
    }
    let pins = read_legacy_pair_pins(conn, id)?;
    let audit = receipt(conn, id)?
        .ok_or_else(|| refused("planned capture requires the original installed catalog"))?;
    if audit["planned_cutover"] != true
        || audit["scope_uuid"] != pins.scope_uuid
        || audit["generation"] != pins.generation
        || audit["storage_sha256"] != pins.storage_sha256
    {
        return Err(refused(
            "the enrolled original cutover no longer matches this pair",
        ));
    }
    let existing =
        lineage_checkpoints::load_canonical_lineage_tx(conn, &pins.scope_uuid, pins.generation)?;
    let recorded: CanonicalLineage = serde_json::from_value(audit["canonical_lineage"].clone())
        .map_err(|_| refused("missing canonical receipt"))?;
    if existing.as_ref() != Some(&recorded) {
        return Err(refused(
            "the original cutover and its canonical record disagree",
        ));
    }
    Ok(planned_view(pins))
}

struct Reviewed {
    pair: LegacyPairPins,
    canonical: CanonicalLineage,
    digest: String,
    history: String,
    provenance: Value,
    planned: bool,
}

fn verify_review<F>(
    conn: &Connection,
    pins: LegacyPairPins,
    review: &str,
    planned: bool,
    verify: F,
) -> Result<Reviewed, DatabaseError>
where
    F: FnOnce(
        &LegacyPairPins,
        &RegisteredTransport,
    ) -> Result<VerifiedHistoricalLineage, DatabaseError>,
{
    if !lower_hex(review, 64) {
        return Err(refused("expected the complete reviewed evidence digest"));
    }
    let transport = read_registered_transport(conn, &pins)?;
    let proof = verify(&pins, &transport)?;
    if pins
        .import_log_entry_count
        .is_some_and(|n| usize::try_from(n).ok() != Some(proof.verified_branch_revisions()))
    {
        return Err(refused(
            "verified branch revision count differs from the completed historical import",
        ));
    }
    let canonical = proof.lineage();
    if proof.pair() != &pins
        || proof.review_sha256() != review
        || proof.is_planned_cutover() != planned
        || canonical.repo_id != pins.repo_id
        || canonical.scope_uuid != pins.scope_uuid
        || canonical.generation != pins.generation
        || canonical.git_baseline_sha.as_deref() != Some(pins.last_git_sha.as_str())
        || canonical.git_ref.as_deref() != Some(pins.git_branch.as_str())
        || !lower_hex(proof.history_sha256(), 64)
    {
        return Err(refused(
            "verified source, registered pair or historical evidence changed since review",
        ));
    }
    Ok(Reviewed {
        pair: pins,
        canonical: canonical.clone(),
        digest: review.into(),
        history: proof.history_sha256().into(),
        provenance: proof.provenance(),
        planned,
    })
}

fn persist(
    conn: &Connection,
    checked: Reviewed,
    original_installed: bool,
) -> Result<EnrollmentOutcome, DatabaseError> {
    let pins = read_legacy_pair_pins(conn, &checked.pair.repo_id)?;
    let comparable = if checked.planned {
        planned_view(pins.clone())
    } else {
        pins.clone()
    };
    if comparable != checked.pair {
        return Err(refused("registered state changed during enrollment"));
    }
    let mut canonical = checked.canonical;
    canonical.scope_uuid.clone_from(&pins.scope_uuid);
    canonical.generation = pins.generation;
    if lineage_checkpoints::independently_established_svn_uuid(conn, &pins.repo_id)?
        .is_some_and(|id| id != canonical.svn_uuid)
    {
        return Err(refused(
            "the historical witness contradicts an existing identity pin",
        ));
    }
    // The ordinary engine may have persisted its earlier identity hold. Preserve it,
    // but never enroll across a different incarnation or an unresolved operation.
    let source = format!("svn-identity:{}", canonical.svn_uuid);
    for (table, key) in [
        ("canonical_checkpoints", "handled_source_key"),
        ("canonical_mapping_outcomes", "source_key"),
    ] {
        let sql=format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE scope_uuid=?1 AND generation=?2
            AND NOT (direction='svn_to_git' AND {key}=?3 AND outcome='unknown_reconciliation_required'
                     AND reason IS 'svn_identity_unproved' AND target_git_sha IS NULL AND target_svn_rev IS NULL AND policy_snapshot IS NULL))");
        if conn.query_row(
            &sql,
            params![pins.scope_uuid, pins.generation, source],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(refused(
                "canonical checkpoints or outcomes contradict historical enrollment",
            ));
        }
    }
    let audit = json!({"version":1,"scope_uuid":pins.scope_uuid,"generation":pins.generation,
        "storage_sha256":pins.storage_sha256,"review_sha256":checked.digest,"history_sha256":checked.history,
        "planned_cutover":checked.planned,"canonical_lineage":canonical,"provenance":checked.provenance});
    let existing =
        lineage_checkpoints::load_canonical_lineage_tx(conn, &pins.scope_uuid, pins.generation)?;
    match (existing, receipt(conn, &pins.repo_id)?) {
        (Some(row), Some(stored)) if row == canonical && stored == audit => {
            return Ok(EnrollmentOutcome::AlreadyEnrolled)
        }
        (None, None) if !checked.planned || original_installed => {}
        _ => {
            return Err(refused(
                "conflicting or missing canonical enrollment receipt",
            ))
        }
    }
    lineage_checkpoints::pin_canonical_lineage(conn, &canonical)?;
    conn.execute(
        "INSERT INTO audit_log (action,svn_rev,git_sha,details,created_at,success,repo_id)
        VALUES (?1,?2,?3,?4,?5,1,?6)",
        params![
            ACTION,
            pins.last_svn_rev,
            pins.last_git_sha,
            audit.to_string(),
            chrono::Utc::now().to_rfc3339(),
            pins.repo_id
        ],
    )?;
    Ok(EnrollmentOutcome::Enrolled)
}

/// Apply an independent archived witness to an existing upgraded database. Claim,
/// evidence revalidation and enrollment roll back together on every refusal.
pub fn enroll_verified_lineage<F>(
    data_dir: &Path,
    id: &str,
    review_sha256: &str,
    verify: F,
) -> Result<EnrollmentOutcome, DatabaseError>
where
    F: FnOnce(
        &LegacyPairPins,
        &RegisteredTransport,
    ) -> Result<VerifiedHistoricalLineage, DatabaseError>,
{
    let (_owner, outcome) = Database::open_existing_for_historical_enrollment(
        data_dir,
        |tx| {
            let pins = read_legacy_pair_pins(tx, id)?;
            verify_review(tx, pins, review_sha256, false, verify)
        },
        |tx, checked| persist(tx, checked, false),
    )?;
    Ok(outcome)
}

/// Capture and bind a stopped original registration while its installed catalog still
/// exists. The ordinary migration, new scope, canonical row and audit commit together.
pub fn prepare_historical_cutover<F>(
    data_dir: &Path,
    id: &str,
    review_sha256: &str,
    verify: F,
) -> Result<EnrollmentOutcome, DatabaseError>
where
    F: FnOnce(
        &LegacyPairPins,
        &RegisteredTransport,
    ) -> Result<VerifiedHistoricalLineage, DatabaseError>,
{
    let (_owner, outcome) = Database::open_existing_for_historical_enrollment(
        data_dir,
        |tx| {
            let original = is_installed(tx)?;
            let pins = read_planned_cutover_pins(tx, id)?;
            Ok((
                original,
                verify_review(tx, pins, review_sha256, true, verify)?,
            ))
        },
        |tx, (original, verified)| persist(tx, verified, original),
    )?;
    Ok(outcome)
}

#[cfg(test)]
mod tests;
