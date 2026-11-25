#!/usr/bin/env python3
"""Exercise phase 7 through the controlled wrapper with disposable, offline phase stubs."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
CATALOG = Path("docs/reliability/required-cases.json")
ALLOWLIST = Path("docs/reliability/real-engine-ci-partial-allowlist.json")
STAMP = "20300102T030405Z"
STATUSES = ("PASS", "FAIL", "PARTIAL", "NOT RUN", "SKIP")


def suite_summary(overall, timestamp):
    catalog = json.loads((ROOT / CATALOG).read_text())
    rows = []
    for entry in catalog["diagnostics"] + catalog["candidate"]:
        if entry["binary"] != "real_engine_scenarios":
            continue
        status = "NOT RUN" if overall == "NOT RUN" else "PASS"
        if entry["id"] == "R17_SVNSERVE_NOTIFICATION_ISOLATION":
            status = overall
        detail = {"fixture": True}
        if entry["id"] == "R01_SVNSERVE_POST_WRITE_RECOVERY" and status == "PASS":
            direct = {"status": "PASS", "registration_mode": "direct", "push_initiator": "team_worker"}
            detail = {
                "recovery_ok": True, "expected_watermark": 3, "final_watermark": 3,
                "mapping_count": 1, "mapped_git_sha": "abcd" * 10,
                "bare_head_stable": True, "duplicate_mapping": False,
                "git_push_checkpoint_recovery": {"status": "PASS"},
                "finalized_history": {
                    "historical_hold": {**direct, "published_git_sha": "abcd" * 10},
                    "mirror_recovery": dict(direct),
                    "contradictory_mirror_hold": {**direct, "pr_registration": {**direct, "registration_mode": "pr"}},
                },
            }
        if status == "NOT RUN":
            detail = {"reason": "missing_tool", "tool": "svnserve"}
        rows.append({"id": entry["id"], "test": entry["test"], "status": status, "detail": detail})
    return {
        "suite": "real-engine-scenarios", "timestamp": timestamp, "overall": overall,
        "toolchain": {"status": "NOT RUN" if overall == "NOT RUN" else "PASS",
                      "missing": "svnserve" if overall == "NOT RUN" else None},
        "scenarios": rows, "counts": {s: sum(r["status"] == s for r in rows) for s in STATUSES},
    }


def executable(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/bash\nset -eu\n" + body)
    path.chmod(0o700)


def setup_repo(repo, *, native=False):
    for path in [Path("scripts/controlled-validation.sh"), Path("scripts/real-engine-scenario-report.py"),
                 CATALOG, ALLOWLIST]:
        (repo / path.parent).mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / path, repo / path)
    tools = repo / "bin"
    tools.mkdir()
    for name in "bash cat chmod cp dirname env find grep mkdir mktemp rm sed seq sort tee tr uname wc".split():
        (tools / name).symlink_to(shutil.which(name, path=os.defpath))
    (tools / "python3").symlink_to(sys.executable)
    executable(tools / "date", f'if [[ "$2" == "+%Y%m%dT%H%M%SZ" ]]; then echo {STAMP}; else exec /bin/date "$@"; fi\n')
    if native:
        shutil.copy2(ROOT / "scripts/real-engine-scenario-suite.sh", repo / "scripts")
        return  # No VCS, Rust or Cargo on PATH.

    for name in ("git", "rustc"):
        executable(tools / name, '[[ "$1" == --version ]] && echo fixture\n')
    executable(tools / "cargo", 'if [[ "$1" == test ]]; then exit "$CARGO_TEST_EXIT"; fi\n')
    executable(tools / "svnadmin", 'if [[ "$1" == create ]]; then mkdir -p "$2/hooks"; fi\n')
    executable(tools / "svn", """
case "$1" in
  checkout) mkdir -p "$3" ;;
  --version|add|commit|rm) ;;
  info) echo 2 ;;
  log) echo '[reposync]' ;;
  cat) case "$2" in
    */conflict_file.txt) echo 'svn modified content' ;;
    */App.java) echo 'public class App {}' ;;
    */deleteme.txt) exit 1 ;;
    *) exit 90 ;;
  esac ;;
  *) exit 90 ;;
esac
""")
    executable(repo / "target/debug/reposync-personal",
               '[[ "$1" == --config && "$3" == log-probe ]]\necho LOG_PROBE > "${2%/*}/probe_data/personal.log"\n')
    executable(repo / "scripts/real-engine-scenario-suite.sh", """
