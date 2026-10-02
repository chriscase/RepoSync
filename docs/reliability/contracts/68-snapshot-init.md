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
5. Status and `GET /api/repos/{id}` show `import_mode`, `starting_revision`, `history_boundary`, and `snapshot_pin`. Snapshot status sets `earlier_history_imported=false` and never claims earlier SVN history was imported.
6. The repository stays `initializing` until publication and mapping are verified (`last_svn_rev` / `last_sync_at` stay unset). Active/held #64 import journals still block ordinary sync.
7. Existing non-empty mismatched Git targets are refused. There is no automatic reset, force-push overwrite, or clone-failed→init.

Full-history import remains the default and still uses `run_full_import`.

## Still later

- Rich onboarding UI / wizard polish
- Late-pair semantics (#67)
- Optional reuse of a previously imported baseline
- Remote deletion, #52 monorepo absorption, schema beyond v12 `kv_state`
- Live acceptance, force-push, merge, closing #68/#61

## Tests

Named cases: `R11_SNAPSHOT_FIXED_R`, `R11_PIN_HOLDS_AFTER_ADVANCE`, `R11_MISMATCHED_TARGET`, `R11_INVALID_REV`, plus `R12_FULL_DEFAULT` next to the existing full-import cases `64A_ORDINARY` / `74_APPLY_POSITIVE` / `74_LFS_POSITIVE`.
