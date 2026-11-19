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

For `R01_SVNSERVE_POST_WRITE_RECOVERY`, inspect `detail.git_push_checkpoint_recovery.status` in the current suite summary. CI requires this subcase to report **PASS** even when the overall row reports **PARTIAL**. It exercises an SVN→Git push that reaches the disposable Git remote before a local checkpoint write fails. The fixture reopens the database with the push operation still active and runs the scheduler's pre-cycle reconciliation. The row also retains the earlier simulation that deletes a `sync_records` row and rewinds the repository watermarks *after* a successful cycle. That separate recovery gap accounts for the row's PARTIAL exemption.

The deployed executable/schema is **NOT ESTABLISHED**. No live acceptance is authorized from this matrix.

Host catalog PASS is not enterprise or live acceptance. Qualification tiers, the #41 go/no-go checklist, and the candidate report Chris uses to decide are in [the enterprise soak runbook](../enterprise-soak-runbook.md) and [candidate-report-template.md](candidate-report-template.md). Until the installed version is known, that version's migration coverage stays **NOT RUN**. local/offline PASS is not enterprise/live PASS.

### R17 scheduler completion events

On Unix, run the daemon test from the repository root with Git and Subversion installed.

```sh
cargo test -p reposync-daemon scheduler::notification_isolation_tests::r17_concurrent_sync_events_match_their_repository -- --exact
```

Expect one passing test. Two repositories share a Git branch name, an SVN revision number and a file path. Each has its own SVN repository and bare Git origin.

The test subscribes to the scheduler’s in-process event channel before either worker starts. Two scheduler ticks admit the workers in sequence. Before the test releases either push, it requires both Git receive hooks to be waiting, both push operations to be active in SQLite, and both refs to point to their original commits. It releases the pushes in reverse admission order. For each completion event, the test checks the repository identity and commit details against the fixture’s source revision, applied SQLite mapping and Git history.

To exercise both repositories becoming due on the same scheduler tick, run this case and expect one pass.

```sh
cargo test -p reposync-daemon scheduler::notification_isolation_tests::r17_same_tick_pushes_complete_for_both_repositories -- --exact
```

Both workers reach their Git receive hooks before either ref changes. The fixture releases both hooks before joining the workers. Each repository must finish idle with one new commit, one applied receipt and its own r2 checkpoint. Earlier runs exposed SQLite contention both before the push, when inserting the intent, and after publication, when inserting the receipt. In the latter case the attempt to record a reconciliation hold could also fail. A rejected push also exposed a repository left at `applying` after its journal entered `reconciliation_required`. Each journal transition and the guarded repository status update now reserve the SQLite writer before checking the current state. The workers can still push to their separate Git repositories concurrently.

`R17_SVNSERVE_NOTIFICATION_ISOLATION` remains **PARTIAL**. That native case checks each stored webhook URL and searches status and audit records for the other repository’s markers. Both daemon tests observe events before delivery. The Teams listener selects one global destination, so delivery to separate repository-specific destinations remains unqualified.

## Regenerating the human scenario file

```sh
python3 scripts/reliability-acceptance-matrix.py --sync-scenarios
```
