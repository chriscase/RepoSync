//! Watermark recovery helpers for migration and git-log auto-detect.
//!
//! Global watermarks and git-log scans must not write a non-empty Git SHA into
//! repository columns: that would satisfy `column_imported` and let
//! `resolve_repo_import_baseline` backfill scoped import-completion proof from
//! unrelated global or git-log state.

use std::path::Path;
use std::process::Command;

use regex_lite::Regex;

use super::Database;
use crate::errors::DatabaseError;

static GIT_LOG_SVN_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

fn git_log_svn_re() -> &'static Regex {
    GIT_LOG_SVN_RE.get_or_init(|| {
        Regex::new(r"(?i)(?:\[(?:gitsvnsync|reposync)\].*SVN r(\d+)|imported from SVN r(\d+))")
            .unwrap()
    })
}

/// Scan a git repository log for the highest SVN revision marker and HEAD SHA.
pub fn scan_git_log_for_svn_watermark(git_dir: &Path) -> (i64, String) {
    let output = match Command::new("git")
        .args(["log", "--oneline", "-200", "--format=%H %s"])
        .current_dir(git_dir)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return (0, String::new()),
    };

    let re = git_log_svn_re();
    let mut max_rev: i64 = 0;
    let mut head_sha = String::new();
    for line in output.lines() {
        if head_sha.is_empty() {
            if let Some(sha) = line.split_whitespace().next() {
                head_sha = sha.to_string();
            }
        }
        if let Some(caps) = re.captures(line) {
            let rev_str = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|m| m.as_str())
                .unwrap_or("0");
            if let Ok(rev) = rev_str.parse::<i64>() {
                max_rev = max_rev.max(rev);
            }
        }
    }
    (max_rev, head_sha)
}

/// Copy SVN revision from global watermarks into repository columns without Git
/// SHA so resolver backfill does not treat global recovery as import completion.
pub fn recover_repo_watermark_from_global_migration(
    db: &Database,
    repo_id: &str,
) -> Result<bool, DatabaseError> {
    let Some(rev_str) = db.get_watermark("svn_rev")? else {
        return Ok(false);
    };
    let rev = rev_str.parse::<i64>().unwrap_or(0);
    if rev <= 0 {
        return Ok(false);
    }
    db.update_repo_watermark_columns_only(repo_id, rev, "")?;
    Ok(true)
}

/// Persist git-log auto-detect without minting scoped `last_svn_rev_<repo>`.
pub fn persist_git_log_auto_detect_watermark(
    db: &Database,
    repo_id: Option<&str>,
    detected_rev: i64,
) -> Result<(), DatabaseError> {
    if detected_rev <= 0 {
        return Ok(());
    }
    match repo_id.filter(|id| !id.is_empty()) {
        Some(rid) => db.update_repo_watermark_columns_only(rid, detected_rev, ""),
        None => db.set_state("last_svn_rev", &detected_rev.to_string()),
    }
}

/// Write repository columns from a git-log scan without Git SHA or scoped KV.
pub fn recover_repo_watermark_from_git_log_scan(
    db: &Database,
    repo_id: &str,
    git_dir: &Path,
) -> Result<bool, DatabaseError> {
    let (max_rev, _head_sha) = scan_git_log_for_svn_watermark(git_dir);
    if max_rev <= 0 {
        return Ok(false);
    }
    db.update_repo_watermark_columns_only(repo_id, max_rev, "")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Repository;
    use chrono::Utc;
    use tempfile::TempDir;

    fn insert_repo(db: &Database, id: &str) {
        let now = Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: id.into(),
            name: id.into(),
            svn_url: "file:///svn".into(),
            svn_branch: "trunk".into(),
            svn_username: String::new(),
            git_provider: "local".into(),
            git_api_url: String::new(),
            git_repo: "repo.git".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 5,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 0,
            last_git_sha: String::new(),
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
    fn global_migration_recovery_ignores_global_git_sha() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("state.db")).unwrap();
        db.initialize().unwrap();
        insert_repo(&db, "recovery");
        db.set_watermark("svn_rev", "9").unwrap();
        db.set_watermark("git_sha", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert!(recover_repo_watermark_from_global_migration(&db, "recovery").unwrap());
        let repo = db.get_repository("recovery").unwrap().unwrap();
        assert_eq!(repo.last_svn_rev, 9);
        assert!(repo.last_git_sha.is_empty());
    }

    #[test]
    fn persist_git_log_auto_detect_team_repo_does_not_mint_scoped_kv() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("state.db")).unwrap();
        db.initialize().unwrap();
        insert_repo(&db, "team");
        persist_git_log_auto_detect_watermark(&db, Some("team"), 42).unwrap();
        assert_eq!(db.get_state("last_svn_rev_team").unwrap(), None);
        assert_eq!(db.get_repository("team").unwrap().unwrap().last_svn_rev, 42);
        assert!(db
            .get_repository("team")
            .unwrap()
            .unwrap()
            .last_git_sha
            .is_empty());
    }

    #[test]
    fn persist_git_log_auto_detect_global_repo_writes_global_key() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("state.db")).unwrap();
        db.initialize().unwrap();
        persist_git_log_auto_detect_watermark(&db, None, 7).unwrap();
        assert_eq!(db.get_state("last_svn_rev").unwrap(), Some("7".into()));
    }
}
