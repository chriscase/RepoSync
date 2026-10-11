# Update pair from parent: bounded execution contract

Refs #69. #69 stays open.

This contract describes the next execution policy. Implementation is in progress.
The existing v1 preview and its execution refusal remain active. Standalone
publication, lineage and Git preparation components do not authorize execution.
The endpoint, durable operation, SVN merge, recovery and real-engine evidence
below must be implemented and independently reviewed before this policy is
enabled. The reviewed durable identity dependency must land on main first.

## Supported operation boundary

| Case | Execution policy | Required evidence |
| --- | --- | --- |
| Clean child, synchronized parent advanced linearly | Planned first supported case | Verified mappings, corresponding baseline trees, complete Git graph, continuous SVN lineage, supported properties, durable identities and exclusive holds |
| Previous completed refresh, next linear parent advance | Planned first supported case | Prior durable receipt plus the same current-state proofs |
| Repeat completed request | Planned receipt lookup | Same request ID and original operation, no external retry |
| Repeat unchanged parent | Planned no-op | Fresh pins match the completed receipt |
| Child or parent has unsynchronized work | Refused | Preserve and report pending work on both sides |
| Child content changed independently | Refused | A later conflict-aware policy requires new approval |
| Parent advanced through a merge commit | Refused | Nonlinear ancestry requires a later policy |
| Unknown, rewritten, replaced or renamed lineage | Refused | Equivalent-looking content is insufficient |
| LFS, active Git attribute rules, symlinks or submodules | Refused | Preparation and publication do not run LFS hooks or map these semantics |
| Unsupported SVN property, projection or tree conflict | Refused | Git file equality alone does not prove SVN correspondence |
| Uncertain partial publication | Held for reconciliation | Inspect durable evidence; never blindly retry or release the hold |
| Re-anchor, recreate, retirement or same-name generation reuse | Not implemented | Separate design and approval required |

The first case supports regular file bytes, executable bits and directories
needed by those files. Corresponding Git and SVN histories can have different
graph shapes. Existing published commits and revisions remain intact.

## Approval and admission

Add an explicitly versioned execution-preview contract. A v1 compatibility
preview must never become an executable approval. The new approved digest binds:

- Child and parent repository IDs, durable UUIDs and generations, effective
  configuration and credential revocation state. Echo generations are separate
  counters and do not replace durable registration identity.
- Operation and publication policies, both Git endpoints/branches/tips, complete
  ancestry and verified mapping evidence.
- SVN repository root/client URLs, UUID, child/parent paths, pegged revisions,
  child mapping revision, parent baseline revision, child copy origin and parent
  path continuity through the source tip.
- Pending work on both sides, conflicts, supported tree/property evidence and
  the intended new result.

Execution requires the digest and an idempotent request ID. Reconstruct fresh
observations, resolve explicit credential revocation and reject any mismatch
before durable intent or external publication. Recheck the inputs immediately
before each write. Historical peg proofs do not establish live freshness.

Acquire process busy guards in deterministic order and atomically create
durable holds for both registered UUIDs under the current daemon writer lease.
Reject competing import, Git push, SVN commit, team cycle, late-pair publication,
managed removal or another refresh. The lease epoch alone does not exclude
concurrent jobs in the same process.

Every writer entrance must honor the holds, including scheduling/recovery,
direct SyncEngine calls, configuration and credential mutation, conflict
resolution and skip-commit. A global engine without a repository ID refuses
while any refresh hold exists. Unknown or corrupt hold data fails closed.
Process exit, a failed HTTP request or a dropped busy guard does not release an
uncertain durable hold.

## Preparation and correspondence

Use operation-specific directories for Git and SVN. The child must be unchanged
since its verified mapping or completed refresh. The mapped parent baseline and
current parent must be synchronized and proved, including supported properties.
Peg all reads; walk full copy history without a fixed revision window. Prove
the child's direct copy origin and the parent's continuous incarnation from the
copy source through the mapped baseline and current source revision. Reject
replacement of either path or an enclosing directory in the relevant interval.

