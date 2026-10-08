#!/usr/bin/env python3
"""Parse real-engine scenario logs and emit machine-readable results."""
from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CI_PARTIAL_ALLOWLIST = (
    ROOT / "docs/reliability/real-engine-ci-partial-allowlist.json"
)
REQUIRED_CASES_PATH = ROOT / "docs/reliability/required-cases.json"
REAL_ENGINE_BINARY = "real_engine_scenarios"


def load_expected_real_engine_case_ids() -> list[str]:
    payload = json.loads(REQUIRED_CASES_PATH.read_text(encoding="utf-8"))
    ids: list[str] = []
    for tier in ("diagnostics", "candidate"):
        for row in payload.get(tier, []):
            if (
                isinstance(row, dict)
                and row.get("binary") == REAL_ENGINE_BINARY
                and row.get("id")
            ):
                ids.append(str(row["id"]))
    if not ids:
        raise ValueError(f"{REQUIRED_CASES_PATH}: no {REAL_ENGINE_BINARY} case ids")
    return sorted(set(ids))


def evidence_detail(log_text: str, case_id: str | None, want_status: str) -> dict:
    for line in log_text.splitlines():
        if "RELIABILITY_EVIDENCE" not in line:
            continue
        payload = line.split("RELIABILITY_EVIDENCE", 1)[1].strip()
        start = payload.find("{")
        if start < 0:
            continue
        try:
            obj, _end = json.JSONDecoder().raw_decode(payload[start:])
        except json.JSONDecodeError:
            continue
        if obj.get("status") != want_status:
            continue
        if case_id is not None and obj.get("case") != case_id:
            continue
        detail = obj.get("detail", {})
        return detail if isinstance(detail, dict) else {}
    return {}


def evidence_status(log_text: str, case_id: str) -> tuple[str | None, dict]:
    for line in log_text.splitlines():
        if "RELIABILITY_EVIDENCE" not in line:
            continue
        payload = line.split("RELIABILITY_EVIDENCE", 1)[1].strip()
        start = payload.find("{")
        if start < 0:
            continue
        try:
            obj, _end = json.JSONDecoder().raw_decode(payload[start:])
        except json.JSONDecodeError:
            continue
        if obj.get("case") != case_id:
            continue
        status = obj.get("status")
        if status in {"PASS", "PARTIAL", "NOT RUN"}:
            detail = obj.get("detail", {})
            return status, detail if isinstance(detail, dict) else {}
    return None, {}


def classify(log_path: Path, case_id: str, test_exit: int) -> tuple[str, dict]:
    text = log_path.read_text() if log_path.is_file() else ""
    if '"status":"NOT RUN"' in text or '"status": "NOT RUN"' in text:
        return "NOT RUN", evidence_detail(text, None, "NOT RUN")
    if test_exit == 0 and "test result: ok" in text:
        status, detail = evidence_status(text, case_id)
        if status is None:
            return "NOT RUN", {"reason": "missing_evidence", "case": case_id}
        if status == "PARTIAL":
            return "PARTIAL", detail
        if status == "NOT RUN":
            return "NOT RUN", detail
        if detail:
            return "PASS", detail
        return "NOT RUN", {"reason": "missing_evidence_detail", "case": case_id}
    tail = next((line for line in reversed(text.splitlines()) if line.strip()), "")
    return "FAIL", {"tail": tail[-500:]}


def append_result(results_path: Path, case_id: str, test_name: str, test_exit: int, log_path: Path) -> str:
    status, detail = classify(log_path, case_id, test_exit)
    row = {"id": case_id, "status": status, "test": test_name, "detail": detail}
    with results_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row) + "\n")
    return status


