//! Conflict detection logic.
//!
//! Given sets of changed files from both SVN and Git, the detector identifies
//! files that have been modified on both sides since the last sync point.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

use crate::path_projection::IncompleteGitRenameError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Categorisation of a conflict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    /// Both sides modified the same file content.
    Content,
    /// One side edited, the other deleted.
    EditDelete,
    /// Both sides renamed the same file differently.
    Rename,
    /// SVN property conflict (no Git equivalent).
    Property,
    /// Branch-level conflict (e.g. divergent branch creation).
    Branch,
    /// Binary file changed on both sides.
    Binary,
}

impl std::fmt::Display for ConflictType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Content => write!(f, "content"),
            Self::EditDelete => write!(f, "edit_delete"),
            Self::Rename => write!(f, "rename"),
            Self::Property => write!(f, "property"),
            Self::Branch => write!(f, "branch"),
            Self::Binary => write!(f, "binary"),
        }
    }
}

/// Lifecycle status of a conflict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStatus {
    /// Just detected, not yet queued.
    Detected,
    /// Queued for resolution (automatic or manual).
    Queued,
    /// Currently being resolved.
    Resolving,
    /// Successfully resolved.
    Resolved,
    /// Deferred for later resolution.
    Deferred,
}

impl std::fmt::Display for ConflictStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Detected => write!(f, "detected"),
            Self::Queued => write!(f, "queued"),
            Self::Resolving => write!(f, "resolving"),
            Self::Resolved => write!(f, "resolved"),
            Self::Deferred => write!(f, "deferred"),
        }
    }
}

/// A detected conflict between SVN and Git changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// Unique conflict ID.
    pub id: String,
    /// The file path where the conflict occurs.
    pub file_path: String,
    /// The type of conflict.
    pub conflict_type: ConflictType,
    /// SVN-side content (text files only).
    pub svn_content: Option<String>,
    /// Git-side content (text files only).
    pub git_content: Option<String>,
    /// Common ancestor (base) content.
    pub base_content: Option<String>,
    /// SVN revision that introduced the SVN-side change.
    pub svn_rev: Option<i64>,
    /// Git SHA that introduced the Git-side change.
    pub git_sha: Option<String>,
    /// Current status.
    pub status: ConflictStatus,
    /// How the conflict was resolved (if resolved).
    pub resolution: Option<String>,
    /// Who resolved it.
    pub resolved_by: Option<String>,
}

impl Conflict {
    /// Create a new conflict with a fresh UUID.
    pub fn new(file_path: impl Into<String>, conflict_type: ConflictType) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            file_path: file_path.into(),
            conflict_type,
            svn_content: None,
            git_content: None,
            base_content: None,
            svn_rev: None,
            git_sha: None,
            status: ConflictStatus::Detected,
            resolution: None,
            resolved_by: None,
        }
    }
}

/// Represents a file change from one side (SVN or Git).
#[derive(Debug, Clone)]
pub struct FileChange {
    /// Relative file path.
    pub path: String,
    /// The kind of change.
    pub change_kind: ChangeKind,
    /// Optional file content (for text files).
    pub content: Option<String>,
    /// Whether this is a binary file.
    pub is_binary: bool,
}

/// Kind of change to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed { from: String },
    PropertyChanged,
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Map a raw Git delta action into a conflict-detector change kind.
pub fn git_action_to_change_kind(
    action: &str,
    path: &str,
    rename_from: Option<&str>,
) -> Result<ChangeKind, IncompleteGitRenameError> {
    match action {
        "A" => Ok(ChangeKind::Added),
        "D" => Ok(ChangeKind::Deleted),
        "M" => Ok(ChangeKind::Modified),
        "R" => {
            let from = rename_from.ok_or_else(|| IncompleteGitRenameError {
                path: path.to_string(),
            })?;
            Ok(ChangeKind::Renamed {
                from: from.to_string(),
            })
        }
        _ => Ok(ChangeKind::Modified),
    }
}

/// Stateless conflict detector that compares two sets of file changes.
pub struct ConflictDetector;

