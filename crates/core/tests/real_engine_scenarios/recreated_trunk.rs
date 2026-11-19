//! A copied replacement for the registered trunk must not inherit its old pair's cursors.
use super::ordinary_svn_merge::{
    assert_publication, check_frontier, cycle, file_at, files_at, journal_documents,
    managed_engine, reflog, svn, youngest,
};
use super::{
    git_cli, git_main_commit_and_tree, git_path_missing, git_show_blob, outage_ledger,
    svn_checkout, svn_commit_file, svn_source_key, svnlook_at, Database, MappingOutcome,
    OutageLedger, SingleRepoFixture, SvnClient, SyncError,
};
use reposync_core::db::lineage_checkpoints::{observe_live_svn_identity, LiveSvnIdentity};
use reposync_core::db::team_cycle_mapping_operations::{
    TeamCycleMappingOperation, TeamCycleMappingOutcome, TeamCycleMappingState,
};
use reposync_core::echo_receipt_scope::{
    handled_svn_no_target_state_key, read_svn_no_target_receipt, repo_echo_generation,
};
use serde_json::{json, Value};
use std::path::Path;

// These values may not change on a hold, even though the directional checkpoint
// can now describe the observed identity conflict.
#[derive(Debug, PartialEq, Eq)]
struct RetainedRecords {
    svn_mirror: String,
    git_baseline: String,
    no_target_receipt: String,
    push_documents: Vec<String>,
    svn_commit_documents: Vec<String>,
    mapping_documents: Vec<String>,
    total_syncs: i64,
}

fn records(f: &SingleRepoFixture, receipt_key: &str) -> RetainedRecords {
    let db = Database::new(&f.db_path).unwrap();
    let id = &f.repo.id;
    let state = |key: &str| db.get_state(key).unwrap().unwrap();
    assert!(db.active_import_operation(id).unwrap().is_none());
    assert!(db.active_git_push_operation(id).unwrap().is_none());
    assert!(db.active_svn_commit_operation(id).unwrap().is_none());
    assert!(db
        .active_team_cycle_mapping_operation(id)
        .unwrap()
        .is_none());
    RetainedRecords {
        svn_mirror: state(&format!("last_svn_rev_{id}")),
        git_baseline: state(&format!("handled_git_baseline_{id}")),
        no_target_receipt: state(receipt_key),
        push_documents: journal_documents(f, "svn_to_git_push_v1:document:*"),
        svn_commit_documents: journal_documents(f, "git_to_svn_commit_v1:document:*"),
        mapping_documents: journal_documents(f, "team_cycle_mapping_v1:document:*"),
        total_syncs: db.get_repository(id).unwrap().unwrap().total_syncs,
    }
}

async fn live_identity(f: &SingleRepoFixture) -> LiveSvnIdentity {
    let r = &f.repo;
    let client = SvnClient::new(&r.svn_url, &r.username, &r.password);
    let info = client.info().await.unwrap();
    observe_live_svn_identity(&client, &info).await.unwrap()
}

fn assert_equal_content(repo: &Path, original: i64, replacement: i64) {
    let mut snapshots = Vec::new();
    for rev in [original, replacement] {
        let rev_arg = rev.to_string();
        let paths = svnlook_at(repo, "tree", &["--full-paths", "-r", &rev_arg, "trunk"]);
        assert_eq!(
            paths.lines().collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["trunk/", "trunk/.gitkeep", "trunk/origin.txt"])
        );
        assert_eq!(file_at(repo, rev, "trunk/.gitkeep"), "");
        assert_eq!(file_at(repo, rev, "trunk/origin.txt"), "SVN origin\n");
        let props: Vec<_> = ["trunk", "trunk/.gitkeep", "trunk/origin.txt"]
            .iter()
            .map(|path| svnlook_at(repo, "proplist", &["--verbose", "-r", &rev_arg, path]))
            .collect();
        let ignore = svnlook_at(repo, "propget", &["-r", &rev_arg, "svn:ignore", "trunk"]);
        assert_eq!(ignore.trim_end(), "*.tmp");
        snapshots.push(props);
    }
    assert_eq!(snapshots[0], snapshots[1]);
}

