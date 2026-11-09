//! #54 operational schema v15: nullable `commit_map.git_sha` and the
//! no-target row contract.
//!
//! The contract is `docs/reliability/contracts/54-nullable-commit-map.md`.
//!
//! The physical rebuild reuses the candidate v13 table body and index DDL
//! byte-for-byte, as the #63 contract requires. The candidate `CopySession`
//! registry itself is neither called nor changed.
//!
//! A NULL `git_sha` row is never authority. It is a legacy projection that is
//! meaningful only when `canonical_mapping_outcomes` already records
//! `intentionally_filtered_no_target` for the same repository scope, current
//! generation, `svn_to_git` direction and SVN source key. Every other NULL row
//! is inert.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::lineage_checkpoints::{
    load_mapping_outcome, svn_source_key, CheckpointDirection, MappingOutcome,
};
use super::personal_scope::PERSONAL_SCOPE_KEY;
use super::repo_scope_identity::{
    legacy_repo_echo_generation_kv_key, repo_echo_generation_kv_key, LEGACY_REPO_ID_KV_READS_KEY,
};
use super::Database;
use crate::errors::DatabaseError;
use crate::pair_refresh::PAIR_GENERATION;

/// Operational registry version that makes `commit_map.git_sha` nullable.
pub(crate) const NULLABLE_COMMIT_MAP_VERSION: u32 = 15;

/// Table body shared, byte-for-byte, with candidate v13 `V13_REBUILD`.
const COMMIT_MAP_V15_TABLE_BODY: &str = " (
 id INTEGER PRIMARY KEY AUTOINCREMENT, svn_rev INTEGER NOT NULL, git_sha TEXT,
 direction TEXT NOT NULL CHECK (direction IN ('svn_to_git', 'git_to_svn')),
 synced_at TEXT NOT NULL, svn_author TEXT NOT NULL DEFAULT '',
 git_author TEXT NOT NULL DEFAULT '', repo_id TEXT
);";

/// Index DDL shared, byte-for-byte, with candidate v13 `V13_INDEXES`.
const COMMIT_MAP_V15_INDEXES: &str = "
CREATE INDEX idx_commit_map_svn_rev ON commit_map(svn_rev);
CREATE INDEX idx_commit_map_git_sha ON commit_map(git_sha);
CREATE INDEX idx_commit_map_repo_svn ON commit_map(repo_id,svn_rev);
CREATE INDEX idx_commit_map_repo_git ON commit_map(repo_id,git_sha);
";

const STAGING_TABLE: &str = "commit_map_v15";

/// Registry index names and their columns. These four always exist on v14.
const REGISTRY_INDEXES: [(&str, &[&str]); 4] = [
    ("idx_commit_map_svn_rev", &["svn_rev"]),
    ("idx_commit_map_git_sha", &["git_sha"]),
    ("idx_commit_map_repo_svn", &["repo_id", "svn_rev"]),
    ("idx_commit_map_repo_git", &["repo_id", "git_sha"]),
];

/// `(name, declared type, notnull, default, pk)` in column order.
type ColumnInfo = (String, String, i64, Option<String>, i64);

fn expected_columns(git_sha_not_null: bool) -> Vec<ColumnInfo> {
    let col = |name: &str, ty: &str, notnull: i64, dflt: Option<&str>, pk: i64| {
        (
            name.to_string(),
            ty.to_string(),
            notnull,
            dflt.map(str::to_string),
            pk,
        )
    };
    vec![
        col("id", "INTEGER", 0, None, 1),
        col("svn_rev", "INTEGER", 1, None, 0),
        col("git_sha", "TEXT", i64::from(git_sha_not_null), None, 0),
        col("direction", "TEXT", 1, None, 0),
        col("synced_at", "TEXT", 1, None, 0),
        col("svn_author", "TEXT", 1, Some("''"), 0),
        col("git_author", "TEXT", 1, Some("''"), 0),
        col("repo_id", "TEXT", 0, None, 0),
    ]
}

fn failed(detail: impl Into<String>) -> DatabaseError {
    DatabaseError::MigrationFailed {
        version: NULLABLE_COMMIT_MAP_VERSION,
        detail: detail.into(),
    }
}

