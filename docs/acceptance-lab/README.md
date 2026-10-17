# Acceptance lab

This is the portable full-stack lab for issue #62. It runs the real RepoSync daemon and the shipped React bundle against synthetic LDAP, Apache SVN, and Gitea. It does not recreate the sync algorithm or mount the UI as a component test.

GitHub-hosted `ubuntu-24.04` is the runtime qualification host. A checkout on a developer Mac is not that qualification. Cursor Cloud and an Omarchy Ubuntu VM use this same launcher and are **NOT_RUN** until that host is exercised.

## One command

From a clean checkout, with Docker and the lab npm package installed:

```bash
npm --prefix tests/acceptance-lab ci --ignore-scripts
npm --prefix tests/acceptance-lab exec -- playwright install --with-deps chromium
scripts/acceptance-lab.sh test --suite smoke --candidate "$(git rev-parse HEAD)" --seed 1
```

`--suite` is `smoke`, `full`, or `soak`. `--candidate` must be the full lowercase SHA of `HEAD`. `--seed` is an integer. Add `--keep` to leave that run's stack up. `down`, `reset`, `report`, and `clean` require `--run-id` and fail closed. There is no latest-run pointer.

`prepare` builds images and does not start the stack. `up` starts one isolated stack. `test` prepares, starts, runs the suite, writes a report, and cleans up unless `--keep` is set.

The artifact directory is `$ACCEPTANCE_LAB_ARTIFACT_ROOT`, otherwise `$RUNNER_TEMP/acceptance-lab`, otherwise `/tmp/acceptance-lab`. It is rejected when it points inside the git checkout. Secrets stay in that run directory at mode `600` and are not part of the published evidence.

## What a run contains

Each invocation gets its own run id, Compose project, loopback ports, volumes, network, and artifact directory. The network is internal. SVN, Git, and LDAP use a per-run fixture CA inside the containers. Certificate checking stays on. The browser talks to the daemon on loopback HTTP. Cleanup checks the run id and `acceptance-lab.disposable=true` labels, then runs `docker compose down -v` for that project only. It does not prune the Docker host. Images are removed only when `ACCEPTANCE_LAB_REMOVE_IMAGES=1` and the tag contains the run id. The cached tags `rsacc-reposync:<candidate>` and `rsacc-*:lab` do not contain a run id, so a normal clean leaves them.

The daemon starts only after Gitea has issued an access token. The literal `test-token` is not used. Gitea answering that token is not GitHub Enterprise qualification. `GHE_QUALIFICATION` and `LIVE_ENTERPRISE` stay **NOT_RUN**. No command defaults to a company hostname.

## Identities

These are different commits. A passing smoke test on one of them does not qualify the others.

| Identity | SHA | What it is |
| --- | --- | --- |
| Frozen audit | `068562a90bfafd9b2a142cea524a89574814cba5` | Tree the operation audit inspected |
| Live main when this harness was written | `af394285869bdffa5f156c651cd1104027832749` | Parent is the audit SHA. Squash of PR 189 (`Refs #63 / Refs #89`). Not audited here |
| PR 194 head | `6c84adf858a8a2e5d7ac3a9f618bee1834f4d297` | Open Codex production lane. Not the candidate and not edited |
| Candidate | `git rev-parse HEAD` at launch | The tree the command actually tests |

The frozen catalog is `tests/acceptance-lab/catalog/operation-catalog.json`, SHA-256 `427f264e613486379eaaabf0d51bd7a3d79fb0297f6ee3654caf3342b80c4223`, 117 operations. The report field `audit_covers_candidate` is true only when the candidate SHA equals the audited SHA. A pull request workflow checks out GitHub's merge commit. That merge can contain the `af394285` product delta. The report records the candidate, the audited SHA, and the pull request head separately, and it does not treat the merge as the frozen audit.

