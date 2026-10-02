# GHE Live Validation Guide

Real end-to-end bidirectional validation of RepoSync against a live GitHub Enterprise instance and a live SVN repository. Unlike `controlled-validation.sh` (which uses local `file://` SVN repos), a **non-dry-run** invocation exercises the network path named by the environment. That path is tier 3 or tier 4 in [`docs/enterprise-soak-runbook.md`](enterprise-soak-runbook.md), not a local soak.

This guide is not approval to run it. Tier 3 needs Chris/admin approval, an explicit allowlist, and dedicated credentials on a disposable target. Tier 4 (active production) is Chris's decision. Agents do not self-authorize either. `--dry-run` never calls GitHub Enterprise or SVN, even if credentials are present, and it is not enterprise qualification.

local/offline PASS is not enterprise/live PASS. Record any real run in [`docs/reliability/candidate-report-template.md`](reliability/candidate-report-template.md) at the exact candidate SHA. Do not combine this script's result with `enterprise-soak.sh` or offline self-tests into one green.

## Prerequisites

| Tool | Minimum | Check |
|------|---------|-------|
| Rust | 1.70+ | `rustc --version` |
| Cargo | 1.70+ | `cargo --version` |
| SVN | 1.14+ | `svn --version` |
| Git | 2.30+ | `git --version` |
| curl | 7.x+ | `curl --version` |
| jq | 1.6+ | `jq --version` |

All tools must be on `$PATH`.

## Environment Variables

### Required (for live run)

| Variable | Description | Example |
|----------|-------------|---------|
| `GHE_API_URL` | GitHub Enterprise API base URL | `https://github.example.com/api/v3` |
| `GHE_TOKEN` | GitHub PAT with `repo` scope | `ghp_abc123...` |
| `GHE_OWNER` | Repository owner or organization | `myorg` |
| `GHE_REPO` | Repository name (created if missing) | `reposync-canary` |
| `SVN_URL` | SVN repository URL (must be writable) | `https://svn.example.com/repos/trunk` |
| `SVN_USERNAME` | SVN username | `svc-reposync` |
| `SVN_PASSWORD` | SVN password | (secret) |

### Optional

| Variable | Description | Default |
|----------|-------------|---------|
| `GHE_WEB_URL` | GitHub Enterprise web base URL | Derived from `GHE_API_URL` |
| `REPOSYNC_CONFIG` | Path to reposync personal config | Auto-generated |

## Quick Start

```bash
# 1. Local preflight only (no live API calls, credentials ignored).
#    Not enterprise qualification.
scripts/ghe-live-validation.sh --dry-run

# 2. Live steps below contact the named endpoints.
#    Do not run them without Chris/admin approval of an allowlisted
#    disposable target and dedicated credentials. Not production.
export GHE_API_URL="https://github.example.com/api/v3"
export GHE_TOKEN="ghp_your_token_here"
export GHE_OWNER="myorg"
export GHE_REPO="reposync-canary"
export SVN_URL="https://svn.example.com/repos/trunk"
export SVN_USERNAME="svc-reposync"
export SVN_PASSWORD="your_svn_password"

# 3. Single-cycle live run (recommended first time)
scripts/ghe-live-validation.sh --cycles 1

# 4. Multi-cycle with interval
scripts/ghe-live-validation.sh --cycles 5 --interval 10

# 5. Strict mode (abort on first failure)
scripts/ghe-live-validation.sh --strict --cycles 3
```

Or via Makefile:

```bash
make validate-ghe-live-dry-run          # preflight
make validate-ghe-live                  # 1-cycle live run
```

## Scenario Matrix (11 Scenarios per Cycle)

Each cycle executes these scenarios in order:

| # | Scenario | Direction | What It Tests |
|---|----------|-----------|---------------|
| S1 | SVN add file | SVN→ | Create a new file via `svn commit`, verify content with `svn cat` |
| S2 | SVN modify file | SVN→ | Modify an existing file, verify updated content |
| S3 | SVN delete file | SVN→ | Delete a file via `svn rm`, verify it's gone |
| S4 | SVN nested dirs | SVN→ | Create deeply nested directory structure, verify leaf file |
| S4b | **SVN→Git sync** | SVN→Git | **Invoke `reposync-personal sync`, pull Git repo (with auth), verify SVN files appear in Git. Fails on pull auth error.** |
| S5 | Git branch + commit | →Git | Create feature branch via refs API, commit file on branch |
| S6 | Open + merge PR | →Git | Open PR from feature branch, squash-merge via API |
| S7 | **Git→SVN sync** | Git→SVN | **Invoke `reposync-personal sync`, verify (a) PR file content in SVN via `svn cat`, (b) SVN log `Git-Commit:` matches exact merge SHA from S6, (c) SVN log `PR:` matches exact PR number from S6** |
| S8 | Echo marker | SVN→ | Commit with `[reposync]` marker, verify in `svn log --xml` |
| S9 | API rate limit | →Git | Check `/rate_limit` endpoint, verify >100 requests remaining |
| S10 | Log-probe | Local | Spawn `reposync-personal log-probe`, verify `personal.log` written |

