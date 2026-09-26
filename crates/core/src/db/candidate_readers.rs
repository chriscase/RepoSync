//! Explicit read-only models for migrated fixture copies. No startup/API wiring,
//! implicit generation selection, remote commands, or mutation is provided here.
use super::candidate_migration::{manifest, source_db, CopySession, FileSeal};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, types::Value, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    SvnToGit,
    GitToSvn,
}
impl Direction {
    fn sql(self) -> &'static str {
        match self {
            Self::SvnToGit => "svn_to_git",
            Self::GitToSvn => "git_to_svn",
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Source {
    Svn(i64),
    Git(String),
}
impl Source {
    fn key(&self) -> String {
        match self {
            Self::Svn(r) => format!("svn:{r}"),
            Self::Git(s) => format!("git:{s}"),
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Target {
    Git(String),
    Svn(i64),
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Scope {
    Qualified(i64),
    MissingGeneration(Option<i64>),
    Disabled,
    NotQualified(String),
    MissingRepository,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Raw {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
impl From<Value> for Raw {
    fn from(v: Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Integer(x) => Self::Integer(x),
            Value::Real(x) => Self::Real(x),
            Value::Text(x) => Self::Text(x),
            Value::Blob(x) => Self::Blob(x),
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LegacyRow {
    pub id: i64,
    pub values: Vec<Raw>,
}
impl LegacyRow {
    pub fn repo(&self) -> Option<&str> {
        match &self.values[7] {
            Raw::Text(s) => Some(s),
            _ => None,
        }
    }
    fn source(&self, d: Direction) -> Option<Source> {
        match d {
            Direction::SvnToGit => match &self.values[1] {
                Raw::Integer(r) => Some(Source::Svn(*r)),
                _ => None,
            },
            Direction::GitToSvn => match &self.values[2] {
                Raw::Text(s) => Some(Source::Git(s.clone())),
                _ => None,
            },
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Canonical {
    Mapped { target: Target, authority: String },
    NoTarget { outcome: String, authority: String },
    HandledBaselineWithoutEmittedEffect,
    Unresolved(String),
    Missing,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lookup {
    pub scope: Scope,
    pub canonical: Canonical,
    pub legacy: Vec<LegacyRow>,
}
#[derive(Debug, Serialize)]
pub struct ListItem {
    pub legacy: LegacyRow,
    pub canonical: Canonical,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub scope: Scope,
    pub rows: Vec<ListItem>,
    pub next_after_id: Option<i64>,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct Emitted {
    pub scope: Scope,
    pub target: Option<Target>,
    pub authority: Option<String>,
    pub nonhandled_records: Vec<(String, String)>,
}
#[derive(Debug, Serialize)]
pub struct FrontierStatus {
    pub direction: Direction,
    pub handled: Source,
    pub current_target: Option<Target>,
    pub last_emitted: Emitted,
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub scope: Scope,
    pub requested_generation: Option<i64>,
    pub enabled: Option<bool>,
    pub migration_disposition: Option<String>,
    pub frontiers: Vec<FrontierStatus>,
}
#[derive(Clone)]
struct Outcome {
    id: String,
    source: String,
    predecessor: String,
    kind: String,
    target: Option<Target>,
}
fn resolved(kind: &str) -> bool {
    matches!(
        kind,
        "applied_verified" | "filtered_no_target" | "empty_no_target" | "semantic_no_delta"
    )
}
fn source(key: &str, d: Direction) -> Result<Source> {
    Ok(match d {
        Direction::SvnToGit => Source::Svn(
            key.strip_prefix("svn:")
                .context("invalid incoming source")?
                .parse()?,
        ),
        Direction::GitToSvn => Source::Git(
            key.strip_prefix("git:")
                .context("invalid outgoing source")?
                .into(),
        ),
    })
}
fn legacy(c: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<LegacyRow>> {
    Ok(c.prepare(sql)?
        .query_map(p, |r| {
            let id = r.get(0)?;
            let mut values = Vec::new();
            for i in 0..8 {
                values.push(Raw::from(r.get::<_, Value>(i)?));
            }
            Ok(LegacyRow { id, values })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub struct CopyReaders<'a> {
    session: &'a CopySession,
    c: Connection,
    copy_seal: BTreeMap<String, FileSeal>,
}
impl<'a> CopyReaders<'a> {
    pub(crate) fn open(session: &'a CopySession) -> Result<Self> {
        session.check_files()?;
        // immutable=1 cannot replay journals or create/update a WAL index. A
        // sidecar means the copy is not quiescent and is refused before open.
        let copy_seal = manifest(session.copy_path(), false)?;
        let c = source_db(&session.copy_path().join("reposync.db"))?;
        ensure!(
            c.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))? == 14,
            "typed canonical readers require a complete v14 copy"
        );
        c.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")?;
        session.check_reader_shape(&c)?;
        ensure!(
            c.prepare("PRAGMA foreign_key_check")?
                .query([])?
                .next()?
                .is_none(),
            "invalid canonical ownership"
        );
        Ok(Self {
            session,
            c,
            copy_seal,
        })
    }
    fn unchanged(&self) -> Result<()> {
        self.session.check_files()?;
        ensure!(
            manifest(self.session.copy_path(), false)? == self.copy_seal,
            "copy changed during reader lifetime"
        );
        Ok(())
    }
    fn read<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.unchanged()?;
        let result = f();
        self.unchanged()?;
        result
    }
    fn scope(&self, repo: &str, g: Option<i64>) -> Result<Scope> {
        let enabled: Option<i64> = self
            .c
            .query_row(
                "SELECT enabled FROM repositories WHERE id=?1",
                [repo],
                |r| r.get(0),
            )
            .optional()?;
        let Some(enabled) = enabled else {
            return Ok(Scope::MissingRepository);
        };
        if enabled == 0 {
            return Ok(Scope::Disabled);
        }
        let state: Option<String> = self
            .c
            .query_row(
                "SELECT disposition FROM repo_migration_state WHERE repo_id=?1",
                [repo],
                |r| r.get(0),
            )
            .optional()?;
        if state.as_deref() != Some("qualified") {
            return Ok(Scope::NotQualified(
                state.unwrap_or_else(|| "missing_disposition".into()),
            ));
        }
        let Some(g) = g else {
            return Ok(Scope::MissingGeneration(None));
        };
        let exists: bool = self.c.query_row(
            "SELECT EXISTS(SELECT 1 FROM pair_lineages WHERE repo_id=?1 AND generation=?2)",
            params![repo, g],
            |r| r.get(0),
        )?;
        Ok(if exists {
            Scope::Qualified(g)
        } else {
            Scope::MissingGeneration(Some(g))
        })
    }
    fn baseline(&self, repo: &str, g: i64, d: Direction) -> Result<(String, Option<Target>)> {
        let (rev,sha):(i64,String)=self.c.query_row("SELECT baseline_svn_rev,baseline_git_sha FROM pair_lineages WHERE repo_id=?1 AND generation=?2",params![repo,g],|r|Ok((r.get(0)?,r.get(1)?)))?;
        Ok(match d {
            Direction::SvnToGit => (Source::Svn(rev).key(), Some(Target::Git(sha))),
            Direction::GitToSvn => (Source::Git(sha).key(), None),
        })
    }
    // Follow the explicit handled chain, never timestamps, row IDs, lexical SHA
    // order, MAX(generation), or unrelated resolved/pending outcome rows.
    fn history(&self, repo: &str, g: i64, d: Direction) -> Result<Vec<Outcome>> {
        let (baseline, _) = self.baseline(repo, g, d)?;
        let mut key:String=self.c.query_row("SELECT source_key FROM pair_frontiers WHERE repo_id=?1 AND generation=?2 AND direction=?3",params![repo,g,d.sql()],|r|r.get(0))?;
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        while key != baseline {
            ensure!(
                seen.insert(key.clone()) && seen.len() <= 10000,
                "ambiguous/unbounded handled chain"
            );
            let o=self.c.query_row("SELECT o.id,o.source_key,o.predecessor_source_key,o.outcome,o.target_git_sha,o.target_svn_rev FROM pair_outcomes o JOIN pair_lineages l ON l.repo_id=o.repo_id AND l.generation=o.generation AND l.policy_sha256=o.policy_sha256 AND l.projection_version=o.projection_version WHERE o.repo_id=?1 AND o.generation=?2 AND o.direction=?3 AND o.source_key=?4",params![repo,g,d.sql(),key],|r|{let git:Option<String>=r.get(4)?;let svn:Option<i64>=r.get(5)?;Ok(Outcome{id:r.get(0)?,source:r.get(1)?,predecessor:r.get(2)?,kind:r.get(3)?,target:git.map(Target::Git).or(svn.map(Target::Svn))})})?;
            ensure!(resolved(&o.kind), "handled chain cites unresolved evidence");
            key = o.predecessor.clone();
            out.push(o);
        }
        Ok(out)
    }
    fn lookup_inner(&self, repo: &str, g: Option<i64>, d: Direction, s: &Source) -> Result<Lookup> {
        ensure!(
            matches!(
                (d, s),
                (Direction::SvnToGit, Source::Svn(_)) | (Direction::GitToSvn, Source::Git(_))
            ),
            "source direction mismatch"
        );
        let column = if d == Direction::SvnToGit {
            "svn_rev"
        } else {
            "git_sha"
        };
        let value = match s {
            Source::Svn(r) => Value::Integer(*r),
            Source::Git(x) => Value::Text(x.clone()),
        };
        let raw=legacy(&self.c,&format!("SELECT * FROM commit_map WHERE (repo_id=?1 OR repo_id IS NULL) AND direction=?2 AND {column}=?3 ORDER BY id"),params![repo,d.sql(),value])?;
        let scope = self.scope(repo, g)?;
        let canonical = if let Scope::Qualified(g) = scope {
            let owned: Vec<_> = raw.iter().filter(|r| r.repo() == Some(repo)).collect();
            if owned.len() > 1 {
                Canonical::Unresolved("ambiguous_owned_legacy_rows".into())
            } else {
                let history = self.history(repo, g, d)?;
                let (baseline, baseline_target) = self.baseline(repo, g, d)?;
                let outcome = history.iter().find(|o| o.source == s.key());
                let authority = if let Some(o) = outcome {
                    if o.kind == "applied_verified" {
                        Canonical::Mapped {
                            target: o.target.clone().context("applied target missing")?,
                            authority: o.id.clone(),
                        }
                    } else {
                        Canonical::NoTarget {
                            outcome: o.kind.clone(),
                            authority: o.id.clone(),
                        }
                    }
                } else if s.key() == baseline {
                    match baseline_target {
                        Some(target) => Canonical::Mapped {
                            target,
                            authority: "lineage_baseline".into(),
                        },
                        None => Canonical::HandledBaselineWithoutEmittedEffect,
                    }
                } else {
                    Canonical::Missing
                };
                if let Some(row) = owned.first() {
                    let interpretation:Option<String>=self.c.query_row("SELECT interpretation FROM legacy_evidence_links WHERE repo_id=?1 AND generation=?2 AND legacy_table='commit_map' AND legacy_key=?3",params![repo,g,row.id.to_string()],|r|r.get(0)).optional()?;
                    let matches = match &authority {
                        Canonical::Mapped {
                            target: Target::Git(sha),
                            authority: a,
                        } => {
                            row.values[2] == Raw::Text(sha.clone())
                                && interpretation.as_deref()
                                    == Some(if a == "lineage_baseline" {
                                        "preserved_legacy_row_not_new_outcome"
                                    } else {
                                        "proved_typed_applied"
                                    })
                        }
                        Canonical::Mapped {
                            target: Target::Svn(rev),
                            ..
                        } => {
                            row.values[1] == Raw::Integer(*rev)
                                && interpretation.as_deref() == Some("proved_typed_applied")
                        }
                        Canonical::NoTarget { .. } => {
                            row.values[2] == Raw::Null
                                && interpretation.as_deref() == Some("proved_typed_no_target")
                        }
                        Canonical::HandledBaselineWithoutEmittedEffect => {
                            interpretation.as_deref()
                                == Some("preserved_legacy_row_not_new_outcome")
                        }
                        _ => false,
                    };
                    if matches {
                        authority
                    } else {
                        Canonical::Unresolved("unlinked_or_conflicting_legacy_evidence".into())
                    }
                } else if authority == Canonical::Missing && !raw.is_empty() {
                    Canonical::Unresolved("legacy_ownerless".into())
                } else {
                    authority
                }
            }
        } else {
            Canonical::Unresolved("generation_not_qualified".into())
        };
        Ok(Lookup {
            scope,
            canonical,
            legacy: raw,
        })
    }
    pub fn lookup(&self, repo: &str, g: Option<i64>, d: Direction, s: &Source) -> Result<Lookup> {
        self.read(|| self.lookup_inner(repo, g, d, s))
    }
    /// Every retained row is available for display in deterministic ID order.
    /// None starts at the first stored ID, including explicit zero/negative IDs.
    /// This global display list never provides canonical authority.
    pub fn legacy_page(&self, after: Option<i64>, limit: usize) -> Result<Vec<LegacyRow>> {
        self.read(|| {
            ensure!((1..=200).contains(&limit), "page size out of bounds");
            legacy(
                &self.c,
                "SELECT * FROM commit_map WHERE (?1 IS NULL OR id>?1) ORDER BY id LIMIT ?2",
                params![after, limit as i64],
            )
        })
    }
    pub fn list(
        &self,
        repo: &str,
        g: Option<i64>,
        d: Direction,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Page> {
        self.read(||{
        ensure!((1..=200).contains(&limit),"page size out of bounds");
        let raw=legacy(&self.c,"SELECT * FROM commit_map WHERE (repo_id=?1 OR repo_id IS NULL) AND direction=?2 AND (?3 IS NULL OR id>?3) ORDER BY id LIMIT ?4",params![repo,d.sql(),after,limit as i64])?;
        let next=raw.last().map(|r|r.id);let mut items=Vec::new();
        for row in raw{let canonical=if row.repo()!=Some(repo){Canonical::Unresolved("legacy_ownerless".into())}else if let Some(source)=row.source(d){self.lookup_inner(repo,g,d,&source)?.canonical}else{Canonical::Unresolved("malformed_legacy_source".into())};items.push(ListItem{legacy:row,canonical});}
        Ok(Page{scope:self.scope(repo,g)?,rows:items,next_after_id:next})
    })
    }
    fn emitted_inner(&self, repo: &str, g: Option<i64>, d: Direction) -> Result<Emitted> {
        let scope = self.scope(repo, g)?;
        let mut result = Emitted {
            scope: scope.clone(),
            target: None,
            authority: None,
            nonhandled_records: Vec::new(),
        };
        if let Scope::Qualified(g) = scope {
            let history = self.history(repo, g, d)?;
            if let Some(o) = history.iter().find(|o| o.kind == "applied_verified") {
                result.target = o.target.clone();
                result.authority = Some(o.id.clone());
            } else {
                result.target = self.baseline(repo, g, d)?.1;
                if result.target.is_some() {
                    result.authority = Some("lineage_baseline".into());
                }
            }
            let reachable: BTreeSet<_> = history.iter().map(|o| o.id.as_str()).collect();
            let records=self.c.prepare("SELECT id,outcome FROM pair_outcomes WHERE repo_id=?1 AND generation=?2 AND direction=?3 ORDER BY id")?.query_map(params![repo,g,d.sql()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            result.nonhandled_records = records
                .into_iter()
                .filter(|(id, _)| !reachable.contains(id.as_str()))
                .collect();
        }
        Ok(result)
    }
    pub fn last_emitted(&self, repo: &str, g: Option<i64>, d: Direction) -> Result<Emitted> {
        self.read(|| self.emitted_inner(repo, g, d))
    }
    pub fn status(&self, repo: &str, g: Option<i64>) -> Result<Status> {
        self.read(||{
        let scope=self.scope(repo,g)?;
        let enabled=self.c.query_row("SELECT enabled FROM repositories WHERE id=?1",[repo],|r|Ok(r.get::<_,i64>(0)?!=0)).optional()?;
        let disposition=self.c.query_row("SELECT disposition FROM repo_migration_state WHERE repo_id=?1",[repo],|r|r.get(0)).optional()?;
        let mut frontiers=Vec::new();
        if let Scope::Qualified(g)=scope{for d in [Direction::SvnToGit,Direction::GitToSvn]{let (key,git,svn):(String,Option<String>,Option<i64>)=self.c.query_row("SELECT source_key,emitted_git_sha,emitted_svn_rev FROM pair_frontiers WHERE repo_id=?1 AND generation=?2 AND direction=?3",params![repo,g,d.sql()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;frontiers.push(FrontierStatus{direction:d,handled:source(&key,d)?,current_target:git.map(Target::Git).or(svn.map(Target::Svn)),last_emitted:self.emitted_inner(repo,Some(g),d)?});}}
        Ok(Status{scope,requested_generation:g,enabled,migration_disposition:disposition,frontiers})
    })
    }
}
