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

fn refused(reason: &str) -> DatabaseError {
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
    if !installed && version != schema::CURRENT_SCHEMA_VERSION {
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
         AND sync_mode='direct' AND (enabled=0 OR sync_status='error_paused')
         AND sync_status NOT IN ('syncing','importing') AND COALESCE(allowed_paths,'[]')='[]'
         AND COALESCE(blocked_patterns,'[]')='[]'",
        [id], |r| Ok(LegacyPairPins {
            repo_id:r.get(0)?, svn_url:r.get(1)?, svn_branch:r.get(2)?, svn_username:r.get(3)?,
            git_provider:r.get(4)?, git_api_url:r.get(5)?, git_repo:r.get(6)?, git_branch:r.get(7)?,
            last_svn_rev:r.get(8)?, last_git_sha:r.get(9)?, scope_uuid:String::new(),
            generation:0, mappings:vec![], storage_sha256:String::new(),
        })
    ).optional()?.ok_or_else(|| refused("the requested paused full-import root was not found"))?;
    if !installed {
        pair.scope_uuid = super::repo_scope_identity::repository_scope_uuid(conn, id)?;
        pair.generation = crate::echo_receipt_scope::repo_echo_generation_tx(conn, id)?;
        if uuid::Uuid::parse_str(&pair.scope_uuid).is_err() || pair.generation < 1 {
            return Err(refused("invalid active scope or generation"));
        }
    }
    super::ordinary_writer_windows::require_quiet_for_refresh(conn, id, &pair.scope_uuid)?;
    super::pair_refresh_operations::require_no_pair_refresh_hold(conn, Some(id))?;
    super::pair_refresh_competing_journals::require_globally_quiet(conn)?;
    let uncertain: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM commit_map WHERE repo_id IS NULL OR (repo_id=?1 AND direction!='svn_to_git'))
          OR EXISTS(SELECT 1 FROM sync_records WHERE repo_id IS NULL OR (repo_id=?1 AND
          (direction!='svn_to_git' OR status!='applied' OR svn_rev IS NULL OR git_sha IS NULL)))
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
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM commit_map WHERE repo_id<>?1) OR EXISTS(SELECT 1 FROM repositories WHERE id<>?1 AND (last_svn_rev>0 OR last_git_sha!=''))",[id],|r|r.get::<_,bool>(0))? {
        for (source,expected) in [("git_sha",&pair.last_git_sha),("svn_rev",&pair.last_svn_rev.to_string())] {
            let value:Option<String>=conn.query_row("SELECT value FROM watermarks WHERE source=?1",[source],|r|r.get(0)).optional()?;
            if value.as_ref().is_some_and(|v|v!=expected) { return Err(refused("a legacy import watermark contradicts its only published pair")); }
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
        return stored_pair(conn, id, true);
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

/// Claim an existing upgraded database without creating data or running a migration.
pub fn open_historical_enrollment_writer(data_dir: &Path) -> Result<Database, DatabaseError> {
    Database::open_existing_for_historical_enrollment(
        data_dir,
        |tx| {
            if is_installed(tx)? {
                return Err(refused("the registered database still requires migration"));
            }
            Ok(())
        },
        |_, ()| Ok(()),
    )
    .map(|(db, ())| db)
}

/// Retrospective enrollment requires independently verified historical provenance.
pub fn enroll_verified_lineage<F>(
    db: &Database,
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
    db.immediate_transaction(|tx| {
        db.require_historical_owner(tx)?;
        let pins = read_legacy_pair_pins(tx, id)?;
        let verified = verify_review(tx, pins, review_sha256, false, verify)?;
        let outcome = persist(tx, verified, false)?;
        db.require_historical_owner(tx)?;
        Ok(outcome)
    })
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
