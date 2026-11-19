use super::*;
use crate::db::git_push_operations::{git_push_target_fingerprint, GitPushIntent};

const REPO: &str = "finalized";
const MIRROR: &str = "last_svn_rev_finalized";
const EMITTED: &str = "dddddddddddddddddddddddddddddddddddddddd";
const TREE: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn finalized_then_rewound(remote: &str) -> (tempfile::TempDir, Database) {
    let (dir, db, _) = open_repo(REPO);
    let push = db
        .begin_svn_to_git_push(GitPushIntent {
            repo_id: REPO,
            initiator_id: "worker",
            request_id: "request",
            target_fingerprint: &git_push_target_fingerprint(REPO, remote, "main"),
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "emit r3",
            pre_push_git_remote: remote,
            pre_push_git_branch: "main",
            pre_push_git_sha: BASELINE_SHA,
            pre_push_git_tree: Some(BASELINE_SHA),
            intended_local_git_sha: EMITTED,
            intended_local_git_parent: Some(BASELINE_SHA),
            intended_local_git_tree: TREE,
        })
        .unwrap();
    db.confirm_svn_to_git_push(REPO, &push.id, EMITTED, TREE)
        .unwrap();
    db.conn()
        .execute(
            "UPDATE repositories SET last_svn_rev=1,last_git_sha=?1 WHERE id=?2",
            params![BASELINE_SHA, REPO],
        )
        .unwrap();
    assert_eq!(
        db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [MIRROR])
            .unwrap(),
        1
    );
    assert_eq!(
        db.conn()
            .execute(
                "DELETE FROM sync_records WHERE repo_id=?1 AND svn_rev=3",
                [REPO]
            )
            .unwrap(),
        1
    );
    (dir, db)
}

