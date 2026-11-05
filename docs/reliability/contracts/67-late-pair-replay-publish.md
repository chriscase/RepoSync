# Late-pair v2 publication and recovery

The v2 protocol extends the existing `late_pair_publish_v1` journal family with an explicit `version: 2` document and `late_pair_reconciliation_v2` policy. It does not relabel incompatible data as version 1. Existing v1 documents remain readable and serialized without a v2 payload; v1/v2 policy or field blending refuses.

## Durable ordering

The operation claims the parent under an IMMEDIATE transaction, comparing the captured parent/credential generation and reserving the active journal before effects. The existing repository busy guard excludes ordinary in-process work during capture/claim and execution. The child remains disabled while reconciliation is incomplete.

For a new target, copy intent precedes `svn copy`. Recovery inspects the exact copy-from path/revision, SVN identity and full stop-on-copy history and verifies its tree. An absent, modified or ambiguous pending outcome is held; it is not an instruction to issue another copy.

The server constructs private Git reconciliation objects matching the captured SVN tree and each intended intermediate result. Every changed replay uses the existing #64 Git-to-SVN intent producer before committing. Its source object, parent/tree, changed paths, message, SVN identity, pre-write revision and tree are pinned. The server verifies the actual committed bytes and modes before confirming the native producer and retaining its completed document in the v2 receipt. Unchanged steps retain their original revision without inventing an SVN commit.

The final Git result preserves the published development tip and the reconciliation chain. Publication proves ancestry, uses an exact expected-tip lease for the one configured ref, and retains the existing #64 SVN-to-Git publication intent before pushing. It never updates an unrelated ref or rewrites published history. A result already present remotely is observed explicitly. Confirmation requires the exact intended SHA/tree and SVN coordinate.

Completion atomically validates the real native producers, current child incarnation and checkpoints, advances the proven covered inbound frontier, enables the child and completes the journal. The covered-frontier record references the immutable completed v2 document and exact retained producer/mapping evidence, child scope, echo generation and projection. It is not an Applied/no-target audit row and grants no effect permission. Missing, duplicate, contradictory, pruned, stale-generation or foreign evidence refuses rather than becoming an old-import baseline. Ordinary continuation consumes this authority through the strict reader; it does not rediscover already covered A1/A2 work.

## State and cancellation

The typed phases are `prepared`, `copy_pending`, `source_ready`, `applying`, `publishing`, `checkpointing`, `completed`, `cancelled` and `held`. Contradictory outer state/frontier, out-of-order receipts and publication before replay completion refuse decoding. A held v2 operation uses nonterminal `reconciliation_held`, preserving scheduler isolation and the active reservation. Existing legacy states retain their interpretation.

Cancellation before any intent is terminal and retains its document. After an intent, cancellation does not undo a remote effect or release uncertainty. Ordered actor/pin-bound cancellation and explicit continuation requests remain in the journal. Continuation cannot authorize blind replay: a pending native intent must have a uniquely matching actual result before it is finished. If the outcome is absent or uncertain, the hold remains. A pre-effect cancelled operation requires a fresh review/new request.

Recovery can finish an already-consumed copy, replay commit or Git publication after reopening the database. It does not infer collection from a missing PID, an empty registry or an HTTP timeout. The native intent and independently observed repository result determine whether completion is possible. Original error/failure records are retained; the status view distinguishes its last durable observation from fresh remote evidence.

## Foreign consumers and scope

The refresh consumer keeps its separate active-pointer refusal and all other journal arms. Its exact late-pair arm delegates to a strict decoder that admits only genuine completed legacy v1 or coherent completed v2 evidence with matching document ID/owner/type. Cancelled, partial, held, malformed, unknown-version and blended records do not become completion proof. Any later #69 composition must carry the accepted helper and consumer together.

A held operation is exposed as held in status and cannot authorize Skip or another ordinary mutation. Existing writer fences, removal checks, refresh holds, session isolation, projection rules and unknown-evidence refusals stay in force. This protocol changes no schema, migration, daemon process ownership, deployment configuration, main-promotion gate or #69 execution/AC7 authority.

## Qualification requirements

Real local SVN/Git cases must prove baseline A1/A2 replay exactly once, explicit parent advancement, existing-target nonoverlap with both trees retained, overlapping/binary refusal, full and snapshot-derived ancestry, unrelated/rewritten/ambiguous/stale/out-of-scope refusal, and reopened recovery after copy, one replay and remote publication before checkpoint persistence. Assertions cover intermediate/final bytes and modes, Git ancestry, real journal producers, mappings/checkpoints, disabled-to-active ordering and a reconstructed-engine quiet repeat.

Production HTTP and UI checks must distinguish a preview from publication, reject stale reviewed inputs without mutation, preserve branch/session ownership, expose held outcomes and errors, and retain selected input. Final source requires independent review and exact-revision CI before qualified epic integration. None of these requirements asserts production deployment or whole-epic qualification.
