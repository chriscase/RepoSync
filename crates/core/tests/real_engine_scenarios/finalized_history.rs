//! Safety and recovery of scoped SVN cursors after a completed team sync.
use super::{
    git_cli, git_main_commit_and_tree, git_path_missing, git_show_blob, git_symbolic_ref,
    make_app_config, outage_ledger, svn_commit_file, svn_source_key, svnlook_at, Database,
    MappingOutcome, OutageLedger, SingleRepoFixture, SyncEngine, SyncError,
};
use reposync_core::config::SyncMode;
use reposync_core::db::git_push_operations::{GitPushOperation, GitPushOperationState};
use reposync_core::db::import_operations::{
    repo_import_baseline_is_pending_read_only, resolve_repo_import_baseline, RepoImportBaseline,
};
use serde_json::{json, Value};

// Snapshot through a new database connection and the actual SVN and Git repositories.
// Operational status/error counters may change on a hold. Receipts and refs must not.
#[derive(Debug, PartialEq)]
struct FinalizedHistory {
    ledger: OutageLedger,
    operations: Vec<GitPushOperation>,
    mirror: Option<String>,
    handled_git_baseline: String,
    bare_reflog: String,
    svn_tree: String,
    total_syncs: i64,
    registration_mode: String,
}

const FINALIZED_CHANGES: [(&str, &str, &str); 3] = [
    ("origin.txt", "SVN origin\n", "Verified SVN origin"),
    (
        "recovery.txt",
        "post-write recovery\n",
        "SVN revision for post-write recovery",
    ),
    (
        "child.txt",
        "pending SVN child\n",
        "Add child after finalized SVN revision",
    ),
];