async fn cold_hold(f: &SingleRepoFixture, before: &OutageLedger, live: &LiveSvnIdentity) -> Value {
    let engine = managed_engine(f);
    let error = engine.run_sync_cycle().await.unwrap_err();
    let SyncError::HistoryBlocked { reason, detail } = error else {
        panic!("unexpected sync result: {error}")
    };
    assert_eq!(reason, "svn_identity_changed");
    assert!(
        detail.contains(&format!("created_rev={}", live.incarnation_created_rev)),
        "{detail}"
    );
    assert!(
        detail.contains(&format!(
            "created_rev={}",
            before.lineage.incarnation_created_rev
        )),
        "{detail}"
    );
    drop(engine);
    let db = Database::new(&f.db_path).unwrap();
    let row = db.get_repository(&f.repo.id).unwrap().unwrap();
    assert_eq!(row.sync_status, "reconciliation_required");
    assert_eq!(
        repo_echo_generation(&db, &f.repo.id).unwrap(),
        before.lineage.generation
    );
    let held = outage_ledger(f);
    assert_eq!(held.watermark, before.watermark);
    assert_eq!(held.lineage, before.lineage);
    assert_eq!(held.inbound_git, before.inbound_git);
    assert_eq!(held.git_checkpoint, before.git_checkpoint);
    assert_eq!(held.commit_map, before.commit_map);
    assert_eq!(held.sync_records, before.sync_records);
    let [diagnostic] = held
        .outcomes
        .strip_prefix(before.outcomes.as_slice())
        .unwrap()
    else {
        panic!("expected one scoped SVN identity diagnostic")
    };
    let key = format!("svn-identity:{}", live.uuid);
    assert_eq!(diagnostic["key"], key);
    assert_eq!(diagnostic["direction"], "svn_to_git");
    assert_eq!(diagnostic["outcome"], "unknown_reconciliation_required");
    assert_eq!(diagnostic["reason"], reason);
    assert!(
        diagnostic["sha"].is_null()
            && diagnostic["target_svn_rev"].is_null()
            && diagnostic["policy_snapshot"].is_null()
    );
    assert_eq!(held.svn_checkpoint.handled_source_key, key);
    assert_eq!(
        held.svn_checkpoint.outcome,
        MappingOutcome::UnknownReconciliationRequired
    );
    assert_eq!(held.svn_checkpoint.reason.as_deref(), Some(reason.as_str()));
    assert!(
        held.svn_checkpoint.target_git_sha.is_none()
            && held.svn_checkpoint.target_svn_rev.is_none()
            && held.svn_checkpoint.policy_snapshot.is_none()
    );
    let bridge = git2::Repository::open(&f.repo.bridge).unwrap();
    let head = bridge.head().unwrap();
    assert_eq!(head.name(), Some("refs/heads/main"));
    assert_eq!(head.target().unwrap().to_string(), before.watermark.1);
    git_cli(
        &f.repo.bridge,
        &["diff", "--cached", "--quiet", "HEAD", "--"],
    );
    git_cli(&f.repo.bridge, &["diff", "--quiet", "--"]);
    diagnostic.clone()
}

