//! Native SVN fixture contract, not an end-to-end sync acceptance test.

use std::path::Path;
use std::process::Command;

#[test]
fn monorepo_fixture_has_mixed_and_excluded_revisions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("../../tests/fixtures/monorepo/test_history.py");
    let status = Command::new("python3")
        .arg("-B")
        .arg(script)
        .status()
        .expect("run the #60 fixture check with Python 3 and the native SVN tools");
    assert!(status.success(), "monorepo fixture check failed: {status}");
}
