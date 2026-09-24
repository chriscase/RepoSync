# RepoSync PR #72 — legacy checkpoint qualification and retention-safe authority

## /goal

Continue `chriscase/RepoSync`, `feature/reposync-reliability`, draft PR #72 from reviewed head `f74fce855a1f1d80dd631397f436ba33906272e6`.

Read review 3 ([5300692069](https://github.com/chriscase/RepoSync/pull/72#pullrequestreview-5300692069)) on PR72 and `RepoSync-PR72-review-3.md`. The F06/G02/G03/G04 bounded proofs are accepted as progress; G01/checkpoint compatibility is still PARTIAL. Resolve H01/H02 and demonstrate one pinned old-code installation surviving the candidate update before starting a general migration.

Original base: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
Previous continuation SHA-256: `74206ae0ddb5088c9afd362ad4fc9a6506413f799612e2f19187ae9152617df8`.
Reviewed lock graph: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

Copy this brief into a new versioned review/goal file and record its hash. Preserve previous goal/brief bytes. Use the posted GitHub review as the available source when a companion attachment is missing; state the exact fallback, not a goal deviation or permission to reinterpret requirements.

## Constraints

No production endpoints/credentials, active sync directories, mixed old/new writers, schema conversion, general operation service, automatic rebase, checkpoint reset, reimport-as-recovery, broad history repair, force push, merge, release, deployment or active acceptance. Preserve unrelated work. Stay on this branch with ordinary sequential commits and keep the PR draft.

Use the isolated container, required-case runner and scanner. Prepare dependencies separately from test execution. Do not reset/prune the user's Docker daemon to fix local storage problems; use CI and accurately report where execution occurred.

## Stage 1 — reproduce using real persisted states

### H01: imported and no-target handled checkpoints

The current mismatch branch requires the KV cursor to have an applied `git_to_svn` row. This rejects an imported baseline or genuinely handled no-target Git SHA after an SVN publication changes only the column.

Add strict actual-engine cases:

- `R01_IMPORT_CURSOR_SVN_ONLY`: real SVN import completion populates both cursor copies, then incoming SVN work is published. Idle poll, reopened engine and further SVN work must remain valid without requiring a Git-origin commit first. Verify all trees and unchanged ordinary setup behavior.
- `R01_NO_TARGET_GIT_CURSOR`: an SVN-derived branch receives a genuinely empty Git commit, processed through the real no-target path; then an SVN-only revision is published. Prove no invented SVN revision, correct handled outcome, continued poll/restart, and later actual Git work applied once.
- Keep deliberately filtered work separate from a genuinely empty commit and from failed reads/application. If a policy-filtered case is supported, bind its outcome to the policy in force; otherwise explicitly mark that additional case unqualified.
- Preserve rejection of unrelated/malformed/missing proof and the pending-both-directions controls. Fixing compatibility must not acknowledge an SVN-emitted tip as if all its Git ancestors had been handled.

Do not make a normal import test pass by manually adding a fake Git→SVN applied row. A no-target outcome must not pretend a remote commit happened.

### H02: retention must not choose a new baseline

Add `R10_RETENTION_FRONTIER` using the production maintenance function and the supported retry shape A→G→B: A is the initial SVN-derived baseline, G is pending Git work, B is a later verified SVN publication before the next SVN revision's pre-commit failure. The Git KV is missing, as in the existing missing-KV fixture.

Age only synthetic diagnostic timestamps and invoke actual retention. Test that removing old diagnostic rows cannot make G disappear from pending work or advance P to B. Test poll and engine reopen after retention. Add a present-KV variant where the mapping needed by the reader ages out.

If evidence is insufficient after pruning, a precise safe block preserving all pending work is acceptable for that degraded case; silently choosing the first remaining row is not. Healthy installations with required durable proof retained must continue syncing. Do not classify the affected degraded fixture as an automatically recovered or qualified installation.

The reviewer supplied SQL/Git component probes. They are explanatory evidence, not substitutes for these real-engine tests.

## Stage 2 — correct checkpoint authority without guessing

Trace import, both directions, no-target/filtered outcomes, restart, maintenance and all reader/writer copies. Implement the smallest current-schema correction whose authority is justified by actual persisted outcome and ancestry evidence.

Distinguish imported baseline, applied outgoing work, intentionally represented-without-target work, pending work, and missing proof. Full object IDs, repository ownership and the active projection/policy matter. Names, commit-message trailers, matching final trees, row insertion order, highest revision, newest SHA or first retained row alone are not sufficient authority.

Do not reconcile by copying the remote tip or both cursor copies unconditionally. Required proof must survive diagnostic log pruning or be safely materialized into a durable record before relying on it. Existing key-value storage may be used for a small additive verified record if its contract and backward behavior are explicit; this is not authorization for a general job/migration system. Never create a 'verified baseline' key from an unverified observed tip.

Missing legacy proof may require an explicit reconciliation state. Keep that local to the affected repository and explain exactly what evidence is missing. If a new schema is necessary, return the red reproduction and concrete schema proposal at the next review rather than implementing it without approval.

## Stage 3 — one pinned old-code upgrade fixture

Build or invoke the original production code at `87379741779a6259f7eeb52a68cc6f061174e5ef` with the locked dependencies in a separate preparation stage. In the isolated runtime, use it to generate synthetic SVN/Git/state/configuration through its actual import path and completion writer. This fixture's provenance must identify code SHA, executable/library artifact hashes, commands, source tree and any test-only adapter/overlay.

A comparison of two fresh candidate-built databases is not an upgrade test. Neither is a database manually assigned the desired current columns. A generator linked to unchanged old production components is acceptable if it exercises the old production path rather than reimplementing it.

Quiesce the old writer, take a consistent copy of the fixture, then start the candidate on that copy. Preserve IDs, endpoint configuration, branch/path ownership, credentials using synthetic values, enabled/disabled status, valid checkpoint state, mappings and local/remote data. Never run both binaries against the same writable state. First startup must not cause reset, automatic reimport or upstream history mutation.

Prove no-change poll, SVN-only work, Git-only work, a repeat poll and engine reopen. Include the imported-cursor H01 case in this actual upgrade journey. Keep exact-deployed-version qualification NOT ESTABLISHED/NOT RUN; the chosen repository base is only the version actually tested.

Retain an untouched pre-upgrade copy and demonstrate restoration before new external effects. Do not present restoring an old DB after new remote writes as safe general rollback.

## Stage 4 — concrete #63/#64 design, not unreviewed implementation

Update the design with evidence from these fixtures. Specify:

- The permanent baseline/checkpoint ownership and generation key, SVN identity/path/revision, Git handled/emitted positions, and explicit no-target outcome.
- Which existing states migrate automatically, which need reconciliation, and exactly why. Include imported baselines, absent KV, split column/KV, no-target work, pruned diagnostics, disabled entries and multi-repo isolation.
- Retention rules: diagnostic evidence may expire; correctness-critical anchors/frontiers may not be silently inferred from what remains.
- Atomic SQLite transitions and startup version/one-writer checks; preservation of configuration/credential ownership.
- One concrete #64 external-effect example: SVN commit or Git push succeeds but reply/checkpoint persistence fails. Specify durable intent, effect identification, evidence required to resume, and the explicit unknown state. No 'retry until success', patch-ID/message-only identification, or blind DB rollback.
- Coordination with #54 before any migration DDL and a specific proposed next implementation slice.

Do not add migration tables or the general operation service in this pass. This design plus the old-code fixture gives the reviewer enough evidence to approve the next change without a speculative architecture rewrite.

## Validation and handoff

Retain all 36 required cases, existing failure/retry and full-tree oracles, rejection controls, scanner tests and matched original-base comparison. Add exact named H01/H02/upgrade cases; required omissions, skips and failures cannot pass. Where feasible also compare old passing identities from `f74fce8`, not only original main. Disclose any test change or old-code overlay.

Export sanitized before/after manifests, full trees and intermediate revisions, mapping/outcome rows, checkpoint sources, retention deletions, no-op/reopen results, scanner status, exact code/toolchain/lock identifiers, CI runs and downloadable artifact digests. Store compact durable summaries in Git; avoid raw credentials or real databases.

Return:

```text
STATUS: READY_FOR_REVIEW | BLOCKED
PR / BRANCH / PREVIOUS HEAD / FUNCTIONAL HEAD / FINAL HEAD:
ORIGINAL GOAL / THIS BRIEF / LOCK HASHES:
OLD-FIXTURE CODE SHA / ARTIFACT HASH / OVERLAY / GENERATION COMMAND:
CANDIDATE AND CI MERGE HEAD / TREE / REMOTE MATCH:
H01 IMPORT / NO-TARGET OUTCOME: PASS | FAIL | PARTIAL | NOT RUN
H02 MISSING-KV / PRESENT-KV RETENTION: PASS | FAIL | PARTIAL | NOT RUN
OLD-INSTALL UPGRADE / RESTORE-BEFORE-WRITES:
RETAINED F01–F06 AND G01–G04 STATUS:
ALL EXACT CASES / COMPARISON / FULL-TREE AND CHECKPOINT PROOFS:
SCANS / CI ARTIFACT URL / EXPIRY / API AND RECOMPUTED HASHES:
SCHEMA CHANGES: NONE (or return proposed change as blocked, not implemented)
OPEN DEPLOYED-VERSION AND RECOVERY LIMITS:
ONE PROPOSED NEXT SLICE:
```

Stop for independent review. No automatic issue closure or merge readiness from a fixture pass.
