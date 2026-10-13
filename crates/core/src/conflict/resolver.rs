//! Conflict resolution actions.
//!
//! The [`ConflictResolver`] provides named operations for resolving a conflict:
//! accept one side, use a custom merge, or defer resolution.

use tracing::{debug, info};

use crate::db::Database;
use crate::errors::ConflictError;

/// Named resolution strategies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Accept the SVN version as-is.
    AcceptSvn,
    /// Accept the Git version as-is.
    AcceptGit,
    /// Accept custom merged content.
    AcceptMerged(String),
    /// Defer resolution to a later time.
    Deferred,
}

/// Stateless conflict resolution operations.
///
/// All methods take the conflict ID and a [`Database`] reference so they can
/// update the conflict record.
pub struct ConflictResolver;

impl ConflictResolver {
    /// Resolve a conflict by accepting the SVN version.
    pub fn accept_svn(
        conflict_id: &str,
        resolved_by: &str,
        db: &Database,
    ) -> Result<(), ConflictError> {
        info!(conflict_id, resolved_by, "resolving conflict: accept SVN");
        Self::apply_resolution(conflict_id, "accept_svn", resolved_by, db)
    }

    /// Resolve a conflict by accepting the Git version.
    pub fn accept_git(
        conflict_id: &str,
        resolved_by: &str,
        db: &Database,
    ) -> Result<(), ConflictError> {
        info!(conflict_id, resolved_by, "resolving conflict: accept Git");
        Self::apply_resolution(conflict_id, "accept_git", resolved_by, db)
    }

    /// Resolve a conflict with custom merged content.
    pub fn accept_merged(
        conflict_id: &str,
        merged_content: &str,
        resolved_by: &str,
        db: &Database,
    ) -> Result<(), ConflictError> {
        info!(
            conflict_id,
            resolved_by, "resolving conflict: accept merged content"
        );
        Self::apply_resolution_with_content(
            conflict_id,
            "accept_merged",
            resolved_by,
            db,
            Some(merged_content),
        )
    }

    /// Defer resolution of a conflict.
    pub fn defer(conflict_id: &str, resolved_by: &str, db: &Database) -> Result<(), ConflictError> {
        info!(conflict_id, resolved_by, "deferring conflict resolution");
        db.resolve_conflict(conflict_id, "deferred", "deferred", resolved_by)
            .map_err(ConflictError::DatabaseError)?;
        debug!(conflict_id, "conflict deferred");
        Ok(())
    }

    /// Apply a resolution to the database.
    ///
    /// This is the internal workhorse that updates the conflict record and
    /// logs the resolution to the audit log.
    pub fn apply_resolution(
        conflict_id: &str,
        resolution: &str,
        resolved_by: &str,
        db: &Database,
    ) -> Result<(), ConflictError> {
        Self::apply_resolution_with_content(conflict_id, resolution, resolved_by, db, None)
    }

    fn apply_resolution_with_content(
        conflict_id: &str,
        resolution: &str,
        resolved_by: &str,
        db: &Database,
        content: Option<&str>,
    ) -> Result<(), ConflictError> {
        // Verify the conflict exists and is not already resolved.
        let conflict = db
            .get_conflict(conflict_id)
            .map_err(ConflictError::DatabaseError)?
            .ok_or_else(|| ConflictError::NotFound(conflict_id.to_string()))?;

        if conflict.status == "resolved" {
            return Err(ConflictError::AlreadyResolved(conflict_id.to_string()));
        }

        // Update the conflict record.
        db.resolve_conflict_with_content(conflict_id, "resolved", resolution, resolved_by, content)
            .map_err(ConflictError::DatabaseError)?;

        // Write an audit log entry.
        let details = format!(
            "Resolved conflict on '{}' with strategy '{}' by '{}'",
            conflict.file_path, resolution, resolved_by
        );
        let _ = db.insert_audit_log(
            "conflict_resolved",
            None,
            conflict.svn_rev,
            conflict.git_sha.as_deref(),
            Some(resolved_by),
            Some(&details),
            true,
        );

        info!(conflict_id, resolution, "conflict resolved");
        Ok(())
    }

