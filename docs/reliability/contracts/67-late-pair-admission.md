# Late-pair admission and reviewed reconciliation

A late pair connects an already-published Git development branch to a related SVN branch without acknowledging pending Git work as imported. The default remains a preview. Publication is a separate explicit action on the captured result.

## Supported sources

Admission requires a repository-scoped, applied SVN-origin mapping at an actual Git ancestor. The Git object and SVN export at that baseline must have identical regular-file bytes and executable modes. A full import or a verified snapshot can provide that baseline; the snapshot's earlier history is not invented or replayed. Missing objects, unrelated/orphan history, a rewritten baseline, ambiguous mappings, incomplete observations and out-of-scope changes refuse before publication.

The v2 capture binds the authenticated actor, parent registration and credential generations, exact configured Git endpoint and tip, SVN UUID/root/source/target paths and revisions, copy ancestry, projection, every pending Git commit, and every intended result tree. The private captured document stays on the server. Clients receive a summary and opaque review pin, not executable captured bytes.

Git endpoint rewrites, ambiguous origin/push endpoints and ambient Git routing overrides are unsupported. Local absolute-path/file endpoints are available for owned fixtures; configured HTTP(S)/Git endpoints are compared explicitly. These checks do not change host Git configuration or credentials.

## Existing and missing SVN targets

A missing target is planned as a copy of the proven baseline revision. Parent SVN advancement is not silently included. The user may explicitly select it in the preview, producing a different pin and intended result.

An existing target requires actual copy ancestry from the source at or after the proven baseline. Its independent work is captured separately. Each Git delta is reconciled against that tree in original commit order. Nonoverlapping paths and text hunks preserve both sides; overlapping text, binary changes, delete/modify and incompatible mode changes produce a blocked preview. A blocked preview has no publishable Git result and creates no operation, child, mapping or checkpoint.

Only regular files and canonical Git executable modes are admitted. Symlinks, submodules, special nodes, external SVN materialization and unsupported path topology refuse. Unverified LFS/pointer changes are held rather than writing pointer text to SVN; first-class LFS resolution is tracked separately in #44.

## Review and freshness

The v2 POST contract uses `reconciliation: true` and an explicit `action`:

- `preview` captures both sides and returns their identities, pending work, conflicts, intended Git SHA/tree digest and `review_pin`.
- `publish` requires `dry_run: false`, `preview: false` and that pin. It recaptures all inputs; a different pin refuses before claiming work.
- `status` returns the actor-owned durable operation, without claiming a fresh remote observation.
- `resume` requires the same operation, actor, branch identities and reviewed intended-result pin. Native source and consumed-effect evidence is checked again.
- `cancel` cancels an unstarted claim or records a request while preserving any possible effects and their hold.

Every status separates preview, durable progress, held state and completed publication. HTTP success alone does not mean a pair is active. The UI disables publication when the form or parent-selection differs from the preview, when conflicts exist, or while a request is pending. Existing session/view ownership prevents a retired route or session from adopting callbacks.

## Explicit bounds

A capture admits at most 32 pending linear Git commits, 1,024 observed SVN lineage entries and a 16 MiB complete durable document. These are refusal bounds, not truncation rules. Future receipt copies, messages, paths, endpoints and control history are reserved before effects; a plan that cannot retain its evidence refuses. At most 32 ordered cancellation/continuation control records are retained. Larger or unsupported work remains unacknowledged.

The legacy v1 API remains a distinct new-target protocol with its original guards. It does not interpret a v2 journal or treat an existing SVN target as equivalent.
