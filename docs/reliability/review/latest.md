# READY_FOR_REVIEW — #69 pair-refresh preview slice

```text
STATUS: READY_FOR_REVIEW
EPIC / ISSUES: Refs #69 (this slice); Refs #61 (epic). Do not close either.
BRANCH: cursor/69-pair-refresh-preview-c31e
REVIEW BASE SHA: b501d3e2bee97d041a56834adf3bfd23c5b363f0
GOAL FILE / SHA-256: docs/reliability/GOAL.md
  16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8
```

The exact CURRENT HEAD SHA is the draft PR tip. This note is part of that tip, so it does not embed a self-hash.

## DELIVERED

Smallest independently mergeable #69 slice: ADR, operation table, and a read-only **update pair from parent** preview. Execution and re-anchor are not in this PR.

- `docs/reliability/contracts/69-pair-refresh.md` defines the default history-preserving refresh, merge/echo treatment, and the separate re-anchor mode.
- `POST /api/repos/{pair_id}/refresh` pins Git tips, SVN UUID/paths/revisions, `pair_refresh_preview_v1`, and compatibility generation `1`. The plan digest binds those inputs. Unsynced work on both sides is reported. `discards_unsynced_work` is false.
- `execute=true` / `dry_run=false` returns `refresh_execute_not_implemented`. No durable #64 job is started. No external Git or SVN write and no checkpoint mutation.
- `reanchor` / `recreate` returns `reanchor_not_implemented`. The UI shows that refusal. Re-anchor is not an alias for reset or force-push.
- Rewritten lineage (#66) is not counted as new work. Verified mappings reuse #67/#68. Snapshot ancestry is enough for an inherited baseline.

R13 is **PARTIAL**. R14 stays **NOT RUN**. This does not close #69 or #61.

## OPEN / PARTIAL

- Update-pair execution, digest revalidation, and the #64 durable job
- Conflict resolution beyond reporting `both_advanced`
- Re-anchor generation and retirement
- Crash recovery across Git, SVN, and SQLite (R14)
- Closing #69 / #61

## EVIDENCE

`docs/reliability/GOAL.md` remains `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.

Named cases: `R13_PREVIEW_PINS_INPUTS`, `R13_PENDING_BOTH_SIDES`, `R13_EXECUTE_REFUSED`, `R13_REANCHOR_NOT_IMPLEMENTED`, `R13_REWRITTEN_LINEAGE`, `R13_DIGEST_BINDS_INPUTS`.

Leave this PR **draft**. Do not merge. Do not undraft. Keep #69 and #61 open. Leave #51 and #52 alone.
