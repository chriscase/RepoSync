# Legacy large-message fix (#228)

This is a narrow backport to `legacy/team-install`. SVN commit messages use a private anonymous regular file as stdin (`commit --file - --encoding UTF-8`). Git CLI uses the same transport with `--cleanup=verbatim`, preserving the existing optional author date. Unix mode is explicitly set to `0600` before any message bytes; creation, permission, write, flush and rewind errors propagate before launch. The inherited child handle keeps the file alive until its last handle closes. No predictable message pathname or pipe writer is introduced.

All four Teams display snippets retain their 100/120/300-byte prefix budgets, backing up to a UTF-8 boundary before adding the ellipsis. This display change does not truncate stored history.

The branch's existing schema, configuration, checkpoints, echo detection, timeouts, retries and APIs remain unchanged. In particular, this backport does **not** import the epic's durable reconciliation/uncertain-cleanup machinery. An existing message containing `[reposync]` is still treated as an echo by the legacy engine. SVN's inherited editor-sentinel handling is unchanged; lossless input transport does not establish arbitrary-message verbatim SVN behavior.

## Verification

The existing `team_mode_e2e` file:// harness supplies `candidate_228_legacy_large_message_failure_retry_and_repeat`: a synthetic >=256KiB multiline Unicode/quotes message, real pre-commit hook interpreter launch failure, unchanged remote revision/checkpoints/no applied mapping, explicit observation of that pre-publication failure, one retry, exact SVN text/provenance/content and quiet repeat. The existing small-message `test_team_mode_git_to_svn_sync` remains a separate required check. Unit regressions cover anonymous-file privacy and content, missing-executable failure, large Git CLI history with the original author date, and all Unicode snippet boundaries. Missing SVN tools are a qualification failure for the new case, not a passing skip.

Record the exact legacy source, toolchain, compiled selectors and results independently. The primary epic's results are not legacy qualification. The untouched legacy baseline already has formatter differences; preserve that evidence and report required-CI failures honestly rather than weakening checks or treating a focused pass as whole-workspace qualification.

## Safe recovery

Preserve the existing pair, Git/SVN history, database and checkpoints. Inspect the actual sanitized error and remote state before deciding whether a write occurred. A confirmed missing executable or pre-commit hook launch refusal is different from a timeout, lost reply or post-publication error. This backport adds no authority to replay an uncertain write: retain evidence and use the existing operational recovery procedure with the responsible owner. A notifier panic alone does not establish sync failure.

Do not reset/reimport, skip to HEAD, manually advance checkpoints, rewrite shared history or delete/recreate the pair. No company inputs, server access, settings/credential changes, installation or deployment are authorized by this source backport.
