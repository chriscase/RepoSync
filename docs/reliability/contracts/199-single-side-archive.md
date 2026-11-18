# Single-side branch-pair archive (#199)

## Contract

A branch pair may explicitly request `archive_pair: true` with exactly one of
`delete_git` and `delete_svn`. Review and approval use the existing authenticated
`/removal/review` and `/removal/approve` protocol. The archive disposition is
included in the reviewed plan digest, so changing it invalidates the approval.
Old requests and retained approvals that omit the field still remove the
registration; their serialization and meaning are unchanged.

Only administrators may review, approve, continue or recover an operation.
Archival refuses active sync, durable writers and ownership/reconciliation holds
before disabling the pair or recording a new operation. Native deletion remains
pinned to the reviewed Git ref/tip or SVN UUID/path/revision. Uncertain effects
remain held, not blindly retried.

Once verified, the pair becomes permanently disabled with `sync_status=archived`.
The surviving remote is untouched. The pair registration, local working tree,
scope identity, credentials, final cursors, mappings and audit remain retained.
The operation records the actor, selected side, branch names and last cursors.
The durable tombstone blocks new sync, import, refresh, late pairing, reactivation
and generic restore. Generic mutation, deletion and maintenance paths preserve
archived registration and evidence. Reattachment is deliberately unsupported.
The parent continues to list the archive; deleting the parent remains blocked
while child registrations exist. No automatic purge is introduced.

The browser sends explicit archive intent for a one-side choice and names the
selected and retained branches. Full-pair deletion and registration-only removal
remain distinct. A completed archive stays visible rather than navigating as
though the pair disappeared.

## Verification boundaries

New native synthetic fixtures cover both remote directions, survivor history,
local custody, terminal persistence after reopening the database, repeated
approval/continuation, busy refusal, malformed choices and old v2 serialization.
Storage regression fixtures cover forbidden mutation and maintenance retention.
Authenticated API regressions cover anonymous and non-administrator denial.
Frontend tests cover the reviewed intent and visible lifecycle.

These checks do not substitute for the four acceptance-lab browser/LDAP scenarios
in #199. The reserved #195 harness is not changed by this implementation. Actual
sandbox-enabled browser and daemon-restart qualification, exact-head CI and
independent review must be recorded before whole-story completion is claimed.
No deployed service, real branch or production credential is a test target.

## Implementation verification, 2026-11-17

On the isolated implementation checkout, the reviewed-removal native suite
passed 14/14, including both archive directions and legacy removal/restore.
The two archive storage invariant tests passed. The authenticated denial test
and the two-direction archive HTTP test passed. Workspace Clippy passed.
Frontend typecheck, lint, 645 unit tests and the production build passed.
`web-ui/a11y/archive199.test.mjs` passed two synthetic browser flows using an
existing Chrome executable with `chromiumSandbox: true`, at 1280px and 600px,
covering selected-side confirmation, exact request intent, retained detail,
readonly controls, reload and audit navigation. Its fixture does not contact
real repositories and does not establish LDAP/daemon end-to-end acceptance.

The independent source review found a legacy `/remove` path that could replace
an archive with ordinary removal. The implementation now refuses that path at
storage and API boundaries; native and HTTP regressions preserve the archive.
Final exact-commit review and hosted CI are separate gates, not claimed here.

Review corrections also bind the displayed dependency preview to the exact
reviewed archive (or original retained legacy request), and classify a verified
terminal archive as disabled rather than reconciliation-held in scoped and
aggregate status. The write fence remains enforced. The successor frontend
verification passed 646 unit tests and three sandbox-enabled browser cases,
including populated credentials and legacy-request recovery. Archive API tests
cover scoped/aggregate health and confirm the fence remains active.
