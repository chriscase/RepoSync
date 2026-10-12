# Update pair from parent: bounded execution contract

Refs #69. #69 stays open.

This contract describes the next execution policy. Implementation is in progress.
The existing v1 preview and its execution refusal remain active. Standalone
publication, lineage, Git and SVN preparation components do not authorize execution.
The endpoint, complete writer exclusion, SVN publication, recovery and real-engine
evidence below must be implemented and independently reviewed before this policy
is enabled. Reviewed durable identity landed on main at af394285869bdffa5f156c651cd1104027832749;
the writer branch integrates it through a main merge.

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

The first case supports regular file bytes, executable bits, the fixed binary
`svn:mime-type=application/octet-stream` property and directories needed by those
files. SVN-only empty directories, inherited properties and other property values
are refused. Corresponding Git and SVN histories can have different
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

The standalone journal now enrolls both durable scope holds and an initiator/request
identity in one transaction under an enrolled current writer lease. Admission
requires a capability owning both process busy slots; acquisition sorts repository
IDs and releases partial acquisition on contention. Registration snapshots bind
the UUID, configuration, echo counter, column checkpoints and both scoped cursor
copies. Missing identities are refused rather than synthesized. Checkpoint
copies must be present and equal to their column values; split or
missing copies refuse rather than being collapsed by completion. Copy-origin
fields must match the pinned lineage and the root mergeinfo property must equal
the approved mergeinfo string.
The first baseline must be the original copy source. A later baseline requires
one completed refresh whose exact child SHA and SVN receipt revision match the
current child checkpoints. Its parent SHA/revision, resulting tree, mergeinfo,
copy origin, registration/configuration/echo identities and transport endpoints
must match the new baseline. Caller-supplied prior mergeinfo is insufficient.
The complete stored journal validates this provenance chain on every read;
missing or ambiguous earlier receipts refuse. This storage evidence does not
replace fresh remote observation or ordinary-engine proof of the next advance.
Omitting prior mergeinfo cannot return to the original copy baseline once a
completed receipt matches either child checkpoint. Both checkpoints must match
one exact receipt and every prior-baseline binding must agree. Duplicate completed
receipts for one child SHA/revision refuse on read even without a later job.
All competing active pointers refuse admission; a latest operation without an active pointer
must still deserialize completely and report completed state. Missing, foreign,
corrupt or uncertain prior documents refuse.

Publication phases are prepared, SVN intent, SVN observed, Git intent and Git
observed. Intent cannot be repeated as a retry. Receipt shape must match the
stored operation identity, result and revision evidence. An uncertainty diagnostic
preserves the phase and both holds; an independently observed exact receipt may
advance its pending intent phase without repeating a write. Drift or invalid
proof preserves uncertainty. Cancellation is permitted only before any publication
intent and without uncertainty. Process guard drop and database reopen preserve
both durable holds. Unknown versions, duplicate ownership, orphan documents,
missing pointers or noncanonical documents fail closed even for unrelated writers.

The final storage transaction creates the applied child correspondence, updates
all child cursor copies, marks completion and releases both holds; a database
failure rolls back all these changes. Completed request lookup is idempotent.
Both `commit_map` and `sync_records` are written. Contradictory child mapping
rows with the result revision or SHA refuse; an exact prior child commit-map
row is preserved without another insert. Parent rows are not adopted.
Unscoped legacy rows with either result coordinate also refuse completion;
equivalent-looking legacy evidence is not assigned to the child. Stored lineage
documents require canonical root/path bindings, positive and consistent revision
ranges, and complete copy-origin evidence before any journal transition.
Parent state and both echo generations remain unchanged. Journal receipt storage
is not independent remote verification, API approval, or credential admission.
The existing import, Git push, SVN commit, team mapping, late-pair publication
and managed removal admission transactions now check these holds before mutation.
Their persisted resume/confirmation/finalization paths also check holds in the
same transaction; late-pair updates cover parent and named child scopes. Legacy
disable checks and updates in one transaction. Personal journals are treated as
global writer scopes, and corrupt refresh evidence blocks unrelated admission.
Two-connection import/refresh admission permits one owner; unrelated registered
imports remain available. These are journal boundaries, not complete external
writer exclusion. Scheduler/direct engine/import working-tree and publication
entrances, conflict/skip working-tree mutations, the authenticated
publication/recovery caller and real production SyncEngine proof remain
prerequisites. No execution endpoint is enabled by these components.

