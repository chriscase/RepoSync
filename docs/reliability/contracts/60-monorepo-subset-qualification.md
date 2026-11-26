# #60: Monorepo subset qualification

Run the separate native qualification on a local source checkout with Git,
Subversion CLI tools, Rust/Cargo and Python 3.11+:

```sh
bash tests/scripts/qualify-monorepo-subset.sh
```

The runner needs no live accounts. It starts disposable real SVN and Git
repositories, runs actual Team sync-engine cycles and the Team daemon HTTP
surface, and runs the real `reposync personal start --foreground` entry point.
The only simulated service is the Personal Git provider API on loopback; its
responses identify synthetic merged PRs backed by real Git commits. The
[three-team fixture](../../../tests/fixtures/monorepo/test_history.py)
independently checks every seed path and byte and refuses to reuse a seed
directory. This checks the data fixture, not RepoSync by itself.

## Evidence by scenario

| # | What proves the behavior using native fixtures |
| --- | --- |
| 1 | [SC01](../../../crates/core/tests/partial_sync_53.rs) uses exactly `team-a/` and `docs/`, commits to both and to `team-b/`, and checks the full Git tree plus an excluded-only SVN frontier advance without an inbound Git cursor advance. Only selected files are seeded in the unprojected baseline; this does not promise that an unprojected import hides existing siblings. |
| 2 | [SC02](../../../crates/core/tests/partial_sync_53.rs) checks no Git ref/commit effect, explicit canonical no-target checkpoint, exact single proven NULL map, audit event, SVN-only watermark progress and quiet reopen/repeat. The same target's `filtered_finalizer_write_fault_...` test vetoes the finalizer and proves atomic rollback and one later completion. |
| 3 | [SC03](../../../crates/core/tests/partial_sync_53.rs) checks selected bytes and deletes in a straddling revision, unchanged excluded files, and SVN revision provenance in the added Git commit. |
| 4 | [SC04](../../../crates/core/tests/partial_sync_53.rs) verifies one in-scope SVN write, untouched outside paths and a single success audit record naming the dropped Git path for that commit. |
| 5 | [SC05](../../../crates/core/tests/partial_sync_53.rs) verifies an excluded-only Git SHA advances the inbound Git cursor, produces a no-target receipt, writes no SVN revision and does no work when repeated. |
| 6 | [SC06](../../../crates/core/tests/partial_sync_53.rs) asserts the original path and exact bytes round trip SVN→Git→SVN, with no repeat publication. |
| 7 | [Team #57 driver](../../../scripts/team-path-rewrite-57.py) configures the real daemon through HTTP, imports a projected snapshot, publishes a mixed SVN revision and sends a Git edit back to the original SVN path. |
| 8 | [SC08](../../../crates/core/tests/partial_sync_53.rs) checks an in-scope SVN rename removes the old Git path, preserves exact new bytes and leaves the entire unrelated tree unchanged. |
| 9 | [Ordinary Team endpoints](../../../crates/core/tests/partial_sync_53.rs): moving an included SVN source out of scope deletes the included old Git path but does not add the excluded destination. [Rewritten Team driver](../../../scripts/team-path-rewrite-57.py): a cross-boundary SVN move holds without Git publication, checkpoint advance or canonical mapping mutation. These are separate current modes, not a choice about deleting previously excluded paths from Git. |
| 10 | [SC10](../../../crates/core/tests/partial_sync_53.rs) runs two idle cycles after an applied publication; both report zero work, with unchanged refs, SVN revision, Git count, map rows and both progress cursors. |
| 11 | [Personal daemon driver](../../../scripts/personal-rewrite-59.py) in `--poll-for-rs60` mode processes an excluded revision without Git ref change and with one NULL mapping, one audit event and a canonical no-target record; it gracefully stops/restarts and captures two quiet polls before verifying selected files under two prefixes and no excluded files. Existing #59 assertions verify eligible merged PR replay back to SVN and no out-of-scope work. Actual PR polling is proved; no PR creation is attempted. |
| 12 | `empty_policy_keeps_the_legacy_full_apply` plus the unchanged native `test_team_mode_*` cases (eight active; the pre-existing conflict fixture remains ignored), and the #59 unrestricted Personal import control. |

The earlier [#55 Personal filter driver](../../../scripts/personal-filter-55.py)
and [direct HTTPS fixture](../../../scripts/personal-filter-55-https.py)
cover the inbound-only mode and transport-specific holds. They complement,
but do not substitute for, the projected full-import and running-daemon
checks above. The native #60 receipt does not promote those transport
fixtures into a claim of projected network coverage.

## What remains outside this receipt

- The original acceptance requires the twelve scenarios to run against
  [Docker Compose](../../../tests/docker-compose.yml). The above is a native
  proof, not that run. No container, runtime access or networking settings
  were changed. To claim literal Compose acceptance, use a separately
  authorized disposable container fixture and map each assertion to that run,
  or explicitly accept the native suite as the replacement.
- Scenario 11 asks about "PR creation". Personal mode does not create PRs; it
  directly mirrors selected SVN changes and observes eligible PRs users have
  already merged. The fixture confirms no create-PR request. If the story
  still requires a new automatic PR feature, that behavior is not qualified.
- Scenario 9 does not authorize a different rule for pre-existing excluded
  Git files. These tests preserve the current endpoint behavior and the
  rewrite safety hold, and do not settle the separate #53 excluded-deletion
  decision.