#[test]
fn refuses_to_backfill_over_a_later_finalized_svn_checkpoint() {
    let (_dir, db) = finalized_then_rewound("origin");
    // A second registration normally revokes legacy KV read authority. The veto only observes it.
    db.conn().execute(
        "INSERT INTO repositories(id,name,svn_url,created_at,updated_at) VALUES ('peer','peer','file:///peer','t','t')",
        [],
    ).unwrap();
    db.set_state(
        crate::db::repo_scope_identity::LEGACY_REPO_ID_KV_READS_KEY,
        "1",
    )
    .unwrap();
    let total_changes = || {
        db.conn()
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let prior_changes = total_changes();
    for _ in 0..2 {
        let baseline = resolve_repo_import_baseline(&db, REPO).unwrap();
        let mirror = db.get_state(MIRROR).unwrap();
        assert!(
            matches!(&baseline, RepoImportBaseline::ReconciliationRequired { reason, detail }
                if reason == "conflicting_finalized_svn_checkpoint" && detail.contains("r1") && detail.contains("r3")),
            "resolver returned {baseline:?}; physically stored mirror is {mirror:?}"
        );
        assert_eq!(mirror, None);
        assert_eq!(
            db.get_repo_watermark(REPO).unwrap(),
            (1, BASELINE_SHA.to_owned())
        );
        assert_eq!(db.count_sync_records().unwrap(), 0);
        assert_eq!(total_changes(), prior_changes);
    }
}

#[test]
fn incomplete_or_foreign_finalized_evidence_preserves_legacy_backfill() {
    for adjustment in [
        "DELETE FROM canonical_mapping_outcomes; DELETE FROM canonical_checkpoints",
        "UPDATE canonical_checkpoints SET direction='git_to_svn',handled_source_key='git:dddddddddddddddddddddddddddddddddddddddd',target_svn_rev=3,target_git_sha=NULL; UPDATE canonical_mapping_outcomes SET direction='git_to_svn',source_key='git:dddddddddddddddddddddddddddddddddddddddd',target_svn_rev=3,target_git_sha=NULL",
        "UPDATE canonical_checkpoints SET scope_uuid='016fa271-7ea9-45de-9346-a667374f285f'; UPDATE canonical_mapping_outcomes SET scope_uuid='016fa271-7ea9-45de-9346-a667374f285f'",
        "UPDATE canonical_checkpoints SET generation=2; UPDATE canonical_mapping_outcomes SET generation=2",
        "UPDATE repositories SET git_branch='other'",
        "DELETE FROM kv_state WHERE key='svn_to_git_push_v1:latest:finalized'",
        "UPDATE kv_state SET value=json_set(value,'$.state','running') WHERE key GLOB 'svn_to_git_push_v1:document:*'; INSERT INTO kv_state(key,value,updated_at) SELECT 'svn_to_git_push_v1:active:finalized',value,'t' FROM kv_state WHERE key='svn_to_git_push_v1:latest:finalized'",
        "INSERT INTO kv_state(key,value,updated_at) SELECT 'repo_echo_generation_'||scope_uuid,'2','t' FROM repositories",
        "INSERT OR REPLACE INTO kv_state(key,value,updated_at) VALUES ('reposync_v13_legacy_repo_id_kv_reads','1','t'),('repo_echo_generation_finalized','2','t')",
        "DROP TABLE canonical_checkpoints; DROP TABLE canonical_mapping_outcomes; DROP TABLE canonical_lineage",
    ] {
        let (_dir, db) = finalized_then_rewound("origin");
        db.conn().execute_batch(adjustment).unwrap();
        assert_eq!(
            resolve_repo_import_baseline(&db, REPO).unwrap(),
            RepoImportBaseline::Verified { svn_rev: 1, git_sha: BASELINE_SHA.into() },
            "{adjustment}"
        );
        assert_eq!(db.get_state(MIRROR).unwrap().as_deref(), Some("1"), "{adjustment}");
    }
}

#[test]
fn explicit_late_pair_push_target_preserves_legacy_backfill() {
    let (_dir, db) = finalized_then_rewound("file:///git/repo.git");
    assert_eq!(
        resolve_repo_import_baseline(&db, REPO).unwrap(),
        RepoImportBaseline::Verified {
            svn_rev: 1,
            git_sha: BASELINE_SHA.into()
        }
    );
    assert_eq!(db.get_state(MIRROR).unwrap().as_deref(), Some("1"));
}

#[test]
fn corrupt_present_evidence_does_not_create_a_mirror() {
    for (corruption, error_fragment) in [
        ("DROP TABLE canonical_checkpoints", "schema is incomplete"),
        ("UPDATE repositories SET scope_uuid=NULL", "scope UUID is missing"),
        ("UPDATE repositories SET scope_uuid='00000000-0000-0000-0000-000000000000'", "scope UUID is invalid"),
        ("UPDATE repositories SET scope_uuid='broken'", "scope UUID is invalid"),
        ("DELETE FROM kv_state WHERE key GLOB 'svn_to_git_push_v1:document:*'", "document is missing"),
        ("UPDATE kv_state SET value='broken' WHERE key GLOB 'svn_to_git_push_v1:document:*'", "invalid svn-to-git push"),
        ("UPDATE kv_state SET value=json_set(value,'$.repo_id','other') WHERE key GLOB 'svn_to_git_push_v1:document:*'", "identity differs"),
        ("UPDATE kv_state SET value=json_set(value,'$.pre_push_git_remote','file:///other') WHERE key GLOB 'svn_to_git_push_v1:document:*'", "target fingerprint differs"),
        ("INSERT INTO kv_state(key,value,updated_at) VALUES ('svn_to_git_push_v1:active:finalized','','t')", "still has an active pointer"),
        ("UPDATE kv_state SET value=json_set(value,'$.last_confirmed_git_tree','short') WHERE key GLOB 'svn_to_git_push_v1:document:*'", "inconsistent target or confirmation"),
        ("INSERT INTO kv_state(key,value,updated_at) SELECT 'repo_echo_generation_'||scope_uuid,'broken','t' FROM repositories", "invalid repository echo generation"),
        ("DELETE FROM canonical_mapping_outcomes", "do not both corroborate"),
        ("DELETE FROM canonical_checkpoints", "do not both corroborate"),
        ("UPDATE canonical_mapping_outcomes SET target_git_sha='abababababababababababababababababababab'", "do not both corroborate"),
        ("UPDATE canonical_checkpoints SET outcome='pending'", "do not both corroborate"),
        ("UPDATE canonical_checkpoints SET generation=2", "do not both corroborate"),
    ] {
        let (_dir, db) = finalized_then_rewound("origin");
        db.conn().execute_batch(corruption).unwrap();
        let error = resolve_repo_import_baseline(&db, REPO).unwrap_err().to_string();
        assert!(error.contains(error_fragment), "{corruption}: {error}");
        assert_eq!(db.get_state(MIRROR).unwrap(), None);
        assert_eq!(db.get_repo_watermark(REPO).unwrap(), (1, BASELINE_SHA.to_owned()));
    }
}

#[test]
fn existing_mirrors_keep_their_original_admission_and_bytes() {
    for value in ["", "garbled", "0", "-1", "3", "1"] {
        let (_dir, db) = finalized_then_rewound("origin");
        db.set_state(MIRROR, value).unwrap();
        let baseline = resolve_repo_import_baseline(&db, REPO).unwrap();
        if value == "1" {
            assert_eq!(
                baseline,
                RepoImportBaseline::Verified {
                    svn_rev: 1,
                    git_sha: BASELINE_SHA.into()
                }
            );
        } else {
            assert!(
                matches!(baseline, RepoImportBaseline::ReconciliationRequired { reason, .. } if reason == "conflicting_import_checkpoint")
            );
        }
        assert_eq!(db.get_state(MIRROR).unwrap().as_deref(), Some(value));
    }
}