fn snapshot(f: &SingleRepoFixture, published: i64, youngest: i64) -> FinalizedHistory {
    let repo = &f.repo;
    let db = Database::new(&f.db_path).unwrap();
    let ledger = outage_ledger(f);
    let svn = f.tmp.path().join("svnserve-root").join(&repo.id);
    assert_eq!(
        svnlook_at(&svn, "uuid", &[]).trim(),
        ledger.lineage.svn_uuid
    );
    assert_eq!(
        svnlook_at(&svn, "youngest", &[]).trim(),
        youngest.to_string()
    );
    assert_eq!(ledger.lineage.git_ref.as_deref(), Some("main"));
    assert_eq!(ledger.lineage.incarnation_created_rev, 1);
    assert_eq!(ledger.lineage.generation, 1);
    assert!(ledger.lineage.svn_branch_path.is_empty());
    assert!(
        ledger.git_checkpoint.is_none()
            && ledger.inbound_git.is_none()
            && ledger.commit_map.is_empty()
    );
    assert_eq!(ledger.outcomes.len(), (published - 1) as usize);

    let operations = {
        let conn = db.conn();
        let mut stmt = conn.prepare(
            "SELECT value FROM kv_state
             WHERE key GLOB 'svn_to_git_push_v1:document:*' AND json_extract(value, '$.repo_id') = ?1
             ORDER BY json_extract(value, '$.source_svn_rev'), key",
        ).unwrap();
        let documents = stmt
            .query_map([&repo.id], |row| row.get::<_, String>(0))
            .unwrap();
        let mut operations = Vec::new();
        for document in documents {
            let operation: GitPushOperation = serde_json::from_str(&document.unwrap()).unwrap();
            operations.push(operation);
        }
        operations
    };
    assert_eq!(operations.len(), ledger.outcomes.len());
    assert!(db.active_git_push_operation(&repo.id).unwrap().is_none());
    assert_eq!(
        db.latest_git_push_operation(&repo.id).unwrap().as_ref(),
        operations.last()
    );

    // Preserve the original bytes as well as the r2 identity used by Git admission.
    let handled_git_baseline = db
        .get_state(&format!("handled_git_baseline_{}", repo.id))
        .unwrap()
        .unwrap();
    let baseline: Value = serde_json::from_str(&handled_git_baseline).unwrap();
    let r2 = &ledger.sync_records[0];
    assert_eq!(r2["revision"], 2);
    assert_eq!(r2["status"], "applied");
    assert_eq!(r2["direction"], "svn_to_git");
    assert_eq!(baseline["version"], 1);
    assert_eq!(baseline["repo_id"], repo.id);
    assert_eq!(baseline["svn_rev"], r2["revision"]);
    assert_eq!(baseline["git_sha"], r2["sha"]);
    assert_eq!(baseline["git_sha"], operations[0].intended_local_git_sha);
    let projection: Value = serde_json::from_str(baseline["projection"].as_str().unwrap()).unwrap();
    assert_eq!(
        projection,
        json!({"allowed_paths": [], "blocked_patterns": []})
    );

    let bare = git2::Repository::open_bare(&repo.bare).unwrap();
    let (tip, tree) = git_main_commit_and_tree(&repo.bare);
    assert_eq!(git_main_commit_and_tree(&repo.bridge), (tip, tree));
    assert_eq!(git_symbolic_ref(&repo.bridge, "HEAD"), "main");
    let mut parent = repo.initial_git_sha.clone();
    let mut expected_ref_updates = Vec::new();
    for revision in 2..=youngest {
        let (path, bytes, message) = FINALIZED_CHANGES[(revision - 2) as usize];
        let rev = revision.to_string();
        assert_eq!(svnlook_at(&svn, "log", &["-r", &rev]).trim_end(), message);
        assert_eq!(
            svnlook_at(&svn, "changed", &["-r", &rev]).trim_end(),
            format!("A   {path}")
        );
        assert_eq!(svnlook_at(&svn, "cat", &["-r", &rev, path]), bytes);
        if revision > published {
            assert!(git_path_missing(&repo.bare, "main", path));
            continue;
        }
        let op = &operations[(revision - 2) as usize];
        let outcome = &ledger.outcomes[(revision - 2) as usize];
        let sha = &op.intended_local_git_sha;
        let commit = bare.find_commit(sha.parse().unwrap()).unwrap();
        assert_eq!(op.state, GitPushOperationState::Completed);
        assert_eq!(op.source_svn_rev, revision);
        assert_eq!(op.initiator_id, "team_worker");
        assert_eq!(op.request_id, format!("svn-r{revision}"));
        assert_eq!(op.pre_push_git_remote, "origin");
        assert_eq!(op.pre_push_git_branch, "main");
        assert_eq!(op.pre_push_git_sha, parent);
        assert_eq!(op.intended_local_git_parent.as_ref(), Some(&parent));
        assert_eq!(op.intended_local_git_tree, commit.tree_id().to_string());
        assert_eq!(commit.parent_count(), 1);
        assert_eq!(commit.parent_id(0).unwrap().to_string(), parent);
        assert_eq!(op.last_confirmed_git_sha.as_ref(), Some(sha));
        assert_eq!(
            op.last_confirmed_git_tree,
            Some(commit.tree_id().to_string())
        );
        assert_eq!(outcome["key"], svn_source_key(revision));
        assert_eq!(outcome["direction"], "svn_to_git");
        assert_eq!(outcome["outcome"], "applied_and_verified");
        assert_eq!(&outcome["sha"], sha.as_str());
        assert_eq!(git_show_blob(&repo.bare, sha, path), bytes);
        assert_eq!(git_show_blob(&repo.bare, "main", path), bytes);
        expected_ref_updates.push(format!("{parent} {sha} "));
        parent = sha.clone();
    }
    assert_eq!(tip.to_string(), parent);
    assert_eq!(
        ledger.svn_checkpoint.handled_source_key,
        svn_source_key(published)
    );
    assert_eq!(ledger.svn_checkpoint.target_git_sha.as_ref(), Some(&parent));
    assert_eq!(
        ledger.svn_checkpoint.outcome,
        MappingOutcome::AppliedAndVerified
    );
    assert_eq!(git_show_blob(&repo.bare, "main", ".gitkeep"), "");
    let bare_reflog = std::fs::read_to_string(repo.bare.join("logs/refs/heads/main")).unwrap();
    assert_eq!(bare_reflog.lines().count(), expected_ref_updates.len());
    for (line, expected) in bare_reflog.lines().zip(&expected_ref_updates) {
        assert!(line.starts_with(expected), "{line}");
    }
    let registration = db.get_repository(&repo.id).unwrap().unwrap();
    FinalizedHistory {
        ledger,
        operations,
        mirror: db.get_state(&format!("last_svn_rev_{}", repo.id)).unwrap(),
        handled_git_baseline,
        bare_reflog,
        svn_tree: svnlook_at(&svn, "tree", &["--show-ids", "--full-paths"]),
        total_syncs: registration.total_syncs,
        registration_mode: registration.sync_mode,
    }
}

fn queue_finalized_change(f: &SingleRepoFixture, revision: i64) {
    let (path, bytes, message) = FINALIZED_CHANGES[(revision - 2) as usize];
    assert_eq!(
        svn_commit_file(
            &f.wc,
            path,
            bytes,
            message,
            &f.repo.username,
            &f.repo.password
        ),
        revision
    );
}

// The managed scheduler uses the service SyncConfig independently of the registration's
// Git push strategy. It sets the registered branch and passes the repository ID to SyncEngine.
fn managed_engine(f: &SingleRepoFixture) -> SyncEngine {
    let db = Database::new(&f.db_path).unwrap();
    let registration = db.get_repository(&f.repo.id).unwrap().unwrap();
    let mut config = make_app_config(&f.repo.svn_url, f.tmp.path());
    assert_eq!(config.sync.mode, SyncMode::Direct);
    config.svn.trunk_path.clear();
    config.github.default_branch = registration.git_branch;
    f.make_engine_with_config(db, config)
}

