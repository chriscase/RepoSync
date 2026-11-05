//! Runs the same inert #117 regression suite in ordinary workspace CI.
//! No SSH, SCP, npm, compiler, service or installed daemon is actually invoked.

use std::io::Write;
use std::path::Path;
use std::process::Command;

#[test]
fn rs117_deployment_dry_run_and_failure_contracts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let result = Command::new("python3")
        .arg(root.join("scripts/test_deploy.py"))
        .current_dir(&root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("Python 3 is required for the inert deployment regression suite");
    // Preserve the nested named results in hosted logs, including on success.
    std::io::stdout()
        .write_all(&result.stdout)
        .expect("retain deployment test stdout");
    std::io::stderr()
        .write_all(&result.stderr)
        .expect("retain deployment test stderr");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "inert deployment checks failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        stderr
    );
    assert!(
        stderr.contains("Ran 35 tests") && stderr.trim_end().ends_with("OK"),
        "require the complete, nonempty #117 suite: {stderr}"
    );
}
