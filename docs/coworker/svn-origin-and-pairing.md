# The SVN-origin rule, and creating or late-pairing a branch

*Back to the [guide index](README.md).*

## The rule

Every history RepoSync manages must descend from a **verified SVN origin**:
Git commits that RepoSync itself recorded as the Git form of a specific SVN
revision. Working mainly in Git does not change this. A branch name, a matching
file tree, or a commit message that says "from r1200" is not proof.

At the pinned commit, admission of a late pair requires all of these. Any
failure is a refusal before anything is copied, written or checkpointed:

- a **verified mapping** exists for the parent (an applied Git-SHA ↔ SVN-revision pairing recorded by RepoSync for that repository);
- your branch tip **descends from** one of those mapped commits (an actual Git ancestor, checked with the equivalent of `git merge-base --is-ancestor`);
- the Git tree at that mapped commit and the SVN export at the mapped revision have **identical regular-file bytes and executable bits**.

Sources: [`evaluate_admission`][admission], [#67 admission contract][c67].

## A valid example

SVN trunk was imported into the Git mirror. RepoSync recorded that Git commit
`d3f5313…` is SVN r1200. You branch from that exact commit and add work:

```bash
# MAPPED is a Git SHA that RepoSync recorded for an SVN revision (see below).
MAPPED=d3f53131a08df3b64015d5e623bd4c9f0d20696a
git switch -c feature/widget "$MAPPED"
# …commit your work…
git merge-base --is-ancestor "$MAPPED" HEAD; echo "exit=$?"    # exit=0
git push -u origin feature/widget
```

`exit=0` means "the mapped SVN-derived commit is an ancestor of the branch tip".
That is the shape RepoSync accepts. (This exit-code behavior was run once in a
scratch repository for this guide.)

You can find candidate mapped SHAs from RepoSync itself. The commit-map listing
is a display aid, not proof; admission re-derives its own verified mappings:

```bash
curl --fail-with-body -sS -H @"$HDR" "$BASE/api/commit-map?repo_id=REPO_ID&limit=20"
# => {"entries":[{"svn_rev":1200,"git_sha":"d3f5…","direction":"…",…}],"total":…}
```

Source: [commit-map listing][commit-map].

## An invalid example

A coworker creates a fresh repository with `git init`, adds a README and files
that happen to match the SVN tree, names the branch `feature/widget`, and pushes.
The content matches; the history does not. Its root commit is unrelated to the
SVN import.

```bash
# In that unrelated clone:
git merge-base --is-ancestor "$MAPPED" HEAD; echo "exit=$?"
#   Two outcomes were seen when this was run in a scratch repository:
#   exit=1    the object exists locally but is not an ancestor
#   exit=128  "Not a valid commit name": the mapped commit is not even present
```

RepoSync refuses with `unrelated_git_first` ("Git tip does not descend from a
verified SVN-import mapping"), or `missing_git_objects` if it cannot resolve
your tip at all. Do **not** respond by editing messages, copying files, or
asking for the pairing to be forced: re-create the branch from the mapped commit
and re-apply your changes onto it (for example with `git cherry-pick`).

Exit codes matter: **0** is the only "yes"; **1** is a genuine "no"; anything
else (such as 128) means the question could not be answered, which is not a
finding that history was rewritten.

## Creating an SVN-derived branch

1. Start from a mapped commit on the paired parent branch (the valid example above).
2. Push the new branch as an ordinary new ref.
3. Decide whether it should be **paired** with an SVN branch now or later. An
   admin makes that request (below). Until then RepoSync does not sync it.

## Late-pairing an existing Git branch

*Late pairing* connects a Git branch you already published to an SVN branch
afterwards. RepoSync must account for the commits you made before pairing
exactly once, not skip them and not duplicate them.

**Who and what.** Late pairing needs an **administrator** session for the
*parent* repository. The parent must be enabled, not in an active/unresolved
import, and not being removed. Branch names are limited to 200 characters of
letters, digits and `. _ - /`, with no `..`, `//`, leading `-`, or leading or
trailing `/`; for Git, not `HEAD`, not ending in `.lock`, and not starting with
`refs/`. The nesting depth is capped at four levels.

Source: [`create_branch_pair`][create-pair].

### Step 1: preview (read-only)

There are two protocols. Use the **reviewed (v2)** one by sending
`"reconciliation": true`. `preview` captures both sides and returns what would
happen without writing anything.

```json
{
  "svn_branch": "branches/widget",
  "git_branch": "feature/widget",
  "reconciliation": true,
  "action": "preview"
}
```

Save that JSON as `preview.json`, then:

```bash
curl --fail-with-body -sS -X POST -H @"$HDR" -H 'Content-Type: application/json' \
  --data-binary @preview.json "$BASE/api/repos/PARENT_REPO_ID/branches"
```

Read the reply before doing anything else. These are the fields that matter:

| Field | What it tells you |
| --- | --- |
| `admitted` | `true` only if no conflicts were found. `false` means a blocked preview; nothing can be published from it. |
| `verified_baseline` | The mapped Git SHA and SVN revision your branch descends from. |
| `pending_git` | Your commits since the baseline that will be replayed (count and list). At most **32** are admitted per capture; more is a refusal, not truncation. |
| `pending_svn` / `existing_svn_target` | Whether the SVN branch already exists. An existing target is **never** assumed equivalent; its independent work is captured and reconciled against yours. |
| `conflicts` | Overlapping text, binary changes, delete-versus-modify and incompatible mode changes block the preview. |
| `unknowns` | Things RepoSync could not establish. Read these; they are not zero. |
| `review_pin` | An opaque token that binds the exact inputs you reviewed. Publishing needs it. |
| `published`, `scheduler_active` | Both `false` in a preview. |

A refusal comes back as `reason: detail`. The reasons you will see most are
`missing_baseline`, `unrelated_git_first`, `missing_git_objects`,
`missing_git_ref`, `ancestry_command_failed`, and `unsafe_skip_import`. Only
regular files and canonical executable modes are admitted: symlinks, submodules,
special nodes and unverified Git LFS pointer changes are refused or held.

Sources: [reviewed view fields][lp-view], [#67 contract][c67].

### Step 2: publish, only on a reviewed result

> **In tree, not live-qualified.** The publish path exists in the source. This
> guide does not claim it has passed a live qualification, and it is not a
> recommendation to publish on a live installation. Your administrator decides
> that; see [Held and unavailable](held-and-unavailable.md).

Publishing is a separate explicit request. It recaptures everything; if the
inputs changed since your preview, the pin no longer matches and it refuses
before doing any work.

```json
{
  "svn_branch": "branches/widget",
  "git_branch": "feature/widget",
  "reconciliation": true,
  "action": "publish",
  "dry_run": false,
  "preview": false,
  "review_pin": "PIN_FROM_THE_PREVIEW"
}
```

A reply with HTTP success does **not** by itself mean the pair is active. Check
`published` and `scheduler_active`, and `pair_state`. If something goes wrong
partway, RepoSync keeps a durable operation and says so (an `action_error` plus
a held state) instead of inventing a result.

### Step 3: watch it, resume it, or cancel it

Use the same endpoint with `"action": "status"`, `"resume"`, or `"cancel"`, and
the `operation_id` from the publish reply. `status` may omit `operation_id` to
show the latest one. These act only on an operation **you** (the same actor)
started, for the same branch names. `resume` also needs the same `review_pin`.
`cancel` stops an unstarted claim, or records a request while keeping any
effect that may already have happened and its hold.

### What not to use

- **`skip_import` / "start from now".** It would mark your existing commits as
  already synchronized without replaying them. It is refused as `unsafe_skip_import`
  unless an explicit compatibility flag is also set, and even then the checkpoints
  are not moved to the current tips.
- **`auto_create_svn_branch` / `auto_create_git_branch`.** Ignored in preview. Do
  not rely on them; create your Git branch yourself and let the reviewed publish
  decide the SVN copy source.
- **The older (v1) protocol**, used when `reconciliation` is omitted. Its preview
  is read-only and still useful, but a v1 `dry_run: false` attempts a
  publication through a different, older path. Prefer the reviewed protocol for
  anything beyond looking.

## Safe next steps

- Not sure your branch qualifies? Run the ancestry check above and ask an
  administrator for a **preview**. A preview changes nothing.
- Preview is blocked? Read `conflicts` and `unknowns`; fix your branch's content
  or ask the owner of the SVN branch about its independent work. Don't edit SVN directly to "make room".
- Publish reported a held or uncertain state? Go to
  [Status and reconciliation](status-and-reconciliation.md). Don't retry by
  issuing the request again with a new pin.

[admission]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/late_pair.rs#L388-L556
[c67]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/67-late-pair-admission.md
[commit-map]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/sync_history.rs#L32-L51
[create-pair]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L3125-L3199
[lp-view]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L3638-L3668