async fn finalized_r3_fixture(registration_mode: &str) -> SingleRepoFixture {
    let f = SingleRepoFixture::with_registration_mode(registration_mode);
    git_cli(&f.repo.bare, &["config", "core.logAllRefUpdates", "true"]);
    assert_eq!(
        f.checkpoint_snapshot(),
        (1, f.repo.initial_git_sha.clone(), None)
    );
    let engine = managed_engine(&f);
    for rev in 2..=3 {
        if rev == 3 {
            queue_finalized_change(&f, rev);
        }
        let stats = engine.run_sync_cycle().await.unwrap();
        assert_eq!((stats.svn_to_git_count, stats.git_to_svn_count), (1, 0));
    }
    f
}

fn verify_finalized_receipts(history: &FinalizedHistory) {
    assert_eq!(history.ledger.sync_records.len(), history.operations.len());
    for (r, op) in history.ledger.sync_records.iter().zip(&history.operations) {
        assert!(r["id"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(r["direction"], "svn_to_git");
        assert_eq!(r["status"], "applied");
        assert_eq!(r["revision"], op.source_svn_rev);
        assert_eq!(r["sha"], op.intended_local_git_sha);
    }
    let latest = history.operations.last().unwrap();
    assert_eq!(
        history.ledger.watermark,
        (latest.source_svn_rev, latest.intended_local_git_sha.clone())
    );
    assert_eq!(history.mirror, Some(latest.source_svn_rev.to_string()));
    assert_eq!(history.total_syncs, history.operations.len() as i64);
}

fn total_changes(db: &Database) -> i64 {
    db.conn()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

fn remove_finalized_mirror(f: &SingleRepoFixture) {
    let db = Database::new(&f.db_path).unwrap();
    assert_eq!(
        db.conn()
            .execute(
                "DELETE FROM kv_state WHERE key=?1",
                [format!("last_svn_rev_{}", f.repo.id)]
            )
            .unwrap(),
        1
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Damage {
    RewoundPairAndDeletedReceipt,
    RewoundSvnAndMissingMirror,
}

async fn history_hold(damage: Damage, registration_mode: &str) -> Value {
    let f = finalized_r3_fixture(registration_mode).await;
    // The historical case had just r3. Queue a real child for the distinct
    // missing-mirror case so a falsely admitted sync would have work to do.
    let youngest = if damage == Damage::RewoundPairAndDeletedReceipt {
        3
    } else {
        queue_finalized_change(&f, 4);
        4
    };
    let mut expected = snapshot(&f, 3, youngest);
    verify_finalized_receipts(&expected);
    assert_eq!(expected.registration_mode, registration_mode);
    let baseline = f.repo.initial_git_sha.clone();
    let db = Database::new(&f.db_path).unwrap();
    let (baseline_reason, cycle_reason) = if damage == Damage::RewoundPairAndDeletedReceipt {
        // Preserve both mutations from the original damaged-finalized drill.
        assert_eq!(
            db.conn()
                .execute(
                    "UPDATE repositories SET last_svn_rev=1, last_git_sha=?1 WHERE id=?2",
                    rusqlite::params![baseline, f.repo.id]
                )
                .unwrap(),
            1
        );
        let removed = expected.ledger.sync_records.pop().unwrap();
        assert_eq!(removed["revision"], 3);
        let deleted = db
            .conn()
            .execute(
                "DELETE FROM sync_records
             WHERE repo_id = ?1 AND svn_rev = 3 AND direction = 'svn_to_git'",
                [&f.repo.id],
            )
            .unwrap();
        assert_eq!(deleted, 1);
        expected.ledger.watermark.1 = baseline;
        ("conflicting_import_checkpoint", "ambiguous_checkpoint")
    } else {
        assert_eq!(
            db.conn()
                .execute(
                    "UPDATE repositories SET last_svn_rev=1 WHERE id=?1",
                    [&f.repo.id]
                )
                .unwrap(),
            1
        );
        remove_finalized_mirror(&f);
        expected.mirror = None;
        (
            "conflicting_finalized_svn_checkpoint",
            "conflicting_finalized_svn_checkpoint",
        )
    };
    expected.ledger.watermark.0 = 1;
    assert_eq!(snapshot(&f, 3, youngest), expected);
    let changes_before = total_changes(&db);
    assert!(!repo_import_baseline_is_pending_read_only(&db, &f.repo.id).unwrap());
    assert_eq!(total_changes(&db), changes_before);
    assert_eq!(snapshot(&f, 3, youngest), expected);
    let resolved = resolve_repo_import_baseline(&db, &f.repo.id).unwrap();
    let RepoImportBaseline::ReconciliationRequired { reason, .. } = resolved else {
        panic!("expected held baseline, got {resolved:?}");
    };
    assert_eq!(reason, baseline_reason);
    assert_eq!(total_changes(&db), changes_before);
    assert_eq!(snapshot(&f, 3, youngest), expected);
    drop(db);

    let mut recovery_error = String::new();
    for attempt in 1..=2 {
        let error = managed_engine(&f).run_sync_cycle().await.unwrap_err();
        assert!(
            matches!(&error, SyncError::HistoryBlocked { reason, .. } if reason == cycle_reason),
            "{error}"
        );
        recovery_error = error.to_string();
        assert_eq!(snapshot(&f, 3, youngest), expected);
        let reg = Database::new(&f.db_path)
            .unwrap()
            .get_repository(&f.repo.id)
            .unwrap()
            .unwrap();
        assert_eq!(reg.total_errors, attempt);
        assert_eq!(reg.sync_status, "reconciliation_required");
    }
    let r3 = &expected.operations[1];
    let r3_receipts: Vec<_> = expected
        .ledger
        .sync_records
        .iter()
        .filter(|r| r["revision"] == 3)
        .collect();
    let retained_receipt_ids: Vec<_> = expected
        .ledger
        .sync_records
        .iter()
        .map(|r| &r["id"])
        .collect();
    json!({
        "status": "PASS",
        "baseline_reason": baseline_reason,
        "cycle_reason": cycle_reason,
        "registration_mode": expected.registration_mode,
        "push_initiator": r3.initiator_id,
        "held_cycles": 2,
        "recovery_error": recovery_error,
        "repo_svn_rev": expected.ledger.watermark.0,
        "scoped_svn_mirror": expected.mirror,
        "canonical_svn_rev": r3.source_svn_rev,
        "svn_youngest": youngest,
        "svn_uuid": expected.ledger.lineage.svn_uuid,
        "scope_uuid": expected.ledger.lineage.scope_uuid,
        "finalized_operation_id": r3.id,
        "published_git_sha": r3.intended_local_git_sha,
        "canonical_outcome_id": expected.ledger.outcomes[1]["id"],
        "retained_receipt_ids": retained_receipt_ids,
        "handled_git_baseline_svn_rev": expected.ledger.sync_records[0]["revision"],
        "handled_git_baseline_sha": expected.ledger.sync_records[0]["sha"],
        "r3_receipts": r3_receipts.len(),
        "mapped_git_sha": r3_receipts.first().map(|r| &r["sha"]),
        "bare_head_stable": true,
        "ref_updates_during_hold": 0
    })
}

async fn mirror_recovery() -> Value {
    let f = finalized_r3_fixture("direct").await;
    queue_finalized_change(&f, 4);
    let mut before = snapshot(&f, 3, 4);
    verify_finalized_receipts(&before);
    assert_eq!(before.registration_mode, "direct");
    remove_finalized_mirror(&f);
    before.mirror = None;
    assert_eq!(snapshot(&f, 3, 4), before);
    let db = Database::new(&f.db_path).unwrap();
    let changes_before = total_changes(&db);
    assert!(!repo_import_baseline_is_pending_read_only(&db, &f.repo.id).unwrap());
    assert_eq!(total_changes(&db), changes_before);
    assert_eq!(snapshot(&f, 3, 4), before);
    assert_eq!(
        resolve_repo_import_baseline(&db, &f.repo.id).unwrap(),
        RepoImportBaseline::Verified {
            svn_rev: 3,
            git_sha: before.ledger.watermark.1.clone()
        }
    );
    assert_eq!(total_changes(&db), changes_before + 1);
    before.mirror = Some("3".into());
    assert_eq!(
        snapshot(&f, 3, 4),
        before,
        "only the missing SVN mirror may change"
    );
    drop(db);
    let stats = managed_engine(&f).run_sync_cycle().await.unwrap();
    assert_eq!((stats.svn_to_git_count, stats.git_to_svn_count), (1, 0));
    let after = snapshot(&f, 4, 4);
    verify_finalized_receipts(&after);
    assert_eq!(after.ledger.lineage, before.ledger.lineage);
    assert_eq!(after.svn_tree, before.svn_tree);
    assert_eq!(after.handled_git_baseline, before.handled_git_baseline);
    assert!(after
        .ledger
        .sync_records
        .starts_with(&before.ledger.sync_records));
    assert!(after.ledger.outcomes.starts_with(&before.ledger.outcomes));
    assert!(after.operations.starts_with(&before.operations));
    assert_eq!(
        after
            .bare_reflog
            .strip_prefix(&before.bare_reflog)
            .unwrap()
            .lines()
            .count(),
        1
    );
    for _ in 0..2 {
        let quiet = managed_engine(&f).run_sync_cycle().await.unwrap();
        assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
        assert_eq!(snapshot(&f, 4, 4), after);
    }
    let r3 = &before.operations[1];
    let child = &after.operations[2];
    json!({
        "status": "PASS",
        "repaired_svn_revision": r3.source_svn_rev,
        "registration_mode": before.registration_mode,
        "push_initiator": r3.initiator_id,
        "child_svn_revision": child.source_svn_rev,
        "svn_uuid": before.ledger.lineage.svn_uuid,
        "scope_uuid": before.ledger.lineage.scope_uuid,
        "finalized_operation_id": r3.id,
        "finalized_receipt_id": before.ledger.sync_records[1]["id"],
        "finalized_git_sha": r3.intended_local_git_sha,
        "child_git_sha": child.intended_local_git_sha,
        "child_operation_id": child.id,
        "child_receipt_id": after.ledger.sync_records[2]["id"],
        "child_canonical_outcome_id": after.ledger.outcomes[2]["id"],
        "bare_ref_updates_for_child": 1,
        "repeat_noop_cycles": 2
    })
}

pub(super) async fn evidence() -> Value {
    let historical = history_hold(Damage::RewoundPairAndDeletedReceipt, "direct").await;
    let mirror = mirror_recovery().await;
    let mut contradictory = history_hold(Damage::RewoundSvnAndMissingMirror, "direct").await;
    let pr_registration = history_hold(Damage::RewoundSvnAndMissingMirror, "pr").await;
    assert_ne!(contradictory["scope_uuid"], pr_registration["scope_uuid"]);
    assert_ne!(contradictory["svn_uuid"], pr_registration["svn_uuid"]);
    contradictory["pr_registration"] = pr_registration;
    json!({
        "historical_hold": historical,
        "mirror_recovery": mirror,
        "contradictory_mirror_hold": contradictory
    })
}

// Candidate acceptance for the preserved R23 failing baseline. The fixture
// restores only its own synthetic pre-write database into a separate directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r23_restored_database_admission() {
    if !super::require_toolchain("R23_RESTORED_DATABASE_ADMISSION") {
        return;
    }

    // Two separate fixture lanes prevent an unexpected direct-cycle write from
    // affecting what the scheduler's baseline resolver observes.
    let mut reports = Vec::new();
    let mut safe_disposition = true;
    for driver in ["scheduler_baseline_resolver", "direct_engine"] {
        let f = SingleRepoFixture::with_registration_mode("direct");
        git_cli(&f.repo.bare, &["config", "core.logAllRefUpdates", "true"]);
        let first = managed_engine(&f).run_sync_cycle().await.unwrap();
        assert_eq!((first.svn_to_git_count, first.git_to_svn_count), (1, 0));
        let prewrite = snapshot(&f, 2, 2);
        verify_finalized_receipts(&prewrite);
        let prewrite_tip = git_main_commit_and_tree(&f.repo.bare);

        // VACUUM INTO obtains a transactionally consistent database including
        // committed WAL content. Never copy the live main DB file in isolation.
        let sealed = f.tmp.path().join("sealed-r2.sqlite");
        let db = Database::new(&f.db_path).unwrap();
        db.conn()
            .execute("VACUUM INTO ?1", [sealed.to_str().unwrap()])
            .unwrap();
        drop(db);
        let seal = std::fs::read(&sealed).unwrap();
        let restored = f.tmp.path().join("restored");
        std::fs::create_dir(&restored).unwrap();
        let restored_db_path = restored.join("sync.db");
        let restored_bridge = restored.join("bridge");
        git_cli(
            f.tmp.path(),
            &[
                "clone",
                "--no-hardlinks",
                "--branch",
                "main",
                f.repo.bare.to_str().unwrap(),
                restored_bridge.to_str().unwrap(),
            ],
        );
        assert_eq!(git_main_commit_and_tree(&restored_bridge), prewrite_tip);

        queue_finalized_change(&f, 3);
        let live = managed_engine(&f);
        let published = live.run_sync_cycle().await.unwrap();
        assert_eq!(
            (published.svn_to_git_count, published.git_to_svn_count),
            (1, 0)
        );
        let idle = live.run_sync_cycle().await.unwrap();
        assert_eq!((idle.svn_to_git_count, idle.git_to_svn_count), (0, 0));
        drop(live);
        let genuine = snapshot(&f, 3, 3);
        verify_finalized_receipts(&genuine);
        let genuine_receipt = &genuine.operations[1];
        assert_ne!(&prewrite.operations[0].id, &genuine_receipt.id);

        // This restores the complete pre-write DB into an independent state
        // directory. The live installation and both remotes remain in place.
        std::fs::copy(&sealed, &restored_db_path).unwrap();
        assert_eq!(std::fs::read(&restored_db_path).unwrap(), seal);
        assert_eq!(std::fs::read(&sealed).unwrap(), seal);

        let repo_id = &f.repo.id;
        let svn_dir = f.tmp.path().join("svnserve-root").join(repo_id);
        let reflog = f.repo.bare.join("logs/refs/heads/main");
        let remote_witness = || {
            (
                git_main_commit_and_tree(&f.repo.bare),
                std::fs::read(&reflog).unwrap(),
                svnlook_at(&svn_dir, "uuid", &[]),
                svnlook_at(&svn_dir, "youngest", &[]),
                svnlook_at(&svn_dir, "tree", &["--show-ids", "--full-paths"]),
                svnlook_at(&svn_dir, "log", &["-r", "3"]),
                svnlook_at(&svn_dir, "cat", &["-r", "3", "recovery.txt"]),
            )
        };
        let remote_after_publish = remote_witness();
        let (published_tip, published_tree) = remote_after_publish.0;
        assert_eq!(
            published_tip.to_string(),
            genuine_receipt.intended_local_git_sha
        );
        assert_eq!(
            published_tree.to_string(),
            genuine_receipt.intended_local_git_tree
        );

        let old_db_witness = |db: &Database| {
            (
                super::outage_ledger_for_db(db, repo_id),
                db.get_state(&format!("last_svn_rev_{repo_id}")).unwrap(),
                db.get_state(&format!("handled_git_baseline_{repo_id}"))
                    .unwrap(),
                git_push_documents(db, repo_id),
                db.active_git_push_operation(repo_id).unwrap(),
                db.latest_git_push_operation(repo_id).unwrap(),
            )
        };
        let expected_db = (
            prewrite.ledger,
            prewrite.mirror,
            Some(prewrite.handled_git_baseline),
            prewrite.operations.clone(),
            None,
            prewrite.operations.last().cloned(),
        );

        // Capture entry and exit independently. The second attempt reopens the
        // same restored database and checkout, not the original live bridge.
        let bridge_probe = || {
            let git = git2::Repository::open(&restored_bridge).unwrap();
            let head = git.head().unwrap();
            let commit = head.peel_to_commit().unwrap();
            let main_log_entries = match git.reflog("refs/heads/main") {
                Ok(log) => Some(log.len()),
                Err(e) if e.code() == git2::ErrorCode::NotFound => None,
                Err(e) => panic!("cannot inspect synthetic main reflog: {:?}", e.code()),
            };
            json!({
                "head_on_main": head.name() == Some("refs/heads/main"),
                "head_sha": commit.id().to_string(),
                "head_tree": commit.tree_id().to_string(),
                "head_is_published": (commit.id(), commit.tree_id()) == (published_tip, published_tree),
                "head_is_prewrite": (commit.id(), commit.tree_id()) == prewrite_tip,
                "main_reflog_entries": main_log_entries,
            })
        };
        let conflict_probe = |db: &Database| {
            let blocking_rows = db.count_conflicts_blocking_apply_for_repo(repo_id).unwrap();
            let first = db.first_conflict_blocking_apply_for_repo(repo_id).unwrap();
            json!({
                "blocking_rows": blocking_rows,
                "first_blocking_row_id": first.as_ref().map(|(id, _)| id),
                "first_blocking_row_is_fixture_path": first
                    .as_ref()
                    .is_some_and(|(_, path)| path == "recovery.txt"),
            })
        };
        for attempt in 1..=2 {
            let db = Database::new(&restored_db_path).unwrap();
            let before = old_db_witness(&db) == expected_db;
            let bridge_before = bridge_probe();
            let index_before = std::fs::read(restored_bridge.join(".git/index")).unwrap();
            let conflicts_before = conflict_probe(&db);
            let (outcome, expected_decision) = if driver == "scheduler_baseline_resolver" {
                // This is the production admission resolver the scheduler
                // calls; it is not a full scheduler tick. A coherent but old
                // local baseline may verify here, so the engine lane is
                // independently required to check the real remotes.
                let result = resolve_repo_import_baseline(&db, repo_id);
                drop(db);
                match result {
                    Ok(RepoImportBaseline::Verified { svn_rev, git_sha }) => {
                        let consistent = (svn_rev, git_sha.as_str())
                            == (
                                expected_db.0.watermark.0,
                                expected_db.0.watermark.1.as_str(),
                            );
                        (
                            json!({"decision": "verified_local", "svn_revision": svn_rev}),
                            consistent,
                        )
                    }
                    Ok(RepoImportBaseline::ReconciliationRequired { reason, .. }) => (
                        json!({"decision": "reconciliation_required", "reason": reason}),
                        false,
                    ),
                    Ok(RepoImportBaseline::Pending) => (json!({"decision": "pending"}), false),
                    Err(_) => (json!({"decision": "resolver_error"}), false),
                }
            } else {
                let mut config = make_app_config(&f.repo.svn_url, &restored);
                config.svn.trunk_path.clear();
                config.github.default_branch = "main".into();
                let mut engine = SyncEngine::new(
                    config,
                    db,
                    reposync_core::svn::SvnClient::new(
                        &f.repo.svn_url,
                        &f.repo.username,
                        &f.repo.password,
                    ),
                    reposync_core::git::GitClient::new(&restored_bridge).unwrap(),
                    std::sync::Arc::new(super::make_identity_mapper()),
                );
                engine.set_repo_id(repo_id.clone());
                match engine.run_sync_cycle().await {
                    Err(SyncError::HistoryBlocked { reason, .. }) => (
                        json!({"decision": "history_blocked", "reason": reason}),
                        reason == "unreceipted_svn_publication",
                    ),
                    Err(error) => {
                        // Never print arbitrary remote messages, paths or credentials.
                        // Match exhaustively so new variants require an explicit safe label.
                        let kind = match &error {
                            SyncError::AlreadyRunning { .. } => "already_running",
                            SyncError::HistoryBlocked { .. } => "history_blocked",
                            SyncError::SvnCommitHeld { .. } => "svn_commit_held",
                            SyncError::GitPushHeld { .. } => "git_push_held",
                            SyncError::CycleMappingHeld { .. } => "cycle_mapping_held",
                            SyncError::UnresolvableConflict { .. } => "unresolvable_conflict",
                            SyncError::EchoDetectionFailed { .. } => "echo_detection_failed",
                            SyncError::InvalidStateTransition { .. } => "invalid_state_transition",
                            SyncError::SvnError(_) => "svn_error",
                            SyncError::GitError(_) => "git_error",
                            SyncError::GitHubError(_) => "github_error",
                            SyncError::DatabaseError(_) => "database_error",
                            SyncError::IdentityError(_) => "identity_error",
                        };
                        let on_recovery_path = matches!(
                            &error,
                            SyncError::UnresolvableConflict { file_path, .. } if file_path == "recovery.txt"
                        );
                        (
                            json!({
                                "decision": "other_engine_error",
                                "error_kind": kind,
                                "fixture_path_is_recovery_txt": on_recovery_path,
                            }),
                            false,
                        )
                    }
                    Ok(stats) => (
                        json!({
                            "decision": "cycle_completed",
                            "svn_to_git_count": stats.svn_to_git_count,
                            "git_to_svn_count": stats.git_to_svn_count
                        }),
                        false,
                    ),
                }
            };
            // Reopen after each operation; check all scoped receipt rows and
            // IDs, not just reported counters or the final Git tree.
            let reopened = Database::new(&restored_db_path).unwrap();
            let after = old_db_witness(&reopened);
            let conflicts_after = conflict_probe(&reopened);
            let bridge_after = bridge_probe();
            drop(reopened);
            let remote_stable = remote_witness() == remote_after_publish;
            let bridge_stable = git_main_commit_and_tree(&restored_bridge) == prewrite_tip;
            let ledger_stable = after == expected_db;
            let checkout_unchanged = bridge_before == bridge_after
                && bridge_before["head_on_main"] == true
                && bridge_before["head_is_prewrite"] == true
                && bridge_before["main_reflog_entries"].as_u64().is_some()
                && std::fs::read(restored_bridge.join(".git/index")).unwrap() == index_before
                && !restored_bridge.join("recovery.txt").exists();
            let no_conflicts =
                conflicts_before == conflicts_after && conflicts_after["blocking_rows"] == 0;
            let accepted = before
                && remote_stable
                && bridge_stable
                && ledger_stable
                && checkout_unchanged
                && no_conflicts
                && expected_decision;
            safe_disposition &= accepted;
            reports.push(json!({
                "driver": driver,
                "cold_attempt": attempt,
                "outcome": outcome,
                "bridge_at_entry": bridge_before,
                "bridge_at_exit": bridge_after,
                "scoped_conflicts_at_entry": conflicts_before,
                "scoped_conflicts_at_exit": conflicts_after,
                "prewrite_ledger_at_entry": before,
                "restored_ledger_unchanged": ledger_stable,
                "remotes_unchanged": remote_stable,
                "restored_bridge_unchanged": bridge_stable && checkout_unchanged,
                "genuine_receipt_id": genuine_receipt.id,
                "genuine_receipt_svn_revision": genuine_receipt.source_svn_rev,
                "genuine_git_sha": genuine_receipt.intended_local_git_sha,
                "restored_receipt_ids": after.3.iter().map(|op| &op.id).collect::<Vec<_>>(),
                "restored_has_genuine_receipt": after.3.iter().any(|op| op.id == genuine_receipt.id),
            }));
        }
        assert_eq!(std::fs::read(&sealed).unwrap(), seal);
        let live_db = Database::new(&f.db_path).unwrap();
        assert_eq!(git_push_documents(&live_db, repo_id), genuine.operations);
        assert_eq!(
            super::outage_ledger_for_db(&live_db, repo_id),
            genuine.ledger
        );
    }
    super::emit_evidence(
        "R23_RESTORED_DATABASE_ADMISSION",
        if safe_disposition { "PASS" } else { "FAIL" },
        json!({"expected_safe_disposition": safe_disposition, "cases": reports}),
    );
    assert!(
        safe_disposition,
        "R23 must hold before replay, without advancing either remote or the restored checkout"
    );
}

/// Independent Git/SVN edits must not be mistaken for a restored receipt.
/// Give the strongest negative the same emitter fields with different bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_r23_ordinary_admission_controls() {
    const CASE: &str = "R23_ORDINARY_ADMISSION_CONTROLS";
    if !super::require_toolchain(CASE) {
        return;
    }

    let controls = [
        "plain_fast_forward",
        "marker_only",
        "ordinary_conflict",
        "exact_hint_wrong_bytes",
        "mixed_disjoint",
    ];
    for control in controls {
        let fixture = SingleRepoFixture::with_registration_mode("direct");
        let repo = &fixture.repo;
        git_cli(&repo.bare, &["config", "core.logAllRefUpdates", "true"]);
        let engine = managed_engine(&fixture);
        let initial = engine.run_sync_cycle().await.unwrap();
        assert_eq!((initial.svn_to_git_count, initial.git_to_svn_count), (1, 0));
        let before = snapshot(&fixture, 2, 2);
        verify_finalized_receipts(&before);
        let before_bridge = git_main_commit_and_tree(&repo.bridge);
        let expect_conflict = matches!(control, "ordinary_conflict" | "exact_hint_wrong_bytes");
        let disjoint = control == "mixed_disjoint";
        if expect_conflict || disjoint {
            queue_finalized_change(&fixture, 3);
        }

        git_cli(&fixture.developer, &["pull", "--ff-only", "origin", "main"]);
        let path = if expect_conflict {
            "recovery.txt"
        } else {
            "ordinary.txt"
        };
        let payload = "independent Git content\n";
        let message = match control {
            "marker_only" => "User work\n\n[reposync] synced from SVN r3".into(),
            "exact_hint_wrong_bytes" => format!(
                "{}\n\n[reposync] synced from SVN r3",
                FINALIZED_CHANGES[1].2
            ),
            _ => "Independent Git work".into(),
        };
        std::fs::write(fixture.developer.join(path), payload).unwrap();
        let developer = reposync_core::git::GitClient::new(&fixture.developer).unwrap();
        let commit = developer
            .commit_via_cli(
                &message,
                "Test User",
                "test@example.com",
                "reposync",
                "sync@reposync.local",
            )
            .unwrap();
        git_cli(&fixture.developer, &["push", "origin", "main"]);
        assert_eq!(git_main_commit_and_tree(&repo.bare).0, commit);
        let reflog_path = repo.bare.join("logs/refs/heads/main");
        let reflog_before = std::fs::read(&reflog_path).unwrap();
        let result = engine.run_sync_cycle().await;
        let db = Database::new(&fixture.db_path).unwrap();
        let svn_dir = fixture.tmp.path().join("svnserve-root").join(&repo.id);
        if expect_conflict {
            assert!(
                matches!(
                    result, Err(SyncError::UnresolvableConflict { file_path, .. }) if file_path == path
                ),
                "{control} must be an ordinary conflict"
            );
            assert_eq!(git_main_commit_and_tree(&repo.bridge), before_bridge);
            assert!(!repo.bridge.join(path).exists());
            assert_eq!(super::outage_ledger_for_db(&db, &repo.id), before.ledger);
            assert_eq!(
                db.count_conflicts_blocking_apply_for_repo(&repo.id)
                    .unwrap(),
                1
            );
            assert_eq!(
                svnlook_at(&svn_dir, "cat", &["-r", "3", path]),
                FINALIZED_CHANGES[1].1
            );
        } else if disjoint {
            let stats = result.unwrap();
            assert_eq!((stats.svn_to_git_count, stats.git_to_svn_count), (1, 1));
            let published = git_main_commit_and_tree(&repo.bare);
            assert_ne!(published.0, commit);
            assert_eq!(git_main_commit_and_tree(&repo.bridge), published);
            assert_eq!(git_show_blob(&repo.bare, "main", path), payload);
            assert_eq!(
                git_show_blob(&repo.bare, "main", "recovery.txt"),
                FINALIZED_CHANGES[1].1,
            );
            assert_eq!(svnlook_at(&svn_dir, "cat", &["-r", "4", path]), payload);
            assert_eq!(svnlook_at(&svn_dir, "youngest", &[]).trim(), "4");
            assert_eq!(
                db.count_conflicts_blocking_apply_for_repo(&repo.id)
                    .unwrap(),
                0
            );
            let after = std::fs::read(reflog_path).unwrap();
            let appended = after.strip_prefix(reflog_before.as_slice()).unwrap();
            assert_eq!(
                appended
                    .split(|b| *b == b'\n')
                    .filter(|s| !s.is_empty())
                    .count(),
                1
            );
            assert!(String::from_utf8_lossy(appended)
                .starts_with(&format!("{commit} {} ", published.0)));
            let noop = engine.run_sync_cycle().await.unwrap();
            assert_eq!((noop.svn_to_git_count, noop.git_to_svn_count), (0, 0));
            continue;
        } else {
            let stats = result.unwrap();
            assert_eq!((stats.svn_to_git_count, stats.git_to_svn_count), (0, 1));
            assert_eq!(git_main_commit_and_tree(&repo.bridge).0, commit);
            assert_eq!(svnlook_at(&svn_dir, "cat", &["-r", "3", path]), payload);
            assert_eq!(
                db.count_conflicts_blocking_apply_for_repo(&repo.id)
                    .unwrap(),
                0
            );
            let noop = engine.run_sync_cycle().await.unwrap();
            assert_eq!((noop.svn_to_git_count, noop.git_to_svn_count), (0, 0));
        }
        assert_eq!(svnlook_at(&svn_dir, "youngest", &[]).trim(), "3");
        assert_eq!(git_main_commit_and_tree(&repo.bare).0, commit);
        assert_eq!(std::fs::read(reflog_path).unwrap(), reflog_before);
    }
    super::emit_evidence(CASE, "PASS", json!({"controls": controls}));
}