Typed registration, checkpoint, reimport reset, deletion, echo-generation and
credential mutations now check holds in the same SQLite transaction as the
write. Credential and registration changes also check ancestors of both held
registrations; global defaults and the encryption key are global mutations.
Unrelated configuration and credential changes and status/session persistence
remain available. Generic KV and raw SQL are low-level storage interfaces, not
publication capabilities; deliberate fault injection can still corrupt evidence
and current-pin checks refuse that drift. Full production entrances must retain
exclusion through local working-tree and external publication work before
execution is enabled.

Managed-removal completion, restore and deletion now check holds before creating
an encryption key or applying the deferred identity migration. Both actions are
inside the same immediate transaction as the requested mutation. Late-pair
updates also check the persisted operation owner and active-pointer owners;
rewriting incoming parent or child fields cannot conceal a held owner. Global
watermark and legacy-authority changes check all holds, including writes reached
through an unrelated non-team registration.

An ordinary SyncEngine cycle now admits a durable writer window before loading
credentials, migrating identity, or modifying a working tree. Refresh admission
and window admission exclude each other transactionally for either registered
identity; a global cycle excludes every refresh. A borrowed window spans the
whole cycle. The per-repository scheduler admits a shared window before credentials,
clone, HEAD or checkout preparation and moves that same non-cloneable capability
into the engine worker. The engine refuses a different scope or database,
including a copied journal, and refuses uncertain publication before loading
credentials. Normal completion releases it. Drop releases only preparation
owned by the same writer epoch; global push/commit intent is marked before the
command and is retained after an uncertain outcome. Retaining uncertain evidence
on drop is read-only and does not renew a lease. Abrupt process exit retains
every window. Unknown, corrupt or contradictory window documents refuse
admission. Managed publication still requires its existing journal in addition
to the window. Operator recovery of stale windows, other working-tree writer
entrances, authenticated execution and complete real-engine recovery evidence
remain unfinished. These conservative retained windows must not be cleared
merely because time passed or a new daemon acquired a lease.

Direct full and snapshot imports now admit a writer window before progress,
LFS or working-tree preparation. Managed publication retains its import journal;
legacy full import marks global publication uncertainty before invoking a push.
A failed legacy batch stops without a blind retry. Successful batches and the
final result require an independent exact-ref read before global checkpoints
or window completion. Both legacy checkpoint values are written atomically,
and a managed installation refuses those global values without a partial write.
Active Personal config journals, including legacy names, exclude refresh admission
before setup clone preparation. Scoped import-baseline backfill checks a hold
in its mutation transaction rather than repairing drift during a held refresh.
Automatic reconciliation also retains a window before local/credential observation
and mapping/checkpoint confirmation. These caller controls do not replace the
unfinished authenticated refresh execution and real-engine crash proof.

The latest corrections refuse known legacy checkpoint incompatibility before a
push. Each independently observed legacy batch and both checkpoints transition
atomically from possible publication back to preparation; later cancellation can
release preparation while retaining the last confirmed cursors. A failed batch
returns an error and retains uncertainty rather than claiming a managed
reconciliation operation exists. Personal initial import holds the same global
window across startup clone and both import modes, before LFS, GitHub API or
checkout work. Personal checkpoint transactions honor refresh holds. Empty
first-run storage may be initialized before admission; existing storage must
admit its window before initialization or conflict attribution. Scheduler
reconciliation acquires exclusion before constructing its credential-resolved
SVN client. These corrections are local and await independent review.

Strict refresh credential snapshots resolve the complete child/parent ancestry
and global defaults inside one read transaction, reject explicit revocation and
malformed identity/storage, and bind values and timestamps with a keyed digest.
They never create an encryption key or fall back to legacy/config credentials.
Credential-checked journal admission and continuation recheck this digest and
unresolved conflicts under the journal's transaction and busy capability. These
are caller primitives; authenticated API admission and remote observation remain
unfinished. Conflict insertion/upsert and exact-skip execution now honor holds;
the web skip caller carries one window from credential resolution into the core
operation. Historical Personal conflict labels use global exclusion.

