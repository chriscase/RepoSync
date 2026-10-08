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

    for row in scenarios:
        if not isinstance(row, dict):
            errors.append("invalid scenario row (not an object)")
            continue
        case_id = row.get("id", "<unknown>")
        status = row.get("status")
        if status == "FAIL":
            errors.append(f"{case_id}: FAIL")
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
        partial_ids = [r["id"] for r in scenarios if r.get("status") == "PARTIAL"]
        unexpected = [cid for cid in partial_ids if cid not in allowed]
        if unexpected:
            errors.append(f"overall PARTIAL with non-allowlisted ids: {', '.join(unexpected)}")
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
    assert git_id in allowed and svn_id in allowed

    def write_summary(tmp: Path, scenarios: list[dict], overall: str) -> Path:
        summary_path = tmp / "summary.json"
        payload = {
            "suite": "real-engine-scenarios",
            "scenarios": scenarios,
            "counts": {},
            "overall": overall,
        }
        summary_path.write_text(json.dumps(payload), encoding="utf-8")
        return summary_path

    import tempfile

    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        pass_only = [
            {"id": "R17_SVNSERVE_JOB_ISOLATION", "status": "PASS"},
        ]
        code, errs = ci_gate(write_summary(tmp, pass_only, "PASS"), allowlist_path)
        assert code == 0 and not errs, (code, errs)

        allowlisted_partial = pass_only + [
            {"id": git_id, "status": "PARTIAL"},
            {"id": svn_id, "status": "PARTIAL"},
        ]
        code, errs = ci_gate(
            write_summary(tmp, allowlisted_partial, "PARTIAL"), allowlist_path
        )
        assert code == 0 and not errs, (code, errs)

        surprise = pass_only + [{"id": "R17_SVNSERVE_JOB_ISOLATION", "status": "PARTIAL"}]
        code, errs = ci_gate(write_summary(tmp, surprise, "PARTIAL"), allowlist_path)
        assert code != 0 and errs, (code, errs)

        failed = pass_only + [{"id": git_id, "status": "FAIL"}]
        code, errs = ci_gate(write_summary(tmp, failed, "FAIL"), allowlist_path)
        assert code != 0 and errs, (code, errs)

        not_run = [{"id": git_id, "status": "NOT RUN"}]
        code, errs = ci_gate(write_summary(tmp, not_run, "NOT RUN"), allowlist_path)
        assert code != 0 and errs, (code, errs)

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