impl ConflictDetector {
    /// Compare SVN and Git change sets and return any detected conflicts.
    ///
    /// Two changes conflict when they affect the same file path and at least
    /// one of them is a content modification.
    pub fn detect(svn_changes: &[FileChange], git_changes: &[FileChange]) -> Vec<Conflict> {
        info!(
            svn_count = svn_changes.len(),
            git_count = git_changes.len(),
            "detecting conflicts"
        );

        let mut conflicts = Vec::new();
        let mut conflict_paths = HashSet::new();

        // Build lookup maps by path.
        let git_by_path: HashMap<&str, &FileChange> =
            git_changes.iter().map(|c| (c.path.as_str(), c)).collect();
        let svn_by_path: HashMap<&str, &FileChange> =
            svn_changes.iter().map(|c| (c.path.as_str(), c)).collect();

        for svn_change in svn_changes {
            if let Some(git_change) = git_by_path.get(svn_change.path.as_str()) {
                let conflict_type = classify_conflict(svn_change, git_change);
                if let Some(ct) = conflict_type {
                    let mut conflict = Conflict::new(&svn_change.path, ct);
                    conflict.svn_content = svn_change.content.clone();
                    conflict.git_content = git_change.content.clone();
                    debug!(
                        path = %conflict.file_path,
                        conflict_type = %conflict.conflict_type,
                        "conflict detected"
                    );
                    conflict_paths.insert(conflict.file_path.clone());
                    conflicts.push(conflict);
                }
            }
        }

        // SVN edits/deletes on a path Git renamed away are edit/delete conflicts.
        for git_change in git_changes {
            if let ChangeKind::Renamed { ref from } = git_change.change_kind {
                if let Some(svn_change) = svn_by_path.get(from.as_str()) {
                    let conflict_type = if matches!(svn_change.change_kind, ChangeKind::Deleted) {
                        // Git rename keeps bytes under a new name; SVN delete is not
                        // "both deleted".
                        Some(ConflictType::EditDelete)
                    } else {
                        let git_deleted_source = FileChange {
                            path: from.clone(),
                            change_kind: ChangeKind::Deleted,
                            content: None,
                            is_binary: git_change.is_binary,
                        };
                        classify_conflict(svn_change, &git_deleted_source)
                    };
                    if let Some(ct) = conflict_type {
                        if conflict_paths.insert(from.clone()) {
                            let mut conflict = Conflict::new(from.as_str(), ct);
                            conflict.svn_content = svn_change.content.clone();
                            conflict.git_content = git_change.content.clone();
                            debug!(
                                path = %conflict.file_path,
                                conflict_type = %conflict.conflict_type,
                                "git rename source conflict detected"
                            );
                            conflicts.push(conflict);
                        }
                    }
                }
            }
        }

        // Also detect rename conflicts: if SVN renamed A -> B and Git renamed A -> C.
        let svn_renames: Vec<(&str, &str)> = svn_changes
            .iter()
            .filter_map(|c| {
                if let ChangeKind::Renamed { ref from } = c.change_kind {
                    Some((from.as_str(), c.path.as_str()))
                } else {
                    None
                }
            })
            .collect();

        let git_renames: std::collections::HashMap<&str, &str> = git_changes
            .iter()
            .filter_map(|c| {
                if let ChangeKind::Renamed { ref from } = c.change_kind {
                    Some((from.as_str(), c.path.as_str()))
                } else {
                    None
                }
            })
            .collect();

        for (svn_from, svn_to) in &svn_renames {
            if let Some(git_to) = git_renames.get(svn_from) {
                if svn_to != git_to && conflict_paths.insert((*svn_from).to_string()) {
                    let conflict = Conflict::new(*svn_from, ConflictType::Rename);
                    debug!(from = svn_from, svn_to, git_to, "rename conflict detected");
                    conflicts.push(conflict);
                }
            }
        }

        info!(count = conflicts.len(), "conflict detection complete");
        conflicts
    }
}