Conflict resolution, deferred status and resolved-content storage now check the
conflict's persisted repository owner in the same transaction as the write.
NULL, empty or personal legacy attribution uses global exclusion; unrelated
registered conflicts remain available. A custom merge writes status, resolution
identity and content atomically, including the named production resolver.
Failure to store content rolls back the status transition. Other conflict
working-tree entrances and refresh admission's fresh conflict gate still need
full caller integration.

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

The standalone SVN preparation component checks the baseline, parent and child
at their approved pegs in separate operation directories, with externals ignored.
It compares every file and directory with pinned Git trees, performs a normal
range merge, checks status separately for text/property/tree conflicts and
compares the complete result property set with the parent plus the approved root
mergeinfo. It never commits a remote revision. Existing operation directories
cannot be overwritten. Its historical checks do not replace live drift checks,
verified mappings, receipt provenance, identity, revocation or exclusive holds.

## Publication and recovery

Persist the operation, initiator, request ID, all input pins, prepared Git
SHA/tree, expected SVN result and next intent before the first external write.
Publish SVN first with the operation marker in the commit message itself.
Recover the actual child-path revision by marker, affected path, ancestry and
tree/property/mergeinfo evidence. Repository HEAD and `previous revision + 1`
are insufficient. Duplicate or ambiguous marker matches retain a hold.

The standalone read-only observer now finds an exact, unique operation message
in complete child copy history. It requires exactly one child revision after
the approved input, an unchanged direct copy origin, root mergeinfo modification,
no changed paths outside the child, and only copy sources from the pinned parent.
It checks all result properties against the parent plus approved mergeinfo and
checks all file bytes, modes and directory topology in a fresh pegged checkout.
A final complete-history read refuses subsequent child work; unrelated repository
HEAD advances are allowed. Repeating observation performs no remote write.
`NotObserved` is an observation of missing evidence, never authorization to
repeat an uncertain commit. Journal provenance, holds and a publication caller
are still required before this observer can be part of enabled execution.

Publish Git only after the SVN result is verified and recorded. Use one normal
refspec for the prepared SHA and child branch. The negotiation callback requires
the exact advertised old tip, prepared new tip and destination ref. Receive-pack
must reject a move after advertisement. Require the accepted update receipt and
fresh remote result. No force update, rebase or remote reset is a recovery tool.

The original Git-only primitive remains Git-only. An approval must explicitly
select `smart_http_v1` for HTTPS. Cleartext HTTP is refused. The approved URL must
already be canonical, with nonempty ASCII repository path segments containing
only letters, digits, '.', '_' and '-'. Empty/dot segments and all percent
escapes are refused, including encoded separators and double encoding.
Effective fetch/push
URL rewrites are refused before connecting. HTTPS uses a fixed upstream endpoint
through an operation-local
loopback bridge because shipping libgit2's redirect setting permits some
same-host redirects. The upstream client rejects every redirect, ignores
inherited proxy configuration and performs
ordinary certificate and hostname verification. Only that pinned client receives
the explicitly resolved HTTP token; libgit2 receives no credential helper, agent
or token fallback. SSH, file, local and scp transports remain unsupported.

The standalone Git recovery observer now reads only the approved child ref
through those same pinned transport protections. It downloads operation-local
objects without changing local refs or invoking receive-pack. Exact prepared
SHA is observed publication evidence; the old input tip is only `NotObserved`,
not proof of a failed earlier attempt and never permission for a blind retry.
Missing, older, other-parent or later child tips refuse recovery. Identity,
revocation, corresponding SVN evidence, journal provenance and durable holds
remain requirements of the unfinished execution caller.

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

The registered-source prerequisite captures both direct-pair registrations,
established checkpoint copies and strict credential chains in one read
transaction. It refuses unsupported modes/projections/LFS, noncanonical or
credential-bearing URLs, shared branch endpoints, and incomplete SVN
credentials. The bounded Git transport requires distinct branches on one HTTPS
remote. Safe preview metadata contains a credential-context HMAC; credential
values are neither serialized nor included in Debug output.

