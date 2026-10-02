# READY_FOR_REVIEW — #68 snapshot-init findings fix

```text
STATUS: READY_FOR_REVIEW
EPIC / ISSUES: Refs #68 (this slice); Refs #61 (epic). Do not close either.
BRANCH / PR URL: cursor/68-snapshot-init-e043 (draft PR #81 to main)
REVIEW BASE SHA: 43dda452dc38bc97991cd0830018d17bdbb31902
PREVIOUS REVIEWED HEAD: f25468a254082a03c516c9c6ea9cbedef2254e4c
CURRENT HEAD SHA: a2d3ee4095e815c50bd9a9f8ff50a8cc245c3435
HANDOFF: this commit's parent is the findings implementation above; branch tip is this handoff commit.
REMOTE HEAD MATCH / WORKTREE STATUS: ordinary commits on cursor/68-snapshot-init-e043
GOAL FILE / SHA-256: docs/reliability/GOAL.md
  16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8
```

## DELIVERED

Findings fix on the #68 snapshot slice (draft PR #81). Happy-path behavior is unchanged.

- Snapshot import shares full-import LFS preflight and `git lfs install --local`. If `lfs_threshold_mb > 0` and git-lfs is missing or install fails, the snapshot fails before a baseline commit. It does not publish a fat blob or a mismatched pointer. LFS-tracked paths must be pointers whose oid and size match the projected bytes.
- `snapshot_import` can finish from `ReconciliationRequired` through the existing reconcile API. Completion still requires the pinned revision, a single baseline, and the exact remote SHA. A missing pin, drifted revision, widened total, SHA mismatch, fingerprint change, or checkpoint stays held. No new remote delete or force-push overwrite.
- Snapshot first publish `force: true` is empty-lease first-ref creation, gated by the existing empty-target check.
- The snapshot worker no longer emits WebSocket `phase: completed` before finalization.

R11 and R12 stay PARTIAL. UI, #67, baseline reuse, live acceptance, and closing #68/#61 remain later.

## OPEN / PARTIAL

R11 and R12 are PARTIAL. UI wizard, #67 late-pair, baseline reuse, live acceptance, and closing #68/#61 remain later.

## EVIDENCE

Local rustc 1.99.0 / git-lfs 3.7.1 / svn 1.14.3:

```text
cargo fmt --all -- --check
cargo clippy -p reposync-core -p reposync-personal -p reposync-web --locked -- -D warnings
python3 scripts/reliability-acceptance-matrix.py --self-test
python3 scripts/reliability-acceptance-matrix.py --check --report
cargo test -p reposync-core --locked --lib snapshot
# 6 passed, including snapshot reconcile honesty and the LFS pointer unit check
cargo test -p reposync-personal --locked --lib
# 21 passed
cargo test -p reposync-web --locked --test server_freeze candidate_r1 -- --test-threads=2
# 7 passed (R11 fixed rev, pin holds, mismatched target, invalid rev,
#           snapshot LFS pointer, snapshot reconcile, R12 full default)
```

CI on the draft PR must re-run these at the published head.
