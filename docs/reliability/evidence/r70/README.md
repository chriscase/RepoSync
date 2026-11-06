# R20 / branch-pair deletion acceptance

Refs #70

R20 records the reviewed deletion-navigation behavior. It does not qualify the
whole reliability epic, a deployed installation, enterprise targets, or a release
binary. The baseline remains NOT RUN.

## Execution and review

- Reviewed head: `3f96f8b1a50a456ceceb5e09257bb9a09c022712`.
- Original base: `43087e8501be56b04549472ddd0dad24b980055e`.
- Actual E2E checkout: `refs/remotes/pull/208/merge`, commit
  `0007b5ac22faf3229e84d6be88c0d9e4a813baef`.
- Both commits have tree `4d76b31c3af84e49c31135860750c5478cce529b`; their
  tracked-tree diff is empty. The merge parents are the original base and head.
- [E2E run 38865124037](https://github.com/chriscase/RepoSync/actions/runs/38865124037)
  passed 35 helper tests and 14 browser scenarios. The real API cases confirm
  removal in the database, preservation of the parent, replacement history and
  absence of removed-detail polling. Warning and failure variants are deterministic.
- Independent direct Grok review returned **SHIP-WITH-NITS** for that head/tree,
  accepting the behavior and explicitly labeled identical-tree candidate-code
  evidence. It did not verify the newly captured mutation receipts below; those
  remain subject to the final evidence-delta review.

[acceptance.json](acceptance.json) maps all seven original acceptance criteria to
the exercised cases. [execution.log](execution.log) retains the relevant actual
checkout, environment, counts, case names and database-confirmation receipt.
The manifest includes the full downloaded log digest and immutable workflow and
UI lockfile identities.

The [GOAL handoff contract](../../GOAL.md#required-handoff-format) requires exact
base/head and artifact traceability and separate identification of PR merge-ref
testing. Identical tracked trees support the reviewed code-behavior claim here;
they do not make the commits identical. The web build embeds its checkout revision
in health metadata. The generated Rust lock and test-binary digests were not
retained, so no binary-identity or reproducible-release claim is made.

Main subsequently advanced to `1221b92b64669ec953e711df7079c6a05510c53a`.
The original receipt remains tied to the original checkout. The acceptance-only
closeout head and its current-main integration checks are identified separately
in [PR #208](https://github.com/chriscase/RepoSync/pull/208); this document does not
claim that an unrecorded future checkout ran.

## Six mutation controls

[mutation-controls.json](mutation-controls.json) records exact source replacements,
source hashes, commands, exit codes, failing assertions, environment versions and
SHA-256 digests of the adjacent full sanitized logs. The unchanged positive
controls establish that 35 helper tests and 12 deterministic mock-browser cases
pass in the same local environment. Each mutation was applied separately in a
disposable source copy and then restored; the production source was not edited.

| Control | Deliberate defect | Required rejection |
| --- | --- | --- |
| [queued-completion](queued-completion.log) | Treat queued states as completed | Queued lifecycle helper assertions fail |
| [child-destination](child-destination.log) | Leave the parent when deleting a child | Parent destination assertion fails |
| [missing-parent-fallback](missing-parent-fallback.log) | Navigate to a confirmed missing parent | Repository-list fallback assertion fails |
| [persisted-warnings](persisted-warnings.log) | Drop normalized cleanup warnings | Notice round-trip assertions fail |
| [detail-query-retirement](detail-query-retirement.log) | Omit import-status query cancellation | Complete query-key assertion fails |
| [parent-warning-visibility](parent-warning-visibility.log) | Drop warnings only in the parent-local notice | Combined browser case fails at the specific child-cleanup warning assertion |

To reproduce, check out the recorded source head, run `npm ci --prefix web-ui`,
apply one manifest replacement, and run its recorded command from the recorded
relative working directory. Restore that file before the next control. Browser
controls require Chrome, a Node version with global WebSocket support, and the
`CHROME_BIN` environment override when Chrome is not on PATH. These controls use
the in-process mock API, not hosted or real-API qualification.

Logs remove ANSI escapes and replace checkout/temporary path prefixes with
placeholders. The manifest describes that processing; digests cover the committed
sanitized logs. No other output is elided.

## Failure and retry disclosure

- [Exact-head push CI](https://github.com/chriscase/RepoSync/actions/runs/38865063541)
  passed. Its default Rust test run ignored R70; it is not the browser receipt.
- [PR CI attempt 1](https://github.com/chriscase/RepoSync/actions/runs/38865124188/attempts/1)
  failed the unchanged process-wide descriptor diagnostic: 37 to 90 descriptors
  (+53). The assertion was not weakened. Attempt 2 passed without source changes.
  [ci-failures.log](ci-failures.log) preserves the first failure and passing retry.
  Parallel measurement interference is consistent with these results; a repeatable
  product leak was not established.
- [Phase 0 run 38865124040](https://github.com/chriscase/RepoSync/actions/runs/38865124040)
  failed before runtime because the pinned legacy fixture lock required an update
  under `--locked`; the follow-on evidence copy failed because its output directory
  was absent. The [prior unrelated-branch run](https://github.com/chriscase/RepoSync/actions/runs/38807266554)
  has the same errors. This remains full-epic qualification debt and is nongating
  for ordinary PR merge. It is not converted to PASS by R20 or by a retry.
