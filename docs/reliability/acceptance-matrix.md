# #62 acceptance matrix (current main)

This is the host-side rollup for epic [#61](https://github.com/chriscase/RepoSync/issues/61) / [#62](https://github.com/chriscase/RepoSync/issues/62). It does **not** replace the sealed Docker runner.

## Entry points

| Command | What it proves | What it does not prove |
| --- | --- | --- |
| `python3 scripts/reliability-acceptance-matrix.py --check --report` | Manifest completeness, R01–R24 rollup, GOAL hash, no silent PASS | Runtime SVN/Git correctness |
| `scripts/real-engine-scenario-suite.sh` + `real-engine-scenario-report.py ci-gate` | Host svnserve real-engine scenarios; suite exits `3` on PARTIAL but `1` if any scenario FAIL; ci-gate requires every cataloged real-engine case id and rejects SKIP | Full R16 catalog beyond loopback fixtures |
| `scripts/reliability-container.sh --all` | Isolated required-case execution against disposable remotes | Deployed-version or live acceptance |
| `scripts/reliability-compare.sh` | Matched-lock base vs candidate identities | Production upgrade |

`docs/reliability/acceptance-matrix.json` is the source of truth for R01–R24 status. `docs/reliability/required-cases.json` remains the exact isolated-case catalog. `docs/reliability/scenarios.json` is regenerated from the matrix and must stay in agreement.

A scenario may be marked **PARTIAL** when named subcases pass and required issue criteria remain open. **PASS** is reserved for a complete issue-level criterion with executed exact cases and an empty open list. Missing tools, filtered cases, or an unavailable Docker daemon are **NOT RUN**, never PASS.

### Real-engine CI gate (loopback svnserve)

`scripts/real-engine-scenario-suite.sh` still exits `3` whenever the rollup is **PARTIAL** so local runs stay honest. GitHub Actions runs `python3 scripts/real-engine-scenario-report.py ci-gate` on the emitted `artifacts/real-engine-scenarios/*/summary.json` afterward. The gate passes only when every scenario is **PASS**, or **PARTIAL** with an ID in the [CI partial allowlist](real-engine-ci-partial-allowlist.json). Both R16 remote-unreachable scenarios must report **PASS**. Any **FAIL**, any **NOT RUN**, or any **PARTIAL** not on that allowlist keeps Build & Test red. Checker fixtures: `python3 scripts/real-engine-scenario-report.py self-test`.

The deployed executable/schema is **NOT ESTABLISHED**. No live acceptance is authorized from this matrix.

Host catalog PASS is not enterprise or live acceptance. Qualification tiers, the #41 go/no-go checklist, and the candidate report Chris uses to decide are in [the enterprise soak runbook](../enterprise-soak-runbook.md) and [candidate-report-template.md](candidate-report-template.md). Until the installed version is known, that version's migration coverage stays **NOT RUN**. local/offline PASS is not enterprise/live PASS.

### R01 recovery after an SVN→Git publication

Run the focused case on Unix with Git, Subversion and Rust installed. The fixture creates its own SQLite database, loopback svnserve repository and bare Git origin.

```sh
cargo test -p reposync-core --test real_engine_scenarios scenario_r01_svnserve_post_write_recovery -- --exact --nocapture
```

Expect one passing Rust test and a **PARTIAL** scenario receipt. The CI gate requires four separately reported results inside its `detail` object.

| Receipt field | What the fixture establishes |
| --- | --- |
| `git_push_checkpoint_recovery` | Git accepted the push before SQLite rolled back finalization. A fresh engine finds the active operation, and the scheduler’s reconciliation routine verifies the published commit and tree. Finalization records one applied receipt. Two quiet cycles leave the remote ref unchanged. |
| `finalized_history.mirror_recovery` | After successful finalization, only `last_svn_rev_<repo>` is missing. The import-baseline resolver copies the intact registration revision into that key. The existing receipt, completed operation and publication survive. The pending r4 SVN change reaches Git exactly once, followed by two quiet cycles. |
| `finalized_history.contradictory_mirror_hold` | The SVN column was rewound and its mirror removed. The current generation’s SVN→Git checkpoint and mapping agree with a later completed push. The resolver reports `conflicting_finalized_svn_checkpoint`, leaves the mirror absent and keeps the later publication intact. Two cold cycles leave the pending r4 unpublished. |
| `finalized_history.historical_hold` | After r3 completed, the fixture rewound both registration columns to r1 and removed the r3 applied receipt. The scoped SVN mirror still says r3, and the handled Git baseline still identifies the r2 publication. The scheduler’s baseline resolver reports `conflicting_import_checkpoint`. A direct engine cycle first checks Git history and reports `ambiguous_checkpoint`. Repeated admissions preserve the published histories and leave the receipt absent. |

The contradictory-mirror case runs once for a persisted `direct` registration and once for `pr`. Both use the managed SVN→Git worker. The receipt records each registration mode and push initiator. The backfill check runs when the SVN mirror is physically absent. Matching but stale existing cursor copies remain outside this check.

For a historical hold, keep the affected pair paused. Capture its repository ID, operation ID if visible, redacted reason and last recorded tips. A signed-in user can read the recorded status with `GET /api/status?repo_id=<id>`. An administrator with the operation ID can read the push journal with `GET /api/repos/<id>/git-push/<op>`. For a completed push with no active operation, the reconcile endpoint returns the recorded completion. See [Reading the status](../coworker/status-and-reconciliation.md#reading-the-status).

Preserve a consistent database copy. Compare its scope and generation, SVN UUID and path incarnation, directional checkpoint, applied row and completed operation with the SVN revision and full Git commit, tree and branch from read-only repository history. The matching journal, mapping and histories confirm that r3 was published. The deleted receipt's identity and a safe restart cursor for the damaged registration still require independent proof. Escalate the discrepancy before resuming sync. Recovery of the lost applied receipt remains open under [#61](https://github.com/chriscase/RepoSync/issues/61) and [#62](https://github.com/chriscase/RepoSync/issues/62). See the [lineage and checkpoints contract](contracts/63-lineage-checkpoints.md).

### R15 ordinary SVN merges

Run the managed trunk fixture with Git and Subversion installed. It creates a disposable svnserve repository and bare Git origin.

```sh
cargo test -p reposync-core --test real_engine_scenarios scenario_r15_ordinary_svn_merge_and_mergeinfo -- --exact --nocapture
```

Require one passing test and exactly one `RELIABILITY_EVIDENCE` record with case `R15_SVNSERVE_ORDINARY_MERGE` and status `PASS`. Missing tools produce a `NOT RUN` record. The same case is required by the real-engine suite. Its source branch contains work outside trunk, so the SVN repository’s youngest revision can be ahead of the managed cursor. The fixture checks which changes actually reach trunk and Git.

| Source operation | Expected observation |
| --- | --- |
| Merge a content change from the feature branch into trunk | SVN records the trunk revision and its mergeinfo. Git receives one commit whose parent is the previous published commit. The applied receipt maps the **trunk merge revision** to that commit. |
| Use `svn merge --record-only` for a separate feature revision | Trunk mergeinfo advances. The feature content stays on its source branch. RepoSync records a no-target outcome for the trunk revision, preserves the Git ref and creates no applied Git mapping for it. |
| Reopen the engine and sync a subsequent ordinary trunk edit | The edit reaches Git once. Two unchanged cycles preserve the refs, SVN history and mapping records. |

R15 remains **PARTIAL**. Repository replacement and same-path recreation have separate lineage checks. Automatic generation changes for those events remain unqualified.

### R17 scheduler completion events

On Unix, run the daemon test from the repository root with Git and Subversion installed.

```sh
cargo test -p reposync-daemon scheduler::notification_isolation_tests::r17_concurrent_sync_events_match_their_repository -- --exact
```

Expect one passing test. Two repositories share a Git branch name, an SVN revision number and a file path. Each has its own SVN repository and bare Git origin.

The test subscribes to the scheduler’s in-process event channel before either worker starts. Two scheduler ticks admit the workers in sequence. Before the test releases either push, it requires both Git receive hooks to be waiting, both push operations to be active in SQLite, and both refs to point to their original commits. It releases the pushes in reverse admission order. For each completion event, the test checks the repository identity and commit details against the fixture’s source revision, applied SQLite mapping and Git history.

To exercise two repositories becoming due on the same scheduler tick, run each case and expect one pass per command.

```sh
cargo test -p reposync-daemon scheduler::notification_isolation_tests::r17_same_tick_pushes_complete_for_both_repositories -- --exact
cargo test -p reposync-daemon scheduler::notification_isolation_tests::r17_same_tick_rejections_count_one_error_per_repository -- --exact
```

Both workers reach their Git receive hooks before either ref changes. The fixture releases both hooks before joining the workers. After successful pushes, each repository must be idle with one new commit, one applied receipt and its own r2 checkpoint. When both receive hooks reject, each repository must retain its r1 Git ref and checkpoint, record a reconciliation hold and count one error.

Earlier runs exposed SQLite contention both before the push, when inserting the intent, and after publication, when inserting the receipt. In the latter case the attempt to record a reconciliation hold could also fail. A rejected push also exposed a repository left at `applying` after its journal entered `reconciliation_required`. Another rejected push recorded a reconciliation hold and terminal status but missed its error count. Push intent creation, ordinary team push confirmation, updates to an active push journal, repository status changes and error-count increments now reserve the SQLite writer before their checks. The workers can still push to their separate Git repositories concurrently.

`R17_SVNSERVE_NOTIFICATION_ISOLATION` remains **PARTIAL**. That native case checks each stored webhook URL and searches status and audit records for the other repository’s markers. The daemon tests observe events before delivery. The Teams listener selects one global destination, so delivery to separate repository-specific destinations remains unqualified.

## Regenerating the human scenario file

```sh
python3 scripts/reliability-acceptance-matrix.py --sync-scenarios
```
