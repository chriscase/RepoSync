//! Static UI contract checks for #65 RS-16 disable vs remove vs remote deletion.

use std::path::Path;
use std::process::Command;

#[test]
fn branch_pair_delete_query_defaults_explicit_safe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = r#"
import { buildBranchPairDeleteQuery } from './web-ui/src/branchPairDeletion.ts';
const empty = buildBranchPairDeleteQuery();
const omitted = buildBranchPairDeleteQuery({});
if (empty.get('explicit_remote_deletion_opts') !== 'true') process.exit(2);
if (empty.get('delete_git') !== 'false' || empty.get('delete_svn') !== 'false') process.exit(3);
if (omitted.get('delete_git') !== 'false' || omitted.get('delete_svn') !== 'false') process.exit(4);
const on = buildBranchPairDeleteQuery({ delete_git: true, delete_svn: false });
if (on.get('delete_git') !== 'true' || on.get('delete_svn') !== 'false') process.exit(5);
"#;
    let output = Command::new("node")
        .arg("--experimental-strip-types")
        .arg("--input-type=module")
        .arg("-e")
        .arg(script)
        .current_dir(&root)
        .output()
        .expect("spawn branch pair delete query check");
    assert!(
        output.status.success(),
        "branch pair delete query contract failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn repo_detail_ui_distinguishes_pause_disable_and_managed_remove() {
    let src = include_str!("../../../web-ui/src/pages/RepoDetail.tsx");
    assert!(
        src.contains("data-testid=\"pause-disable-repo\""),
        "Pause/disable control must be explicit"
    );
    assert!(
        src.contains("data-testid=\"remove-from-reposync\""),
        "Managed removal must be separate from disable"
    );
    assert!(
        src.contains("api.disableRepo"),
        "UI must call explicit disable API"
    );
    assert!(
        src.contains("api.removeManagedRepo"),
        "UI must call managed removal API"
    );
    assert!(
        !src.contains("Permanently remove this repository configuration"),
        "Root delete copy must not promise permanent removal while disabling"
    );
    assert!(
        src.contains("delete_git: false, delete_svn: false"),
        "Branch remote deletion must default to false in UI"
    );
    let query_builder = include_str!("../../../web-ui/src/branchPairDeletion.ts");
    assert!(
        query_builder.contains("explicit_remote_deletion_opts"),
        "Branch pair DELETE query builder must send explicit remote opts"
    );
    assert!(
        src.contains("ManagedRemovalPanel"),
        "Managed removal states must render in the detail view"
    );
    let repos_list = include_str!("../../../web-ui/src/pages/Repositories.tsx");
    assert!(
        repos_list.contains("readManagedRemovalReceipt"),
        "Repositories list must restore managed-removal receipts after navigation or reload"
    );
    assert!(
        repos_list.contains("ManagedRemovalReceiptNotice"),
        "Repositories list must render managed-removal receipt notice"
    );
    let panel = include_str!("../../../web-ui/src/components/ManagedRemovalPanel.tsx");
    assert!(
        panel.contains("data-testid=\"managed-removal-panel\""),
        "Managed removal panel must expose state for tests"
    );
    assert!(
        panel.contains("Retry only retries owned local cleanup"),
        "Retry copy must not imply remote deletion"
    );
}
