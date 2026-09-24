# Bounded reliability continuation review handoff

**STATUS:** READY_FOR_REVIEW. This document records the bounded #66 pre-reset containment pass, following the independent Phase 0 review. No issue acceptance criteria changed, and no whole R01–R24 scenario is declared complete.

**EPIC / ISSUES:** #61; correction evidence #62; designs #63/#64; bounded #66 gate. #41 remains the later release gate. [Draft PR #72](https://github.com/chriscase/RepoSync/pull/72) remains open against `main`.

| Identity | Exact value |
| --- | --- |
| Original reviewed base | `87379741779a6259f7eeb52a68cc6f061174e5ef` |
| Previous reviewed head | `3d7bb2f0156b804761678fbdff0b49f01b3ae5a3` |
| Current functional/test head before this documentation commit | `6679f84202be59c810753db714494e2b59a66a15` (formatting-only successor of tested `ac76a8a7dfe88acd9238c87155441cbf2afa09b6`) |
| Final documentation head | Report in the PR/final relay after this document is committed; no self-referential SHA is claimed here. |
| `GOAL.md` SHA-256 | `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8` (original bytes preserved) |
| `NEXT-SLICE.md` SHA-256 | `266cb1eea867e65b53483c25107b55556e915a3d18e746e7bdbf85d704e6e703` (attachment copy) |
| Resolved `Cargo.lock` SHA-256 | `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05` |

## Findings from the posted Phase 0 review