def write_summary(results_path: Path, summary_path: Path, toolchain_status: str, missing: str | None) -> dict:
    results = [
        json.loads(line)
        for line in results_path.read_text().splitlines()
        if line.strip()
    ]
    counts = {"PASS": 0, "FAIL": 0, "PARTIAL": 0, "NOT RUN": 0, "SKIP": 0}
    for row in results:
        counts[row["status"]] = counts.get(row["status"], 0) + 1
    if counts["FAIL"] > 0:
        overall = "FAIL"
    elif counts["PARTIAL"] > 0:
        overall = "PARTIAL"
    elif counts["NOT RUN"] == len(results) and results:
        overall = "NOT RUN"
    else:
        overall = "PASS"
    summary = {
        "suite": "real-engine-scenarios",
        "timestamp": summary_path.parent.name,
        "toolchain": {"status": toolchain_status, "missing": missing},
        "scenarios": results,
        "counts": counts,
        "overall": overall,
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    return summary


def counts_from_scenarios(scenarios: list) -> dict[str, int]:
    counts = {"PASS": 0, "FAIL": 0, "PARTIAL": 0, "NOT RUN": 0, "SKIP": 0}
    for row in scenarios:
        if not isinstance(row, dict):
            continue
        status = row.get("status")
        if status in counts:
            counts[status] += 1
    return counts


def load_ci_partial_allowlist(path: Path) -> set[str]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    ids = payload.get("allowed_partial_scenario_ids")
    if not isinstance(ids, list) or not ids:
        raise ValueError(f"{path}: allowed_partial_scenario_ids must be a non-empty list")
    return {str(item) for item in ids}


def ci_gate(summary_path: Path, allowlist_path: Path) -> tuple[int, list[str]]:
    """Return (exit_code, errors). Exit 0 when CI may accept the suite summary."""
    if not summary_path.is_file():
        return 1, [f"missing summary: {summary_path}"]
    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    try:
        allowed = load_ci_partial_allowlist(allowlist_path)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        return 1, [f"allowlist {allowlist_path}: {exc}"]

    errors: list[str] = []
    scenarios = summary.get("scenarios")
    if not isinstance(scenarios, list):
        return 1, ["summary.scenarios must be a list"]

    try:
        expected_ids = load_expected_real_engine_case_ids()
    except (OSError, ValueError, json.JSONDecodeError, KeyError) as exc:
        return 1, [f"expected case catalog: {exc}"]

    seen: dict[str, int] = {}
    for row in scenarios:
        if not isinstance(row, dict):
            continue
        case_id = str(row.get("id", "<unknown>"))
        if case_id != "<unknown>":
            seen[case_id] = seen.get(case_id, 0) + 1

    for case_id in expected_ids:
        if seen.get(case_id, 0) == 0:
            errors.append(f"missing expected scenario id: {case_id}")
    for case_id, count in sorted(seen.items()):
        if count > 1:
            errors.append(f"duplicate scenario id: {case_id} ({count} rows)")
    for case_id in sorted(seen):
        if case_id not in expected_ids:
            errors.append(f"unexpected scenario id: {case_id}")

    computed_counts = counts_from_scenarios(scenarios)
    reported_counts = summary.get("counts")
    if not isinstance(reported_counts, dict):
        errors.append("summary.counts must be an object")
    else:
        for key in ("PASS", "FAIL", "PARTIAL", "NOT RUN", "SKIP"):
            reported = reported_counts.get(key, 0)
            if reported != computed_counts[key]:
                errors.append(
                    f"counts.{key}={reported!r} disagrees with scenario rows ({computed_counts[key]})"
                )

    row_statuses: list[str] = []
    for row in scenarios:
        if not isinstance(row, dict):
            errors.append("invalid scenario row (not an object)")
            continue
        case_id = row.get("id", "<unknown>")
        status = row.get("status")
        if isinstance(status, str):
            row_statuses.append(status)
        if status == "FAIL":
            errors.append(f"{case_id}: FAIL")
        elif status == "SKIP":
            errors.append(f"{case_id}: SKIP is not allowed in CI summaries")
        elif status == "NOT RUN":
            errors.append(f"{case_id}: NOT RUN (missing evidence or tools in CI)")
        elif status == "PARTIAL" and case_id not in allowed:
            errors.append(f"{case_id}: PARTIAL not on CI allowlist ({allowlist_path.name})")
        elif status not in {"PASS", "PARTIAL", "SKIP"}:
            errors.append(f"{case_id}: unexpected status {status!r}")

    overall = summary.get("overall")
    if overall == "FAIL":
        errors.append("overall: FAIL")
    elif overall == "NOT RUN":
        errors.append("overall: NOT RUN")
    elif overall == "PARTIAL":
        if row_statuses and all(status == "PASS" for status in row_statuses):
            errors.append("overall PARTIAL but every scenario row is PASS")
        partial_ids = [r["id"] for r in scenarios if r.get("status") == "PARTIAL"]
        unexpected = [cid for cid in partial_ids if cid not in allowed]
        if unexpected:
            errors.append(f"overall PARTIAL with non-allowlisted ids: {', '.join(unexpected)}")
    elif overall == "PASS":
        if computed_counts["FAIL"] > 0 or computed_counts["PARTIAL"] > 0:
            errors.append(
                "overall PASS but scenario rows include FAIL or PARTIAL "
                f"(counts={computed_counts})"
            )
    elif overall != "PASS":
        errors.append(f"overall: unexpected {overall!r}")

    if errors:
        return 1, errors
    return 0, []


def run_self_test() -> None:
    allowlist_path = DEFAULT_CI_PARTIAL_ALLOWLIST
    allowed = load_ci_partial_allowlist(allowlist_path)
    git_id = "R16_SVNSERVE_GIT_REMOTE_UNREACHABLE"
    svn_id = "R16_SVNSERVE_SVN_REMOTE_UNREACHABLE"
    allowlist_payload = json.loads(allowlist_path.read_text(encoding="utf-8"))
    expected_allowed = {
        str(item) for item in allowlist_payload.get("allowed_partial_scenario_ids", [])
    }
    assert allowed == expected_allowed, (allowed, expected_allowed)
    expected_ids = load_expected_real_engine_case_ids()
    assert len(expected_ids) >= 8

    def write_summary(tmp: Path, scenarios: list[dict], overall: str) -> Path:
        summary_path = tmp / "summary.json"
        payload = {
            "suite": "real-engine-scenarios",
            "scenarios": scenarios,
            "counts": counts_from_scenarios(scenarios),
            "overall": overall,
        }
        summary_path.write_text(json.dumps(payload), encoding="utf-8")
        return summary_path

    import tempfile

    def full_pass_rows() -> list[dict]:
        return [{"id": case_id, "status": "PASS"} for case_id in expected_ids]

    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        missing_summary = tmp / "no-such-summary.json"
        code, errs = ci_gate(missing_summary, allowlist_path)
        assert code == 1 and errs and "missing summary" in errs[0], (code, errs)

        short = [{"id": expected_ids[0], "status": "PASS"}]
        code, errs = ci_gate(write_summary(tmp, short, "PASS"), allowlist_path)
        assert code != 0 and any("missing expected scenario" in e for e in errs), (
            code,
            errs,
        )

        skipped = full_pass_rows()
        skipped[0] = {**skipped[0], "status": "SKIP"}
        code, errs = ci_gate(write_summary(tmp, skipped, "PASS"), allowlist_path)
        assert code != 0 and any("SKIP" in e for e in errs), (code, errs)

        allowlisted_partial = full_pass_rows()
        for row in allowlisted_partial:
            if row["id"] in {git_id, svn_id}:
                row["status"] = "PARTIAL"
        code, errs = ci_gate(
            write_summary(tmp, allowlisted_partial, "PARTIAL"), allowlist_path
        )
        assert code == 0 and not errs, (code, errs)

        surprise = full_pass_rows()
        for row in surprise:
            if row["id"] == "R17_SVNSERVE_JOB_ISOLATION":
                row["status"] = "PARTIAL"
        code, errs = ci_gate(write_summary(tmp, surprise, "PARTIAL"), allowlist_path)
        assert code != 0 and errs, (code, errs)

        failed = full_pass_rows()
        for row in failed:
            if row["id"] == git_id:
                row["status"] = "FAIL"
        code, errs = ci_gate(write_summary(tmp, failed, "FAIL"), allowlist_path)
        assert code != 0 and errs, (code, errs)

        not_run = full_pass_rows()
        for row in not_run:
            if row["id"] == git_id:
                row["status"] = "NOT RUN"
        code, errs = ci_gate(write_summary(tmp, not_run, "NOT RUN"), allowlist_path)
        assert code != 0 and errs, (code, errs)

        duplicate = full_pass_rows()
        duplicate.append({"id": expected_ids[0], "status": "PASS"})
        code, errs = ci_gate(write_summary(tmp, duplicate, "PASS"), allowlist_path)
        assert code != 0 and any("duplicate scenario id" in e for e in errs), (
            code,
            errs,
        )

        unexpected_id = full_pass_rows()
        unexpected_id.append({"id": "R99_UNEXPECTED_SCENARIO", "status": "PASS"})
        code, errs = ci_gate(
            write_summary(tmp, unexpected_id, "PASS"), allowlist_path
        )
        assert code != 0 and any("unexpected scenario id" in e for e in errs), (
            code,
            errs,
        )

        all_pass_partial_overall = full_pass_rows()
        code, errs = ci_gate(
            write_summary(tmp, all_pass_partial_overall, "PARTIAL"), allowlist_path
        )
        assert code != 0 and any(
            "overall PARTIAL but every scenario row is PASS" in e for e in errs
        ), (code, errs)

        stamped_pass_with_partial_rows = full_pass_rows()
        for row in stamped_pass_with_partial_rows:
            if row["id"] in {git_id, svn_id}:
                row["status"] = "PARTIAL"
        code, errs = ci_gate(
            write_summary(tmp, stamped_pass_with_partial_rows, "PASS"), allowlist_path
        )
        assert code != 0 and any(
            "overall PASS but scenario rows include FAIL or PARTIAL" in e for e in errs
        ), (code, errs)

        mismatched_counts = full_pass_rows()
        summary_path = write_summary(tmp, mismatched_counts, "PASS")
        payload = json.loads(summary_path.read_text(encoding="utf-8"))
        payload["counts"]["PASS"] = 0
        summary_path.write_text(json.dumps(payload), encoding="utf-8")
        code, errs = ci_gate(summary_path, allowlist_path)
        assert code != 0 and any("counts.PASS" in e and "disagrees" in e for e in errs), (
            code,
            errs,
        )

    ci_yml = ROOT / ".github/workflows/ci.yml"
    ci_text = ci_yml.read_text(encoding="utf-8")
    missing_block = 'if [[ -z "$summary" ]]; then'
    assert missing_block in ci_text, "ci.yml must guard missing real-engine summary"
    start = ci_text.index(missing_block)
    end = ci_text.index("fi", start)
    missing_branch = ci_text[start : end + 2]
    assert "exit 1" in missing_branch, missing_branch
    assert 'exit "${suite_ec' not in missing_branch, missing_branch

    print("SELF-TEST: real-engine ci-gate PASS")


def main(argv: list[str]) -> int:
    command = argv[1]
    if command == "self-test":
        run_self_test()
        return 0
    if command == "ci-gate":
        summary_path = Path(argv[2])
        allowlist_path = (
            Path(argv[3]) if len(argv) > 3 else DEFAULT_CI_PARTIAL_ALLOWLIST
        )
        code, errors = ci_gate(summary_path, allowlist_path)
        if errors:
            print("CI-GATE: FAIL", file=sys.stderr)
            for error in errors:
                print(f"- {error}", file=sys.stderr)
            return code
        print("CI-GATE: PASS")
        return 0
    if command == "append":
        status = append_result(Path(argv[2]), argv[3], argv[4], int(argv[5]), Path(argv[6]))
        if status == "FAIL":
            return 1
        if status == "PARTIAL":
            return 3
        return 0
    if command == "summary":
        summary = write_summary(Path(argv[2]), Path(argv[3]), argv[4], argv[5] if len(argv) > 5 else None)
        print(json.dumps(summary, indent=2))
        if summary["overall"] == "FAIL":
            return 1
        if summary["overall"] == "PARTIAL":
            return 3
        if summary["overall"] == "NOT RUN":
            return 2
        return 0
    raise SystemExit(f"unknown command: {command}")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
