# Limit which Git changes reach an SVN monorepo

On `epic/reposync-partial-52`, a team can limit which paths its **scheduled Git→SVN writes** may change. For example, the team that owns `trunk/team/` can publish changes under `team/` and leave `trunk/team-other/` alone. Each included path keeps its name relative to the paired branch.

**Can everyone with access to the Git destination read the entire selected SVN branch and the history you plan to import?** If the answer is no, stop before initial import. This receiver applies no inbound path filter during import or subsequent SVN→Git sync. A dedicated Git repository can still receive sibling teams' files.

| What you need | What this receiver supports |
| --- | --- |
| Limit writes back to SVN, keeping paths such as `team/readme.md` | Set the team repository policy below. |
| Give Git readers only `team/`, or split a Git monorepo into several repositories | Subset export and Git repository splitting are unavailable. |
| Put SVN's `trunk/team/readme.md` at Git's root as `readme.md` | The receiver has no `path_rewrite_rules` setting. It cannot remap paths, including LFS paths, or rewrite existing history. |
| Use the same policy with `reposync personal` | Personal mode supports branch sync. Its config has no subset-policy fields and its outbound engine uses empty path rules. [Personal configuration](../crates/core/src/personal_config.rs) · [Personal Git→SVN implementation](../crates/personal/src/git_to_svn.rs) |

## Configure a team repository

1. Choose the SVN branch and Git branch. For a disposable rehearsal, pair a fictional `trunk` with an empty Git `main` and use only files the Git audience may read. In **Repositories**, create the registration with **Enabled** off so the scheduler cannot pick it up before review. Both policy lists start unset.
2. As an administrator, open that repository, choose **Edit**, and find **Path policy** in its sync settings. For **Allowed path prefixes**, choose **Edit**, then **Add**, and enter `team/`. For **Blocked patterns**, add `*.exe`. The path is relative to `trunk`, so leave `trunk/` out of the prefix.
3. Paste these paths one per line into **Git→SVN preview**. The preview runs in the browser against the proposed values. Choose **Save**, then reopen the repository and check that the stored lists match.

   | Sample Git path | Expected outbound result |
   | --- | --- |
   | `team/ok.txt` | Included as `trunk/team/ok.txt` in SVN |
   | `team/skip.exe` | Excluded by `*.exe` |
   | `team-other/leak.txt` | Outside the `team/` prefix |

4. For a fresh registration, use the repository's import panel when it offers a start action. **Full SVN history** replays branch history. **Snapshot at pinned SVN revision** starts from the selected revision or HEAD. Both can bring sibling paths into Git. Check the import result and Git tree before turning on scheduled sync. For example, if `trunk/team-other/notes.txt` is present at the baseline, it can appear at `team-other/notes.txt` in Git. A later SVN edit to that sibling file can also reach Git.
5. With scheduled sync enabled in the rehearsal, make one Git commit changing all three sample paths. The Git commit keeps all three changes. Of those paths, only `trunk/team/ok.txt` should change in SVN. A later Git commit touching only `team/skip.exe` should create no SVN revision. An existing excluded SVN file retains its prior content.

The daemon loads these lists from the repository record. Neither the service TOML nor `personal.toml` provides these settings. `reposync validate` checks service configuration. **Avoid `reposync sync now` for a policy-controlled pair.** It constructs an engine from service configuration without loading the registration's path rules. [Scheduler](../crates/daemon/src/scheduler.rs) · [Service configuration](../crates/core/src/config.rs) · [CLI commands](../crates/cli/src/main.rs)

### Matching and changes of scope

An empty allow list admits every path before blocking. A nonempty allow list admits a path equal to a listed prefix or beneath it at a directory boundary. Matching is case-sensitive. For matching, backslashes become slashes, leading slashes are removed, and a trailing slash on a component prefix is optional.

Blocked patterns then veto matches. A plain entry such as `team/cache` blocks that path and its descendants. A leading star tests the suffix of the whole path (`*.exe`). Otherwise a trailing star tests the start of the whole path (`team/build-*`). A lone `*` blocks everything. These limited forms do not implement general shell globs or regular expressions.

Deletes use the same rules. A Git rename out of scope deletes only the old SVN path. A rename into scope adds only the new path. Two included endpoints produce a delete and an add. Two excluded endpoints produce no SVN write. [Engine matcher and rename handling](../crates/core/src/path_projection.rs)

