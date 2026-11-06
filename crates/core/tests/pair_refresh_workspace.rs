//! Current storage and owned-resource lifecycle are checked without job effects.
use std::fs::DirBuilder;
use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::client::GitClient;
use reposync_core::identity::mapper::IdentityMapper;
use reposync_core::models::{Session, User};
use reposync_core::svn::client::SvnClient;
use reposync_core::sync_engine::SyncEngine;

const TOKEN: &str = "disposable-workspace-admin-session";
const NAMESPACE: &str = "pair-refresh-resources-v2";

fn seed(db: &Database) {
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
}

fn engine(db: Database, directory: &Path, git: &Path) -> SyncEngine {
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
    config.daemon.data_dir = directory.to_owned();
    git2::Repository::init(git).unwrap();
    SyncEngine::new(
        config,
        db,
        SvnClient::new("file:///unused/svn", "fixture", ""),
        GitClient::new(git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    )
}

struct Fixture {
    directory: tempfile::TempDir,
    db: Database,
    engine: SyncEngine,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::new(directory.path().join("reposync.db")).unwrap();
        seed(&db);
        let engine = engine(
            Database::new(directory.path().join("reposync.db")).unwrap(),
            directory.path(),
            &directory.path().join("git-work"),
        );
        Self {
            directory,
            db,
            engine,
        }
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
fn owned_server_workspace_is_private_bound_and_cleans_only_after_last_owner_drop() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let before = f.rows();
    let workspace = context.create_workspace(&f.engine).unwrap();
    let pin = workspace.pin();
    let root = pin.operation_directory.path.clone();
    assert_eq!(pin.version, 2);
    assert!(!pin.operation_id.is_nil());
    assert_eq!(
        root,
        f.directory
            .path()
            .canonicalize()
            .unwrap()
            .join(NAMESPACE)
            .join(pin.operation_id.to_string())
    );
    assert_eq!(
        pin.database.path,
        f.directory
            .path()
            .join("reposync.db")
            .canonicalize()
            .unwrap()
    );
    for path in [&pin.namespace.path, &root] {
        assert_eq!(
            std::fs::symlink_metadata(path).unwrap().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(pin.actor_policy, *context.pin());
    assert_eq!(f.rows(), before);
    let other = workspace.clone();
    drop(workspace);
    assert!(root.is_dir());
    other.require_current().unwrap();
    drop(other);
    assert!(!root.exists());
    assert!(f.directory.path().join(NAMESPACE).is_dir());
    assert_eq!(f.rows(), before);
}

#[test]
fn retention_before_an_attempt_survives_all_handle_drops_and_database_reopen() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let workspace = context.create_workspace(&f.engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    std::fs::write(root.join("retained-data"), "original").unwrap();
    let before = f.rows();
    context
        .retain_workspace_before_enrollment(&f.engine, &workspace)
        .unwrap();
    drop(workspace);
    assert_eq!(
        std::fs::read_to_string(root.join("retained-data")).unwrap(),
        "original"
    );
    let reopened = Database::new(f.directory.path().join("reposync.db")).unwrap();
    assert!(root.is_dir());
    assert_eq!(f.rows(), before);
    assert!(reopened
        .get_state("pair_refresh_operations_v1:document:none")
        .unwrap()
        .is_none());
}

#[test]
fn wrong_data_root_and_memory_storage_refuse_before_namespace_creation() {
    let f = Fixture::new();
    let foreign = tempfile::tempdir().unwrap();
    let wrong = engine(
        Database::new(f.directory.path().join("reposync.db")).unwrap(),
        foreign.path(),
        &f.directory.path().join("git-other"),
    );
    let context = wrong.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    assert!(context.create_workspace(&wrong).is_err());
    assert!(!foreign.path().join(NAMESPACE).exists());
    assert!(!f.directory.path().join(NAMESPACE).exists());
    let memory = Database::in_memory().unwrap();
    seed(&memory);
    let memory_engine = engine(
        memory,
        f.directory.path(),
        &f.directory.path().join("git-memory"),
    );
    let context = memory_engine
        .capture_pair_refresh_actor_policy(TOKEN)
        .unwrap();
    assert!(context.create_workspace(&memory_engine).is_err());
    assert!(!f.directory.path().join(NAMESPACE).exists());
}

#[test]
fn symlinked_namespace_or_database_is_never_adopted() {
    let f = Fixture::new();
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("marker"), "foreign").unwrap();
    symlink(foreign.path(), f.directory.path().join(NAMESPACE)).unwrap();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    assert!(context.create_workspace(&f.engine).is_err());
    assert_eq!(
        std::fs::read_to_string(foreign.path().join("marker")).unwrap(),
        "foreign"
    );
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 1);
    let alias = f.directory.path().join("alias.db");
    symlink(f.directory.path().join("reposync.db"), &alias).unwrap();
    let aliased = engine(
        Database::new(&alias).unwrap(),
        f.directory.path(),
        &f.directory.path().join("git-alias"),
    );
    let context = aliased.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    assert!(context.create_workspace(&aliased).is_err());
}

#[test]
fn connected_database_replacement_refuses_resources_before_namespace_creation() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let path = f.directory.path().join("reposync.db");
    let moved = f.directory.path().join("old-connected.db");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::copy(&moved, &path).unwrap();
    assert!(context.create_workspace(&f.engine).is_err());
    assert!(!f.directory.path().join(NAMESPACE).exists());
    assert!(moved.is_file());
    assert!(path.is_file());
}

#[test]
fn changed_resource_identity_retains_both_original_and_foreign_replacement() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let workspace = context.create_workspace(&f.engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    let moved: PathBuf = root.with_extension("moved");
    std::fs::write(root.join("original"), "owned").unwrap();
    std::fs::rename(&root, &moved).unwrap();
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    std::fs::write(root.join("foreign"), "foreign").unwrap();
    assert!(workspace.require_current().is_err());
    assert!(context
        .retain_workspace_before_enrollment(&f.engine, &workspace)
        .is_err());
    drop(workspace);
    assert_eq!(
        std::fs::read_to_string(root.join("foreign")).unwrap(),
        "foreign"
    );
    assert_eq!(
        std::fs::read_to_string(moved.join("original")).unwrap(),
        "owned"
    );
}

#[test]
fn actor_logout_prevents_retention_and_unpublished_resources_are_cleaned() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let workspace = context.create_workspace(&f.engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    f.db.delete_session(TOKEN).unwrap();
    assert!(context
        .retain_workspace_before_enrollment(&f.engine, &workspace)
        .is_err());
    drop(workspace);
    assert!(!root.exists());
}

#[test]
fn hardlinked_database_and_permissive_namespace_refuse_without_adoption() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    std::fs::hard_link(
        f.directory.path().join("reposync.db"),
        f.directory.path().join("hardlink.db"),
    )
    .unwrap();
    assert!(context.create_workspace(&f.engine).is_err());
    assert!(!f.directory.path().join(NAMESPACE).exists());
    std::fs::remove_file(f.directory.path().join("hardlink.db")).unwrap();
    DirBuilder::new()
        .mode(0o755)
        .create(f.directory.path().join(NAMESPACE))
        .unwrap();
    assert!(context.create_workspace(&f.engine).is_err());
    assert_eq!(
        std::fs::read_dir(f.directory.path().join(NAMESPACE))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        std::fs::symlink_metadata(f.directory.path().join(NAMESPACE))
            .unwrap()
            .mode()
            & 0o777,
        0o755
    );
}
