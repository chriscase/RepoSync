# PR72 foundation merge closeout

The original [GOAL](../GOAL.md) and prior review briefs remain historical,
byte-identical contracts. Chris's September 28 closeout instruction permits
merging this foundation only after the configured checks and independent
review gates pass. It does not authorize release, deployment, or candidate
activation.

## Cumulative scope

The default runtime contains the previously reviewed pre-reset Git history
gate, directional checkpoint and maintenance protections, and SVN apply
failure/unchanged-path containment. These are active reliability changes, not
fixture-only code. The proposed v13/v14 migration, typed readers, DTOs, and
scoped inspection HTTP routes are compiled or invoked only through the
`reliability-fixture` test path. Normal database initialization still ends at
v12; the ordinary web router does not register the candidate routes.
The durable operation service, production permission model, and general #64
cancellation/recovery work remain outside this increment.

## Closeout corrections and coverage

Repository-wide `cargo fmt` was applied in its own commit. Separate commits
address the configured Clippy warnings without changing synchronization
decisions, and replace only the disposable metadata test's SVN author setup.
The original metadata assertions on the emitted Git commit, mapping, audit
entry, and file contents remain. The isolated comparison logs had shown the
test's `svn propset --revprop` failing at its pre-revprop-change hook before
the engine ran; setting that synthetic author with `svnadmin setrevprop`
avoids hook execution. The focused test passes locally.

`test_team_mode_conflict_detection` remains **ignored in ordinary suites**.
An explicit run on this branch returned `GitError(ApplyFailed)` for overlapping
`shared.txt` edits before reaching its conflict assertion. It is not a passing
test or a claim of conflict-detection coverage. [#73](https://github.com/chriscase/RepoSync/issues/73)
tracks activation and stronger tree/checkpoint assertions; [#29](https://github.com/chriscase/RepoSync/issues/29)
is the earlier closed implementation issue. Existing failed-apply controls
separately assert that pending SVN work is not acknowledged.

The three matched comparison anchors are original main
`87379741779a6259f7eeb52a68cc6f061174e5ef`, retained
`f74fce855a1f1d80dd631397f436ba33906272e6`, and immediately reviewed
`ab097580d9415ae2f8df44bed4041f740a1b38f2`. The 86 required exact
cases, 26 literal DTO payloads, original GOAL, and locked dependency graph
are retained. The local configured formatter, lint, build, and full workspace
test commands pass after the corrections; the full workspace suite reports
376 passed, zero failed, one ignored. The initial sandboxed local HTTP tests
could not bind loopback; the same unfiltered suite passed when rerun with
loopback permitted and production-related environment variables removed.
CI supplies the sealed-runtime and cross-platform qualification.

Repository inspection found main-push CI and E2E workflows and a release
workflow triggered only by `v*` tags. GitHub reports no repository webhooks,
environments, branch protection, or rulesets for main. The PR introduces one
pull-request/dispatch diagnostic workflow and no main-push deployment step.
This inspection does not establish the absence of external systems that poll
the repository. No release or deploy action is part of this closeout.
