# #62 acceptance matrix (current main)

This is the host-side rollup for epic [#61](https://github.com/chriscase/RepoSync/issues/61) / [#62](https://github.com/chriscase/RepoSync/issues/62). It does **not** replace the sealed Docker runner.

## Entry points

| Command | What it proves | What it does not prove |
| --- | --- | --- |
| `python3 scripts/reliability-acceptance-matrix.py --check --report` | Manifest completeness, R01–R24 rollup, GOAL hash, no silent PASS | Runtime SVN/Git correctness |
| `scripts/real-engine-scenario-suite.sh` + `real-engine-scenario-report.py ci-gate` | Host svnserve real-engine scenarios; suite exits `3` on PARTIAL but `1` if any scenario FAIL; ci-gate requires every cataloged real-engine case id and rejects SKIP | Full R16 catalog beyond loopback fixtures |
| `scripts/reliability-container.sh --all` | Isolated required-case execution against disposable remotes | Deployed-version or live acceptance |
| `scripts/reliability-compare.sh` | Matched-lock base vs candidate identities | Production upgrade |

`docs/reliability/acceptance-matrix.json` is the source of truth for R01–R24 status. `docs/reliability/required-cases.json` remains the exact isolated-case catalog. `docs/reliability/scenarios.json` is regenerated from the matrix and must stay in agreement.

A scenario may be marked **PARTIAL** when named subcases pass and required issue criteria remain open. **PASS** is reserved for a complete issue-level criterion with executed exact cases and an empty open list. Missing tools, filtered cases, or an unavailable Docker daemon are **NOT RUN**, never PASS.

### Real-engine CI gate (loopback svnserve)

`scripts/real-engine-scenario-suite.sh` still exits `3` whenever the rollup is **PARTIAL** so local runs stay honest. GitHub Actions runs `python3 scripts/real-engine-scenario-report.py ci-gate` on the emitted `artifacts/real-engine-scenarios/*/summary.json` afterward. The gate passes only when every scenario is **PASS**, or **PARTIAL** with an id listed in `docs/reliability/real-engine-ci-partial-allowlist.json` (today the two loopback R16 svnserve unreachable cases). Any **FAIL**, any **NOT RUN**, or any **PARTIAL** not on that allowlist keeps Build & Test red. Checker fixtures: `python3 scripts/real-engine-scenario-report.py self-test`.

The deployed executable/schema is **NOT ESTABLISHED**. No live acceptance is authorized from this matrix.

Host catalog PASS is not enterprise or live acceptance. Qualification tiers, the #41 go/no-go checklist, and the candidate report Chris uses to decide are in [the enterprise soak runbook](../enterprise-soak-runbook.md) and [candidate-report-template.md](candidate-report-template.md). Until the installed version is known, that version's migration coverage stays **NOT RUN**. local/offline PASS is not enterprise/live PASS.

## Regenerating the human scenario file

```sh
python3 scripts/reliability-acceptance-matrix.py --sync-scenarios
```
