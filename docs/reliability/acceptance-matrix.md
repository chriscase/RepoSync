# #62 acceptance matrix (current main)

This is the host-side rollup for epic [#61](https://github.com/chriscase/RepoSync/issues/61) / [#62](https://github.com/chriscase/RepoSync/issues/62). It does **not** replace the sealed Docker runner.

## Entry points

| Command | What it proves | What it does not prove |
| --- | --- | --- |
| `python3 scripts/reliability-acceptance-matrix.py --check --report` | Manifest completeness, R01–R24 rollup, GOAL hash, no silent PASS | Runtime SVN/Git correctness |
| `scripts/reliability-container.sh --all` | Isolated required-case execution against disposable remotes | Deployed-version or live acceptance |
| `scripts/reliability-compare.sh` | Matched-lock base vs candidate identities | Production upgrade |

`docs/reliability/acceptance-matrix.json` is the source of truth for R01–R24 status. `docs/reliability/required-cases.json` remains the exact isolated-case catalog. `docs/reliability/scenarios.json` is regenerated from the matrix and must stay in agreement.

A scenario may be marked **PARTIAL** when named subcases pass and required issue criteria remain open. **PASS** is reserved for a complete issue-level criterion with executed exact cases and an empty open list. Missing tools, filtered cases, or an unavailable Docker daemon are **NOT RUN**, never PASS.

The deployed executable/schema is **NOT ESTABLISHED**. No live acceptance is authorized from this matrix.

## Regenerating the human scenario file

```sh
python3 scripts/reliability-acceptance-matrix.py --sync-scenarios
```
