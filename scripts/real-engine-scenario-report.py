#!/usr/bin/env python3
"""Parse real-engine scenario logs and emit machine-readable results."""
from __future__ import annotations

import json
import sys
from pathlib import Path


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


def classify(log_path: Path, case_id: str, test_exit: int) -> tuple[str, dict]:
    text = log_path.read_text() if log_path.is_file() else ""
    if '"status":"NOT RUN"' in text or '"status": "NOT RUN"' in text:
        return "NOT RUN", evidence_detail(text, None, "NOT RUN")
    if test_exit == 0 and "test result: ok" in text:
        return "PASS", evidence_detail(text, case_id, "PASS")
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


def main(argv: list[str]) -> int:
    command = argv[1]
    if command == "append":
        status = append_result(Path(argv[2]), argv[3], argv[4], int(argv[5]), Path(argv[6]))
        return 1 if status == "FAIL" else 0
    if command == "summary":
        summary = write_summary(Path(argv[2]), Path(argv[3]), argv[4], argv[5] if len(argv) > 5 else None)
        print(json.dumps(summary, indent=2))
        return 1 if summary["overall"] == "FAIL" else (2 if summary["overall"] == "NOT RUN" else 0)
    raise SystemExit(f"unknown command: {command}")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
