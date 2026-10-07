//! Shared sync-status resolution for unscoped readers (CLI, `/api/status`, nav).
//!
//! Team-mode managed repos persist lifecycle state only to `repositories.sync_status`.
//! When the repositories table is non-empty, unscoped status surfaces aggregate those
//! per-repo values with a worst-state-wins rule. Legacy single-repo callers with an
//! empty repositories table continue to read `kv_state.sync_state`.

use crate::db::Database;
use crate::errors::DatabaseError;

/// Relative severity for worst-state-wins aggregation (higher = worse).
pub fn sync_state_priority(state: &str) -> u8 {
    match state {
        "idle" => 0,
        "detecting" | "applying" | "syncing" | "initializing" => 1,
        "conflict_found" => 2,
        "reconciliation_required" => 3,
        "error_paused" => 4,
        "error" | "failed" => 5,
        _ => 0,
    }
}

/// Pick the highest-severity state from `states`. Returns `idle` when empty.
pub fn worst_sync_state<'a>(states: impl IntoIterator<Item = &'a str>) -> String {
    let mut worst: Option<(u8, &str)> = None;
    for state in states {
        let priority = sync_state_priority(state);
        if worst.map(|(p, _)| priority > p).unwrap_or(true) {
            worst = Some((priority, state));
        }
    }
    worst
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| "idle".to_string())
}

/// Resolve the unscoped sync state for CLI/API/nav surfaces.
///
/// - Non-empty `repositories` table: worst per-repo `sync_status` among all rows.
/// - Empty table: legacy global `kv_state.sync_state`, defaulting to `idle`.
pub fn resolve_unscoped_sync_state(db: &Database) -> Result<String, DatabaseError> {
    let repos = db.list_repositories()?;
    if repos.is_empty() {
        Ok(db
            .get_state("sync_state")?
            .unwrap_or_else(|| "idle".to_string()))
    } else {
        Ok(worst_sync_state(
            repos.iter().map(|r| r.sync_status.as_str()),
        ))
    }
}

/// Resolve a load-bearing Git checkpoint tip for history inspection.
///
/// Unlike [`resolve_unscoped_last_git_hash`], legacy installs with an empty
/// `repositories` table read commit-map/sync-record fallback only (no global
/// `last_git_hash` kv), because personal writers advance `commit_map` while the
/// `git_sha` watermark stays at import baseline.
///
/// - Empty `repositories` table: commit-map fallback via `get_last_git_hash()`.
/// - Exactly one managed repo: that repo's `last_git_sha` column when non-empty.
/// - Multiple managed repos: `None` (fail closed; never infer from global kv or
///   the last `commit_map` row).
pub fn resolve_scoped_checkpoint_tip(db: &Database) -> Result<Option<String>, DatabaseError> {
    let repos = db.list_repositories()?;
    if repos.is_empty() {
        db.get_last_git_hash()
    } else if repos.len() == 1 {
        let sha = repos[0].last_git_sha.trim();
        if sha.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sha.to_string()))
        }
    } else {
        Ok(None)
    }
}

