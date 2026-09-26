#!/usr/bin/env python3
"""Compare overlapping exact test identities, preserving known base failures."""
import json
import sys
from pathlib import Path

base = json.loads(Path(sys.argv[1]).read_text())
candidate = json.loads(Path(sys.argv[2]).read_text())
assert base["lock_sha256"] == candidate["lock_sha256"], "dependency graph differs"
assert base["goal_sha256"] == candidate["goal_sha256"], "goal contract differs"
base_tests, candidate_tests = base["tests"], candidate["tests"]
regressions = [name for name, status in base_tests.items()
               if status == "ok" and candidate_tests.get(name) != "ok"]
known_failures = {name: candidate_tests.get(name, "MISSING") for name, status in base_tests.items()
                  if status == "FAILED"}
known_ignored = {name: candidate_tests.get(name, "MISSING") for name, status in base_tests.items()
                 if status == "ignored"}
report = {
    "base_head": base["source_head"], "base_tree": base["source_tree"],
    "candidate_head": candidate["source_head"], "candidate_tree": candidate["source_tree"],
    "lock_sha256": base["lock_sha256"],
    "base_totals": base["totals"], "candidate_totals": candidate["totals"],
    "base_catalog_count": len(base_tests), "candidate_catalog_count": len(candidate_tests),
    "newly_added_test_count": len(set(candidate_tests) - set(base_tests)),
    "regressions": regressions, "known_base_failures": known_failures,
    "known_base_ignored": known_ignored,
}
Path(sys.argv[3]).write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
print(json.dumps(report, indent=2, sort_keys=True))
if regressions:
    raise SystemExit("matched-base comparison found newly failing old tests")
