//! Explicit, offline candidate migrations. Never registered with normal startup.
//! This qualification API is feature gated and admits only sealed temporary
//! installation copies. Its registry is the future activation candidate, not a
//! second migration algorithm maintained by tests.
use super::schema;
use anyhow::{ensure, Context, Result};
use rusqlite::{types::Value, Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const CANDIDATE_VERSIONS: &[i64] = &[13];
const V13_REBUILD: &str = r#"
CREATE TABLE commit_map_v13 (
 id INTEGER PRIMARY KEY AUTOINCREMENT, svn_rev INTEGER NOT NULL, git_sha TEXT,
 direction TEXT NOT NULL CHECK (direction IN ('svn_to_git', 'git_to_svn')),
 synced_at TEXT NOT NULL, svn_author TEXT NOT NULL DEFAULT '',
 git_author TEXT NOT NULL DEFAULT '', repo_id TEXT
);
"#;
const V13_INDEXES: &str = r#"
CREATE INDEX idx_commit_map_svn_rev ON commit_map(svn_rev);
CREATE INDEX idx_commit_map_git_sha ON commit_map(git_sha);
CREATE INDEX idx_commit_map_repo_svn ON commit_map(repo_id,svn_rev);
CREATE INDEX idx_commit_map_repo_git ON commit_map(repo_id,git_sha);
"#;
fn hash(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}
fn version(c: &Connection) -> Result<i64> {
    Ok(c.pragma_query_value(None, "user_version", |r| r.get(0))?)
}
fn rows(c: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut q = c.prepare(sql)?;
    let n = q.column_count();
    let mut out = q
        .query_map([], |r| {
            let mut values = Vec::new();
            for i in 0..n {
                values.push(r.get::<_, Value>(i)?);
            }
            Ok(format!("{values:?}"))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    out.sort();
    Ok(out)
}
fn tables(c: &Connection) -> Result<Vec<String>> {
    Ok(c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
}
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
#[derive(Clone, Debug, PartialEq)]
struct Legacy {
    rows: BTreeMap<String, Vec<String>>,
    sequences: Vec<String>,
}
impl Legacy {
    fn capture(c: &Connection, names: &[String]) -> Result<Self> {
        let mut out = BTreeMap::new();
        for name in names {
            out.insert(
                name.clone(),
                rows(c, &format!("SELECT * FROM {}", quote(name)))?,
            );
        }
        Ok(Self {
            rows: out,
            sequences: rows(c, "SELECT name,seq FROM sqlite_sequence")?,
        })
    }
    fn public(&self) -> BTreeMap<String, serde_json::Value> {
        self.rows.iter().map(|(name,rows)| {
   // Never export individual credential values or secret-table digests.
   let secret=["users","user_credentials","sessions","encrypted_secrets","kv_state"].contains(&name.as_str());
   (name.clone(),serde_json::json!({"rows":rows.len(),"id_keyed_rows_sha256":if secret {None} else {Some(hash(rows.join("\n")))},"all_values_privately_compared":true}))
  }).collect()
    }
}
fn integrity(c: &Connection) -> Result<()> {
    ensure!(
        c.pragma_query_value::<i64, _>(None, "foreign_keys", |r| r.get(0))? == 1,
        "foreign keys disabled"
    );
    ensure!(
        rows(c, "PRAGMA foreign_key_check")?.is_empty(),
        "foreign key violation"
    );
    ensure!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
        "integrity failure"
    );
    Ok(())
}
fn shape(c: &Connection) -> Result<Vec<String>> {
    rows(
        c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    )
}
fn rebuild(c: &Connection, hook: &mut dyn FnMut(i64, &str) -> Result<()>) -> Result<()> {
    let sequence: Option<i64> = c
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='commit_map'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    c.execute_batch(V13_REBUILD)?;
    hook(13, "after_create")?;
    c.execute_batch("INSERT INTO commit_map_v13(id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id) SELECT id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id FROM commit_map;")?;
    hook(13, "after_copy")?;
    c.execute_batch("DROP TABLE commit_map; ALTER TABLE commit_map_v13 RENAME TO commit_map;")?;
    c.execute_batch(V13_INDEXES)?;
    // Copying explicit IDs only recovers MAX(retained id). Restore the historical
    // sequence, including absent versus empty-but-used, before validation/commit.
    c.execute(
        "DELETE FROM sqlite_sequence WHERE name IN ('commit_map','commit_map_v13')",
        [],
    )?;
    if let Some(seq) = sequence {
        c.execute(
            "INSERT INTO sqlite_sequence(name,seq) VALUES('commit_map',?1)",
            [seq],
        )?;
    }
    hook(13, "after_replace")?;
    Ok(())
}
use rusqlite::OptionalExtension;
fn expected_shape(v: i64) -> Result<Vec<String>> {
    let c = Connection::open_in_memory()?;
    schema::run_migrations(&c)?;
    if v >= 13 {
        rebuild(&c, &mut |_, _| Ok(()))?;
    }
    shape(&c)
}
fn check_shape(c: &Connection, v: i64) -> Result<()> {
    ensure!([12, 13].contains(&v), "unsupported candidate version {v}");
    ensure!(
        shape(c)? == expected_shape(v)?,
        "version {v} physical schema mismatch"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FileSeal {
    pub sha256: String,
    pub mode: u32,
}
fn manifest(root: &Path) -> Result<BTreeMap<String, FileSeal>> {
    fn visit(root: &Path, p: &Path, out: &mut BTreeMap<String, FileSeal>) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for entry in fs::read_dir(p)? {
            let path = entry?.path();
            let m = fs::symlink_metadata(&path)?;
            ensure!(
                !m.file_type().is_symlink(),
                "symlink in sealed installation"
            );
            if m.is_dir() {
                visit(root, &path, out)?;
            } else {
                ensure!(m.is_file(), "special file in sealed installation");
                let rel = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 fixture path")?
                    .to_string();
                ensure!(
                    !["reposync.db-wal", "reposync.db-shm", "reposync.db-journal"]
                        .contains(&rel.as_str()),
                    "quiescent source/copy required"
                );
                out.insert(
                    rel,
                    FileSeal {
                        sha256: hash(fs::read(&path)?),
                        mode: m.permissions().mode(),
                    },
                );
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out)?;
    Ok(out)
}
fn owned_root(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    let temporary = std::env::temp_dir().canonicalize()?;
    ensure!(
        canonical.starts_with(&temporary) && canonical != temporary,
        "only temporary fixture installations admitted"
    );
    // A symlink in an ancestor must not smuggle a non-fixture install into a copy.
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "symlink fixture root"
    );
    let mut p = canonical.clone();
    while p != temporary && p.parent().is_some() {
        ensure!(
            !fs::symlink_metadata(&p)?.file_type().is_symlink(),
            "symlink fixture ancestor"
        );
        p.pop();
    }
    Ok(canonical)
}
fn source_db(path: &Path) -> Result<Connection> {
    ensure!(
        !path
            .to_str()
            .context("UTF8 path")?
            .contains(['?', '#', '%']),
        "unsupported fixture DB path"
    );
    Ok(Connection::open_with_flags(
        format!("file:{}?immutable=1", path.display()),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}
/// Seal an already-quiesced v12 source and its independent copy. No source
/// writer is opened. Repeated calls may resume a structurally valid v13 copy.
pub struct CopySession {
    source: PathBuf,
    copy: PathBuf,
    seal: BTreeMap<String, FileSeal>,
    legacy: Legacy,
    names: Vec<String>,
}
impl CopySession {
    pub fn seal(source: &Path, copy: &Path) -> Result<Self> {
        let source = owned_root(source)?;
        let copy = owned_root(copy)?;
        ensure!(
            source != copy && !source.starts_with(&copy) && !copy.starts_with(&source),
            "source/copy overlap"
        );
        let seal = manifest(&source)?;
        ensure!(seal.contains_key("reposync.db"), "missing source DB");
        let c = source_db(&source.join("reposync.db"))?;
        check_shape(&c, version(&c)?)?;
        ensure!(version(&c)? == 12, "source must remain original v12");
        let names = tables(&c)?;
        let legacy = Legacy::capture(&c, &names)?;
        let session = Self {
            source,
            copy,
            seal,
            legacy,
            names,
        };
        session.check_files()?;
        Ok(session)
    }
    pub fn source_seal(&self) -> &BTreeMap<String, FileSeal> {
        &self.seal
    }
    pub fn source_unchanged(&self) -> Result<()> {
        ensure!(manifest(&self.source)? == self.seal, "source changed");
        Ok(())
    }
    fn check_files(&self) -> Result<()> {
        self.source_unchanged()?;
        let mut actual = manifest(&self.copy)?;
        let mut source = self.seal.clone();
        actual.remove("reposync.db");
        source.remove("reposync.db");
        ensure!(actual == source, "copy non-DB bytes/modes differ");
        Ok(())
    }
    pub fn migrate(
        &self,
        target: i64,
        hook: &mut dyn FnMut(i64, &str) -> Result<()>,
    ) -> Result<MigrationReport> {
        self.check_files()?;
        let result = (|| {
            let mut c = Connection::open_with_flags(
                self.copy.join("reposync.db"),
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=1000;")?;
            let starting = version(&c)?;
            check_shape(&c, starting)?;
            integrity(&c)?;
            ensure!(
                target == 13 && starting <= target,
                "unsupported target/downgrade"
            );
            ensure!(
                Legacy::capture(&c, &self.names)? == self.legacy,
                "copy legacy values/sequences differ from sealed source"
            );
            for &v in CANDIDATE_VERSIONS
                .iter()
                .filter(|&&v| v > starting && v <= target)
            {
                hook(v, "before_transaction")?;
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                check_shape(&tx, v - 1)?;
                integrity(&tx)?;
                rebuild(&tx, hook)?;
                hook(v, "before_validation")?;
                ensure!(
                    Legacy::capture(&tx, &self.names)? == self.legacy,
                    "legacy values or sequence changed"
                );
                check_shape(&tx, v)?;
                integrity(&tx)?;
                hook(v, "before_version")?;
                tx.pragma_update(None, "user_version", v)?;
                hook(v, "after_version")?;
                tx.commit()?;
            }
            check_shape(&c, target)?;
            integrity(&c)?;
            Ok(MigrationReport {
                starting_version: starting,
                final_version: version(&c)?,
                legacy: self.legacy.public(),
                sequence_rows: self.legacy.sequences.clone(),
                fk_check: "empty".into(),
                integrity: "ok".into(),
                source_seal_sha256: hash(serde_json::to_vec(&self.seal)?),
                source_unchanged: true,
            })
        })();
        self.source_unchanged()?;
        self.check_files()?;
        result
    }
}
#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub starting_version: i64,
    pub final_version: i64,
    pub legacy: BTreeMap<String, serde_json::Value>,
    pub sequence_rows: Vec<String>,
    pub fk_check: String,
    pub integrity: String,
    pub source_seal_sha256: String,
    pub source_unchanged: bool,
}
