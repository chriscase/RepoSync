# Enterprise Soak/Canary Validation Runbook

Qualification gate for RepoSync before any enterprise or production enablement (GitHub Enterprise Cloud/Server and legacy SVN). This runbook is the #41 gate for epic #61. It does not authorize a live run.

`scripts/enterprise-soak.sh` is **tier 1 only**: repeated cycles against local `file://` SVN repositories and the log-probe subsystem. It does not contact GitHub or a network SVN server. A local or offline PASS is not an enterprise or live PASS for any candidate SHA. February 2026 offline soak and GHE dry-run evidence does not qualify the current candidate and must not be folded into one green result with mock or offline checks.

Record outcomes in [`docs/reliability/candidate-report-template.md`](reliability/candidate-report-template.md). Chris decides live acceptance and deployment. Implementation agents do not self-authorize either.

Live network validation, when separately approved, is documented in [`docs/ghe-live-validation-guide.md`](ghe-live-validation-guide.md). That guide is not approval to run it.

## Qualification tiers

Keep these tiers distinct. Never combine them into one PASS.

| Tier | What it is | What a pass proves | Typical command |
|------|------------|--------------------|-----------------|
| 1. Local engine | `enterprise-soak.sh` using `file://` SVN | Local cycle stability and log-probe health only | `scripts/enterprise-soak.sh` |
| 2. Local HTTP-provider / offline self-tests | Provider or API self-tests and offline provenance checks with no enterprise endpoint | Those offline assertions only | `scripts/test-s7-provenance.sh` and the local suites named in their own docs |
| 3. Disposable allowlisted enterprise | A dedicated GH Enterprise + SVN target that is not an active production repo | Both-direction content and provenance on that disposable target, at the exact candidate SHA | `scripts/ghe-live-validation.sh` only after the gate below |
| 4. Active-production acceptance | The environment people actually use | Chris's explicit acceptance decision | No agent command. Chris decides. |

### Tier 1 — Local engine (`enterprise-soak.sh`, file://)

QUALIFICATION-TIER: local-file-engine

- No GitHub API, no enterprise hostname, no production database.
- Synthetic canary commits stay inside the script's temporary local repository.
- Exit status reflects the local error-rate threshold only.
- The summary always reports enterprise qualification as **NOT RUN**.
- `--dry-run` is preflight only. It is not a soak and not qualification.

### Tier 2 — Local HTTP-provider and offline self-tests

- Includes offline provenance matching (`scripts/test-s7-provenance.sh`) and any local HTTP-provider harness that never leaves the machine.
- A green self-test does not exercise GH Enterprise auth, webhooks, branch protection, or real SVN.
- Report this tier separately from tier 1. Do not cite it as enterprise evidence.

### Tier 3 — Disposable allowlisted enterprise target

Required before this tier may run:

- Chris or an admin has approved this specific run in writing.
- The target is on an explicit allowlist and is **not** an actively used production repository.
- Credentials are dedicated to the disposable target. Do not reuse production tokens.
- The candidate report names the exact base SHA, head SHA, and artifact digest under test.

This tier is still not active-production acceptance.

### Tier 4 — Active-production acceptance

Chris decides. Agents never self-authorize production enablement, deployment, webhook or ruleset changes, runner configuration, or synthetic commits in repositories people are using.

## Release-gate order

Aligned with `docs/reliability/GOAL.md` sections 5 and 6 (that file is not edited by this runbook):

**unit/component → actual local engine → local provider/API/UI → old-install upgrade → disposable authorized enterprise target → Chris's active-environment acceptance.**

Before any active-environment acceptance:

- #62's isolated scenario matrix and #63's upgrade/restore fixtures must pass at the **exact candidate SHA**.
- The installed version and configuration must be obtained safely. Until the installed version is known, report that version's migration coverage as **NOT RUN**. Do not guess a schema by maximum revision or an arbitrary SHA.
- Historical #41 script output is not blanket approval. Requalify it for this SHA.

local/offline PASS is not enterprise/live PASS.

## Environment topology

The picture below is the **intended production shape**. Tier 1 does not deploy it. Tier 3 uses a disposable copy of this shape. Tier 4 is the real one, and only Chris accepts it.

```
┌─────────────┐     ┌────────────────┐     ┌──────────────┐
│ SVN Server   │◄───►│ RepoSync       │◄───►│ GitHub       │
│ (on-prem/    │     │ daemon         │     │ Enterprise   │
│  hosted)     │     │                │     │ (Cloud/      │
│              │     │ personal.db    │     │  Server)     │
│              │     │ personal.log   │     │              │
└─────────────┘     └────────────────┘     └──────────────┘
```

## Prerequisites for an approved tier 3 run