    /// Get the content that should be used for a resolved conflict.
    ///
    /// Returns the content corresponding to the chosen resolution strategy.
    pub fn resolved_content(
        resolution: &Resolution,
        svn_content: Option<&str>,
        git_content: Option<&str>,
    ) -> Option<String> {
        match resolution {
            Resolution::AcceptSvn => svn_content.map(|s| s.to_string()),
            Resolution::AcceptGit => git_content.map(|s| s.to_string()),
            Resolution::AcceptMerged(content) => Some(content.clone()),
            Resolution::Deferred => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db_with_conflict() -> (Database, String) {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();

        let id = db
            .insert_conflict_entry(
                "src/main.rs",
                "content",
                Some("svn version"),
                Some("git version"),
                Some("base version"),
                Some(42),
                Some("abc123"),
            )
            .unwrap();

        (db, id)
    }

    #[test]
    fn test_accept_svn() {
        let (db, id) = setup_db_with_conflict();
        ConflictResolver::accept_svn(&id, "admin", &db).unwrap();

        let conflict = db.get_conflict(&id).unwrap().unwrap();
        assert_eq!(conflict.status, "resolved");
        assert_eq!(conflict.resolution.as_deref(), Some("accept_svn"));
    }

    #[test]
    fn test_accept_git() {
        let (db, id) = setup_db_with_conflict();
        ConflictResolver::accept_git(&id, "admin", &db).unwrap();

        let conflict = db.get_conflict(&id).unwrap().unwrap();
        assert_eq!(conflict.status, "resolved");
        assert_eq!(conflict.resolution.as_deref(), Some("accept_git"));
    }

    #[test]
    fn test_accept_merged() {
        let (db, id) = setup_db_with_conflict();
        ConflictResolver::accept_merged(&id, "custom content", "admin", &db).unwrap();

        let conflict = db.get_conflict(&id).unwrap().unwrap();
        assert_eq!(conflict.status, "resolved");
        assert_eq!(conflict.resolution.as_deref(), Some("accept_merged"));
    }

    #[test]
    fn test_defer() {
        let (db, id) = setup_db_with_conflict();
        ConflictResolver::defer(&id, "admin", &db).unwrap();

        let conflict = db.get_conflict(&id).unwrap().unwrap();
        assert_eq!(conflict.status, "deferred");
    }

    #[test]
    fn test_cannot_resolve_twice() {
        let (db, id) = setup_db_with_conflict();
        ConflictResolver::accept_svn(&id, "admin", &db).unwrap();

        let result = ConflictResolver::accept_git(&id, "admin", &db);
        assert!(matches!(result, Err(ConflictError::AlreadyResolved(_))));
    }

    #[test]
    fn test_not_found() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();

        let result = ConflictResolver::accept_svn("nonexistent-id", "admin", &db);
        assert!(matches!(result, Err(ConflictError::NotFound(_))));
    }

    #[test]
    fn test_resolved_content() {
        assert_eq!(
            ConflictResolver::resolved_content(&Resolution::AcceptSvn, Some("svn"), Some("git"),),
            Some("svn".to_string())
        );
        assert_eq!(
            ConflictResolver::resolved_content(&Resolution::AcceptGit, Some("svn"), Some("git"),),
            Some("git".to_string())
        );
        assert_eq!(
            ConflictResolver::resolved_content(
                &Resolution::AcceptMerged("custom".into()),
                Some("svn"),
                Some("git"),
            ),
            Some("custom".to_string())
        );
        assert_eq!(
            ConflictResolver::resolved_content(&Resolution::Deferred, Some("svn"), Some("git"),),
            None
        );
    }
}