| Finding | Disposition at this gate | Evidence and remaining limit |
| --- | --- | --- |
| F01 sandbox boundary | Corrected for the fixture runtime; application-wide enrollment partial | `Dockerfile.reliability`, `scripts/reliability-container.sh`, and the CI workflow separate dependency build from a read-only, no-network, no-host-source test container. `persist-credentials: false`; private tmpfs and six synthetic canaries prove fixture access, denied fake host file read/write, denied host loopback/egress, and denied `file://` traversal/symlink. Fixture helper paths are canonical under `/fixture/tmp`. This does not establish enrollment checks for every normal-installation application path. |
| F02 rewrite proof | Corrected for bounded gate | SVN-origin production mapping and cursor precede the rewrite. Independent developer, bare remote and bridge clones exercise changed-content replacement, original replacement and metadata-only amend. `non_fast_forward` blocks before checkout reset; bridge HEAD/index/tree, Git remote SHA/tree, SVN revision/export, cursor, mappings and success count are unchanged on repeat and restart. Full rebase/reconciliation remains open. |
| F03 cursor/gate precision | Corrected for bounded gate | `docs/reliability/design.md` defines P/O/R/L. `inspect_team_history` runs before SVN legacy adoption, resolves repository-owned P, pins freshly advertised/fetched R, distinguishes command error from valid negative ancestry, and blocks dirty/unpublished or unqualified DAG/backlog before writes. P=R still permits SVN work; L=R with older P replays pending commits. Multiwriter/publication atomicity remains #64. |
| F04 R06 fixture claims | Corrected reporting; integrated R06 NOT RUN | R06 core test manually establishes a child checkpoint after SVN import. The separate API fixture begins with unrelated Git-first history and is an R08 lead. R03 is synthetic route coverage, not actual in-flight cancellation. Matrix and sandbox guide now say so. |
| F05 required evidence | Corrected for bounded cases | `required-cases.json` and `reliability-runtime.py` demand exact test identities, one pass each, zero ignored/failures, and self-check that omission of R09 fails. Per-case sanitized proof JSON/logs, source head/tree, tool versions and lock/goal hashes are retained. Exact-base and candidate comparison uses one Dockerfile/toolchain/lock with a test-only synthetic SVN-author overlay; no base runtime change. New gate/fixture regions were formatted separately; the older repository-wide formatting failure remains visible. |
| F06 SVN apply failure | Tracked existing P0 blocker; not repaired in this slice | Unchanged `sync_svn_to_git` can advance an SVN watermark after failed apply. Recorded on [#62](https://github.com/chriscase/RepoSync/issues/62#issuecomment-5804597973). The four existing team E2E failures remain visible; their individual causes are not all attributed to F06. No migration or live qualification is claimed. |

## Required real-engine cases

The exact command for the contained suite is `scripts/reliability-container.sh --all`. `docs/reliability/required-cases.json` is the executable manifest. The two diagnostics pass by *observing old defects*; the 19 candidate cases pass by exercising the new gate (18 regression checks and one admission check). Each row remains a subcase, not whole-scenario acceptance.

| Case ID | Exact Rust test | Outcome |
| --- | --- | --- |
| R02_R03_ROUTE | `diagnostic_r02_r03_root_delete_disables_and_per_repo_cancel_is_missing` | BASELINE DEFECT OBSERVED |
| R06_CHECKPOINT | `diagnostic_r06_late_pair_checkpoint_omits_existing_git_work` | BASELINE DEFECT OBSERVED |
| R09_ORIGINAL | `candidate_r09_original_replacement_fixture_rejected` | PASS, bounded subcase; scenario PARTIAL |
| R09_REPLACEMENT | `candidate_r09_changed_content_rewrite_rejected` | PASS, bounded subcase; scenario PARTIAL |
| R09_AMEND | `candidate_r09_metadata_amend_rejected` | PASS, bounded subcase; scenario PARTIAL |
| R01_LINEAR | `candidate_r01_two_pending_git_commits_sync_once` | PASS, bounded subcase; scenario PARTIAL |
| R10_L_EQUALS_R | `candidate_r10_caught_up_bridge_lagging_cursor_replays` | PASS, bounded subcase; scenario PARTIAL |
| R01_SVN_PENDING | `candidate_r01_unchanged_git_still_admits_new_svn_work` | PASS, gate admission; R01 PARTIAL |
| R16_MISSING_BRANCH | `candidate_r16_missing_remote_branch_blocks_stale_tracking_ref` | PASS, bounded subcase; scenario PARTIAL |
| R16_TRANSPORT | `candidate_r16_remote_transport_failure_blocks_without_reset` | PASS, bounded subcase; scenario PARTIAL |
| R16_AUTH | `candidate_r16_auth_denial_is_distinct_from_transport` | PASS, bounded subcase; scenario PARTIAL |
| R16_FETCH | `candidate_r16_fetch_failure_does_not_use_stale_ref` | PASS, bounded subcase; scenario PARTIAL |
| R10_MISSING_OBJECT | `candidate_r10_missing_checkpoint_object_blocks` | PASS, bounded subcase; scenario PARTIAL |
| R10_MISSING_CURSOR | `candidate_r10_missing_repository_cursor_does_not_borrow_global` | PASS, bounded subcase; scenario PARTIAL |
| R10_AMBIGUOUS | `candidate_r10_ambiguous_repository_cursor_blocks` | PASS, bounded subcase; scenario PARTIAL |
| R10_SHALLOW | `candidate_r10_shallow_history_blocks` | PASS, bounded subcase; scenario PARTIAL |
| R10_ANCESTRY_ERROR | `candidate_r10_ancestry_command_error_is_not_rewrite` | PASS, bounded subcase; scenario PARTIAL |
| R09_LOCAL | `candidate_r09_local_dirty_index_and_unpublished_commit_preserved` | PASS, bounded subcase; scenario PARTIAL |
| R10_MERGE | `candidate_r10_merge_dag_rejected_before_replay` | PASS, bounded subcase; scenario PARTIAL |
| R10_OVERFLOW | `candidate_r10_over_1000_pending_commits_rejected` | PASS, bounded subcase; scenario PARTIAL |
| R17_SCOPE | `candidate_r17_repository_cursors_remain_scoped` | PASS, bounded subcase; scenario PARTIAL |

**Not run as full acceptance:** R02/R03/R06/R08 remain partial or failed baseline observations. R04/R05/R07/R11–R15/R18–R24 are NOT RUN; R20 has no browser runner. R01/R09/R10/R16/R17 candidate status is PARTIAL in `scenarios.json`. No migration, operation system, cancellation or production acceptance ran.

## Before/after proof and allowed rejection effects

The required-case artifact includes one JSON/log pair per case. For changed-content R09, the independent developer ref rewrites the already-handled history. The tested result is `HistoryBlocked(non_fast_forward)` on the first poll, repeat, and reopened engine/DB. The snapshot comparison checks bridge HEAD, index bytes, worktree status/tree, remote SHA/tree, SVN revision/export content, per-repository watermark, legacy cursor, mapping count and success count. The test permits only fetched objects and `refs/reposync/inspection/incoming`, plus a scoped block/status/audit/error record; it does not permit remote writes or logical checkpoint advance. Metadata-only amend has the same explicit rejection even when Git tree content matches. Local dirty/index/unpublished commits remain in place.

For qualified linear history, P is the SVN-import-derived handled Git SHA. Two developer commits reach the SVN target in order (revisions 3 and 4 in the fixture), with an intermediate revision/tree and mappings for both Git SHAs; remote tree remains stable and the next poll is a no-op. With L=R but P older, both commits still replay. With P=R and a new SVN revision, the SVN direction writes its expected Git mapping. A separate healthy pair proceeds while another pair's missing cursor object remains blocked. Merge DAG and >1,000 pending commits are explicit pre-write unsupported blocks, not silent truncation.

Concrete values from the successful isolated artifact (fixture SHAs are deterministic only within that run):

| Subcase | Before | After / finding |
| --- | --- | --- |
| R09 changed-content replacement | Bridge/P `89930a4874b855f81705991e3073bf35cbff3405`, bridge tree `1798ee06f3dc6ad7f05fbae4361b3e05fbcd8eb8`, remote rewritten tip `d595aa309a1aaf9e485aaa6c8902403c526405b2`, remote tree `52bb37359c5e1d56282191a0d59db08199896095`, SVN r3 exporting `first version`, watermark `(2, P)`, 2 mappings and 2 successes. | `non_fast_forward`; every listed bridge/remote/SVN/cursor/mapping/success field equal before and after. Index SHA-256 `8cae0c23fdfd9670dc34388389d74c998369b39f182ae4725f01cb4e3ffd4ec8` preserved. Repeat and reopened engine/DB reject again. |
| R09 metadata amend | Bridge/P `5280cb86e2105d54f7f1354218b53e5b59d84e51`, amended remote `17b9060416d6fa28f4d78f0baf27d434cd03662f`; both trees `1798ee06f3dc6ad7f05fbae4361b3e05fbcd8eb8`, SVN r3, 2 mappings. | `non_fast_forward` despite equal trees; bridge/index, remote, SVN, watermark, mappings and successes preserved through repeat/reopen. |
| R01 two-commit fast-forward | P/bridge `571500fc28813f8fdadcd6a208e4a6c3f0e9c09a`, SVN r2, 1 mapping; developer first commit `f0c9b05a9a2ec9182d33ed41b15c926594a91be8`, R `10db4e2bd5860f6d324f94a204d66525b3d18a81`. | Bridge reaches R; SVN r3 then r4 with intermediate file proof, 3 mappings total; remote tree stays `5f0fffa7ec196e44388925d25eeeb73426b6ac90`; repeat is a no-op. |
| R10 L=R with P lagging | P `e57c4cbf0a06f2df609709882ef6e1c9bf1a2db1`, L=R `b2ac9093be8bb17f2622819801dc08b5db72bae8`, SVN r2, 1 mapping. | SVN r4, 3 mappings: the two pending Git commits were not skipped. |
| R01 P=R with new SVN work | P=R `609113935f39c41d233e40774767df8b9eb32775`, SVN r2. | Newly committed SVN r3 is processed and one SVN→Git mapping is added; the full broader R01 round-trip is still partial. |

The scoped block record uses existing key-value storage; it is not a durable operation journal. Rejection status is `reconciliation_required`; repeated polling and reopening preserve the reason. The remote may move after inspection or another writer may publish concurrently; pinned R protects the local reset input but does not supply cross-system atomicity.

## Tests, comparison and CI

- Required isolated suite: exact test manifest; 2 baseline observations + 18 candidate regressions + 1 candidate admission = **21 passed, 0 failed, 0 ignored** on the successful isolated run preceding this report; 551 filter exclusions across per-case binary invocations. Six boundary canaries PASS; R09 omission self-check rejected. The functional predecessor `ac76a8a...` completed these cases in [successful dedicated CI](https://github.com/chriscase/RepoSync/actions/runs/35937121407); formatting-only head and final documentation-head CI are checked before the final relay.
- Parallel broad E2E at functional head `ac76a8a7dfe88acd9238c87155441cbf2afa09b6`: [run](https://github.com/chriscase/RepoSync/actions/runs/35937121381) built; the team binary reported **23 passed, 4 failed, 1 ignored, 0 filtered**. All 19 new candidate cases passed. The four failures are `test_team_mode_bidirectional_sync`, `test_team_mode_commit_mapping_integrity`, `test_team_mode_echo_suppression`, and `test_team_mode_svn_to_git_sync`, matching the previous reviewed baseline. LFS/provenance jobs after that step were skipped. General CI remains blocked at pre-existing formatting.
- Matched comparison command: `scripts/reliability-compare.sh`, archiving exact base `8737974...`, applying only the recorded test-helper SVN-author overlay, and running `scripts/reliability-container.sh --baseline` for both base and candidate with `Cargo.lock` SHA above. The exact matched result was **base 324 passed / 5 failed / 1 ignored / 0 filtered, candidate 345 passed / 5 failed / 1 ignored / 0 filtered**; catalogs 330 and 351, with 21 added tests and **zero regressions among old passing tests**. All five base failures remained failures: the four named team tests plus `integration::test_full_svn_to_git_cycle_with_metadata` (SVN revprop hook refusal in the isolated fixture). The conflict test remained ignored. The test-only base overlay SHA-256 was `fa8b4c40a6eee781cd19b8ab0056b0352656eaa04b3a133b6cc8d9615dc09904`; base runtime source was unchanged.
- Local build check: `cargo check -p reposync-core --tests --locked --offline -j 2` passed. Docker Desktop on this host returned daemon I/O errors after its earlier cache filled the disk; the isolated Docker execution ran in GitHub CI, and the host daemon was not reset/pruned.

**CI source identity and artifacts:** The [successful dedicated run](https://github.com/chriscase/RepoSync/actions/runs/35937121407) used synthetic PR merge head `12128e71a6d23b63ca8acf45fc1b91cc4030b69f` and tree `cdf44d7a1cea3ace9ce39562ef292849ec9fa0af`, the same tree as feature head `ac76a8a7dfe88acd9238c87155441cbf2afa09b6`; its original base was `8737974...` with tree `f383bb9b123f3f75b2dde7f17f7311a264946abd`. [Artifact 10783846326](https://api.github.com/repos/chriscase/RepoSync/actions/artifacts/10783846326/zip) has GitHub API digest `sha256:b90c4cba45863fcef8f0e8ccaafe13cfc1d43ca333c2c7b1405282dbe690bf5b` and expires **2026-10-08 00:16:48 UTC** (14-day retention). Downloaded `summary.json` SHA-256 is `674fdb76a6034025c020ab2c0692fa4d535453c6ad213b4460fe9143ec7d0b1f`; `comparison.json` SHA-256 is `ea5c57995fb5637d2a62fb4872576146ae4c539c0e0dd24aa12b26ce5812efa0`. The artifact contains synthetic fixture outputs only; raw SQLite databases and synthetic secret values are not exported. The later formatting-only and documentation-head checks will be listed in the final relay.

## Remaining gaps and next slice

The deployed version is **NOT ESTABLISHED**. No pinned old executable/schema fixture, migrated installation, interruption/restart migration, post-external-write recovery, full SVN UUID/copy-origin lineage certificate, distributed single-writer guarantee, API→child-engine SVN-origin R06, running-import cancellation, browser R20 or live acceptance exists at this gate. #63/#64 still need legacy inventory and additive, restart-safe migration/operation design qualification. #66 containment still needs reviewed multiwriter/publication handling and support or explicit product policy for merge DAGs/backlogs. No schema, general operation system, merge, release, deployment, production target, live credential or active synchronized repository was touched. No feature-branch force push or checkpoint reset occurred; only disposable fixture refs were forcibly updated to create rewrite tests.

**Recommended next smallest slice:** repair F06's existing `sync_svn_to_git` failed-apply branch so it cannot advance the SVN watermark for an unapplied revision, with one real-engine failure/retry fixture and a matched-base comparison. Keep recovery after an external write and the broader #63/#64 migration work behind their own review gate.
