//! Filesystem role controls; these do not assert actor or job authority.
use std::os::unix::fs::{symlink, PermissionsExt};

use super::*;

fn fixture() -> (tempfile::TempDir, Database, RefreshWorkspace) {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::new(directory.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let actor = RefreshActorPolicyPin {
        version: 2,
        actor_id: "filesystem-control".into(),
        session_context_digest: "31".repeat(32),
        policy_context_digest: "41".repeat(32),
    };
    let workspace = create(&db, directory.path(), &actor).unwrap();
    (directory, db, workspace)
}

#[test]
fn generated_svn_role_roots_become_private_without_changing_ordinary_tree_modes() {
    let (_directory, _db, workspace) = fixture();
    let svn = workspace.create_directory("svn").unwrap();
    let operation = svn.join(workspace.pin().operation_id.to_string());
    let child = operation.join("child");
    std::fs::create_dir_all(&child).unwrap();
    for path in [&operation, &child] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o775)).unwrap();
    }
    let ordinary = child.join("ordinary");
    std::fs::write(&ordinary, "versioned").unwrap();
    std::fs::set_permissions(&ordinary, std::fs::Permissions::from_mode(0o664)).unwrap();
    workspace.track_working_copy(&child).unwrap();
    for path in [&operation, &child] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        std::fs::metadata(&ordinary).unwrap().permissions().mode() & 0o777,
        0o664
    );
    assert_eq!(workspace.resource_pin().unwrap().directories.len(), 3);
    workspace.require_current().unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    drop(workspace);
    assert!(!root.exists());
}

#[test]
fn generated_role_symlink_refuses_without_changing_or_removing_its_foreign_target() {
    let (_directory, _db, workspace) = fixture();
    let svn = workspace.create_directory("svn").unwrap();
    let operation = svn.join(workspace.pin().operation_id.to_string());
    std::fs::create_dir(&operation).unwrap();
    let foreign = tempfile::tempdir().unwrap();
    std::fs::set_permissions(foreign.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
    std::fs::write(foreign.path().join("marker"), "foreign").unwrap();
    symlink(foreign.path(), operation.join("child")).unwrap();
    assert!(workspace
        .track_working_copy(&operation.join("child"))
        .is_err());
    assert_eq!(
        std::fs::metadata(foreign.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o775
    );
    let root = workspace.pin().operation_directory.path.clone();
    drop(workspace);
    assert!(!root.exists());
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(foreign.path().join("marker")).unwrap(),
        "foreign"
    );
}
