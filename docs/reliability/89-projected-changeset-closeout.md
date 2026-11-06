# #89: projected changeset and excluded-change leakage

The failure this story prevents is concrete: a Git commit containing only excluded paths must not dirty the SVN working copy and leak those paths when a later allowed commit is published. A mixed commit must publish the same projected path set that is verified, journaled and reconciled after a lost response.

This closeout reuses the implementation already integrated in epic `31a8326f252ae2643a3346006b25e74fb3e4e9d1`. The new delta strengthens executable evidence; it does not replace the production engine, change path policy, remove conflict holds or introduce a second harness. Final command outcomes are recorded against the frozen candidate in the accompanying qualification receipt. Independent review, exact-head CI and qualified post-merge verification remain separate gates before issue closure.

## Canonical acceptance

| Criterion | Existing source and credited behavior | Executable proof |
| --- | --- | --- |
| One typed projected set before any WC mutation | `ProjectedGitToSvnChangeset` and `project_git_to_svn_changeset` in `path_projection.rs`; `sync_git_to_svn` projects raw inputs before reading included blobs or checkout/staging. The resulting `file_contents` drives staging, no-delta verification and `persist_git_to_svn_intent`. Empty projection records a durable filtered receipt before checkout. | Projection unit cases; RSC02 mixed/deletion/rename cases; exact journal paths in the new lost-reply case. |
| All-filtered pristine WC; later allowed work cannot publish residue; two/three commits and restart | An empty included set takes the no-target path without writing excluded blobs or initializing a WC. In a batch that already initialized a WC, the filtered boundary must still be clean. | `rs89_rsc02_filtered_restart_allowed_preserves_receipt_and_tree`; strengthened `candidate_rsc02_filtered_then_allowed_cycle_leaves_no_blocked_residue`. |
| Component-aware allow/block for deletes and both rename ends | `path_matches_prefix`, `path_is_projected` and `project_rename_endpoints` apply the same policy to each endpoint. Prefix `team` does not include `team-other`. | Existing out-of-scope delete and seven rename cases, including excluded-destination blob-read refusal; projection/hook unit cases. |
| Mixed commits preserve excluded files/directories; reconcile the post-filter path set after lost reply | Included paths alone enter the journal; reconciliation verifies branch-relative observed changes and the intended full SVN tree using the same target/projection fingerprint. | Existing mixed/sibling case plus `rs89_rsc02_mixed_lost_reply_reconciles_only_projected_paths`. |
| Actual engine, per-revision paths/trees/checkpoints | Existing `QualifiedPair` runs local SVN, Git, SQLite, identity mapping and the real engine. R19 verifies untouched trees and explicit deletion. | Independent SVN export and `svnlook changed` assertions for every revision added by the two/three-commit and lost-reply scenarios; durable receipt/mapping/checkpoint and repeat assertions. |

## New evidence

The three-commit scenario is allowed → all-filtered → allowed in one engine cycle, followed by restart. A test-only tracing subscriber reads the **actual reused temporary WC** at the engine's existing filtered-boundary event, before its TempDir is dropped. `svn status --no-ignore` must succeed with empty stdout and stderr. The test fails if the boundary or initialized WC cannot be observed. It does not change the engine's decision, replace any OS function, inject completion evidence or suppress status entries. Each of the two real SVN revisions is then independently exported and compared as a complete file tree, with its exact `svnlook changed` action/path set.

The new two-commit case executes an all-filtered first cycle, reopens the engine and database, then publishes the allowed second commit. It checks that the first cycle never initializes a WC, leaves the SVN revision/tree and applied-mapping count unchanged, and advances only the handled Git cursor with an authentic filtered receipt. The receipt's source SHA and projection are read back and remain byte-identical across restart. The successor creates exactly one SVN revision with exactly one allowed path. A further reconstructed-engine repeat leaves the revision, tree and checkpoint unchanged.

The new mixed lost-reply case first seeds included and excluded files/directories through the real engine. It then mixes an allowed addition with a blocked suffix edit, an out-of-scope deletion and a sibling-prefix edit. The existing scoped lost-reply fixture fault is enabled only for this repository: SVN really accepts the projected commit before the engine loses the response. The test requires the existing reconciliation hold, unchanged local mapping/checkpoint and exactly one new remote revision. Its full exported tree preserves the excluded content; `svnlook changed` and journal intent both contain only `team/allowed.txt`. A reconstructed engine performs the existing reconciliation, adds exactly one mapping, records the actual revision and clears the active operation. Repeated reconciliation and a fresh engine cycle cannot publish or map it again.

