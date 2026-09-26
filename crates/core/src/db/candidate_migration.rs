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

pub const CANDIDATE_VERSIONS: &[i64] = &[13, 14];
use super::candidate_authority::{insert_baseline, Lineage, V14_SQL};
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
fn rebuild(
    c: &Connection,
    hook: &mut dyn FnMut(i64, &str, &Connection) -> Result<()>,
) -> Result<()> {
    let sequence: Option<i64> = c
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='commit_map'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    c.execute_batch(V13_REBUILD)?;
    hook(13, "after_create", c)?;
    c.execute_batch("INSERT INTO commit_map_v13(id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id) SELECT id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id FROM commit_map;")?;
    hook(13, "after_copy", c)?;
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
    hook(13, "after_replace", c)?;
    Ok(())
}
use rusqlite::OptionalExtension;
fn expected_shape(v: i64) -> Result<Vec<String>> {
    let c = Connection::open_in_memory()?;
    schema::run_migrations(&c)?;
    if v >= 13 {
        rebuild(&c, &mut |_, _, _| Ok(()))?;
    }
    if v >= 14 {
        c.execute_batch(V14_SQL)?;
    }
    shape(&c)
}
fn check_shape(c: &Connection, v: i64) -> Result<()> {
    ensure!(
        [12, 13, 14].contains(&v),
        "unsupported candidate version {v}"
    );
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
fn manifest(root: &Path, allow_sidecars: bool) -> Result<BTreeMap<String, FileSeal>> {
    fn visit(
        root: &Path,
        p: &Path,
        allow_sidecars: bool,
        out: &mut BTreeMap<String, FileSeal>,
    ) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for entry in fs::read_dir(p)? {
            let path = entry?.path();
            let m = fs::symlink_metadata(&path)?;
            ensure!(
                !m.file_type().is_symlink(),
                "symlink in sealed installation"
            );
            if m.is_dir() {
                visit(root, &path, allow_sidecars, out)?;
            } else {
                ensure!(m.is_file(), "special file in sealed installation");
                let rel = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 fixture path")?
                    .to_string();
                if ["reposync.db-wal", "reposync.db-shm", "reposync.db-journal"]
                    .contains(&rel.as_str())
                {
                    ensure!(allow_sidecars, "quiescent source required");
                    continue;
                }
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
    visit(root, root, allow_sidecars, &mut out)?;
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
    qualified: BTreeMap<String, Lineage>,
    dispositions: BTreeMap<String, (String, String)>,
}
impl CopySession {
    pub fn seal(source: &Path, copy: &Path) -> Result<Self> {
        let source = owned_root(source)?;
        let copy = owned_root(copy)?;
        ensure!(
            source != copy && !source.starts_with(&copy) && !copy.starts_with(&source),
            "source/copy overlap"
        );
        let seal = manifest(&source, false)?;
        ensure!(seal.contains_key("reposync.db"), "missing source DB");
        let c = source_db(&source.join("reposync.db"))?;
        check_shape(&c, version(&c)?)?;
        ensure!(version(&c)? == 12, "source must remain original v12");
        let names = tables(&c)?;
        let legacy = Legacy::capture(&c, &names)?;
        let repo_ids = c
            .prepare("SELECT id FROM repositories ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let dispositions = repo_ids
            .into_iter()
            .map(|id| (id, ("not_qualified".into(), "lineage_not_proved".into())))
            .collect();
        let session = Self {
            source,
            copy,
            seal,
            legacy,
            names,
            qualified: BTreeMap::new(),
            dispositions,
        };
        session.check_files()?;
        Ok(session)
    }
    pub fn source_seal(&self) -> &BTreeMap<String, FileSeal> {
        &self.seal
    }
    pub fn source_unchanged(&self) -> Result<()> {
        ensure!(
            manifest(&self.source, false)? == self.seal,
            "source changed"
        );
        Ok(())
    }
    fn check_files(&self) -> Result<()> {
        self.source_unchanged()?;
        let mut actual = manifest(&self.copy, true)?;
        let mut source = self.seal.clone();
        actual.remove("reposync.db");
        source.remove("reposync.db");
        ensure!(actual == source, "copy non-DB bytes/modes differ");
        Ok(())
    }
    pub fn migrate(
        &self,
        target: i64,
        hook: &mut dyn FnMut(i64, &str, &Connection) -> Result<()>,
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
                [13, 14].contains(&target) && starting <= target,
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
                hook(v, "before_transaction", &c)?;
                integrity(&c)?;
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                check_shape(&tx, v - 1)?;
                integrity(&tx)?;
                if v == 13 {
                    rebuild(&tx, hook)?;
                } else {
                    self.convert_v14(&tx, hook)?;
                }
                hook(v, "before_validation", &tx)?;
                ensure!(
                    Legacy::capture(&tx, &self.names)? == self.legacy,
                    "legacy values or sequence changed"
                );
                check_shape(&tx, v)?;
                if v == 14 {
                    self.validate_v14(&tx)?;
                }
                integrity(&tx)?;
                hook(v, "before_version", &tx)?;
                tx.pragma_update(None, "user_version", v)?;
                hook(v, "after_version", &tx)?;
                tx.commit()?;
            }
            check_shape(&c, target)?;
            if target == 14 {
                self.validate_v14(&c)?;
            }
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
                canonical: if target == 14 {
                    self.canonical_rows(&c)?
                } else {
                    BTreeMap::new()
                },
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
    pub canonical: BTreeMap<String, Vec<String>>,
}

impl CopySession {
    /// Read-safe disposition cannot grant authority. Qualified state can only be
    /// produced by the separate disposable-endpoint proof path below.
    pub fn disposition(&mut self, repo: &str, disposition: &str, reason: &str) -> Result<()> {
        ensure!(
            [
                "not_qualified",
                "needs_reconciliation",
                "external_effect_unknown"
            ]
            .contains(&disposition),
            "invalid disposition"
        );
        ensure!(self.dispositions.contains_key(repo), "unknown repository");
        self.qualified.remove(repo);
        self.dispositions
            .insert(repo.into(), (disposition.into(), reason.into()));
        Ok(())
    }
    fn canonical_rows(&self, c: &Connection) -> Result<BTreeMap<String, Vec<String>>> {
        [
            "repo_migration_state",
            "pair_lineages",
            "pair_outcomes",
            "pair_frontiers",
            "legacy_evidence_links",
        ]
        .iter()
        .map(|name| Ok((name.to_string(), rows(c, &format!("SELECT * FROM {name}"))?)))
        .collect()
    }
    fn convert_v14(
        &self,
        c: &Connection,
        hook: &mut dyn FnMut(i64, &str, &Connection) -> Result<()>,
    ) -> Result<()> {
        c.execute_batch(V14_SQL)?;
        hook(14, "after_schema", c)?;
        let seal = hash(serde_json::to_vec(&self.seal)?);
        for (repo, (disposition, reason)) in &self.dispositions {
            c.execute(
                "INSERT INTO repo_migration_state VALUES(?1,?2,?3,?4)",
                rusqlite::params![repo, disposition, reason, seal],
            )?;
            if let Some(l) = self.qualified.get(repo) {
                insert_baseline(c, l)?;
                for table in ["commit_map", "sync_records", "import_progress"] {
                    let keys = c
                        .prepare(&format!(
                            "SELECT CAST(id AS TEXT) FROM {table} WHERE repo_id=?1 ORDER BY id"
                        ))?
                        .query_map([repo], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    for key in keys {
                        c.execute("INSERT INTO legacy_evidence_links VALUES(?1,1,?2,?3,'preserved_legacy_row_not_new_outcome')",rusqlite::params![repo,table,key])?;
                    }
                }
                for key in [
                    format!("last_git_sha_{repo}"),
                    format!("handled_git_baseline_{repo}"),
                    format!("no_target_git_outcomes_{repo}"),
                ] {
                    if c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key=?1)",
                        [&key],
                        |r| r.get::<_, bool>(0),
                    )? {
                        c.execute("INSERT INTO legacy_evidence_links VALUES(?1,1,'kv_state',?2,'preserved_legacy_row_not_new_outcome')",rusqlite::params![repo,key])?;
                    }
                }
            }
            hook(14, "after_repository", c)?;
        }
        Ok(())
    }
    fn validate_v14(&self, c: &Connection) -> Result<()> {
        // Derive expected canonical contents using this same conversion, never a
        // competing test migration. Legacy values remain a separate exact oracle.
        let reference = Connection::open_in_memory()?;
        reference.execute_batch("PRAGMA foreign_keys=ON")?;
        schema::run_migrations(&reference)?;
        rebuild(&reference, &mut |_, _, _| Ok(()))?;
        for (table, values) in &self.legacy.rows {
            let source = source_db(&self.source.join("reposync.db"))?;
            let mut stmt = source.prepare(&format!("SELECT * FROM {}", quote(table)))?;
            let n = stmt.column_count();
            let mut q = stmt.query([])?;
            while let Some(row) = q.next()? {
                let mut vals = Vec::new();
                for i in 0..n {
                    vals.push(row.get::<_, Value>(i)?);
                }
                reference.execute(
                    &format!(
                        "INSERT INTO {} VALUES({})",
                        quote(table),
                        vec!["?"; n].join(",")
                    ),
                    rusqlite::params_from_iter(vals),
                )?;
            }
            ensure!(
                rows(&reference, &format!("SELECT * FROM {}", quote(table)))? == *values,
                "reference copy failure"
            );
        }
        self.convert_v14(&reference, &mut |_, _, _| Ok(()))?;
        let actual = self.canonical_rows(c)?;
        let expected = self.canonical_rows(&reference)?;
        ensure!(actual == expected, "canonical conversion/plan mismatch");
        Ok(())
    }
    /// Qualify the narrow pinned-import topology using read-only commands against
    /// explicitly enrolled disposable file:// endpoints. Inventory labels grant
    /// no authority. Nontrivial copy ancestry/properties/policy remain unqualified.
    pub fn qualify_imported_pair(
        &mut self,
        repo: &str,
        svn_root: &Path,
        git_remote: &Path,
    ) -> Result<Lineage> {
        self.source_unchanged()?;
        ensure!(
            !repo.is_empty()
                && repo.len() <= 128
                && repo
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "unsafe fixture repository ID"
        );
        let original_svn_url = format!("file://{}", svn_root.display());
        let original_git_parent = git_remote
            .parent()
            .context("fixture remote parent")?
            .to_path_buf();
        let svn_root = owned_root(svn_root)?;
        let git_remote = owned_root(git_remote)?;
        let c = source_db(&self.source.join("reposync.db"))?;
        let (url,branch,provider,api,git_repo,git_branch,rev,sha,enabled,paths,blocks):(String,String,String,String,String,String,i64,String,i64,Option<String>,Option<String>)=c.query_row("SELECT svn_url,svn_branch,git_provider,git_api_url,git_repo,git_branch,last_svn_rev,last_git_sha,enabled,allowed_paths,blocked_patterns FROM repositories WHERE id=?1",[repo],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?)))?;
        ensure!(
            enabled == 1 && paths.is_none() && blocks.is_none(),
            "disabled/filtered policy unqualified"
        );
        ensure!(
            (url == original_svn_url || url == format!("file://{}", svn_root.display()))
                && branch == "trunk"
                && provider == "local",
            "fixture SVN identity mismatch or unsupported topology"
        );
        ensure!(
            (api == format!("file://{}", original_git_parent.display())
                || api
                    == format!(
                        "file://{}",
                        git_remote
                            .parent()
                            .context("canonical remote parent")?
                            .display()
                    ))
                && git_remote
                    .file_name()
                    .context("fixture remote name")?
                    .to_str()
                    == Some(&format!("{git_repo}.git")),
            "fixture Git identity mismatch"
        );
        ensure!(
            rev > 0
                && (sha.len() == 40 || sha.len() == 64)
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid baseline"
        );
        let kv: Option<String> = c
            .query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [format!("last_git_sha_{repo}")],
                |r| r.get(0),
            )
            .optional()?;
        ensure!(
            kv.as_deref() == Some(&sha),
            "split/missing directional cursor"
        );
        ensure!(c.query_row("SELECT count(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git' AND svn_rev=?2 AND git_sha=?3 AND status='applied'",rusqlite::params![repo,rev,sha],|r|r.get::<_,i64>(0))?==1,"missing/ambiguous import evidence");
        ensure!(
            rev == 2,
            "only pinned original two-revision source topology qualified"
        );
        let unresolved:i64=c.query_row("SELECT count(*) FROM sync_records WHERE repo_id=?1 AND (status!='applied' OR git_sha IS NULL)",[repo],|r|r.get(0))?;
        ensure!(unresolved == 0, "unknown/pruned/filtered evidence");
        // Candidate receipt versions are not an old import baseline proof. Preserve
        // and disposition these shapes instead of manufacturing historical decisions.
        let receipts: i64 = c.query_row(
            "SELECT count(*) FROM kv_state WHERE key IN (?1,?2)",
            rusqlite::params![
                format!("handled_git_baseline_{repo}"),
                format!("no_target_git_outcomes_{repo}")
            ],
            |r| r.get(0),
        )?;
        ensure!(receipts == 0, "receipt topology requires separate proof");
        let bridge = self.source.join(format!("repos/{repo}/git-repo"));
        ensure!(
            bridge.canonicalize()?.starts_with(&self.source),
            "bridge escape"
        );
        let reference = format!("refs/heads/{git_branch}");
        ensure!(
            command("git", &["check-ref-format", &reference])?.is_empty(),
            "invalid Git ref"
        );
        ensure!(
            command(
                "git",
                &[
                    "-C",
                    bridge.to_str().context("bridge path")?,
                    "rev-parse",
                    &reference
                ]
            )?
            .trim()
                == sha,
            "bridge baseline mismatch"
        );
        ensure!(
            command(
                "git",
                &[
                    "-C",
                    git_remote.to_str().context("remote path")?,
                    "rev-parse",
                    &reference
                ]
            )?
            .trim()
                == sha,
            "remote baseline replaced"
        );

        // The original importer legitimately skips an empty revision when its
        // commit reports no tree delta. Prove retained applied history against
        // actual pinned Git objects, not an assumed record count or a trailer
        // alone. Missing rows for an extant import commit still mean pruning.
        let history = command(
            "git",
            &[
                "-C",
                bridge.to_str().unwrap(),
                "rev-list",
                "--reverse",
                &sha,
            ],
        )?;
        let commits: Vec<_> = history.lines().collect();
        ensure!(
            (2..=3).contains(&commits.len()),
            "unsupported import history"
        );
        let bootstrap = commits[0];
        ensure!(
            command(
                "git",
                &[
                    "-C",
                    bridge.to_str().unwrap(),
                    "show",
                    "-s",
                    "--format=%B",
                    bootstrap
                ]
            )?
            .trim()
                == "Synthetic empty remote root"
                && command(
                    "git",
                    &["-C", bridge.to_str().unwrap(), "ls-tree", "-r", bootstrap]
                )?
                .is_empty(),
            "bootstrap identity/tree unproved"
        );
        let root_parents = command(
            "git",
            &[
                "-C",
                bridge.to_str().unwrap(),
                "rev-list",
                "--parents",
                "-n",
                "1",
                bootstrap,
            ],
        )?;
        ensure!(
            root_parents.split_whitespace().count() == 1,
            "bootstrap not a root"
        );
        let mut imported = Vec::new();
        let mut previous = bootstrap;
        for oid in &commits[1..] {
            let parents = command(
                "git",
                &[
                    "-C",
                    bridge.to_str().unwrap(),
                    "rev-list",
                    "--parents",
                    "-n",
                    "1",
                    oid,
                ],
            )?;
            ensure!(
                parents.split_whitespace().collect::<Vec<_>>() == vec![*oid, previous],
                "nonlinear/replaced import ancestry"
            );
            let message = command(
                "git",
                &[
                    "-C",
                    bridge.to_str().unwrap(),
                    "show",
                    "-s",
                    "--format=%B",
                    oid,
                ],
            )?;
            let imported_rev = if message
                .lines()
                .any(|l| l == "[reposync] imported from SVN r1")
            {
                1
            } else if message
                .lines()
                .any(|l| l == "[reposync] imported from SVN r2")
            {
                2
            } else {
                anyhow::bail!("unknown import commit")
            };
            ensure!(
                imported
                    .last()
                    .map_or(true, |previous_rev| imported_rev > *previous_rev),
                "duplicate/unordered import revision"
            );
            ensure!(c.query_row("SELECT count(*) FROM sync_records WHERE repo_id=?1 AND svn_rev=?2 AND git_sha=?3 AND direction='svn_to_git' AND status='applied'",rusqlite::params![repo,imported_rev,oid],|r|r.get::<_,i64>(0))?==1,"pruned/ambiguous applied history");
            ensure!(c.query_row("SELECT count(*) FROM commit_map WHERE svn_rev=?1 AND git_sha=?2 AND direction='svn_to_git' AND (repo_id IS NULL OR repo_id=?3)",rusqlite::params![imported_rev,oid,repo],|r|r.get::<_,i64>(0))?==1,"pruned/ambiguous legacy mapping history");
            if imported_rev == 1 {
                ensure!(
                    command(
                        "git",
                        &["-C", bridge.to_str().unwrap(), "ls-tree", "-r", oid]
                    )?
                    .is_empty(),
                    "unexpected r1 tree"
                );
            }
            imported.push(imported_rev);
            previous = oid;
        }
        ensure!(
            imported.last() == Some(&2)
                && c.query_row(
                    "SELECT count(*) FROM sync_records WHERE repo_id=?1",
                    [repo],
                    |r| r.get::<_, i64>(0)
                )? == imported.len() as i64,
            "extra/unproved applied history"
        );
        // Confirm r1 is genuinely empty with no SVN property projection. This
        // proves a baseline shape; it creates no historical no-target receipt.
        let empty = tempfile::tempdir()?;
        let empty_export = empty.path().join("r1");
        let r1 = format!("{url}/{branch}@1");
        command(
            "svn",
            &[
                "export",
                "--non-interactive",
                "--no-auth-cache",
                "-r",
                "1",
                &r1,
                empty_export.to_str().context("r1 export path")?,
            ],
        )?;
        ensure!(
            manifest(&empty_export, false)?.is_empty(),
            "r1 projection is not empty"
        );
        ensure!(
            !command(
                "svn",
                &[
                    "proplist",
                    "--non-interactive",
                    "--no-auth-cache",
                    "--xml",
                    "-R",
                    "-r",
                    "1",
                    &r1
                ]
            )?
            .contains("<property "),
            "r1 property projection unproved"
        );
        let pinned = format!("{url}/{branch}@{rev}");
        let rev_str = rev.to_string();
        let uuid = command(
            "svn",
            &[
                "info",
                "--non-interactive",
                "--no-auth-cache",
                "--show-item",
                "repos-uuid",
                "-r",
                &rev_str,
                &pinned,
            ],
        )?
        .trim()
        .to_string();
        ensure!(
            command(
                "svn",
                &[
                    "info",
                    "--non-interactive",
                    "--no-auth-cache",
                    "--show-item",
                    "repos-root-url",
                    "-r",
                    &rev_str,
                    &pinned
                ]
            )?
            .trim()
                == url,
            "SVN root replaced"
        );
        let ancestry = command(
            "svn",
            &[
                "log",
                "--non-interactive",
                "--no-auth-cache",
                "--xml",
                "-v",
                "-r",
                &format!("1:{rev}"),
                &pinned,
            ],
        )?;
        ensure!(
            !ancestry.contains("copyfrom-")
                && ancestry.contains("action=\"A\"")
                && ancestry.contains(">/trunk</path>"),
            "copy/unknown ancestry requires separate proof"
        );
        let props = command(
            "svn",
            &[
                "proplist",
                "--non-interactive",
                "--no-auth-cache",
                "--xml",
                "-R",
                "-r",
                &rev_str,
                &pinned,
            ],
        )?;
        ensure!(
            !props.contains("<property "),
            "SVN properties require separate projection proof"
        );
        let temp = tempfile::tempdir()?;
        let exported = temp.path().join("export");
        command(
            "svn",
            &[
                "export",
                "--non-interactive",
                "--no-auth-cache",
                "-r",
                &rev_str,
                &pinned,
                exported.to_str().context("export path")?,
            ],
        )?;
        let svn_files = manifest(&exported, false)?;
        let tree = command(
            "git",
            &["-C", bridge.to_str().unwrap(), "ls-tree", "-r", &sha],
        )?;
        let mut git_files = BTreeMap::new();
        for line in tree.lines() {
            let (meta, path) = line
                .split_once('\t')
                .context("unsupported Git tree entry")?;
            ensure!(meta.starts_with("100644 blob "), "unsupported Git mode");
            let bytes = command_bytes(
                "git",
                &[
                    "-C",
                    bridge.to_str().unwrap(),
                    "show",
                    &format!("{sha}:{path}"),
                ],
            )?;
            git_files.insert(path.to_string(), hash(bytes));
        }
        let svn_hashes: BTreeMap<_, _> =
            svn_files.into_iter().map(|(k, v)| (k, v.sha256)).collect();
        ensure!(git_files == svn_hashes, "SVN/Git full tree mismatch");
        let tree_hash = hash(serde_json::to_vec(&git_files)?);
        let proof = hash(serde_json::to_vec(
            &serde_json::json!({"source_seal":self.seal,"svn_uuid":uuid,"svn_root":url,"svn_path":branch,"svn_revision":rev,"ancestry_sha256":hash(canonical_svn_log(&ancestry)?),"git_identity":git_remote,"git_ref":reference,"git_sha":sha,"tree":tree_hash}),
        )?);
        let l = Lineage {
            repo_id: repo.into(),
            generation: 1,
            svn_uuid: uuid.clone(),
            svn_root_url: url,
            svn_branch_path: branch.clone(),
            source_svn_uuid: uuid,
            source_svn_path: branch,
            source_svn_rev: 1,
            copy_from_path: None,
            copy_from_rev: None,
            baseline_svn_rev: rev,
            baseline_svn_tree_sha256: tree_hash,
            git_provider: provider,
            git_repo_identity: git_remote.to_string_lossy().into(),
            git_ref: reference,
            baseline_git_sha: sha,
            projection_version: 1,
            projection_json:
                "{\"allowed_paths\":null,\"blocked_patterns\":null,\"regular_files_only\":true}"
                    .into(),
            proof_sha256: proof,
        };
        self.source_unchanged()?;
        self.dispositions.insert(
            repo.into(),
            ("qualified".into(), "pinned_disposable_import_proof".into()),
        );
        self.qualified.insert(repo.into(), l.clone());
        Ok(l)
    }
}
fn command_bytes(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = std::process::Command::new(program)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    // Do not emit stderr or endpoint credentials into public migration errors.
    ensure!(
        output.status.success(),
        "read-only fixture proof command failed: {program}"
    );
    Ok(output.stdout)
}
fn command(program: &str, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(command_bytes(program, args)?)?)
}