Do not collect or invent these credentials in order to satisfy the checklist. Record them as NOT RUN until an approved run exists.

| Token | Required scopes | Purpose |
|-------|-----------------|---------|
| GitHub PAT (dedicated) | `repo`, `read:org` | API access for sync on the disposable repo |
| SVN credentials (dedicated) | Read/write on the disposable path | SVN checkout, commit, log |

Permissions an approved operator must confirm on the disposable target:

- SVN read/write on the synchronization path
- GitHub push and the ability to create and merge PRs in the disposable repo
- Network path from the daemon host to that SVN server and that GitHub API
- Webhook delivery and signature checks, if webhooks are in scope
- Branch protection: sync must not violate rules, and rules must not be weakened to make the soak pass
- GHE Server rate limits (watch for HTTP 429)
- Enterprise audit log shows the dedicated sync identity

### Pre-soak checklist (tier 3 only)

- [ ] Written Chris/admin approval for this allowlisted target
- [ ] Candidate report drafted with exact SHAs and artifact digest
- [ ] #62 matrix and #63 fixtures already PASS at that same SHA
- [ ] Installed-version migration coverage recorded, or explicitly NOT RUN
- [ ] Disposable repo only; production writers are not this target
- [ ] `svn info <url>` succeeds for the disposable URL
- [ ] GitHub API identity check succeeds with the dedicated token
- [ ] Personal config points at the disposable endpoints (`reposync-personal --config <path> status`)
- [ ] Data directory is writable

## Running tier 1 (local, no external services)

```bash
# Preflight only. Not a soak and not enterprise qualification.
scripts/enterprise-soak.sh --dry-run

# Short local soak (5 cycles, 2s interval) — default
scripts/enterprise-soak.sh

# Extended local soak (50 cycles, 10s interval)
scripts/enterprise-soak.sh --cycles 50 --interval 10

# Stricter local error-rate threshold
scripts/enterprise-soak.sh --cycles 20 --max-error-rate 0
```

`--dry-run` checks tools and that the workspace binary exists or builds. It does not run cycles. A preflight PASS is not tier 1 evidence and is not enterprise qualification.

### What each local cycle does

1. Injects a unique file into the **local** SVN repository.
2. Reads it back and checks a byte-exact match.
3. Runs `reposync-personal log-probe`.
4. Records SVN head revision, disk usage, and timing.

Any cycle that fails content verification is a local data-integrity failure. The script's error-rate threshold can still exit 0 when some cycles failed. That exit code is **not** an enterprise GO. Data-integrity failures are NO-GO for qualification even when the aggregate error rate is inside the threshold.

## Tier 3 command (do not run without approval)

```bash
# Local preflight only. Does not call GHE or SVN. Not qualification.
scripts/ghe-live-validation.sh --dry-run
```

The live form (`scripts/ghe-live-validation.sh` without `--dry-run`) contacts the endpoints named by `GHE_*` and `SVN_*`. It is out of scope unless tier 3's approval gate is already satisfied. This repository's agents must not start that live path.

## Go/no-go checklist

Use PASS / FAIL / PARTIAL / NOT RUN per tier. A single rolled-up green is not acceptable.

### Must be true before active-environment acceptance

- [ ] #62 isolated scenario matrix PASS at the exact candidate SHA
- [ ] #63 upgrade/restore fixtures PASS at the exact candidate SHA
- [ ] Installed version known, or that version's migration coverage reported as NOT RUN
- [ ] Tier 1, tier 2, tier 3, and tier 4 outcomes recorded separately
- [ ] Both directions assert content and provenance (SVN→Git and Git→SVN), including no duplicate and no lost commits
- [ ] Data-integrity failures treated as NO-GO even if the aggregate error rate is acceptable
- [ ] Cancellation/restart, late pairing, rewrite containment, verified snapshot initialization, coordinated refresh, normal sync after upgrade, and credential inheritance/rotation each marked PASS or left disabled and explicitly unqualified
- [ ] Backup, restore, and roll-forward procedures reviewed (section below)
- [ ] Production writers quiescent during an approved upgrade or backup
- [ ] One authoritative writer, and configured targets verified before resume
- [ ] No synthetic commits in actively used repos unless a separate explicit approval says so
- [ ] Candidate report published with exact base/head SHA, artifact digest, host/version/permissions, limitations, and a GO / NO-GO recommendation
- [ ] Chris has made the live-acceptance decision. An agent recommendation is not that decision.

Untested optional features stay disabled. Do not enable them because a neighboring tier passed.

### Automatic tier 1 signals (not an enterprise gate)