The lab package lock is separate from `web-ui/package-lock.json`. Playwright `1.55.1`, TypeScript `5.8.3`, `@types/node` `22.14.0`. Lock SHA-256 `dac7b710a1408e7e09222325b3ef6d24572986c075f69f3c4db063dc9c2d606f`. `npm ci --ignore-scripts` does not download browsers; the Chromium install is a separate command. There is no root `Cargo.lock` and no root `rust-toolchain.toml` in this change. The daemon image builds with `CARGO_BUILD_JOBS=2`.

The harness compiler pin is official `rust:1.98.1-bookworm`, the same tag as `Dockerfile.reliability`. `prepare` pulls that image and records its id, RepoDigest, and `rustc` / `cargo` version strings before the daemon build. That pulled compiler image must have a nonempty registry digest. Locally built lab images record their immutable id and the complete `RepoDigests` list, and an empty list is stored as empty. The image then copies the resolved `Cargo.lock`, its sha256, and `cargo metadata --locked` into `build-graph/` under the artifact root. Those files compare one lab build with the next. They are not committed, and they are not a shared root lock.

This pin does not build or modify the product `Dockerfile`, which remains `rust:1.83-bookworm`. A newer lab compiler does not show that the product image or its MSRV builds. Hosted run [38183172933](https://github.com/chriscase/RepoSync/actions/runs/38183172933) remains the Cargo 1.83.0 result: it failed while parsing `getrandom` 0.4.3 (`edition2024`) before any service started. That result belongs to the product toolchain owner. The first hosted run, [38182857210](https://github.com/chriscase/RepoSync/actions/runs/38182857210), remains the entrypoint-context failure.

## Suites and honest results

`smoke` must pass its executed smoke operations and the harness gates. `full` fails while any selected row is `NOT_RUN`, `PARTIAL`, `FAIL`, or `SKIP`, or a workflow the product claims to support is not `PASS`. `PARTIAL` is not `PASS`. Playwright retries are `0`. A later pass after a retry is flaky and is not a clean `PASS`. An omitted required case fails the gate. Explicit `NOT_RUN` rows stay visible.

`NOT_RUN` includes the company-installed version (`R21`), a pinned historical binary, coordinated update-from-parent success, re-anchor success, GitHub Enterprise, a live enterprise run, Cursor Cloud, the Omarchy VM, and production fault hooks. Those hooks are not added to another owner's files. The external fault proxy can show a lost reply only as far as the proxy itself goes; it is not an in-process crash hook and it is not distributed rollback.

Safe refusal can pass while the successful workflow stays unqualified. Pair refresh execution and re-anchor are not implemented on the audited tree. A refusal is not evidence that the successful workflow works. Group membership does not grant an admin role; that gap stays visible.

The real-engine suite is invoked for `full` and `soak` only, without `REPOSYNC_REAL_ENGINE_USE_COMPOSE`. Its allowlisted `PARTIAL` rows in the existing CI gate are not acceptance `PASS` rows. `team_mode_e2e.rs` is not edited or treated as executed.

## Resources

Public GitHub-hosted Ubuntu runners are documented as 4 vCPUs, 16 GB RAM, and 14 GB SSD. The workflow prints `nproc`, `/proc/meminfo`, and `df` so the report can be compared with that shape. The lab starts at two Cargo jobs and one browser worker. An out-of-memory error, a full disk, or a timeout is an infrastructure failure. It is not a product `PASS`. Soak is a separate result from functional correctness.

A scheduled soak is not installed. There is no cron, no paid runner, and no self-hosted runner. `workflow_dispatch` can run `soak` on the public Ubuntu host when someone chooses that input.

## Cursor Cloud

**NOT_RUN.** When a cloud environment is available, qualification is:

1. Open this repository at the candidate SHA with a clean checkout.
2. Install Docker, Node 22, `npm --prefix tests/acceptance-lab ci --ignore-scripts`, and Chromium via `npm --prefix tests/acceptance-lab exec -- playwright install --with-deps chromium`.
3. Run `scripts/acceptance-lab.sh test --suite smoke --candidate "$(git rev-parse HEAD)" --seed 1`.
4. Publish the run's `publish/` directory. Do not point the artifact root at the checkout.
5. Leave the active project environment otherwise unchanged.

Until those steps have a recorded run id and report, Cursor Cloud is not qualified.

## Omarchy VM

**NOT_RUN.** The same `scripts/acceptance-lab.sh` command is the Omarchy entry point, inside a disposable Ubuntu VM that does not share host credentials, SSH agents, or company mounts. Runner registration is a separate private-control task and is not part of this workflow.

## Workflow

`.github/workflows/acceptance-lab.yml` is optional. It does not change required checks. Pull requests that touch the lab paths run `smoke` on `ubuntu-24.04`. `workflow_dispatch` can run `smoke`, `full`, or `soak`. The pull request path stays on `smoke`.

GitHub registers `workflow_dispatch` from the workflow file on the default branch. Until this file is on the default branch, a dispatched run is unavailable. The pull request run is the smoke path. This lab does not merge in order to enable dispatch.

The daemon image is built in one job and loaded in the browser job (`ACCEPTANCE_LAB_SKIP_REPOSYNC_BUILD=1`). The entrypoint is passed as its own Docker build context. The root `.dockerignore` on main omits `tests/`, so a copy from the repository context cannot see that file. The build job uploads `acceptance-lab-build-graph-<run id>-<attempt>` after prepare. A successful image without that graph fails the job. The upload still runs when prepare fails after the base image identity was written. The real-engine job runs only for a dispatched `full` or `soak`, and it does not join the lab Compose network. Evidence upload is limited to the run's `publish/` directory after scrub and scan. A failed scan withholds the bundle.

On that dispatched job, the workflow installs the harness Rust pin from `tests/acceptance-lab/pins.env` and removes a rustup toolchain named `stable` when one is present. `scripts/real-engine-scenario-suite.sh` is not edited. It selects `rustup run stable` when that name exists, and otherwise uses `cargo` on `PATH`. The engine job waits for the daemon build and downloads its build graph from the same workflow attempt. `engine-lock.mjs` checks the candidate, immutable image ID and both recorded lockfile hashes before installing that lock into the disposable job checkout. It rejects a different existing root lock. No root lock is committed. The suite retains `--locked`, and a post-run comparison checks that the installed lock did not change. The engine artifact records the selected lock identity, scenario summary and scrubbed scenario logs. A missing summary or failed scan remains a failure. Full and soak are manual dispatch inputs; they are not installed on a schedule.

When the browser job does not produce `identity.json`, the candidate report run id is `gha<github.run_id>a<github.run_attempt>`. It is not the literal `unknown`. Reports already uploaded from earlier attempts stay as they were. A smoke result does not qualify the full suite. `NOT_RUN`, `PARTIAL`, and `UNQUALIFIED` rows block a full PASS. Runtime fault, egress, zero-discovery, artifact, and cleanup controls are not claimed from the synthetic harness self-test.

`GITHUB_TOKEN` for this workflow is `contents: read`. That checks out the repository. The artifact upload action obtains its own Actions runtime token. This workflow does not grant `actions: write`. It does not write repository contents, open or merge pull requests, or close issues.

Dispatch inputs are job environment values. `tests/acceptance-lab/src/ci-gate.mjs` accepts a suite of `smoke`, `full`, or `soak` and an integer seed before the launcher command is built. A quote or shell metacharacter in the seed is rejected there. A pull request cannot select `full` or `soak`.

The browser publish directory must contain `playwright.json`, `self-test.json`, and `report.json`. A dispatched `full` or `soak` must also produce the real-engine `summary.json`. The daemon image upload fails when the image is missing. A missing required runtime artifact fails the workflow with `infrastructure FAIL`. The files that attempt did produce are still uploaded, including `infrastructure-failure.txt` when the required set is absent. A later attempt does not replace that record. A product `FAIL` inside a complete bundle stays a suite failure for the product owner.

Syntax, type, and harness-detector checks do not qualify the services, the browser, or runtime fault detection. Those stay **NOT_RUN** until a GitHub-hosted run records them. A smoke result does not qualify the full suite.

## Reading a failure

Fault control is published by its own loopback forwarder across the internal
and publish bridges. The fault proxy remains on the internal bridge. Startup
requires a healthy controller and retains its stats and both services' logs.
The read-loss cases require a healthy scoped OPTIONS request, a new one-response
drop, and unchanged snapshots. They remain PARTIAL: a lost response to a read
does not qualify a post-write recovery, held restart, or full WebDAV commit.

Fresh fixture targets must be accessible, private, empty Git repositories.
Their RepoSync registrations start disabled. The harness supplies credentials,
starts exactly one full import, matches its durable operation ID and completion,
and verifies the r1 mappings, receipt and directional checkpoints before enabling
polling. It does not replay or reset a failed import. Terminal status and outcome
details are captured immediately; complete service logs are scrubbed and retained.
The earlier import failures lacked terminal cause evidence, so the fixture ordering
fix must be checked by a new hosted run; it is not a production import repair claim.

The follow-up to the `78a06db` review exercises SVN revision gaps with actual
sibling-path commits between two confirmed content revisions. Empty-to-project
qualification additionally requires an explicit no-target receipt and cursor;
quietly omitting the outside revision does not qualify that catalog row.
SVN refusal checks allow the operator's intentional source revision, require
unchanged destination/mappings/checkpoints, and require an exact daemon failure
audit row. `SVN-PROP-ONLY-REV` sets only `svn:eol-style` on an existing file.

The remaining branch-admission cases seed real SVN-derived Git refs and verify
preview pins, no-copy/no-child/no-journal behavior, copyfrom provenance, replay
receipts, directional checkpoints, parent isolation and idle repeat.
The publish journal records one handled Git endpoint per cycle; a cycle may
drain both pending commits. Per-commit receipts and the intermediate SVN tree
prove individual replay. Historical SVN reads pin both the operative and peg
revision so absence and bytes come from that path at that revision, including
after deletion or recreation at HEAD. Observer errors remain fatal.

The remote-delete oracle uses three real managed child pairs. Explicit false
flags remove only one registration and must preserve its Git ref and SVN path.
Explicit true flags must return a successful, warning-free response and remove
exactly the selected child ref/path and registration. Full remote head and SVN
branch inventories, retained parent/sibling/kept bytes, directional checkpoints
and the exact SVN deletion revision prove preservation. Provider read errors,
root/busy refusals, missing preconditions and retained targets are failures.
The case retains both API responses and before/after observations; a refusal
or an unchanged SVN tip alone cannot qualify remote deletion.
SVN-read and outbound Git checkpoints remain separate;
a verified Git-to-SVN echo does not have to advance the SVN-read cursor.
Late-pair publish remains PARTIAL unless crash and pre-final activation timing are also
exercised. LDAP group-role and logout use complete API bodies; observed role
refusal does not implement group mapping, and logout does not qualify expiry or
disabled-user revocation. The browser login asserts the actual 401 and the error
span inside the login form, then requires a successful login. A separate
Chromium locator regression covers the `lab.invalid` banner collision.

Scrubbed per-case observations are retained under `observations/` in the
published evidence, alongside Playwright annotations. Regression fixtures and
negative controls remain implementation checks, not runtime qualification.

`publish/report.md` lists the suite result, whether the audit SHA covers the candidate, and each operation. Infrastructure failures stay infrastructure failures. Product failures stay on their existing owners. This branch already contains four Personal production files, `crates/cli/src/personal/mod.rs`, `import.rs`, `daemon_ctl.rs`, and `crates/personal/src/engine.rs`, so that a fresh personal database is initialized, import commits on the configured branch, and Git-to-SVN errors keep their cause. Those files are part of this draft. The harness repair does not add further production Rust. It does not patch the web UI, shared lockfiles, or PR 194 to make a red case green.

### Accounting correction design

The following additive fields are this correction's design. No enumerated
historical 40-test accounting contract was recovered. The report schema remains
`reposync.acceptance-lab.report.v1` and existing fields and consumers remain.

`selected_suite_result`, `selected_suite_counts`, and `selected_suite_ids`
describe the selected executors: smoke selects 16 browser cases; full selects
141 browser cases plus 21 engine cases; soak selects its 2 browser cases.
Harness controls are excluded from these executor counts and still gate the
result through `gate_failed`. A selected executor result also fails on omission,
unexpected skip, zero discovery, runner/parse errors, retries or assertion failure.
This subset result does not qualify the whole release.

`counts`, `rows`, and `r_coverage` continue to include the whole obligation union,
including the 18 synthetic harness controls. `overall` is PASS only when every
retained obligation and every additional reported row is PASS, no row is
UNQUALIFIED, and all controls above pass. FAIL, PARTIAL, NOT_RUN, SKIP, unknown
status, missing required rows, or duplicate row identities prevent that PASS.
`full_qualification` remains NOT_RUN for smoke/soak and follows the whole-plan
result for full. The legacy `suite_result` and CLI/CI exit still require the
whole plan for full; smoke/soak gate their selected executors and harness controls.
The launcher continues to stamp these existing fields into run identity.

`catalog/qualification-plan.json` retains all 519 obligations from source
`1e4de1de99012c5c879d4e73d3700e6ea39df81d`: the operation catalog, acceptance
matrix, frozen audit and mandatory harness controls. Missing catalog entries
become visible NOT_RUN rows rather than disappearing. New catalog entries are
also included. Retiring/replacing an accepted obligation requires an explicit
reviewed plan amendment. Reports include the plan schema, SHA-256 and required
count. Markdown lists every JSON row, including the 325 matrix-only obligations.

The qualification plan also retains each case's criterion associations. Removing
a catalog/matrix entry cannot remove its R-criterion coverage. Raw self-test,
Playwright and engine observations are validated before merging identities:
duplicate identities and invalid statuses become FAIL observations and visible
`input_validation_errors`, independent of input order. Unmapped engine results
remain in rows, counts and Markdown; an unmapped engine PASS stays UNQUALIFIED
until its mapping is reviewed. These corrections address the reserved verifier's
three P2 findings on `8011806`; that review was BLOCK and did not execute a
candidate or establish hosted qualification.

Re-review of `adaf2fb` remained BLOCK on unannotated Playwright tests and
duplicate operation declarations. Missing/malformed identities now remain
blocking diagnostics; an unannotated observation is retained as a FAIL row.
Catalog IDs are checked before joining executions, and matrix/audit declarations
are checked within each declaration list while shared references across criteria
remain valid. The adjacent audit also rejects conflicting status metadata,
hidden first-attempt failures, contradictory outcome/error fields, ambiguous
result containers, reported unavailable engine tools and overall/count discrepancies. Playwright and
self-test identities must belong to their declared evidence source. Valid
legacy engine aliases, normal Playwright outcome fields and mirrored annotations
remain accepted. Empty executor selections cannot qualify the whole plan.

If all full executors and controls passed, the counterfactual result would be
180 PASS / 339 NOT_RUN / overall FAIL. That is not an actual historical run.
The verified historical `05fe87bb` full run instead recorded 131 PASS / 34 FAIL /
15 PARTIAL / 339 NOT_RUN, overall FAIL, with 141 browser cases attempted once
and engine 16 PASS / 5 PARTIAL. It does not qualify the current source head.

Run the accounting regressions with
`node --test tests/acceptance-lab/src/report-accounting.test.mjs`. They process
synthetic JSON in temporary directories without invoking fixture executables,
the launcher, browser, Docker, SVN, Git or the production engine. Their success
is implementation evidence; hosted full seed-1 qualification remains NOT_RUN.


### Repair verification after the 418ed374 review

The CI harness-regression job checks missing or conflicting identity metadata,
failed compiler identity commands, and isolation setup/trigger failures before
building the daemon. Evidence and report identities must include candidate,
suite, seed and run ID. Repeated agreeing identity copies in a publish bundle
remain supported.

The isolation smoke case uses two valid repository-scoped Git users and distinct
SVN users. Both repositories exercise both sync directions. The negative source
change must be committed before triggering; a server access-log refusal must
identify the daemon IP and the target repository. Git's unauthenticated 401
challenge is not a refusal receipt. SVN refusal must identify Alice attempting
Bob's repository. Git-to-SVN recovery verifies both stored Git checkpoints against
the pending source SHA; the observer selects the UUID-scoped key when the schema
contains `scope_uuid`, and the repository-ID key on the older schema. A missing
UUID-scoped value never falls back to a legacy mirror. The SVN-read cursor need
not equal the new SVN revision.
Restoring the correct credential must publish the pending
change with one source receipt, and a repeated trigger must be idle. The wrong
Git-token case keeps the correct user's permission intact to detect stale token
selection. Provisioning and observer reads still use the lab admin credentials;
this does not qualify admin-token rotation or real-engine/live isolation.

Local harness regressions (Node, Bash and the SQLite CLI required; no Docker daemon
or browser download required):

```sh
npm --prefix tests/acceptance-lab ci --ignore-scripts
npm --prefix tests/acceptance-lab run typecheck
node tests/acceptance-lab/src/identity-check.mjs
npm --prefix tests/acceptance-lab exec -- playwright test -c tests/acceptance-lab/playwright.regression.config.ts --grep-invert 'annotated pass then fails'
bash tests/acceptance-lab/src/fail-closed-check.sh
```

The deliberately failing Playwright fixture inside the final command must exit
nonzero; the enclosing regression must succeed only after aggregation and
enforcement both reject it. These synthetic checks do not count as runtime
qualification. Full, real-engine, old-install and live tiers remain unqualified.

### Full-suite continuation and engine lock setup

Fixture authentication is paced using a run-local login budget: at most eight
requests in a rolling 61-second window, with the same budget used by API and
browser logins. The budget survives Playwright worker restarts. It does not
retry refusals or disable the daemon's authentication rate limiter. Successful
fixture session tokens are added to the run's scrub list.

The service-bind case uses the reachable LDAP TLS endpoint, verifies healthy
user logins before and after the fault, checks that the wrong-bind configuration
was accepted, and requires both a 401 and the daemon's observed LDAP service-bind
rejection. A throttle response or failed setup cannot qualify that case.

Published-file polling treats an absent SVN path or Git blob as not ready;
transport, authentication, and other observer errors still fail. Relative paths
permit dotfiles and reject traversal. Local reset/rebase fixtures never push;
the SVN symlink fixture commits a real `svn:special` node. Refresh fixtures first
establish a child through the production branch-pair API and verify its parent,
remote branches, and pins. Admission failures remain failures, and unsupported
refresh/re-anchor execution is not promoted to PASS.

The browser catalog uses one worker in default execution mode. A failed case
keeps its failed result, while later cases run after the worker restarts. Shared
lab pairs are found by their persisted repository identity and matching remotes
when the worker's in-memory cache is empty; ambiguous or mismatched pairs fail.
Later cases can still encounter state left by an earlier failure, so those
failures must be interpreted with their logs and prerequisite history.

`LAB_GITEA_API_NOT_GHE` records `PARTIAL` / `UNQUALIFIED` after the synthetic
Gitea API check. It does not throw solely because the fixture is not GHE.
Authentication or provider-identity errors still fail. GitHub Enterprise and
live qualification stay unqualified, and a PARTIAL row still blocks full PASS.

Additional local checks (no Docker or browser download):

```sh
node tests/acceptance-lab/src/engine-lock-check.mjs
node tests/acceptance-lab/src/continuation-check.mjs
```

The continuation check executes an instrumented copy of the shipped catalog
registration: the GHE case stays unqualified, a real failure stays FAIL, and a
later case must execute. The lock checks reject missing, wrong-candidate or
modified graph data before installing a lock or writing a success receipt.
