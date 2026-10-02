# #69 slice: coordinated pair-refresh preview

This is the smallest product slice of issue #69. It does **not** close #69 or #61.
Update-pair-from-parent **execution** and **re-anchor/recreate** are not in this PR.

## Operation table

| Operation | This PR | External writes | Checkpoint / #64 job | Result |
| --- | --- | --- | --- | --- |
| Update pair from parent (preview) | SUPPORTED, read-only | None | None | Pins the inputs below and returns a plan digest |
| Update pair from parent (execute / publish) | NOT IMPLEMENTED | Refused | Not started | `refresh_execute_not_implemented` |
| Re-anchor / recreate pair | NOT IMPLEMENTED | Refused | Not started | `reanchor_not_implemented` |
| Conflict resolution | NOT IMPLEMENTED | None | None | Conflicts are reported only |
| Force-push auto-rebase | UNSUPPORTED | Refused | None | Not a mode of this API |
| Rewrite of committed SVN revisions | UNSUPPORTED | Refused | None | SVN history stays append-only |

Preview may update the bridge-local inspection refs
`refs/reposync/pair-refresh/parent` and `refs/reposync/pair-refresh/pair`.
Those refs are not checkpoints, not published branches, and not remote updates.
The fetch uses `--no-tags --no-write-fetch-head` and does not checkout or reset.

## Default: Update pair from parent

`POST /api/repos/{pair_id}/refresh` with `operation=update_pair_from_parent`
(the default) and `execute=false` (the default) previews a history-preserving
refresh of an existing child pair against its parent.

Preserved history:

- Published Git commits stay. The preview does not reset, rebase, or force-push.
- Committed SVN revisions stay. The preview does not edit, delete, or replace them.
- Git and SVN graphs do not have to be identical. Correspondence is the verified
  mapping plus ancestry, not an equal tree or a similar commit message.

Provenance:

- A pair-scoped applied `sync_records` row, or a completed import's confirmed
  SHA and revision (#67 / #68), is a verified mapping. `commit_map` messages,
  trailers, and the `last_git_sha` column are not proof.
- The pair's own verified mapping wins. If the pair has none, a parent mapping
  that the pair tip still descends from is an inherited baseline
  (`inherited_parent_mapping`). Nothing is written back.
- If the pair tip does not descend from that mapping, the lineage is rewritten
  under #66. The replacement commits are **not** counted as new work, even when
  the diff looks the same. The plan sets `rewritten=true` and does not guess.

Merge and echo, for a later execution slice (not done here):

- Parent commits already contained in the pair tip are inherited. They must not
  be replayed into SVN again.
- Pair commits already named by a verified mapping are not new work.
- A later update would add a new Git reconciliation commit and a normal SVN
  merge revision (mergeinfo on that new revision). That is new history on both
  sides, not a rewrite of old revisions.
- Equal trees, patch-ids, or commit messages are not echo proof.

Unsynced work on **both** sides is part of the report: pair Git commits after
the mapping that are not already on the parent tip, parent Git commits not
already on the pair tip, SVN revisions after each mapped revision, and local
bridge commits that are ahead of or diverged from the fetched tip. The preview
never says that work was discarded. `discards_unsynced_work` is false.
`both_advanced` is a reported conflict when both sides have countable unsynced
work. File, binary, tree, and SVN mergeinfo conflicts are not resolved here.

The plan digest is the SHA-256 of a canonical JSON identity: operation, policy
`pair_refresh_preview_v1`, pair and parent ids, compatibility pair generation,
branches, fetched and local tips, SVN UUID, paths, revisions, baseline SHAs and
revisions, pending Git SHAs, pending SVN counts, local-ahead SHAs, rewrite
flags, and conflict codes. Notes and prose are not part of the digest. The same
inputs produce the same digest. A moved tip produces a different digest. This
slice does not accept an approval that executes; `approval.eligible` stays false.

Pair generation is the pre-#63 compatibility value `1`
(`compatibility_single_registration`). #63 generation tables are not activated.
This pin is the single existing registration. It is not a new generation and
not a re-anchor.

## Re-anchor / recreate — NOT IMPLEMENTED

`operation` of `reanchor`, `re-anchor`, or `recreate` returns
`reanchor_not_implemented` and does not fetch, copy, reset, or start a job.
Re-anchor is a separate explicit mode. It is not an alias for update-from-parent,
reset, force-push, or deleting the old SVN path. A future design would keep the
old generation's refs and path lineage, start from an SVN-origin baseline, and
preserve displaced unsynced work. That evidence gate is not met here.

`execute=true` or `dry_run=false` on the default operation returns
`refresh_execute_not_implemented`. No durable operation row is inserted.

## How this reuses earlier issues

| Issue | Reuse in this slice |
| --- | --- |
| #64 | Execution, when it exists, must be an exclusive durable operation with intent recorded before each external effect. This preview starts no import, SVN-commit, or refresh job and does not claim crash recovery. |
| #66 | Rewritten or unproven lineage is reported and blocked from being treated as replayable new work. Inspection refs are allowed. Checkout reset, checkpoint adoption, and remote writes are not. |
| #67 | Verified SVN-import mappings and `git merge-base --is-ancestor` (exit 0 / 1 / other) are the admission evidence. An existing path is not treated as equivalent by guesswork. |
| #68 | A snapshot import's confirmed SHA and revision is a valid baseline even when earlier SVN revisions were never imported. |

No Git-style rewrite of committed SVN revisions is authorized. No arbitrary
force-push auto-rebase is authorized.

## API

`POST /api/repos/{pair_id}/refresh`

| Field | Default | Effect |
| --- | --- | --- |
| `operation` | `update_pair_from_parent` | Preview that operation. `reanchor` / `recreate` are refused. |
| `execute` | `false` | `true` is refused. |
| `dry_run` | preview | `false` is refused as an execute attempt. |

The repository must already be a child pair (`parent_id` set). A root repository
returns `not_a_branch_pair`.

## Still later

- Executing the update, including revalidation of the digest and a #64 job
- Conflict resolution UI
- Re-anchor generation, retirement, and same-name path reuse
- Restart/reconciliation across Git, SVN, and SQLite
- Closing #69 / #61

## Tests

Named cases: `R13_PREVIEW_PINS_INPUTS`, `R13_PENDING_BOTH_SIDES`,
`R13_EXECUTE_REFUSED`, `R13_REANCHOR_NOT_IMPLEMENTED`,
`R13_REWRITTEN_LINEAGE`, `R13_DIGEST_BINDS_INPUTS`.

R13 is **PARTIAL**. R14 stays **NOT RUN**.
