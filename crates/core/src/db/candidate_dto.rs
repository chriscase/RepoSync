//! Versioned offline DTO adapter over CopyReaders; no SQL or operational routes.
//! Missing canonical authority is not absence of pending work or permission to retry.
use super::candidate_readers as model;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "reposync.copy_read.v1";
/// Distinct transport profile: repository-owned legacy rows only. Missing in
/// this profile is not a claim about hidden diagnostics or retry safety.
pub const SCOPED_SCHEMA: &str = "reposync.copy_read.scoped.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    SvnToGit,
    GitToSvn,
}
impl From<model::Direction> for Direction {
    fn from(d: model::Direction) -> Self {
        match d {
            model::Direction::SvnToGit => Self::SvnToGit,
            model::Direction::GitToSvn => Self::GitToSvn,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Identity {
    Svn(i64),
    Git(String),
}
impl From<model::Source> for Identity {
    fn from(s: model::Source) -> Self {
        match s {
            model::Source::Svn(r) => Self::Svn(r),
            model::Source::Git(s) => Self::Git(s),
        }
    }
}
impl From<model::Target> for Identity {
    fn from(t: model::Target) -> Self {
        match t {
            model::Target::Svn(r) => Self::Svn(r),
            model::Target::Git(s) => Self::Git(s),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Qualification {
    Qualified { generation: i64 },
    MissingGeneration { requested_generation: Option<i64> },
    Disabled,
    NotQualified { disposition: String },
    MissingRepository,
}
impl From<model::Scope> for Qualification {
    fn from(s: model::Scope) -> Self {
        match s {
            model::Scope::Qualified(generation) => Self::Qualified { generation },
            model::Scope::MissingGeneration(requested_generation) => Self::MissingGeneration {
                requested_generation,
            },
            model::Scope::Disabled => Self::Disabled,
            model::Scope::NotQualified(disposition) => Self::NotQualified { disposition },
            model::Scope::MissingRepository => Self::MissingRepository,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Canonical {
    Mapped { target: Identity, authority: String },
    NoTarget { outcome: String, authority: String },
    HandledBaselineWithoutEmittedEffect,
    Unresolved { reason: String },
    Missing,
}
impl From<model::Canonical> for Canonical {
    fn from(c: model::Canonical) -> Self {
        match c {
            model::Canonical::Mapped { target, authority } => Self::Mapped {
                target: target.into(),
                authority,
            },
            model::Canonical::NoTarget { outcome, authority } => {
                Self::NoTarget { outcome, authority }
            }
            model::Canonical::HandledBaselineWithoutEmittedEffect => {
                Self::HandledBaselineWithoutEmittedEffect
            }
            model::Canonical::Unresolved(reason) => Self::Unresolved { reason },
            model::Canonical::Missing => Self::Missing,
        }
    }
}
/// All SQLite values survive. Real bits avoid JSON's silent nonfinite -> null
/// coercion; blob bytes use hex. Neither representation grants authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SqlValue {
    Null,
    Integer(i64),
    RealBits(String),
    Text(String),
    BlobHex(String),
}
impl From<model::Raw> for SqlValue {
    fn from(r: model::Raw) -> Self {
        match r {
            model::Raw::Null => Self::Null,
            model::Raw::Integer(x) => Self::Integer(x),
            model::Raw::Real(x) => Self::RealBits(format!("{:016x}", x.to_bits())),
            model::Raw::Text(s) => Self::Text(s),
            model::Raw::Blob(b) => Self::BlobHex(hex::encode(b)),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Legacy {
    pub id: i64,
    pub svn_rev: SqlValue,
    pub git_sha: SqlValue,
    pub direction: SqlValue,
    pub synced_at: SqlValue,
    pub svn_author: SqlValue,
    pub git_author: SqlValue,
    pub repo_id: SqlValue,
}
impl TryFrom<model::LegacyRow> for Legacy {
    type Error = anyhow::Error;
    fn try_from(r: model::LegacyRow) -> Result<Self> {
        ensure!(
            r.values.len() == 8 && r.values[0] == model::Raw::Integer(r.id),
            "unsupported legacy row shape"
        );
        let mut v = r.values.into_iter().skip(1).map(SqlValue::from);
        Ok(Self {
            id: r.id,
            svn_rev: v.next().unwrap(),
            git_sha: v.next().unwrap(),
            direction: v.next().unwrap(),
            synced_at: v.next().unwrap(),
            svn_author: v.next().unwrap(),
            git_author: v.next().unwrap(),
            repo_id: v.next().unwrap(),
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    QualifiedGeneration,
    UnqualifiedScope,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NonhandledRecord {
    pub outcome_id: String,
    pub state: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Nonhandled {
    pub availability: Availability,
    pub records: Vec<NonhandledRecord>,
}
impl Nonhandled {
    fn from_emitted(e: &model::Emitted) -> Self {
        Self {
            availability: availability(&e.scope),
            records: e
                .nonhandled_records
                .iter()
                .map(|(id, state)| NonhandledRecord {
                    outcome_id: id.clone(),
                    state: state.clone(),
                })
                .collect(),
        }
    }
}
fn availability(s: &model::Scope) -> Availability {
    if matches!(s, model::Scope::Qualified(_)) {
        Availability::QualifiedGeneration
    } else {
        Availability::UnqualifiedScope
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Emitted {
    pub scope: Qualification,
    pub target: Option<Identity>,
    pub authority: Option<String>,
    pub nonhandled: Nonhandled,
}
impl From<model::Emitted> for Emitted {
    fn from(e: model::Emitted) -> Self {
        let nonhandled = Nonhandled::from_emitted(&e);
        Self {
            scope: e.scope.into(),
            target: e.target.map(Into::into),
            authority: e.authority,
            nonhandled,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frontier {
    pub direction: Direction,
    pub handled: Identity,
    pub current_target: Option<Identity>,
    pub last_emitted: Emitted,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub legacy: Legacy,
    pub canonical: Canonical,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Request {
    Lookup {
        repository: String,
        generation: Option<i64>,
        direction: Direction,
        source: Identity,
    },
    List {
        repository: String,
        generation: Option<i64>,
        direction: Direction,
        after_id: Option<i64>,
        limit: usize,
    },
    Status {
        repository: String,
        generation: Option<i64>,
    },
    LastEmitted {
        repository: String,
        generation: Option<i64>,
        direction: Direction,
    },
    LegacyPage {
        after_id: Option<i64>,
        limit: usize,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Data {
    Lookup {
        scope: Qualification,
        canonical: Canonical,
        legacy: Vec<Legacy>,
        nonhandled: Nonhandled,
    },
    List {
        scope: Qualification,
        rows: Vec<Item>,
        next_after_id: Option<i64>,
        nonhandled: Nonhandled,
    },
    Status {
        scope: Qualification,
        enabled: Option<bool>,
        migration_disposition: Option<String>,
        nonhandled_availability: Availability,
        frontiers: Vec<Frontier>,
    },
    LastEmitted {
        result: Emitted,
    },
    LegacyPage {
        rows: Vec<Legacy>,
        next_after_id: Option<i64>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub schema: String,
    pub request: Request,
    pub data: Data,
}
impl Response {
    fn new(request: Request, data: Data) -> Self {
        Self {
            schema: SCHEMA.into(),
            request,
            data,
        }
    }
    fn scoped(mut self) -> Self {
        self.schema = SCOPED_SCHEMA.into();
        self
    }
    /// An explicit consumer boundary: never silently accept another version or
    /// deserialize a response for a different operation as this contract.
    pub fn decode(json: &[u8]) -> Result<Self> {
        Self::decode_version(json, SCHEMA)
    }
    pub fn decode_scoped(json: &[u8]) -> Result<Self> {
        Self::decode_version(json, SCOPED_SCHEMA)
    }
    fn decode_version(json: &[u8], schema: &str) -> Result<Self> {
        let r: Self = serde_json::from_slice(json)?;
        ensure!(r.schema == schema, "unsupported copy DTO schema");
        ensure!(
            matches!(
                (&r.request, &r.data),
                (Request::Lookup { .. }, Data::Lookup { .. })
                    | (Request::List { .. }, Data::List { .. })
                    | (Request::Status { .. }, Data::Status { .. })
                    | (Request::LastEmitted { .. }, Data::LastEmitted { .. })
                    | (Request::LegacyPage { .. }, Data::LegacyPage { .. })
            ),
            "copy DTO operation mismatch"
        );
        Ok(r)
    }
}
impl model::CopyReaders<'_> {
    pub fn lookup_dto(
        &self,
        repo: &str,
        g: Option<i64>,
        d: model::Direction,
        s: &model::Source,
    ) -> Result<Response> {
        self.lookup_dto_visibility(repo, g, d, s, false)
    }
    pub fn lookup_scoped_dto(&self, repo: &str, g: Option<i64>, d: model::Direction, s: &model::Source) -> Result<Response> {
        self.lookup_dto_visibility(repo, g, d, s, true)
    }
    fn lookup_dto_visibility(&self, repo: &str, g: Option<i64>, d: model::Direction, s: &model::Source, scoped: bool) -> Result<Response> {
        let m = if scoped { self.lookup_scoped(repo, g, d, s)? } else { self.lookup(repo, g, d, s)? };
        // Separate nonhandled visibility is mandatory even for Canonical::Missing.
        let nonhandled = Nonhandled::from_emitted(&self.last_emitted(repo, g, d)?);
        let response = Response::new(
            Request::Lookup {
                repository: repo.into(),
                generation: g,
                direction: d.into(),
                source: s.clone().into(),
            },
            Data::Lookup {
                scope: m.scope.into(),
                canonical: m.canonical.into(),
                legacy: m
                    .legacy
                    .into_iter()
                    .map(Legacy::try_from)
                    .collect::<Result<_>>()?,
                nonhandled,
            },
        );
        Ok(if scoped { response.scoped() } else { response })
    }
    pub fn list_dto(
        &self,
        repo: &str,
        g: Option<i64>,
        d: model::Direction,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Response> {
        self.list_dto_visibility(repo, g, d, after, limit, false)
    }
    pub fn list_scoped_dto(&self, repo: &str, g: Option<i64>, d: model::Direction, after: Option<i64>, limit: usize) -> Result<Response> {
        self.list_dto_visibility(repo, g, d, after, limit, true)
    }
    fn list_dto_visibility(&self, repo: &str, g: Option<i64>, d: model::Direction, after: Option<i64>, limit: usize, scoped: bool) -> Result<Response> {
        let p = if scoped { self.list_scoped(repo, g, d, after, limit)? } else { self.list(repo, g, d, after, limit)? };
        let nonhandled = Nonhandled::from_emitted(&self.last_emitted(repo, g, d)?);
        let response = Response::new(
            Request::List {
                repository: repo.into(),
                generation: g,
                direction: d.into(),
                after_id: after,
                limit,
            },
            Data::List {
                scope: p.scope.into(),
                rows: p
                    .rows
                    .into_iter()
                    .map(|r| {
                        Ok(Item {
                            legacy: r.legacy.try_into()?,
                            canonical: r.canonical.into(),
                        })
                    })
                    .collect::<Result<_>>()?,
                next_after_id: p.next_after_id,
                nonhandled,
            },
        );
        Ok(if scoped { response.scoped() } else { response })
    }
    pub fn status_dto(&self, repo: &str, g: Option<i64>) -> Result<Response> {
        let s = self.status(repo, g)?;
        let nonhandled_availability = availability(&s.scope);
        Ok(Response::new(
            Request::Status {
                repository: repo.into(),
                generation: g,
            },
            Data::Status {
                scope: s.scope.into(),
                enabled: s.enabled,
                migration_disposition: s.migration_disposition,
                nonhandled_availability,
                frontiers: s
                    .frontiers
                    .into_iter()
                    .map(|f| Frontier {
                        direction: f.direction.into(),
                        handled: f.handled.into(),
                        current_target: f.current_target.map(Into::into),
                        last_emitted: f.last_emitted.into(),
                    })
                    .collect(),
            },
        ))
    }
    pub fn status_scoped_dto(&self, repo: &str, g: Option<i64>) -> Result<Response> {
        self.status_dto(repo, g).map(Response::scoped)
    }
    pub fn last_emitted_dto(
        &self,
        repo: &str,
        g: Option<i64>,
        d: model::Direction,
    ) -> Result<Response> {
        Ok(Response::new(
            Request::LastEmitted {
                repository: repo.into(),
                generation: g,
                direction: d.into(),
            },
            Data::LastEmitted {
                result: self.last_emitted(repo, g, d)?.into(),
            },
        ))
    }
    pub fn last_emitted_scoped_dto(&self, repo: &str, g: Option<i64>, d: model::Direction) -> Result<Response> {
        self.last_emitted_dto(repo, g, d).map(Response::scoped)
    }
    pub fn legacy_page_dto(&self, after: Option<i64>, limit: usize) -> Result<Response> {
        let rows = self.legacy_page(after, limit)?;
        let next = rows.last().map(|r| r.id);
        Ok(Response::new(
            Request::LegacyPage {
                after_id: after,
                limit,
            },
            Data::LegacyPage {
                rows: rows
                    .into_iter()
                    .map(Legacy::try_from)
                    .collect::<Result<_>>()?,
                next_after_id: next,
            },
        ))
    }
}
