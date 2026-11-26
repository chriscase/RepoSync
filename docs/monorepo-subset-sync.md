# Mirror a subtree of an SVN monorepo

RepoSync can mirror an SVN team's directory to a dedicated Git repository and
send eligible Git changes back to the original SVN paths. The rules apply to one
paired SVN branch, not to the whole SVN server. This guide describes the
subset-capable build. Check the server and CLI versions before relying on it.

## Choose an import and a layout

| Need | Use |
| --- | --- |
| Git readers may see the entire SVN branch and its imported history; only future scheduled writes need filtering | **Team, filter only.** Keep branch-relative names. Full or snapshot imports can expose sibling paths; adding a filter does not undo that. |
| Several people use one mirror; only selected file content may appear on Git; one clean starting revision is enough | **Team, projected snapshot.** Use a fresh paused direct-root registration, an empty Git destination and one strip prefix. This is not a history replay. |
| An individual needs a Git PR workflow and history for just the selected paths | **Personal, projected full import.** Starts at the current SVN branch incarnation, with an optional strip prefix. It is not a rewrite of an existing Git repository. |
| Subdivide a source **Git** monorepo, combine several rewrites, or retrofit a historic Git repository | Not supported. Use a separate migration or repository layout. |

If Git readers must not see other teams' data, **do not run an unprojected
Team import**. If Team users also insist on keeping full SVN path names,
there is no separate filter-only Team import that guarantees privacy. Choose a
supported projected workflow or pair an SVN branch already limited to that
audience. Team projection is a file-content boundary, not a metadata
redaction boundary: selected Team changes may still carry the original SVN
revision message and mapped author into Git. If the audience must not see
that information, don't use Team projection as a confidentiality control.

## What the rules mean

- `allowed_paths` takes literal, case-sensitive file or directory prefixes in
  SVN branch-relative coordinates. `src/teams/foo/` matches that directory and
  its descendants, not `src/teams/foobar/`. It is not a glob or regex.
- After allowing a path, `blocked_patterns` can veto it. A plain entry blocks
  a file or component subtree. A leading `*` tests the suffix (`*.tmp`);
  otherwise a trailing `*` tests the beginning of the whole path
  (`src/teams/foo/generated-*`). A sole `*` blocks everything. Do not use
  general glob syntax such as `**`. With no allow list, blocking starts from
  all paths.
- With one strip rule, SVN→Git filters in SVN coordinates then strips the
  prefix; Git→SVN prepends the same prefix **before** filtering. Configure a
  nonempty allow list entirely within it. Enter canonical relative paths,
  without a server URL, leading slash, `.`, `..` or a `.git` component.
  In an unrewritten policy, backslashes in matches normalize to slashes,
  leading slashes are ignored, and the trailing slash on a literal prefix is
  optional.

## Set up a Team mirror

1. Use a fresh registration under **Repositories**, with **Enabled** off. For
   a projected mirror use a direct root pair (no parent), no automatic
   conflict merges, no LFS rewriting (threshold 0), and an empty Git
   destination branch. Use a separate disposable pair to try this first.
2. Open the repository, select **Edit → Path policy**, and open the collapsed
   section. Edit **Allowed path prefixes**, then **Blocked patterns**. For
   a projected snapshot, also choose **Edit strip prefix** and enter the
   common SVN-relative directory. For filter-only mode, leave rewrite unset.
3. Check both preview directions. Enter SVN paths for SVN→Git and Git paths for
   Git→SVN. Check an included path, a blocked path, and a similar sibling
   prefix; check that the resulting destination names are the ones you intend.
   The preview evaluates sample paths against the proposed rules. It does not
   predict whether a commit, copy, rename, or import can be applied.
4. Save, reopen **Path policy**, and verify the effective policy. If using a
   projected Team mirror, choose **Snapshot at pinned SVN revision**, confirm
   the intended revision, and wait for a completed matching snapshot. A full
   import, a nonempty target or a missing matching baseline is refused
   with a rewrite. In filter-only mode choose an import **only after**
   verifying everyone with access can read all the planned import data.
5. Verify the Git tree and the import status. Only then enable scheduled sync.
   Use the registered scheduler, not standalone `reposync sync now`: that
   command does not load this repository's policy.

### Alternative: Team settings API

The service TOML does **not** hold the Team policy; it lives on each repository
registration. An administrator can GET `/api/repos/{id}` and PUT the same
endpoint. This is a sample JSON request body, not TOML:

