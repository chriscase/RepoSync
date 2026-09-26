# PR72 recovered work and copy-only v13/v14: review handoff

**Status: READY_FOR_REVIEW — bounded candidate awaiting independent review.** This is the bounded copy-only candidate, not production activation. Original GOAL, prior brief/review bytes and all original 51 required cases remain unchanged.

## Recovery identity

Accessible feature worktree: `/Users/chriscase/.codex/.chatgpt-projects/g-p-6ab4032b242881918e64ca6dd809e6a1/reposync-reliability`, branch `feature/reposync-reliability`. At recovery, HEAD/upstream/remote/merge-base all equaled reviewed `c52dea6554a629efb7725ea620659501ad6af7a1`; zero after-head or local-only continuation commits, staged/unstaged/untracked changes. Clean. Preservation ref `refs/recovery/pr72-before-copy-migration` pins it.

The accessible Grok session names `/Users/chriscase/Documents/GitHub/RepoSync`, clean older `main` `bfbee349f453fdcdd9b66738234e34831168307b`. Its five-message `exirt` session has no migration task/code-tool turn; the Grok registry has no RepoSync worktree row. No interrupted migration implementation was found. Other-machine/container state remains NOT ESTABLISHED. The older clone and unrelated local legacy commit remain untouched. The metadata-only source checkout's staged deletions were preserved as an exact private binary patch, SHA-256 `8e23b809895fa2d38267bc9fb5104696381438bd0875a07c95c75c90ee2d823e`, before implementation. No competing migration change was found among the inspected worktrees/499 Git locations. See [recovery audit](recovery-audit.md).

## Fixed contract identities

