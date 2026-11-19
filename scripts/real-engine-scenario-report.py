#!/usr/bin/env python3
"""Parse real-engine scenario logs and emit machine-readable results."""
from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CI_PARTIAL_ALLOWLIST = (
    ROOT / "docs/reliability/real-engine-ci-partial-allowlist.json"
)
REQUIRED_CASES_PATH = ROOT / "docs/reliability/required-cases.json"
REAL_ENGINE_BINARY = "real_engine_scenarios"
POST_WRITE_RECOVERY = "R01_SVNSERVE_POST_WRITE_RECOVERY"

PINNED_CI_PARTIAL_ALLOWLIST_IDS = frozenset(
    {
        POST_WRITE_RECOVERY,
        "R16_SVNSERVE_SVN_REMOTE_RECREATION",
        "R17_SVNSERVE_NOTIFICATION_ISOLATION",
    }
)


def load_expected_real_engine_tests() -> dict[str, str]:
    payload = json.loads(REQUIRED_CASES_PATH.read_text(encoding="utf-8"))
    tests = {}
    for tier in ("diagnostics", "candidate"):
        for row in payload.get(tier, []):
            if (
                isinstance(row, dict)
                and row.get("binary") == REAL_ENGINE_BINARY
                and row.get("id")
            ):
                tests[str(row["id"])] = row.get("test", "")
    if not tests:
        raise ValueError(f"{REQUIRED_CASES_PATH}: no {REAL_ENGINE_BINARY} case ids")
    return tests


def load_expected_real_engine_case_ids() -> list[str]:
    return sorted(load_expected_real_engine_tests())


def evidence_status(log_text: str, case_id: str) -> tuple[str | None, dict]:
    result = None
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
        if result is not None:
            return "FAIL", {"reason": "duplicate_evidence", "case": case_id}
        status = obj.get("status")
        if status not in ("PASS", "PARTIAL", "NOT RUN"):
            return "FAIL", {"reason": "invalid_evidence_status", "case": case_id}
        detail = obj.get("detail", {})
        result = status, detail if isinstance(detail, dict) else {}
    return result if result is not None else (None, {})


def classify(log_path: Path, case_id: str, test_exit: int) -> tuple[str, dict]:
    text = log_path.read_text() if log_path.is_file() else ""
    if test_exit == 0 and "test result: ok" in text:
        status, detail = evidence_status(text, case_id)
        if status is None:
            return "NOT RUN", {"reason": "missing_evidence", "case": case_id}
        if status == "PASS" and not detail:
            return "NOT RUN", {"reason": "missing_evidence_detail", "case": case_id}
        return status, detail
    tail = next((line for line in reversed(text.splitlines()) if line.strip()), "")
    return "FAIL", {"tail": tail[-500:]}


def append_result(results_path: Path, case_id: str, test_name: str, test_exit: int, log_path: Path) -> str:
    status, detail = classify(log_path, case_id, test_exit)
    row = {"id": case_id, "status": status, "test": test_name, "detail": detail}
    with results_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row) + "\n")
    return status


def write_summary(
    results_path: Path, summary_path: Path, toolchain_status: str, missing: str | None,
    timestamp: str | None = None,
) -> dict:
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
        "timestamp": timestamp if timestamp is not None else summary_path.parent.name,
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
    return evaluate_ci_summary(summary, allowlist_path)


def evaluate_ci_summary(summary: dict, allowlist_path: Path) -> tuple[int, list[str]]:
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
        if case_id == POST_WRITE_RECOVERY and status in ("PASS", "PARTIAL"):
            detail = row.get("detail")
            recovery = (
                detail.get("git_push_checkpoint_recovery") if isinstance(detail, dict) else None
            )
            if not isinstance(recovery, dict) or recovery.get("status") != "PASS":
                errors.append(f"{case_id}: detail.git_push_checkpoint_recovery.status must be PASS")
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


