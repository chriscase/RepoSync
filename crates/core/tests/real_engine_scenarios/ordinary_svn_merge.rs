//! A managed trunk consumes ordinary SVN merges made by a separate working copy.
use super::{
    git_cli, git_main_commit_and_tree, git_path_missing, git_show_blob, make_app_config,
    outage_ledger, svn_checkout, svn_commit_file, svn_source_key, svnlook_at, Database,
    MappingOutcome, OutageLedger, SingleRepoFixture, SyncEngine,
};
use reposync_core::db::git_push_operations::GitPushOperationState;
use reposync_core::db::team_cycle_mapping_operations::{
    TeamCycleMappingDirection, TeamCycleMappingOperation, TeamCycleMappingOutcome,
    TeamCycleMappingState,
};
use reposync_core::echo_receipt_scope::{
    handled_svn_no_target_state_key, read_svn_no_target_receipt,
};
use reposync_core::svn::SvnClient;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const MERGED: &str = "source change for the ordinary merge\n";
const RECORDED: &str = "only in the feature working copy\n";
const CHILD: &str = "ordinary work after the record-only merge\n";

fn svn(f: &SingleRepoFixture, args: &[&str]) {
    let output = Command::new("svn")
        .current_dir(&f.wc)
        .args(args)
        .args([
            "--username",
            &f.repo.username,
            "--password",
            &f.repo.password,
            "--non-interactive",
            "--no-auth-cache",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svn {}: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr)
    );
}

fn managed_engine(f: &SingleRepoFixture) -> SyncEngine {
    let db = Database::new(&f.db_path).unwrap();
    let row = db.get_repository(&f.repo.id).unwrap().unwrap();
    assert_eq!(row.svn_branch, "trunk");
    assert_eq!(row.sync_mode, "direct");
    let target =
        reposync_core::svn::operand::configured_branch_url(&row.svn_url, &row.svn_branch).unwrap();
    assert_eq!(target, f.repo.svn_url);
    let mut config = make_app_config(&row.svn_url, f.tmp.path());
    config.svn.trunk_path.clear();
    config.github.default_branch = row.git_branch;
    f.make_engine_with_config(db, config)
}

fn youngest(repo: &Path) -> i64 {
    svnlook_at(repo, "youngest", &[]).trim().parse().unwrap()
}

fn assert_revision(repo: &Path, rev: i64, message: &str, changed_paths: &[&str]) {
    let rev = rev.to_string();
    assert_eq!(svnlook_at(repo, "log", &["-r", &rev]).trim_end(), message);
    let changes = svnlook_at(repo, "changed", &["-r", &rev]);
    assert_eq!(
        changes.lines().collect::<BTreeSet<_>>(),
        changed_paths.iter().copied().collect()
    );
}

fn file_at(repo: &Path, rev: i64, path: &str) -> String {
    svnlook_at(repo, "cat", &["-r", &rev.to_string(), path])
}

fn mergeinfo(repo: &Path, rev: i64) -> String {
    svnlook_at(
        repo,
        "propget",
        &["-r", &rev.to_string(), "svn:mergeinfo", "trunk"],
    )
}

fn files_at(f: &SingleRepoFixture, expected_files: &[(&str, &str)]) {
    let (tip, tree) = git_main_commit_and_tree(&f.repo.bare);
    assert_eq!(git_main_commit_and_tree(&f.repo.bridge), (tip, tree));
    let bare = git2::Repository::open_bare(&f.repo.bare).unwrap();
    let refs = bare
        .references()
        .unwrap()
        .names()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join(" ");
    assert_eq!(refs, "refs/heads/main");
    assert_eq!(bare.find_tree(tree).unwrap().len(), expected_files.len());
    for (path, bytes) in expected_files {
        assert_eq!(git_show_blob(&f.repo.bare, "refs/heads/main", path), *bytes);
        assert_eq!(
            std::fs::read_to_string(f.repo.bridge.join(path)).unwrap(),
            *bytes
        );
    }
}

fn reflog(f: &SingleRepoFixture) -> String {
    std::fs::read_to_string(f.repo.bare.join("logs/refs/heads/main")).unwrap()
}