fn columns(conn: &Connection) -> Result<Vec<ColumnInfo>, DatabaseError> {
    let mut stmt = conn.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info('commit_map') ORDER BY cid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Collapse whitespace outside single-quoted literals and drop it next to
/// `(`, `)` and `,`. Case, quoting and literal contents are kept, so any
/// `CHECK`, `COLLATE`, `UNIQUE`, `REFERENCES` or value difference stays
/// significant; only formatting differences between historical binaries are
/// ignored.
fn normalize_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut pending_space = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if pending_space && !out.is_empty() && !out.ends_with(['(', ',']) {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
            while let Some(inner) = chars.next() {
                out.push(inner);
                if inner == '\'' {
                    if chars.peek() == Some(&'\'') {
                        out.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space
            && !out.is_empty()
            && !matches!(ch, '(' | ')' | ',')
            && !out.ends_with(['(', ','])
        {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// Normalized stored SQL of the operational v14 `commit_map` table, built by
/// the registry itself in an empty replica. The registry's commit_map DDL has
/// had a single form (v1 table plus the v6 `repo_id` column) in its history.
fn expected_v14_table_sql() -> &'static str {
    static EXPECTED: OnceLock<String> = OnceLock::new();
    EXPECTED.get_or_init(|| {
        let replica = Connection::open_in_memory().expect("in-memory replica");
        super::schema::run_migrations_up_to(&replica, 14).expect("v14 replica");
        let sql: String = replica
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name='commit_map'",
                [],
                |row| row.get(0),
            )
            .expect("v14 replica commit_map");
        normalize_sql(&sql)
    })
}

/// Reason the indexes on `commit_map` cannot be carried through the rebuild,
/// or `None`. Registry indexes must keep their exact semantics; any index that
/// backs a constraint (no stored SQL to recreate) is refused.
fn unsupported_index_semantics(conn: &Connection) -> Result<Option<String>, DatabaseError> {
    let indexes: Vec<(String, i64, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT name, \"unique\", origin, partial FROM pragma_index_list('commit_map') ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (name, unique, origin, partial) in &indexes {
        if origin != "c" {
            return Ok(Some(format!(
                "index {name} backs a table constraint (origin {origin}) and cannot be rebuilt"
            )));
        }
        if let Some((_, wanted)) = REGISTRY_INDEXES
            .iter()
            .find(|(registry, _)| registry.eq_ignore_ascii_case(name))
        {
            if *unique != 0 || *partial != 0 {
                return Ok(Some(format!(
                    "registry index {name} is unique or partial, unlike operational v14"
                )));
            }
            let keys: Vec<(Option<String>, i64, Option<String>)> = {
                let mut stmt = conn.prepare(
                    "SELECT name, \"desc\", coll FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
                )?;
                let rows = stmt
                    .query_map([name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            let same = keys.len() == wanted.len()
                && keys
                    .iter()
                    .zip(wanted.iter())
                    .all(|((column, desc, coll), want)| {
                        column.as_deref() == Some(*want)
                            && *desc == 0
                            && coll
                                .as_deref()
                                .is_some_and(|c| c.eq_ignore_ascii_case("BINARY"))
                    });
            if !same {
                return Ok(Some(format!(
                    "registry index {name} covers {keys:?}, unlike operational v14"
                )));
            }
        }
    }
    for (registry, _) in REGISTRY_INDEXES {
        if !indexes
            .iter()
            .any(|(name, ..)| name.eq_ignore_ascii_case(registry))
        {
            return Ok(Some(format!("registry index {registry} is missing")));
        }
    }
    Ok(None)
}

/// Stored `sqlite_schema.sql` text for the table and its four registry indexes
/// after the exact v15 rebuild, computed once from an empty replica.
fn expected_v15_schema() -> &'static BTreeMap<String, String> {
    static EXPECTED: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    EXPECTED.get_or_init(|| {
        let replica = Connection::open_in_memory().expect("in-memory replica");
        replica
            .execute_batch(&format!(
                "CREATE TABLE {STAGING_TABLE}{COMMIT_MAP_V15_TABLE_BODY}
                 ALTER TABLE {STAGING_TABLE} RENAME TO commit_map;
                 {COMMIT_MAP_V15_INDEXES}"
            ))
            .expect("v15 replica DDL");
        stored_registry_schema(&replica).expect("v15 replica schema")
    })
}

fn stored_registry_schema(conn: &Connection) -> Result<BTreeMap<String, String>, DatabaseError> {
    let mut out = BTreeMap::new();
    let table: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND lower(name)='commit_map'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(sql) = table {
        out.insert("commit_map".to_string(), sql);
    }
    for (name, _) in REGISTRY_INDEXES {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='index' AND lower(name)=lower(?1) AND lower(tbl_name)='commit_map'",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(sql) = sql {
            out.insert(name.to_string(), sql);
        }
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
enum Shape {
    OperationalV14,
    NullableV15,
    Unsupported(String),
}

fn classify_shape(conn: &Connection) -> Result<Shape, DatabaseError> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND lower(name)='commit_map')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(Shape::Unsupported("commit_map table is missing".into()));
    }
    let actual = columns(conn)?;
    if actual == expected_columns(false) {
        if &stored_registry_schema(conn)? == expected_v15_schema() {
            return Ok(Shape::NullableV15);
        }
        return Ok(Shape::Unsupported(
            "commit_map already allows NULL git_sha but its schema differs from v15".into(),
        ));
    }
    if actual != expected_columns(true) {
        return Ok(Shape::Unsupported(format!(
            "commit_map columns differ from operational v14: {actual:?}"
        )));
    }
    let table_sql: String = conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND lower(name)='commit_map'",
        [],
        |row| row.get(0),
    )?;
    if normalize_sql(&table_sql) != expected_v14_table_sql() {
        return Ok(Shape::Unsupported(
            "commit_map table definition (constraints, collations or clauses) differs from operational v14"
                .into(),
        ));
    }
    let own_foreign_keys: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('commit_map'))",
        [],
        |row| row.get(0),
    )?;
    if own_foreign_keys {
        return Ok(Shape::Unsupported(
            "commit_map declares a foreign key, unlike operational v14".into(),
        ));
    }
    if let Some(reason) = unsupported_index_semantics(conn)? {
        return Ok(Shape::Unsupported(reason));
    }
    Ok(Shape::OperationalV14)
}

/// Views, and triggers on other tables, whose SQL mentions `commit_map`. They
/// are not dropped by the rebuild; their stored SQL must be unchanged after it.
fn external_dependents(conn: &Connection) -> Result<Vec<(String, String, String)>, DatabaseError> {
    let mut stmt = conn.prepare(
        "SELECT type, name, sql FROM sqlite_schema
         WHERE type IN ('view', 'trigger')
           AND lower(tbl_name) <> 'commit_map'
           AND instr(lower(sql), 'commit_map') > 0
         ORDER BY type, name",
    )?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// A foreign key into `commit_map` would turn the DROP into an implicit delete.
fn refuse_foreign_keys(conn: &Connection) -> Result<(), DatabaseError> {
    let tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' AND lower(name) <> 'commit_map' ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for table in tables {
        let refers: bool = conn.query_row(
            // SQLite identifiers are case-insensitive: REFERENCES COMMIT_MAP(id)
            // targets this table too.
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1) WHERE lower(\"table\") = 'commit_map')",
            [&table],
            |row| row.get(0),
        )?;
        if refers {
            return Err(failed(format!(
                "table {table} has a foreign key to commit_map; refusing to rebuild under it"
            )));
        }
    }
    Ok(())
}

/// Row count plus a SHA-256 over every column of every row in `id` order,
/// including the SQLite storage class of each value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowDigest {
    pub(crate) rows: u64,
    pub(crate) sha256: String,
}

