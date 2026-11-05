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

Named cases: `R11_SNAPSHOT_FIXED_R`, `R11_PIN_HOLDS_AFTER_ADVANCE`, `R11_MISMATCHED_TARGET`, `R11_INVALID_REV`, plus supporting `R11_SNAPSHOT_LFS` and `R11_SNAPSHOT_RECONCILE`. `R12_FULL_DEFAULT` stays next to the existing full-import cases `64A_ORDINARY` / `74_APPLY_POSITIVE` / `74_LFS_POSITIVE`. UI: `candidate_r68_import_card_full_default_ui`, `candidate_r68_import_card_snapshot_boundary_ui`, and `candidate_r68_import_card_invalid_revision_refusal_ui` (mounted import card, headless Chrome). API: `candidate_r68_refused_snapshot_status_omits_verified_boundary`. Unit: `web-ui/src/importBaseline.test.ts`. The journal unit `snapshot_reconcile_completes_exact_baseline_and_refuses_dishonest_evidence` covers exact-SHA completion, confirmed-receipt recovery, and refusals (wrong SHA, missing pin, drifted pin, widened total, non-import type) without a second remote write.


## Whole-story continuation contract — 2026-11-05

The earlier slice and its PARTIAL evidence are retained above. This section
specifies the additional closeout behavior and required qualification; it does
not declare #68, R11, R12 or the reliability epic complete.

A newly finalized snapshot writes one repository-scoped applied SVN-origin
baseline record, keyed by its import operation, in the same transaction as
operation completion and both checkpoint copies. This happens only after the
existing exact-ref publication confirmation and single-pinned-baseline checks.
A failed finalizer rolls back the applied record as well as the checkpoints.
Reopening a held import and verifying its actual target through the authenticated
reconcile API can finish that same transaction; repeating its receipt does not
publish again or create another mapping. Ordinary sync then has genuine producer
evidence for its initial handled frontier. The strict history reader, full-import
writer, schema and native fencing contract are unchanged.

When that same SHA/revision also appears in the completed snapshot operation,
late-pair inspection retains `snapshot_import_confirmed` rather than hiding the
history-boundary classification behind its new applied record. This enriches an
already verified identical mapping; it does not admit another SHA/revision or
execute late pairing.

The outbound byte bridge must not write an LFS pointer as SVN file contents.
For pointer-shaped changed blobs it inspects `filter` at the exact source commit
with [Git's source-tree attribute query](https://git-scm.com/docs/git-check-attr).
LFS/other filtered pointer content, local attribute overrides, unavailable query
capability or incomplete output causes a durable pre-publication refusal.
Unfiltered literal pointer-shaped text remains a supported ordinary file. Later
attribute changes cannot retroactively reinterpret an earlier commit in a replay
batch. Modified LFS payload hydration is not implemented by this change; #44 and
#52 remain separate. Snapshot LFS publication still requires real tooling and
object verification, and ordinary supported changes on that baseline must not
replace unchanged SVN binary contents with pointer bytes.

Old completed import documents are not silently rewritten or used to repair
missing/split legacy provenance. Keep such pairs held for their existing #63
recovery/admission process; do not manufacture applied rows, reset watermarks or
reimport over an existing target. For an active held snapshot, preserve its
operation ID and use the authenticated remote-verification action. An absent or
mismatched ref remains held rather than granting permission to republish.

### Required exact-source closeout evidence

- Existing full/default and selected-revision API cases, pin-after-advance,
  nonempty-target and invalid-revision refusal, truthful boundary metadata and
  snapshot-derived late-pair preview must remain passing.
- `rs68_snapshot_continues_bidirectionally_and_restarts_without_replay` verifies
  real later SVN and Git work, scoped mappings and reconstructed-engine repeats.
- `rs68_snapshot_checkpoint_failure_reopens_and_reconciles_without_republication`
  verifies atomic rollback, real target observation and continued ordinary sync.
- `rs68_cancel_before_snapshot_effects_preserves_uninitialized_target_across_reopen`
  verifies cancellation before effects without inventing a baseline.
- `rs68_missing_lfs_object_after_snapshot_never_writes_pointer_text_to_svn` is a
  modeled skip-smudge negative, including a queued filter change and durable
  refusal after reopening. It is not a real LFS lifecycle pass.
- `rs68_literal_pointer_text_without_filter_remains_a_regular_file` preserves the
  ordinary-file positive control.
- `rs68_snapshot_real_lfs_publication_fetch_and_missing_object_verification`
  requires actual installed git-lfs, a 1.1 MB SVN binary, a verified snapshot
  pointer/object, a fresh real fetch, ordinary two-way continuation and a failing
  object check after deliberate loss of that fixture's fetched object. It has no
  skip or pointer-substitute fallback. The standard hosted workflow already
  installs git-lfs; local absence is not a pass or permission to install it.
- The feature-qualified
  `rs68_snapshot_lost_push_reply_reopens_and_reconciles_through_http_once` checks
  a real accepted push with lost reply, reconstructed server/database, absent-ref
  refusal, authenticated reconciliation and repeated receipt. Its owned Git
  command trace must show fresh observation and no publication command.
- Reuse the #88 independent raw-export/type/projection cases and existing real
  snapshot LFS and full-default controls. Do not replace expected bytes with a
  second run of the copier.
- The three mounted R68 import-card cases and importBaseline unit tests remain
  required. The standalone card uses the real router context required by #120.
  Its browser runtime uses the existing installed sandbox-enabled Playwright
  route and awaits context closure; callers collect their owned Vite handle
  before asserting results. A timeout is a failed gate, not cleanup proof.

Run environment-qualified fixture cases separately with `--exact` and
`--test-threads=1`, retaining original logs, selected inventory, source identity
and all prior failures. The new lost-reply case is feature-gated and must be
explicitly selected; the normal workspace run alone does not execute it. Local
UI, browser, hosted Rust and real LFS evidence are separate classes. Retained
independent review, exact-head CI, publisher acceptance mapping and actual
postmerge verification are required before any whole-story closeout. R11/R12
remain PARTIAL and #68/#61 remain open until their required proofs are accepted.
No native/process hold, unknown descendant, deployment, live credential, main
promotion, changed-child execution or #121 reader/usability gate is released.
