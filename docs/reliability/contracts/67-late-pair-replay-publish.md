# #67 slice: late-pair publish and baseline replay

This slice follows [67-late-pair-admission.md](67-late-pair-admission.md). It does **not** close #67 or #61.

## What shipped

`POST /api/repos/{id}/branches` with `dry_run=false` / `preview=false` on an admitted plan:

1. Refuses publish when the SVN target already exists (`existing_svn_target_blocks_publish`), except when resuming an in-flight `late_pair_publish` journal for the same fingerprint (journaled `svn_copy_pending` intent verified via copyfrom path/rev, or later phases).
2. Persists `svn_copy_pending` (target branch + copy-from path/rev) **before** `svn copy`, then copies a **new** SVN branch from the verified baseline revision (`proposed_svn_copy_source_revision`), not parent HEAD.
3. Inserts a child repository with baseline Git/SVN watermarks (not the feature tip), `enabled=false` until replay completes.
4. Replays pending Git commits through the production `SyncEngine` Git→SVN path.
5. Validates Git reachability (credential chain + `git ls-remote` for HTTP(S) remotes) **before** any SVN copy.
6. Clones the child Git workdir from the parent’s derived remote URL with credentials applied like normal sync (never uses the token as a URL host).
7. Records a durable `late_pair_publish` operation (`late_pair_publish_v1` journal) with resumable phases through `svn_copy_pending` / `svn_copied` / `child_registered` / `replay_in_progress` (replay errors stay `replay_in_progress` with `outcome_detail`, not terminal `failed`).
8. Enables the child (`scheduler_active=true` in the response plan) only after the pinned Git tip is handled.

Policy identity for published plans: `late_pair_publish_v1`.

## Still later

- Dual-side reconcile for an existing SVN target (R07)
- Overlapping/binary conflict stop
- Cancel/restart after partial replay with live engine recovery proofs beyond operation phase resume
- Parent SVN-ahead retention into Git
- Closing #67 / #61

## Tests

Named cases: `R06_LATE_PAIR_PUBLISH_REPLAY`, `R06_LATE_PAIR_PUBLISH_HTTPS_CREDENTIALS_REFUSE`, `R06_NO_ACTIVE_ON_PARTIAL` (foreign existing target blocks publish). Core integration tests cover publish replay and foreign-target refusal; HTTPS credential refusal uses the same preflight gate as publish before SVN mutation.
