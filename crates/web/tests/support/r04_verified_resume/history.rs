use crate::{inbound_git_sha_kv, rs67_command};
use git2::{Oid, Repository};
use reposync_core::db::{import_operations::ImportOperationState, Database};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

const MAIN: &str = "refs/heads/main";
const PREFIX: usize = 50;
const TOTAL: usize = 52;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Held,
    Verified,
    Completed,
}

// Read both repositories independently of the operation's staged/confirmed mappings.
fn compare_histories(source: &Path, git: &Repository, length: usize) -> Vec<String> {
    let mut commit = git.find_reference(MAIN).unwrap().peel_to_commit().unwrap();
    let mut history = vec![commit.id()];
    while commit.parent_count() != 0 {
        assert_eq!(
            commit.parent_count(),
            1,
            "unexpected merge at {}",
            commit.id()
        );
        commit = commit.parent(0).unwrap();
        history.push(commit.id());
    }
    assert_eq!(history.len(), length, "number of reachable commits");
    history.reverse();

    let svn = source.to_str().unwrap();
    let trunk = format!("file://{svn}/trunk");
    let mut contents = HashSet::new();
    for (i, oid) in history.iter().enumerate() {
        let rev = (i + 1).to_string();
        let commit = git.find_commit(*oid).unwrap();
        let marker = format!("[reposync] imported from SVN r{rev}");
        assert!(commit.message().unwrap().lines().any(|line| line == marker));
        let tree = commit.tree().unwrap();
        let files = rs67_command(
            "svn",
            &["list", "--non-interactive", "-R", "-r", &rev, &trunk],
        );
        if i == 0 {
            assert!(
                files.is_empty() && tree.is_empty(),
                "expected empty trunk at r1"
            );
        } else {
            assert_eq!(files, b"history.txt\n", "SVN inventory at r{rev}");
            assert_eq!(tree.len(), 1, "Git inventory at r{rev}");
            let entry = tree.get_name("history.txt").unwrap();
            assert_eq!(entry.filemode(), 0o100644);
            let blob = git.find_blob(entry.id()).unwrap();
            let bytes = rs67_command("svnlook", &["cat", "-r", &rev, svn, "trunk/history.txt"]);
            assert_eq!(blob.content(), bytes, "committed file bytes at r{rev}");
            assert!(
                contents.insert(bytes),
                "fixture reused file contents at r{rev}"
            );
        }
    }
    history.into_iter().map(|oid| oid.to_string()).collect()
}

