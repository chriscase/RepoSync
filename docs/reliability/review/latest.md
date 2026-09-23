# Phase 0 review handoff

**Independent-review correction (2026-09-23):** This file records the earlier Phase 0 submission. [The posted review](phase0-independent-review.md) found that its macOS policy was only a network guard, not a complete isolation boundary, and that its R06 language combined two separate partial fixtures. The core R06 test manually established a child checkpoint; the API fixture began from unrelated Git-first history and is an R08 admission lead. R03 remains synthetic route coverage. The artifact logs were not independently proved sanitized at this gate. See [sandbox.md](../sandbox.md) and [scenarios.json](../scenarios.json) for corrected coverage; earlier wording below is retained as historical submission evidence.

**STATUS:** READY_FOR_REVIEW
**EPIC / ISSUES:** #61; Phase 0 #62 and design portions of #63/#64/#66; diagnostic leads for #65/#67/#70; #41 remains the later release gate.
**BRANCH / PR:** `feature/reposync-reliability` / [draft PR #72](https://github.com/chriscase/RepoSync/pull/72)
**REVIEW BASE SHA:** `87379741779a6259f7eeb52a68cc6f061174e5ef`; fresh remote `main` matched it.
**PREVIOUS REVIEWED HEAD:** none.
**FUNCTIONAL CODE HEAD SHA:** `deacc6b39728b81b9fde74045fa4cb691761d614`. This report is a later documentation-only commit; the current branch head is the commit containing this report, identified in PR #72 and the final relay.
**GOAL FILE / SHA-256:** `docs/reliability/GOAL.md` / `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`. Both GOAL and the coworker FAQ were copied byte for byte from Chris's attachments.
**WORKTREE / REMOTE:** the functional head was pushed with an ordinary push to its feature branch; final docs-only head/clean status are verified in the final relay.

## Delivered

- New isolated macOS entry point, `scripts/reliability-phase0.sh`, compiles in a preparation stage, then runs test binaries with a private HOME/config/temp directory, fixture credentials and no Cargo/SSH credential directories exposed to the test process. A loopback-only OS policy was probed with both an allowed local connection and a rejected external target. This extends the existing real-engine E2E and web integration tests. Existing E2E zero-test and LFS gates were left intact. `tests/docker-compose.yml` remains unqualified for this epic as written because its ports bind beyond loopback and it includes a notification hook.
- Actual team-engine R06 and R09 diagnostic fixtures, plus a local API/provider test for R06 and R02/R03. These assert baseline defects and must be converted to safe expected outcomes when implementation follows. No runtime sync, lifecycle or schema semantics changed. The synthetic fixtures do not claim the coworker's exact commit graph.
- [Design proposal](../design.md) for canonical SVN lineage, directional checkpoints, typed mapping outcomes, legacy-state inventory and #54 migration sequencing, durable operation/cancellation/recovery, pre-reset ancestry gate, and later snapshot/pair/refresh admission. [Scenario matrix](../scenarios.json) retains R01–R24 and marks candidate acceptance NOT RUN.

## Evidence at the functional head

| Command / tier | Exit | Count and result |
| --- | ---: | --- |
| `scripts/reliability-phase0.sh --diagnostics` / actual local engine, API and provider | 0 | 3 passed, 0 failed, 0 ignored; 330 tests filtered by the diagnostic name. These 3 tests assert observed baseline defects, not acceptance PASS. |
| `scripts/reliability-phase0.sh --baseline` / isolated Rust workspace | 101 | 328 passed, 4 failed, 1 ignored; 9 test binaries completed. The four existing team SVN→Git/bidirectional tests fail: `test_team_mode_svn_to_git_sync`, `test_team_mode_commit_mapping_integrity`, `test_team_mode_echo_suppression`, `test_team_mode_bidirectional_sync`. Conflict detection remains ignored for its documented layout gap. |
| `npm run build` / UI | 0 | TypeScript/Vite build passed; no browser tests exist in `web-ui/package.json`. |
| `npm run lint` / UI | 1 | 9 existing lint errors in ImportPhaseGraphic, RepoDetail and Repositories. |
| `cargo fmt --check` / Rust | 1 | Formatting differs across 43 files at this baseline. The two supplied attachments intentionally retain Markdown hard-break spaces. |
| `cargo clippy --workspace --offline -- -D warnings` / Rust | 101 | 11 errors in the existing core library; downstream crates did not run. |

Preparation: `cargo test --workspace --no-run --offline` first exited 101 because `axum` was not cached. After dependency download, unconstrained resolution initially selected `toml` 0.8.20 with an incompatible current transitive combination and exited 101. A **local ignored lockfile** was resolved with `cargo update -p toml --precise 0.8.23`; subsequent preparation exited 0. The repository's dependency declaration and ignored-lockfile policy were not changed. `npm ci` exited 0. Local `git-lfs` is unavailable; the existing CI workflow installs it, but its later LFS step did not execute after an earlier failure. Local Docker Compose/live/soak and active-environment tests were NOT RUN.

Local diagnostic artifacts: `artifacts/reliability-phase0/20260923T175325Z-diagnostics/` (`summary.json` SHA-256 `8a2b2c173035f8cf602fb63320a8b9fda74dcc83eb0189995a806778b344afb0`; log SHA-256 `bfe09a888f5d8aefb3ac1b925cd7d3d5695bcdc443fcabbe4d0a26209000f052`). Local full-baseline artifacts: `artifacts/reliability-phase0/20260923T175042Z-baseline/` (`summary.json` SHA-256 `a715d9c3ff6757b1d3e5f17da087faeda96523f9bbe9ccf647492d76e8514e24`; log SHA-256 `7d044a801fa8b3db79362d55755a953ab234330332fbbef691c4e802c9f76f38`). These ignored local logs are not presented as durable CI artifacts.

**CI for functional head:** PR checks ran on synthetic merge ref `8f3daf05a77f978f266930defde7105f0dbe42e9` (PR head `deacc6b...`). [Dedicated Phase 0 run](https://github.com/chriscase/RepoSync/actions/runs/35898479322) succeeded: 3 diagnostic tests passed, 0 failed/ignored; the artifact's own `source_head` is the merge-ref SHA. [Sanitized artifact ZIP](https://api.github.com/repos/chriscase/RepoSync/actions/artifacts/10768091656/zip) expires **2026-10-07 17:54 UTC**; GitHub artifact digest `sha256:f0b3678ae891b68601c30bc4ece3c2a455fbe344b989be41b1504bbe5b78f40a`. Downloaded `summary.json` SHA-256: `876a12eaf09af81638665bf6eb6d21c4b70e2ac3e7100351cdbf847095889bac`. [General CI](https://github.com/chriscase/RepoSync/actions/runs/35898479397) failed at formatting, so its later gates were skipped. [E2E CI](https://github.com/chriscase/RepoSync/actions/runs/35898479356) built successfully, then failed the same four team tests (5 passed, 4 failed, 1 ignored in that test binary); provenance/LFS steps were skipped. Neither failed run qualifies the candidate.

## Scenario outcomes and open coverage

- **R06 — expected baseline failure:** two Git commits descended from an actual SVN-import commit were skipped after a late-pair checkpoint. SVN target export lacks their file; child sync records lack the Git SHA. The actual API also recorded the local provider's real feature ref as checkpoint without a mapping. Existing target's independent SVN work, conflicts and a fully reconciled plan remain NOT RUN.
- **R09 — expected baseline failure:** after a Git rewrite with the old synchronized SHA no longer ancestral, the real engine created a further SVN revision. Exports show the old and rewritten contents at separate revisions; `sync_records` records both Git SHA/revision pairs. The exact coworker graph, missing objects, merge DAGs and >1,000 commits remain NOT RUN.
- **R02/R03 — partial/failed baseline:** root DELETE only disabled registration; SVN revision and Git main ref were unchanged. Per-repo status reported importing while cancel returned HTTP 404. No real running import process was cancelled, so connection/export/push/crash boundaries remain NOT RUN.
- **R01 — partial:** existing Git→SVN tests pass, but four team SVN→Git/bidirectional baseline tests fail. **R20 — NOT RUN:** no browser test harness is configured, so the deletion-page report is not yet a browser reproduction. All other rows remain as marked in [scenarios.json](../scenarios.json); no candidate acceptance row is PASS.
- **Migration/recovery:** reviewed source schema is version 12. No pinned old-code installation fixture, migrated DB, installer path, interrupted migration, post-external-write recovery or actual deployed-version inventory has been tested. The deployed version is **NOT ESTABLISHED**; no real DB, credentials or active repository were accessed. PR #51 was inspected and remains open; its provenance/LFS work was not copied or claimed as current evidence. #44 and monorepo #52/#54/#60 remain separate scopes.

**Goal deviations:** none. No issues were created or acceptance criteria changed. No RepoSync production sync endpoint or live RepoSync credential, active synchronized repository, checkpoint reset, feature-branch force push, merge, release or deployment was used. The only forced Git update was to a disposable local bare ref inside the R09 fixture.

## Requested next action

Review the fixture oracles, isolated boundary and state/migration design at functional SHA `deacc6b39728b81b9fde74045fa4cb691761d614`, then decide whether to proceed with one bounded slice: a fail-closed team Git ancestry gate **before** checkout reset/replay, converting R09 into a future-facing zero-write regression. Keep #63/#64 schema and operation implementation behind this review of their contract.
