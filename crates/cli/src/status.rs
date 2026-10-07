//! Unscoped status display helpers for `reposync status`.

use std::io::Write;

use anyhow::{Context, Result};

use reposync_core::db::Database;

/// Resolve the "Last Git hash" line for unscoped `reposync status` output.
///
/// Uses the same resolution as web `/api/status` and engine `get_status`.
pub fn last_git_hash_display(db: &Database) -> Result<String> {
    let hash = reposync_core::sync_status::resolve_unscoped_last_git_hash(db)
        .context("failed to read last Git hash")?;
    Ok(hash.as_deref().unwrap_or("none").to_string())
}

/// Write the full `reposync status` report to `writer`.
pub fn write_status_report(db: &Database, writer: &mut impl Write) -> Result<()> {
    let state = reposync_core::sync_status::resolve_unscoped_sync_state(db)
        .context("failed to read sync state")?;

    let last_sync = db
        .get_state("last_sync_at")
        .context("failed to read last sync time")?;

    let last_svn_rev = db
        .get_last_svn_revision()
        .context("failed to read last SVN revision")?;

    let last_git_hash = last_git_hash_display(db)?;

    let total_syncs = db
        .count_sync_records()
        .context("failed to count sync records")?;

    let active_conflicts = db
        .count_active_conflicts()
        .context("failed to count active conflicts")?;

    let total_conflicts = db
        .count_all_conflicts()
        .context("failed to count total conflicts")?;

    let total_errors = db.count_errors().context("failed to count errors")?;

    writeln!(writer, "RepoSync Status")?;
    writeln!(writer, "=================")?;
    writeln!(writer)?;
    writeln!(writer, "  Sync state       : {}", state)?;
    writeln!(
        writer,
        "  Last sync at     : {}",
        last_sync.as_deref().unwrap_or("never")
    )?;
    writeln!(
        writer,
        "  Last SVN revision: {}",
        last_svn_rev
            .map(|r: i64| r.to_string())
            .unwrap_or_else(|| "none".to_string())
    )?;
    writeln!(writer, "  Last Git hash    : {}", last_git_hash)?;
    writeln!(writer, "  Total sync ops   : {}", total_syncs)?;
    writeln!(writer, "  Active conflicts : {}", active_conflicts)?;
    writeln!(writer, "  Total conflicts  : {}", total_conflicts)?;
    writeln!(writer, "  Total errors     : {}", total_errors)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reposync_core::models::Repository;

    fn insert_repo(db: &Database, id: &str, last_git_sha: &str) {
        let now = chrono::Utc::now().to_rfc3339();
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
            updated_at: now,
            last_svn_rev: 1,
            last_git_sha: last_git_sha.into(),
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

    #[test]
    fn legacy_empty_table_uses_global_kv_then_commit_map() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        assert_eq!(
            last_git_hash_display(&db).unwrap(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "legacy installs must keep global kv behavior"
        );

        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 1, 'svn_to_git', '2020-01-01T00:00:00Z')",
                ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
            )
            .unwrap();
        assert_eq!(
            last_git_hash_display(&db).unwrap(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "legacy installs without global kv must still report commit-map tip"
        );
    }

    #[test]
    fn multi_repo_install_prints_none_not_global_max() {
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
        insert_repo(&db, "alpha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        insert_repo(&db, "beta", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(
            last_git_hash_display(&db).unwrap(),
            "none",
            "multi-repo installs must not print a misleading global git tip"
        );
    }

    #[test]
    fn write_status_report_wires_scoped_last_git_hash_line() {
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
        insert_repo(&db, "only", "cccccccccccccccccccccccccccccccccccccccc");

        let mut output = Vec::new();
        write_status_report(&db, &mut output).unwrap();
        let rendered = String::from_utf8(output).unwrap();
        assert!(
            rendered.contains("  Last Git hash    : cccccccccccccccccccccccccccccccccccccccc"),
            "cmd_status wiring must print scoped tip, not global kv or commit-map max:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("dddddddddddddddddddddddddddddddddddddddd"),
            "cmd_status wiring must not regress to global kv:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
            "cmd_status wiring must not regress to commit-map max:\n{}",
            rendered
        );
    }

    #[test]
    fn single_managed_repo_prints_column_not_foreign_max() {
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
        insert_repo(&db, "only", "cccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(
            last_git_hash_display(&db).unwrap(),
            "cccccccccccccccccccccccccccccccccccccccc",
            "single managed repo must print its column tip"
        );

        db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha = '' WHERE id = 'only'",
                [],
            )
            .expect("clear managed-repo column");
        assert_eq!(
            last_git_hash_display(&db).unwrap(),
            "none",
            "empty managed-repo column must not fall back to foreign global max"
        );
    }
}
