# Branch policy, local hooks and GitHub Actions

*Back to the [guide index](README.md).*

Three different mechanisms get mixed up when people say "stop people rewriting
paired branches". They sit at different points, and only one of them can actually
refuse a bad update.

> **This guide changes nothing.** It describes options and gives a recommendation.
> It does not create or change a branch rule, ruleset, webhook, hook, workflow,
> secret or bypass grant on any repository. Turning any of that on needs an
> authorized administrator's decision (for this project, Chris or another admin).

## The three mechanisms

| Mechanism | When it runs | Can it stop the rewrite? | Who controls it | Can a developer bypass it? |
| --- | --- | --- | --- | --- |
| **GitHub Actions workflow on `push`** | *After* the host has accepted the update | **No.** It is a reaction, not a barrier. It can notify, annotate or request a check; it cannot prevent the push that triggered it. | Whoever can edit workflows and secrets | n/a: it never gates the push |
| **Local Git hooks** (`pre-rebase`, `pre-push`) | On the developer's machine, before the local command finishes | Only the **local command** on **that machine**. | Each developer, per clone | **Yes**, easily (see below) |
| **Server-side rules** (protected branches / rulesets; a host-side `pre-receive` hook where the host allows one) | On the host, *before* the ref is updated | **Yes**, for the refs and actors the rule covers | A repository or organization **administrator** | Only by someone given bypass rights |

Primary references: [GitHub workflow events][gh-events],
[Git hooks][git-hooks], [GitHub protected branches][gh-protect].

### Push-triggered Actions

A workflow with `on: push` starts because an update was already accepted. It is
useful for telling people something happened. It is **not** a safeguard against a
force push, and RepoSync's history gate does not depend on one: polling catches
the rewrite regardless.

If you do write a notification workflow, keep to least privilege: do not hand a
RepoSync operation token to a job that runs code from a pull request or a fork,
do not interpolate branch names or other event text into `run:` shell commands
(pass them as environment variables and quote them), and keep any secret in a
protected environment, not a repository-wide secret. This guide does not give you
a workflow to copy; the opt-in request workflow is [held](held-and-unavailable.md).

### Local hooks: useful nudges, not enforcement

A hook can make the common mistake harder for a person who installed it:

- **`pre-rebase`** receives the upstream and, only when you name a branch, that
  branch as a second argument. When you rebase the *current* branch the second
  argument is empty, so a hook has to look up the current branch itself. Its
  non-zero exit stops the rebase.
- **`pre-push`** is told the refs being pushed (on standard input) and can warn or
  refuse a non-fast-forward update to a paired ref.

What they cannot do (the rebase, push and `core.hooksPath` rows, and the
worktree note, were each run once in a scratch repository for this guide; the
rest is standard Git behavior described in the [hooks documentation][git-hooks]):

| Way around it | What happens |
| --- | --- |
| `git rebase --no-verify …` | The `pre-rebase` hook is skipped. (`git push --no-verify` skips `pre-push`.) |
| `git -c core.hooksPath=<dir> …`, or setting `core.hooksPath` | Git looks in that directory instead, so a hook in `.git/hooks` does not run. |
| Not installing it | Hooks are not copied by `git clone`. Every clone starts without them. |
| Different tools | Anything that does not run Git's hook points (some GUIs, the web editor, API clients) never triggers them. |
| Other ways to rewrite | `git commit --amend`, `git reset` followed by a push, and history-rewriting tools are not all covered by a `pre-rebase` hook. |

Other things worth knowing when someone installs hooks by hand:

- **Linked worktrees** (`git worktree add`) use the main repository's hooks
  directory, unless `core.hooksPath` says otherwise. `git rev-parse --git-path hooks`
  shows the directory in effect.
- A hook installed into a directory that already has hooks, or into a clone with
  `core.hooksPath` set, can replace or shadow what was there. Look before you copy.
- Offline behavior: a purely local check works offline. One that asks the remote
  or RepoSync a question needs the network and must decide what to do when it
  cannot reach it; "fail open" and "fail closed" are both defensible, but pick on purpose.

**RepoSync supplies no hook installer or remover, and this guide installs
nothing.** A guarded installer that preserves an existing hook chain is **held**
(see [Held and unavailable](held-and-unavailable.md)). Do not modify other
people's clones.

One thing that does exist: `GET /api/repos/{id}/hooks/pre-commit` returns a
downloadable Git **`pre-commit`** script that checks staged files against that
repository's allowed-path and blocked-pattern rules. It has nothing to do with
rebases or rewrites. Its own header tells you to copy it into place yourself.
Source: [pre-commit hook endpoint][precommit].

