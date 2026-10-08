//! Static UI contract checks for #65 RS-16 disable vs remove vs remote deletion.

use std::path::Path;
use std::process::Command;

fn node_command() -> Command {
    let bin = std::env::var("REPOSYNC_NODE_BIN").unwrap_or_else(|_| "node".into());
    Command::new(bin)
}

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
    let output = node_command()
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
        src.contains("removeBranchOpts"),
        "Branch remote deletion must default to false in managed removal UI"
    );
    assert!(
        src.contains("delete_git: false, delete_svn: false"),
        "Branch remote deletion must default to false in UI"
    );
    assert!(
        src.contains("removalPreviewReady"),
        "Remove confirm must wait for dependency preview to load"
    );
    assert!(
        src.contains("data-testid=\"removal-preview-retry\""),
        "Remove confirm must offer retry when preview fails"
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
        repos_list.contains("shouldDisplayManagedRemovalReceipt"),
        "Repositories list must gate managed-removal receipt rendering"
    );
    assert!(
        repos_list.contains("readAllManagedRemovalReceipts"),
        "Repositories list must show every persisted managed-removal receipt"
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
    assert!(
        panel.contains("managed-removal-restore"),
        "Managed removal panel must expose restore control"
    );
    assert!(
        src.contains("managed-removal-dependency-preview"),
        "Remove confirm must render dependency preview from the API"
    );
    assert!(
        src.contains("getRemovalDependencyPreview"),
        "Remove confirm must load dependency preview before confirmation"
    );
    assert!(
        src.contains("branchPairPreviewQuery"),
        "Branch pair remove confirm must load child dependency preview"
    );
    assert!(
        src.contains("branch-removal-preview-unavailable"),
        "Branch pair remove must show proceed-without-preview when preview fails"
    );
    let managed_src = include_str!("../../../web-ui/src/managedRemoval.ts");
    assert!(
        managed_src.contains("removalDependencyPreviewConfirmReady"),
        "Preview gate must allow confirm after failed preview fetch"
    );
    assert!(
        src.contains("trackedRemovalOperationId"),
        "Managed removal status must be keyed by operation id"
    );
    let api_src = include_str!("../../../web-ui/src/api.ts");
    assert!(
        api_src.contains("operation_id=${encodeURIComponent(operationId)}"),
        "GET /removal must accept operation_id for receipt round-trip"
    );
    let managed_src = include_str!("../../../web-ui/src/managedRemoval.ts");
    assert!(
        managed_src.contains("localStorage.setItem"),
        "Managed removal receipts must persist in localStorage"
    );
    let notice = include_str!("../../../web-ui/src/components/ManagedRemovalReceiptNotice.tsx");
    assert!(
        notice.contains("managed-removal-receipt-partial-cleanup"),
        "Receipt notice must surface partial cleanup"
    );
    assert!(
        src.contains("removalDependencyPreview?.parent_removal_blocked"),
        "Managed remove confirm must honor parent_removal_blocked"
    );
    assert!(
        src.contains("branchPairDependencyPreview?.parent_removal_blocked"),
        "Branch pair remove confirm must honor parent_removal_blocked"
    );
    assert!(
        managed_src.contains("shouldPersistPolledManagedRemovalReceipt"),
        "Polled removal status must not overwrite cleared or newer receipts"
    );
}

#[test]
fn managed_removal_receipt_display_gate_and_roundtrip() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = r#"
const store = new Map();
globalThis.localStorage = {
  getItem(key) { return store.has(key) ? store.get(key) : null; },
  setItem(key, value) { store.set(key, String(value)); },
  removeItem(key) { store.delete(key); },
  key(index) { return Array.from(store.keys())[index] ?? null; },
  get length() { return store.size; },
};
globalThis.sessionStorage = {
  getItem(key) { return store.has(key) ? store.get(key) : null; },
  setItem(key, value) { store.set(key, String(value)); },
  removeItem(key) { store.delete(key); },
};
import {
  persistManagedRemovalReceipt,
  readManagedRemovalReceipt,
  shouldDisplayManagedRemovalReceipt,
  clearManagedRemovalReceipt,
  removalDependencyPreviewConfirmReady,
  shouldPersistPolledManagedRemovalReceipt,
  readAllManagedRemovalReceipts,
} from './web-ui/src/managedRemoval.ts';