pub(crate) fn commit_map_row_digest(conn: &Connection) -> Result<RowDigest, DatabaseError> {
    let mut stmt = conn.prepare(
        "SELECT id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id
         FROM commit_map ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    while let Some(row) = rows.next()? {
        count += 1;
        for index in 0..8 {
            match row.get_ref(index)? {
                ValueRef::Null => hasher.update([0u8]),
                ValueRef::Integer(value) => {
                    hasher.update([1u8]);
                    hasher.update(value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    hasher.update([2u8]);
                    hasher.update(value.to_le_bytes());
                }
                ValueRef::Text(bytes) => {
                    hasher.update([3u8]);
                    hasher.update((bytes.len() as u64).to_le_bytes());
                    hasher.update(bytes);
                }
                ValueRef::Blob(bytes) => {
                    hasher.update([4u8]);
                    hasher.update((bytes.len() as u64).to_le_bytes());
                    hasher.update(bytes);
                }
            }
        }
    }
    Ok(RowDigest {
        rows: count,
        sha256: hex::encode(hasher.finalize()),
    })
}

fn sequences(conn: &Connection) -> Result<BTreeMap<String, i64>, DatabaseError> {
    let has_sequence: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='sqlite_sequence')",
        [],
        |row| row.get(0),
    )?;
    if !has_sequence {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(rows)
}

/// Non-registry indexes and triggers defined on `commit_map`, with their SQL.
fn extra_objects(conn: &Connection) -> Result<Vec<(String, String, String)>, DatabaseError> {
    let registry: Vec<&str> = REGISTRY_INDEXES.iter().map(|(name, _)| *name).collect();
    let mut stmt = conn.prepare(
        "SELECT type, name, sql FROM sqlite_schema
         WHERE lower(tbl_name) = 'commit_map' AND type IN ('index', 'trigger') AND sql IS NOT NULL
         ORDER BY type, name",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(kind, name, _)| {
            !(kind == "index" && registry.iter().any(|r| r.eq_ignore_ascii_case(name)))
        })
        .collect())
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
}

