//! #53 SVN→Git filtering: a verified projection of genuine SVN revisions.
//!
//! Before Git fetch/reset and conflict detection, the engine prepares one
//! projected file-level plan per fetched SVN revision. Every revision, its
//! identity, metadata and order are kept even when its selected file view is
//! empty. A revision whose selected Git-content effect is genuinely zero is
//! finalized as `intentionally_filtered_no_target` without any Git commit,
//! push, inbound Git cursor movement or applied count. Every other revision
//! feeds only its selected patch to the unchanged apply path.
//!
//! [`VerifiedSvnProjection`] is the only authority for that filtered
//! finalization. It is crate-private, its fields are private, it does not
//! implement `Serialize`/`Deserialize`, and it has no Boolean, scalar or JSON
//! constructor. It is minted only from a current source observation, and it
//! borrows the actual ordinary writer window and database for its whole life.

use rusqlite::{Connection, OptionalExtension};

use crate::db::lineage_checkpoints::{load_canonical_lineage_tx, LiveSvnIdentity};
use crate::db::nullable_commit_map::current_generation_read_only;
use crate::db::ordinary_writer_windows::OrdinaryWriterWindow;
use crate::db::Database;
use crate::errors::DatabaseError;

/// Identity read from one current observation of the database, the writer
/// fence and the live SVN repository. Compared again, field by field, inside
/// the journal transaction that consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedIdentity {
    repo_id: String,
    scope_uuid: String,
    generation: i64,
    writer_epoch: Option<u64>,
    svn_uuid: String,
    svn_root_url: String,
    svn_branch_path: String,
    incarnation_created_rev: i64,
    policy_snapshot: String,
}

/// Source/included/excluded file-endpoint counts for one revision, for
/// structured diagnostics only. They carry no authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ProjectionCounts {
    pub(crate) source: usize,
    pub(crate) included: usize,
    pub(crate) excluded: usize,
}

/// Opaque proof that one genuine SVN revision has zero selected Git-content
/// effect under the captured policy, scope, generation, writer epoch and SVN
/// lineage. Holding it borrows the actual writer window and database.
pub(crate) struct VerifiedSvnProjection<'a, 'db> {
    db: &'a Database,
    window: &'a OrdinaryWriterWindow<'db>,
    identity: CapturedIdentity,
    revision: i64,
    svn_author: String,
    counts: ProjectionCounts,
}

impl std::fmt::Debug for VerifiedSvnProjection<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedSvnProjection")
            .field("repo_id", &self.identity.repo_id)
            .field("revision", &self.revision)
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

fn refused(detail: impl Into<String>) -> DatabaseError {
    DatabaseError::Other(format!(
        "svn filtered projection refused: {}",
        detail.into()
    ))
}

