//! The reviewed hook scripts and the copies the tests name must not drift.
#[path = "fixtures/image_hook.rs"]
mod image_hook;

use image_hook::script;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

// Scripts whose bytes are held by the suites that predate the shared installer.
const HELD_ELSEWHERE: [(&str, &str); 2] =
    [("r04-gate", "pre-receive"), ("r65-deny", "pre-receive")];

fn reviewed_root() -> PathBuf {
    match std::env::var_os("REPOSYNC_FIXTURE_HOOKS") {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/reliability-hooks"),
    }
}

#[test]
fn reviewed_hook_scripts_match_the_tests_and_nothing_else_is_shipped() {
    let named = [
        (script::COUNT_REJECT, "pre-receive"),
        (script::COUNT_HOLD, "pre-receive"),
        (script::REF_MOVE, "pre-receive"),
        (script::REJECT, "pre-receive"),
        (script::WITNESS_REJECT, "pre-receive"),
        (script::PROTECTED_REF, "pre-receive"),
        (script::R17_HOLD, "pre-receive"),
        (script::R17_HOLD_REJECT, "pre-receive"),
        (script::SVN_PERMISSION_REFUSAL, "pre-commit"),
        (script::SVN_REJECT_AFTER_GIT, "pre-commit"),
        (script::SVN_HOLD_HTTP_DISCONNECT, "pre-commit"),
        (script::SVN_HOLD_SHUTDOWN, "pre-commit"),
    ];
    let root = reviewed_root();
    let mut expected = BTreeSet::new();
    for ((name, body), hook) in named {
        let path = root.join(name).join(hook);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("{} is missing: {error}", path.display()));
        assert_eq!(bytes, body.as_bytes(), "{name}/{hook} differs");
        assert!(expected.insert(format!("{name}/{hook}")), "{name} twice");
    }
    for (name, hook) in HELD_ELSEWHERE {
        assert!(expected.insert(format!("{name}/{hook}")));
    }
    let mut shipped = BTreeSet::new();
    for directory in std::fs::read_dir(&root).unwrap() {
        let directory = directory.unwrap();
        assert!(directory.file_type().unwrap().is_dir());
        for file in std::fs::read_dir(directory.path()).unwrap() {
            let file = file.unwrap();
            let metadata = std::fs::symlink_metadata(file.path()).unwrap();
            assert!(
                metadata.is_file(),
                "{:?} is not a regular file",
                file.path()
            );
            assert_eq!(
                metadata.permissions().mode() & 0o111,
                0o111,
                "{:?} is not executable",
                file.path()
            );
            shipped.insert(format!(
                "{}/{}",
                directory.file_name().to_string_lossy(),
                file.file_name().to_string_lossy()
            ));
        }
    }
    assert_eq!(shipped, expected);
}