/// Resolve the unscoped Git tip for CLI/API/engine surfaces.
///
/// - Empty `repositories` table: legacy global `last_git_hash` kv, then commit-map
///   fallback via `get_last_git_hash()`.
/// - Exactly one managed repo: that repo's `last_git_sha` column when non-empty.
/// - Multiple managed repos: `None` (no honest single global tip; never infer from
///   global kv or the last `commit_map` row).
pub fn resolve_unscoped_last_git_hash(db: &Database) -> Result<Option<String>, DatabaseError> {
    let repos = db.list_repositories()?;
    if repos.is_empty() {
        match db.get_state("last_git_hash")? {
            Some(s) if !s.is_empty() => Ok(Some(s)),
            _ => db.get_last_git_hash(),
        }
    } else if repos.len() == 1 {
        let sha = repos[0].last_git_sha.trim();
        if sha.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sha.to_string()))
        }
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::models::{Repository, SyncState};

    #[test]
    fn error_paused_round_trips_through_sync_state() {
        let parsed = SyncState::from_str_val("error_paused");
        assert_eq!(parsed, SyncState::ErrorPaused);
        assert_ne!(parsed, SyncState::Idle);
        assert_eq!(parsed.to_string(), "error_paused");
    }

    #[test]
    fn resolve_unscoped_uses_global_key_without_repositories() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("sync_state", "applying").unwrap();
        assert_eq!(
            resolve_unscoped_sync_state(&db).unwrap(),
            "applying",
            "legacy single-repo readers must keep global-key behavior"
        );
    }

    #[test]
    fn resolve_unscoped_aggregates_managed_repo_status() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("sync_state", "idle").unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for (id, status) in [("alpha", "idle"), ("beta", "reconciliation_required")] {
            db.insert_repository(&Repository {
                id: id.into(),
                name: id.into(),
                svn_url: "file:///tmp/svn".into(),
                svn_branch: "trunk".into(),
                svn_username: "fixture".into(),
                git_provider: "github".into(),
                git_api_url: "http://127.0.0.1:1".into(),
                git_repo: "org/repo".into(),
                git_branch: "main".into(),
                sync_mode: "team".into(),
                poll_interval_secs: 60,
                lfs_threshold_mb: 0,
                auto_merge: false,
                enabled: true,
                created_by: None,
                parent_id: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                last_svn_rev: 0,
                last_git_sha: String::new(),
                last_sync_at: None,
                sync_status: status.into(),
                total_syncs: 0,
                total_errors: 0,
                allowed_paths: None,
                blocked_patterns: None,
                consecutive_errors: 0,
                teams_webhook_url: None,
            })
            .unwrap();
        }
        assert_eq!(
            resolve_unscoped_sync_state(&db).unwrap(),
            "reconciliation_required",
            "worst managed-repo state must win over stale global idle"
        );
    }

    #[test]
    fn worst_state_wins_across_team_repos() {
        assert_eq!(
            worst_sync_state(["idle", "reconciliation_required", "idle"]),
            "reconciliation_required"
        );
        assert_eq!(
            worst_sync_state(["idle", "error_paused", "initializing"]),
            "error_paused"
        );
        assert_eq!(
            worst_sync_state(["idle", "error", "reconciliation_required"]),
            "error"
        );
        assert_eq!(worst_sync_state([]), "idle");
    }

    #[test]
    fn error_paused_beats_idle_and_initializing() {
        assert!(sync_state_priority("error_paused") > sync_state_priority("idle"));
        assert!(sync_state_priority("error_paused") > sync_state_priority("initializing"));
        assert!(sync_state_priority("error") > sync_state_priority("error_paused"));
    }

    #[test]
    fn resolve_unscoped_git_hash_uses_legacy_global_chain() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        assert_eq!(
            resolve_unscoped_last_git_hash(&db).unwrap().as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "legacy single-repo readers must keep global-key behavior"
        );
    }

    #[test]
    fn resolve_unscoped_git_hash_falls_back_to_commit_map_without_repositories() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 1, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
            )
            .unwrap();
        assert_eq!(
            resolve_unscoped_last_git_hash(&db).unwrap().as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "legacy installs without global kv must still report commit-map tip"
        );
    }

    #[test]
    fn resolve_unscoped_git_hash_omits_foreign_tip_with_multiple_managed_repos() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 9, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
            )
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for (id, sha) in [
            ("alpha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            ("beta", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ] {
            db.insert_repository(&Repository {
                id: id.into(),
                name: id.into(),
                svn_url: "file:///tmp/svn".into(),
                svn_branch: "trunk".into(),
                svn_username: "fixture".into(),
                git_provider: "github".into(),
                git_api_url: "http://127.0.0.1:1".into(),
                git_repo: "org/repo".into(),
                git_branch: "main".into(),
                sync_mode: "team".into(),
                poll_interval_secs: 60,
                lfs_threshold_mb: 0,
                auto_merge: false,
                enabled: true,
                created_by: None,
                parent_id: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                last_svn_rev: 1,
                last_git_sha: sha.into(),
                last_sync_at: None,
                sync_status: "idle".into(),
                total_syncs: 0,
                total_errors: 0,
                allowed_paths: None,
                blocked_patterns: None,
                consecutive_errors: 0,
                teams_webhook_url: None,
            })
            .unwrap();
        }
        assert_eq!(
            resolve_unscoped_last_git_hash(&db).unwrap(),
            None,
            "multiple managed repos must not surface a misleading global git tip"
        );
    }

    #[test]
    fn resolve_scoped_checkpoint_tip_uses_commit_map_without_repositories() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 1, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
            )
            .unwrap();
        assert_eq!(
            resolve_scoped_checkpoint_tip(&db).unwrap().as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "history inspect must prefer commit-map tip over global kv on legacy installs"
        );
    }

    #[test]
    fn resolve_scoped_checkpoint_tip_omits_foreign_tip_with_multiple_managed_repos() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 9, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
            )
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for (id, sha) in [
            ("alpha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            ("beta", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ] {
            db.insert_repository(&Repository {
                id: id.into(),
                name: id.into(),
                svn_url: "file:///tmp/svn".into(),
                svn_branch: "trunk".into(),
                svn_username: "fixture".into(),
                git_provider: "github".into(),
                git_api_url: "http://127.0.0.1:1".into(),
                git_repo: "org/repo".into(),
                git_branch: "main".into(),
                sync_mode: "team".into(),
                poll_interval_secs: 60,
                lfs_threshold_mb: 0,
                auto_merge: false,
                enabled: true,
                created_by: None,
                parent_id: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                last_svn_rev: 1,
                last_git_sha: sha.into(),
                last_sync_at: None,
                sync_status: "idle".into(),
                total_syncs: 0,
                total_errors: 0,
                allowed_paths: None,
                blocked_patterns: None,
                consecutive_errors: 0,
                teams_webhook_url: None,
            })
            .unwrap();
        }
        assert_eq!(
            resolve_scoped_checkpoint_tip(&db).unwrap(),
            None,
            "history inspect must not borrow a foreign commit-map tip in multi-repo installs"
        );
    }

    #[test]
    fn resolve_scoped_checkpoint_tip_reports_single_managed_repo_column() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 9, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
            )
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: "only".into(),
            name: "only".into(),
            svn_url: "file:///tmp/svn".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "github".into(),
            git_api_url: "http://127.0.0.1:1".into(),
            git_repo: "org/repo".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 60,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 1,
            last_git_sha: "cccccccccccccccccccccccccccccccccccccccc".into(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        assert_eq!(
            resolve_scoped_checkpoint_tip(&db).unwrap().as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccc"),
            "history inspect must use the managed-repo column, not foreign global kv or commit-map tip"
        );

        db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha = '' WHERE id = 'only'",
                [],
            )
            .unwrap();
        assert_eq!(
            resolve_scoped_checkpoint_tip(&db).unwrap(),
            None,
            "empty managed-repo column must not fall back to global kv or commit-map tip"
        );
    }

    #[test]
    fn resolve_scoped_checkpoint_tip_omits_global_kv_without_commit_map() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        assert_eq!(
            resolve_scoped_checkpoint_tip(&db).unwrap(),
            None,
            "legacy history inspect must not read global kv when commit_map is empty"
        );
    }

    #[test]
    fn resolve_unscoped_git_hash_reports_single_managed_repo_column() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 9, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
            )
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: "only".into(),
            name: "only".into(),
            svn_url: "file:///tmp/svn".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "github".into(),
            git_api_url: "http://127.0.0.1:1".into(),
            git_repo: "org/repo".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 60,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 1,
            last_git_sha: "cccccccccccccccccccccccccccccccccccccccc".into(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        assert_eq!(
            resolve_unscoped_last_git_hash(&db).unwrap().as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccc"),
            "single managed repo must report its column tip, not foreign global max"
        );

        db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha = '' WHERE id = 'only'",
                [],
            )
            .unwrap();
        assert_eq!(
            resolve_unscoped_last_git_hash(&db).unwrap(),
            None,
            "empty managed-repo column must not fall back to global kv or commit-map tip"
        );
    }
}
