//! Pending is read-only; no timeout or queue may release another writer's hold.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

fn rows(f: &job::Fixture) -> Vec<(String, String, String)> {
    f.db.conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn preparing_contention_is_read_only_then_admits_after_the_real_owner_releases() {
    let f = job::Fixture::new();
    let first = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .try_begin_ordinary_writer_window(Some("child"))
        .unwrap()
        .is_none());
    assert_eq!(rows(&f), before);
    assert!(f.db.begin_ordinary_writer_window(Some("child")).is_err());
    assert_eq!(rows(&f), before);
    first.finish_preparation().unwrap();
    let next =
        f.db.try_begin_ordinary_writer_window(Some("child"))
            .unwrap()
            .unwrap();
    next.require_engine_scope(&f.db, Some("child")).unwrap();
    assert!(f.db.begin_ordinary_writer_window(Some("child")).is_err());
    next.finish_preparation().unwrap();
    f.db.begin_ordinary_writer_window(Some("child"))
        .unwrap()
        .finish_preparation()
        .unwrap();
}

#[test]
fn uncertain_global_publication_is_an_error_and_keeps_its_exact_document() {
    let f = job::Fixture::new();
    let first = f.db.begin_ordinary_writer_window(None).unwrap();
    first.mark_unjournalled_publication_possible().unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .try_begin_ordinary_writer_window(Some("child"))
        .is_err());
    assert!(f
        .db
        .try_begin_ordinary_writer_window(Some("unrelated"))
        .is_err());
    assert_eq!(rows(&f), before);
    drop(first);
    assert_eq!(rows(&f), before);
    assert!(f
        .db
        .try_begin_ordinary_writer_window(Some("child"))
        .is_err());
    assert_eq!(rows(&f), before);
}

#[test]
fn refresh_and_corrupt_evidence_never_become_waitable_preparation() {
    for corrupt in [false, true] {
        let f = job::Fixture::new();
        let busy = f.busy();
        let id = (!corrupt).then(|| f.admit(f.pins(), &busy));
        if corrupt {
            f.db.set_state("pair_refresh_writer_window_v1:document:not-a-uuid", "{}")
                .unwrap();
        }
        let before = rows(&f);
        assert!(f
            .db
            .try_begin_ordinary_writer_window(Some("child"))
            .is_err());
        assert_eq!(rows(&f), before);
        if let Some(id) = id {
            assert!(!f.no_hold(Some("child")));
            assert_eq!(
                f.db.pair_refresh_operation(id).unwrap().phase,
                reposync_core::db::pair_refresh_operations::PairRefreshPhase::Prepared
            );
        } else {
            assert!(f.no_hold(None));
            assert!(f
                .db
                .try_begin_ordinary_writer_window(Some("unrelated"))
                .is_err());
            assert_eq!(rows(&f), before);
        }
    }
}

#[test]
fn unrelated_preparation_remains_independently_admissible() {
    let f = job::Fixture::new();
    let first = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
    let other =
        f.db.try_begin_ordinary_writer_window(Some("unrelated"))
            .unwrap()
            .unwrap();
    other
        .require_engine_scope(&f.db, Some("unrelated"))
        .unwrap();
    assert!(f
        .db
        .try_begin_ordinary_writer_window(Some("child"))
        .unwrap()
        .is_none());
    other.finish_preparation().unwrap();
    first.finish_preparation().unwrap();
}
