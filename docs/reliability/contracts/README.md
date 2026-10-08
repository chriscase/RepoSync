# Reliability implementation contracts

These documents are the reviewable handoff from the current-main #62 catalog
to later runtime work. They do not activate schema, operations, or live
acceptance.

| Issue | Contract | Product merge in this PR |
| --- | --- | --- |
| #63 | [63-lineage-checkpoints.md](63-lineage-checkpoints.md) | No |
| #64 | [64-durable-jobs.md](64-durable-jobs.md) | No |
| #66 | [66-history-containment.md](66-history-containment.md) | No |
| #65 | [65-managed-remove.md](65-managed-remove.md) | Additive removal API only; does not close #65 |
| #68 | [68-snapshot-init.md](68-snapshot-init.md) | Team snapshot/selected-revision init; does not close #68 |
| #67 | [67-late-pair-admission.md](67-late-pair-admission.md) | Late-pair admission/preview only; does not close #67 |
| #69 | [69-pair-refresh.md](69-pair-refresh.md) | Update-pair-from-parent preview only; re-anchor and execution are NOT IMPLEMENTED; does not close #69 |
| #71 | [71-git-operation-requests.md](71-git-operation-requests.md) | Manual preview/status client and workflow docs; refresh execute is NOT IMPLEMENTED; no live webhooks or branch rules; does not close #71 |
| #89 | [89-projected-changeset.md](89-projected-changeset.md) | RS-C02 projected Git→SVN changeset, persisted conflict apply gate, hook literals; does not close #89 |

Authoritative sources that these contracts must not silently rewrite:

- `docs/reliability/GOAL.md` (SHA-256 `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`)
- Issue bodies #61, #63, #64, #66
- Existing copy-only [migration-proposal.md](../migration-proposal.md)
- Existing [import-cancellation.md](../import-cancellation.md) and [RS64-B-reconciliation.md](../RS64-B-reconciliation.md)
- Existing team-gate description in [design.md](../design.md)

Monorepo epic #52 / schema #54 / tests #60 remain separate. Coordinate numbers
and nullable mapping semantics; do not absorb that scope here.
