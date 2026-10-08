//! Confinement regressions for import copy-path publish.

use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::import::copy_tree_with_policy;
use std::fs::hard_link;
use std::os::unix::fs::symlink;

fn test_db() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn noop_policy() -> FilePolicy {
    FilePolicy::new(0, vec![])
}

#[test]
fn import_copy_parent_dir_symlink_outside_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("canary"), "UNCHANGED").unwrap();
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/payload.txt"), "copied").unwrap();
    symlink(&outside, dst.join("sub")).unwrap();

    copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap();

    assert!(
        !outside.join("payload.txt").exists(),
        "must not write through a planted parent-directory symlink"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("canary")).unwrap(),
        "UNCHANGED"
    );
    assert_eq!(
        std::fs::read_to_string(dst.join("sub/payload.txt")).unwrap(),
        "copied"
    );
}

#[test]
fn import_copy_dest_hardlink_outside_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    let outside = tmp.path().join("outside-secret");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(&outside, "ORIGINAL").unwrap();
    std::fs::write(src.join("payload.txt"), "copied").unwrap();
    hard_link(&outside, dst.join("payload.txt")).unwrap();

    copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap();

    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "ORIGINAL",
        "must not mutate bytes reachable through a planted destination hardlink"
    );
    assert_eq!(
        std::fs::read_to_string(dst.join("payload.txt")).unwrap(),
        "copied"
    );
}

#[test]
fn import_copy_gitattributes_hardlink_outside_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    let outside = tmp.path().join("outside-secret");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(&outside, "ORIGINAL-ATTRS").unwrap();
    std::fs::write(src.join(".gitattributes"), "* text=auto\n").unwrap();
    std::fs::write(src.join("readme.txt"), "hello").unwrap();
    hard_link(&outside, dst.join(".gitattributes")).unwrap();

    copy_tree_with_policy(&src, &dst, &noop_policy(), &test_db()).unwrap();

    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "ORIGINAL-ATTRS",
        "export-present .gitattributes merge must not mutate a planted hardlink target"
    );
    let merged = std::fs::read_to_string(dst.join(".gitattributes")).unwrap();
    assert_eq!(merged, "* text=auto\n");
}
