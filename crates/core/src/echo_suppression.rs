//! Receipt-backed echo classification for team-mode sync (RS-05).
//!
//! Marker text (`[reposync]`) is diagnostic only; skip decisions require an
//! exact repository-scoped operation receipt.

use tracing::{debug, warn};

use crate::db::Database;
use crate::errors::{DatabaseError, SyncError};
use crate::history_inspect::is_full_git_oid;

pub const SYNC_MARKER: &str = "[reposync]";

/// How an incoming change should be treated during team-mode fetch/apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoDisposition {
    /// RepoSync already produced or handled this change for this repository.
    SkipEcho,
    /// Apply once; marker was present without a matching receipt.
    ApplyGenuineWithMarkerHint,
    /// Apply once; no marker and no receipt.
    ApplyGenuine,
}

pub struct TeamEchoContext<'a> {
    pub db: &'a Database,
    pub repo_id: &'a str,
    pub no_target_projection: &'a str,
}

/// Classify an incoming SVN revision for team mode.
pub fn classify_incoming_svn_revision(
    ctx: &TeamEchoContext<'_>,
    svn_rev: i64,
    message: &str,
) -> Result<EchoDisposition, DatabaseError> {
    let has_marker = message.contains(SYNC_MARKER);
    if ctx.db.has_repo_emitted_svn_revision(ctx.repo_id, svn_rev)? {
        debug!(
            repo_id = ctx.repo_id,
            rev = svn_rev,
            "skipping echo SVN revision (repo-scoped git_to_svn receipt)"
        );
        return Ok(EchoDisposition::SkipEcho);
    }
    if has_marker {
        warn!(
            repo_id = ctx.repo_id,
            rev = svn_rev,
            "SVN revision carries [reposync] marker without repo-scoped receipt; treating as genuine"
        );
        Ok(EchoDisposition::ApplyGenuineWithMarkerHint)
    } else {
        Ok(EchoDisposition::ApplyGenuine)
    }
}

/// Classify an incoming Git commit for team mode.
pub fn classify_incoming_git_commit(
    ctx: &TeamEchoContext<'_>,
    git_sha: &str,
    message: &str,
) -> Result<Result<EchoDisposition, SyncError>, DatabaseError> {
    let has_marker = message.contains(SYNC_MARKER);
    if ctx.db.has_repo_emitted_git_commit(ctx.repo_id, git_sha)? {
        debug!(
            repo_id = ctx.repo_id,
            sha = %git_sha,
            "skipping echo Git commit (repo-scoped svn_to_git receipt)"
        );
        return Ok(Ok(EchoDisposition::SkipEcho));
    }
    if verified_git_no_target_receipt(ctx, git_sha)? {
        debug!(
            repo_id = ctx.repo_id,
            sha = %git_sha,
            "skipping handled Git commit (repo-scoped no-target receipt)"
        );
        return Ok(Ok(EchoDisposition::SkipEcho));
    }
    if has_marker {
        warn!(
            repo_id = ctx.repo_id,
            sha = %git_sha,
            "Git commit carries [reposync] marker without repo-scoped receipt; treating as genuine"
        );
        Ok(Ok(EchoDisposition::ApplyGenuineWithMarkerHint))
    } else {
        Ok(Ok(EchoDisposition::ApplyGenuine))
    }
}

fn verified_git_no_target_receipt(
    ctx: &TeamEchoContext<'_>,
    sha: &str,
) -> Result<bool, DatabaseError> {
    let key = format!("handled_git_no_target_{}_{}", ctx.repo_id, sha);
    let Some(raw) = ctx.db.get_state(&key)? else {
        return Ok(false);
    };
    let receipt = serde_json::from_str::<serde_json::Value>(&raw).ok();
    let Some(record) = receipt else {
        return Ok(false);
    };
    if record["repo_id"] != ctx.repo_id || record["git_sha"] != sha || !is_full_git_oid(sha) {
        return Ok(false);
    }
    if record["projection"] != ctx.no_target_projection {
        return Ok(false);
    }
    Ok(matches!(
        (record["version"].as_u64(), record["outcome"].as_str()),
        (Some(1), Some("empty_commit" | "filtered")) | (Some(3), Some("no_svn_delta"))
    ))
}

/// Legacy personal-mode echo detection: marker text only.
pub fn personal_mode_marker_echo(message: &str) -> bool {
    message.contains(SYNC_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SyncDirection, SyncRecord, SyncRecordStatus};

    fn setup_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    fn insert_git_to_svn_record(db: &Database, repo_id: &str, svn_rev: i64, git_sha: &str) {
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: uuid::Uuid::new_v4().to_string(),
            repo_id: Some(repo_id.to_string()),
            svn_revision: Some(svn_rev),
            git_hash: Some(git_sha.to_string()),
            direction: SyncDirection::GitToSvn,
            author: "test".into(),
            message: "echo".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
    }

    fn insert_svn_to_git_record(db: &Database, repo_id: &str, svn_rev: i64, git_sha: &str) {
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: uuid::Uuid::new_v4().to_string(),
            repo_id: Some(repo_id.to_string()),
            svn_revision: Some(svn_rev),
            git_hash: Some(git_sha.to_string()),
            direction: SyncDirection::SvnToGit,
            author: "test".into(),
            message: "echo".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
    }

    fn ctx<'a>(db: &'a Database, repo_id: &'a str) -> TeamEchoContext<'a> {
        TeamEchoContext {
            db,
            repo_id,
            no_target_projection: "{}",
        }
    }

    #[test]
    fn same_svn_rev_across_repos_only_skips_receipt_backed_repo() {
        let db = setup_db();
        let shared_rev = 42_i64;
        let git_sha = "c".repeat(40);
        insert_git_to_svn_record(&db, "repo-a", shared_rev, &git_sha);

        let marker = format!("Imported\n\n{SYNC_MARKER} synced from Git abcdef01");
        let disposition_a =
            classify_incoming_svn_revision(&ctx(&db, "repo-a"), shared_rev, &marker).unwrap();
        let disposition_b =
            classify_incoming_svn_revision(&ctx(&db, "repo-b"), shared_rev, &marker).unwrap();

        assert_eq!(disposition_a, EchoDisposition::SkipEcho);
        assert_eq!(disposition_b, EchoDisposition::ApplyGenuineWithMarkerHint);
    }

    #[test]
    fn forged_git_marker_without_receipt_is_genuine() {
        let db = setup_db();
        let git_sha = "d".repeat(40);
        let marker = format!("Real work\n\n{SYNC_MARKER} synced from SVN r9");
        let disposition = classify_incoming_git_commit(&ctx(&db, "repo-a"), &git_sha, &marker)
            .unwrap()
            .unwrap();
        assert_eq!(disposition, EchoDisposition::ApplyGenuineWithMarkerHint);
    }

    #[test]
    fn true_git_echo_skipped_without_marker() {
        let db = setup_db();
        let git_sha = "e".repeat(40);
        insert_svn_to_git_record(&db, "repo-a", 9, &git_sha);
        let disposition = classify_incoming_git_commit(
            &ctx(&db, "repo-a"),
            &git_sha,
            "edited message with no marker",
        )
        .unwrap()
        .unwrap();
        assert_eq!(disposition, EchoDisposition::SkipEcho);
    }

    #[test]
    fn personal_mode_marker_echo_unchanged() {
        assert!(personal_mode_marker_echo("x [reposync] y"));
        assert!(!personal_mode_marker_echo("plain commit"));
    }
}
