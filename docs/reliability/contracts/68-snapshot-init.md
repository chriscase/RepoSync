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


## Synthetic installer/update qualification

GOAL section 6 permits a pinned old executable/schema with synthetic history;
the unknown work-server version does not prevent this tier. Production-version,
privileged installer, service lifecycle and broader #118 platform/release proof
remain separate. Do not describe a successful source build or library-only open
as a successful installation update.

The explicit offline entry is:

```sh
bash scripts/install.sh --offline-update \
  --root /absolute/canonical/private/existing-prefix \
  --archive /path/to/pinned-two-binary-package.tar.gz \
  --sha256 EXPECTED_PACKAGE_SHA256
```

This Unix-only mode updates an already-configured, user-owned prefix containing
`bin/reposync`, `bin/reposync-daemon`, `config/config.toml` and
`data/reposync.db`. It requires a private root and refuses linked targets. The
package must contain exactly the two regular executable members used by the
historical release layout. Configuration, authors, database/WAL, key material
and repository contents are separately owned existing state, not invented
release members. Missing historical templates are not fixed by this mode.

Package digest and complete member inventory are checked before target changes.
Only the two binaries are replaced; their prior modes are preserved. A normal
replacement error attempts to restore already-replaced binaries. If restoration
is uncertain, the original/secondary errors and recovery staging path remain
visible and the staging files are retained. The mode is not a crash-atomic pair
transaction, a signature or ABI check, a migration, a process stop/start command,
or permission to operate a live installation. Use a stopped owned installation
and keep the pinned package/provenance. No sudo, network, account, ownership or
service action occurs. The existing default installer flow remains byte-exact.

The opt-in pinned-old fixture reuses the canonical `8737974` source and retained
reviewed lock to construct synthetic old history/schema. It installs actual old
binaries into a separately configured temporary prefix, runs their CLI status,
updates through the real offline entry, and checks unchanged state bytes/modes.
It then executes the installed candidate's status and daemon version only, plus
an actual omitted-mode Personal full import/repeat through the existing local
Git HTTP fixture. Finally, every retained in-process engine preservation,
bidirectional, mapping and reconstructed-no-op assertion must pass before the
additional installer receipt is emitted. A supplied invalid specification fails;
it cannot silently fall back to the library-only test path.

These observations remain distinct: installed CLI launch/inspection is not
engine synchronization; in-process engine reconstruction is not a physical
service restart; an actual two-revision default import is not a snapshot. Local
results require source/artifact binding, independent review, fresh hosted
qualification and post-integration repetition before the synthetic R12 gate is
closed. The actual work-server version and active-environment acceptance stay
unestablished. No older CI result or control clock transfers to a successor.


## 2026-11-05 scoped R11/R12 qualification reconciliation

This section supersedes the historical missing-continuation, Personal-catalog and
unqualified synthetic installer/update statements above for the tested source
`a887e048662c9b4804649be10feff13b0c7b22c7` / tree `960d1536df6bcd0bad7ba5e893bb6c65eecc7a15`.
It does not erase prior failures or qualify a later source version by inference.