```json
{
  "save_request_id": "<fresh UUID>",
  "expected_updated_at": "<updated_at from GET>",
  "allowed_paths": "[\"src/teams/foo/\"]",
  "blocked_patterns": "[\"*.tmp\"]",
  "path_rewrite_rules": "[{\"strip_prefix\":\"src/teams/foo/\"}]"
}
```

These fields take strings containing JSON. Omit any field to keep it. To clear
either list, send JSON `null`; to clear the rewrite, send the string `"[]"`
(subject to the same new-paused-registration restrictions). After saving,
read `path_policy` and `path_rewrite_prefix`. A current receiver reports
`managed_incremental` and `single_strip_prefix_v1` capabilities. A malformed
read is not an empty list, and an older receiver may not filter inbound edits.
If a save returns HTTP 409 because the record changed, reread and compare
before a new save; if the response is lost, use the same request ID and exact
payload to recover the existing receipt.

## Set up a Personal mirror

1. Use a new data directory and an empty Git destination branch. Give the
   wizard the SVN URL for the **branch itself**, not the monorepo root.
   Run `reposync personal init --output ./subset-personal.toml` and choose
   the SVN-relative scope, optional strip prefix and optional block patterns.
   With an existing **new** Personal config instead of the wizard, add this to
   its `[options]` section (not the Team service config):

   ```toml
   projected_history = true
   allowed_paths = ["team/alpha/selected/"]
   blocked_patterns = ["team/alpha/selected/blocked.txt"]
   path_rewrite_rules = [{ strip_prefix = "team/alpha/" }]
   ```

   Omit the rewrite array to keep the original relative paths. A scoped legacy
   config without the opt-in is inbound-only; do not mistake it for this mode.
   Store credentials outside the file, in the configured environment variables.
   A local rehearsal can use `file://`. For network setups, use an explicit
   supported SVN transport profile and, for HTTPS, the admitted CA bundle.
   See [transport configuration](personal-branch/configuration.md#scoped-full-history-and-one-prefix-rewrite)
   and qualify your deployment. The worked round trip below uses `file://`;
   it does not qualify network behavior for the projected full-import workflow.

2. Run a full import and inspect the baseline. The CLI validates the
   Personal configuration when it loads the file:

   ```sh
   reposync personal --personal-config ./subset-personal.toml import --full
   reposync personal --personal-config ./subset-personal.toml status
   reposync personal --personal-config ./subset-personal.toml doctor
   ```

   A scoped Personal snapshot is refused. A copied SVN branch starts at its
   proved creation snapshot, not in the predecessor's history. An excluded-only
   version creates no Git commit; projected commits use a fixed revision-based
   message and mirror identity rather than exposing whole-revision SVN metadata.
3. Check Git contains only intended files and that status reports a verified
   projected baseline. Start the Personal worker:
   `reposync personal --personal-config ./subset-personal.toml start`.
   For Git→SVN, submit and merge an eligible PR into the paired branch. Don't
   use a direct push as a substitute: the Personal worker expects the admitted
   merged PR and an unambiguous first-parent transition from its checkpoint.
4. Check the next SVN poll and a merged PR by inspecting files and revisions
   on both sides. Use `status`, `doctor`, `log` and `pr-log` on that Personal
   config to inspect the checkpoint and outcomes. Check for divergence or a
   re-import requirement rather than assuming an accepted Git PR reached SVN.
   See [Personal configuration](personal-branch/configuration.md#scoped-full-history-and-one-prefix-rewrite).

## Worked example: Team Foo

The fictional company has these files at SVN r2. Pair its `trunk` with a
fresh dedicated Git `main` using the Team settings above.

```text
SVN trunk/                      Git main (projected snapshot r2)
└── src/teams/                  └── seed
    ├── foo/
    │   ├── seed      ──────────────► seed
    │   └── excluded.tmp        blocked by *.tmp
    └── foobar/
        └── seed                outside scope
```

| Step | SVN result | Git result |
| --- | --- | --- |
| Projected snapshot at r2 | No new SVN revision | Only `seed` |
| Commit SVN r3 adding `foo/forward` and `foobar/hidden` under `src/teams/` | Both SVN files exist | `forward` appears at Git root; no `hidden` |
| Commit and push a change to Git's `forward` in the direct Team pair | r4 updates `src/teams/foo/forward` only | `forward` contains the new edit |
| Copy the unrelated SVN sibling at r5, then edit the selected team at r6 | Sibling and selected SVN history remain intact | r5 makes no Git commit; r6 publishes the selected change |
| Move selected `forward` across the boundary at r7 | SVN source move exists and needs review | No Git publication or SVN checkpoint advance for r7 |

For a runnable disposable equivalent, use the existing test drivers from a
source checkout. Requires Python 3.11+ and `git`, `svn`, `svnadmin` and
`svnlook` on PATH. Both use local temporary repositories and loopback service
fixtures; neither contacts your configured workplace or GitHub repositories.

```sh
cargo build -p reposync-cli -p reposync-daemon -p reposync-personal
python3 scripts/team-path-rewrite-57.py target/debug/reposync-daemon
python3 scripts/personal-rewrite-59.py --cli target/debug/reposync \
  --personal target/debug/reposync-personal
cargo test -p reposync-web --test repo_path_policy_56
cargo test -p reposync-core --test partial_sync_53
```

The Team driver prints `"result": "pass"` after r2–r7 and logs its temporary
directory. The Personal driver prints `PASS` after historical and incremental
filtering, both reverse PR paths, and a no-write divergence hold. The API suite
checks real HTTP saves and rereads; the core suite checks filtering and
checkpoint evidence on disposable fixtures.

## Ongoing operations and troubleshooting

- **Why is my SVN file not appearing in Git?** Confirm the paired branch and
  selected revision with `svn log -v -r REV SVN_BRANCH_URL`. Preview the
  branch-relative SVN path, not `trunk/...` or a full URL. Inspect the local
  Git clone with `git ls-tree -r --name-only main`. A fully filtered revision
  can be recorded without a Git commit. Team audit can show
  `svn_to_git_filtered`. Missing/ambiguous diff evidence, certain property
  changes or applicable copy/rename provenance may hold an SVN revision.
- **Why did my Git push drop these files?** The files are not removed from the
  source Git commit. Inspect `git show --name-status -M SHA` and compare the
  mapped SVN path with the allow and block rules. For Team, inspect
  `reposync --config SERVICE_CONFIG audit --limit 30` for
  `path_violation_filtered` or `path_violation_skipped`. On a disposable
  filesystem SVN fixture compare
  `svnlook changed -r REV LOCAL_SVN_REPOSITORY`; verify the actual revision
  instead of treating an audit line as publication proof. For Personal,
  inspect `doctor` and `pr-log`: a direct push or a diverged history is
  not an admitted PR.
- **Files added outside the scope:** a successful scheduled Team filtering
  run need not publish them to the opposite side. They still exist on their
  source side, and on an unprojected initial import an SVN sibling can
  already exist in Git. On a projected Personal mirror don't introduce
  out-of-scope Git paths; the worker can hold for divergence.
- **Scope changes:** preserve the old mirror and record the current rules.
  Filter-only Team edits affect future work; they neither remove earlier
  publications nor replay skipped revisions. To obtain a different projected
  tree, use a fresh registration/destination and a new snapshot (Team) or new
  data directory/destination and full projected import (Personal). Never erase
  a preserved mirror or reset its cursor to clear a hold.
- **Conflicts and boundary moves:** Git→SVN Team projection evaluates rename
  endpoints separately: moving out can delete the formerly selected SVN path;
  moving in can add the newly selected path. A Team **SVN→Git** rewrite holds
  selected, cross-boundary or ambiguous SVN copy/move provenance, rather than
  applying half the move. This is not a promise to delete previously excluded
  SVN material from Git. When a conflict or copy is held, inspect the revisions
  and affected paths before deciding how to reconcile; do not skip it by
  resetting the frontier. A projected Personal mirror also holds simultaneous
  selected SVN and unhandled Git changes before writing either side.
  See [conflict resolution](conflict-resolution.md) and the
  [Team filtering contract](reliability/contracts/53-svn-to-git-filtering.md).
- **Unsupported kinds/configuration:** there is no LFS support for these
  projected workflows. Personal reverse writes of executable-mode or special
  nodes are refused. A malformed stored list can stop scheduled sync; check
  the daemon logs and use an intentional replace or clear rather than taking
  an unavailable editor to mean the policy is empty.

These examples qualify the stated paths, not every transport, copy/rename
pattern, migration, or broad end-to-end scenario in the parent epic.
