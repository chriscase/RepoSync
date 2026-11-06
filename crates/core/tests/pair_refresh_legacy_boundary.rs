//! Grok controls for unscoped correspondence and global provenance mutations.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;
use reposync_core::db::pair_refresh_operations::{
    capture_refresh_registration, PairRefreshPhase, RefreshJobPins,
};
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;
use rusqlite::params;

fn confirmed(f: &job::Fixture, pins: &RefreshJobPins) -> uuid::Uuid {
    let guard = f.busy();
    let id = f.admit(pins.clone(), &guard);
    let mut copy = pins.svn.pins.lineage.child_copy.clone();
    copy.inspected_revision = 7;
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    f.db.record_pair_refresh_svn_receipt(
        id,
        SvnRefreshPublicationReceipt {
            operation_id: id,
            revision: 7,
            child_url: pins.svn.pins.lineage.pins.child_url.clone(),
            child_copy: copy,
            git_tree: pins.git.tree_sha.clone(),
            mergeinfo: pins.svn.mergeinfo.clone(),
            properties: pins.svn.result_properties.clone(),
        },
    )
    .unwrap();
    f.db.record_pair_refresh_git_intent(id).unwrap();
    f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
    id
}

#[test]
fn null_and_empty_scope_correspondence_never_become_child_completion_evidence() {
    for table in ["commit_map", "sync_records"] {
        for owner in [None, Some("")] {
            for (rev, matching_sha) in [(7, false), (6, true), (7, true)] {
                let f = job::Fixture::new();
                let pins = f.pins();
                let id = confirmed(&f, &pins);
                let sha = if matching_sha {
                    pins.git.prepared_sha.clone()
                } else {
                    "e".repeat(40)
                };
                if table == "commit_map" {
                    f.db.conn().execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at) VALUES(?1,?2,?3,'git_to_svn','t')",params![owner,rev,sha]).unwrap();
                } else {
                    f.db.conn().execute("INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) VALUES(?1,?2,?3,?4,'git_to_svn','fixture','fixture','t','t','applied')",params![uuid::Uuid::new_v4().to_string(),owner,rev,sha]).unwrap();
                }
                let before = capture_refresh_registration(&f.db.conn(), "child").unwrap();
                assert!(f.db.finalize_pair_refresh(id).is_err());
                assert_eq!(
                    capture_refresh_registration(&f.db.conn(), "child").unwrap(),
                    before
                );
                assert_eq!(
                    f.db.pair_refresh_operation(id).unwrap().phase,
                    PairRefreshPhase::GitObserved
                );
                assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
                let count: i64 =
                    f.db.conn()
                        .query_row(
                            "SELECT COUNT(*) FROM commit_map WHERE repo_id='child'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap();
                assert_eq!(count, 0);
            }
        }
    }
}

#[test]
fn structurally_missing_lineage_cannot_take_scope_holds() {
    for fault in 0..8 {
        let f = job::Fixture::new();
        let mut pins = f.pins();
        let guard = f.busy();
        let lineage = &mut pins.svn.pins.lineage;
        match fault {
            0 => {
                lineage.pins.child.repository_uuid.clear();
                lineage.child_copy.repository_uuid.clear();
            }
            1 => {
                lineage.pins.child.child_path.clear();
                lineage.child_copy.child_path.clear();
                lineage.pins.child_url = lineage.pins.repository_root_url.clone();
            }
            2 => {
                lineage.pins.child.parent_path = "/../trunk".into();
                lineage.child_copy.parent_path = "/../trunk".into();
                lineage.pins.parent_url = format!("{}/../trunk", lineage.pins.repository_root_url);
            }
            3 => {
                lineage.child_copy.copy_from_revision = 0;
                lineage.pins.child.parent_baseline_revision = 0;
            }
            4 => lineage.child_copy.created_revision = lineage.child_copy.copy_from_revision,
            5 => lineage.parent_originating_revision = 0,
            6 => lineage.parent_originating_revision = 3,
            _ => lineage.pins.parent_url = "https://svn.example/foreign/trunk".into(),
        }
        assert!(f
            .db
            .admit_pair_refresh(
                "admin",
                "request",
                &pins.approval_digest().unwrap(),
                pins,
                &guard
            )
            .is_err());
        assert!(f.no_hold(None));
    }
}

#[test]
fn unrelated_non_team_cursor_cannot_mutate_global_checkpoint_during_a_hold() {
    let f = job::Fixture::new();
    let mut unrelated = f.db.get_repository("unrelated").unwrap().unwrap();
    unrelated.sync_mode = "svn_to_git".into();
    f.db.update_repository(&unrelated).unwrap();
    f.db.set_state("last_git_hash", "original-global").unwrap();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    for exact in [false, true] {
        let result = if exact {
            f.db.advance_exact_skip_watermarks(
                "unrelated",
                &"f".repeat(40),
                &["f".repeat(40)],
                "excluded",
            )
        } else {
            f.db.advance_all_watermarks("unrelated", &"f".repeat(40))
        };
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("durable pair refresh"));
        assert_eq!(
            f.db.get_state("last_git_hash").unwrap().as_deref(),
            Some("original-global")
        );
        assert_eq!(
            f.db.get_repo_watermark("unrelated").unwrap(),
            (2, "e".repeat(40))
        );
    }
    assert!(!f.no_hold(None));
}

#[test]
fn new_registration_and_legacy_authority_write_are_atomic_and_refuse_global_hold() {
    let f = job::Fixture::new();
    let authority = "reposync_v13_legacy_repo_id_kv_reads";
    f.db.set_state(authority, "1").unwrap();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    let mut new = f.db.get_repository("unrelated").unwrap().unwrap();
    new.id = "another".into();
    new.name = "another".into();
    assert!(f
        .db
        .insert_repository(&new)
        .unwrap_err()
        .to_string()
        .contains("durable pair refresh"));
    assert!(f.db.get_repository("another").unwrap().is_none());
    let error =
        reposync_core::db::repo_scope_identity::revoke_legacy_repo_id_kv_reads(&f.db.conn())
            .unwrap_err();
    assert!(error.to_string().contains("durable pair refresh"));
    assert_eq!(f.db.get_state(authority).unwrap().as_deref(), Some("1"));
    assert!(!f.no_hold(None));
}
