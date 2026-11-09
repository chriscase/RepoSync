# History rewrites and SVN merges

*Back to the [guide index](README.md).*

Two distinctions prevent most of the trouble people run into:

1. **Rebasing private, unpublished work is not the same as rewriting history that
   is already published on a paired branch.**
2. **A normal SVN merge appends a revision. Deleting and recreating a branch, or
   replacing a repository's history, is a different kind of event.**

## Private rebase versus rewriting published paired history

RepoSync remembers the last Git commit it handled (its checkpoint) by commit ID.
A rebase, an amend (even one that only changes the message) or a squash gives
the commits **new IDs**. If those commits were already on the paired remote
branch, RepoSync's recorded point is no longer an ancestor of the new tip, and it
cannot tell "these are the same changes again" from "these are new changes". It
stops and holds the pair; it does not guess.

### Is it published?

A commit is *published to a paired ref* once it is on the remote paired branch,
even briefly. RepoSync may already have replayed it to SVN. Check before you rewrite:

```bash
git fetch origin
# Commits only you have (safe to rewrite):
git log --oneline origin/feature/widget..HEAD
# Is a particular commit already on the remote paired branch? (0 = yes)
git merge-base --is-ancestor <sha> origin/feature/widget; echo "exit=$?"
```

As with every ancestry check in this guide: **0** is yes, **1** is no, and any
other exit code means the question was not answered.

### Which case am I in?

| Situation | Rebase / amend? | Why |
| --- | --- | --- |
| Commits exist only in your clone | **Yes.** | Nothing has seen them. |
| A personal branch that is **not** paired and that you have pushed | **Yes** (tell people who may have fetched it). | RepoSync is not tracking it. |
| A branch you are about to late-pair, and you have **not** yet asked for a preview/publish | **Yes**, before you ask. | Nothing is checkpointed yet. A pairing preview is pinned to your tip, so any change after a preview needs a new preview. See [pairing](svn-origin-and-pairing.md#late-pairing-an-existing-git-branch). |
| Commits already on a **paired** branch | **No.** | Published paired history. Expect a durable hold. |
| The shared parent branch (for example the mainline) that other pairs descend from | **No.** | Same reason, with a larger blast radius. |

A safe way to rebase unpublished work:

```bash
git switch feature/widget
git branch backup/feature-widget-before-rebase     # keep a way back
git fetch origin
git log --oneline origin/feature/widget..HEAD       # confirm these are only yours
git rebase origin/feature/widget                    # replay only your unpublished commits
git push origin feature/widget                      # still a plain, fast-forward push
```

If `git push` is rejected after a rebase, you rewrote something that was already
remote. Stop; do not add `--force`. Go back to your backup branch.

### If you already rewrote and pushed

Do not push more. See [If you caused it](status-and-reconciliation.md#if-you-caused-it).
RepoSync's gate leaves both the Git remote and the SVN branch untouched when it
holds, so there is nothing to "undo" on the SVN side; the work is establishing
what was handled and what was replaced, and that is an administrator task.

### What RepoSync does not promise

**It does not turn an arbitrary force push into a safe SVN rebase, and nothing in
this guide set advertises that it will.** SVN revisions are committed snapshots;
RepoSync does not rewrite them to mirror a Git rewrite. Polling is what catches a
rewrite, so a missing or lost webhook does not let one through. A forced-push
flag from the Git host is only a hint that prompts an inspection.

A side note on history: before the history gate existed, the original
fetch-then-walk path could replay a rewritten branch as duplicate SVN commits.
That behavior is documented as a baseline reproduction in the #66 contract; the
gate is what now blocks it. Do not rely on being the exception.

Sources: [#66 contract][c66], [Git rebase documentation][git-rebase].

## Normal SVN merge versus delete, recreate or replace

### A normal SVN merge appends

The usual way to bring a parent's work into an SVN branch is to run
`svn merge` in a working copy and **commit the result**. That creates a **new
revision** on the branch and may update the `svn:mergeinfo` property. Earlier
revisions stay exactly where they were, and revision numbers keep going up. A
jump in revision numbers alone is not evidence of out-of-order or rewritten
history, because revisions are numbered per repository, not per branch.

From RepoSync's point of view that new revision is ordinary new SVN work to carry
back to Git. See the SVN book on
[basic merging](https://svnbook.red-bean.com/en/1.8/svn.branchmerge.basicmerging.html).

Be exact about the limit of that statement: the project's own contracts say that
file, binary, tree and `svn:mergeinfo` conflicts are **not resolved** by the
refresh preview, and that the content and merge-property cases need explicit
tests. Don't read "a merge appends" as "every merge is handled correctly".

### Delete-and-recreate or replacement is a lineage event

These are different in kind, not degree:

| What happened on the SVN side | Why it is a different event |
| --- | --- |
| The branch path was deleted and a branch was created again at the same path | The path now names a different *incarnation* with a different origin. The old revisions are about something else. |
| A different source was copied into the branch's place | Same, and the content may not relate to what Git has. |
| The SVN repository was replaced, restored from a dump, or loaded over | The repository's UUID or its revision history may differ from what RepoSync recorded. |
| A Git branch was deleted and recreated with the same name | Name reuse is not identity. A matching name is not proof of a matching history. |
| A repository entry in RepoSync was deleted and re-registered with the same label | The label (`id`) is not identity; RepoSync keys its recorded state by a separate scope identity so a reused label cannot adopt old state. |

RepoSync's reviewed operations (late-pair and the v2 refresh) record the SVN
UUID, URL, path and copy-ancestry of what they act on. **Re-anchor, the
operation that would deliberately replace a pair's lineage, is not implemented**
(`reanchor_not_implemented`), and it is deliberately not an alias for refresh,
reset, force-push or deleting the old SVN path. We did not find code in the
*ordinary* sync path that detects a branch recreation as such, so do not assume
RepoSync will notice one.

**If you suspect a lineage event:** stop pushing, do not "reconnect" by editing
anything, and ask an administrator to inspect with the preview and review tools in
[Requesting a refresh](refresh.md). Treat "the new branch looks the same" as
meaningless until lineage is shown.

Sources: [#63 lineage contract][c63], [#69 contract][c69], [re-anchor refusal][core-refresh].

[c66]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/66-history-containment.md
[c63]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/63-lineage-checkpoints.md
[c69]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/69-pair-refresh.md
[core-refresh]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/pair_refresh.rs#L64-L83
[git-rebase]: https://git-scm.com/docs/git-rebase