/// Read-only snapshot of the scope identity inside one SQLite transaction.
/// No I/O and no nested database call happen under it.
fn read_scope_snapshot(
    tx: &Connection,
    repo_id: &str,
) -> Result<(String, i64, Option<u64>), DatabaseError> {
    let writer_epoch = crate::writer_fence::ordinary_current_epoch(tx)?;
    let scope: Option<String> = tx
        .query_row(
            "SELECT scope_uuid FROM repositories WHERE id = ?1",
            [repo_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|value| !value.is_empty());
    let Some(scope) = scope else {
        return Err(refused(
            "repository registration or its scope identity is missing",
        ));
    };
    let generation = current_generation_read_only(tx, repo_id, &scope)?;
    Ok((scope, generation, writer_epoch))
}

impl CapturedIdentity {
    /// Capture outside any SQLite reservation, except one read-only snapshot.
    /// `require_engine_scope` runs before and after the snapshot, so the
    /// recorded epoch is the one the actual window was admitted under.
    fn capture(
        db: &Database,
        window: &OrdinaryWriterWindow<'_>,
        repo_id: &str,
        live: &LiveSvnIdentity,
        policy_snapshot: &str,
    ) -> Result<Self, DatabaseError> {
        window.require_engine_scope(db, Some(repo_id))?;
        let (scope_uuid, generation, writer_epoch, lineage) = db.transaction(|tx| {
            let (scope, generation, epoch) = read_scope_snapshot(tx, repo_id)?;
            let lineage = load_canonical_lineage_tx(tx, &scope, generation)?;
            Ok((scope, generation, epoch, lineage))
        })?;
        window.require_engine_scope(db, Some(repo_id))?;
        let Some(lineage) = lineage else {
            return Err(refused(
                "no canonical SVN lineage is pinned for the current scope generation",
            ));
        };
        if lineage.repo_id != repo_id
            || lineage.svn_uuid != live.uuid
            || lineage.svn_root_url != live.root_url
            || lineage.svn_branch_path != live.branch_path
            || lineage.incarnation_created_rev != live.incarnation_created_rev
        {
            return Err(refused(
                "live SVN identity differs from the pinned canonical lineage",
            ));
        }
        Ok(Self {
            repo_id: repo_id.to_owned(),
            scope_uuid,
            generation,
            writer_epoch,
            svn_uuid: live.uuid.clone(),
            svn_root_url: live.root_url.clone(),
            svn_branch_path: live.branch_path.clone(),
            incarnation_created_rev: live.incarnation_created_rev,
            policy_snapshot: policy_snapshot.to_owned(),
        })
    }

    /// Compare every captured field with the current database state inside
    /// the caller's journal transaction.
    fn revalidate_tx(&self, tx: &Connection) -> Result<(), DatabaseError> {
        let (scope, generation, epoch) = read_scope_snapshot(tx, &self.repo_id)?;
        if epoch != self.writer_epoch {
            return Err(refused("writer epoch changed since the projection"));
        }
        if scope != self.scope_uuid {
            return Err(refused("repository scope changed since the projection"));
        }
        if generation != self.generation {
            return Err(refused("scope generation changed since the projection"));
        }
        let lineage = load_canonical_lineage_tx(tx, &scope, generation)?
            .ok_or_else(|| refused("canonical SVN lineage disappeared"))?;
        if lineage.repo_id != self.repo_id
            || lineage.svn_uuid != self.svn_uuid
            || lineage.svn_root_url != self.svn_root_url
            || lineage.svn_branch_path != self.svn_branch_path
            || lineage.incarnation_created_rev != self.incarnation_created_rev
        {
            return Err(refused(
                "canonical SVN lineage changed since the projection",
            ));
        }
        Ok(())
    }
}

impl<'a, 'db> VerifiedSvnProjection<'a, 'db> {
    pub(crate) fn repo_id(&self) -> &str {
        &self.identity.repo_id
    }

    pub(crate) fn revision(&self) -> i64 {
        self.revision
    }

    pub(crate) fn svn_author(&self) -> &str {
        &self.svn_author
    }

    pub(crate) fn policy_snapshot(&self) -> &str {
        &self.identity.policy_snapshot
    }

    /// Scope captured at projection time; `revalidate_tx` proves it current.
    pub(crate) fn scope_uuid(&self) -> &str {
        &self.identity.scope_uuid
    }

    /// Generation captured at projection time; `revalidate_tx` proves it current.
    pub(crate) fn generation(&self) -> i64 {
        self.identity.generation
    }

    pub(crate) fn counts(&self) -> ProjectionCounts {
        self.counts
    }

    /// The actual owner check, outside any SQLite reservation: the journal
    /// database must be the capability's database and the borrowed window
    /// must still own this engine scope.
    pub(crate) fn require_owner(&self, db: &Database) -> Result<(), DatabaseError> {
        if !std::ptr::eq::<Database>(self.db, db) {
            return Err(refused("journal database is not the projection's database"));
        }
        self.window
            .require_engine_scope(db, Some(&self.identity.repo_id))
    }

    /// Revalidate the captured identity inside the journal transaction.
    pub(crate) fn revalidate_tx(&self, tx: &Connection) -> Result<(), DatabaseError> {
        self.identity.revalidate_tx(tx)
    }
}
