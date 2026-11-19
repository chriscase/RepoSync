# Controlled-Environment Validation Guide

Run the build, workspace tests and local sync scenarios together to collect a single validation bundle.

## Prerequisites

| Tool | Version or component | Check |
|------|----------------------|-------|
| Rust and Cargo | Stable toolchain with Clippy | `rustc --version`, `cargo --version`, `cargo clippy --version` |
| Python | Python 3 | `python3 --version` |
| SVN | 1.14+ | `svn --version` |
| SVN utilities | 1.14+ | `svnadmin --version`, `svnserve --version`, `svnlook --version` |
| Git | 2.30+ | `git --version` |

Put these tools on `$PATH` and cache the Cargo dependencies before an offline run. Phase 4 uses a local `file://` SVN repository. The default Phase 7 fixtures start `svnserve` on loopback and use local bare Git repositories. The script runs with the host's network access.

## Quick Start

```bash
# Full validation (recommended)
scripts/controlled-validation.sh

# Quick mode (fewer SVN revisions in Phase 4)
scripts/controlled-validation.sh --quick
```

The script runs unattended. A preflight or build failure stops the run early and leaves the artifacts produced so far.

## What Is Being Tested

### Phase 1: Build
Full workspace compilation via `cargo build --workspace`. If this fails, subsequent phases are skipped.

### Phase 2: Cargo Tests
Runs `cargo test --workspace` which exercises:
- Core library unit tests (sync engine, database, SVN/Git clients)
- Team-mode end-to-end tests (bidirectional sync, conflict detection, audit logging)
- Personal-mode integration tests (20+ scenarios)
- Spawn-based black-box logging tests (log-probe)
- CLI unit tests

### Phase 3: Clippy
Static analysis with `-D warnings` to catch lints.

### Phase 4: Live SVN Scenarios
Creates real local SVN repos and exercises:
- **Basic SVN commits** — multiple revisions verified
- **Echo suppression** — `[reposync]` marker detection
- **Conflict path** — file modification tracking
- **File deletion** — propagation verification
- **Nested directories** — deep path handling

### Phase 5: Log Probe
Spawns the real `reposync-personal` binary with the `log-probe` subcommand and verifies:
- Log file created at `{data_dir}/personal.log`
- Log messages appear with correct markers

### Phase 6: Secret Redaction
Scans the artifacts generated so far for leaked token patterns (ghp_, gho_, ghs_, etc.).

### Phase 7 — Real-engine scenarios

The scenario suite drives the team sync engine against disposable SVN and Git repositories. The wrapper reads the summary from this invocation and checks it against the suite exit code. The summary records each scenario's status and evidence.

## Expected Outcomes

| Phase | Expected |
|-------|----------|
| Build | PASS — zero errors |
| Cargo tests | PASS — all tests green |
| Clippy | PASS — zero warnings |
| Live SVN | PASS — all 5 scenarios pass |
| Log probe | PASS — personal.log written |
| Secret scan | PASS — no tokens found |
| Real-engine scenarios | PASS when every required scenario passes |

| Wrapper exit | Overall result | Interpretation |
|--------------|----------------|----------------|
| 0 | PASS | All phases passed. |
| 1 | FAIL | A phase failed, or the current suite result lacks qualifying evidence. A missing, malformed or contradictory summary fails Phase 7. |
| 2 | NOT RUN | Every required real-engine scenario reported NOT RUN and the earlier phases passed. |
| 3 | PARTIAL | The earlier phases passed and the CI gate accepted the suite's PARTIAL report. |

An earlier phase failure takes precedence over a later PARTIAL or NOT RUN. For PASS and PARTIAL, Phase 7 applies the same evidence checks as CI. Every expected scenario must appear exactly once. A skipped or unrun scenario, a PARTIAL outside the allowlist, or missing required R01 checkpoint-recovery evidence makes the phase FAIL. The wrapper keeps a qualifying PARTIAL visible as exit 3. [Real-engine CI policy](reliability/acceptance-matrix.md)

## Output Artifacts

Each run creates a bundle named for its UTC start time. A suffix distinguishes bundles that start in the same second.

```
artifacts/controlled-validation/<UTC_TIMESTAMP>/
├── timeline.log          # Consolidated human-readable log
├── events.ndjson         # Machine-readable event stream
├── summary.md            # Overall result and phase results
├── manifest.json         # Full artifact listing
├── env-snapshot.txt      # Environment filtered by variable name
├── tool-versions.txt     # Tool versions
├── build/
│   ├── stdout.log
│   └── stderr.log
├── cargo-test/
│   ├── stdout.log
│   └── stderr.log
├── clippy/
│   ├── stdout.log
│   └── stderr.log
├── local-svn-scenarios/       # SVN scenario artifacts
├── log-probe/
│   ├── stdout.log
│   ├── stderr.log
│   └── personal.log      # Captured log output
├── redaction/            # Secret scan results
└── real-engine-scenarios/
    ├── suite.log
    └── <UTC_TIMESTAMP>/
        └── summary.json  # Current suite result and per-scenario evidence
```

### events.ndjson Format

Each line is a JSON object:
```json
{"timestamp":"2026-02-24T12:00:00Z","phase":"cargo-test","action":"complete","status":"pass","duration_ms":15000}
```

Fields: `timestamp`, `phase`, `action`, `status`, `duration_ms`, plus optional `svn_rev`, `git_sha`, `pr_number`.

The `real-engine-scenarios` completion event records the Phase 7 verdict as `pass`, `fail`, `not_run` or `partial`. Its `suite_exit` field is the original child process exit code. The bundle's `manifest.json` and `summary.md` give the overall wrapper result.

## Failure Triage

1. **Build failure**: Check `build/stderr.log` for compiler errors.
2. **Test failure**: Check `cargo-test/stderr.log` for failing test names and assertions.
3. **Clippy failure**: Check `clippy/stderr.log` for lint violations.
4. **SVN scenario failure**: Check `local-svn-scenarios/` and `timeline.log` for which scenario failed.
5. **Log probe failure**: Check `log-probe/stderr.log` and verify the binary exists at `target/debug/reposync-personal`.
6. **Secret leak**: Check `redaction/` — review the flagged file and determine if it's a real leak or false positive.
7. **Real-engine result.** Inspect `real-engine-scenarios/<UTC_TIMESTAMP>/summary.json` inside this bundle. Use `real-engine-scenarios/suite.log` for the child process output, including errors that prevented it from producing a summary.

## Cleanup

The script automatically cleans up temporary directories on exit. Artifact directories are preserved for review. To clean old artifacts:

```bash
rm -rf artifacts/controlled-validation/
```

## CI Integration

The script can be added to CI pipelines directly:

```yaml
- name: Controlled validation
  run: scripts/controlled-validation.sh --quick
```

In this workflow step, exits 1, 2 and 3 all produce a failing step. The `make validate` and `make validate-quick` targets also treat any nonzero script exit as a recipe error. Read `manifest.json` for the specific outcome or invoke the script directly to collect its exit code.