| Signal | Meaning |
|--------|---------|
| Local error rate above `--max-error-rate` (default 20%) | Script exits non-zero. Local soak is NO-GO. |
| Local error rate within threshold and every cycle passed | Local stability only. Enterprise qualification remains NOT RUN. |
| Any cycle content or log-probe failure | Local data-integrity failure. Enterprise qualification is NO-GO even if the script exits 0. |
| Secret-shaped tokens in artifacts | Stop and scrub. Do not publish the bundle. |

### Manual review when a tier 3 bundle exists

- [ ] Both directions match exact content, not only command exit codes
- [ ] Provenance trailers match this cycle's merge SHA and PR number
- [ ] Timing is stable across cycles
- [ ] SVN revisions advance only as the scenario requires
- [ ] `events.ndjson` has no unexplained errors
- [ ] `personal.log` is well-formed
- [ ] Disk use is bounded
- [ ] Working copies are clean between cycles
- [ ] No secrets in the artifact bundle
- [ ] Webhook and branch-protection notes are filled in or explicitly NOT RUN

## Acceptance thresholds (tier 1 local soak)

These numbers judge the local soak only. Meeting them does not qualify GH Enterprise.

| Metric | Local soak acceptable | Needs investigation |
|--------|----------------------|---------------------|
| Cycle pass rate | ≥ 95%, and zero content mismatches for qualification use | < 95%, or any content mismatch |
| SVN commit latency | < 5s per cycle | > 5s |
| Log probe success | 100% | < 100% |
| Disk growth per cycle | < 1MB | > 5MB |

For tier 3, any data-integrity failure is NO-GO regardless of these rates.

## Backup, restore, and roll-forward

SQLite, Git, and SVN are not one atomic transaction. Recovery has to follow what was actually written.

### Before any new Git or SVN write

On a disposable copy, an older executable plus a consistent pre-write snapshot can be tested as rollback. Prove DB and WAL consistency, configuration, permissions, and refs before trusting that snapshot. This does not qualify a restore performed after later publishes.

### After new Git or SVN writes

Stop, reconcile, and roll forward. Do not use a blind watermark reset or an old database restore as general rollback. In particular, do not restore an older database over a daemon that has already published commits.

1. Stop the daemon and confirm it is not running. Keep every production writer quiescent for an approved upgrade or backup window.
2. Confirm there is a single authoritative writer for the pair.
3. Copy the current database and log for forensics. Copying is not restoring.
4. Reconcile the external Git and SVN state with recorded mappings under #63 and #64. Identify the actual published effect. Do not replay from an obsolete checkpoint.
5. Roll forward from that reconciled state. Never promise a lossless downgrade that drops mappings the new version needs.
6. Verify the configured targets, then resume only when the operator intends to.

Forbidden as general rollback after external writes:

- Resetting a watermark, checkpoint, or "last synced" cursor to an older value to skip or replay history
- Replacing the live database with a snapshot taken before those commits
- Force-push, remote deletion, or an independent `svn merge` to "undo" the sync
- Synthetic repair commits in a repository people are using, unless a separate approval allows them

`personal.log` and a forensic copy of the current database still belong in the incident bundle.

## Incident capture checklist

- [ ] `personal.log` (full, not truncated)
- [ ] Forensic copy of the current `personal.db` (do not overwrite the live file with an older one)
- [ ] Which qualification tier this run was, and the candidate SHA
- [ ] SVN server logs, if the tier was allowed to use SVN and the logs are available
- [ ] GitHub API rate-limit headers, if a tier 3 call was approved
- [ ] Connectivity notes with secrets removed
- [ ] Sanitized environment (no tokens)
- [ ] Daemon process status
- [ ] Disk space
- [ ] Script artifact bundle, if this was a scripted run
- [ ] Whether any Git or SVN write had already succeeded

## Output artifacts (tier 1)

```
artifacts/enterprise-soak/<UTC_TIMESTAMP>/
├── timeline.log
├── events.ndjson
├── summary.md
├── manifest.json
├── env-snapshot.txt
├── tool-versions.txt
├── health-snapshots/
└── cycle-001/
```

`summary.md` states the tier (`local-file-engine`) and `Enterprise qualification: NOT RUN`. Do not relabel that summary as a GHE GO.

### events.ndjson shape

```json
{"timestamp":"2026-02-24T12:00:00Z","phase":"cycle-1","action":"svn-commit","status":"pass","duration_ms":150,"svn_rev":"42"}
```

## Candidate report

Every claim of qualification uses [`docs/reliability/candidate-report-template.md`](reliability/candidate-report-template.md). The worked example in that file is illustrative. It is not a live run and not evidence for current main.

Host catalog check (does not execute enterprise tests):

```bash
python3 scripts/reliability-acceptance-matrix.py --check
```