- Original base/main: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
- Independently reviewed starting head: `c52dea6554a629efb7725ea620659501ad6af7a1` (posted review #5311615743).
- Retained comparison anchor: `f74fce855a1f1d80dd631397f436ba33906272e6`; not the immediate predecessor.
- Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
- Exact copied current goal: `2c7b9414b4d91536219974ba4d4fcb407f143eaf2a09361f578fedc46ffd309f`.
- Dependency lock: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.
- Prior proposal archive: `581a3b411d2488f1708c136a6d7ccc8e9cd75fcf3616a57c9e942c10cc533e37`; prior handoff/brief archives verified byte-identical to c52.


## Published functional identity and CI receipt

- Functional code head: `cdb0907f75512b20f4932f9e4f9cb57aa60d1f42`.
- Tested pre-handoff documentation/remote head: `5142e87f06004eada3874ae610c6b3e21f404048`. It differs from the functional head only in the migration-contract clarification.
- Tested PR synthetic merge: `9bfa043c47a481d533a4c4d7d16ac122c306d299`. Feature and merge tree both `d7dc06e1b3c09cc51bdb85abe8dd34c00bf77ccf`.
- Final documentation identity is the commit containing this file; its exact SHA, remote/synthetic-merge/tree identities and any documentation-head CI receipt are reported in the PR body and return handoff. This avoids inventing a self-referential commit hash inside its own contents. Runtime/harness changes stop at the functional head.
- Worktree at publication of functional evidence: clean; ordinary pushes only; PR72 remains draft.

[Complete isolated and matched gate](https://github.com/chriscase/RepoSync/actions/runs/36261197715): **70/0/0 exact cases**, required/executed IDs identical; 2 retained observations, 67 regression cases and 1 admission control. Six sandbox canaries PASS. Complete publication scan PASS, **190 files / 964879 bytes**; independent downloaded scan PASS, **191 / 964973** (includes scan receipt). Internal exact-case scan: 144 / 755985.

Original matched base: **324/5/1**; retained f74 anchor: **362/1/1**; candidate: **375/1/1**. Both regression lists are empty. Remaining failure is `integration::test_full_svn_to_git_cycle_with_metadata`; ignored is `team_mode_e2e::test_team_mode_conflict_detection`. No failure/skip is removed.

[Broader E2E](https://github.com/chriscase/RepoSync/actions/runs/36261197746) PASS: team **52/0/1**, personal **41/0/0**, server **12/0/0**, ordinary startup **1/0/0**; S7 provenance self-test PASS; bounded LFS selection **10/0/0**, plus two core threshold cases. [Ordinary CI](https://github.com/chriscase/RepoSync/actions/runs/36261197649) remains failed at formatting; no broad formatting rewrite was performed.

[Download artifact 10912417105](https://github.com/chriscase/RepoSync/actions/runs/36261197715/artifacts/10912417105), expires **2026-10-10T18:15:06Z**. API and recomputed ZIP SHA-256 both **`37e484698f52b4f91aec68b3496bb3a75066233d063419b7c6f1e54c39654a80`**.

| Extracted evidence | SHA-256 |
| --- | --- |
| summary.json | `19519130db79e5912ca78cf20b5585a37326243a32d4e22ca618f571a2db4439` |
| comparison.json | `bf083969f63e4cf33fafa4e2c6110d37ba23e7f935a2e0f8d0b256c77afe471a` |
| previous-comparison.json | `7779fb4bb46de1190053f295cd0b2b8ca6d238cb13e429e441b21ee2739270c4` |
| V14_OLD_TOPOLOGY.json | `a629249e11cc4d8e8a9c12409fe4e08d72d2a2fc2c1c5721bd311dbe41ddbd83` |
| MIGRATION_CRASH.json | `86fe70b48f3d265cae9beff92599e3b8170882a5ac918cfa9ede566c4bd095e3` |
| K01_REJECTION.json | `c5c05e43bc33cbebe5eca7e6877fc2228fdfc2563ff09b279d2996db1ab3d5d8` |
| K03_ORIGIN.json | `163a163c6914ca662c471fcd6c6087e56272eb92def804172bfcfde9044eb6d2` |
| PINNED_UNQUALIFIED.json | `74b2a9c5ceb8cda606827f39753a5dbec3a7b0f0614924bb98ce3a3d85b9cfe2` |
| MAPPING_NULL.json | `0d07024bff7a6c7f6c18b733963329e6e25207bb571e7f651c067f08fa830371` |
| STARTUP_V12.json | `d0e80b69010cb1ad3de551c865141b46b7d9c9d1fc57a22e14a30fd7228e3298` |

The earlier failed [run36260520453](https://github.com/chriscase/RepoSync/actions/runs/36260520453) retains its 67/3/0 result and [scanned artifact10912321000](https://github.com/chriscase/RepoSync/actions/runs/36260520453/artifacts/10912321000), API/recomputed ZIP SHA-256 `93398e8f996b416651bdb86f097e6a398620ec35db7df7054f1ea6daca00ec22`, expiry2026-10-10T17:55:59Z. The three failures were the original empty-r1 qualification assumption, corrected by actual retained-history proof; assertions were strengthened rather than hidden. The first unarchived failed run36260041046 also remains in GitHub history.

## Requirement checklist

| Requirement | Result at bounded review gate |
| --- | --- |
| Recovery and preservation | DONE AND VERIFIED — Accessible state audited and preserved; no Grok migration code located; other-machine state unestablished |
| K01 owner/source/policy/resolved frontier authority | DONE AND VERIFIED — Candidate implementation and strict positive/negative matrices |
| K02 sequence/history/row/index preservation | DONE AND VERIFIED — Six source shapes; every retained SQL value and exact sequence entry compared |
| K03 complete copy origin and nullable checks | DONE AND VERIFIED — All seven combinations; mandatory directional NULL bypass refused |
| Candidate v13/v14 registry and shape validation | DONE AND VERIFIED — One checked implementation; exact physical shape, FK/integrity and canonical validation |
| Copy-only boundary / source preservation | DONE AND VERIFIED — Temporary sealed copies only; original read-only and unchanged; pinned writer exits first |
| Atomicity / interruptions / retry | DONE AND VERIFIED — Handled errors plus 13 abrupt-exit boundaries; v13 retained on failed v14; no half version success |
| Old-install qualification | DONE AND VERIFIED — Actual pinned importer and enrolled local SVN/Git identities/trees/history; disabled/unqualified repositories receive no frontiers |
| #54 compatibility | DONE AND VERIFIED — Distinct candidate typed mapping adapter and concrete reader audit; operational reader activation not implemented |
| Retained tests / canaries / scanning / matched comparison | DONE AND VERIFIED — Complete exact manifest and independent downloaded-evidence scan required |
| Normal startup nonactivation | DONE AND VERIFIED — Default and fixture builds remain schema v12; new candidate tables absent; restart/idempotent initialize proved |
| General #64 recovery and production activation | NOT DONE, explicitly outside scope |

## Exact new case coverage

All original 51 case objects/oracles remain. Added 19 required IDs:

| IDs | Strict oracle |
| --- | --- |
| K01_VALID | DONE AND VERIFIED — Six baseline frontiers, three valid resolved transitions, explicit generation, no invented outbound SVN effect, replacement/reset refused |
| K01_REJECTION | DONE AND VERIFIED — Cross-repo/gen/direction/source/policy/projection, pending/unknown/reconciliation, nonexistent/NULL, later-baseline and stale predecessor all rejected without frontier damage |
| K03_ORIGIN | DONE AND VERIFIED — NULL/NULL and complete positive origins accepted; path/NULL, NULL/positive, zero, negative rejected; empty root path/positive accepted |
| V13_NEVER_USED, V13_ROWS, V13_SPARSE_HIGH, V13_EMPTY_USED, V13_LARGE_SEQUENCE | Exact legacy rows/IDs/all values/indexes and sequence continuity; next IDs 1,4,101,101,4294967297; other audit sequence remains81 |
| V13_ROLLBACK | Seven error boundaries leave v12 and exact DB bytes; retry reaches complete v13 |
| V14_OLD_TOPOLOGY | Two independently proved imported pairs, four matching initial directional frontiers, preserved owned evidence links; disabled third has no generation; repeated invocation is byte-identical |
| V14_RESTART | Six v14 errors preserve committed valid13, zero partial canonical tables; reopen/retry reaches14 |
| V14_UNQUALIFIED, PINNED_UNQUALIFIED | Read-safe dispositions; no canonical authority; all old values/sequences retained. Actual oldest retained rows are removed/altered in labeled overlays, never assuming an absent r1 row is pruning |
| MIGRATION_CRASH | Child exit86 at seven v13 and six v14 boundaries, SQLite journal recovery and successful retry; source unchanged |
| MIGRATION_WRITE_FAILURE | Actual SQLITE_READONLY/query-only, NOT NULL and FK-off rejection; version/content preserved and retry succeeds |
| MIGRATION_FORGED | Versions13/14/99 with wrong physical shape, missing index, extra column/trigger all refused with copied bytes unchanged |
| COPY_BOUNDARY | Overlap, symlink, pending source WAL and changed non-DB copy bytes refused |
| MAPPING_NULL | Mapped, owned NULL unresolved, ownerless unresolved, missing, explicitly linked typed no-target distinguished; wrong generation unresolved and duplicate scoped rows refused |
| STARTUP_V12 | Ordinary fresh/repeated/restart initialize remains12, git_sha NOT NULL, zero candidate tables |

## Fixture provenance and migration proofs

The generator is the existing unchanged driver over an archived original `8737974` production source and the locked graph. Driver SHA-256 `9a2485558f3f70677014007a3d45930eb2739c8d8b64aa854704bd681d9bad6c`. Linux old-generator binary remains `40941fc98963903043e8709eb587fdafd435853c06529ed721c79d822c3194ea`. A fresh local archive/build was also used to rerun the 15 copy cases; cached local binary identity was not taken as standalone provenance.

The first isolated qualification required two applied rows and failed three new cases (all retained51 passed). Original production import can skip an empty r1. The corrected proof requires an empty/property-free actual SVN r1, an empty bootstrap, linear Git ancestry and exact retained rows/mappings for each actual imported object; r2 source/Git full trees, pinned UUID/path/ref/policy and baseline values must agree. A missing row for an extant import commit still fails as pruned history. No historical filtered/no-target decision is invented.

v12 original remains sealed and unchanged; copy13 rebuild and copy14 ownership are independently atomic. Sanitized reports include pre/final versions, ID-bearing row digests/counts, exact sqlite_sequence rows, FK empty/integrity ok, source seal and full installation hashes/modes, canonical rows, and before/after endpoint file manifests. Every legacy value (including secret-table values, credential ownership, enabled/parent/config/checkpoint/receipt/policy fields) is compared privately; credential values/individual secret digests are withheld. No remote fixture ref/tree/file changes occur. Ownerless old mappings stay unclaimed.

## Commands

- `cargo test -p reposync-core --features reliability-fixture --locked --lib candidate_authority -- --nocapture --test-threads=1`: 3/0/0 locally.
- With the freshly archived original generator supplied as `REPOSYNC_OLD_GENERATOR`, `cargo test -p reposync-core --features reliability-fixture --locked --test copy_migration -- --nocapture --test-threads=1`: 15/0/0 locally.
- `cargo test -p reposync-core --locked --test startup_schema -- --nocapture`: default-feature startup 1/0/0 locally.
- `scripts/reliability-container.sh --all`: isolated required 70 cases.
- `scripts/reliability-compare.sh`: original base and retained f74 anchor, identical locked method/graph/GOAL; known failures/ignore retained.
- `python3 scripts/reliability_scan.py --scan artifacts --status-file artifacts/evidence-scan-status.json`: complete publication barrier. Downloaded artifact is independently rescanned and ZIP hash recomputed.

## Remaining qualification gaps and smallest next slice

Deployed versions/installations, writer fencing and backup/restore ownership, arbitrary historical copy ancestry and policies, inherited/rotated/encrypted credential installations, operational NULL/API reader activation, daemon/installer/scheduler v13/v14 activation, remote production lineage, live acceptance, down migration and general #64 external-effect recovery are NOT QUALIFIED. Current remote lineage proof applies only to enrolled disposable fixtures. No merge/deploy/release/force push occurred.

After independent review, the smallest proposed implementation slice is the #54 typed read adapters on migrated copies (mapping lookup/list/status and emitted-hash behavior) with strict nullable/ownerless cases, while keeping normal startup at12. Additional historical migration shapes and production activation require their own reviewed boundary. Stop here.