def controlled_status(summary_path: Path, suite_exit: str) -> tuple[str, list[str]]:
    """Match this invocation's native exit to its complete scenario report."""
    expected = {"0": "PASS", "2": "NOT RUN", "3": "PARTIAL"}.get(suite_exit)
    if expected is None:
        return "FAIL", [f"native suite exited with status {suite_exit}"]
    try:
        summary = json.loads(summary_path.read_text(encoding="utf-8"))
        if not isinstance(summary, dict) or summary.get("suite") != "real-engine-scenarios":
            return "FAIL", ["expected a real-engine-scenarios summary"]
        timestamp = summary.get("timestamp")
        utc_format = "%Y%m%dT%H%M%SZ"
        if (
            not isinstance(timestamp, str)
            or datetime.strptime(timestamp, utc_format).strftime(utc_format) != timestamp
        ):
            return "FAIL", ["expected a UTC suite timestamp in YYYYMMDDTHHMMSSZ format"]
        if summary.get("overall") != expected:
            return "FAIL", [f"suite exit {suite_exit} requires overall {expected}"]
        toolchain = summary.get("toolchain")
        if not isinstance(toolchain, dict) or "missing" not in toolchain:
            return "FAIL", ["missing native toolchain metadata"]
        missing = toolchain["missing"]
        if toolchain.get("status") == "NOT RUN":
            if expected != "NOT RUN" or not isinstance(missing, str) or not missing.strip():
                return "FAIL", ["unavailable toolchain requires a named missing tool and overall NOT RUN"]
        elif toolchain.get("status") != "PASS" or missing is not None:
            return "FAIL", ["invalid native toolchain status or missing tool"]
        rows = summary.get("scenarios")
        if not isinstance(rows, list) or not rows or not all(
            isinstance(row, dict) and isinstance(row.get("id"), str)
            and isinstance(row.get("status"), str) for row in rows
        ):
            return "FAIL", ["missing or malformed scenario rows"]
        counts = counts_from_scenarios(rows)
        reported = summary.get("counts")
        if not isinstance(reported, dict) or reported != counts or any(
            type(n) is not int for n in reported.values()
        ):
            return "FAIL", ["summary counts must be integers matching the scenario rows"]
        tests = load_expected_real_engine_tests()
        if sorted(row["id"] for row in rows) != sorted(tests):
            return "FAIL", ["expected each cataloged scenario ID exactly once"]
        for row in rows:
            case_id = row["id"]
            if not row.get("test") or row["test"] != tests[case_id]:
                return "FAIL", [f"{case_id}: expected catalog test {tests[case_id]}"]
            detail = row.get("detail")
            if not isinstance(detail, dict) or not detail:
                return "FAIL", [f"{case_id}: missing scenario evidence detail"]
            if row["status"] == "NOT RUN":
                tool = detail.get("tool")
                if detail.get("reason") != "missing_tool" or not isinstance(tool, str) or not tool.strip():
                    return "FAIL", [f"{case_id}: NOT RUN requires a named missing tool"]
                if missing and tool != missing:
                    return "FAIL", [f"{case_id}: scenario evidence disagrees with the missing tool"]
        if counts[expected] == 0:
            return "FAIL", [f"overall {expected} has no corresponding scenario rows"]

        if expected == "NOT RUN":
            # CI rejects all NOT RUN results. Controlled validation preserves
            # the distinction only for a complete catalog with no executed row.
            if counts[expected] != len(rows):
                return "FAIL", ["NOT RUN requires every scenario row to be NOT RUN"]
        else:
            code, errors = evaluate_ci_summary(summary, DEFAULT_CI_PARTIAL_ALLOWLIST)
            if code:
                return "FAIL", errors
        return expected, []
    except (OSError, ValueError) as exc:
        return "FAIL", [str(exc)]


