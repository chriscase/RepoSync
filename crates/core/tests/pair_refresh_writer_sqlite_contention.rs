//! Concurrent connections must serialize admission before reading durable windows.
use reposync_core::db::Database;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn admission_waits_for_a_competing_sqlite_writer_before_reading_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    let contender = Database::new(&path).unwrap();
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer
        .execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES('contending-writer','held','now')",
            [],
        )
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = contender.try_begin_ordinary_writer_window(Some("child"));
        let result = result.and_then(|window| {
            let window = window.expect("a serialized admission must see the quiet scope");
            window.require_engine_scope(&contender, Some("child"))?;
            window.finish_preparation()
        });
        result_tx
            .send(result.map_err(|error| error.to_string()))
            .unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let premature = result_rx.recv_timeout(Duration::from_millis(100));
    writer.execute_batch("COMMIT").unwrap();
    thread.join().unwrap();
    assert_eq!(premature, Err(mpsc::RecvTimeoutError::Timeout));
    result_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    db.begin_ordinary_writer_window(Some("child"))
        .unwrap()
        .finish_preparation()
        .unwrap();
}

#[test]
fn preparation_drop_waits_for_sqlite_contention_and_releases_only_its_window() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (drop_tx, drop_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let owner_path = path.clone();
    let thread = std::thread::spawn(move || {
        let owner = Database::new(&owner_path).unwrap();
        let window = owner.begin_ordinary_writer_window(Some("child")).unwrap();
        ready_tx.send(()).unwrap();
        drop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(window);
        done_tx.send(()).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer
        .execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES('contending-writer','held','now')",
            [],
        )
        .unwrap();
    drop_tx.send(()).unwrap();
    let premature = done_rx.recv_timeout(Duration::from_millis(100));
    writer.execute_batch("COMMIT").unwrap();
    thread.join().unwrap();
    assert_eq!(premature, Err(mpsc::RecvTimeoutError::Timeout));
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        db.get_state("contending-writer").unwrap().as_deref(),
        Some("held")
    );
    db.begin_ordinary_writer_window(Some("child"))
        .unwrap()
        .finish_preparation()
        .unwrap();
}