// Read every scoped document, including its ID and timestamps. A stable latest
// pointer alone cannot exclude an extra terminal operation.
fn journal_documents(f: &SingleRepoFixture, pattern: &str) -> Vec<String> {
    let db = Database::new(&f.db_path).unwrap();
    let conn = db.conn();
    let mut docs = conn
        .prepare(
            "SELECT value FROM kv_state WHERE key GLOB ?1 AND json_extract(value, '$.repo_id') = ?2
         ORDER BY key",
        )
        .unwrap();
    docs.query_map([pattern, &f.repo.id], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn assert_publication(
    f: &SingleRepoFixture,
    ledger: &OutageLedger,
    rev: i64,
    parent: &str,
) -> String {
    let (tip, tree) = git_main_commit_and_tree(&f.repo.bare);
    let bare = git2::Repository::open_bare(&f.repo.bare).unwrap();
    let commit = bare.find_commit(tip).unwrap();
    assert_eq!(commit.parent_count(), 1);
    assert_eq!(commit.parent_id(0).unwrap().to_string(), parent);
    let sha = tip.to_string();
    let db = Database::new(&f.db_path).unwrap();
    let push = db.latest_git_push_operation(&f.repo.id).unwrap().unwrap();
    assert!(db.active_git_push_operation(&f.repo.id).unwrap().is_none());
    assert_eq!(push.state, GitPushOperationState::Completed);
    assert_eq!(push.initiator_id, "team_worker");
    assert_eq!(push.source_svn_rev, rev);
    assert_eq!(push.pre_push_git_sha, parent);
    assert_eq!(push.pre_push_git_branch, "main");
    assert_eq!(push.last_confirmed_git_sha.as_ref(), Some(&sha));
    assert_eq!(push.last_confirmed_git_tree, Some(tree.to_string()));
    assert_eq!(
        ledger.svn_checkpoint.outcome,
        MappingOutcome::AppliedAndVerified
    );
    assert_eq!(ledger.svn_checkpoint.target_git_sha.as_ref(), Some(&sha));
    let record = ledger.sync_records.last().unwrap();
    assert_eq!(record["revision"], rev);
    assert_eq!(record["sha"], sha);
    assert_eq!(record["direction"], "svn_to_git");
    assert_eq!(record["status"], "applied");
    assert!(record["id"].as_str().is_some_and(|id| !id.is_empty()));
    let outcome = ledger.outcomes.last().unwrap();
    assert_eq!(outcome["key"], svn_source_key(rev));
    assert_eq!(outcome["direction"], "svn_to_git");
    assert_eq!(outcome["outcome"], "applied_and_verified");
    assert_eq!(outcome["sha"], sha);
    sha
}

fn check_frontier(f: &SingleRepoFixture, ledger: &OutageLedger, rev: i64, sha: &str) {
    let db = Database::new(&f.db_path).unwrap();
    let key = format!("last_svn_rev_{}", f.repo.id);
    assert_eq!(db.get_state(&key).unwrap(), Some(rev.to_string()));
    assert_eq!(ledger.watermark, (rev, sha.to_owned()));
    assert_eq!(
        ledger.svn_checkpoint.handled_source_key,
        svn_source_key(rev)
    );
    assert!(
        ledger.git_checkpoint.is_none()
            && ledger.inbound_git.is_none()
            && ledger.commit_map.is_empty()
    );
}

async fn cycle(engine: &SyncEngine, imports: usize) {
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        (stats.svn_to_git_count, stats.git_to_svn_count),
        (imports, 0)
    );
}

pub(super) async fn run() -> Value {
    let f = SingleRepoFixture::on_svn_branch("direct", "trunk");
    let repo = &f.repo;
    let svn_repo = f.tmp.path().join("svnserve-root").join(&repo.id);
    let feature_url = format!("{}/branches/feature", f.daemon.repo_url(&repo.id));
    git_cli(&repo.bare, &["config", "core.logAllRefUpdates", "true"]);
    let engine = managed_engine(&f);
    cycle(&engine, 1).await;
    let before = outage_ledger(&f);
    let baseline = repo.svn_rev;
    let initial_sha = assert_publication(&f, &before, baseline, &repo.initial_git_sha);
    check_frontier(&f, &before, baseline, &initial_sha);
    assert_eq!(before.sync_records.len(), 1);
    assert_eq!(before.outcomes.len(), 1);
    assert_eq!(youngest(&svn_repo), baseline);
    assert_eq!(
        before.lineage.svn_uuid,
        svnlook_at(&svn_repo, "uuid", &[]).trim()
    );
    assert_eq!(before.lineage.svn_branch_path, "trunk");
    assert_eq!(before.lineage.incarnation_created_rev, 1);
    assert!(before.lineage.copy_from_path.is_none() && before.lineage.copy_from_rev.is_none());
    assert_eq!(before.lineage.generation, 1);
    assert_eq!(before.lineage.git_ref.as_deref(), Some("main"));
    assert_eq!(before.lineage.svn_url, repo.svn_url);
    assert_eq!(before.lineage.svn_root_url, f.daemon.repo_url(&repo.id));
    let pushes = || journal_documents(&f, "svn_to_git_push_v1:document:*");
    let mappings = || journal_documents(&f, "team_cycle_mapping_v1:document:*");
    assert_eq!(pushes().len(), 1);
    assert!(mappings().is_empty());
    let commit_file = |wc: &Path, path: &str, bytes: &str, message: &str| {
        svn_commit_file(wc, path, bytes, message, &repo.username, &repo.password)
    };
    let initial_files = [(".gitkeep", ""), ("origin.txt", "SVN origin\n")];
    files_at(&f, &initial_files);
    let baseline_log = reflog(&f);
    assert_eq!(baseline_log.lines().count(), 1);

    svn(
        &f,
        &[
            "copy",
            "--parents",
            &repo.svn_url,
            &feature_url,
            "-m",
            "Copy trunk to feature",
        ],
    );
    let copy = youngest(&svn_repo);
    let copy_history = svnlook_at(
        &svn_repo,
        "changed",
        &["--copy-info", "-r", &copy.to_string()],
    );
    assert!(
        copy_history.contains("branches/feature/")
            && copy_history.contains(&format!("from trunk/:r{baseline}")),
        "{copy_history}"
    );
    let feature = f.tmp.path().join("feature-wc");
    svn_checkout(&feature_url, &feature, &repo.username, &repo.password);
    let source = commit_file(&feature, "merged.txt", MERGED, "Feature change for trunk");
    assert_eq!((copy, source), (baseline + 1, baseline + 2));
    assert_revision(
        &svn_repo,
        source,
        "Feature change for trunk",
        &["A   branches/feature/merged.txt"],
    );
    assert_eq!(
        file_at(&svn_repo, source, "branches/feature/merged.txt"),
        MERGED
    );
    drop(engine);
    let engine = managed_engine(&f);
    cycle(&engine, 0).await;
    let gap = outage_ledger(&f);
    assert_eq!(gap, before);
    check_frontier(&f, &gap, baseline, &initial_sha);
    assert_eq!(reflog(&f), baseline_log);
    files_at(&f, &initial_files);
    assert_eq!(youngest(&svn_repo), source);

    svn(&f, &["update", "."]);
    svn(&f, &["merge", "-c", &source.to_string(), &feature_url, "."]);
    svn(
        &f,
        &["commit", ".", "-m", "Merge feature content into trunk"],
    );
    let merged = youngest(&svn_repo);
    assert_eq!(merged, source + 1);
    let merged_prop = mergeinfo(&svn_repo, merged);
    assert_eq!(merged_prop, format!("/branches/feature:{source}"));
    assert_revision(
        &svn_repo,
        merged,
        "Merge feature content into trunk",
        &["_U  trunk/", "A   trunk/merged.txt"],
    );
    assert_eq!(file_at(&svn_repo, merged, "trunk/merged.txt"), MERGED);
    cycle(&engine, 1).await;
    let after_merge = outage_ledger(&f);
    let merge_sha = assert_publication(&f, &after_merge, merged, &initial_sha);
    check_frontier(&f, &after_merge, merged, &merge_sha);
    assert_eq!(after_merge.lineage, before.lineage);
    assert_eq!(after_merge.sync_records.len(), 2);
    assert!(after_merge.sync_records.starts_with(&before.sync_records));
    assert_eq!(after_merge.outcomes.len(), 2);
    assert!(after_merge.outcomes.starts_with(&before.outcomes));
    let merge_pushes = pushes();
    assert_eq!(merge_pushes.len(), 2);
    assert!(mappings().is_empty());
    let merged_files = [initial_files[0], initial_files[1], ("merged.txt", MERGED)];
    files_at(&f, &merged_files);
    let merged_log = reflog(&f);
    let update = merged_log.strip_prefix(&baseline_log).unwrap();
    assert_eq!(update.lines().count(), 1);
    assert!(update.starts_with(&format!("{initial_sha} {merge_sha} ")));
    assert_eq!(youngest(&svn_repo), merged);

    let withheld = commit_file(
        &feature,
        "source-only.txt",
        RECORDED,
        "Feature change recorded only",
    );
    assert_eq!(withheld, merged + 1);
    assert_revision(
        &svn_repo,
        withheld,
        "Feature change recorded only",
        &["A   branches/feature/source-only.txt"],
    );
    svn(&f, &["update", "."]);
    svn(
        &f,
        &[
            "merge",
            "--record-only",
            "-c",
            &withheld.to_string(),
            &feature_url,
            ".",
        ],
    );
    svn(
        &f,
        &[
            "commit",
            ".",
            "-m",
            "Record feature change without applying its file",
        ],
    );
    let recorded = youngest(&svn_repo);
    assert_eq!(recorded, withheld + 1);
    assert_revision(
        &svn_repo,
        recorded,
        "Record feature change without applying its file",
        &["_U  trunk/"],
    );
    let recorded_prop = mergeinfo(&svn_repo, recorded);
    assert_eq!(
        recorded_prop,
        format!("/branches/feature:{source},{withheld}")
    );
    assert_eq!(
        svnlook_at(&svn_repo, "cat", &["branches/feature/source-only.txt"]),
        RECORDED
    );
    cycle(&engine, 0).await;
    let after_record = outage_ledger(&f);
    check_frontier(&f, &after_record, recorded, &merge_sha);
    assert_eq!(after_record.lineage, before.lineage);
    assert_eq!(after_record.sync_records, after_merge.sync_records);
    let [no_target] = after_record
        .outcomes
        .strip_prefix(after_merge.outcomes.as_slice())
        .unwrap()
    else {
        panic!("expected one mergeinfo-only outcome")
    };
    assert_eq!(no_target["key"], svn_source_key(recorded));
    assert_eq!(no_target["direction"], "svn_to_git");
    assert_eq!(no_target["outcome"], "intentionally_filtered_no_target");
    assert_eq!(no_target["reason"], "no_git_content");
    assert!(no_target["sha"].is_null() && no_target["target_svn_rev"].is_null());
    assert_eq!(
        after_record.svn_checkpoint.outcome,
        MappingOutcome::IntentionallyFilteredNoTarget
    );
    assert!(after_record.svn_checkpoint.target_git_sha.is_none());
    assert_eq!(pushes(), merge_pushes);
    let mapping_documents = mappings();
    let [document] = mapping_documents.as_slice() else {
        panic!("expected one metadata-only operation")
    };
    let operation: TeamCycleMappingOperation = serde_json::from_str(document).unwrap();
    assert_eq!(operation.state, TeamCycleMappingState::Completed);
    assert_eq!(operation.direction, TeamCycleMappingDirection::SvnToGit);
    assert_eq!(
        operation.outcome,
        TeamCycleMappingOutcome::SvnNoTargetContent
    );
    assert_eq!(operation.source_svn_rev, Some(recorded));
    assert_eq!(operation.pre_write_svn_rev, merged);
    assert_eq!(operation.pre_write_git_sha, merge_sha);
    assert_eq!(operation.initiator_id, "team_worker");
    assert_eq!(no_target["policy_snapshot"], operation.projection);
    assert!(engine
        .db()
        .active_team_cycle_mapping_operation(&repo.id)
        .unwrap()
        .is_none());
    let receipt_key = handled_svn_no_target_state_key(
        &before.lineage.scope_uuid,
        before.lineage.generation,
        recorded,
    );
    let stored_receipt = || {
        Database::new(&f.db_path)
            .unwrap()
            .get_state(&receipt_key)
            .unwrap()
            .unwrap()
    };
    let raw_receipt = stored_receipt();
    let receipt = read_svn_no_target_receipt(engine.db(), &repo.id, recorded)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw_receipt).unwrap(),
        receipt
    );
    assert_eq!(receipt["scope_uuid"], before.lineage.scope_uuid);
    assert_eq!(receipt["generation"], before.lineage.generation);
    assert_eq!(receipt["projection"], operation.projection);
    assert_eq!(receipt["outcome"], "no_git_content");
    assert_eq!(receipt["svn_revision"], recorded);
    assert_eq!(receipt["repo_id"], repo.id);
    files_at(&f, &merged_files);
    assert_eq!(reflog(&f), merged_log);
    assert_eq!(youngest(&svn_repo), recorded);
    drop(engine);

    let continued = commit_file(&f.wc, "continued.txt", CHILD, "Continue after mergeinfo");
    assert_revision(
        &svn_repo,
        continued,
        "Continue after mergeinfo",
        &["A   trunk/continued.txt"],
    );
    assert_eq!(continued, recorded + 1);
    let trunk_tree = svnlook_at(
        &svn_repo,
        "tree",
        &["--full-paths", "-r", &continued.to_string(), "trunk"],
    );
    assert_eq!(
        trunk_tree.lines().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "trunk/",
            "trunk/.gitkeep",
            "trunk/continued.txt",
            "trunk/merged.txt",
            "trunk/origin.txt",
        ])
    );
    let after_source = svnlook_at(&svn_repo, "tree", &["--show-ids", "--full-paths"]);
    let reopened = managed_engine(&f);
    assert_eq!(stored_receipt(), raw_receipt);
    assert_eq!(mappings(), mapping_documents);
    cycle(&reopened, 1).await;
    let final_ledger = outage_ledger(&f);
    let child_sha = assert_publication(&f, &final_ledger, continued, &merge_sha);
    check_frontier(&f, &final_ledger, continued, &child_sha);
    assert_eq!(final_ledger.lineage, before.lineage);
    assert_eq!(final_ledger.sync_records.len(), 3);
    assert!(final_ledger
        .sync_records
        .starts_with(&after_record.sync_records));
    assert_eq!(final_ledger.outcomes.len(), 4);
    assert!(final_ledger.outcomes.starts_with(&after_record.outcomes));
    let final_pushes = pushes();
    assert_eq!(final_pushes.len(), 3);
    for prior in &merge_pushes {
        assert!(final_pushes.contains(prior), "completed push was rewritten");
    }
    // Inspect branch histories at the actual SVN URLs, including the global
    // revision gaps between trunk commits and the sibling's own changes.
    for (url, expected) in [
        (&repo.svn_url, [merged, recorded, continued]),
        (&feature_url, [copy, source, withheld]),
    ] {
        let client = SvnClient::new(url, &repo.username, &repo.password);
        let log = client.log(copy, continued).await.unwrap();
        let revisions: Vec<_> = log.iter().map(|entry| entry.revision).collect();
        assert_eq!(revisions, expected);
    }
    let final_log = reflog(&f);
    let update = final_log.strip_prefix(&merged_log).unwrap();
    assert_eq!(update.lines().count(), 1);
    assert!(update.starts_with(&format!("{merge_sha} {child_sha} ")));
    let final_files = [
        initial_files[0],
        initial_files[1],
        ("merged.txt", MERGED),
        ("continued.txt", CHILD),
    ];
    for _ in 0..2 {
        cycle(&reopened, 0).await;
        let quiet_ledger = outage_ledger(&f);
        assert_eq!(quiet_ledger, final_ledger);
        check_frontier(&f, &quiet_ledger, continued, &child_sha);
        assert_eq!(
            reopened
                .db()
                .get_repository(&repo.id)
                .unwrap()
                .unwrap()
                .total_syncs,
            3
        );
        assert_eq!(stored_receipt(), raw_receipt);
        assert_eq!(mappings(), mapping_documents);
        assert_eq!(pushes(), final_pushes);
        assert_eq!(reflog(&f), final_log);
        files_at(&f, &final_files);
        assert!(git_path_missing(&repo.bare, "main", "source-only.txt"));
        assert_eq!(youngest(&svn_repo), continued);
        assert_eq!(
            svnlook_at(&svn_repo, "tree", &["--show-ids", "--full-paths"]),
            after_source
        );
        assert_eq!(mergeinfo(&svn_repo, continued), recorded_prop);
        for (path, bytes) in final_files {
            assert_eq!(
                svnlook_at(&svn_repo, "cat", &[&format!("trunk/{path}")]),
                bytes
            );
        }
        assert_eq!(
            svnlook_at(&svn_repo, "cat", &["branches/feature/merged.txt"]),
            MERGED
        );
        assert_eq!(
            svnlook_at(&svn_repo, "cat", &["branches/feature/source-only.txt"]),
            RECORDED
        );
    }
    json!({
        "svn_uuid": before.lineage.svn_uuid,
        "scope_uuid": before.lineage.scope_uuid,
        "generation": before.lineage.generation,
        "branch_path": before.lineage.svn_branch_path,
        "incarnation_created_rev": before.lineage.incarnation_created_rev,
        "source_only_revisions": [copy, source, withheld],
        "merge_revision": merged,
        "mergeinfo": merged_prop,
        "record_only_revision": recorded,
        "record_only_mergeinfo": recorded_prop,
        "no_target_outcome": no_target["outcome"],
        "no_target_operation_id": operation.id,
        "no_target_outcome_id": no_target["id"],
        "continued_revision": continued,
        "git_before": initial_sha,
        "git_after_merge": merge_sha,
        "git_after_continuation": child_sha,
        "merge_receipt_id": after_merge.sync_records[1]["id"],
        "continuation_receipt_id": final_ledger.sync_records[2]["id"],
        "bare_ref_updates": final_log.strip_prefix(&baseline_log).unwrap().lines().count(),
        "quiet_cycles": 2
    })
}