Prepare an unreferenced Git commit with exactly two parents: the pinned child
tip first, pinned parent tip second. Its tree is exactly the verified parent
tree for this clean-child case. Its operation marker and signature timestamp are
pinned so the same preparation inputs reproduce the same SHA. Preparation
updates no HEAD or branch. Refuse shallow boundaries, grafts, replace refs and
linked worktrees; the operation owns its repository.
Scan incoming parent history and unchanged input trees for unsupported content.

Prepare a normal SVN merge over `(parent_baseline_revision, parent_revision]`
from the pinned parent peg. Do not export-copy content over the child. Verify
full file bytes, executable modes and supported properties independently.
Verify `svn:mergeinfo` against the exact parent path/range plus any earlier range
proved by the previous completed receipt. Missing, partial, conflicting or
unrelated mergeinfo is refused. Unsupported properties cannot be hidden by an
equal Git tree or omitted from the evidence.

## Publication and recovery

Persist the operation, initiator, request ID, all input pins, prepared Git
SHA/tree, expected SVN result and next intent before the first external write.
Publish SVN first with the operation marker in the commit message itself.
Recover the actual child-path revision by marker, affected path, ancestry and
tree/property/mergeinfo evidence. Repository HEAD and `previous revision + 1`
are insufficient. Duplicate or ambiguous marker matches retain a hold.

Publish Git only after the SVN result is verified and recorded. Use one normal
refspec for the prepared SHA and child branch. The negotiation callback requires
the exact advertised old tip, prepared new tip and destination ref. Receive-pack
must reject a move after advertisement. Require the accepted update receipt and
fresh remote result. No force update, rebase or remote reset is a recovery tool.

The original Git-only primitive remains Git-only. An approval must explicitly
select `smart_http_v1` for HTTPS. Cleartext HTTP is refused. The approved URL must
already be canonical, with no empty or dot path segments. Effective fetch/push
URL rewrites are refused before connecting. HTTPS uses a fixed upstream endpoint
through an operation-local
loopback bridge because shipping libgit2's redirect setting permits some
same-host redirects. The upstream client rejects every redirect, ignores
inherited proxy configuration and performs
ordinary certificate and hostname verification. Only that pinned client receives
the explicitly resolved HTTP token; libgit2 receives no credential helper, agent
or token fallback. SSH, file, local and scp transports remain unsupported.

Any error after publication negotiation is an uncertain remote outcome. A
verification redirect after a successful write also leaves an uncertain result.
Recovery first observes the remote and compares it with durable evidence. It
does not repeat a write based only on a missing receipt. External drift after
SVN publication retains a reconciliation-required hold and reports the partial
SVN result. Do not claim rollback across SVN, Git and SQLite.

After both external results are verified, one SQLite transaction records an
applied child `git_to_svn` mapping for the prepared Git SHA and observed SVN
revision, writes both column and key/value child Git/SVN cursors, completes the
operation and releases both holds. Parent cursors stay unchanged. A successful
refresh does not bump echo generation. Ordinary child SyncEngine must suppress
the emitted SVN merge and inherit the parent work without duplicate replay.

## Evidence before enabling execution

Use disposable real Git and svnadmin repositories initialized and synchronized
through production SyncEngine. Prove preserved histories, mergeinfo, both Git
parents, corresponding final trees, all checkpoint forms and a no-op ordinary
child cycle. Prove repeated requests and the next parent advance.

Exercise stale digests, every input drift, rewritten/unsupported/conflicted
inputs, credential revocation, UUID/generation drift, competing writers,
protected refs and SVN permission failures. Inject restart at intent, SVN write
before receipt, SVN receipt before Git, Git write before receipt and checkpoint
before response. Verify exactly one logical integration or an explicit retained
reconciliation hold. Keep existing test oracles unchanged.

Require exact-head Build & Test and End-to-End gates, independent Grok Build
adversarial review and Chris's merge authorization. The implementation writer
does not review or merge its own PR.
