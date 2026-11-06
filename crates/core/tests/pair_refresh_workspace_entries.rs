//! Ordinary SCM entries keep their own permissions and link semantics.
use std::fs::DirBuilder;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::path::Path;
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
    _directory: tempfile::TempDir,
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
            _directory: directory,
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

fn entries(root: &Path, foreign: &Path) {
    let nested = root.join("nested");
    DirBuilder::new().mode(0o775).create(&nested).unwrap();
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o775)).unwrap();
    std::fs::write(nested.join("file"), "owned").unwrap();
    std::fs::set_permissions(nested.join("file"), std::fs::Permissions::from_mode(0o664)).unwrap();
    symlink(foreign, nested.join("directory-link")).unwrap();
    symlink(foreign.join("marker"), nested.join("file-link")).unwrap();
    symlink(foreign.join("absent"), nested.join("dangling-link")).unwrap();
    std::fs::hard_link(foreign.join("marker"), nested.join("hard-link")).unwrap();
}

#[test]
fn unpublished_cleanup_unlinks_links_and_permissive_entries_without_touching_targets() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let workspace = context.create_workspace(&f.engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("marker"), "foreign").unwrap();
    entries(&root, foreign.path());
    let before = f.rows();
    workspace.require_current().unwrap();
    drop(workspace);
    assert!(!root.exists());
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(foreign.path().join("marker")).unwrap(),
        "foreign"
    );
    assert_eq!(f.rows(), before);
}

#[test]
fn retention_syncs_ordinary_entries_and_preserves_link_targets_and_modes() {
    let f = Fixture::new();
    let context = f.engine.capture_pair_refresh_actor_policy(TOKEN).unwrap();
    let workspace = context.create_workspace(&f.engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("marker"), "foreign").unwrap();
    entries(&root, foreign.path());
    let before = f.rows();
    context
        .retain_workspace_before_enrollment(&f.engine, &workspace)
        .unwrap();
    drop(workspace);
    assert_eq!(
        std::fs::read_to_string(root.join("nested/file")).unwrap(),
        "owned"
    );
    assert_eq!(
        std::fs::metadata(root.join("nested/file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o664
    );
    assert_eq!(
        std::fs::metadata(root.join("nested"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o775
    );
    assert_eq!(
        std::fs::read_link(root.join("nested/directory-link")).unwrap(),
        foreign.path()
    );
    assert!(std::fs::symlink_metadata(root.join("nested/dangling-link"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(root.join("nested/hard-link")).unwrap(),
        "foreign"
    );
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(foreign.path().join("marker")).unwrap(),
        "foreign"
    );
    assert_eq!(f.rows(), before);
}