#[derive(Debug, PartialEq)]
pub enum MappingLookup {
    Mapped(String),
    ProvedNoTarget(String),
    LegacyUnresolvedNull,
    LegacyOwnerless,
    Missing,
}
/// #54's distinct read semantics, scoped to an explicitly named generation.
/// This does not change legacy operational readers or activate candidate DBs.
pub fn lookup_mapping(
    c: &Connection,
    repo: &str,
    generation: i64,
    rev: i64,
) -> Result<MappingLookup> {
    ensure!(
        c.query_row(
            "SELECT count(*) FROM commit_map WHERE repo_id=?1 AND svn_rev=?2",
            rusqlite::params![repo, rev],
            |r| r.get::<_, i64>(0)
        )? <= 1,
        "ambiguous scoped mapping"
    );
    let mapped: Option<Option<String>> = c
        .query_row(
            "SELECT git_sha FROM commit_map WHERE repo_id=?1 AND svn_rev=?2 ORDER BY id LIMIT 1",
            rusqlite::params![repo, rev],
            |r| r.get(0),
        )
        .optional()?;
    match mapped {
        None => {
            let ownerless: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM commit_map WHERE repo_id IS NULL AND svn_rev=?1)",
                [rev],
                |r| r.get(0),
            )?;
            Ok(if ownerless {
                MappingLookup::LegacyOwnerless
            } else {
                MappingLookup::Missing
            })
        }
        Some(Some(sha)) => Ok(MappingLookup::Mapped(sha)),
        Some(None) => {
            let proof:Option<String>=c.query_row("SELECT o.outcome FROM pair_outcomes o JOIN legacy_evidence_links e ON e.repo_id=o.repo_id AND e.generation=o.generation JOIN commit_map m ON e.legacy_table='commit_map' AND e.legacy_key=CAST(m.id AS TEXT) WHERE m.repo_id=?1 AND m.svn_rev=?2 AND m.git_sha IS NULL AND o.repo_id=?1 AND o.generation=?3 AND o.direction='svn_to_git' AND o.source_svn_rev=?2 AND o.outcome IN ('filtered_no_target','empty_no_target','semantic_no_delta') AND e.interpretation='proved_typed_no_target'",rusqlite::params![repo,rev,generation],|r|r.get(0)).optional()?;
            Ok(proof
                .map(MappingLookup::ProvedNoTarget)
                .unwrap_or(MappingLookup::LegacyUnresolvedNull))
        }
    }
}

