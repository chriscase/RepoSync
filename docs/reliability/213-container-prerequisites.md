# Phase 0 candidate-suite container prerequisites

Refs #213. Refs #62.

Implementation base: `7d4c8f81a6faeb664956bbe4c56c4a0da2fb1c65`.
This reservation covers the container recipe, reliability scripts and fixture
receipts only. Production crates, test assertions, workflows, DB helpers,
#195 and #204 remain with their existing owners.

The retained before artifact is run `38899116945`, artifact `12047251189`, ZIP
SHA-256 `b6d63be5ab01dba2403ee02897b9c93fc20ae38b73959b14ecacd582b0e3d45c`.
Its candidate catalog contains 2,263 exact identities: 2,088 passed, 173 failed,
2 ignored. The two core aliases account for 701 duplicated catalog identities;
these counts must not be presented as unique Rust tests. Every original failed
name and its observed prerequisite classification is preserved in
[fixtures/213-before-failures.json](fixtures/213-before-failures.json).

| Observed before group | Catalog entries | Container input |
| --- | ---: | --- |
| Missing pair-refresh HTTP server script | 154 (77 per core alias) | Existing `pair_refresh_http_server.py`, exact compile-time path |
| Native owner capture unavailable | 2 (1 per core alias) | Synthetic image-only machine ID; real Linux boot/PID namespace and process observations |
| HTTP authentication helpers and application paths | 9 | Existing `git_http_auth_server.py`, empty CLI/Personal path ancestors, two compiled applications |
| Git author unavailable | 1 | Synthetic author/committer name and `example.invalid` email inside the image |
| Empty SVN author leading to empty import signature | 1 | Non-login name for the existing numeric UID/GID 65532 inside the image |
| Unresolved hook/wrapper assertions | 6 | No assertion or product repair; owned execution probe records evidence |

The image copies only the two explicit Python fixtures, never their adjacent
Rust helper modules or a source checkout. Historical contexts retain their own
fixture bytes when present. Missing scripts are supplied from this reviewed
harness, with origin and digest in `fixture-inputs.json` before the prepared
context is hashed. No archived Rust source or Cargo manifest is changed.

OpenSSL is an explicit runtime package because the existing HTTPS fixture tests
invoke its executable to generate synthetic certificates. It was previously
available transitively through `ca-certificates`; this makes the prerequisite
explicit rather than claiming a newly observed OpenSSL failure.

The build explicitly compiles ordinary applications before test preparation,
using the same locked graph and feature selection. The final Cargo test artifact
at the same application path wins if test feature unification rebuilt it.
`runtime-inputs.json` binds both application bytes and features to their actual
artifact paths, plus both script digests. Test executables retain the usual
`deps/` layout so existing Personal helpers find their sibling application.
Exact aliases at `/src/target/debug/{reposync,reposync-personal}` satisfy the
existing compile-time `CARGO_BIN_EXE_*` paths. The archived generator's separate
flat packaging remains unchanged. Runtime guards check the precise directory,
script, application and alias allowlists and their digests.

The synthetic passwd/group entries name the existing container UID/GID without
changing it. Git takes its default name from that synthetic account and its
fallback email from `EMAIL`; explicit test Git configuration and author variables
retain precedence. The owned probe checks both default and explicit identities.
The machine ID is a constant synthetic image input, owned by root
with ordinary image creation mode. Native boot, namespace and PID death checks
remain real. No host identity is copied, no credential is introduced, and no
mount flag, capability, numeric user or host access policy changes.

`runtime-prerequisites.json` records tools, input digests, numeric/named identity,
hashed machine/boot identities and actual fixture mount flags. An owned script
runs directly, through PATH, and through an explicit shell control. An owned
bare Git repository has a rejecting pre-receive hook and marker; its push result
and marker are retained. Probe errors remain explicit errors. A failed direct
execution/hook probe does not turn any test PASS or prevent the complete catalog
from reporting its six unresolved assertions. The repository already documents
fixture tmpfs execution restrictions in its SVN subprocess fixture adapter;
these are a hypothesis for the six failures until hosted receipts establish it.

Lightweight local checks (no Rust compilation or Docker build):