pub(super) fn observe(
    root: &Path,
    bare: &Path,
    repo_id: &str,
    operation_id: &str,
    phase: Phase,
) -> Value {
    let completed = phase == Phase::Completed;
    let length = if completed { TOTAL } else { PREFIX };
    let source = root.join("svn-repo");
    let svn = source.to_str().unwrap();
    let youngest = String::from_utf8(rs67_command("svnlook", &["youngest", svn])).unwrap();
    let youngest: usize = youngest.trim().parse().unwrap();
    // r52 is committed in SVN after the partial publication is verified.
    assert_eq!(youngest, if completed { TOTAL } else { TOTAL - 1 });
    let uuid = String::from_utf8(rs67_command("svnlook", &["uuid", svn])).unwrap();
    let git = Repository::open_bare(bare).unwrap();
    let history = compare_histories(&source, &git, length);
    let tip = history.last().unwrap();
    let prefix = &history[PREFIX - 1];

    let refs: Vec<_> = git
        .references()
        .unwrap()
        .names()
        .map(|name| name.unwrap().to_owned())
        .collect();
    assert_eq!(refs, [MAIN]);
    let reflog = git.reflog(MAIN).unwrap();
    let updates = if completed { 2 } else { 1 };
    assert_eq!(reflog.len(), updates);
    let first = reflog.get(updates - 1).unwrap();
    assert_eq!(first.id_old(), Oid::zero());
    assert_eq!(first.id_new().to_string(), *prefix);
    if completed {
        let second = reflog.get(0).unwrap();
        assert_eq!(second.id_old(), first.id_new());
        assert_eq!(second.id_new().to_string(), *tip);
    }

    let db = Database::open_existing_read_only(root.join("reposync.db")).unwrap();
    let op = db
        .get_import_operation(repo_id, operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.repo_id, repo_id);
    assert_eq!(op.id, operation_id);
    assert_eq!(op.initiator_id, "legacy");
    assert_eq!(op.request_id, super::REQUEST);
    assert_eq!(op.operation_type, "full_import");
    assert_eq!(op.total_revisions, Some(youngest as u64));
    assert_eq!(op.processed_revisions, length as u64);
    assert_eq!(op.local_commits, length as u64);
    assert_eq!(op.last_local_svn_rev, Some(length as i64));
    assert_eq!(op.last_local_git_sha.as_ref(), Some(tip));
    let active = db
        .get_state(&format!("import_operation_v1:active:{repo_id}"))
        .unwrap();
    let pending = &op.pending_commit_maps;
    let mut rows = db.list_commit_map((TOTAL + 1) as u32).unwrap();
    rows.reverse(); // insertion order in this single-registration fixture
    let expected_maps = if phase == Phase::Held { 0 } else { length };
    assert_eq!(rows.len(), expected_maps);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.repo_id.as_deref(), Some(repo_id));
        assert_eq!(row.direction, "svn_to_git");
        assert_eq!(row.svn_rev, (i + 1) as i64);
        assert_eq!(row.git_sha, history[i]);
    }
    if phase == Phase::Held {
        assert_eq!(pending.len(), PREFIX);
        for (i, row) in pending.iter().enumerate() {
            assert_eq!(row.svn_rev, (i + 1) as i64);
            assert_eq!(row.git_sha, history[i]);
        }
        assert_eq!(op.intended_ref.as_deref(), Some(MAIN));
        assert_eq!(op.intended_git_sha.as_ref(), Some(tip));
        assert_eq!(op.confirmed_batches, 0);
        assert_eq!(op.last_confirmed_svn_rev, None);
        assert_eq!(op.last_confirmed_git_sha, None);
    } else {
        assert!(pending.is_empty());
        assert!(op.intended_ref.is_none() && op.intended_git_sha.is_none());
        assert_eq!(op.confirmed_batches, updates as u64);
        assert_eq!(op.last_confirmed_svn_rev, Some(length as i64));
        assert_eq!(op.last_confirmed_git_sha.as_ref(), Some(tip));
    }

    let checkpoint = db.get_repo_watermark(repo_id).unwrap();
    let svn_cursor = db.get_state(&format!("last_svn_rev_{repo_id}")).unwrap();
    let git_cursor = inbound_git_sha_kv(&db, repo_id);
    assert_eq!(op.resume_authorized, phase == Phase::Verified);
    if completed {
        assert_eq!(active, None);
        assert_eq!(op.state, ImportOperationState::Completed);
        assert_eq!(checkpoint, (TOTAL as i64, tip.clone()));
        assert_eq!(svn_cursor, Some(TOTAL.to_string()));
        assert_eq!(git_cursor.as_ref(), Some(tip));
    } else {
        assert_eq!(active.as_deref(), Some(operation_id));
        assert_eq!(op.state, ImportOperationState::ReconciliationRequired);
        assert_eq!(checkpoint, (0, String::new()));
        assert_eq!(svn_cursor, None);
        assert_eq!(git_cursor, None);
    }
    let prefix_map_ids: Vec<_> = rows.iter().take(PREFIX).map(|row| row.id).collect();
    let suffix_map_ids: Vec<_> = rows.iter().skip(PREFIX).map(|row| row.id).collect();
    json!({
        "initiator_id": op.initiator_id, "request_id": op.request_id,
        "history_length": length, "prefix_sha": prefix, "tip": tip, "suffix": &history[PREFIX..],
        "ref_updates": reflog.len(), "pending_maps": pending.len(), "durable_maps": rows.len(),
        "prefix_map_ids": prefix_map_ids, "suffix_map_ids": suffix_map_ids, "checkpoint": checkpoint,
        "svn_uuid": uuid.trim(), "svn_youngest": youngest,
    })
}
