# Candidate copy safety and #54 typed readers — review 8

Normal `Database::initialize`, operational API/UI, daemon, installer and scheduler remain on the unchanged v12 boundary. Candidate registry remains13/14 and feature-gated. The same migration implementation qualifies disposable copies; no production activation or general #64 transition service is introduced.

## L01 storage contract

A quiesced source and writable copy must have separate regular Unix device/inode identities and exactly one hard link each. Copy WAL/SHM/journal files must also be private regular files. Admission records both DB identities and revalidates them immediately before the write-capable SQLite open, including before possible journal recovery. Replacement after sealing rejects. No implementation unlinks, replaces or repairs input storage. Source/canary bytes and user_version12 remain exact under rejected alias tests; a normal independent copy still reaches14.

This is a supported Linux/macOS fixture boundary under private-directory, nonconcurrent, quiescent ownership. It is not writer fencing against an attacker/concurrent host replacing paths between checks. Unknown identities/aliasing refuse rather than claim that broader protection.

## L02 evidence admission

[The vocabulary](legacy-evidence-vocabulary.json) is shared by Rust admission and Python inventory. Actual receipts are `handled_git_no_target_<repo>_<source>`; exact ownership uses the longest existing repository ID followed by underscore, avoiding `pair`/`pair_two` prefix confusion. Historical aggregate keys remain unsupported preserved evidence, never a runtime-receipt proof.

Before endpoint qualification, one decision checks column/scoped outgoing agreement, optional scoped incoming SVN agreement, actual owned receipt/baseline keys, supported SVN watermark sources, scoped and singleton import progress, and `effect_unknown_<repo>`. Unsupported receipt versions1/2/3, filtered receipts, incoming/progress conflicts and unknown effects refuse. The real qualifier removes any earlier grant and records `needs_reconciliation`, `external_effect_unknown` or disabled `not_qualified` itself. Copy conversion preserves every legacy SQL value/sequence and emits only that read-safe disposition; no test assigns the new overlays' state manually.

Global `last_svn_rev`/`last_git_hash` are display references, never pair authority; a differing global value alone does not force independent repositories to share cursors. A matching singleton import completion is retained as an ambiguous reference, not linked as owned evidence. Conflicting unowned progress/watermark claims exceed this narrow r2 import proof and refuse with ownership/revision diagnostics; they are never resolved by maxima or borrowing another repository's state. Inventory classifications remain local inspection, not remote admission proof.

Clean two-pair original imports and disabled controls remain. The combined2/999/888/777 and actual-key receipt/progress/unknown overlays run through the real qualifier and14 conversion. Original installation and enrolled endpoint manifests remain unchanged.

## L03 structural authority

The lineage baseline reserves the initial directional source without inventing an outgoing SVN effect. An outcome cannot reuse the Git baseline or an incoming revision at/below its SVN baseline. Frontier updates cannot keep the same source or return to the Git baseline. Outcome source uniqueness and the checked writer reject replayed/recorded identities; predecessor equality and the transaction protect continuity/atomicity. The public writer's failed baseline return inserts no committed false outcome.

All four resolved outcome kinds remain immutable after later frontiers advance: UPDATE, DELETE and REPLACE through either unique conflict key reject. Pending/unresolved rows remain separate; no general transition/recovery service is implemented.

These are structural checks. Predecessor labels and arbitrary hash strings do **not** prove Git ancestry. The writer requires separately qualified pinned external evidence; unknown arbitrary Git histories/backward ordering remain unqualified. The unit positive is a two-step structural writer control, not a claim that its synthetic hashes are real descendant objects. Original imported lineage still receives actual disposable SVN/Git proof.

## Reader entry and decision matrix

Only `CopySession::readers()` constructs the model, after storage/source checks and quiescent-copy admission. It opens immutable read-only SQLite, refuses sidecars, checks version14 and the physical schema cached during copy admission, and checks FK ownership. Reader calls perform no installation/in-memory initialization or migration, journal recovery, index update, receipt materialization, synchronization or remote command. Full copy/source seals are checked before and after each call, including errors. Nonconcurrent private-directory assumptions also apply to these reads.

| Evidence / requested scope | Canonical interpretation | Legacy display |
| --- | --- | --- |
| Explicit existing qualified generation; proved baseline | Initial mapped target or handled baseline with no invented emitted effect | Raw matching rows remain separate |
| Matching handled-chain applied outcome and, when a legacy row exists, explicit link plus matching values | Mapped source/target with outcome ID | All raw values retained |
| Matching handled-chain no-target outcome and explicit link for an existing nullable mapping | Typed no-target, never inferred from NULL | NULL remains representable |
| Missing/wrong generation, disabled/not-qualified/missing repository | Explicit unqualified scope; no canonical result | Scoped retained rows available |
| Non-NULL unlinked/malformed/conflicting row | Unresolved | Original value/ID preserved |
| Duplicate owned rows | Ambiguity, no arbitrary selection | Every row retained |
| Ownerless row | Unresolved legacy display, never borrowed ownership | Original NULL ownership preserved |
| Pending/effect-unknown or resolved row outside handled chain | No handled-source/emitted-target authority | Status exposes nonhandled records separately |
| No mapping/outcome for the requested source | Missing | Empty or separately unqualified display |

`lookup` requires repository, explicit optional generation, direction and directional source identity. The two directions may share an SVN number without becoming duplicates. No maximum generation is selected. Canonical interpretation follows only the explicit frontier/predecessor chain back to the lineage baseline, not timestamps, lexical hashes or last row IDs.

`list` uses ID-based deterministic pagination with an explicit `None` first-page cursor (including zero and negative retained IDs) for display, with per-row canonical classification and ambiguity. `legacy_page` represents **every** old row and SQL value (including NULL/malformed/ownerless) globally, and grants no authority. `status` separates enabled/disposition/requested generation, current handled sources, current outcome targets and last actual emitted targets.

`last_emitted` traverses the handled chain to the last applied effect; a later no-target source does not erase that object. Incoming import baseline is a real initial Git target; outgoing imported baseline has no invented SVN target. Intervening pending/effect-unknown rows do not move a frontier or become emitted history. Both directions are tested.

The old `candidate_migration::lookup_mapping` remains only prototype/display compatibility for the retained MAPPING_NULL oracle; its `Mapped` value is explicitly **not canonical generation authority**. New typed consumers never call it. Existing operational String/Option readers remain unchanged and unactivated; this copy qualification does not close #54/#63.

## Remaining boundary

Deployed versions, arbitrary historical topology/policy, production remote identity, fencing/backup ownership, credential variants, activation, live acceptance, down migration and general #64 external-effect recovery remain unqualified. The smallest proposed next slice after independent review is additional typed-reader historical-evidence qualification on copies, including nullable legacy API serialization; activation needs a separately reviewed boundary.