[Exact Linux CI42023789883](https://github.com/chriscase/RepoSync/actions/runs/42023789883) and job127232824038 succeeded at
2026-11-05T12:26:54Z. All23 steps passed. Original decoded API job log:815900
bytes, SHA256 `7f8f6bad518c13ceb4043e9c80000e1eb135fba321aef7fc025366805b3f4789`; its separate operator original is1082655 bytes,
SHA256 `97b4d3a7e4bd1179a56299bae94d60725a53fe2efdf8cd17233646e85eae8802`.
These are separate decoded originals, not identically framed byte streams.

### Whole #68 acceptance mapping

| Criterion | Actual implementation and qualified evidence |
| --- | --- |
| Original1: explicit modes and defaults | Existing Add Repository/import card exposes full/default and pinned HEAD/revision choice; retained local mounted full-default/snapshot-boundary/invalid-revision cases and225UI checks have identical frontend/harness inputs. Current API R12_FULL_DEFAULT and installed-candidate omitted-mode two-revision full import/repeat pass. |
| Original2: one fixed identity/revision | Current fixed-R, pin-after-advance and invalid-revision tests pass; verified UUID/canonical path/operative+peg pin remains shared with snapshot materialization and publication. Unsupported lineage is refused. |
| Original3: genuine tree/publication/baseline | Actual snapshot materialization/projection and remote commit are verified; atomic import finalization records truthful applied baseline provenance. Normal and checkpoint-rollback/reconcile tests pass; empty cursor-only admission is not used. |
| Original4: boundary and exact continuation | Existing starting revision/history boundary/earlier-history omission remain visible. rs68_snapshot_continues_bidirectionally_and_restarts_without_replay passes with SVN then Git advancement and reconstructed no-op. |
| Original5: target safety | Mismatched/nonempty target refusal and exact baseline reconciliation controls pass; no reset, force publication or clone-failure fallback was added. |
| Original6: initialization/cancel/restart | Current import journal controls, cancel-before-effects, finalizer rollback/reopen and actual accepted-lost-reply HTTP reconstruction/reconcile/repeat pass. No physical crashed-descendant collection claim. |
| Original7: file policy/content/LFS | Existing shared no-follow/projection/file-type tests pass with the workspace. Real Git LFS3.8.1 publication, raw/pointer/object bytes, fresh fetch, later changes, reconstructed no-op and missing-object detection pass. Filter-managed unresolved pointer publication refuses before SVN mutation; ordinary literal pointer files remain supported. Unsupported properties/hydration work from #52/#44 is not claimed. |
| Original8: optional baseline reuse | Not implemented; explicitly optional and not needed for delivery. No PASS is claimed for this feature. |
| RS29-1: exact source/CI and R11/R12 | This exact head passed source review and Linux CI. Matrix R11/R12 now cite the scoped mandatory proof; issue68/61 remain open pending final-head/postmerge and all other epic requirements respectively. |
| RS29-2: accepted/lost-push reply | rs68_snapshot_lost_push_reply_reopens_and_reconciles_through_http_once passes through the actual authenticated reconcile route, reconstructed DB/server, one applied baseline and repeat receipt. |
| RS29-3: real LFS lifecycle | Default-selected rs68_snapshot_real_lfs_publication_fetch_and_missing_object_verification passes. This is genuine git-lfs execution, not pointer-text equivalence alone. |
| RS29-4: independent file projection | Retained #88 shared manifest/file-type/confinement controls pass; no expected tree is copied from the candidate result and no new unsupported feature is activated. |
| RS29-5: diagnostics remain honest | Initial writer-fence failures, parser rejections, native restricted loopback failure, browser timeout/inspection denial and successful explicitly authorized retry remain historical evidence. No failure is relabeled skipped or silently green. |
| RS29-6: onboarding/default compatibility | The existing onboarding path already supplies explicit choices; no second wizard implementation is necessary. Defaults are checked at API, real installed CLI and retained mounted UI levels. |
| RS29-7: boundary display | Existing API DTO/detail/import card show starting revision/history boundary and earlier-history omission; source-equivalent unit and mounted cases remain separately scoped local evidence. |
| RS29-8: cancel/reconcile/refusal | Current #64 cancellation/refusal controls, new cancel-before-snapshot-effects and real HTTP/reopen/reconcile tests pass; no reset shortcuts or fence weakening. |
| RS29-9: full/default and snapshot tests | Normal workspace plus all three separately selected feature cases pass. The records-only successor still requires its own independent review and exact CI; then the actual merge requires a fresh postmerge run. |

### Authentic synthetic installed-update tier

The added step ran all12 installer mechanism cases and exactly one nonignored
`candidate_r01_old_import_cursor_survives_svn_only_poll_and_upgrade`:1 passed,
0 failed/ignored. The193-byte stdout matches its independently retained runtime
SHA256. Both R01/R12 receipts match independent source/compiler/package records.
Old source is87379741779a6259f7eeb52a68cc6f061174e5ef/tree
f383bb9b123f3f75b2dde7f17f7311a264946abd; reviewed lock is
`9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`. All200 old production bodies were checked before and
after build; only the reviewed test generator and lock were added to preparation.

Actual candidate Linux package:26017422 bytes,
SHA256 `46f4d78939d5115fd894628412c60d574c434e874b1c95be84f5485cde889e48`.
CLI SHA256 `2d327785dd95fc09e9282301d4adee74fafbcfce12bd8bb61a1f2a2ff9b5d4d2`;
daemon SHA256 `877d52a0c90c00d655f4013d938c7a89058b83784692f7ccb1442bc2a39fde6c`.
Actual old package SHA256 `60a43a8ec2e1962609b41e0b09949b4776bf07251e606294e51a01cdbe9e485f`.
Both use the genuine two-binary layout. These are pinned-source dev/no-debuginfo
synthetic artifacts, not distributed releases or proof of a historical deployment.

Observed: old installed CLI status; actual real-entry owned-root binary update;
config/data/refs/WAL/key byte-and-mode preservation; installed new CLI status and
daemon VERSION ONLY; actual installed omitted-mode Personal import with two
revisions/mappings and no-op repeat; then the original same-state in-process
engine no-change, bidirectional, restore and reconstructed no-op assertions.
All17 foreground command records exited0; schema12-to12; activation false.
Installed executable observations and in-process engine observations stay distinct.

`required-cases.json` records these ordinary-CI profiles separately from the old
sealed diagnostic/candidate/browser case declarations. Registering the evidence
metadata neither activates that sealed runtime nor grants a native/process profile.
Current workflow assertions enforce these ordinary-CI selections and receipts.

### Preserved exclusions and final gates

- Actual actively deployed version/schema provenance remains NOT ESTABLISHED;
  GOAL126 explicitly allows the initial pinned synthetic tier without it.
- Privileged/default installation, fresh templates, physical daemon/service
  restart, #118 platform/release packaging and work-server qualification remain
  separate; no production/deployment effect occurred.
- #67 late-pair replay remains open; only baseline admission/evidence is credited.
  Optional baseline reuse and richer wizard polish are not declared delivered.
- Broader real-engine scenarios remain17PASS/5PARTIAL/overallPARTIAL with CI-GATE
  PASS; GHE is local DRY RUN, not enterprise qualification. #62/#61 are not closed.
- #121 actual-reader/usability deferrals and frozen69/native/process holds remain.
- The a887 head window is CLOSED_TERMINAL_SUCCESS. No obsolete clock acts.
  This records-only successor needs retained review and its own exact-head run,
  then qualified epic-only integration, equal-tree check and its OWN postmerge
  qualification/clock before honest #68 closeout. No main promotion or deployment.