pub(super) async fn run() -> Value {
    let f = SingleRepoFixture::on_svn_branch("direct", "trunk");
    let repo = &f.repo;
    let root = f.daemon.repo_url(&repo.id);
    let svn_repo = f.tmp.path().join("svnserve-root").join(&repo.id);
    let archive = format!("{root}/preserved");
    git_cli(&repo.bare, &["config", "core.logAllRefUpdates", "true"]);
    let engine = managed_engine(&f);
    cycle(&engine, 1).await;
    let applied = repo.svn_rev;
    let applied_ledger = outage_ledger(&f);
    let published = assert_publication(&f, &applied_ledger, applied, &repo.initial_git_sha);
    assert_eq!(applied_ledger.sync_records.len(), 1);
    assert_eq!(applied_ledger.outcomes.len(), 1);
    let applied_receipt = &applied_ledger.sync_records[0]["id"];
    let original = live_identity(&f).await;
    assert_eq!(original.uuid, applied_ledger.lineage.svn_uuid);
    assert_eq!(original.root_url, root);
    assert_eq!(original.url, repo.svn_url);
    assert_eq!(original.branch_path, "trunk");
    assert_eq!(original.incarnation_created_rev, 1);
    assert_eq!(
        original.incarnation_created_rev,
        applied_ledger.lineage.incarnation_created_rev
    );
    assert!(original.copy_from_path.is_none() && original.copy_from_rev.is_none());

    svn(&f, &["update", "."]);
    svn(&f, &["propset", "svn:ignore", "*.tmp", "."]);
    svn(&f, &["commit", ".", "-m", "Set trunk ignore property"]);
    let meta = youngest(&svn_repo);
    assert_eq!(meta, applied + 1);
    cycle(&engine, 0).await;
    let before = outage_ledger(&f);
    check_frontier(&f, &before, meta, &published);
    assert_eq!(before.lineage, applied_ledger.lineage);
    assert_eq!(before.sync_records, applied_ledger.sync_records);
    assert_eq!(
        before.svn_checkpoint.outcome,
        MappingOutcome::IntentionallyFilteredNoTarget
    );
    let [filtered] = before
        .outcomes
        .strip_prefix(applied_ledger.outcomes.as_slice())
        .unwrap()
    else {
        panic!("expected one no-target outcome")
    };
    assert_eq!(filtered["key"], svn_source_key(meta));
    assert_eq!(filtered["reason"], "no_git_content");
    assert!(filtered["sha"].is_null() && filtered["target_svn_rev"].is_null());
    let receipt_key = handled_svn_no_target_state_key(
        &before.lineage.scope_uuid,
        before.lineage.generation,
        meta,
    );
    let established = records(&f, &receipt_key);
    assert_eq!(established.total_syncs, 1);
    assert_eq!(established.svn_mirror, meta.to_string());
    let baseline: Value = serde_json::from_str(&established.git_baseline).unwrap();
    assert_eq!(baseline["repo_id"], repo.id);
    assert_eq!(baseline["svn_rev"], applied);
    assert_eq!(baseline["git_sha"], published);
    assert_eq!(established.push_documents.len(), 1);
    assert!(established.svn_commit_documents.is_empty());
    let [doc] = established.mapping_documents.as_slice() else {
        panic!("expected one completed mapping operation")
    };
    let operation: TeamCycleMappingOperation = serde_json::from_str(doc).unwrap();
    assert_eq!(operation.state, TeamCycleMappingState::Completed);
    assert_eq!(
        operation.outcome,
        TeamCycleMappingOutcome::SvnNoTargetContent
    );
    assert_eq!(operation.source_svn_rev, Some(meta));
    let receipt = read_svn_no_target_receipt(engine.db(), &repo.id, meta)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&established.no_target_receipt).unwrap(),
        receipt
    );
    assert_eq!(receipt["outcome"], "no_git_content");
    assert_eq!(receipt["scope_uuid"], before.lineage.scope_uuid);
    assert_eq!(receipt["generation"], before.lineage.generation);
    assert_eq!(receipt["svn_revision"], meta);
    let publish_log = reflog(&f);
    let git_base = git_main_commit_and_tree(&repo.bare);
    assert_eq!(git_base.0.to_string(), published);
    assert_eq!(publish_log.lines().count(), 1);
    drop(engine);

    let original_peg = format!("{}@{meta}", repo.svn_url);
    svn(
        &f,
        &[
            "copy",
            &original_peg,
            &archive,
            "-m",
            "Preserve the original trunk",
        ],
    );
    let copied = youngest(&svn_repo);
    svn(
        &f,
        &["delete", &repo.svn_url, "-m", "Retire the original trunk"],
    );
    let deleted = youngest(&svn_repo);
    assert_eq!(
        svnlook_at(&svn_repo, "changed", &["-r", &deleted.to_string()]).trim_end(),
        "D   trunk/"
    );
    svn(
        &f,
        &[
            "copy",
            &format!("{archive}@{copied}"),
            &repo.svn_url,
            "-m",
            "Recreate trunk with identical contents",
        ],
    );
    let recreated = youngest(&svn_repo);
    assert_eq!((copied, deleted, recreated), (meta + 1, meta + 2, meta + 3));
    for (rev, target, source, peg) in [
        (copied, "preserved", "trunk", meta),
        (recreated, "trunk", "preserved", copied),
    ] {
        let changes = svnlook_at(
            &svn_repo,
            "changed",
            &["--copy-info", "-r", &rev.to_string()],
        );
        assert!(
            changes.contains(&format!("A + {target}/"))
                && changes.contains(&format!("from {source}/:r{peg}")),
            "{changes}"
        );
    }
    let replacement = live_identity(&f).await;
    assert_eq!(replacement.uuid, original.uuid);
    assert_eq!(replacement.root_url, original.root_url);
    assert_eq!(replacement.url, original.url);
    assert_eq!(replacement.branch_path, original.branch_path);
    assert_eq!(replacement.incarnation_created_rev, recreated);
    assert_eq!(replacement.copy_from_path.as_deref(), Some("/preserved"));
    assert_eq!(replacement.copy_from_rev, Some(copied));
    assert_equal_content(&svn_repo, meta, recreated);

    let unchanged_source = svnlook_at(&svn_repo, "tree", &["--show-ids", "--full-paths"]);
    let diagnostic = cold_hold(&f, &before, &replacement).await;
    assert_eq!(records(&f, &receipt_key), established);
    assert_eq!(youngest(&svn_repo), recreated);
    assert_eq!(
        svnlook_at(&svn_repo, "tree", &["--show-ids", "--full-paths"]),
        unchanged_source
    );
    assert_eq!(reflog(&f), publish_log);
    files_at(&f, &[(".gitkeep", ""), ("origin.txt", "SVN origin\n")]);

    // Queue independent, valid work on each side while the same pair is held.
    let new_wc = f.tmp.path().join("replacement-wc");
    svn_checkout(&repo.svn_url, &new_wc, &repo.username, &repo.password);
    let pending_svn = svn_commit_file(
        &new_wc,
        "svn-pending.txt",
        "new incarnation only\n",
        "SVN change after replacement",
        &repo.username,
        &repo.password,
    );
    assert_eq!(pending_svn, recreated + 1);
    assert_eq!(
        svnlook_at(&svn_repo, "changed", &["-r", &pending_svn.to_string()]).trim_end(),
        "A   trunk/svn-pending.txt"
    );
    git_cli(&f.developer, &["pull", "--ff-only", "origin", "main"]);
    let pending_git = f.developer_commit(
        "git-pending.txt",
        "developer change\n",
        "Pending independent FF",
    );
    git_cli(&f.developer, &["push", "origin", "main"]);
    let git_remote = git_main_commit_and_tree(&repo.bare);
    assert_eq!(git_remote.0.to_string(), pending_git);
    let bare = git2::Repository::open_bare(&repo.bare).unwrap();
    let child = bare.find_commit(git_remote.0).unwrap();
    assert_eq!(child.parent_count(), 1);
    assert_eq!(child.parent_id(0).unwrap(), git_base.0);
    let external_log = reflog(&f);
    let added = external_log.strip_prefix(&publish_log).unwrap();
    assert_eq!(added.lines().count(), 1);
    assert!(added.starts_with(&format!("{published} {pending_git} ")));

    let pending_source = svnlook_at(&svn_repo, "tree", &["--show-ids", "--full-paths"]);
    let second = cold_hold(&f, &before, &replacement).await;
    assert_eq!(second, diagnostic);
    assert_eq!(records(&f, &receipt_key), established);
    assert_eq!(youngest(&svn_repo), pending_svn);
    assert_eq!(
        svnlook_at(&svn_repo, "tree", &["--show-ids", "--full-paths"]),
        pending_source
    );
    assert_eq!(
        file_at(&svn_repo, pending_svn, "trunk/svn-pending.txt"),
        "new incarnation only\n"
    );
    assert_eq!(
        file_at(&svn_repo, pending_svn, "preserved/origin.txt"),
        "SVN origin\n"
    );
    assert_eq!(git_main_commit_and_tree(&repo.bare), git_remote);
    assert_eq!(bare.references().unwrap().count(), 1);
    assert_eq!(git_main_commit_and_tree(&repo.bridge), git_base);
    assert_eq!(
        git_show_blob(&repo.bare, "main", "git-pending.txt"),
        "developer change\n"
    );
    assert!(git_path_missing(&repo.bare, "main", "svn-pending.txt"));
    assert!(git_path_missing(&repo.bridge, "main", "git-pending.txt"));
    assert!(
        !repo.bridge.join("svn-pending.txt").exists()
            && !repo.bridge.join("git-pending.txt").exists()
    );
    assert_eq!(reflog(&f), external_log);

    json!({
        "svn_uuid": replacement.uuid,
        "scope_uuid": before.lineage.scope_uuid,
        "generation": before.lineage.generation,
        "branch_path": replacement.branch_path,
        "original_created_revision": original.incarnation_created_rev,
        "applied_revision": applied,
        "no_target_revision": meta,
        "copy_revision": copied,
        "delete_revision": deleted,
        "recreated_revision": replacement.incarnation_created_rev,
        "copy_from_path": replacement.copy_from_path,
        "copy_from_revision": replacement.copy_from_rev,
        "pending_svn_revision": pending_svn,
        "published_git_sha": published,
        "pending_git_sha": pending_git,
        "applied_receipt_id": applied_receipt,
        "no_target_operation_id": operation.id,
        "hold_reason": diagnostic["reason"],
        "diagnostic_source_key": diagnostic["key"],
        "diagnostic_outcome_id": diagnostic["id"],
        "held_cycles": 2,
        "engine_ref_updates": 0
    })
}