[[ $# == 2 && "$1" == --output-dir ]]
printf '%s\n' "$2" > child-output-dir
mkdir -p "$2"
if [[ -f fixture-summary ]]; then sed "s/FIXTURE_TIMESTAMP/${2##*/}/" fixture-summary > "$2/summary.json"; fi
echo 'native suite fixture'
exit "$NATIVE_EXIT"
""")


class ControlledValidationTests(unittest.TestCase):
    def test_native_missing_tool_preserves_runs(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            setup_repo(repo, native=True)
            env = {"HOME": tmp, "TMPDIR": tmp, "PATH": str(repo / "bin"), "LC_ALL": "C"}

            def run(*args, expected=2):
                proc = subprocess.run(["bash", "scripts/real-engine-scenario-suite.sh", *args], cwd=repo,
                                      env=env, text=True, capture_output=True, timeout=10)
                self.assertEqual(proc.returncode, expected, proc.stdout + proc.stderr)

            def snapshot(path):
                return {f.name: f.read_bytes() for f in path.iterdir()}

            dest = repo / "chosen output"
            run("--output-dir", dest)
            original = snapshot(dest)
            summary = json.loads(original["summary.json"])
            self.assertEqual(summary["timestamp"], STAMP)
            self.assertEqual(summary["overall"], "NOT RUN")
            self.assertEqual(summary["toolchain"], {"status": "NOT RUN", "missing": "svn"})
            expected = suite_summary("NOT RUN", STAMP)
            self.assertEqual(summary["counts"], expected["counts"])
            self.assertCountEqual([(r["id"], r["test"]) for r in summary["scenarios"]],
                                  [(r["id"], r["test"]) for r in expected["scenarios"]])
            self.assertEqual(summary["scenarios"], [json.loads(row) for row in original["results.ndjson"].splitlines()])
            self.assertTrue(all(row["status"] == "NOT RUN" for row in summary["scenarios"]))
            run("--output-dir", dest, expected=1)
            self.assertEqual(snapshot(dest), original)

            run()
            parent = repo / "artifacts/real-engine-scenarios"
            first, = parent.iterdir()
            before = snapshot(first)
            run()
            second, = set(parent.iterdir()) - {first}
            self.assertEqual(snapshot(first), before)
            self.assertEqual(json.loads((second / "summary.json").read_bytes())["timestamp"], STAMP)

    def test_current_run_evidence(self):
        # Current summary, child exit, stale global summary, prior Cargo test exit, phase 7, overall, process exit.
        cases = [
            ("PASS", 0, "NOT RUN", 0, "PASS", "PASS", 0),
            ("PARTIAL", 3, "PASS", 0, "PARTIAL", "PARTIAL", 3),
            ("NOT RUN", 2, "PASS", 0, "NOT RUN", "NOT RUN", 2),
            ("FAIL", 1, "NOT RUN", 0, "FAIL", "FAIL", 1),
            (None, 0, "PASS", 0, "FAIL", "FAIL", 1),
            ("malformed", 0, "PASS", 0, "FAIL", "FAIL", 1),
            ("PARTIAL", 0, "PASS", 0, "FAIL", "FAIL", 1),
            ("PARTIAL", 3, "PASS", 1, "PARTIAL", "FAIL", 1),
        ]
        for current, child_exit, stale, earlier, phase, overall, code in cases:
            with self.subTest(current=current, child_exit=child_exit, earlier=earlier), tempfile.TemporaryDirectory() as tmp:
                repo = Path(tmp)
                setup_repo(repo)
                retained = repo / "artifacts/controlled-validation" / STAMP
                retained.mkdir(parents=True)
                (retained / "summary.md").write_text("retained")
                old = repo / "artifacts/real-engine-scenarios/20000101T000000Z/summary.json"
                old.parent.mkdir(parents=True)
                old.write_text(json.dumps(suite_summary(stale, old.parent.name)))
                if current:
                    payload = "{" if current == "malformed" else json.dumps(suite_summary(current, "FIXTURE_TIMESTAMP"))
                    (repo / "fixture-summary").write_text(payload)
                env = {"HOME": tmp, "TMPDIR": tmp, "PATH": str(repo / "bin"), "LC_ALL": "C",
                       "NATIVE_EXIT": str(child_exit), "CARGO_TEST_EXIT": str(earlier)}
                run = subprocess.run(["bash", "scripts/controlled-validation.sh", "--quick"], cwd=repo,
                                     env=env, capture_output=True, text=True, timeout=15)
                self.assertEqual(run.returncode, code, run.stdout + run.stderr)
                self.assertEqual((retained / "summary.md").read_text(), "retained")
                bundle, = set(retained.parent.iterdir()) - {retained}
                out = bundle / "real-engine-scenarios" / STAMP
                self.assertEqual((repo / "child-output-dir").read_text().strip(), str(out))
                self.assertIn("native suite fixture", (out.parent / "suite.log").read_text())
                events = [json.loads(line) for line in (bundle / "events.ndjson").read_text().splitlines()]
                self.assertEqual([e["status"] for e in events if e["phase"] == "real-engine-scenarios"
                                  and e["action"] == "complete"], [phase.lower().replace(" ", "_")])
                summary = (bundle / "summary.md").read_text()
                self.assertIn(f"| Real-engine scenarios | {phase} |", summary)
                self.assertIn(f"**Overall:** {overall}", summary)
                manifest = json.loads((bundle / "manifest.json").read_text())
                self.assertEqual(manifest["overall"], overall)
                paths = {entry["path"] for entry in manifest["artifacts"]}
                self.assertIn("real-engine-scenarios/suite.log", paths)
                self.assertEqual(str((out / "summary.json").relative_to(bundle)) in paths, current is not None)


if __name__ == "__main__":
    unittest.main()
