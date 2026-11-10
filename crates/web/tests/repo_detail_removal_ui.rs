//! Static UI contract checks for #65 RS-16 disable vs remove vs remote deletion.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// Files the Node contracts execute. Their bytes are the relocated tree's identity.
const UI_IDENTITY_FILES: &[&str] = &[
    "web-ui/src/branchPairDeletion.ts",
    "web-ui/src/managedRemoval.ts",
];

const SYSTEM_NODE_FALLBACKS: &[&str] = &["/usr/bin/node", "/usr/bin/nodejs"];

fn compiled_repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// sha256 over each identity file as `relative || 0x00 || u64be(len) || bytes`.
fn ui_source_bind(root: &Path) -> Option<String> {
    let mut hasher = Sha256::new();
    for relative in UI_IDENTITY_FILES {
        let bytes = fs::read(root.join(relative)).ok()?;
        hasher.update(relative.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Some(hex::encode(hasher.finalize()))
}

/// Ordinary runs stay on the checkout that compiled this test.
/// A relocated sealed-image root is admitted only when
/// `REPOSYNC_UI_SOURCE_ROOT` is set and `REPOSYNC_UI_SOURCE_BIND` equals
/// [`ui_source_bind`] of that root.
fn resolve_repo_root(
    compiled: &Path,
    opted_root: Option<&str>,
    bind: Option<&str>,
) -> Result<PathBuf, String> {
    let Some(opted) = opted_root.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(compiled.to_path_buf());
    };
    let Some(bind) = bind.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(
            "relocated UI source requires REPOSYNC_UI_SOURCE_BIND to match its identity".into(),
        );
    };
    let root = PathBuf::from(opted);
    let actual = ui_source_bind(&root).ok_or_else(|| {
        format!(
            "relocated UI source {} is missing a bindable web-ui identity",
            root.display()
        )
    })?;
    if actual != bind {
        return Err(format!(
            "relocated UI source {} identity does not match REPOSYNC_UI_SOURCE_BIND",
            root.display()
        ));
    }
    Ok(root)
}

fn repo_root() -> PathBuf {
    resolve_repo_root(
        &compiled_repo_root(),
        std::env::var("REPOSYNC_UI_SOURCE_ROOT").ok().as_deref(),
        std::env::var("REPOSYNC_UI_SOURCE_BIND").ok().as_deref(),
    )
    .unwrap_or_else(|error| panic!("repo root: {error}"))
}

