//! Live engine policy and current database admin sessions are required together.
use chrono::{Duration, FixedOffset, Utc};
use reposync_core::config::{AppConfig, IdentityConfig, SyncMode};
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
fn current_admin_context_is_private_read_only_and_bound_to_its_actual_engine() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let before = f.rows();
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    authority.require_current(&engine).unwrap();
    let encoded = serde_json::to_string(authority.pin()).unwrap();
    assert!(!encoded.contains(TOKEN));
    assert!(!encoded.contains(&"41".repeat(32)));
    assert_eq!(authority.pin().version, 2);
    assert_eq!(authority.pin().actor_id, "admin-id");
    assert_eq!(authority.pin().session_context_digest.len(), 64);
    assert_eq!(authority.pin().policy_context_digest.len(), 64);
    let replacement = f.engine(|_| {});
    assert!(authority.require_current(&replacement).is_err());
    assert_eq!(f.rows(), before);
}

#[test]
fn expiry_uses_rfc3339_instants_including_offsets() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let expired = (Utc::now() - Duration::hours(1))
        .with_timezone(&FixedOffset::east_opt(14 * 3600).unwrap())
        .to_rfc3339();
    f.db.conn()
        .execute("UPDATE sessions SET expires_at=?1", [&expired])
        .unwrap();
    assert!(engine.capture_pair_refresh_actor_policy(TOKEN).is_err());
    let valid = (Utc::now() + Duration::hours(1))
        .with_timezone(&FixedOffset::west_opt(12 * 3600).unwrap())
        .to_rfc3339();
    f.db.conn()
        .execute("UPDATE sessions SET expires_at=?1", [&valid])
        .unwrap();
    engine
        .capture_pair_refresh_actor_policy(TOKEN)
        .unwrap()
        .require_current(&engine)
        .unwrap();
}

#[test]
fn actor_revocation_role_change_logout_and_session_replacement_refuse_recheck() {
    for mutation in [
        "UPDATE users SET enabled=0",
        "UPDATE users SET enabled=2",
        "UPDATE users SET role='user'",
        "DELETE FROM sessions",
        "UPDATE sessions SET created_at='2000-01-01T00:00:00Z'",
        "UPDATE users SET updated_at='2000-01-01T00:00:00Z'",
    ] {
        let f = Fixture::new();
        let engine = f.engine(|_| {});
        let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
        f.db.conn().execute(mutation, []).unwrap();
        let before = f.rows();
        assert!(authority.require_current(&engine).is_err(), "{mutation}");
        assert_eq!(f.rows(), before);
    }
}

#[test]
fn malformed_future_and_expired_session_times_refuse_without_fallback() {
    for (field, value) in [
        ("expires_at", "invalid".to_string()),
        ("created_at", "invalid".to_string()),
        ("created_at", (Utc::now() + Duration::hours(1)).to_rfc3339()),
        ("expires_at", (Utc::now() - Duration::hours(1)).to_rfc3339()),
    ] {
        let f = Fixture::new();
        let engine = f.engine(|_| {});
        let before = f.rows();
        f.db.conn()
            .execute(&format!("UPDATE sessions SET {field}=?1"), [value])
            .unwrap();
        assert!(engine.capture_pair_refresh_actor_policy(TOKEN).is_err());
        assert_eq!(f.rows(), before);
    }
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    for unknown in ["", "legacy-in-memory-admin", "bearer with whitespace"] {
        assert!(engine.capture_pair_refresh_actor_policy(unknown).is_err());
    }
}

#[test]
fn effective_engine_mode_size_lfs_and_projection_policies_are_not_overridden() {
    for case in 0..7 {
        let f = Fixture::new();
        let engine = f.engine(|config| match case {
            0 => config.sync.mode = SyncMode::Pr,
            1 => config.sync.sync_branches = vec!["parent".into()],
            2 => config.sync.max_file_size = 1,
            3 => config.sync.lfs_threshold = 1,
            4 => config.sync.lfs_patterns = vec!["*.bin".into()],
            5 => config.sync.ignore_patterns = vec!["private/**".into()],
            6 => config.sync.sync_branches = vec!["child".into(), "parent".into()],
            _ => unreachable!(),
        });
        let before = f.rows();
        assert!(
            engine.capture_pair_refresh_actor_policy(TOKEN).is_err(),
            "case {case}"
        );
        assert_eq!(f.rows(), before);
    }
    for case in 0..3 {
        let f = Fixture::new();
        let mut engine = f.engine(|_| {});
        match case {
            0 => engine.set_path_rules(vec!["src".into()], vec![]),
            1 => engine.set_path_rules(vec![], vec!["private/**".into()]),
            2 => engine.set_lfs_threshold_bytes(1),
            _ => unreachable!(),
        }
        assert!(engine.capture_pair_refresh_actor_policy(TOKEN).is_err());
    }
}

#[test]
fn established_key_rotation_or_missing_key_invalidates_context_without_key_creation() {
    let f = Fixture::new();
    let engine = f.engine(|_| {});
    let authority = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    f.db.set_state("encryption_key", &"42".repeat(32)).unwrap();
    assert!(authority.require_current(&engine).is_err());
    f.db.conn()
        .execute("DELETE FROM kv_state WHERE key='encryption_key'", [])
        .unwrap();
    let before = f.rows();
    assert!(authority.require_current(&engine).is_err());
    assert!(engine.capture_pair_refresh_actor_policy(TOKEN).is_err());
    assert_eq!(f.rows(), before);
}

#[test]
fn relevant_effective_settings_are_bound_without_serializing_configuration_secrets() {
    let f = Fixture::new();
    let first = f.engine(|config| config.github.token = Some("private-config-token".into()));
    let second = f.engine(|config| config.sync.auto_merge = false);
    let third = f.engine(|config| config.sync.sync_tags = false);
    let pin = first.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    assert!(!serde_json::to_string(pin.pin())
        .unwrap()
        .contains("private-config-token"));
    for engine in [&second, &third] {
        let next = engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
        assert_eq!(
            next.pin().session_context_digest,
            pin.pin().session_context_digest
        );
        assert_ne!(
            next.pin().policy_context_digest,
            pin.pin().policy_context_digest
        );
    }
}
