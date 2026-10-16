# Phase 0 historical build restoration · #211

Refs #211. Refs #62.

Implementation base: `a8f814aff7d87e5a3c3215c21b3d3eaf5186f2e5`.
This slice restores build/comparison evidence collection. The scenario catalog,
production crates, workflows, #195 acceptance lab and #204 work remain with
their existing owners. Deployed-version coverage is **NOT ESTABLISHED**.

## Reviewed dependency inputs

`fixtures/baseline-locks.json` binds every historical source to its source tree,
complete workspace manifest inventory, manifest hashes and lock hash. No libc
manifest overlay remains. Three sources have identical workspace dependency
graphs and therefore identical lock bytes; each has an explicit mapping/file.

| Role | Source SHA | Lock SHA-256 |
| --- | --- | --- |
| Original base and archived generator | `87379741779a6259f7eeb52a68cc6f061174e5ef` | `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05` |
| Previous | `f74fce855a1f1d80dd631397f436ba33906272e6` | `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05` |
| Immediate reviewed | `ab097580d9415ae2f8df44bed4041f740a1b38f2` | `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05` |
| Merged start | `54e183484961bd9ad12bd03505b3b00610de7cd9` | `eca9dfdc221c31be47daad874924ce09dd621ed686ecdad78426458b482591c9` |
| Candidate | exact execution source recorded at runtime | `295583faf31523be3fb43d729e1777d45751cda900439f6ae6b0e682fe6605c6` |

Locks were derived from the retained candidate lock with
`cargo update --offline --workspace` in exact source archives. No third-party
package was upgraded. Historical graphs remove current workspace dependency
edges and two unused registry packages (`roxmltree` and `wasm-streams`).
Every historical registry package/version/source/checksum must remain present
in the candidate lock. This preserves shared dependency versions; it does not
claim identical feature graphs or dependency edges.

All Cargo compilation remains `--locked`. Missing source mappings, manifest
drift, mismatched lock bytes and mismatched execution provenance fail loudly.
The runtime's existing `old-generator-provenance.json.lock_sha256` field still
reads the candidate environment hash. For this bounded change the authoritative
historical lock receipt is `old-generator-build-lock.json`, and the compiled
generator package also retains `lock.sha256`. The old field must not be used as
the historical dependency identity; correcting that runtime field is a separate
qualification-runner change.

## Execution and failure collection

`reliability-compare.sh` attempts setup for all four historical sources plus the
candidate, then emits all four comparison reports before aggregating failure.
An archive, lock-staging or harness-overlay error records that source's setup
failure and leaves its execution NOT_RUN; later sources are still attempted.
Reports retain both actual lock hashes. Exact old passing test identities must
remain passing; missing/renamed identities remain blockers. Candidate failures,
including candidate-only failures, also fail the aggregate. This does not supply
the separately owned reviewed assertion-lineage mapping.

If diagnostic setup, build or runtime fails, an EXIT handler collects comparisons
before returning the original failure. This is necessary because the unchanged workflow
skips subsequent ordinary steps after a failure. Artifact directories and build
lock receipts exist before compilation, allowing the existing always-run scan
and artifact publisher to retain failure evidence.

BuildKit target cache identities are isolated per exact source/tree, prepared
context content hash (including the test adapter), selected lock, Dockerfile
recipe and build mode, with an explicit Rust 1.98.1 image tag.
The archived generator uses a separate namespace/key bound to its original
source, historical lock, adapter and recipe. Changing candidate source changes
its target cache; baselines cannot see a different source's compiled outputs.
Only checksum-verified registry downloads are shared. Actual compiler/Cargo
versions remain in each runtime's tool-version receipt. `build-identity.json`
records cache/input identities and `image-id.txt` records the built image.
The profile uses no incremental compilation or
debug symbols, with ordinary unoptimized test assertions still enabled. This
limits hosted disk consumption across the five isolated builds.

Evidence retention requires a writable artifact root and temporary storage,
and the driver must remain running. Runner cancellation, resource exhaustion or
an unavailable global tool can still leave partial evidence; they cannot count
as complete execution/publication.

## Local validation

- `python3 scripts/reliability-lock-overlay.py self-test`: PASS. Negative controls
  reject omitted mappings, substituted lock bytes, unknown sources, missing
  manifests, incorrect execution lock identity, omitted old tests and newly
  failing candidate-only tests; a valid comparison passes.
- Each exact historical archive: pinned staging and
  `cargo metadata --offline --locked --format-version 1 --no-deps`: PASS, with
  unchanged lock hash. Metadata validation is not compilation proof.
- Persisted driver controls in the same self-test inject build, archive and
  lock-staging failures, plus wrapper archive/staging/context-copy failures.
  Later sources are attempted, all five source exit receipts and all four
  explicit NOT_RUN reports survive, and original/aggregate failures stay nonzero.
  The build-only fault case reaches all five stub builds. Cache controls prove
  five distinct source keys and a changed candidate key when only its declared
  source changes; Docker mount declarations use those keys and a separate old
  generator namespace. No Docker container or Rust compiler runs in these controls.
- Existing acceptance matrix self-test/check: PASS. Catalog remains 1 PASS,
  21 PARTIAL and 2 NOT RUN; these classifications are not execution receipts.
- Shell syntax and `git diff --check`: PASS.

## Hosted qualification

Pending exact final-source execution and independent source review. The previous
run `38885178085` stopped in the historical generator's locked build; diagnostics
and comparisons were NOT_RUN. Its result is retained as the before evidence.
No full qualification, release or broader issue completion follows from this
build restoration.
