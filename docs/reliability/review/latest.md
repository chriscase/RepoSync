# PR #72 review 5: receipt admission and read-safe inventory handoff

**STATUS: AWAITING_FINAL_CI for the bounded I01/I02 and inventory slice.** [Draft PR #72](https://github.com/chriscase/RepoSync/pull/72) remains on `feature/reposync-reliability`. [Review #5308189300](https://github.com/chriscase/RepoSync/pull/72#pullrequestreview-5308189300) accepted the five H01/H02 cases as bounded progress and requested I01/I02 corrections. The [previous handoff](review-4-handoff.md) retains F/G/H details. The [new brief](pr72-receipt-inventory-goal.md) is copied byte for byte. No issue acceptance criteria, schema, general operation service, or live installation changed.

| Identity | Exact value |
| --- | --- |
| Original base / previous comparison anchor / reviewed start | `87379741779a6259f7eeb52a68cc6f061174e5ef` / `f74fce855a1f1d80dd631397f436ba33906272e6` / `afd3cd6f63078ab97609cde732880afc34394405` |
| Reproduction / receipt correction / inventory / comparison-context commits | `573e678284b1b119c411695fd52e4eb5bab851cf` / `516c5dd231768d67960ab9f7a8cc64a8f37846c3` / `0c7b9e2912b2f554999195a59434d144d33e3c06` / `838c3ff437293e86777e2b792a056e6db15b3271` |
| Runtime functional head / comparison harness head | `0c7b9e2912b2f554999195a59434d144d33e3c06` / `838c3ff437293e86777e2b792a056e6db15b3271` |
| GOAL / previous brief / new brief / lock SHA-256 | `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8` / `ad1ce4d538cf0906e72c8bfe02df9482cdcf3c2fe5e08ff8dc6d9a5106ddfbd9` / `0c2ae50b4af1b45a83695cd6bff35d0c73a7ee6f9b7e2a92846af695ba75bef2` / `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05` |
| Old fixture provenance | Original production source `8737974`, unchanged import/completion code, test-only driver `scripts/legacy_import_generator.rs` SHA-256 `9a2485558f3f70677014007a3d45930eb2739c8d8b64aa854704bd681d9bad6c` |

## Findings and exact cases

| Finding | Exact case and bounded result |
| --- | --- |
| I01 equal copies | `R10_POLICY_EQUAL_CURSOR`: genuinely filtered nonempty G under X, G/G copies, policy Y rejects before remote write. |
| I01 split copies | `R10_POLICY_SPLIT_CURSOR`: SVN publication splits column/KV; changing policy still rejects before affected replay. |
| I01 scoped KV only | `R10_POLICY_KV_ONLY`: receipt and active policy checked with absent column; no global-tip inference. |
| I01 controls | Matching policy permits ordinary subsequent sync; malformed/stale receipt cannot pass by equal SHA. Existing empty, applied, old import, pending-direction, restart and retention controls remain required. |
| I02 read failure | `R01_NO_TARGET_READ_FAILURE`: required blob read fault stops at G, with queued successor pending; no receipt, cursor or SVN mapping fabricated; retry applies once. |
| I02 stage failure | `R01_NO_TARGET_STAGE_FAILURE`: required SVN add fault leaves unversioned content and stops at G; successor pending; retry applies once. |
| I02 verified no delta | `R01_VERIFIED_NO_DELTA`: nonempty source whose exact target bytes already exist gets pinned SVN UUID/URL/revision/path-hash proof and a version-2 receipt. An old version-1 `no_svn_delta` remains unchanged and blocks as unverified. Genuine empty/filtered outcomes keep separate version-1 semantics. |
| Old topology | `R10_PINNED_OLD_TOPOLOGY_INVENTORY`: old production import route creates two independent SVN r2/Git pairs and one disabled repository; copy after writer exit, seal, inventory twice with identical report and input hashes/modes. Synthetic pruned, missing-receipt, unverified-v1, unknown-effect, WAL and future-schema variants have explicit expected classifications/refusals. |

The exact-case manifest now requires **48** cases: the prior **41** plus these **7**. Required-case discovery treats missing, skipped and failed cases as failure. The first [isolated run](https://github.com/chriscase/RepoSync/actions/runs/36043566401) completed diagnostics but failed comparison packaging because the archived base contexts lacked the new inventory reader; commit `838c3ff` supplies it to both contexts. [Broader E2E](https://github.com/chriscase/RepoSync/actions/runs/36043566332) succeeded at the functional head; [general CI](https://github.com/chriscase/RepoSync/actions/runs/36043566405) retains a repository-wide formatting failure and is not represented as green. The final documentation-head isolated run, exact counts, matched comparison, scanner, artifact hashes and merge-tree identity are posted on PR #72 after CI completes.

## Inventory and migration boundary

The documented [read-safe inventory command](../inventory.md) reads a sealed, quiesced **fixture copy** using SQLite `mode=ro&immutable=1`, rejects incomplete/WAL/future-schema input, and reports sanitized repository-scoped ownership, cursors, mappings, receipts and missing proof. The unchanged old generator's two imported repositories classify `qualified_fixture_shape`; the disabled third is `not_qualified`. Synthetic degraded overlays classify `needs_reconciliation` or `external_effect_unknown`. This is not deployed-version eligibility: remote SVN UUID/copy ancestry and Git identity remain unknown without separate proof. The inventory performs no Git/SVN network operation or index refresh.

The [single design contract](../design.md) supersedes first-retained-row checkpoint inference, specifies receipt checks for equal/split/missing copies, separates permanent applied-mapping proof from expiring diagnostics, and aligns future #54 nullable outcomes with the actual embedded migration runner. It also states a concrete #64 post-publication/lost-reply recovery rule. All migration DDL and general operation-service work remain future work. The smallest proposed next slice is a separately reviewed, #54-aligned transactional #63 generation/ownership migration proposal after additional historical/deployed-version inventory. No reset, reimport recovery, merge, release, deployment or live acceptance is authorized by this fixture result.
