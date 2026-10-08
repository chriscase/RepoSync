# #68 slice: verified SVN snapshot / selected-revision init

This is the smallest product slice of issue #68. It does **not** close #68 or #61.

## What shipped

Team onboarding can start a per-repository import in one of two explicit modes:

| Mode | How to request | Default |
| --- | --- | --- |
| Full history | omit `import_mode`, or `import_mode=full` | Yes — unchanged on upgrade |
| Snapshot | `import_mode=snapshot` plus optional `svn_revision=HEAD\|<n>` | No |

The request may use the query string or a JSON body. `svn_revision` is rejected on full-history imports.

Snapshot initialization:

1. Reuses the personal snapshot engine in `crates/core/src/snapshot.rs` (`resolve_snapshot_pin`, `materialize_snapshot`, projected-content verify). Personal `ImportMode::Snapshot` / `import_snapshot` is a wrapper over the same functions and `copy_tree_with_policy`. There is not a second snapshot implementation.
2. Resolves `HEAD` once to a numeric revision, or accepts an explicit positive revision. Pins SVN UUID, canonical URL, operative/peg revision, and copy ancestry when the log already exposes it.
3. All export / verify / baseline / publish steps use that pin even if SVN advances.
4. Materializes the selected tree under the active file policy, creates one baseline Git commit, verifies **projected file bytes** (not only names/counts), publishes through the existing #64 import journal, then records the verified mapping/checkpoints.
5. When `lfs_threshold_mb > 0`, snapshot import runs the same Git LFS preflight and `git lfs install --local` as full import before that commit. Missing `git-lfs`, or a failed local install, fails the snapshot before any baseline commit. It does not store a fat blob or invent pointer text. LFS-tracked paths in the commit must be pointers whose oid and size match the projected bytes. Other paths still match raw bytes. Full-history import keeps its existing warn-and-continue behavior when LFS tooling is missing.
6. The first snapshot publish uses `git push --force-with-lease=refs/heads/{branch}:` (empty expected ref). That creates the branch only when the remote ref is absent. The empty-target gate already refuses an existing branch before the worker starts. This is not a force-push overwrite.
7. A snapshot stuck in `ReconciliationRequired` finishes through the existing reconcile API. `snapshot_import` is accepted only when the stored pin's operative and peg revisions match the recorded local revision, the operation is one baseline (`total`, `processed`, and `local_commits` are 1), and the observed remote SHA is that baseline. A missing pin, a revision mismatch, a wider total, a SHA mismatch, a fingerprint change, or a non-zero checkpoint stays held. Reconcile does not delete remotes or overwrite with a different SHA.
8. Status and `GET /api/repos/{id}` show `import_mode`, `starting_revision`, `history_boundary`, and `snapshot_pin`. Snapshot status sets `earlier_history_imported=false` and never claims earlier SVN history was imported. The worker broadcasts completion only after finalization succeeds.
9. The repository stays `initializing` until publication and mapping are verified (`last_svn_rev` / `last_sync_at` stay unset). Active/held #64 import journals still block ordinary sync.
10. Existing non-empty mismatched Git targets are refused. There is no automatic reset, force-push overwrite, or clone-failed→init.

Full-history import remains the default and still uses `run_full_import`.

R11 and R12 stay **PARTIAL**. This slice does not close #68.

## Mode / API compatibility

| Consumer | Full history (default) | Snapshot |
| --- | --- | --- |
| `POST /api/repos/{id}/import` with no body | `import_mode=full` | — |
| JSON `{"import_mode":"snapshot","svn_revision":"HEAD"}` or `"<n>"` | rejected if `svn_revision` set on full | pins once, returns `starting_revision` + `history_boundary` |
| Query `?import_mode=snapshot&svn_revision=…` | same as omitting body for full | same as JSON |
| Upgraded installs / omitted fields | unchanged full replay | opt-in only |
| Web UI: Add Repository → Initial SVN import | pre-selects full; prefs flow to repo detail | optional HEAD or numeric revision |
| Web UI: Import progress card | `Start full history import` sends `{}` | `Start snapshot import` sends snapshot body |
| `GET /api/repos/{id}/import/status` and repo detail | `earlier_history_imported=true` when applicable | `earlier_history_imported=false`, boundary text |

**User-facing limitation:** Snapshot mode builds Git from one verified SVN revision and **does not import earlier SVN history**. The mirror is still SVN-origin. A refused or failed snapshot (no pin recorded) must not be described as a verified baseline; only server `history_boundary` / `starting_revision` copy is shown after verification.

## Still later

- Rich onboarding UI / wizard polish
- Late-pair semantics (#67)
- Optional reuse of a previously imported baseline
- Remote deletion, #52 monorepo absorption, schema beyond v12 `kv_state`
- Live acceptance, force-push, merge, closing #68/#61

## Tests

Named cases: `R11_SNAPSHOT_FIXED_R`, `R11_PIN_HOLDS_AFTER_ADVANCE`, `R11_MISMATCHED_TARGET`, `R11_INVALID_REV`, plus supporting `R11_SNAPSHOT_LFS` and `R11_SNAPSHOT_RECONCILE`. `R12_FULL_DEFAULT` stays next to the existing full-import cases `64A_ORDINARY` / `74_APPLY_POSITIVE` / `74_LFS_POSITIVE`. UI: `candidate_r68_import_card_full_default_ui` and `candidate_r68_import_card_snapshot_boundary_ui` (mounted import card, headless Chrome). Unit: `web-ui/src/importBaseline.test.ts`. The journal unit `snapshot_reconcile_completes_exact_baseline_and_refuses_dishonest_evidence` covers exact-SHA completion, confirmed-receipt recovery, and refusals (wrong SHA, missing pin, drifted pin, widened total, non-import type) without a second remote write.