/// Test-only: fail the next v15 rebuild on this thread at `stage`. The stage
/// `abort_after_rename` terminates the whole process mid-transaction.
#[cfg(test)]
pub(crate) fn set_test_fault(stage: Option<&'static str>) {
    FAULT.with(|fault| *fault.borrow_mut() = stage);
}

#[allow(unused_variables)]
fn fault(stage: &'static str) -> Result<(), DatabaseError> {
    #[cfg(test)]
    {
        let hit = FAULT.with(|fault| *fault.borrow() == Some(stage));
        if hit {
            if stage == "abort_after_rename" {
                std::process::abort();
            }
            return Err(failed(format!("injected fault at {stage}")));
        }
    }
    Ok(())
}

/// Rebuild `commit_map` so `git_sha` may be NULL. Runs inside the registry
/// migration transaction; every precondition and verification error leaves
/// that transaction to roll back.
pub(crate) fn migrate_v15(conn: &Connection) -> Result<(), DatabaseError> {
    if conn.is_autocommit() {
        return Err(failed(
            "the commit_map rebuild must run inside the registry migration transaction",
        ));
    }
    match classify_shape(conn)? {
        Shape::NullableV15 => return Ok(()),
        Shape::Unsupported(reason) => return Err(failed(reason)),
        Shape::OperationalV14 => {}
    }
    refuse_foreign_keys(conn)?;

    let before = commit_map_row_digest(conn)?;
    let mut other_sequences = sequences(conn)?;
    let sequence = other_sequences.remove("commit_map");
    let extras = extra_objects(conn)?;
    let dependents = external_dependents(conn)?;
    fault("before_create")?;

    conn.execute_batch(&format!(
        "CREATE TABLE {STAGING_TABLE}{COMMIT_MAP_V15_TABLE_BODY}"
    ))?;
    fault("after_create")?;
    conn.execute_batch(&format!(
        "INSERT INTO {STAGING_TABLE} (id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
         SELECT id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id
         FROM commit_map ORDER BY id;"
    ))?;
    fault("after_copy")?;
    // Modern ALTER TABLE RENAME re-validates every trigger and view against
    // the intermediate schema, where `commit_map` is momentarily absent, so a
    // trigger on another table that queries it would abort the rename. The
    // legacy mode skips that re-check and rewrites nothing; the name being
    // restored is the same, so those objects resolve to the rebuilt table.
    // The previous setting is restored on every path.
    let legacy_alter: bool =
        conn.pragma_query_value(None, "legacy_alter_table", |row| row.get(0))?;
    conn.pragma_update(None, "legacy_alter_table", true)?;
    let replaced = conn.execute_batch(&format!(
        "DROP TABLE commit_map; ALTER TABLE {STAGING_TABLE} RENAME TO commit_map;"
    ));
    let restored = conn.pragma_update(None, "legacy_alter_table", legacy_alter);
    replaced?;
    restored?;
    fault("after_rename")?;
    fault("abort_after_rename")?;
    conn.execute_batch(COMMIT_MAP_V15_INDEXES)?;
    for (_, _, sql) in &extras {
        conn.execute_batch(sql)?;
    }
    // Copying explicit IDs only recovers MAX(retained id). Restore the exact
    // historical sequence, including its absence.
    conn.execute(
        "DELETE FROM sqlite_sequence WHERE name IN ('commit_map', ?1)",
        [STAGING_TABLE],
    )?;
    if let Some(seq) = sequence {
        conn.execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES ('commit_map', ?1)",
            [seq],
        )?;
    }
    fault("before_validation")?;

    if classify_shape(conn)? != Shape::NullableV15 {
        return Err(failed("rebuilt commit_map does not have the v15 schema"));
    }
    if commit_map_row_digest(conn)? != before {
        return Err(failed(
            "commit_map rows or values changed during the rebuild",
        ));
    }
    let mut after_sequences = sequences(conn)?;
    if after_sequences.remove("commit_map") != sequence || after_sequences != other_sequences {
        return Err(failed("sqlite_sequence changed during the rebuild"));
    }
    if extra_objects(conn)? != extras {
        return Err(failed(
            "additional commit_map indexes or triggers were not restored exactly",
        ));
    }
    if external_dependents(conn)? != dependents {
        return Err(failed(
            "a view or trigger that refers to commit_map changed during the rebuild",
        ));
    }
    for (kind, name, _) in &dependents {
        if kind == "view" {
            let quoted = name.replace('"', "\"\"");
            conn.prepare(&format!("SELECT * FROM \"{quoted}\" LIMIT 0"))
                .map_err(|e| failed(format!("view {name} no longer resolves: {e}")))?;
        }
    }
    let integrity: String =
        conn.query_row("PRAGMA integrity_check(commit_map)", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(failed(format!(
            "integrity check after rebuild: {integrity}"
        )));
    }
    let violation: bool = conn
        .prepare("PRAGMA foreign_key_check(commit_map)")?
        .query([])?
        .next()?
        .is_some();
    if violation {
        return Err(failed("foreign key violation after rebuild"));
    }
    fault("after_validation")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// No-target classification (read-only)
