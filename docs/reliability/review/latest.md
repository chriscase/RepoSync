# READY_FOR_REVIEW — #67 late-pair admission/preview slice

```text
STATUS: READY_FOR_REVIEW
EPIC / ISSUES: Refs #67 (this slice); Refs #61 (epic). Do not close either.
BRANCH / PR URL: cursor/67-late-pair-admission-61e1 (draft PR to main)
REVIEW BASE SHA: 4f985a24d1fede55bde88b521800ae9e5db0f5a3
PREVIOUS REVIEWED HEAD: none for this branch
CURRENT HEAD SHA: (see git log / PR)
REMOTE HEAD MATCH / WORKTREE STATUS: ordinary commits on cursor/67-late-pair-admission-61e1
GOAL FILE / SHA-256: docs/reliability/GOAL.md
  16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8
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

See the PR body after local commands at this head. CI on the draft PR must re-run them.