Registered-source journal admission compares those actual endpoints, branches,
identities and configuration with the preparation in the same transaction
creating both holds. Its checked intent transitions additionally require the
current writer, both process busy slots, quiet ordinary windows/journals and
conflicts, and current strict credentials. A repeated intent refuses rather
than authorizing publication again. These checks are local input evidence;
they do not prove live remote state, authorize an actor or enable the API.

Refresh SVN clients retain separate private configuration directories for their
entire clone lifetime, disable platform password stores and credential caching,
and use only the captured explicit credentials or anonymous access. Subversion
documents that an explicit configuration directory overrides other configuration
sources in [svn_config_get_config](https://subversion.apache.org/docs/api/latest/svn__config_8h.html).
Disposable authenticated svnserve controls demonstrate a readable ambient cache
with the ordinary CLI, refusal with missing or incorrect refresh passwords,
success with the explicit password, and unchanged ambient cache contents. This
is authentication evidence; full publication/recovery proof remains below.

Registered SVN paths use nonempty ASCII segments containing letters, digits,
'.', '_' and '-'. Trailing slashes, empty segments and peg syntax refuse before
distinctness is checked. A real local SVN control confirms that the rejected
trailing-slash and repeated-slash forms otherwise name the same node.
Explicit SVN default port 3690 and uppercase SVN hosts also refuse. Both inputs
must share one exact scheme/host/port authority; alternate authority spellings
cannot establish distinct branches. A nondefault SVN port remains supported
when both inputs use the same canonical authority. Live UUID/root proof remains
required even with equal authorities.
Any retained child, parent, legacy, global or Personal history block also refuses
source capture, admission and the first publication intent without clearing it.

The read-only Git source observer downloads both registered checkpoint branches
into a private repository without updating refs or FETCH_HEAD. Each authenticated
advertisement must contain both exact checkpoint tips. The parent and child use
their separately resolved credentials through the pinned HTTPS transport. This
proves Git inputs only; fresh remote revalidation, SVN evidence and authenticated
execution approval are still required before publication.

The corresponding SVN observer reads both registered inputs with isolated
clients and requires one repository UUID/root/HEAD snapshot. Pegged checkpoints
and complete branch histories must agree with the registered cursors. File,
property, deletion and replacement revisions beyond either cursor refuse.
Unrelated repository revisions may advance HEAD. Identity/location/HEAD drift
during observation also refuses. This proves input history, not copy lineage or
tree correspondence.

Baseline selection rechecks the captured registration in one read transaction.
It returns only the unique completed child receipt matching both current child
checkpoints and unchanged registration/echo identities, endpoints and policy.
An earlier receipt with changed or partially matching child checkpoints refuses
rather than reverting to the initial copy. No prior receipt means independent
first-copy and correspondence proof is still required. Credential rotation does
not invalidate historical publication evidence; current credentials remain a
separate admission requirement.

The preparation assembler combines those private snapshots with independent
copy/continuous-parent lineage and full Git/SVN bytes, modes, properties and
mergeinfo checks. The first supported case requires the current child SVN
checkpoint to be the original direct-copy creation revision. Its current scoped
Git checkpoint itself must be a shared ancestor of the advanced parent; an
arbitrary historical mapping or equal-tree child commit is not substituted.
A later refresh uses the exact completed receipt selected above, retains the
original copy origin and parent incarnation, and carries its prior mergeinfo.
Both results are computed locally before a final read-only registration,
credential, checkpoint, baseline and writer/conflict check. Preparation neither
admits a job nor changes a remote ref, remote revision or database checkpoint.

The private preparation resources are temporary and are cleaned on drop. Their
paths participate in the prepared pins, so a durable execution caller must
retain or safely reconstruct them before admission; this assembler is not that
caller. Authenticated actor/effective-policy approval, fresh remote checks at
each first publication intent, durable retention, exclusive orchestration and
crash recovery remain required before enabling execution.

Exact skip requests retain both the process busy guard and a durable ordinary
window through their existing remote observation and checkpoint action. They
may queue briefly behind an ordinary Preparing run instead of replacing the
exact-skip result with an immediate busy response. Pending admission performs
no write or lease renewal; the original owner must release its own window.
Refresh holds, uncertain publication and corrupt evidence refuse. A bounded
wait never expires, releases or adopts someone else's durable record. Inputs
are observed and checked after the request actually acquires both guards.

Window admission and ownership transitions reserve the SQLite writer before
reading their evidence. Under WAL, a deferred transaction can read an old
snapshot and fail its subsequent write upgrade immediately, without honoring
the busy timeout. Immediate transactions serialize this local database
contention without retrying an external publication or changing the five-second
request deadline. A held, uncertain or corrupt record still refuses, and only
its actual preparation owner can release it. Deterministic independent-
connection controls exercise contended admission and preparation drop; the
original concurrent exact-skip assertions remain unchanged.

The running SyncEngine now captures a read-only actor and effective policy
context from its own database and configuration. This requires a present,
enabled database admin with a current session; compatibility in-memory admin
sessions cannot supply this authority. Stored timestamps are parsed as RFC3339
instants, including offsets, and malformed, future or expired evidence refuses.
Expiry is sampled after the session row is read, so waiting for the database
cannot extend a session using an older entrance timestamp.
Only direct whole-tree policy with unrestricted branches and disabled size/LFS
filters is qualified, including the engine's actual path and LFS overrides.
Safe preview pins contain the actor ID and domain-separated session/policy
digests. The bearer and established fingerprint key remain private. Capture
never creates a key or writes database state. Recheck requires the same current
engine instance, current actor/session and unchanged context; logout, role or
enabled changes, session drift and key rotation refuse.

This context still needs atomic binding to authenticated job enrollment,
retained resources and publication intents. A copied preview pin cannot
construct the live context, and this component enables no execution endpoint.

Owned preparation resources now live beneath the actual connected database's
configured data directory. The database binds path identity to the main file
descriptor SQLite actually opened, including newly created databases. The
adapter supports the pinned bundled SQLite 3.45.0 Unix implementation, checks
its exact runtime source ID and built-in VFS open/allocation method, and borrows
the descriptor only for fstat. It neither opens nor closes another database
descriptor, which could release SQLite's process locks. Its private native
layout requires an explicit audit when SQLite changes; unsupported runtimes or
VFS implementations leave resource identity unknown. In-process native VFS
registration is trusted code. Memory, replaced/aliased/hardlinked database paths, foreign roots and
unsupported owner/permission metadata refuse before resource creation. A fresh
server operation UUID names its private resource directory. No existing Git
repository or working copy is adopted or reinitialized. The legacy temporary
preparation API and its cleanup behavior remain unchanged.

Git observation and SVN preparation share the owned directory lifetime. The
resource pin includes the original storage/root identities and tracked Git,
SVN operation and working-copy directory identities. Role/path replacement
refuses further use or retention. Namespace creation, synchronization and
unpublished cleanup use held directory descriptors and no-follow opens;
macOS iteration uses a fresh descriptor relative to the held directory.
Observed unknown identity retains resources rather than deleting through a
replacement pathname.

Ordinary SCM entries do not inherit the private root permission or hard-link
policy. Traversal classifies entries with no-follow stat before opening files
or directories. Synchronization preserves symlinks through parent directory
sync; cleanup unlinks them without opening their targets. Hard links lose only
their owned directory entry, and group-writable ordinary files/directories are
supported. Changed entries refuse traversal; special entries refuse retention
without blocking on an open. Generated SVN operation and working-copy roots
become private through held, no-follow descriptors before tracking, regardless
of the checkout's umask; versioned descendants keep their modes. Root and
tracked-role identity checks stay strict.

Before any durable enrollment attempt, resources and directory entries are
synchronized and retention is marked through the actual engine's actor entry
point. Actor/session/policy and workspace binding are checked before sync and
again immediately before the retention flag is set; a preparation result cannot
set that flag independently. Cleanup is not conditional on a later
successful commit: an unknown commit result or abrupt process exit must not
delete a potentially enrolled job's resources. Marking retention grants no
admission, publication, adoption or recovery authority. The opaque resources
still need authenticated atomic enrollment, a global writer barrier, fresh
remote/actor/policy checks at each first intent and a qualified recovery caller.

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
