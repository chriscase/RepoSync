//! Read-only pre-cutover inspection for the two installed Team branches qualified by #313.
use std::path::Path;

use rusqlite::Connection;

use super::{lineage_checkpoints, offline_snapshot, schema};
use crate::errors::DatabaseError;

#[derive(Debug)]
pub struct PairHistory {
    pub id: Option<uuid::Uuid>,
    pub last_svn_revision: i64,
    pub retained: bool,
}

#[derive(Debug)]
pub struct InstalledTeamPreflight {
    pub schema_version: u32,
    pub pairs: Vec<PairHistory>,
    pub unattributed_history: bool,
    pub unexpected_identity_records: bool,
}

impl InstalledTeamPreflight {
    pub fn needs_review(&self) -> bool {
        self.unattributed_history
            || self.unexpected_identity_records
            || self.pairs.iter().any(|pair| pair.retained)
    }
}

pub fn inspect(path: &Path) -> Result<InstalledTeamPreflight, DatabaseError> {
    offline_snapshot::with_snapshot(path, inspect_catalog).map_err(|error| match error {
        // Some SQLite diagnostics quote identifiers from the source catalog.
        DatabaseError::SqliteError(_) => DatabaseError::Other(
            "Unknown: SQLite could not read the expected catalog and records from this snapshot"
                .into(),
        ),
        other => other,
    })
}

fn inspect_catalog(conn: &Connection) -> Result<InstalledTeamPreflight, DatabaseError> {
    let version = conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
    if !matches!(version, 13 | 14) || !schema::installed_team::pending(conn, version)? {
        return Err(DatabaseError::Other(format!(
            "Unknown for this preflight: schema v{version} does not match an installed Team v13/v14 catalog"
        )));
    }
    let check = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))?;
    if check != "ok" {
        return Err(DatabaseError::Other(
            "Unknown: SQLite reports an inconsistent snapshot".into(),
        ));
    }

    let mut query = conn.prepare("SELECT id, last_svn_rev FROM repositories ORDER BY id")?;
    let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut pairs = Vec::new();
    for row in rows {
        let (id, last_svn_revision) = row?;
        // The installed releases also wrote repo-ID keys and import progress. Include those
        // even when an interrupted import did not advance the registration's own checkpoint.
        let other_history: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_state WHERE
                 (key = 'last_git_sha_' || ?1 AND value <> '')
              OR (key = 'last_svn_rev_' || ?1 AND value NOT IN ('', '0')))
             OR EXISTS(SELECT 1 FROM import_progress WHERE repo_id = ?1 AND
                 (phase <> 'idle' OR current_rev <> 0 OR commits_created <> 0 OR batches_pushed <> 0))
             OR EXISTS(SELECT 1 FROM conflicts WHERE repo_id = ?1)
             OR EXISTS(SELECT 1 FROM audit_log WHERE repo_id = ?1 AND (svn_rev IS NOT NULL OR git_sha <> ''))
             OR EXISTS(SELECT 1 FROM repositories WHERE id = ?1 AND
                 (last_sync_at IS NOT NULL OR total_syncs <> 0 OR last_svn_rev < 0))",
            [&id],
            |r| r.get(0),
        )?;
        let retained = other_history || lineage_checkpoints::retained_legacy_baseline(conn, &id)?;
        pairs.push(PairHistory {
            id: uuid::Uuid::parse_str(&id).ok(),
            last_svn_revision,
            retained,
        });
    }

    // Legacy singletons and orphan records cannot be assigned by guessing from the
    // number of current registrations. Inspect their existence without reading payloads.
    let unattributed_history = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM (
             SELECT repo_id FROM commit_map UNION ALL SELECT repo_id FROM sync_records
             UNION ALL SELECT repo_id FROM conflicts
             UNION ALL SELECT repo_id FROM audit_log WHERE svn_rev IS NOT NULL OR git_sha <> ''
             UNION ALL SELECT repo_id FROM import_progress WHERE phase <> 'idle'
                 OR current_rev <> 0 OR commits_created <> 0 OR batches_pushed <> 0
         ) h WHERE NOT EXISTS(SELECT 1 FROM repositories r WHERE r.id = h.repo_id))
         OR EXISTS(SELECT 1 FROM watermarks WHERE value NOT IN ('', '0'))
         OR EXISTS(SELECT 1 FROM sync_state) OR EXISTS(SELECT 1 FROM pr_sync_log)
         OR EXISTS(SELECT 1 FROM kv_state WHERE
              (key IN ('last_git_hash','last_git_sha','last_sync_at') AND value <> '')
           OR (key = 'last_svn_rev' AND value NOT IN ('','0'))
           OR (key GLOB 'last_git_sha_*' AND value <> '' AND NOT EXISTS(
                SELECT 1 FROM repositories r WHERE key = 'last_git_sha_' || r.id))
           OR (key GLOB 'last_svn_rev_*' AND value NOT IN ('','0') AND NOT EXISTS(
                SELECT 1 FROM repositories r WHERE key = 'last_svn_rev_' || r.id)))",
        [],
        |r| r.get(0),
    )?;
    // Neither installed binary wrote the later producer records or snapshot journal.
    // Their presence in this older physical catalog cannot establish how they got there.
    let unexpected_identity_records = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key GLOB 'independently_established_svn_uuid:*'
                     OR key GLOB 'import_operation_v1:*')",
        [],
        |r| r.get(0),
    )?;
    Ok(InstalledTeamPreflight {
        schema_version: version,
        pairs,
        unattributed_history,
        unexpected_identity_records,
    })
}
