# Oversized messages and notification errors (#228)

Commit text is staged in a private anonymous regular file and supplied as stdin. SVN uses `commit --file - --encoding UTF-8`; Git CLI uses `commit --file - --cleanup=verbatim`. The full UTF-8 message and RepoSync provenance trailers are preserved, without an unbounded argv element or a shared/predictable message path. Write/flush/seek errors stop before launch. The child inherits its own file handle; the kernel reclaims the anonymous file after the last handle closes. Existing command supervision, cancellation and uncertain-cleanup behavior are unchanged.

Subversion option handling explicitly recognizes stdin (`-`) and converts according to `--encoding`: [Apache Subversion source](https://github.com/apache/subversion/blob/trunk/subversion/svn/svn.c). Git documents `--file -` and verbatim cleanup in [git-commit](https://git-scm.com/docs/git-commit). No repo-specific AGENTS.md or skill instructions were present in the verified epic source.

Teams display snippets retain their 100/120/300-byte prefix budgets and append an ellipsis, backing up to a valid UTF-8 boundary. This never truncates stored Git/SVN history.

## Recovery

Preserve the existing pair, Git/SVN history, database and journals. Capture the installed version and actual sanitized error first; the reported squash incident has no verified server OS/error/artifact and is not diagnosed by this fix.

- A known process-creation failure occurs before SVN publication. Verify the remote and existing journal/checkpoints rather than editing them. Restore the missing tool/prerequisite or install the separately verified fix through the normal deployment process.
- A timeout, lost reply, cancellation with unconfirmed cleanup, or post-write failure is uncertain. Inspect/reconcile the remote through the existing #63/#64 observe-first flow before retrying. Only its verified absent-unchanged result can authorize the recorded retry; a successful publication must instead be finalized without another commit.
- A notification-only panic does not prove synchronization failed. Inspect the remote effect, journal and checkpoints before issuing any retry.

Do not reset/reimport, skip to HEAD, advance checkpoints manually, rewrite shared history or delete/recreate the pair. This source fix does not authorize work-server access or installation.

## Focused verification

The #62 catalog/scenario runner selects `R01_SVNSERVE_LARGE_MESSAGE_RETRY`. It uses disposable svnserve and bare Git with a >=256KiB multiline Unicode/quotes message, real hook-interpreter launch refusal, checkpoint/mapping checks, explicit reconciliation, one successful retry, exact SVN message/trailer/content checks and quiet repeat. Existing small-message real-engine cases remain mandatory. Unit cases cover private anonymous-file ownership, known missing-command launch, Git CLI exact verbatim history and all four Teams truncation call sites.

Record exact commit/toolchain/commands/results for the primary and legacy backport independently. A source edit, catalog entry or zero selected tests is not qualification. No Windows execution is inferred from Unix results.