/// Classify what kind of conflict exists between two changes to the same path.
fn classify_conflict(svn: &FileChange, git: &FileChange) -> Option<ConflictType> {
    // Binary conflict.
    if svn.is_binary || git.is_binary {
        return Some(ConflictType::Binary);
    }

    match (&svn.change_kind, &git.change_kind) {
        // Both modified the same file.
        (ChangeKind::Modified, ChangeKind::Modified) => Some(ConflictType::Content),

        // Both added the same path.
        (ChangeKind::Added, ChangeKind::Added) => Some(ConflictType::Content),

        // One side modified/added, other modified/added (cross-combination).
        // e.g. SVN modifies a file that Git independently adds, or vice versa.
        (ChangeKind::Modified, ChangeKind::Added) | (ChangeKind::Added, ChangeKind::Modified) => {
            Some(ConflictType::Content)
        }

        // One side modified, other deleted.
        (ChangeKind::Modified, ChangeKind::Deleted)
        | (ChangeKind::Deleted, ChangeKind::Modified) => Some(ConflictType::EditDelete),

        // One side added, other deleted.
        (ChangeKind::Added, ChangeKind::Deleted) | (ChangeKind::Deleted, ChangeKind::Added) => {
            Some(ConflictType::EditDelete)
        }

        // Git rename at destination: classify like Modified so dest-side SVN
        // add/edit/delete conflicts are not dropped.
        (ChangeKind::Modified, ChangeKind::Renamed { .. })
        | (ChangeKind::Renamed { .. }, ChangeKind::Modified) => Some(ConflictType::Content),
        (ChangeKind::Added, ChangeKind::Renamed { .. })
        | (ChangeKind::Renamed { .. }, ChangeKind::Added) => Some(ConflictType::Content),
        (ChangeKind::Deleted, ChangeKind::Renamed { .. })
        | (ChangeKind::Renamed { .. }, ChangeKind::Deleted) => Some(ConflictType::EditDelete),

        // Property changes.
        (ChangeKind::PropertyChanged, _) | (_, ChangeKind::PropertyChanged) => {
            Some(ConflictType::Property)
        }

        // Both deleted -- no conflict.
        (ChangeKind::Deleted, ChangeKind::Deleted) => None,

        // Other combinations are not conflicts.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, kind: ChangeKind) -> FileChange {
        FileChange {
            path: path.to_string(),
            change_kind: kind,
            content: None,
            is_binary: false,
        }
    }

    #[test]
    fn test_no_conflicts_disjoint() {
        let svn = vec![change("a.rs", ChangeKind::Modified)];
        let git = vec![change("b.rs", ChangeKind::Modified)];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_content_conflict() {
        let svn = vec![change("main.rs", ChangeKind::Modified)];
        let git = vec![change("main.rs", ChangeKind::Modified)];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].conflict_type, ConflictType::Content);
    }

    #[test]
    fn test_edit_delete_conflict() {
        let svn = vec![change("file.rs", ChangeKind::Modified)];
        let git = vec![change("file.rs", ChangeKind::Deleted)];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].conflict_type, ConflictType::EditDelete);
    }

    #[test]
    fn test_both_deleted_no_conflict() {
        let svn = vec![change("file.rs", ChangeKind::Deleted)];
        let git = vec![change("file.rs", ChangeKind::Deleted)];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_binary_conflict() {
        let svn = vec![FileChange {
            path: "image.png".to_string(),
            change_kind: ChangeKind::Modified,
            content: None,
            is_binary: true,
        }];
        let git = vec![change("image.png", ChangeKind::Modified)];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].conflict_type, ConflictType::Binary);
    }

    #[test]
    fn test_rename_conflict() {
        let svn = vec![FileChange {
            path: "new_a.rs".to_string(),
            change_kind: ChangeKind::Renamed {
                from: "old.rs".to_string(),
            },
            content: None,
            is_binary: false,
        }];
        let git = vec![FileChange {
            path: "new_b.rs".to_string(),
            change_kind: ChangeKind::Renamed {
                from: "old.rs".to_string(),
            },
            content: None,
            is_binary: false,
        }];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].conflict_type, ConflictType::Rename);
    }

    #[test]
    fn test_git_rename_missing_source_path_fails_closed() {
        let err = git_action_to_change_kind("R", "new.txt", None).unwrap_err();
        assert_eq!(err.path, "new.txt");
    }

    #[test]
    fn test_svn_edit_on_git_rename_source_is_edit_delete_conflict() {
        let svn = vec![change("old.txt", ChangeKind::Modified)];
        let git = vec![FileChange {
            path: "new.txt".to_string(),
            change_kind: ChangeKind::Renamed {
                from: "old.txt".to_string(),
            },
            content: None,
            is_binary: false,
        }];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "old.txt");
        assert_eq!(conflicts[0].conflict_type, ConflictType::EditDelete);
    }

    #[test]
    fn test_svn_delete_on_git_rename_source_is_edit_delete_conflict() {
        let svn = vec![change("old.txt", ChangeKind::Deleted)];
        let git = vec![FileChange {
            path: "new.txt".to_string(),
            change_kind: ChangeKind::Renamed {
                from: "old.txt".to_string(),
            },
            content: None,
            is_binary: false,
        }];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "old.txt");
        assert_eq!(conflicts[0].conflict_type, ConflictType::EditDelete);
    }

    fn git_rename(from: &str, to: &str) -> FileChange {
        FileChange {
            path: to.to_string(),
            change_kind: ChangeKind::Renamed {
                from: from.to_string(),
            },
            content: None,
            is_binary: false,
        }
    }

    #[test]
    fn test_svn_add_on_git_rename_destination_is_content_conflict() {
        let svn = vec![change("new.txt", ChangeKind::Added)];
        let git = vec![git_rename("old.txt", "new.txt")];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "new.txt");
        assert_eq!(conflicts[0].conflict_type, ConflictType::Content);
    }

    #[test]
    fn test_svn_edit_on_git_rename_destination_is_content_conflict() {
        let svn = vec![change("new.txt", ChangeKind::Modified)];
        let git = vec![git_rename("old.txt", "new.txt")];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "new.txt");
        assert_eq!(conflicts[0].conflict_type, ConflictType::Content);
    }

    #[test]
    fn test_svn_delete_on_git_rename_destination_is_edit_delete_conflict() {
        let svn = vec![change("new.txt", ChangeKind::Deleted)];
        let git = vec![git_rename("old.txt", "new.txt")];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "new.txt");
        assert_eq!(conflicts[0].conflict_type, ConflictType::EditDelete);
    }

    #[test]
    fn test_multiple_conflicts() {
        let svn = vec![
            change("a.rs", ChangeKind::Modified),
            change("b.rs", ChangeKind::Deleted),
            change("c.rs", ChangeKind::Modified),
        ];
        let git = vec![
            change("a.rs", ChangeKind::Modified),
            change("b.rs", ChangeKind::Modified),
            change("d.rs", ChangeKind::Added),
        ];
        let conflicts = ConflictDetector::detect(&svn, &git);
        assert_eq!(conflicts.len(), 2); // a.rs content, b.rs edit/delete
    }
}