def run_self_test() -> None:
    allowlist_path = DEFAULT_CI_PARTIAL_ALLOWLIST
    allowed = load_ci_partial_allowlist(allowlist_path)
    git_id = "R16_SVNSERVE_GIT_REMOTE_UNREACHABLE"
    svn_id = "R16_SVNSERVE_SVN_REMOTE_UNREACHABLE"
    recreation_id = "R16_SVNSERVE_SVN_REMOTE_RECREATION"
    assert allowed == PINNED_CI_PARTIAL_ALLOWLIST_IDS, (allowed, PINNED_CI_PARTIAL_ALLOWLIST_IDS)
    json_ids = {
        str(item)
        for item in json.loads(allowlist_path.read_text(encoding="utf-8")).get(
            "allowed_partial_scenario_ids", []
        )
    }
    assert json_ids == PINNED_CI_PARTIAL_ALLOWLIST_IDS, (json_ids, PINNED_CI_PARTIAL_ALLOWLIST_IDS)
    expected_tests = load_expected_real_engine_tests()
    expected_ids = sorted(expected_tests)
    assert len(expected_ids) >= 8

    def write_summary(tmp: Path, scenarios: list[dict], overall: str, **metadata) -> Path:
        summary_path = tmp / "summary.json"
        payload = {
            "suite": "real-engine-scenarios",
            "timestamp": "20261119T014057Z",
            "toolchain": {"status": "PASS", "missing": None},
            "scenarios": scenarios,
            "counts": counts_from_scenarios(scenarios),
            "overall": overall,
            **metadata,
        }
        summary_path.write_text(json.dumps(payload), encoding="utf-8")
        return summary_path

    import tempfile

    def full_pass_rows() -> list[dict]:
        rows = [
            {"id": case_id, "test": expected_tests[case_id], "status": "PASS", "detail": {"fixture": True}}
            for case_id in expected_ids
        ]
        post_write = next(row for row in rows if row["id"] == POST_WRITE_RECOVERY)
        post_write["detail"] = {"git_push_checkpoint_recovery": {"status": "PASS"}}
        return rows

    with tempfile.TemporaryDirectory() as raw:
        tmp = Path(raw)
        missing_summary = tmp / "no-such-summary.json"
        code, errs = ci_gate(missing_summary, allowlist_path)
        assert code == 1 and errs and "missing summary" in errs[0], (code, errs)
        assert controlled_status(missing_summary, "0")[0] == "FAIL"
        missing_summary.write_text('{"suite":', encoding="utf-8")
        assert controlled_status(missing_summary, "3")[0] == "FAIL"

        def check_controlled(rows: list[dict], overall: str, suite_exit: str, expected: str, **metadata) -> None:
            status, errors = controlled_status(write_summary(tmp, rows, overall, **metadata), suite_exit)
            assert status == expected and bool(errors) == (expected == "FAIL"), (status, errors)

        passing = full_pass_rows()
        check_controlled(passing, "PASS", "0", "PASS")
        for invalid_stamp in (None, "not-a-time", "20260231T140000Z"):
            check_controlled(passing, "PASS", "0", "FAIL", timestamp=invalid_stamp)
        unavailable = {"status": "NOT RUN", "missing": "svnserve"}
        for invalid_toolchain in (None, {"status": "PASS"}, {"status": "PASS", "missing": "git"}, unavailable):
            check_controlled(passing, "PASS", "0", "FAIL", toolchain=invalid_toolchain)
        for changes in ({"test": passing[1]["test"]}, {"test": None}, {"detail": None}, {"detail": {}}):
            check_controlled([dict(passing[0], **changes), *passing[1:]], "PASS", "0", "FAIL")
        for wrong_exit in ("1", "2", "3", "127"):
            check_controlled(passing, "PASS", wrong_exit, "FAIL")
        check_controlled([], "PASS", "0", "FAIL")
        not_run_rows = [
            dict(row, status="NOT RUN", detail={"reason": "missing_tool", "tool": "svnserve"})
            for row in passing
        ]
        check_controlled(not_run_rows, "NOT RUN", "2", "NOT RUN")
        check_controlled(not_run_rows, "NOT RUN", "2", "NOT RUN", toolchain=unavailable)
        # A completed Cargo invocation without its case receipt proves no result.
        log_path = tmp / "test.log"
        case_id = not_run_rows[0]["id"]
        receipt = {"case": case_id, "status": "NOT RUN", "detail": not_run_rows[0]["detail"]}
        cargo_ok = "test result: ok. 1 passed; 0 failed; 0 ignored;\n"
        for case_receipt in (None, dict(receipt, status="PASS", detail={}), dict(receipt, case="other")):
            marker = "RELIABILITY_EVIDENCE " + json.dumps(case_receipt) + "\n" if case_receipt else ""
            log_path.write_text(marker + cargo_ok)
            status, detail = classify(log_path, case_id, 0)
            assert detail["reason"] in {"missing_evidence", "missing_evidence_detail"}
            rows = [dict(row, status=status, detail=detail) for row in not_run_rows]
            check_controlled(rows, "NOT RUN", "2", "FAIL")
        log_path.write_text("RELIABILITY_EVIDENCE " + json.dumps(receipt) + "\n" + cargo_ok)
        assert classify(log_path, case_id, 0) == ("NOT RUN", receipt["detail"])
        assert classify(log_path, case_id, 1)[0] == "FAIL"
        passed = dict(receipt, status="PASS", detail={"verified": True})
        for earlier, expected_status in ((receipt, "FAIL"), (dict(receipt, case="other"), "PASS")):
            log_path.write_text("".join("RELIABILITY_EVIDENCE " + json.dumps(r) + "\n"
                                        for r in (earlier, passed)) + cargo_ok)
            assert classify(log_path, case_id, 0)[0] == expected_status
        check_controlled([dict(not_run_rows[0], detail={"reason": ""}), *not_run_rows[1:]], "NOT RUN", "2", "FAIL")
        for wrong_tool in ("", "git"):
            check_controlled(not_run_rows, "NOT RUN", "2", "FAIL", toolchain={"status": "NOT RUN", "missing": wrong_tool})
        check_controlled(not_run_rows[:-1], "NOT RUN", "2", "FAIL")
        check_controlled([not_run_rows[0], *passing[1:]], "PASS", "0", "FAIL")
        check_controlled([passing[0], *not_run_rows[1:]], "NOT RUN", "2", "FAIL")
        # The receiving suite reports the legacy R01 and R17 gaps as PARTIAL.
        receiving_rows = full_pass_rows()
        for row in receiving_rows:
            if row["id"] in (POST_WRITE_RECOVERY, "R17_SVNSERVE_NOTIFICATION_ISOLATION"):
                row["status"] = "PARTIAL"
        check_controlled(receiving_rows, "PARTIAL", "3", "PARTIAL")
        check_controlled(receiving_rows, "PARTIAL", "0", "FAIL")
        next(row for row in receiving_rows if row["id"] == POST_WRITE_RECOVERY).pop("detail")
        check_controlled(receiving_rows, "PARTIAL", "3", "FAIL")

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
            if row["id"] in allowed:
                row["status"] = "PARTIAL"
        code, errs = ci_gate(
            write_summary(tmp, allowlisted_partial, "PARTIAL"), allowlist_path
        )
        assert code == 0 and not errs, (code, errs)

        invalid_details = [
            None,
            {},
            {"git_push_checkpoint_recovery": "PASS"},
            {"git_push_checkpoint_recovery": {}},
        ]
        for status in ("FAIL", "PARTIAL", "NOT RUN", "SKIP"):
            invalid_details.append({"git_push_checkpoint_recovery": {"status": status}})
        for outer_status in ("PASS", "PARTIAL"):
            rows = full_pass_rows()
            post_write = next(row for row in rows if row["id"] == POST_WRITE_RECOVERY)
            post_write["status"] = outer_status
            code, errs = ci_gate(write_summary(tmp, rows, outer_status), allowlist_path)
            assert code == 0 and not errs, (outer_status, errs)

            for malformed_detail in invalid_details:
                post_write["detail"] = malformed_detail
                code, errs = ci_gate(write_summary(tmp, rows, outer_status), allowlist_path)
                assert code == 1 and any("git_push_checkpoint_recovery.status" in e for e in errs), (
                    outer_status, malformed_detail, errs
                )

        for outage_id in (git_id, svn_id):
            outage_partial = full_pass_rows()
            for row in outage_partial:
                if row["id"] == outage_id:
                    row["status"] = "PARTIAL"
            code, errs = ci_gate(
                write_summary(tmp, outage_partial, "PARTIAL"), allowlist_path
            )
            assert code == 1 and any(
                e.startswith(f"{outage_id}: PARTIAL not on CI allowlist") for e in errs
            ), (code, errs)

        surprise = full_pass_rows()
        for row in surprise:
            if row["id"] == "R17_SVNSERVE_JOB_ISOLATION":
                row["status"] = "PARTIAL"
        code, errs = ci_gate(write_summary(tmp, surprise, "PARTIAL"), allowlist_path)
        assert code != 0 and errs, (code, errs)
        check_controlled(surprise, "PARTIAL", "3", "FAIL")

        failed = full_pass_rows()
        for row in failed:
            if row["id"] == recreation_id:
                row["status"] = "FAIL"
        code, errs = ci_gate(write_summary(tmp, failed, "FAIL"), allowlist_path)
        assert code == 1 and f"{recreation_id}: FAIL" in errs, (code, errs)

        not_run = full_pass_rows()
        for row in not_run:
            if row["id"] == recreation_id:
                row["status"] = "NOT RUN"
        code, errs = ci_gate(write_summary(tmp, not_run, "NOT RUN"), allowlist_path)
        assert code == 1 and any(
            e.startswith(f"{recreation_id}: NOT RUN") for e in errs
        ), (code, errs)

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

        code, errs = ci_gate(
            write_summary(tmp, allowlisted_partial, "PASS"), allowlist_path
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
        assert controlled_status(summary_path, "0")[0] == "FAIL"
        payload["counts"] = counts_from_scenarios(mismatched_counts)
        payload["counts"]["FAIL"] = False  # compares equal to zero in Python
        summary_path.write_text(json.dumps(payload), encoding="utf-8")
        assert controlled_status(summary_path, "0")[0] == "FAIL"

    ci_yml = ROOT / ".github/workflows/ci.yml"
    ci_text = ci_yml.read_text(encoding="utf-8")
    missing_block = 'if [[ -z "$summary" ]]; then'
    assert missing_block in ci_text, "ci.yml must guard missing real-engine summary"
    start = ci_text.index(missing_block)
    end = ci_text.index("fi", start)
    missing_branch = ci_text[start : end + 2]
    assert "exit 1" in missing_branch, missing_branch
    assert 'exit "${suite_ec' not in missing_branch, missing_branch


def main(argv: list[str]) -> int:
    command = argv[1]
    if command == "self-test":
        run_self_test()
        subprocess.run([sys.executable, str(ROOT / "scripts/test_controlled_validation.py")], check=True)
        print("SELF-TEST: real-engine ci-gate PASS")
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
    if command == "controlled-status":
        if len(argv) == 4:
            status, errors = controlled_status(Path(argv[2]), argv[3])
        else:
            status, errors = "FAIL", ["usage: controlled-status SUMMARY_PATH SUITE_EXIT"]
        for error in errors:
            print(error, file=sys.stderr)
        print(status)
        return 0
    if command == "append":
        status = append_result(Path(argv[2]), argv[3], argv[4], int(argv[5]), Path(argv[6]))
        if status == "FAIL":
            return 1
        if status == "PARTIAL":
            return 3
        return 0
    if command == "summary":
        timestamp = None
        if len(argv) >= 7 and argv[-2] == "--timestamp":
            timestamp = argv[-1]
            argv = argv[:-2]
        summary = write_summary(
            Path(argv[2]), Path(argv[3]), argv[4], argv[5] if len(argv) > 5 else None, timestamp
        )
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
