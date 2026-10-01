#!/usr/bin/env python3
"""Host-side #62 acceptance-matrix checker.

Validates that every isolated required-case ID is classified against the
stable R01–R24 scenario list, that GOAL.md is unchanged, and that no
scenario is marked PASS while criteria remain open. This is a catalog and
honesty gate. Isolated real-engine proof remains
scripts/reliability-container.sh.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "docs/reliability/acceptance-matrix.json"
REQUIRED_PATH = ROOT / "docs/reliability/required-cases.json"
SCENARIOS_PATH = ROOT / "docs/reliability/scenarios.json"
GOAL_PATH = ROOT / "docs/reliability/GOAL.md"
GOAL_SHA256 = "16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8"
SCENARIO_IDS = [f"R{n:02d}" for n in range(1, 25)]
ALLOWED_STATUS = {"PASS", "FAIL", "PARTIAL", "NOT RUN"}
ALLOWED_TIERS = {
    "actual_engine",
    "local_api",
    "local_api_real_remotes",
    "copy_only",
    "browser",
    "harness",
    "not_run",
}


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_json(path: Path):
    return json.loads(path.read_text())


def required_ids(manifest: dict) -> set[str]:
    ids = set()
    for tier in ("diagnostics", "candidate"):
        for case in manifest[tier]:
            if case["id"] in ids:
                raise SystemExit(f"FAIL: duplicate required-case id {case['id']}")
            ids.add(case["id"])
    return ids


def classified_ids(matrix: dict) -> dict[str, list[str]]:
    owners: dict[str, list[str]] = {}
    for scenario in matrix["scenarios"]:
        for field in ("required_case_ids", "supporting_case_ids"):
            for case_id in scenario.get(field, []):
                owners.setdefault(case_id, []).append(f"{scenario['id']}:{field}")
    for case_id in matrix.get("harness_case_ids", []):
        owners.setdefault(case_id, []).append("harness")
    return owners


def check_goal() -> None:
    digest = sha256_file(GOAL_PATH)
    if digest != GOAL_SHA256:
        raise SystemExit(
            f"FAIL: docs/reliability/GOAL.md digest {digest} != pinned {GOAL_SHA256}"
        )


def validate(matrix: dict, manifest: dict) -> list[str]:
    errors: list[str] = []
    if matrix.get("schema") != "reposync.reliability.acceptance-matrix.v1":
        errors.append("matrix schema must be reposync.reliability.acceptance-matrix.v1")
    if matrix.get("goal_sha256") != GOAL_SHA256:
        errors.append("matrix goal_sha256 does not match pinned GOAL.md")
    if matrix.get("deployed_version") != "NOT ESTABLISHED":
        errors.append("deployed_version must remain NOT ESTABLISHED")
    scenarios = matrix.get("scenarios") or []
    seen = [row["id"] for row in scenarios]
    if seen != SCENARIO_IDS:
        errors.append(f"scenarios must be exactly {SCENARIO_IDS}, got {seen}")
    expected = required_ids(manifest)
    owners = classified_ids(matrix)
    missing = sorted(expected - set(owners))
    extra = sorted(set(owners) - expected)
    if missing:
        errors.append(f"required-case IDs missing from matrix: {missing}")
    if extra:
        errors.append(f"matrix IDs not present in required-cases.json: {extra}")
    for row in scenarios:
        sid = row["id"]
        for key in ("baseline", "candidate"):
            if row.get(key) not in ALLOWED_STATUS:
                errors.append(f"{sid}.{key}={row.get(key)!r} is not a legal status")
        if row.get("tier") not in ALLOWED_TIERS:
            errors.append(f"{sid}.tier={row.get('tier')!r} is not a legal tier")
        open_items = row.get("open") or []
        required = row.get("required_case_ids") or []
        if row.get("candidate") == "PASS":
            if open_items:
                errors.append(f"{sid} cannot be PASS while open criteria remain")
            if not required:
                errors.append(f"{sid} cannot be PASS with zero required_case_ids")
        if row.get("candidate") == "NOT RUN" and required:
            errors.append(
                f"{sid} is NOT RUN but lists required_case_ids {required}; "
                "use PARTIAL or move those IDs"
            )
        if row.get("candidate") == "FAIL" and not (
            row.get("diagnostic_expected_baseline_failure") or required
        ):
            errors.append(f"{sid} FAIL needs diagnostic evidence or required cases")
    return errors


def rollup_counts(matrix: dict) -> dict[str, int]:
    counts = {status: 0 for status in ALLOWED_STATUS}
    for row in matrix["scenarios"]:
        counts[row["candidate"]] += 1
    return counts


def render_report(matrix: dict) -> str:
    lines = [
        "# #62 acceptance matrix report",
        "",
        f"- schema: `{matrix['schema']}`",
        f"- original reviewed base: `{matrix['reviewed_original_base']}`",
        f"- authored against main: `{matrix['authored_against_main']}`",
        f"- deployed version: **{matrix['deployed_version']}**",
        f"- GOAL SHA-256: `{matrix['goal_sha256']}`",
        "",
        "| ID | Candidate | Baseline | Tier | Required cases | Open |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for row in matrix["scenarios"]:
        required = ", ".join(row.get("required_case_ids") or []) or "—"
        open_items = "; ".join(row.get("open") or []) or "—"
        lines.append(
            f"| {row['id']} | {row['candidate']} | {row['baseline']} | "
            f"{row['tier']} | {required} | {open_items} |"
        )
    counts = rollup_counts(matrix)
    lines.extend(
        [
            "",
            "## Candidate rollup",
            "",
            f"- PASS: {counts['PASS']}",
            f"- FAIL: {counts['FAIL']}",
            f"- PARTIAL: {counts['PARTIAL']}",
            f"- NOT RUN: {counts['NOT RUN']}",
            "",
            "PASS is issue-complete only. PARTIAL means named subcases exist and "
            "required criteria remain open. Isolated execution is still "
            "`scripts/reliability-container.sh --all`.",
            "",
        ]
    )
    return "\n".join(lines) + "\n"


def scenarios_view(matrix: dict) -> dict:
    return {
        "reviewed_base": matrix["reviewed_original_base"],
        "authored_against_main": matrix["authored_against_main"],
        "scope": matrix["scope"],
        "scenarios": [
            {
                "id": row["id"],
                "baseline": row["baseline"],
                "candidate": row["candidate"],
                "tier": row["tier"],
                "evidence": row["evidence"],
                "diagnostic_expected_baseline_failure": row.get(
                    "diagnostic_expected_baseline_failure", False
                ),
            }
            for row in matrix["scenarios"]
        ],
    }


def sync_scenarios(matrix: dict) -> None:
    payload = json.dumps(scenarios_view(matrix), indent=2) + "\n"
    SCENARIOS_PATH.write_text(payload)


def host_tools() -> dict[str, str]:
    tools = {}
    for name in ("python3", "git", "svn", "svnadmin", "cargo", "docker"):
        path = shutil.which(name)
        tools[name] = path or "NOT RUN"
    return tools


def maybe_run_host_check() -> dict:
    """Run only the catalog self-check. Do not pretend this is isolated proof."""
    proc = subprocess.run(
        [sys.executable, str(Path(__file__).resolve()), "--check"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return {
        "command": "python3 scripts/reliability-acceptance-matrix.py --check",
        "exit_code": proc.returncode,
        "outcome": "PASS" if proc.returncode == 0 else "FAIL",
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }


def self_test() -> None:
    matrix = load_json(MATRIX_PATH)
    manifest = load_json(REQUIRED_PATH)
    check_goal()
    errors = validate(matrix, manifest)
    if errors:
        raise SystemExit("SELF-TEST FAIL:\n- " + "\n- ".join(errors))

    broken = json.loads(json.dumps(matrix))
    r09 = next(row for row in broken["scenarios"] if row["id"] == "R09")
    r09["required_case_ids"] = [
        case_id for case_id in r09["required_case_ids"] if case_id != "R09_REPLACEMENT"
    ]
    omitted = validate(broken, manifest)
    if not any("R09_REPLACEMENT" in item for item in omitted):
        raise SystemExit("SELF-TEST FAIL: omitting R09_REPLACEMENT was not rejected")

    pass_cheat = json.loads(json.dumps(matrix))
    r11 = next(row for row in pass_cheat["scenarios"] if row["id"] == "R11")
    r11["candidate"] = "PASS"
    r11["open"] = []
    r11["required_case_ids"] = []
    cheated = validate(pass_cheat, manifest)
    if not any("R11 cannot be PASS with zero required_case_ids" in item for item in cheated):
        raise SystemExit("SELF-TEST FAIL: empty PASS was not rejected")

    if scenarios_view(matrix) != load_json(SCENARIOS_PATH):
        raise SystemExit(
            "SELF-TEST FAIL: docs/reliability/scenarios.json is stale; "
            "run with --sync-scenarios"
        )
    print("SELF-TEST: PASS")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="validate manifests")
    parser.add_argument("--report", action="store_true", help="print markdown report")
    parser.add_argument(
        "--json",
        action="store_true",
        help="print machine-readable rollup on stdout",
    )
    parser.add_argument(
        "--sync-scenarios",
        action="store_true",
        help="rewrite docs/reliability/scenarios.json from the matrix",
    )
    parser.add_argument("--self-test", action="store_true", help="run checker fixtures")
    parser.add_argument(
        "--host-tools",
        action="store_true",
        help="report local tool availability without claiming isolated proof",
    )
    args = parser.parse_args()
    if not any(
        [
            args.check,
            args.report,
            args.json,
            args.sync_scenarios,
            args.self_test,
            args.host_tools,
        ]
    ):
        args.check = True
        args.report = True

    if args.self_test:
        self_test()
        return 0

    matrix = load_json(MATRIX_PATH)
    manifest = load_json(REQUIRED_PATH)
    if args.check:
        check_goal()
        errors = validate(matrix, manifest)
        if errors:
            print("FAIL: acceptance matrix is inconsistent", file=sys.stderr)
            for error in errors:
                print(f"- {error}", file=sys.stderr)
            return 1
        print("CHECK: PASS")
    if args.sync_scenarios:
        sync_scenarios(matrix)
        print(f"wrote {SCENARIOS_PATH.relative_to(ROOT)}")
    if args.report:
        sys.stdout.write(render_report(matrix))
    if args.json:
        payload = {
            "matrix_sha256": sha256_file(MATRIX_PATH),
            "required_cases_sha256": sha256_file(REQUIRED_PATH),
            "goal_sha256": sha256_file(GOAL_PATH),
            "candidate_rollup": rollup_counts(matrix),
            "scenarios": {
                row["id"]: {
                    "candidate": row["candidate"],
                    "baseline": row["baseline"],
                    "tier": row["tier"],
                }
                for row in matrix["scenarios"]
            },
            "host_catalog_check": maybe_run_host_check() if args.host_tools else None,
            "tools": host_tools() if args.host_tools else None,
            "isolated_runner": "scripts/reliability-container.sh --all",
            "live_acceptance": "NOT AUTHORIZED",
        }
        print(json.dumps(payload, indent=2))
    if args.host_tools and not args.json:
        print(json.dumps({"tools": host_tools(), "catalog": maybe_run_host_check()}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