Before changing scope, record the current lists and preview paths on both sides of the new boundary. Narrowing the scope leaves previously published files and commits in place. Widening it does not backfill previously skipped commits. Restoring the previous lists changes future writes only.

### Read or update the policy through the API

Read `GET /api/repos/{id}` and inspect `path_policy`. On this receiver, version 1 reports `enforced_direction: "git_to_svn"`, `svn_to_git_filtering: "not_applied"` and `path_rewriting: "none_read_only"`. Each list has a state of `unset`, `list` (with `entries`), or `malformed`.

An administrator can update the same registration with `PUT /api/repos/{id}`. The example is a request body. Supply a fresh request ID and copy `updated_at` from the detail read. Each write field is a **string containing a JSON array**.

```json
{
  "save_request_id": "<fresh UUID>",
  "expected_updated_at": "<updated_at from GET>",
  "allowed_paths": "[\"team/\"]",
  "blocked_patterns": "[\"*.exe\"]"
}
```

Omit a field to preserve it. Send JSON `null` to clear it. A stored empty list and an unset field both impose no restriction of that kind. Clearing the allow list can broaden writes to the entire branch. On a stale-edit conflict (HTTP 409), reread the registration and compare changes before preparing a new save. After an uncertain response, retry the same request ID and payload to recover the save. Reread the policy to confirm the current state. [Repository API](../crates/web/src/api/repos.rs) · [Policy validation and response fields](../crates/web/src/api/repo_path_policy_56.rs)

## Verify on disposable local repositories

From a disposable source checkout with the Rust build dependencies, Git and the Subversion CLI tools (`svn`, `svnadmin` and `svnlook`), run the two engine cases below. Each creates its own local bare Git repository, `file://` SVN repository and temporary database. They exercise the engine directly with explicit rules. The third case exercises the HTTP save and reread against a loopback server.

```sh
cargo test -p reposync-core --test team_mode_e2e \
  candidate_rsc02_mixed_commit_preserves_excluded_and_rejects_sibling_prefix -- --exact --nocapture
cargo test -p reposync-core --test team_mode_e2e \
  candidate_rsc02_filtered_then_allowed_cycle_leaves_no_blocked_residue -- --exact --nocapture
cargo test -p reposync-web --test repo_path_policy_56 \
  rs56_get_edit_put_get_preserves_replaces_and_explicitly_clears_both_fields -- --exact
```

Look for **one passing named test per command**. The first emits `RS_C02_MIXED_AND_SIBLING` and checks that the SVN journal contains only `team/ok.txt`. The second emits `RS_C02_FILTERED_THEN_ALLOWED` and checks that two allowed commits surrounding a fully blocked commit create exactly two SVN revisions. The third reads back replacements and explicit clears and checks that omitted fields survive. [Engine fixtures](../crates/core/tests/team_mode_e2e.rs) · [API fixture](../crates/web/tests/repo_path_policy_56.rs)

## Troubleshooting a rehearsal

- **Why did my Git push drop files on the SVN side?** Inspect the source commit with `git show --name-status -M <sha>` in the local Git clone. In the repository's audit log, or in `reposync --config <local-config.toml> audit --limit 30` on the fixture host, look for `path_violation_filtered` (some paths excluded) or `path_violation_skipped` (all excluded). Compare the SVN revision with `svnlook changed -r <revision> <local-svn-repository>`. A filter event explains path selection. Check the resulting SVN revision or sync record to confirm publication.
- **Why is my SVN file missing in Git?** Check the registered SVN branch, import completion, snapshot revision if applicable, and subsequent sync records. Verify the path on the destination branch with `git ls-tree -r --name-only main` in the local clone at the received revision. Changing `allowed_paths` cannot fix inbound delivery.
- **Why has this repository stopped syncing?** A malformed stored list makes the scheduler refuse that repository. Look for the field name and `refusing repository sync` in daemon logs. Use the editor's **Replace with a new list** or an intentional **Clear** and verify the reread. If the editor says the policy is unavailable, check which server build answered. If a conflict is holding the pair, inspect the source paths and revisions in **Conflicts** before resolving it. [Conflict workflow](conflict-resolution.md)
