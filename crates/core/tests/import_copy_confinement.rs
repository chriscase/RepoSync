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

#[tokio::test]
async fn rs88_real_pinned_snapshot_uses_raw_export_expectations_and_preserves_projection() {
    use reposync_core::snapshot::{
        materialize_snapshot, project_snapshot_tree, resolve_snapshot_pin,
        verify_commit_matches_projection, verify_projected_snapshot, SnapshotRevision,
    };
    use reposync_core::svn::SvnClient;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    fn run(program: &str, args: &[&std::ffi::OsStr], cwd: &std::path::Path) {
        let out = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("svn");
    run(
        "svnadmin",
        &["create".as_ref(), repo.as_os_str()],
        temp.path(),
    );
    let url = reqwest::Url::from_file_path(&repo).unwrap().to_string();
    let wc = temp.path().join("wc");
    run(
        "svn",
        &["checkout".as_ref(), url.as_ref(), wc.as_os_str()],
        temp.path(),
    );
    std::fs::create_dir_all(wc.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(wc.join("nested")).unwrap();
    for (name, body) in [
        (".gitignore", b"target/\n".as_slice()),
        (".editorconfig", b"root = true\n"),
        (".github/workflows/build.yml", b"name: synthetic\n"),
        ("nested/.dot", b"nested\n"),
        ("binary", b"\0\xff\xfe\x01"),
        ("run.sh", b"#!/bin/sh\nexit 0\n"),
        ("omit.tmp", b"policy excluded\n"),
    ] {
        std::fs::write(wc.join(name), body).unwrap();
    }
    run(
        "svn",
        &[
            "add".as_ref(),
            "--".as_ref(),
            ".gitignore".as_ref(),
            ".editorconfig".as_ref(),
            ".github".as_ref(),
            "nested".as_ref(),
            "binary".as_ref(),
            "run.sh".as_ref(),
            "omit.tmp".as_ref(),
        ],
        &wc,
    );
    run(
        "svn",
        &[
            "propset".as_ref(),
            "svn:executable".as_ref(),
            "*".as_ref(),
            "--".as_ref(),
            "run.sh".as_ref(),
        ],
        &wc,
    );
    run(
        "svn",
        &[
            "commit".as_ref(),
            "-m".as_ref(),
            "synthetic pinned tree".as_ref(),
        ],
        &wc,
    );
    let client = SvnClient::new(&url, "", "");
    let pin = resolve_snapshot_pin(&client, SnapshotRevision::Head)
        .await
        .unwrap();
    assert_eq!(pin.operative_rev, 1);
    let git_dir = temp.path().join("git");
    let git = git2::Repository::init(&git_dir).unwrap();
    let database = test_db();
    let policy = FilePolicy::new(0, vec!["*.tmp".into()]);
    let stats = materialize_snapshot(&client, &git_dir, &policy, &database, &pin)
        .await
        .unwrap();
    assert_eq!(stats.copied, 6);
    assert_eq!(stats.skipped, 1);
    assert!(!git_dir.join("omit.tmp").exists());
    assert_eq!(
        std::fs::read(git_dir.join("binary")).unwrap(),
        b"\0\xff\xfe\x01"
    );
    assert_ne!(
        std::fs::metadata(git_dir.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o100,
        0
    );
    assert!(
        verify_projected_snapshot(&client, &git_dir, &policy, &database, &pin)
            .await
            .unwrap()
            .verified
    );
    let mut index = git.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = git.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Synthetic", "synthetic@example.invalid").unwrap();
    let commit = git
        .commit(Some("HEAD"), &signature, &signature, "pinned", &tree, &[])
        .unwrap();
    // Advance HEAD after capture: expected content must remain bound to r1.
    std::fs::write(wc.join("binary"), b"later").unwrap();
    run(
        "svn",
        &["commit".as_ref(), "-m".as_ref(), "later revision".as_ref()],
        &wc,
    );
    let raw = project_snapshot_tree(&client, &policy, &database, &pin)
        .await
        .unwrap();
    assert!(
        raw.path().join("omit.tmp").exists(),
        "expectations must retain raw policy input"
    );
    assert_eq!(
        std::fs::read(raw.path().join("binary")).unwrap(),
        b"\0\xff\xfe\x01"
    );
    assert!(
        verify_commit_matches_projection(raw.path(), &git_dir, &commit.to_string(), &policy)
            .unwrap()
            .verified
    );
    assert!(
        verify_projected_snapshot(&client, &git_dir, &policy, &database, &pin)
            .await
            .unwrap()
            .verified
    );
    std::fs::write(git_dir.join("binary"), b"wrong").unwrap();
    assert!(
        verify_projected_snapshot(&client, &git_dir, &policy, &database, &pin)
            .await
            .is_err()
    );
    // Post-commit verification uses the committed tree, not this dirty file.
    assert!(
        verify_commit_matches_projection(raw.path(), &git_dir, &commit.to_string(), &policy)
            .unwrap()
            .verified
    );
}
