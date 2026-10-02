# READY_FOR_REVIEW — #68 smallest team snapshot-init slice

```text
STATUS: READY_FOR_REVIEW
EPIC / ISSUES: Refs #68 (this slice); Refs #61 (epic). Do not close either.
BRANCH / PR URL: cursor/68-snapshot-init-e043 (draft PR to main)
REVIEW BASE SHA: 43dda452dc38bc97991cd0830018d17bdbb31902
PREVIOUS REVIEWED HEAD: 99cc4c801c9c72f5953831776777942a9acbdc84 (#65 / PR #80)
CURRENT HEAD SHA: a38570e424f4e5c7ed74b443436faa36380656d4
REMOTE HEAD MATCH / WORKTREE STATUS: ordinary commits on cursor/68-snapshot-init-e043
GOAL FILE / SHA-256: docs/reliability/GOAL.md
  16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8
```

## DELIVERED

Smallest #68 product slice: team/onboarding snapshot-at-fixed-revision initialization.

- Explicit `import_mode=full|snapshot` on `POST /api/repos/{id}/import` (query or JSON). Omitted mode stays full-history.
- Shared engine in `crates/core/src/snapshot.rs` reused by personal `import_snapshot`.
- Pin once: UUID, canonical URL, operative/peg revision (+ copy ancestry when present).
- Materialize selected tree → baseline Git commit → projected-content verify → #64 publish → verified mapping.
- Status/API show starting revision / history boundary; `earlier_history_imported=false` for snapshot.
- Repo stays initializing until publication + mapping. Mismatched non-empty Git targets and invalid revisions refuse without overwrite.

## OPEN / PARTIAL

R11 and R12 are PARTIAL. UI wizard, #67 late-pair, baseline reuse, live acceptance, and closing #68/#61 remain later.

## EVIDENCE

Local rustc 1.99.0 / svn 1.14.3:

```text
cargo fmt --all -- --check
cargo clippy -p reposync-core -p reposync-personal -p reposync-web --locked -- -D warnings
python3 scripts/reliability-acceptance-matrix.py --self-test
python3 scripts/reliability-acceptance-matrix.py --check --report
cargo test -p reposync-core --locked --lib -- snapshot snapshot_pin exact_identity
# 5 passed
cargo test -p reposync-web --locked --test server_freeze candidate_r1 -- --test-threads=1
# 5 passed (R11_SNAPSHOT_FIXED_R, R11_PIN_HOLDS_AFTER_ADVANCE,
#           R11_MISMATCHED_TARGET, R11_INVALID_REV, R12_FULL_DEFAULT)
```

CI on the draft PR must re-run these at the published head.