### Server-side rules

Only the host can refuse the update itself. On GitHub that means a protected
branch rule or a ruleset on the paired refs, with the right bypass list. A
custom `pre-receive` hook is an option only on hosting that supports it, and only
with the authority to install one. What a rule can actually do depends on the
host, its version, the plan, the permissions of whoever configures it, and
the bypass settings. This guide did not test any host or version, and states no
supported-version matrix.

## Recommendation for paired refs

For **each paired Git branch, named explicitly**, ask an administrator to:

1. **Block non-fast-forward updates (force pushes).** This is the rule that turns an
   accidental rewrite into a visible rejected push instead of a RepoSync hold.
2. **Block deletion.**
3. **Leave ordinary fast-forward pushes working.** Do not add a rule that forces every
   change through a pull request or a status check on a ref that RepoSync itself publishes
   to by direct push, unless the administrators have deliberately granted RepoSync's
   actor a bypass. Granting any bypass is an administrator decision and is not
   made or implied here.
4. **Keep the bypass list empty** unless there is a recorded reason. Do not give a
   developer-facing automation token bypass rights.
5. **Name the paired refs.** Do not freeze every developer branch with a wildcard;
   private, unpaired branches are where rebasing is fine.

An **illustrative** GitHub ruleset shape for one paired branch. It is a sketch to
discuss with your administrator, **not an applied rule** and not a command to
run. Check the field names against the ruleset documentation for the GitHub
version you actually use, and test on a throwaway repository first:

```json
{
  "name": "reposync-paired-ref-recommendation",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["refs/heads/feature/widget"],
      "exclude": []
    }
  },
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" }
  ],
  "bypass_actors": []
}
```

### What rules will and will not interfere with RepoSync

Checked in the source at the pinned commit:

| RepoSync action | Is it a fast-forward? | Effect of the recommended rules |
| --- | --- | --- |
| Ordinary sync publication | Yes (plain push) | Unaffected. |
| Late-pair publication to Git | Yes. The code proves the new tip descends from the old one before it pushes, and uses `--force-with-lease` with an exact expected tip only to close a race. | Unaffected: the update is still a fast-forward. |
| v2 pair-refresh publication to Git (draft) | Pushes the prepared commit to the child ref with a refspec that has no force marker, and checks that the remote's old value equals the pinned tip. | Unaffected when the result is a fast-forward; a non-fast-forward would be refused by the remote. |
| **Initial import** to a destination branch | The first batch is published with a force-style push | Would be rejected if the rule is already in place. Apply the rule **after** the branch is populated and paired. |
| Setup wizard **Reset & Reimport** | Force-pushes a fresh import | Would be rejected, which is the correct outcome for a paired ref. Don't use it on one. |
| Legacy `DELETE /api/repos/{id}/branch-pair` | Deletes remote branches by default if the caller doesn't say otherwise | The deletion rule would reject the Git branch delete. Expect a visible failure, not a silent skip. |

**A rejected update should stay a visible failure.** If RepoSync is refused by a
rule, that is the rule working; resolve it with the administrator, not by
loosening the rule from a script.

A rule is also not a substitute for RepoSync's own checks. Even with a perfect
ruleset, RepoSync still validates ancestry before it replays anything.

Sources: [late-pair Git publication][lpp], [v2 refresh publication][refresh-pub], [import publication][import-pub],
[reset & reimport][reset-reimport], [branch-pair delete defaults][delete-flags].

## Safe next steps

- Developer: install only hooks you understand into clones you own, treat them as
  a seatbelt, and keep the plain `git push` habit.
- Team lead: ask an administrator to review the recommendation above for the
  specific paired refs. Bring the list of refs and who currently has bypass or
  admin rights.
- Administrator: apply on a test repository first; confirm that a fast-forward push
  still succeeds, that a force push is rejected, and that RepoSync's own publication
  still works, before touching a live repository.

[gh-events]: https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows
[git-hooks]: https://git-scm.com/docs/githooks
[gh-protect]: https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches
[precommit]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L4477-L4513
[refresh-pub]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/pair_refresh/publication.rs#L262-L293
[lpp]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/late_pair_publish.rs#L615-L650
[import-pub]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/import.rs#L2608-L2640
[reset-reimport]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/setup.rs#L1515-L1520
[delete-flags]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L3952-L3962