The attribution regression supplies malformed persisted ID/path values in an in-memory two-repository database. Operator listings must propagate decode errors instead of dropping a row, while conflict count, NULL ownership and the retained row remain intact. This tests an already-correct implementation; it is not a new ownership inference or migration.

## Historical reconciliation

Reuse and preserve these merged changes:

- [#92](https://github.com/chriscase/RepoSync/pull/92): typed projection before WC mutation.
- [#132](https://github.com/chriscase/RepoSync/pull/132): independent rename-endpoint projection and registered RSC02 cases.
- [#133](https://github.com/chriscase/RepoSync/pull/133): rename-aware conflict detection.
- [#162](https://github.com/chriscase/RepoSync/pull/162): projected conflict endpoints, confined reads and out-of-scope blob-read evidence.
- [#184](https://github.com/chriscase/RepoSync/pull/184): idempotent conflicts, hook matching and catalog coverage.
- [#185](https://github.com/chriscase/RepoSync/pull/185): persisted-row apply gate, upsert integrity and hook quoting.
- [#186](https://github.com/chriscase/RepoSync/pull/186): legacy NULL-row attribution/refusal and persisted-gate isolation.

The later source already propagates operator-listing decode errors with `collect::<Result<...>>()`; the new negative test preserves that behavior. Existing RSC02 cases cover the current single-repository hint, ambiguous NULL ownership, orphan siblings, persisted SVN-only gating, force-push-back and resume-authorized publication refusal. Hook unit tests cover component boundaries, quoted metacharacters, suffix literals and blocked descendants. Earlier comments naming those gaps are historical; their assertions must be reconciled with current source rather than reimplemented or treated as current failures.

Retain one explicit limitation from the #186 review: current single-registration attribution is conservative, but does not prove that a legacy NULL row historically belonged to the sole surviving registration. Removing other registrations can over-block that survivor. This closeout does not weaken the resulting hold, infer a missing historical owner or claim a general migration/ownership solution. The reviewer and publisher must assess that recorded limitation explicitly; no author waiver or silent dismissal is made.

## Reproducible focused profile

Use the existing local fixture software and a recorded lockfile. These commands need no real account, remote server, company data or deployment:

```sh
CARGO_BUILD_JOBS=2 cargo clippy --locked --offline -p reposync-core --lib --test team_mode_e2e -- -D warnings
cargo fmt --all -- --check
cargo test --locked --offline -p reposync-core --lib path_projection::tests -- --nocapture
cargo test --locked --offline -p reposync-core --lib conflict_repo_attribution::tests -- --nocapture
cargo test --locked --offline -p reposync-core --test team_mode_e2e rsc02 -- --nocapture --test-threads=1
cargo test --locked --offline -p reposync-core --test team_mode_e2e candidate_r19_ -- --nocapture --test-threads=1
```

Verify `svn`, `svnadmin` and `svnlook` actually execute before interpreting existing graceful-tool-absence guards. The new fixture cases require the tools; a missing tool is not a pass. Logs retain named outcomes and `RELIABILITY_EVIDENCE` records. A command failure must be diagnosed, not hidden by changing assertions, ignoring cases or accepting a broader partial summary.

`Cargo.lock` is ignored by this repository. The local profile reuses the retained 82,330-byte lock from the prior accepted project checkout (SHA256 `51bbace031e7c8b6471abff70582b0f512cd163430e6d459d42e6e0210735c92`), after checking all six Cargo manifests are byte-identical. That is a recorded local dependency input, not a claim that hosted CI resolved the same graph. No manifest, dependency declaration or workflow is changed here.

Standard hosted CI already executes the integration target and the core unit tests; no new workflow or dispatch mechanism is required. Publisher69e9 owns exact-source publication, fresh run/operator binding, integration and post-merge verification. Existing passing baseline logs are kept separate from executions on this candidate. The two R19 cases establish this story's untouched-tree/delete behavior; they do not close the entire #60 matrix, #52/#57 path-rewrite product, #53/#59 policies, live soak, GHE operation or the whole reliability epic.

Schema, DB module/initialization, production code, workflows, GOAL, other writers' work and frozen #69/native/process/deployment holds remain unchanged. #121's actual-reader/usability acceptance remains separately deferred under the user's direction; this story does not supply or waive it.
