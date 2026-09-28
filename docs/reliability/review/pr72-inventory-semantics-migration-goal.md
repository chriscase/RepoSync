# RepoSync PR #72 — complete safe inventory, qualify no-target semantics, specify migration

## /goal

Continue `chriscase/RepoSync`, branch `feature/reposync-reliability`, draft PR #72 from `649dad8272dedbe16592c183ba37066c01965bac`.

Read independent Review 5 (#5309937153) on PR72 and `RepoSync-PR72-review-5.md`. Retain the demonstrated I01 current-cursor policy checks and I02 read/staging failure fixes. Correct J01–J03, then deliver a concrete #54-aligned #63 migration proposal in the same pass. **No DDL execution, schema conversion or general #64 operation service.** No intervening planning-only review is needed before writing the proposal.

Original base: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
Locked graph SHA-256: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

Copy/hash this brief without altering prior goals or issue acceptance. Preserve accepted tests and source provenance. The posted review is the fallback when attachments are unavailable.

## A. J01: no-target proof must match the source semantics

The existing `R01_VERIFIED_NO_DELTA` positive changes only Git executable mode and compares bytes, then acknowledges it without an SVN revision. That is not proof of semantic equivalence under the current path-only policy.

1. Preserve the scenario as a regression, but correct its oracle. This review explicitly authorizes strengthening/reclassifying this one faulty success expectation; document old/new semantics and manifest mapping. Do not remove the case or hide it as ignored.
2. Inspect source before/after tree entries, not merely path content. For mode/type changes unsupported by this slice, return a truthful non-success/reconciliation outcome before advancing the cursor or recording verified no-target proof. Proving an appropriate SVN mapping is also acceptable, but do not expand into universal file-property support.
3. Do not silently create a new bytes-only default. Keep metadata loss distinct from intentional path filtering. Bind any permitted projection semantics to an explicit version and review boundary.
4. Add a genuine positive: a nonempty regular-file content delta whose intended target content is actually present in SVN at the pinned revision. Arrange that state through real fixture operations and deterministic coordination, not a manual checkpoint skip. A controlled intervening SVN change is a permissible test technique; it does not qualify general multiwriter publication.
5. Add a mismatch negative, with a queued successor. A metadata/content mismatch must leave the offending work pending and cannot mint a verified no-target receipt. Keep read/stage-failure retry controls and same-policy controls.
6. Distinguish byte manifests from semantic manifests. Verify Git modes/types and relevant SVN properties, or explicitly report their lack of qualification. Existing v2 byte-only receipts are not automatically canonical-ready; document conservative preservation/classification rather than relabeling them.

Suggested added case purposes: unsupported executable-mode safe refusal, genuine nonempty already-represented content, and mismatched target refusal. Use stable IDs and retain the old mode case history.

## B. J02: complete the inventory's authority inputs

Extend the read-safe command, not operational startup.

- Enumerate every actual checkpoint store used by supported production paths: repository columns; scoped incoming SVN and outgoing Git KV; legacy global incoming/outgoing keys; import watermarks; relevant commit-map/sync-record evidence; and import progress. Identify source and ownership independently of the chosen authority.
- Vary each source independently on explicitly labeled copies. Include column SVN=2 / scoped SVN=999 / global SVN=888 / import watermark=777. The report must expose those values, their disagreement and proposed disposition. A benign stale value requires an explicit reason; neither maximum-value selection nor silent omission is acceptable.
- Include input schema columns/indexes/foreign-key validity and unsupported shapes to the extent required by the proposed migration. Do not mutate schema or normalize values while reading.
- Identify actual local credential presence and parent/global inheritance references without outputting values or low-entropy secret hashes. Unknown inheritance stays UNKNOWN. Add a small parent/child or unsupported-inheritance classification control rather than calling independent roots an inheritance test.
- Preserve disabled state. Do not automatically reactivate disabled entries or borrow another repository's revisions/SHAs. Keep remote UUID/ref/copy proof UNKNOWN unless supplied by separately qualified evidence.
- Retain consistent-copy seals, no WAL-dropping, no migration/maintenance, no network/Git subprocesses, deterministic output and content/mode preservation.

The review's exact-script probes use synthetic schema-shaped inputs, not unchanged-old-code outputs. In implementing evidence, retain the existing pinned driver and label every degraded overlay. Save sanitized full inventory reports as artifacts, not only counts/classification snippets.

## C. J03: every inventory read stays inside the sealed input

Treat database/config path fields as untrusted data even when the surrounding copy is quiescent.

- Validate repository IDs and Git ref names before constructing paths. Reject absolute/traversal inputs without opening them.
- Establish a single confined read helper or equally auditable boundary. Ensure ancestors cannot escape through symlinks; final-component checks alone are insufficient.
- Read only files covered by the seal. Treat linked worktrees or unsupported ref storage as unknown/refused. Add bounded packed-ref support only with corresponding confinement/read-only tests; do not call mutable Git commands to fill the gap.
- Use synthetic external canaries and an audited reader/OS trace to prove no outside open, not merely unchanged outside content. Test absolute branch, traversal branch, invalid repository ID and symlink-ancestor forms, plus normal safe controls.
- Keep private manifests private. Redact refusal output as well as successful output. Do not print a malformed path's secret-bearing content while reporting an error.

## D. Deliver the concrete migration proposal now, without applying it

After the corrected classifications are available, write the next #54/#63 proposal with candidate SQL in documentation only:

- Tables/columns, keys, CHECK/FK/uniqueness constraints and schema/version strategy for `(repo_id, generation)`, baseline ownership, handled/emitted frontiers and typed applied/no-target/unresolved outcomes.
- Exact mapping rules from each inventoried legacy shape; preservation of original rows/IDs/settings, disabled state, credential ownership and recovery evidence. Uncertain records remain preserved and unqualified; never infer source origin from a title/message or equal bytes.
- Coordination with #54 nullable `commit_map.git_sha` and all affected readers. Use the actual embedded migration runner, not a fictional migrations directory. Do not rewrite issue acceptance without review.
- Transactional migration plus `user_version` transition; single-writer and newer-schema refusal policy; interruption/repeat behavior and table-rebuild preservation checks.
- Testable before/after manifests for each pinned fixture, including incoming cursor disagreements, historical filtered decisions behind current cursors, old v1/v2 receipts, already-pruned proof and separate repo identities with equal revision numbers.
- Scope permanent authority separately from expiring diagnostics; storage-growth and retention contract.
- Separate safe restoration before new external writes from #64 reconciliation after lost reply or DB-after-publish failure. General recovery remains design-only.

Missing deployed-version information is not a reason to stall fixture work. Leave it NOT ESTABLISHED and clearly separate that future gate. The review is authorizing a concrete proposal, not its execution.

## Validation, sequencing, and handoff

Use separate ordinary commits for no-target semantics, inventory fixes/tests and proposal. Stay on the same draft PR. No force push, merge, production access, acceptance, reset/reimport recovery or deployment.

Use targeted tests while iterating; one final strict-case suite and matched comparisons at the functional head. Preserve the existing 48 cases except the explicitly strengthened/reclassified mode-only oracle, and add new exact cases. Keep known failure/ignored outcomes visible. Identify the original base, retained `f74fce8` comparison anchor and immediate prior review separately; do not call the retained anchor the immediate predecessor.

Run scans and publish downloadable sanitized artifacts. Keep CI head/tree/binary/lock identities distinct. Note where tests ran; do not repair Docker storage destructively or repeatedly run unchanged expensive checks for status updates.

Return a compact delta handoff: J01 outcome/oracle change and true positive; J02 complete cursor inventory and classifications; J03 no-outside-open evidence; pinned fixture provenance; retained/new exact cases; comparisons with accurate anchors; scan/hash results; concrete migration proposal and unresolved decisions; functional/final/CI SHAs; deployed version NOT ESTABLISHED; DDL applied NONE. Stop for independent review.