- `python3 scripts/reliability-prep.py --self-test`: packaging, missing application,
  ambiguous application path, absent/symlink script, legacy flat compatibility,
  historical script preservation and explicit missing-script fallback.
- `python3 scripts/reliability-runtime.py --self-test`: existing authoritative
  parent parser plus missing/altered fixture, altered application, extra Rust
  content and escaped/symlink ancestor controls.
- Existing lock/driver stub controls, acceptance-matrix controls, shell syntax
  and whitespace checks remain required. These local controls are owner evidence,
  not a replacement for hosted qualification or independent review.

Qualification requires the exact final head's hosted diagnostics, full populated
candidate catalog, four actual historical comparison reports and scanned
published artifact. The before/after ledger must preserve all 173 original names
and enumerate new failures and missing identities. All six unresolved assertions
remain FAIL without actual evidence. Historical MISSING identities remain
blockers: this slice neither maps nor waives them. Assertion lineage, the 1,001
commit scale requirement and live refusal coverage remain separate work.

## Required source-review corrections

The initial source review at `2388c7c` was NOT-ACCEPTED. Its two required findings
are addressed in this slice; a new exact-head review remains required.

Prerequisite receipt creation now precedes integrity checks and observations.
The numeric UID/GID, declared fixture paths, package/fixture allowlists and
hashes, Docker-socket refusal, host/private-network boundaries, cwd and owned
home/tmp requirements remain necessary integrity stops. A refusal persists
`integrity: FAIL` and its exception before propagating; no case catalog is
invented for an integrity refusal. Named-account/native-identity availability,
tool availability and owned shell/Git setup are observations. Their errors and
partial steps are retained, later probes still run, and actual tests retain
responsibility for their results. In particular a failed shell/Git setup probe
cannot suppress the six unresolved assertions. New controls inject native,
tool, shell and Git setup errors, verify durable partial errors and later probe
execution, and then prove the same three-entry synthetic process catalog is still
accounted. This is owner control evidence, with no Rust compilation or execution. A separate control proves an integrity refusal still stops and is
recorded. Evidence persistence requires the existing writable evidence mount.

All four actual historical refs lack **both** scripts:
`87379741779a6259f7eeb52a68cc6f061174e5ef`,
`f74fce855a1f1d80dd631397f436ba33906272e6`,
`ab097580d9415ae2f8df44bed4041f740a1b38f2`, and
`54e183484961bd9ad12bd03505b3b00610de7cd9`.
For each, both inputs therefore have origin `reviewed_harness_fallback`.
The “preserve existing historical bytes” branch is exercised only by a synthetic
local control in this cohort; no historical execution of that branch is claimed.

The fallback anchor is the previously qualified base main
`7d4c8f81a6faeb664956bbe4c56c4a0da2fb1c65`, tree
`0d9d37853e0df188bb5ec84752d0c78adda46caf`. These blobs are unchanged by this PR:

| Fixture | Reviewed Git blob SHA-1 | Expected content SHA-256 |
| --- | --- | --- |
| `git_http_auth_server.py` | `3b856fdd546e005613eb73e7e60d2a9470ec224b` | `ed5eed6797a2ca2b35cac5645293a44e90d1f77e409b95353c1465ae15083f33` |
| `pair_refresh_http_server.py` | `e690a127cd40b5172ef9ea86d5e4379a6576fc6e` | `5417a82fa1e72a65b5e43d74aaca45cf61ba471a10583dfe17c15d5d94521bd9` |

The prep script pins these expected identities. Fallback bytes must match both
before copying. The complete expected/actual origin and blob receipt is retained
on the host and in the prepared build context; packaging rechecks it against the
same pinned anchor and actual files. Runtime validation rechecks the staged and
packaged content identities. Controls reject changed fallback bytes, changed
staged bytes and an unreviewed anchor. Application digests identify the actual
compiled executable bytes, with Cargo feature metadata, hosted checkout/tree,
lock, recipe, cache and toolchain receipts providing their build provenance;
a generated binary digest alone is not source attestation.

The setup-wizard assertion exercises a PATH wrapper and reset/reimport flow,
including SVN/import setup. It is not classified as a proven Git-hook failure.
The other five assertions use rejection/barrier hooks. All six remain unresolved
FAILs in the before ledger until actual container execution explains them.
