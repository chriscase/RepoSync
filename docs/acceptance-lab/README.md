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

On that dispatched job, the workflow installs the harness Rust pin from `tests/acceptance-lab/pins.env` and removes a rustup toolchain named `stable` when one is present. `scripts/real-engine-scenario-suite.sh` is not edited. It selects `rustup run stable` when that name exists, and otherwise uses `cargo` on `PATH`. The suite passes `--locked`. This repository has no root `Cargo.lock`, and this harness does not add one, so a dispatched engine run can fail there. That failure stays a suite result. `workflow_dispatch` itself stays unavailable until this workflow file is on the default branch. Pull requests keep running smoke. Full and soak are manual dispatch inputs for after that registration. They are not installed on a schedule.

When the browser job does not produce `identity.json`, the candidate report run id is `gha<github.run_id>a<github.run_attempt>`. It is not the literal `unknown`. Reports already uploaded from earlier attempts stay as they were. A smoke result does not qualify the full suite. `NOT_RUN`, `PARTIAL`, and `UNQUALIFIED` rows block a full PASS. Runtime fault, egress, zero-discovery, artifact, and cleanup controls are not claimed from the synthetic harness self-test.

`GITHUB_TOKEN` for this workflow is `contents: read`. That checks out the repository. The artifact upload action obtains its own Actions runtime token. This workflow does not grant `actions: write`. It does not write repository contents, open or merge pull requests, or close issues.

Dispatch inputs are job environment values. `tests/acceptance-lab/src/ci-gate.mjs` accepts a suite of `smoke`, `full`, or `soak` and an integer seed before the launcher command is built. A quote or shell metacharacter in the seed is rejected there. A pull request cannot select `full` or `soak`.

The browser publish directory must contain `playwright.json`, `self-test.json`, and `report.json`. A dispatched `full` or `soak` must also produce the real-engine `summary.json`. The daemon image upload fails when the image is missing. A missing required runtime artifact fails the workflow with `infrastructure FAIL`. The files that attempt did produce are still uploaded, including `infrastructure-failure.txt` when the required set is absent. A later attempt does not replace that record. A product `FAIL` inside a complete bundle stays a suite failure for the product owner.

Syntax, type, and harness-detector checks do not qualify the services, the browser, or runtime fault detection. Those stay **NOT_RUN** until a GitHub-hosted run records them. A smoke result does not qualify the full suite.

## Reading a failure

`publish/report.md` lists the suite result, whether the audit SHA covers the candidate, and each operation. Infrastructure failures stay infrastructure failures. Product failures stay on their existing owners. This lab does not patch production Rust, the web UI, shared lockfiles, or PR 194 to make a red case green.
