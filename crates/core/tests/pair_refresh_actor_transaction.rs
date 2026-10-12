//! Authorization inside the actual SQLite writer reservation, without nested locking.
use chrono::{Duration, Utc};
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::client::GitClient;
use reposync_core::identity::mapper::IdentityMapper;
use reposync_core::models::{Session, User};
use reposync_core::svn::client::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use std::sync::Arc;

const TOKEN: &str = "disposable-refresh-admin-session";

struct Fixture {
    dir: tempfile::TempDir,
    db: Database,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("reposync.db")).unwrap();
        db.initialize().unwrap();
        db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        let created = (Utc::now() - Duration::minutes(2)).to_rfc3339();
        db.insert_user(&User {
            id: "admin-id".into(),
            username: "admin".into(),
            display_name: "Admin".into(),
            email: "admin@example.invalid".into(),
            password_hash: "fixture".into(),
            role: "admin".into(),
            enabled: true,
            created_at: created.clone(),
            updated_at: created,
        })
        .unwrap();
        db.insert_session(&Session {
            token: TOKEN.into(),
            user_id: "admin-id".into(),
            created_at: (Utc::now() - Duration::minutes(1)).to_rfc3339(),
            expires_at: (Utc::now() + Duration::hours(1)).to_rfc3339(),
        })
        .unwrap();
        Self { dir, db }
    }

    fn engine(&self, change: impl FnOnce(&mut AppConfig)) -> SyncEngine {
        let mut config: AppConfig = toml::from_str(
            r#"
            [daemon]
            data_dir = "/unused"
            [svn]
            url = "file:///unused/svn"
            username = "fixture"
            password_env = ""
            [github]
            repo = "fixture/repo"
            token_env = ""
        "#,
        )
        .unwrap();
        change(&mut config);
        let work = self.dir.path().join("git-work");
        git2::Repository::init(&work).unwrap();
        SyncEngine::new(
            config,
            Database::new(self.dir.path().join("reposync.db")).unwrap(),
            SvnClient::new("file:///unused/svn", "fixture", ""),
            GitClient::new(&work).unwrap(),
            Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
        )
    }

    fn rows(&self) -> Vec<(String, String, String)> {
        self.db
            .conn()
            .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

#[test]
fn actual_reserved_engine_transaction_rechecks_without_relocking_or_mutating() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let before = f.rows();
    let conn = engine.db().conn();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    authority
        .require_current_in_reserved_transaction(&engine, &conn)
        .unwrap();
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);
    assert_eq!(f.rows(), before);
}

#[test]
fn autocommit_and_stale_read_snapshot_cannot_supply_current_authorization() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let conn = engine.db().conn();
    assert!(authority
        .require_current_in_reserved_transaction(&engine, &conn)
        .is_err());
    conn.execute_batch("BEGIN").unwrap();
    let present: i64 = conn
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(present, 1);
    f.db.delete_session(TOKEN).unwrap();
    let stale: i64 = conn
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stale, 1);
    assert!(authority
        .require_current_in_reserved_transaction(&engine, &conn)
        .is_err());
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);
    assert!(authority.require_current(&engine).is_err());
}

#[test]
fn identical_actor_rows_in_different_storage_do_not_authorize_its_transaction() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let foreign = Fixture::new();
    let conn = foreign.db.conn();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(authority
        .require_current_in_reserved_transaction(&engine, &conn)
        .is_err());
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);
    let replacement = f.engine(|_| {});
    let conn = engine.db().conn();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(authority
        .require_current_in_reserved_transaction(&replacement, &conn)
        .is_err());
    conn.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn revocation_committed_before_reservation_refuses_inside_that_transaction() {
    for sql in [
        "DELETE FROM sessions",
        "UPDATE users SET enabled=0",
        "UPDATE users SET role='user'",
        "UPDATE sessions SET expires_at='2000-01-01T00:00:00Z'",
    ] {
        let f = Fixture::new();
        let engine = f.engine(|_| {});
        let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
        f.db.conn().execute(sql, []).unwrap();
        let before = f.rows();
        let conn = engine.db().conn();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            authority
                .require_current_in_reserved_transaction(&engine, &conn)
                .is_err(),
            "{sql}"
        );
        conn.execute_batch("ROLLBACK").unwrap();
        drop(conn);
        assert_eq!(f.rows(), before);
    }
}

#[test]
fn missing_key_is_not_recreated_by_reserved_authorization() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    f.db.conn()
        .execute("DELETE FROM kv_state WHERE key='encryption_key'", [])
        .unwrap();
    let before = f.rows();
    let conn = engine.db().conn();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(authority
        .require_current_in_reserved_transaction(&engine, &conn)
        .is_err());
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);
    assert_eq!(f.rows(), before);
    assert!(f.db.get_state("encryption_key").unwrap().is_none());
}

#[test]
fn copied_exact_actor_and_key_context_still_cannot_supply_foreign_storage_authorization() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    // SQLite performs the copy itself; no unrelated fd is opened/closed on
    // the live source inode, and all actor/session/key rows are exact copies.
    f.db.conn()
        .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
        .unwrap();
    let foreign = Fixture {
        db: Database::new(&path).unwrap(),
        dir,
    };
    let foreign_engine = foreign.engine(|_| {});
    let copied_context = foreign_engine
        .capture_pair_refresh_actor_policy(TOKEN)
        .unwrap();
    assert_eq!(copied_context.pin(), authority.pin());
    assert_eq!(foreign.rows(), f.rows());
    let conn = foreign_engine.db().conn();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(authority
        .require_current_in_reserved_transaction(&engine, &conn)
        .is_err());
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);
    assert_eq!(foreign.rows(), f.rows());
}
