# #67 slice: late-pair admission and preview

This is the smallest product slice of issue #67. It does **not** close #67 or #61.

## What shipped

`POST /api/repos/{id}/branches` now admits an SVN-derived Git development branch
at **preview time** and refuses unsafe pairing before any SVN copy, Git remote
create, checkpoint/watermark write, child registration, or scheduler
activation.

| Request | Default | Effect |
| --- | --- | --- |
| `dry_run` / `preview` | `true` | Return a pinned plan. No publish. |
| `skip_import` | `false` | Historical "start from now". Refused unless `compatibility_skip_import` is also set, and even then watermarks are **not** applied. |
| `auto_create_svn_branch` / `auto_create_git_branch` | ignored | This slice does not create remote refs. |

Admission:

1. Load verified SVN→Git mappings for the parent from scoped applied
   `sync_records` and a completed import's confirmed SHA/revision (snapshot or
   full). Commit messages, trailers, and unscoped filenames are not proof.
2. Resolve the candidate Git tip from the parent's local clone
   (`{data_dir}/repos/{parent}/git-repo`) by fetching into
   `refs/reposync/late-pair/inspect` without checkout or reset.
3. Prove the tip **descends from** a mapping SHA with `git merge-base
   --is-ancestor`. A Git-created feature ref is allowed when that ancestry
   holds. An unrelated Git-first or orphan root is refused
   (`unrelated_git_first`) before any mutation.
4. Snapshot-derived parents work with their **bounded** history: the snapshot
   baseline commit is a valid mapping even when earlier SVN revisions were
   never imported.
5. The preview pins Git tip, SVN source revision (the verified baseline),
   parent/pair identity, and `policy_version=late_pair_admission_v1`. It reports
   inherited mappings, pending Git count/summary, pending SVN work when the
   parent HEAD revision is knowable, proposed SVN copy source for a **new**
   target (the baseline revision, not live parent HEAD), conflicts/unknowns,
   and `pair_state=preparing`.
6. An already-existing SVN target is probed read-only. Existence is **not**
   equivalence. The plan sets `existing_svn_target.equivalent=false` and does
   not copy or treat `E160020 already exists` as success.
7. `dry_run=false` is refused with `publish_not_implemented`. A failed or
   partial setup cannot become scheduler-active because no child row is
   inserted.

R06 and R08 stay **PARTIAL**. Replay of missing Git commits, conflict
resolution, and default publish of a reconciled pair remain later.

## Still later

- Full replay of pending Git commits into SVN / bidirectional reconcile
- Conflict resolution UI for overlapping edits
- #69 pair refresh / re-anchor
- Making the pair scheduler-active after verified replay
- Closing #67 / #61

## Tests

Named cases: `R08_REFUSE_GIT_FIRST`, `R06_SVN_DERIVED_PREVIEW`,
`R06_SNAPSHOT_BASELINE`, `R06_SKIP_IMPORT_BLOCKED`,
`R06_EXISTING_TARGET_NOT_EQUIVALENT`, `R06_NO_ACTIVE_ON_PARTIAL`. Core unit
tests cover Git-first refusal, SVN-derived admission, skip_import containment,
snapshot-root baselines, and commit-message-is-not-proof.
