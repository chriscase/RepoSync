# RepoSync reliability: Codex implementation and review handoff

**Owner:** Chris  
**Prepared:** September 23, 2026  
**Repository:** `chriscase/RepoSync`  
**Epic:** <https://github.com/chriscase/RepoSync/issues/61>  
**Reviewed main:** `87379741779a6259f7eeb52a68cc6f061174e5ef`  
**Initial assignment:** Phase 0 — sandbox, reproductions, and the state/migration design.  
**Authorization:** Work on a feature branch and publish a draft PR. No merge, release, deployment, live acceptance, force push, or production data access is authorized.

## 1. Mission and existing work

Make RepoSync trustworthy for SVN-origin teams who develop primarily in Git, including late pairing, cancellable imports, safe entry removal, snapshot initialization, rewrite containment, and coordinated pair refresh. Preserve existing installations and prove compatibility before the active environment is used for acceptance.

The issues have **already been created**. Do not create duplicates or replace the existing backlog. Read their current bodies and relevant discussions before starting:

| Issue | Scope | Initial dependency/order |
|---|---|---|
| [#61](https://github.com/chriscase/RepoSync/issues/61) | Coordination epic and invariants | Governs all work |
| [#62](https://github.com/chriscase/RepoSync/issues/62) | Sandbox, reproductions, automated upgrade/recovery matrix | First |
| [#63](https://github.com/chriscase/RepoSync/issues/63) | Canonical SVN lineage/checkpoints and legacy migration | Design in Phase 0; implementation after review |
| [#64](https://github.com/chriscase/RepoSync/issues/64) | Durable jobs, cancellation, external-write recovery | Design in Phase 0; builds on state contract |
| [#66](https://github.com/chriscase/RepoSync/issues/66) | Rewrite/unproven-history detection before reset/replay | Early protection after reviewed reproduction/design |
| [#65](https://github.com/chriscase/RepoSync/issues/65) | Disable versus remove versus explicit remote branch deletion | After operation/state foundations |
| [#68](https://github.com/chriscase/RepoSync/issues/68) | Verified SVN snapshot/selected-revision initialization | After foundations; reuse personal importer |
| [#67](https://github.com/chriscase/RepoSync/issues/67) | Late pairing without dropping earlier Git/SVN work | After foundations and rewrite gate |
| [#69](https://github.com/chriscase/RepoSync/issues/69) | Coordinated pair refresh | After safe pairing/recovery |
| [#70](https://github.com/chriscase/RepoSync/issues/70) | Deletion-page navigation | Small independent fix after reproduction and review |
| [#71](https://github.com/chriscase/RepoSync/issues/71) | Git-centric requests/hooks/documentation | Safe APIs first; protective docs can start early |
| [#41](https://github.com/chriscase/RepoSync/issues/41) | Existing enterprise qualification, updated for this epic | Release gate, not current permission to run live tests |

Reuse the existing validation work in #32/#33/#40/#46, `Makefile`, `tests/docker-compose.yml`, actual engine integration tests, and existing validation scripts. Inspect PR #51 before touching its provenance/LFS work: an open PR is not necessarily absent from current code, and its historical result is not evidence for the new head. Coordinate with #44 and monorepo epic #52, especially migration #54 and tests #60. Do not absorb those entire feature scopes or renumber competing migrations independently.

## 2. Non-negotiable product and safety rules

**SVN origin is required.** A Git feature reference descended from a verified SVN import is allowed; unrelated Git-first histories are not. Prove identity and ancestry using stored mappings and actual repositories. Commit message markers and matching trees alone are not authoritative proof.

**A checkpoint is evidence, not convenience.** It represents verified work or an explicit recorded filtering decision. Never set the current Git tip/SVN HEAD just to stop replay. Keep processed SVN and processed Git positions separate; a revision emitted by Git→SVN must not cause pending incoming SVN changes to be skipped.

**Unknown history stops the affected pair.** Before destructive checkout/reset or writes, distinguish unchanged tip, proven fast-forward, rewrite, missing object/history, missing remote and changed SVN identity. Do not silently clone/init/reset/reimport as recovery.

**Cancellation is not undo.** Operations must record requests, ownership, external effects, and partial completion durably. A remote operation may succeed even if its response/checkpoint is lost. Verify and reconcile before retrying. No claim of one atomic transaction across SVN, Git and SQLite.

**Existing installations are preserved.** IDs, settings/defaults, credentials and hierarchy/rotation, mappings, checkpoints, files and disabled states must survive. Keep legacy root DELETE non-destructive. Do not silently purge old disabled repos or alter full-import defaults. Ambiguous unsafe state may require an explicit per-pair reconciliation state, not a guessed migration or blanket disable of healthy installations.

**No live side effects.** Production access, credential collection, live rules/webhooks/runner changes, or deployment needs separate approval. Local test scripts must not inherit ambient credentials or accidentally use production URLs. The feature branch contains RepoSync source changes; it is not permission to modify the user's actively synchronized repositories.

## 3. First execution: Phase 0 only

### Establish identity and a clean development boundary

Read repository instructions such as `AGENTS.md` if present, contribution guidance and relevant issues. Inspect the working tree before editing. Do not reset/stash/discard someone else's work. Use a separate worktree when necessary.

Fetch repository source metadata and record current main. If main has advanced from the reviewed SHA, report the change and inspect the intervening diff; do not force main backwards. Start `feature/reposync-reliability` from the current reviewed-compatible base. If that branch already exists, inspect and continue it only when it belongs to this work; otherwise use a clearly named non-colliding branch.

Copy this handoff to `docs/reliability/GOAL.md` as the initial agreed contract, and the companion coworker FAQ to `docs/reliability/coworker-FAQ.md`. Record the goal file's SHA-256 in the PR/handoff. Do not silently rewrite acceptance criteria to make the implementation pass. Put proposed goal changes in an explicit amendment for review.

### Audit and run the baseline in isolation

Inspect the actual commands before executing them. `Makefile` currently exposes `test`, `test-e2e`, `test-all`, `validate-quick` and live-validation commands; do not assume every target is sandbox-safe. The current E2E workflow already guards against zero tests and installs LFS tooling. Preserve that behavior.

Run applicable baseline Rust/UI checks and safe local integration tests; record exact commands, versions, exit codes, counts and omissions. Do not fix unrelated failures silently. No `validate-ghe-live` or live-soak execution is authorized in Phase 0.

Implement/extend the one-command isolated harness. Prioritize reproducing root disable/delete, the per-repo cancellation gap, late pairing with pre-existing commits, and rewrite duplication. Cover the deletion-page report when the browser harness is available. Report every coworker scenario as reproduced, contradicted by evidence, or not yet reproduced with the specific gap. Do not invent the coworker's exact commit graph.

### Produce the design, not an unreviewed rewrite

In `docs/reliability/design.md`, propose:
- Canonical lineage identity, directional checkpoints, mapping outcomes, legacy-state reconciliation, and migration sequencing compatible with #54.
- Operation states/ownership, cancellation boundaries, subprocess supervision and recovery at each Git/SVN/DB boundary.
- The pre-replay ancestry gate, preservation of old refs/evidence, and incomplete-history handling.
- Snapshot initialization and late-pair reconciliation admission; default history-preserving refresh versus separately gated re-anchor.
- Supported deployment/legacy-version matrix and remaining unknowns.

Use the minimum architecture that can enforce these properties. An in-process service plus SQLite journal and explicit single-daemon ownership can be preferable to a new distributed service. Do not introduce a generic VCS framework, queue platform or major UI rewrite just to complete this epic.

**Stop for the first review after publishing the Phase 0 branch and draft PR.** The stop point is intentional: tests/fixtures/instrumentation/design may be implemented, but no runtime-semantic fix, migration execution change, or destructive lifecycle change should bypass this gate. Include the proposed smallest next implementation slice. A minimal rewrite-containment fix can be prioritized next once its evidence/design is reviewed.

## 4. Sandbox contract

Use disposable real SVN repositories and Git remotes with the real engine. Add an isolated local HTTP provider for API-dependent paths; a mock alone cannot qualify Git/SVN data propagation. Bind exposed ports to loopback. Give the application temporary private data/config directories and test-only credentials.

Install/build dependencies in a separate preparation stage. During tests, enforce a network boundary that cannot reach production, in addition to validating target URLs. Isolate HOME, Git config/credential helpers/hooks, SVN auth caches, SSH agents, notification sinks and inherited secrets. Do not mount the host Docker socket into the application under test or expose production volumes. Test the guard by attempting a non-sandbox target and proving it is rejected without making contact.

Default fixtures are synthetic and created by pinned legacy code. A real installation snapshot is optional and requires permission. Before any copied state is started, deny egress, disable scheduling, remove secrets and notification endpoints, and remap **all** URLs, including global/per-repo settings, inherited credentials, `.git/config`, submodules/externals and webhook settings. Do not let migration startup accidentally become live synchronization. Never commit actual production DBs, credentials or customer source.

Use deterministic barriers to inject faults before/after remote writes and DB persistence. Poll for explicit conditions with bounded timeouts, not unbounded sleeps. Capture enough data to distinguish process cancellation from a completed remote operation whose acknowledgement was lost.

## 5. Stable acceptance matrix

Start with the IDs below; extend rather than renumber them. Each row can expand into multiple individually named tests. Record separate baseline and candidate outcomes.

| ID | Required evidence |
|---|---|
| R01 | Ordinary SVN→Git, then Git→SVN; actual intermediate/final remote trees and mapping; second unchanged run does no work. |
| R02 | Legacy root DELETE stays non-destructive; explicit managed removal removes only owned local data and preserves remote history. |
| R03 | Cancellation at connect/export/replay/batch publish/verification/final publish; persistent request, bounded process stop, truthful partial result. |
| R04 | Cancel/restart/delete/scheduler races; no old job mutates a new generation or resumes partial import as complete. |
| R05 | SVN commit or Git push succeeds, response/DB write fails; recovery identifies the actual effect and does not duplicate it. |
| R06 | Existing Git branch with at least two unsynced commits is paired later; changes reach SVN once in correct dependency order. |
| R07 | Parent SVN ahead and existing SVN target ahead; retain both sides' work or surface real conflicts without overwrite. |
| R08 | Unrelated Git-first root, forged marker or ambiguous baseline is rejected before remote mutation. |
| R09 | Rebase/amend/squash/reset/force push of already-synced versus unsynced work; quarantine before unsafe replay, with and without webhook. |
| R10 | Same-tip equality, missing objects/shallow history, merge DAG frontier and more than 1,000 pending commits; no false rewrite or skipped backlog. |
| R11 | Snapshot at fixed R while SVN advances; exact baseline tree, documented history boundary and subsequent bidirectional sync. |
| R12 | Default full-history import still works; existing configuration/default semantics remain compatible. |
| R13 | Parent refresh with pending work/conflicts; exact preview approval; changed tips/policy invalidate the plan; repeated request is a no-op. |
| R14 | Refresh interrupted after each external step; preserved history and recoverable state; optional re-anchor remains explicitly unqualified until proven. |
| R15 | Normal SVN merge/mergeinfo-only change versus branch delete/recreate or changed UUID; revision gaps alone do not imply rewrite. |
| R16 | Remote deleted/unavailable/auth rejected; no hidden initialization/reset/checkpoint advance; bounded retry with useful error. |
| R17 | Two independent repos import concurrently, sharing numeric SVN revisions and branch names; no state/credential/job crossover. |
| R18 | Parent/child/shared-target locking, credential inheritance/rotation, disabled repos, and cleanup ownership boundaries. |
| R19 | Binary/rename/delete/dotfile/LFS/policy cases; explicit filtered/no-target outcomes; no writes outside active projection. |
| R20 | Deletion UI navigates correctly; failure, cancelled/queued state, parent absent, Back and stale deep links remain usable. |
| R21 | Old install→new binary preserves all required data; no-change first sync and one new change each direction succeed. |
| R22 | Repeated/interrupted migration, disk/write failure, future schema rejection, exclusive writer and tested pre-write restore. |
| R23 | Post-external-write recovery does not use blind old-DB restore or checkpoint reset; mappings and remote state agree. |
| R24 | Authorized Git-centric request/status/cancel; stale/duplicate/tampered/out-of-order webhook and hostile branch input handled safely. |

Required assertions are about real remote contents/logs, refs, preserved paths, checkpoint/mapping rows and operation states—not merely exit code, number of files, UI status, or the presence of a trailer. For Git history, verify parent relationships/trees and exact intended changes. For SVN, inspect the specific emitted revisions and file contents. Do not mistake equivalent final content for proof that no duplicate intermediate commits were created.

Mark **PASS / FAIL / PARTIAL / NOT RUN**. Baseline bug demonstrations may be marked `EXPECTED BASELINE FAILURE` in a diagnostic section; that is not candidate acceptance PASS. Missing tools, ignored tests, unreachable providers and empty selected suites remain visible. Mock/local/enterprise/live tiers must never be combined into one misleading green result.

## 6. Compatibility and release gates

The actively deployed version is currently unknown. Begin with known legacy schemas/pinned current-baseline fixtures and document the coverage. Ask Chris for a version/schema inventory only when it becomes necessary for qualification; this should not block initial synthetic sandbox work. Do not request or expose credentials in chat.

Test the installer/update path and deployment artifact, not just the library schema function: DB/WAL consistency, data paths/permissions, configuration, required key material, local refs, restart ownership and ordinary sync all matter. Unknown mapping ownership must not be guessed using a global maximum revision or arbitrary SHA.

The safe restore procedure depends on whether external writes occurred. Before such writes, an old executable plus a consistent old snapshot can be tested as rollback. After external writes, stop and reconcile or roll forward; do not replay from an obsolete checkpoint. Never promise a lossless down migration that drops newly meaningful mappings.

Release qualification is layered: **unit/component → actual local engine → local provider/API/UI → old-install upgrade → disposable authorized enterprise target → Chris's active-environment acceptance.** The exact candidate SHA and artifact are identified at every gate. Data-integrity failures are NO-GO, regardless of aggregate success rate. Existing #41 scripts and historical results must be requalified for the candidate; they are not blanket approval.

## 7. Keep coordination simple

Use one feature branch and one draft PR unless a specific conflict justifies more. Use ordinary commits; do not rebase/force-push a published review head. Do not merge into main. Link issues with `Refs #N`, not automatic closing keywords. Keep issues open while accepted implementation, independent review or required qualification is missing.

Keep `docs/reliability/review/latest.md` as the compact current handoff. Detailed test logs belong in sanitized CI artifacts; include their links, retention/expiry and checksums. Store small durable manifests/summaries in Git when needed. The branch history already records earlier handoffs; no custom status platform is needed.

Chris communicates decisions and relays results to the reviewing chat. The implementing agent and reviewing chat do not share an automatic background conversation. At each agreed gate, publish the exact branch/PR/head and ask for one concrete decision. The reviewer fetches the actual GitHub diff, tests, issues and evidence at that SHA. A later head requires a later review.

Do not repeatedly ask for approval of routine implementation details already inside the agreed scope. Escalate when semantics are ambiguous, a migration could lose information, an invariant cannot be proved, or the requested result would require production access or new destructive authority.

### Required handoff format

```text
STATUS: READY_FOR_REVIEW | BLOCKED | NEEDS_DECISION
EPIC / ISSUES:
BRANCH / PR URL:
REVIEW BASE SHA:
PREVIOUS REVIEWED HEAD (if any):
CURRENT HEAD SHA:
REMOTE HEAD MATCH / WORKTREE STATUS:
GOAL FILE / SHA-256:

DELIVERED:
- What changed and why; distinguish implementation from design.

EVIDENCE:
- Exact commands, exit codes, test counts and skipped/not-run cases.
- CI run links and artifacts tied to this head (identify PR merge-ref testing separately).
- Scenario IDs, real-engine/provider/UI tiers, before/after tree and mapping assertions.
- Migration/recovery evidence; installed-version coverage and gaps.

OPEN:
- Each unfulfilled acceptance item and risk; no blanket 'all done'.
- Goal deviations proposed or approved; no silent scope reduction.

REQUESTED NEXT ACTION:
- One bounded implementation/review decision, with proposed next slice.
```

Separate actual functional code head from a later documentation-only head if applicable. Do not call old CI evidence “current” without identifying the exact tested commit/tree. Do not hash a file containing its own claimed final hash; record the goal hash outside the goal file.

## 8. Bootstrap prompt for this assignment

```text
/goal
Work in chriscase/RepoSync on epic #61. The issues already exist: #62–#71;
#41 has the rollout addendum. Read them and this coordination handoff first.

Execute Phase 0 only: establish a clean isolated feature worktree/branch,
inspect current main versus 87379741779a6259f7eeb52a68cc6f061174e5ef,
reuse the existing tests/validation infrastructure, implement the sandbox
and initial actual-engine reproductions, and document the canonical
lineage/checkpoint, durable-operation and upgrade design for #63/#64/#66.
Copy this contract and the coworker FAQ into docs/reliability/ as instructed.
Do not create duplicate issues or silently change their acceptance criteria.

Preserve SVN-origin lineage and existing installs. Never contact production,
use live credentials, run live acceptance, modify active Git/SVN repositories,
reset checkpoints, force-push, merge, release or deploy. Production semantics
and migration changes require the first review gate; test/design work does not.

Push ordinary commits to feature/reposync-reliability (or a safe non-colliding
branch) and open/update a draft PR to main. Return the required handoff with
base/head SHA, goal hash, commands/counts/skips, scenario outcomes, CI/artifacts,
unknown installed-version coverage, and the smallest proposed next slice.
Stop for review; do not implement the whole epic in one unreviewed pass.
```

## Source map for the initial audit

All code references below describe the reviewed SHA, not runtime proof of a deployed installation:

- [Repository API, import/pairing/deletion](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/web/src/api/repos.rs)
- [Core import and progress](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/import.rs)
- [Git fetch/reset/commit enumeration](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/git/client.rs)
- [Team synchronization and checkpoint fallbacks](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/sync_engine.rs)
- [Current migration runner/schema](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/db/schema.rs)
- [Existing personal snapshot importer](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/personal/src/initial_import.rs)
- [Existing test entry points](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/Makefile)
- [Existing E2E workflow/zero-test and LFS gates](https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/.github/workflows/e2e.yml)

Source review supports the initial hypotheses; #62 is responsible for turning those hypotheses into reproducible failures and candidate-specific evidence.
