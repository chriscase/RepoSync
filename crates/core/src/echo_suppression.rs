//! Receipt-backed echo classification for team-mode sync (RS-05).
//!
//! Marker text (`[reposync]`) is diagnostic only; skip decisions require an
//! exact repository-scoped operation receipt.

use tracing::{debug, warn};

use crate::db::git_push_operations::GitPushOperationState;
use crate::db::svn_commit_operations::SvnCommitOperationState;
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
    /// Marker without receipt, but a Running journal still owns this emit.
    DeferPendingJournal,
}

pub struct TeamEchoContext<'a> {
    pub db: &'a Database,
    pub repo_id: &'a str,
    pub no_target_projection: &'a str,
}

/// Result of validating a stored no-target receipt against admission scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoTargetReceiptVerdict {
    Accepted,
    RepoOrShaMismatch,
    ProjectionMismatch,
    UnverifiedOutcome,
}

/// Validate a no-target receipt with the same scoping the admission writer binds.
pub(crate) fn verify_no_target_receipt(
    record: &serde_json::Value,
    repo_id: &str,
    sha: &str,
    projection: &str,
) -> NoTargetReceiptVerdict {
    if record["repo_id"] != repo_id || record["git_sha"] != sha || !is_full_git_oid(sha) {
        return NoTargetReceiptVerdict::RepoOrShaMismatch;
    }
    if record["projection"] != projection {
        return NoTargetReceiptVerdict::ProjectionMismatch;
    }
    if no_target_outcome_is_verified(record) {
        NoTargetReceiptVerdict::Accepted
    } else {
        NoTargetReceiptVerdict::UnverifiedOutcome
    }
}

fn no_target_outcome_is_verified(record: &serde_json::Value) -> bool {
    match (record["version"].as_u64(), record["outcome"].as_str()) {
        (Some(1), Some("empty_commit" | "filtered")) => true,
        (Some(3), Some("no_svn_delta")) => {
            let target = &record["target"];
            target["svn_revision"].as_i64().is_some_and(|rev| rev > 0)
                && target["svn_uuid"].as_str().is_some_and(|v| !v.is_empty())
                && target["svn_url"].as_str().is_some_and(|v| !v.is_empty())
                && target["semantic_projection"] == "regular_file_bytes_no_properties_v1"
                && target["paths"].as_object().is_some_and(|paths| {
                    !paths.is_empty()
                        && paths.values().all(|entry| {
                            entry.is_null()
                                || (entry["sha256"].as_str().is_some_and(is_full_git_oid)
                                    && entry["git_mode"] == 33188
                                    && entry["svn_executable"] == false)
                        })
                })
        }
        _ => false,
    }
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
        if running_journal_claims_pending_svn_echo(ctx, svn_rev)? {
            warn!(
                repo_id = ctx.repo_id,
                rev = svn_rev,
                "SVN revision carries [reposync] marker without receipt while a Running git-to-SVN journal is active; deferring"
            );
            return Ok(EchoDisposition::DeferPendingJournal);
        }
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
        if running_journal_claims_pending_git_echo(ctx, git_sha)? {
            warn!(
                repo_id = ctx.repo_id,
                sha = %git_sha,
                "Git commit carries [reposync] marker without receipt while a Running svn-to-Git journal is active; deferring"
            );
            return Ok(Ok(EchoDisposition::DeferPendingJournal));
        }
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
    Ok(
        verify_no_target_receipt(&record, ctx.repo_id, sha, ctx.no_target_projection)
            == NoTargetReceiptVerdict::Accepted,
    )
}

fn running_journal_claims_pending_git_echo(
    ctx: &TeamEchoContext<'_>,
    git_sha: &str,
) -> Result<bool, DatabaseError> {
    let Some(op) = ctx.db.active_git_push_operation(ctx.repo_id)? else {
        return Ok(false);
    };
    Ok(op.state == GitPushOperationState::Running && op.intended_local_git_sha == git_sha)
}

fn running_journal_claims_pending_svn_echo(
    ctx: &TeamEchoContext<'_>,
    svn_rev: i64,
) -> Result<bool, DatabaseError> {
    let Some(op) = ctx.db.active_svn_commit_operation(ctx.repo_id)? else {
        return Ok(false);
    };
    if op.state != SvnCommitOperationState::Running {
        return Ok(false);
    }
    if op.last_confirmed_svn_rev == Some(svn_rev) {
        return Ok(true);
    }
    Ok(op.pre_write_svn_rev + 1 == svn_rev)
}