// ---------------------------------------------------------------------------

/// What a `commit_map` row says about its Git side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitMapTarget {
    /// A non-empty Git SHA is recorded.
    GitCommit,
    /// NULL SHA corroborated by a canonical `intentionally_filtered_no_target`
    /// outcome for this repository scope and current generation.
    FilteredNoTarget,
    /// NULL SHA without that corroboration. Inert; not "synced".
    NoTargetUnverified,
    /// A historical empty-string SHA. Neither a commit nor a no-target proof.
    LegacyEmptyValue,
}

impl CommitMapTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitCommit => "git_commit",
            Self::FilteredNoTarget => "filtered_no_target",
            Self::NoTargetUnverified => "no_target_unverified",
            Self::LegacyEmptyValue => "legacy_empty_value",
        }
    }
}

/// One `commit_map` row, NULL-capable, with its classified target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMapRow {
    pub id: i64,
    pub svn_rev: i64,
    pub git_sha: Option<String>,
    pub target: CommitMapTarget,
    pub direction: String,
    pub synced_at: String,
    pub svn_author: String,
    pub git_author: String,
    pub repo_id: Option<String>,
}

/// `(id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)`
/// as read from storage, before classification.
type RawCommitMapRow = (
    i64,
    i64,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<String>,
);

fn table_exists(conn: &Connection, name: &str) -> Result<bool, DatabaseError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )?)
}

