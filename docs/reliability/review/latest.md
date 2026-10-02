# READY_FOR_REVIEW — #67 late-pair admission/preview slice

```text
STATUS: READY_FOR_REVIEW
EPIC / ISSUES: Refs #67 (this slice); Refs #61 (epic). Do not close either.
BRANCH / PR URL: cursor/67-late-pair-admission-61e1 — https://github.com/chriscase/RepoSync/pull/82 (draft)
REVIEW BASE SHA: 779d8e2b76f92cc98adf9153d0a97f725918c5fe
PREVIOUS REVIEWED HEAD: none for this branch
CURRENT HEAD SHA: 5cddec9e6ce9dcdd1f806f73f62f3260b4c90601
REMOTE HEAD MATCH / WORKTREE STATUS: ordinary commits on cursor/67-late-pair-admission-61e1
GOAL FILE / SHA-256: docs/reliability/GOAL.md
  16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8
AGENT: bc-107871d8-17b1-5ed0-a9cf-4107850961e1
```

## DELIVERED

Smallest independently mergeable #67 product slice: SVN-origin admission and dry-run/preview for late pairing. Full replay/publish is not in this PR.

- `POST /api/repos/{id}/branches` defaults to `dry_run`/`preview`. It proves the Git tip descends from a verified SVN-import mapping (scoped applied records + completed import SHA plus `merge-base --is-ancestor`). Unrelated Git-first / orphan history is refused before SVN copy, checkpoint write, remote mutation, or child insert.
- A Git-created development ref that truly descends from verified SVN-derived parent history is admitted. Snapshot-bounded ancestry is enough.
- The preview pins Git tip, SVN source/target revisions (or unknowns), parent/pair identity, and `late_pair_admission_v1`. It reports inherited work, pending Git count/summary, pending SVN if knowable, proposed copy source for a new target, existing-target non-equivalence, and `pair_state=preparing`.
- Unsafe `skip_import` / start-from-now is refused (`unsafe_skip_import`). `compatibility_skip_import` can acknowledge the request in the plan but still does not apply watermarks. An existing SVN path is never treated as equivalent.
- `dry_run=false` returns `publish_not_implemented`. No scheduler-active child is created.

R06, R07, and R08 are PARTIAL. Replay, conflict UI, #69, closing #67/#61 remain later.

## OPEN / PARTIAL

R06/R07/R08 PARTIAL. Full Git→SVN replay, overlapping conflict resolution, and publish of a reconciled pair are later. R02/R09/R11/R12 remain PARTIAL as previously.

## EVIDENCE

Commands at this head (`cp -f docs/reliability/fixtures/Cargo.lock Cargo.lock` first; rustc 1.99.0 / cargo 1.99.0):

```text
sha256sum docs/reliability/GOAL.md
# 16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8

cargo test -p reposync-core --locked --lib late_pair
# ok. 7 passed; 0 failed; 0 ignored; 0 measured; 240 filtered out

cargo test -p reposync-web --locked --test server_freeze -- candidate_r0 --test-threads=2
# ok. 9 passed (6 late-pair + overlapping R02 names); 0 failed

cargo test -p reposync-web --locked --test server_freeze diagnostic_r02_r03_root_delete -- --test-threads=2
# ok. 1 passed (R02_R03_ROUTE refuses skip_import of unproven Git-first)

cargo clippy -p reposync-core -p reposync-web --locked -- -D warnings
# Finished `dev` profile (libs/bins only; --all-targets hits pre-existing team_mode_e2e lints)

python3 scripts/reliability-acceptance-matrix.py --self-test
# SELF-TEST: PASS

python3 scripts/reliability-acceptance-matrix.py --check --report
# CHECK: PASS; R06/R07/R08 PARTIAL; PASS rollup 0 / PARTIAL 20 / NOT RUN 4
```

API fixture note: `push_feature_commits` must checkout `origin/main` (snapshot SHA). Bare `git init` leaves unborn `master` as HEAD; `checkout -b feature` from that clone created an unrelated root and falsely failed admission.

Leave this PR **draft**. Do not merge. Do not undraft. Keep #67 and #61 open.