// SVN emits XML attributes in hash iteration order. Canonicalize that harmless
// serialization variation while retaining every attribute/value and log byte.
fn canonical_svn_log(xml: &str) -> Result<String> {
    let tags = regex_lite::Regex::new(r"<([A-Za-z][A-Za-z0-9_-]*)\s+([^>]*?)>")?;
    let attributes = regex_lite::Regex::new(r#"([A-Za-z][A-Za-z0-9_-]*)="([^"]*)""#)?;
    let mut out = String::new();
    let mut end = 0;
    for cap in tags.captures_iter(xml) {
        let whole = cap.get(0).context("XML tag")?;
        out.push_str(&xml[end..whole.start()]);
        let raw = cap.get(2).context("XML attributes")?.as_str();
        let mut attrs = BTreeMap::new();
        let mut rest = 0;
        for attr in attributes.captures_iter(raw) {
            let m = attr.get(0).context("attribute")?;
            ensure!(
                raw[rest..m.start()].trim().is_empty(),
                "unsupported SVN log XML"
            );
            ensure!(
                attrs
                    .insert(attr[1].to_string(), attr[2].to_string())
                    .is_none(),
                "duplicate XML attribute"
            );
            rest = m.end();
        }
        ensure!(
            raw[rest..].trim().is_empty(),
            "unsupported SVN log XML suffix"
        );
        out.push('<');
        out.push_str(&cap[1]);
        for (key, value) in attrs {
            out.push_str(&format!(" {key}=\"{value}\""));
        }
        out.push('>');
        end = whole.end();
    }
    out.push_str(&xml[end..]);
    Ok(out)
}