fn kv_value(conn: &Connection, key: &str) -> Result<Option<String>, DatabaseError> {
    Ok(conn
        .query_row("SELECT value FROM kv_state WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

fn parse_generation(raw: String) -> Result<i64, DatabaseError> {
    raw.parse::<i64>()
        .map_err(|_| DatabaseError::Other("invalid repo echo generation".into()))
}

/// The result `echo_receipt_scope::repo_echo_generation_tx` returns for a
/// repository whose `scope_uuid` already exists, without any of its side
/// effects (it can revoke legacy reads and sweep receipts).
pub(crate) fn current_generation_read_only(
    conn: &Connection,
    repo_id: &str,
    scope_uuid: &str,
) -> Result<i64, DatabaseError> {
    let token = if repo_id == PERSONAL_SCOPE_KEY {
        repo_id
    } else {
        scope_uuid
    };
    if let Some(raw) = kv_value(conn, &repo_echo_generation_kv_key(token))? {
        return parse_generation(raw);
    }
    let repositories: i64 =
        conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
    let legacy_reads = kv_value(conn, LEGACY_REPO_ID_KV_READS_KEY)?;
    if repositories <= 1 && legacy_reads.as_deref() == Some("1") {
        if let Some(raw) = kv_value(conn, &legacy_repo_echo_generation_kv_key(repo_id))? {
            return parse_generation(raw);
        }
    }
    Ok(PAIR_GENERATION)
}

/// True only when a NULL-target row is proven no-target (see module docs).
/// Never writes: it reads the stored `scope_uuid` instead of minting one.
pub(crate) fn no_target_row_proven(
    conn: &Connection,
    repo_id: Option<&str>,
    direction: &str,
    svn_rev: i64,
) -> Result<bool, DatabaseError> {
    if direction != "svn_to_git" {
        return Ok(false);
    }
    let Some(repo_id) = repo_id.filter(|id| !id.is_empty()) else {
        return Ok(false);
    };
    if !table_exists(conn, "canonical_mapping_outcomes")? || !table_exists(conn, "repositories")? {
        return Ok(false);
    }
    let has_scope_column: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('repositories') WHERE name='scope_uuid')",
        [],
        |row| row.get(0),
    )?;
    if !has_scope_column {
        return Ok(false);
    }
    let scope: Option<String> = conn
        .query_row(
            "SELECT scope_uuid FROM repositories WHERE id = ?1",
            [repo_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|value| !value.is_empty());
    let Some(scope) = scope else {
        return Ok(false);
    };
    let generation = current_generation_read_only(conn, repo_id, &scope)?;
    let outcome = load_mapping_outcome(
        conn,
        &scope,
        generation,
        CheckpointDirection::SvnToGit,
        &svn_source_key(svn_rev),
    )?;
    Ok(outcome.is_some_and(|loaded| {
        loaded.outcome == MappingOutcome::IntentionallyFilteredNoTarget
            && loaded.target_git_sha.is_none()
            && loaded.target_svn_rev.is_none()
    }))
}

fn classify_row(
    conn: &Connection,
    git_sha: Option<&str>,
    repo_id: Option<&str>,
    direction: &str,
    svn_rev: i64,
) -> Result<CommitMapTarget, DatabaseError> {
    Ok(match git_sha {
        Some("") => CommitMapTarget::LegacyEmptyValue,
        Some(_) => CommitMapTarget::GitCommit,
        None if no_target_row_proven(conn, repo_id, direction, svn_rev)? => {
            CommitMapTarget::FilteredNoTarget
        }
        None => CommitMapTarget::NoTargetUnverified,
    })
}

/// True when some NULL-target row for `svn_rev`, among the given repository
/// ids (all repositories when `repo_ids` is `None`), is proven no-target.
pub(crate) fn any_proven_no_target_row(
    conn: &Connection,
    svn_rev: i64,
    repo_ids: Option<&[&str]>,
) -> Result<bool, DatabaseError> {
    let candidates: Vec<(Option<String>, String)> = {
        let mut stmt = conn.prepare(
            "SELECT repo_id, direction FROM commit_map
             WHERE svn_rev = ?1 AND git_sha IS NULL ORDER BY id",
        )?;
        let rows = stmt
            .query_map([svn_rev], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (repo_id, direction) in candidates {
        if let Some(allowed) = repo_ids {
            if !repo_id.as_deref().is_some_and(|id| allowed.contains(&id)) {
                continue;
            }
        }
        if no_target_row_proven(conn, repo_id.as_deref(), &direction, svn_rev)? {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Database {
    /// Insert a `commit_map` row whose Git side may be absent.
    ///
    /// `Some(sha)` keeps the established behavior: for a scoped row the
    /// canonical `applied_and_verified` outcome and checkpoint are written in
    /// the same transaction. `Some("")` is refused. `None` is accepted only for
    /// an `svn_to_git` row of a scoped repository whose revision already has a
    /// canonical `intentionally_filtered_no_target` outcome; it writes only
    /// the `commit_map` row.
    pub fn insert_commit_map_with_target(
        &self,
        svn_rev: i64,
        git_sha: Option<&str>,
        direction: &str,
        svn_author: &str,
        git_author: &str,
        repo_id: Option<&str>,
    ) -> Result<i64, DatabaseError> {
        match git_sha {
            Some("") => Err(DatabaseError::Other(
                "commit_map must not store an empty-string Git SHA; use None for a proven no-target row"
                    .into(),
            )),
            Some(sha) => self.insert_commit_map_applied(
                svn_rev, sha, direction, svn_author, git_author, repo_id,
            ),
            None => {
                self.insert_commit_map_no_target(svn_rev, direction, svn_author, git_author, repo_id)
            }
        }
    }

    fn insert_commit_map_no_target(
        &self,
        svn_rev: i64,
        direction: &str,
        svn_author: &str,
        git_author: &str,
        repo_id: Option<&str>,
    ) -> Result<i64, DatabaseError> {
        if direction != "svn_to_git" {
            return Err(DatabaseError::Other(
                "a no-target commit_map row is only valid for svn_to_git".into(),
            ));
        }
        let Some(repo_id) = repo_id.filter(|id| !id.is_empty()) else {
            return Err(DatabaseError::Other(
                "a no-target commit_map row requires a managed repository scope".into(),
            ));
        };
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        if !no_target_row_proven(&tx, Some(repo_id), direction, svn_rev)? {
            return Err(DatabaseError::Other(format!(
                "a no-target commit_map row for r{svn_rev} requires an existing canonical \
                 intentionally_filtered_no_target outcome for this repository scope and generation"
            )));
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6)",
            params![svn_rev, direction, now, svn_author, git_author, repo_id],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        tracing::debug!(
            id,
            svn_rev,
            repo_id,
            "inserted proven no-target commit_map row"
        );
        Ok(id)
    }

    /// Newest rows first, every row, with `git_sha` as `Option` and an explicit
    /// target classification. `repo_id` restricts the listing to one repository.
    pub fn list_commit_map_rows(
        &self,
        repo_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CommitMapRow>, DatabaseError> {
        let conn = self.conn();
        let raw: Vec<RawCommitMapRow> = {
            let map = |row: &rusqlite::Row<'_>| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            };
            match repo_id {
                Some(repo_id) => {
                    let mut stmt = conn.prepare(
                        "SELECT id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id
                         FROM commit_map WHERE repo_id = ?1 ORDER BY id DESC LIMIT ?2",
                    )?;
                    let rows = stmt
                        .query_map(params![repo_id, limit], map)?
                        .collect::<Result<Vec<_>, _>>()?;
                    rows
                }
                None => {
                    let mut stmt = conn.prepare(
                        "SELECT id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id
                         FROM commit_map ORDER BY id DESC LIMIT ?1",
                    )?;
                    let rows = stmt
                        .query_map(params![limit], map)?
                        .collect::<Result<Vec<_>, _>>()?;
                    rows
                }
            }
        };
        raw.into_iter()
            .map(
                |(id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)| {
                    let target = classify_row(
                        &conn,
                        git_sha.as_deref(),
                        repo_id.as_deref(),
                        &direction,
                        svn_rev,
                    )?;
                    Ok(CommitMapRow {
                        id,
                        svn_rev,
                        git_sha,
                        target,
                        direction,
                        synced_at,
                        svn_author,
                        git_author,
                        repo_id,
                    })
                },
            )
            .collect()
    }
}

#[cfg(test)]
#[path = "nullable_commit_map_tests.rs"]
mod tests;
