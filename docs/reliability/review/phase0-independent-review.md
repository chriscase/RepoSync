# RepoSync PR #72 — Phase 0 independent review

**Date:** September 23, 2026  
**Decision:** PARTIAL Phase 0 acceptance; CHANGES REQUIRED; bounded continuation authorized. **No merge, release, deployment, or active-environment acceptance.**

This review accepts the useful diagnostic groundwork and the direction of the architecture. It does not accept the sandbox as a complete production-isolation boundary, treat all coworker scenarios as reproduced, approve the migration implementation, or close any issue. Repair the specific proof gaps, then implement the narrow history-containment slice in the accompanying continuation brief. A second planning-only round is not required between those repairs and the bounded implementation, provided its prerequisites pass.

## Revision and evidence identity

| Item | Reviewed value |
| --- | --- |
| Repository / PR | `chriscase/RepoSync`, draft [PR #72](https://github.com/chriscase/RepoSync/pull/72) |
| Base | `87379741779a6259f7eeb52a68cc6f061174e5ef` |
| Functional head | `deacc6b39728b81b9fde74045fa4cb691761d614` |
| Current head | `3d7bb2f0156b804761678fbdff0b49f01b3ae5a3` |
| Goal | `docs/reliability/GOAL.md` |
| Goal SHA-256 | `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8` |
| Current-head tree | `2fb3a934112844e1727471f9c8111ba927cfeb68` |
| Final CI merge commit | `f93587c0329c747e3ecdd23970f7b068efc32a04` |
| CI merge tree | `2fb3a934112844e1727471f9c8111ba927cfeb68` — matches current head |

The ten changed paths are documentation, the sandbox script/workflow, and two integration-test files. No runtime synchronization implementation or schema file is changed. The final commit adds only `docs/reliability/review/latest.md`. The goal's Git blob (`236d7a55f34688cbfc8092e1617190d18d4cb98a`) matches the independently hashed original attachment; the supplied goal SHA-256 is correct.

The reviewer read the changed source, design and test assertions, inspected Git commit/tree metadata, and fetched the final diagnostic and E2E job logs. **The reviewer did not independently execute the macOS harness, repeat the local build/lint commands, or download and rehash the artifact ZIP.** Local test totals and local lint results remain agent-reported unless separately corroborated below.

### CI evidence independently checked

The [final isolated diagnostic run](https://github.com/chriscase/RepoSync/actions/runs/35899093246), job `107310356111`, executed three diagnostic tests: three passed, zero failed, zero ignored, 330 filtered out. These tests expect unsafe baseline outcomes. Their success is not candidate acceptance.

The [final E2E run](https://github.com/chriscase/RepoSync/actions/runs/35899093139), job `107310356598`, built successfully and then reported five passed, four failed and one ignored in `team_mode_e2e`. Three failures reported `nothing to commit (working tree clean)`; the bidirectional test reported that an SVN file had not reached Git. Subsequent provenance and LFS validation steps were skipped. Do not infer LFS qualification from installation of the tool.

Artifact metadata confirms ID `10769010009`, associated with the submitted head, digest `sha256:bfd636accc21d053a6701beebff4f546f9aafc0e71e8e5e1d24ccbad64e23766`, expiration October 7, 2026 at 17:58:17 UTC. That is the provider-reported digest, not a reviewer-recomputed digest. The diagnostic log records a three-file upload: aggregate summary, test output and tool versions.

The agent's disclosed full local results — 328 passed, four failed, one ignored; UI build success; lint/fmt/clippy failures — do not establish upgrade compatibility. The actual installed version is still **NOT ESTABLISHED**.

## Findings

### F01 — P1: the sandbox is a network restriction, not the claimed complete isolation boundary

**Sources:** [sandbox script](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/scripts/reliability-phase0.sh), [workflow](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/.github/workflows/reliability-phase0.yml), [sandbox guide](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/docs/reliability/sandbox.md).

The macOS policy starts with `(allow default)`, denies outbound networking, then permits `localhost:*`. A private HOME and scrubbed environment are useful, but the policy does not confine filesystem access to fixture-owned paths and permits access to other services on the host's loopback interfaces. A live local RepoSync instance or a local tunnel is not made safe simply by having a loopback address. `file://` targets also need ownership checks, not only a local scheme check.

The final CI log additionally shows checkout persisting its authentication header in the source checkout's local Git configuration until post-job cleanup. The runtime retains access to that checkout; setting `GIT_CONFIG_GLOBAL=/dev/null` does not remove repository-local configuration. This is a concrete counterexample to the broad claim that no ambient credentials are exposed. It is **not evidence that a production credential was leaked or that production was contacted**. Checkout v4 documents this behavior and its `persist-credentials: false` option: [official checkout v4 documentation](https://github.com/actions/checkout/tree/v4).

**Required correction:** make the actual test runtime a fixture-only environment, either through an appropriately constrained OS policy or a disposable VM/container with private networking and only necessary mounts. Protect the host filesystem, local services, credential stores and sockets; disable checkout credential persistence in the diagnostic workflow. Enforce target and owned-path admission. Use synthetic canaries to prove denied host-file reads/writes and denied non-fixture loopback access, as well as successful fixture access and rejected external networking. Do not probe actual secrets or production services. Until this is demonstrated, call the existing mechanism a partial network guard rather than complete isolation.

### F02 — P1: R09 is useful, but cannot prove preservation before RepoSync's reset

**Source:** [new core diagnostics](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/crates/core/tests/team_mode_e2e.rs), `diagnostic_r09_rewritten_paired_tip_replays_into_svn`.

The fixture uses the same `git_work` as both the developer checkout and the engine checkout. The fixture itself resets that checkout to the baseline before force-publishing a replacement. Converting only the final SVN-count assertion cannot prove that RepoSync preserves its own preexisting checkout before rejecting the incoming rewrite: the fixture has already changed it.

The replacement also changes file contents. That demonstrates acceptance and replay of a generic non-fast-forward rewrite, but does not reproduce the coworker's precise rebase/duplicate-history graph. Keep the diagnostic as evidence of that narrower failure rather than discarding it.

The negative ancestry assertion accepts any unsuccessful subprocess exit. Git specifies status 1 for a valid negative ancestry result and other nonzero statuses for errors; test those separately. [Official Git documentation](https://git-scm.com/docs/git-merge-base).

**Required correction:** use distinct developer, bare-remote and bridge repositories; establish and verify the SVN-origin baseline through production paths; then rewrite only from the developer clone. Assert the bridge HEAD/index/worktree, owned history, both remote histories and directional checkpoint/mapping records are preserved when the engine rejects the rewrite. Add at least one real rebase or metadata-only amend variant and an explicit rejection-state assertion, so an unchanged file tree or a `nothing to commit` result cannot masquerade as successful containment.

### F03 — P1: the proposed admission gate needs precise cursor semantics and placement

**Sources:** [design](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/docs/reliability/design.md), [sync engine](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/crates/core/src/sync_engine.rs).

The design says an equal tip is a no-op without defining exactly which tip is compared or which direction is skipped. Define:

- `P`: last Git commit whose handling is durably recorded for this repository/pair.
- `O`: previously observed remote tip, when available.
- `R`: freshly observed remote tip for this inspection.
- `L`: current bridge checkout tip.

`O == R` or `L == R` does not establish that `P == R`. For example, `P=A`, `L=O=R=C`, with `A -> B -> C`, still leaves B and C to handle. Even `P == R` means only that this Git direction has no new commits; pending SVN changes must still be processed.

The current cycle fetches SVN changes before fetching Git changes, and the SVN-fetch path can adopt/persist legacy checkpoints. A check placed only just before `git reset` does not automatically satisfy the stronger promise of no checkpoint advancement before rejection. Trace the complete call order. Also, the outer cycle's generic idle/error finalization must not silently erase a distinct reconciliation state.

**Required correction:** define the comparison, the accepted legacy cursor source, unknown/error classifications, checkpoint ownership and allowed side effects before implementing the gate. Fresh inspection fetch metadata and rejection diagnostics are allowed; destructive checkout, logical checkpoint adoption, replay and remote writes are not allowed on rejection. Distinguish absent refs, auth/transport failures, missing/shallow objects and actual non-fast-forward ancestry. Protect unpublished local state. A single ancestry check does not prove arbitrary Git history is SVN-origin or make cross-system publication atomic.

### F04 — P2: R06 comprises two partial proofs, not one valid end-to-end late-pairing reproduction

**Sources:** [core tests](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/crates/core/tests/team_mode_e2e.rs), [API test](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/crates/web/tests/server_freeze.rs).

The core test imports an SVN change and demonstrates the consequence of manually setting a child's checkpoint at its Git tip. It does not call `create_branch_pair` and does not populate the normal repository row for that child. It is a useful checkpoint-consequence test, not the complete API workflow.

The API test does call the real pairing endpoint, but its Git seed is independently created and has no established SVN import/mapping. That makes it an informative **R08 unrelated-history admission** example, not proof of valid SVN-derived late pairing. The two fixtures do not share the same proven source graph.

**Required correction:** split/relabel the evidence. Preserve both useful tests. Before claiming R06 end-to-end coverage or implementing #67, connect real SVN-derived baseline creation, the real API pairing operation, the resulting persisted child row, and the actual child engine in one fixture. Assert baseline trees as well as commits/counters. This integrated R06 extension is not a prerequisite to the narrowly scoped rewrite-containment implementation if the reporting is corrected now.

R03 likewise proves a missing route while an importing status is synthetically assigned, not a running importer that resisted cancellation. The existing handoff correctly discloses that limitation; retain it.

### F05 — P2: any-test-passed is not a required-scenario gate, and evidence needs stronger reproducibility

**Sources:** [script](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/scripts/reliability-phase0.sh), [matrix](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/docs/reliability/scenarios.json), [handoff](https://github.com/chriscase/RepoSync/blob/3d7bb2f0156b804761678fbdff0b49f01b3ae5a3/docs/reliability/review/latest.md).

The script filters by `diagnostic_` and rejects only a zero total. It can remain green if R09 is removed, ignored, or renamed outside the filter while another diagnostic passes. Candidate regressions must be required by identity, not by a positive aggregate count.

The artifact records counts and terse observations, not retained per-scenario before/after tree and mapping evidence. The matrix is maintained separately, and `sandbox.md` uses candidate-status wording inconsistent with the JSON's NOT RUN values. Preparation also depends on an ignored resolved lockfile and a CI-only `cargo update` step; the exact dependency graph is not retained with the diagnostic evidence.

**Required correction:** define a required test/scenario manifest and fail when required cases are missing, filtered, ignored or failed. Generate per-case outcomes tied to the executed SHA; separate successful defect demonstrations from candidate acceptance. Preserve sanitized before/after manifests, command output, tool versions, lockfile/dependency graph and hashes. Keep one reproducible preparation path for matched-base and candidate runs. Format new code without broad unrelated style churn. Do not call raw logs sanitized merely because they were piped through `tee`; establish a safe fixture-only export policy and synthetic secret-canary check.

### F06 — P0 release blocker, pre-existing: failed SVN apply may advance past unapplied work

**Source:** [unchanged `sync_svn_to_git`](https://github.com/chriscase/RepoSync/blob/deacc6b39728b81b9fde74045fa4cb691761d614/crates/core/src/sync_engine.rs), particularly the `if !diff_applied` branch.

When patch application fails, the current code can call `advance_svn_watermark`, record a failure and continue. That is a concrete data-integrity risk under the epic's checkpoint contract. It is not introduced by this PR. The four failing team tests are also meaningful release blockers, not harmless warnings to suppress. Their exact individual root causes still need investigation; this review does not attribute all four to this branch.

**Required handling:** retain a visible blocker under #62/#63 and characterize it against the exact base with the same dependency graph. Do not reset watermarks, force a reimport, treat `nothing to commit` as unconditional success, or weaken tests to make the suite green. The immediate #66 containment slice need not repair every existing apply defect. However, any new regression it causes must be fixed, and ordinary healthy-path controls must remain meaningful. No production qualification while required data-integrity cases are unresolved.

## Design disposition

Accepted in direction: immutable pair generations; SVN UUID/path/origin identity; distinct incoming and outgoing checkpoints; typed handled/filtered/pending outcomes; a SQLite-backed operation journal; single-writer ownership; explicit cancellation quiescence; recovery that verifies external effects; and preservation of existing installations rather than implicit reimport.

Not yet accepted as implementation-ready: migration DDL and legacy adoption rules; recovery at each partial-effect boundary; durable cancellation; remote-write concurrency guarantees; and broad schema rollout. Those remain behind their issue-specific evidence gates. Clarify how a job with earlier committed steps and a later known failure is represented; do not collapse it into a no-effect failure. Keep credential rotation separate from history/projection changes so ordinary rotation does not invalidate otherwise valid lineage.

## Authorized next pass

Use the accompanying `RepoSync-PR72-bounded-continuation.md`. Repair F01/F05 and the relevant fixture/design precision first, then implement one pre-reset team-history containment slice of #66. Keep the same draft PR and ordinary append-only commits. Preserve the original GOAL bytes. Leave the broader #63/#64 implementation, migration, lifecycle features and production work out of scope.

The next handoff must distinguish findings corrected, subcases proven, remaining issue acceptance, exact executable/dependency provenance and test-tier limits. Good partial progress is useful; a green diagnostic job is not permission to claim the whole reliability epic is complete.
