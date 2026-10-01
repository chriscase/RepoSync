# Post-#75 #62 first-review-gate handoff

```text
STATUS: READY_FOR_REVIEW
EPIC / ISSUES: #61; this gate #62 plus design for #63/#64/#66
BRANCH / PR URL: feature/reposync-reliability (draft PR to main)
REVIEW BASE SHA: 93e3f5cf24e347d7be477c2c9e0e90531268b7fc (current main = merge of #75)
PREVIOUS REVIEWED HEAD: f9009e7e862e3910480256d3609d99ac000f9665 (merged PR #72);
  54e183484961bd9ad12bd03505b3b00610de7cd9 (merged PR #74); 93e3f5c (merged PR #75)
CURRENT HEAD SHA: e456f96371cf9af46c3cc41bc174390004706079 (functional/docs head
  before this evidence note; a later docs-only commit may follow)
REMOTE HEAD MATCH / WORKTREE STATUS: ordinary commits on feature/reposync-reliability
  fast-forwarding the previously merged feature branch
GOAL FILE / SHA-256: docs/reliability/GOAL.md
  16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8
```

## DELIVERED

Host-side #62 acceptance matrix and checker against current main, plus
reviewable contracts for #63/#64/#66. No schema activation, no general
operation service, no rewrite-replay product change, no merge to main, no
live acceptance.

Commits on this increment, in order:

1. `test(reliability): add current-main #62 acceptance matrix`
2. `docs(reliability): #63 lineage and checkpoint contract`
3. `docs(reliability): #64 durable job and recovery contract`
4. `docs(reliability): #66 rewrite containment contract`
5. this handoff
6. host-test evidence note (this file)

`docs/reliability/scenarios.json` is regenerated from
`docs/reliability/acceptance-matrix.json`. Every ID in
`docs/reliability/required-cases.json` is classified. Isolated execution is
still `scripts/reliability-container.sh --all`.

Open PRs inspected, not merged, not duplicated:

- #51 (`fix/s7-exact-provenance-lfs-ci`) — provenance/LFS CI only; this branch
  does not edit `scripts/test-s7-provenance.sh`, `scripts/ghe-live-validation.sh`,
  or `scripts/large-file-validation.sh`
- #44 — GitHub 404; no separate open PR with that number

Monorepo epic #52 is not absorbed.

## EVIDENCE

Host catalog (this agent environment, rustc 1.99.0, svn 1.14.3):

```text
python3 scripts/reliability-acceptance-matrix.py --self-test
# SELF-TEST: PASS (R09 omission + empty-PASS fixtures)

python3 scripts/reliability-acceptance-matrix.py --check --report
# CHECK: PASS; candidate rollup PASS 0 / FAIL 0 / PARTIAL 16 / NOT RUN 8

cp docs/reliability/fixtures/Cargo.lock Cargo.lock
cargo test --workspace --locked --lib -- --test-threads=1
# reposync-core lib: 224 passed; 0 failed; 0 ignored
# reposync-personal lib: 21 passed; 0 failed; 0 ignored
# reposync-web lib: 0 passed (no unit tests in lib.rs)

cargo test -p reposync-core --locked --test startup_schema -- --exact --nocapture
# 1 passed; 0 failed; 0 ignored  (STARTUP_V12, user_version=12)

cargo test -p reposync-core --locked --lib db::import_operations:: -- --test-threads=1
# 1 passed; 0 failed; 223 filtered out
```

Isolated Docker suite and matched-lock compare: **NOT RUN** (Docker daemon
unavailable). Full `cargo test --workspace` integration/E2E binaries: **NOT RUN**
here (old-generator/Chrome/LFS image not packaged). No production endpoints or
credentials.

CI on draft PR #76 must execute the new matrix check plus existing
format/clippy/workspace tests and the isolated reliability workflow.

## #62 acceptance (candidate)

| ID | Status |
| --- | --- |
| R01 | PARTIAL |
| R02 | PARTIAL |
| R03 | PARTIAL |
| R04 | PARTIAL |
| R05 | PARTIAL |
| R06 | NOT RUN (baseline FAIL reproduction retained) |
| R07 | NOT RUN |
| R08 | NOT RUN (baseline PARTIAL Git-first admission retained) |
| R09 | PARTIAL (baseline FAIL reproduction retained) |
| R10 | PARTIAL |
| R11 | NOT RUN |
| R12 | PARTIAL |
| R13 | NOT RUN |
| R14 | NOT RUN |
| R15 | PARTIAL |
| R16 | PARTIAL |
| R17 | PARTIAL |
| R18 | PARTIAL |
| R19 | PARTIAL |
| R20 | NOT RUN |
| R21 | PARTIAL |
| R22 | PARTIAL |
| R23 | PARTIAL |
| R24 | NOT RUN |

The deployed version remains **NOT ESTABLISHED**. F06 (watermark advance after
failed nonempty SVN apply) stays a P0 release blocker under #62/#63; the
bounded apply-stop tests exist and do not close that original defect class
for all paths.

## OPEN

- #63 implementation: atomic `user_version` wrapper and exclusive owner;
  no v13/v14 activation
- #64-C: explicit Git→SVN lost-reply recovery; no general job platform
- #66 follow-up: durable pair quarantine + personal-mode coverage;
  no automatic rebase
- #65/#67/#68/#69/#70/#71 and #41 live qualification
- #73 ignored conflict fixture
- Isolated CI evidence for this exact head, once Docker/CI run

No goal deviations. Original issue acceptance criteria are unchanged.

## REQUESTED NEXT ACTION

Independent review of this draft PR. If the contracts are accepted, implement
#63 atomic-migration-wrapper first, then #64-C, then the #66 durable-block /
personal-mode slice. Do not merge to main from this agent run unless CI is
green, an independent different-family review is posted, and hygiene matches
repo norms. Prefer PM merge.