> **S4b and S7 are the critical cross-system sync scenarios.** They invoke the actual RepoSync
> sync engine (`reposync-personal sync`) and verify that changes made on one side arrive on the
> other side.
>
> **S5→S6→S7 form a PR-based Git→SVN proof.** RepoSync only replays *merged PR* commits from
> Git to SVN (direct pushes to main are not synced). These three scenarios exercise the real
> production workflow: create a feature branch, commit via the Contents API, open and squash-merge
> a PR, then run sync and verify the file content lands in SVN.
>
> **S7 exact provenance matching:** S7 requires an **exact** match between the replayed SVN
> commit metadata and the values recorded in S6. Specifically, the `Git-Commit:` trailer must
> contain the exact merge SHA from `s6-merge-sha.txt`, and the `PR:` trailer must reference the
> exact PR number from `s6-pr-number.txt`. A generic "some trailer exists" is insufficient —
> only an exact match to *this cycle's* PR proves the replay actually occurred. S7 fails if
> (a) file content doesn't match, (b) expected SHA or PR# is missing from S6 artifacts, or
> (c) the trailer values don't exactly match.
>
> **Provenance self-test:** Run `scripts/test-s7-provenance.sh` offline to verify the metadata
> matching logic. This test is also executed in CI (both `ci.yml` and `e2e.yml`).
>
> Sync engine logs are captured in `cycle-NNN/sync-engine-data/`.

## CLI Options

```
Usage: scripts/ghe-live-validation.sh [OPTIONS]

Options:
  --dry-run          Local tool preflight only (no live API calls).
                     Not enterprise qualification.
  --cycles N         Number of validation cycles (default: 1)
  --interval N       Seconds between cycles (default: 5)
  --strict           Fail immediately on any scenario failure
  --config PATH      Path to reposync personal config
  --artifacts-dir D  Override artifact output directory
  --help             Show this help
```

## Output Artifacts

Each run produces a timestamped artifact bundle:

```
artifacts/ghe-live-validation/<UTC_TIMESTAMP>/
├── timeline.log            # Human-readable real-time progress
├── events.ndjson           # Machine-readable event stream
├── summary.md              # Go/No-Go report with scenario table
├── manifest.json           # Full artifact listing with sizes
├── env-snapshot.txt        # Sanitized environment (no secrets)
├── tool-versions.txt       # Tool versions
├── verification/
│   ├── svn-info.txt        # SVN connection info (dry-run)
│   ├── svn-checkout.log    # SVN checkout output
│   ├── git-clone.log       # Git clone output
│   └── leak-scan.log       # Secret scan results
└── cycle-001/              # Per-cycle artifacts
    ├── s1-commit.log       # SVN commit output per scenario
    ├── s4b-sync-stdout.log # SVN→Git sync engine stdout
    ├── s4b-sync-stderr.log # SVN→Git sync engine stderr
    ├── s4b-git-pull.log    # Git pull after sync
    ├── s5-git-sha.txt      # Git commit SHA (feature branch)
    ├── s6-pr-number.txt    # PR number
    ├── s6-merge-sha.txt    # PR merge commit SHA
    ├── s7-sync-stdout.log  # Git→SVN sync engine stdout
    ├── s7-sync-stderr.log  # Git→SVN sync engine stderr
    ├── s7-svn-update.log   # SVN update after sync
    ├── s7-svn-log.xml      # SVN log XML for metadata verification
    ├── s9-rate-limit.json  # GHE rate limit response
    ├── s10-probe-stdout.log
    ├── s10-probe-stderr.log
    ├── daemon.log          # Captured personal.log
    └── sync-engine-data/   # RepoSync data dir (DB, logs)
```

### events.ndjson Format

```json
{"timestamp":"2026-02-24T12:00:00Z","phase":"scenario","action":"s1-svn-add","status":"pass","duration_ms":0}
{"timestamp":"2026-02-24T12:00:01Z","phase":"cycle-1","action":"complete","status":"pass","duration_ms":15000}
```

## Go/No-Go Criteria

### Automatic (script enforced)

| Criterion | Threshold | Behavior |
|-----------|-----------|----------|
| Any scenario failure | > 0 failures | Exit code 1 (NO-GO) |
| Strict mode failure | First failure | Immediate abort |
| Secret leakage | Any match | Flagged in summary |