fn accepts_strip_types(bin: &Path) -> bool {
    Command::new(bin)
        .args(["--experimental-strip-types", "-e", "process.exit(0)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Explicit override, then a PATH `node` that accepts `--experimental-strip-types`,
/// then the first supported system binary that does.
fn select_node_runtime(
    override_bin: Option<&str>,
    path_dirs: &[PathBuf],
    system_fallbacks: &[PathBuf],
) -> Result<PathBuf, String> {
    if let Some(bin) = override_bin.map(str::trim).filter(|bin| !bin.is_empty()) {
        return Ok(PathBuf::from(bin));
    }
    for dir in path_dirs {
        let candidate = dir.join("node");
        if candidate.is_file() && accepts_strip_types(&candidate) {
            return Ok(candidate);
        }
    }
    for candidate in system_fallbacks {
        if candidate.is_file() && accepts_strip_types(candidate) {
            return Ok(candidate.clone());
        }
    }
    Err("no compatible Node runtime (override, PATH, or system fallback)".into())
}

fn node_command() -> Command {
    let override_bin = std::env::var("REPOSYNC_NODE_BIN").ok();
    let path_dirs: Vec<PathBuf> = std::env::var("PATH")
        .ok()
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let fallbacks = SYSTEM_NODE_FALLBACKS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let selected = select_node_runtime(override_bin.as_deref(), &path_dirs, &fallbacks)
        .unwrap_or_else(|error| panic!("node runtime: {error}"));
    Command::new(selected)
}

#[test]
fn branch_pair_delete_query_defaults_explicit_safe() {
    let root = repo_root();
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
    let root = repo_root();
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
if (removalDependencyPreviewConfirmReady({ isLoading: false, isFetching: false, isError: true, dependencyPreview: null })) process.exit(9);
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

fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn compatible_node_script() -> &'static str {
    "#!/bin/sh\nif [ \"$1\" = \"--experimental-strip-types\" ]; then exit 0; fi\nexit 1\n"
}

fn older_node_script() -> &'static str {
    "#!/bin/sh\nexit 1\n"
}

fn write_ui_root(root: &Path, behavior: &str) {
    let source = root.join("web-ui/src");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("branchPairDeletion.ts"),
        format!("export const rootBehavior = \"{behavior}\";\n"),
    )
    .unwrap();
    fs::write(
        source.join("managedRemoval.ts"),
        format!("export const side = \"{behavior}\";\n"),
    )
    .unwrap();
}

fn executed_ui_behavior(root: &Path) -> String {
    let path = std::env::var("PATH").unwrap_or_default();
    let path_dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let fallbacks = SYSTEM_NODE_FALLBACKS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let node = select_node_runtime(None, &path_dirs, &fallbacks)
        .expect("host Node is required to prove UI behavior");
    let script = r#"
import { rootBehavior } from './web-ui/src/branchPairDeletion.ts';
import { side } from './web-ui/src/managedRemoval.ts';
process.stdout.write(`${rootBehavior}:${side}`);
"#;
    let output = Command::new(node)
        .args([
            "--experimental-strip-types",
            "--input-type=module",
            "-e",
            script,
        ])
        .current_dir(root)
        .output()
        .expect("execute relocated UI behavior");
    assert!(
        output.status.success(),
        "ui behavior failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn node_runtime_prefers_compatible_path_over_older_system_node() {
    let dir = tempfile::tempdir().unwrap();
    let path_dir = dir.path().join("path");
    let system = dir.path().join("system");
    let path_node = path_dir.join("node");
    let system_nodejs = system.join("nodejs");
    write_executable(&path_node, compatible_node_script());
    write_executable(&system_nodejs, older_node_script());

    let selected = select_node_runtime(
        None,
        std::slice::from_ref(&path_dir),
        std::slice::from_ref(&system_nodejs),
    )
    .unwrap();
    assert_eq!(
        selected, path_node,
        "older system Node must not override PATH"
    );
}

#[test]
fn node_runtime_uses_nodejs_only_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path_dir = dir.path().join("path");
    fs::create_dir_all(&path_dir).unwrap();
    let system_nodejs = dir.path().join("nodejs");
    write_executable(&system_nodejs, compatible_node_script());

    let selected = select_node_runtime(
        None,
        std::slice::from_ref(&path_dir),
        std::slice::from_ref(&system_nodejs),
    )
    .unwrap();
    assert_eq!(selected, system_nodejs);
}

#[test]
fn node_runtime_honors_explicit_override() {
    let dir = tempfile::tempdir().unwrap();
    let path_dir = dir.path().join("path");
    let override_bin = dir.path().join("override-node");
    let path_node = path_dir.join("node");
    let system_nodejs = dir.path().join("nodejs");
    write_executable(&override_bin, older_node_script());
    write_executable(&path_node, compatible_node_script());
    write_executable(&system_nodejs, compatible_node_script());

    let selected = select_node_runtime(
        Some(override_bin.to_str().unwrap()),
        &[path_dir],
        &[system_nodejs],
    )
    .unwrap();
    assert_eq!(selected, override_bin);
}

#[test]
fn node_runtime_rejects_when_no_runtime_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path_dir = dir.path().join("path");
    fs::create_dir_all(&path_dir).unwrap();
    let missing_node = dir.path().join("missing-node");
    let missing_nodejs = dir.path().join("missing-nodejs");

    let error = select_node_runtime(None, &[path_dir], &[missing_node, missing_nodejs])
        .expect_err("absent runtimes must not fall through to a bare node name");
    assert!(
        error.contains("no compatible Node runtime"),
        "unexpected error: {error}"
    );
}

#[test]
fn ui_source_bind_matches_the_runtime_canonicalization() {
    let dir = tempfile::tempdir().unwrap();
    write_ui_root(dir.path(), "a");
    assert_eq!(
        ui_source_bind(dir.path()).as_deref(),
        Some("e2b7b767784ff9d3339e030731e4777ea4ddaf5d7f519329b42bf214eb742e71")
    );
}

#[test]
fn relocated_ui_source_requires_an_explicit_identity_binding() {
    let dir = tempfile::tempdir().unwrap();
    let compiled = dir.path().join("compiled-checkout");
    let other = dir.path().join("unrelated-src");
    let fixture = dir.path().join("sealed-image");
    write_ui_root(&compiled, "compiled-checkout");
    write_ui_root(&other, "unrelated-src");
    write_ui_root(&fixture, "sealed-image");
    assert_ne!(
        ui_source_bind(&compiled),
        ui_source_bind(&other),
        "distinct UI behavior must not share an identity"
    );
    assert_ne!(ui_source_bind(&compiled), ui_source_bind(&fixture));

    let ordinary = resolve_repo_root(&compiled, None, None).unwrap();
    assert_eq!(ordinary, compiled);
    assert_eq!(
        executed_ui_behavior(&ordinary),
        "compiled-checkout:compiled-checkout"
    );
    assert_eq!(
        executed_ui_behavior(&other),
        "unrelated-src:unrelated-src",
        "the decoy tree must actually behave differently"
    );

    let unbound = resolve_repo_root(&compiled, Some(other.to_str().unwrap()), None);
    assert!(
        unbound.is_err(),
        "a present web-ui directory is not an opt-in"
    );
    let mismatched = resolve_repo_root(
        &compiled,
        Some(other.to_str().unwrap()),
        ui_source_bind(&compiled).as_deref(),
    );
    assert!(
        mismatched.is_err(),
        "a binding for the compiled tree must not select the other tree"
    );

    let fixture_bind = ui_source_bind(&fixture).unwrap();
    let admitted = resolve_repo_root(
        &compiled,
        Some(fixture.to_str().unwrap()),
        Some(&fixture_bind),
    )
    .unwrap();
    assert_eq!(admitted, fixture);
    assert_eq!(executed_ui_behavior(&admitted), "sealed-image:sealed-image");
}
