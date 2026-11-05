# #117: inert deployment planning and fail-closed artifact promotion

`scripts/deploy.sh` retains its Bash entry point and delegates to Python 3 standard-library helpers. No installation, deployment, service restart, credential change or host reconfiguration is authorized by this change or its tests. Production and frozen recovery/collection gates remain separate. This is the existing Linux development-server deployment path, not the #118 release installer or Docker packaging.

## Commands and compatibility

```sh
# Read-only: resolves source identities and prints a plan; requires a clean checkout.
scripts/deploy.sh --dry-run
scripts/deploy.sh --dry-run --no-web-ui

# Inert synthetic regression tests, never a real deployment:
python3 scripts/test_deploy.py
cargo test -p reposync-web --test deploy_safety -- --nocapture
```

Actual deployment still requires separate operator authorization, quiesced writers, supported backup/reconciliation procedures, existing SSH/service permissions and a suitable host. None was executed to qualify this story. Do not use the former dry-run as a connectivity test: it now performs no transport, build, temporary-worktree or service action. Local Git identity/status reads disable optional Git locks. Dirty or mismatched source is refused before any effect.

The default is the checked-out `HEAD`, not an implicit deployment of `main` alongside another branch's UI. `--branch` and `DEPLOY_BRANCH` remain accepted, but must resolve to that same exact commit. The full immutable object is sent to `refs/reposync/deploy/<SHA>` without force or a merge into the server's checkout. `DEPLOY_SSH_HOST`, `DEPLOY_SSH_OPTS` and `DEPLOY_REMOTE_REPO` select the existing authorized connection/location; the default repository is `~/GitSvnSync`. The helpers do not change SSH policy, credentials, configuration, service units or access grants.

For an approved future deployment, supply a reviewed `Cargo.lock` with `--lockfile /path/to/Cargo.lock` (or the checkout's ignored `Cargo.lock`). Absence is a pre-transport refusal. The exact lock bytes are transferred and hashed before `cargo build --locked`. This does not resolve, fetch or qualify a dependency graph during dry-run/testing. #118's installable packages, supported platform qualification and the #62/#63 upgrade/backup obligations remain separate.

## Identity and failure ordering

1. Resolve the requested commit and its `web-ui` source-tree identity once. Reject a dirty checkout or a branch that differs from checked-out HEAD.
2. On the existing Linux host, verify the requested object/tree and a clean repository. Require the existing `reposync` service's `ExecStart` to name `<remote-repo>/target/release/reposync-daemon`. Unsupported wrappers or a different installation path refuse; no unit is rewritten.
3. Create a private staging directory beside the checkout and a detached source worktree. Build into a fresh target directory using the checksum-verified lock. Compiler exit is checked directly, with no tail pipeline. Failures preserve staging and never promote/restart.
4. Build UI from another detached worktree of the same commit using `npm ci` and the committed npm lock. Transfer a regular-file archive with a SHA-256 expectation. The receiver checks the complete archive before writing static resources; links, duplicates, absolute/traversing names and unsupported nodes refuse. Limits are 10,000 regular files / 256 MiB member bytes. `index.html` and JavaScript assets are required.
5. `--no-web-ui` means reuse only a previously sealed release whose UI source-tree identity and every retained static-file checksum match. The copied bytes are checked again. Unverified legacy assets or changed UI sources refuse with a rebuild instruction; skipping UI is never permission to combine arbitrary old assets with a new backend.
6. Seal a manifest containing the full source SHA, compile-time abbreviated Git identity, lock hash, binary hash, UI source-tree identity and every static-file hash. Recheck this manifest and actual bytes immediately before promotion.
7. Hold a nonblocking promotion lock shared by this helper's deployers; reject a changed live-binary preimage. Record promotion intent and preserve the old daemon. Atomically replace only the managed binary pointer. Its target is a release directory containing its matching `static/`; the existing application serves static files next to its real executable. Prior binaries, static assets and releases are retained, not cleared before transfer.
8. Restart via the existing service permission, then require bounded healthy HTTP observations, exact clean build identity, a positive service PID, the running `/proc/<PID>/exe` hash and the promoted pointer's hash. Unknown, mismatched or `+dirty` identities are failures. A success receipt is written only after these checks.

The promotion lock does not stop engine work or prove global quiescence; obtaining that prerequisite is the operator's separate responsibility. Preparation and artifact failures cannot replace the live pointer. A failure after pointer promotion or service restart returns nonzero, leaves its intent/staging/previous assets intact and produces no verified receipt. **Do not blindly retry, restore an old DB, reset watermarks or automatically roll back after possible external effects.** Preserve the printed staging path, inspect the actual process/artifact and reconcile or roll forward under the existing recovery procedure. This story does not supply a transactional rollback across a running synchronization engine, DB and remote repositories.

## Acceptance and evidence limits

| Canonical criterion | Executed synthetic evidence |
| --- | --- |
| Dry-run invokes zero transport, service, remote mutation or build commands, with/without UI | Actual Bash/Python entry point runs inside a clean synthetic Git checkout; all forbidden executables are tripwire stubs. Both invocations must exit 0 without a single tripwire or worktree/build artifact. Dirty/mismatched source refuses before transport. |
| Immutable requested commit and matching UI; reject dirty/mismatched builds and exact artifact mismatch | Pinned-object client tests; fresh-worktree UI build contract; checksummed lock/archive; sealed binary/static hashes; verified UI reuse; dirty/unknown/wrong health identities and running-binary mismatch negatives. |
| Compile, static-transfer, health and identity failures stop before later mutation and preserve prior assets until promotion | Injected nonzero compile/transport exits; actual owned staging/extraction/symlink/filesystem assertions; no later seal/promotion/restart on pre-promotion failures; no false verified receipt, deletion or rollback on post-promotion failure. |

These are inert regression tests with real disposable Git/filesystem data and modeled transport/build/service/proc observations. They are **not** a Linux service restart, real compiler/npm build, physical process-collection proof, production/network test, release qualification, deployment authorization or whole-epic PASS. The same 24 Python cases are invoked by the Rust integration selector in ordinary workspace CI; there is no workflow modification or zero-selected fallback. Final-source command receipts, independent review and exact-head/postmerge CI remain separately bound gates.