### Manual Review

A scenario pass on one host is not Chris's active-environment GO and does not by itself qualify a candidate SHA. Apply the go/no-go checklist in [`docs/enterprise-soak-runbook.md`](enterprise-soak-runbook.md) and file [`docs/reliability/candidate-report-template.md`](reliability/candidate-report-template.md).

Before anyone treats a tier 3 bundle as evidence, verify:

- [ ] The run was approved, allowlisted, and aimed at a disposable target
- [ ] The report names this exact head SHA and artifact digest
- [ ] All 11 scenarios PASS for every cycle, in both directions where the scenario says so
- [ ] Content and provenance match this cycle (data-integrity failures are NO-GO)
- [ ] No secret patterns in artifact files
- [ ] Rate limit headroom is sufficient (>100 remaining)
- [ ] SVN commit latency is acceptable
- [ ] GHE API response times are acceptable
- [ ] `personal.log` output is well-formed
- [ ] No unexpected error patterns in `events.ndjson`
- [ ] Dry-run output was not counted as a live PASS

## Failure Triage

| Scenario | Common Causes | What to Check |
|----------|---------------|---------------|
| S1-S4 (SVN) | Auth failure, read-only repo, network | `cycle-NNN/sN-commit.log`, SVN access |
| S4b (SVN→Git sync) | Git pull auth, token scope, empty sync | `cycle-NNN/s4b-git-pull.log`, `s4b-sync-*.log` |
| S5-S6 (Git PR) | Token scope, repo permissions, merge conflict | `cycle-NNN/s5-error.json`, `s6-*-error.json`, token scopes |
| S7 (Git→SVN sync) | PR not detected, missing metadata trailers, SVN auth | `cycle-NNN/s7-sync-*.log`, `s7-svn-log.xml`, `s7-svn-update.log` |
| S8 (echo) | SVN log format differs | `svn log --xml` output manually |
| S9 (rate limit) | Token exhausted | `cycle-NNN/s9-rate-limit.json` |
| S10 (log-probe) | Binary not built, config error | `cycle-NNN/s10-probe-stderr.log` |

## Rollback Procedure

If issues are found after an approved run, follow the backup and roll-forward rules in [`docs/enterprise-soak-runbook.md`](enterprise-soak-runbook.md). After new Git or SVN writes, stop, reconcile, and roll forward under #63/#64. Do not blind-reset a watermark or checkpoint, and do not restore an older database over a daemon that has already published commits.

1. **Stop the daemon immediately** and keep production writers quiescent:
   ```bash
   reposync-personal --config <path> stop
   ```

2. **Verify it stopped** and that a single authoritative writer is identified:
   ```bash
   reposync-personal --config <path> status
   # Should show "○ Not running"
   ```

3. **Copy incident artifacts** (a copy is not a restore):
   ```bash
   cp personal.db personal.db.incident-$(date +%Y%m%d)
   cp personal.log personal.log.incident-$(date +%Y%m%d)
   ```

4. **Reconcile** the forensic copy with the actual Git and SVN revisions that were published. Roll forward from that external state. Verify configured targets before any resume.

## CI Integration

Both `ci.yml` and `e2e.yml` workflows run:

1. **S7 provenance self-test** (`scripts/test-s7-provenance.sh`) — verifies the exact-match
   metadata logic offline. Fails the PR if any of the 8 test cases regress.

2. **Large-file & LFS validation** (`scripts/large-file-validation.sh --quick`) — exercises
   file-policy enforcement and LFS utilities. CI installs `git-lfs` so the LFS preflight
   scenario is **executed, not skipped**. The workflow fails if LFS preflight is skipped
   (indicating `git-lfs` was not installed).

3. **GHE live validation dry-run** (`scripts/ghe-live-validation.sh --dry-run`) — preflight
   tool verification (no network credentials needed).

## Relationship to Other Validation Scripts

| Script | Tier | Network | What a pass is |
|--------|------|---------|----------------|
| `controlled-validation.sh` | Local | No | Local pre-merge checks only |
| `enterprise-soak.sh` | 1 local `file://` | No | Local cycle stability only. Enterprise qualification stays NOT RUN. |
| `test-s7-provenance.sh` | 2 offline | No | Provenance-matcher regression only |
| `large-file-validation.sh` | Local | No | File-policy and LFS checks only |
| `ghe-live-validation.sh --dry-run` | Preflight | No | Tool preflight. Not enterprise qualification. |
| `ghe-live-validation.sh` | 3 or 4 | **Yes** | Live scenarios on the named target, only after approval. Not a combined green with the rows above. |

Do not run the live script as the automatic next step after a local soak. Tier 3 requires the go/no-go checklist and a candidate report. Tier 4 requires Chris.