if (removalDependencyPreviewConfirmReady({ isLoading: true, isFetching: false, isError: false, dependencyPreview: null })) process.exit(8);
if (!removalDependencyPreviewConfirmReady({ isLoading: false, isFetching: false, isError: true, dependencyPreview: null })) process.exit(9);
if (removalDependencyPreviewConfirmReady({ isLoading: false, isFetching: false, isError: false, dependencyPreview: null })) process.exit(11);
if (removalDependencyPreviewConfirmReady({ isLoading: false, isFetching: true, isError: true, dependencyPreview: null })) process.exit(12);
if (!removalDependencyPreviewConfirmReady({ isLoading: false, isFetching: false, isError: false, dependencyPreview: { repo_id: 'x', repo_name: 'x', parent: null, children: [], parent_removal_blocked: false, block_reason: null, credentials: [], managed_local_path: 'p', sibling_local_paths_preserved: [], shared_git_registrations: [] } })) process.exit(10);

const storedReceipt = {
  repoId: 'repo-a',
  operationId: 'op-1',
  state: 'failed',
  message: 'x',
  remote_git: 'failed',
  remote_svn: 'untouched',
  restore_supported: false,
  retryable: true,
  registration_listed: true,
  updated_at: '2026-10-08T12:00:00.000Z',
};
if (shouldPersistPolledManagedRemovalReceipt(null, { operation_id: 'op-1', updated_at: '2026-10-08T13:00:00.000Z' })) process.exit(13);
if (shouldPersistPolledManagedRemovalReceipt(storedReceipt, { operation_id: 'op-2' })) process.exit(14);
if (shouldPersistPolledManagedRemovalReceipt(storedReceipt, { operation_id: 'op-1', updated_at: '2026-10-08T11:00:00.000Z' })) process.exit(15);
if (!shouldPersistPolledManagedRemovalReceipt(storedReceipt, { operation_id: 'op-1', updated_at: '2026-10-08T13:00:00.000Z' })) process.exit(16);

if (shouldDisplayManagedRemovalReceipt(null)) process.exit(2);
if (shouldDisplayManagedRemovalReceipt({ repoId: '', operationId: 'op', state: 'completed', message: '' })) process.exit(3);

const receipt = {
  repoId: 'repo-a',
  operationId: 'op-1',
  state: 'failed',
  message: 'done',
  remote_git: 'deleted',
  remote_svn: 'untouched',
  restore_supported: false,
  retryable: true,
  registration_listed: true,
  partial_cleanup: {
    outcome_detail: 'cleanup failed',
    registration_listed: true,
    remote_git: 'deleted',
    remote_svn: 'untouched',
    retry_is_local_cleanup_only: true,
  },
  updated_at: '2026-10-08T00:00:00.000Z',
};
if (!shouldDisplayManagedRemovalReceipt(receipt)) process.exit(4);
persistManagedRemovalReceipt(receipt);
const restored = readManagedRemovalReceipt('repo-a');
if (!restored || restored.operationId !== 'op-1') process.exit(5);
if (!restored.partial_cleanup || restored.partial_cleanup.remote_git !== 'deleted') process.exit(7);
persistManagedRemovalReceipt({
  ...receipt,
  repoId: 'repo-b',
  operationId: 'op-2',
  updated_at: '2026-10-08T01:00:00.000Z',
});
const all = readAllManagedRemovalReceipts();
if (all.length !== 2) process.exit(17);
if (all[0].repoId !== 'repo-b' || all[1].repoId !== 'repo-a') process.exit(18);
clearManagedRemovalReceipt('repo-a');
if (readManagedRemovalReceipt('repo-a') !== null) process.exit(6);
"#;
    let output = node_command()
        .arg("--experimental-strip-types")
        .arg("--input-type=module")
        .arg("-e")
        .arg(script)
        .current_dir(&root)
        .output()
        .expect("spawn managed removal receipt behavioral check");
    assert!(
        output.status.success(),
        "managed removal receipt behavioral check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