/// Legacy personal-mode echo detection: marker text only.
pub fn personal_mode_marker_echo(message: &str) -> bool {
    message.contains(SYNC_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::git_push_operations::{git_push_target_fingerprint, GitPushIntent};
    use crate::db::svn_commit_operations::{svn_commit_target_fingerprint, SvnCommitIntent};
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

    fn write_no_target_receipt(db: &Database, repo_id: &str, git_sha: &str, receipt: &str) {
        let key = format!("handled_git_no_target_{}_{}", repo_id, git_sha);
        db.set_state(&key, receipt).unwrap();
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

    #[test]
    fn verified_no_target_receipt_requires_admission_scope() {
        let db = setup_db();
        let sha = "f".repeat(40);
        let projection = "{}";
        let valid = serde_json::json!({
            "version": 1,
            "repo_id": "repo-a",
            "git_sha": sha,
            "outcome": "filtered",
            "projection": projection,
        });
        write_no_target_receipt(&db, "repo-a", &sha, &valid.to_string());
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "repo-a"), &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::SkipEcho
        );

        let other_repo = serde_json::json!({
            "version": 1,
            "repo_id": "repo-b",
            "git_sha": sha,
            "outcome": "filtered",
            "projection": projection,
        });
        write_no_target_receipt(&db, "repo-b", &sha, &other_repo.to_string());
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "repo-a"), &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::SkipEcho
        );
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "repo-b"), &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::SkipEcho
        );

        let forged_sha = "0".repeat(40);
        let wrong_sha = serde_json::json!({
            "version": 1,
            "repo_id": "repo-a",
            "git_sha": "1".repeat(40),
            "outcome": "filtered",
            "projection": projection,
        });
        write_no_target_receipt(&db, "repo-a", &forged_sha, &wrong_sha.to_string());
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "repo-a"), &forged_sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::ApplyGenuine
        );

        let weak_v3_sha = "2".repeat(40);
        let weak_v3 = serde_json::json!({
            "version": 3,
            "repo_id": "repo-a",
            "git_sha": weak_v3_sha,
            "outcome": "no_svn_delta",
            "projection": projection,
        });
        write_no_target_receipt(&db, "repo-a", &weak_v3_sha, &weak_v3.to_string());
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "repo-a"), &weak_v3_sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::ApplyGenuine
        );
    }

    #[test]
    fn malformed_no_target_receipt_does_not_suppress() {
        let db = setup_db();
        let sha = "1".repeat(40);
        write_no_target_receipt(&db, "repo-a", &sha, "{not-json");
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "repo-a"), &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::ApplyGenuine
        );
    }

    #[test]
    fn running_git_push_journal_defers_marker_without_receipt() {
        let db = setup_db();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','idle',0,0)",
                [],
            )
            .unwrap();
        let git_sha = "dddddddddddddddddddddddddddddddddddddddd";
        let fingerprint = git_push_target_fingerprint("pair", "origin", "main");
        db.begin_svn_to_git_push(GitPushIntent {
            repo_id: "pair",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: &fingerprint,
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "add feature",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: git_sha,
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        })
        .unwrap();
        let marker = format!("synced\n\n{SYNC_MARKER} synced from SVN r3");
        assert_eq!(
            classify_incoming_git_commit(&ctx(&db, "pair"), git_sha, &marker)
                .unwrap()
                .unwrap(),
            EchoDisposition::DeferPendingJournal
        );
    }

    #[test]
    fn running_svn_commit_journal_defers_marker_without_receipt() {
        let db = setup_db();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','idle',0,0)",
                [],
            )
            .unwrap();
        let git_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let fingerprint = svn_commit_target_fingerprint("pair", "uuid", "/repo", "/repo", "{}");
        db.begin_git_to_svn_commit(SvnCommitIntent {
            repo_id: "pair",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: &fingerprint,
            source_git_sha: git_sha,
            source_git_parent: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            source_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
            target_svn_uuid: "uuid",
            target_svn_path: "/repo",
            target_svn_root_url: "/repo",
            target_svn_branch_path: "",
            pre_write_svn_rev: 4,
            pre_write_svn_tree: "dddddddddddddddddddddddddddddddddddddddd",
            projection: "{}",
            intended_changed_paths: vec![],
            intended_svn_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            author: "dev",
            source_message: "feature",
        })
        .unwrap();
        let marker = format!("synced\n\n{SYNC_MARKER} synced from Git {git_sha}");
        assert_eq!(
            classify_incoming_svn_revision(&ctx(&db, "pair"), 5, &marker).unwrap(),
            EchoDisposition::DeferPendingJournal
        );
    }
}
