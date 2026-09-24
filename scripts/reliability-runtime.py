#!/usr/bin/env python3
"""Fixture-only test runner, executed inside the disposable Docker namespace."""
import hashlib
import json
import os
import socket
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import unquote, urlsplit

FIXTURE = Path("/fixture")
OUTPUT = Path("/evidence")
TESTS = Path("/opt/reliability-tests")
SECRET = "REPOSYNC_SYNTHETIC_SECRET_CANARY_72"
MANDATORY = {
    "diagnostics": {"R02_R03_ROUTE", "R06_CHECKPOINT"},
    "candidate": {"R09_ORIGINAL", "R09_REPLACEMENT", "R09_AMEND", "R01_LINEAR",
                  "R10_L_EQUALS_R", "R01_SVN_PENDING", "R16_MISSING_BRANCH",
                  "R16_TRANSPORT", "R16_AUTH", "R16_FETCH", "R10_MISSING_OBJECT",
                  "R10_MISSING_CURSOR", "R10_AMBIGUOUS",
                  "R10_SHALLOW", "R10_ANCESTRY_ERROR", "R09_LOCAL", "R10_MERGE",
                  "R10_OVERFLOW", "R17_SCOPE"},
}
MANDATORY["candidate"].update({"EVIDENCE_SCAN_CLEAN", "EVIDENCE_SCAN_CANARY",
                               "EVIDENCE_SCAN_ERROR"})
MANDATORY["candidate"].update({
    "R01_ALTERNATING_DUAL_CURSOR", "R01_PENDING_BOTH_DIRECTIONS",
    "R10_LEGACY_STALE_COPY", "R09_IGNORED_FILE_COLLISION",
    "R09_IGNORED_DIRECTORY_COLLISION", "R09_READONLY_INDEX",
    "R01_IGNORED_NONCOLLISION",
})
MANDATORY["candidate"].update({
    "R01_FAILED_APPLY_BARRIER", "R01_FAILED_APPLY_RETRY",
    "R01_METADATA_ONLY", "R19_UNTOUCHED_TREE", "R19_EXPLICIT_DELETE",
})
SCANNER_CASES = {
    "EVIDENCE_SCAN_CLEAN": "evidence_scan_clean",
    "EVIDENCE_SCAN_CANARY": "evidence_scan_canary",
    "EVIDENCE_SCAN_ERROR": "evidence_scan_error",
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def probe_connection(address):
    try:
        with socket.create_connection(address, timeout=1):
            return True
    except OSError:
        return False


def enrolled_file_target(url):
    parsed = urlsplit(url)
    if parsed.scheme != "file" or parsed.netloc not in ("", "localhost"):
        raise ValueError("not an enrolled local file target")
    path = Path(unquote(parsed.path))
    if not path.is_absolute():
        raise ValueError("relative file target")
    root = (FIXTURE / "tmp").resolve(strict=True)
    resolved = path.resolve(strict=True)
    if not resolved.is_relative_to(root):
        raise ValueError("file target escapes fixture root")
    return resolved


def boundary_canaries():
    FIXTURE.joinpath("tmp").mkdir(parents=True, exist_ok=True)
    FIXTURE.joinpath("home").mkdir(exist_ok=True)
    FIXTURE.joinpath("config").mkdir(exist_ok=True)
    owned = FIXTURE / "canary.txt"
    owned.write_text("fixture-owned\n")
    assert owned.read_text() == "fixture-owned\n"
    assert enrolled_file_target((FIXTURE / "tmp").as_uri()) == (FIXTURE / "tmp")
    for bad in ("file:///etc/passwd", "file:///fixture/tmp/../../etc/passwd", "https://example.invalid/"):
        try:
            enrolled_file_target(bad)
        except (ValueError, FileNotFoundError):
            pass
        else:
            raise AssertionError(f"non-fixture target admitted: {bad}")
    escape = FIXTURE / "tmp" / "escape"
    escape.symlink_to("/etc")
    try:
        enrolled_file_target(escape.as_uri())
    except ValueError:
        pass
    else:
        raise AssertionError("symlink escape admitted")
    escape.unlink()

    host_path = Path(os.environ["REPOSYNC_HOST_CANARY_PATH"])
    assert not host_path.exists(), "host-private canary was mounted into runtime"
    try:
        host_path.read_bytes()
    except (FileNotFoundError, PermissionError):
        pass
    else:
        raise AssertionError("host-private read was allowed")
    try:
        host_path.write_text("unexpected write")
    except (FileNotFoundError, PermissionError, OSError):
        pass
    else:
        raise AssertionError("host-private write was allowed")

    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    assert probe_connection(listener.getsockname()), "fixture loopback failed"
    listener.close()
    host_port = int(os.environ["REPOSYNC_HOST_LISTENER_PORT"])
    assert not probe_connection(("127.0.0.1", host_port)), "unrelated host loopback reachable"
    assert not probe_connection(("192.0.2.1", 80)), "external egress reachable"
    return {
        "fixture_read_write": "PASS",
        "host_private_read_write_denied": "PASS",
        "fixture_loopback": "PASS",
        "host_loopback_denied": "PASS",
        "external_egress_denied": "PASS",
        "file_target_traversal_and_symlink_denied": "PASS",
    }


def run_case(case, binaries):
    if case["binary"] == "evidence_scan":
        return run_scan_case(case)
    binary = TESTS / binaries[case["binary"]]
    test_name = case["test"]
    listing = subprocess.run([str(binary), "--list"], capture_output=True, text=True, check=True)
    matches = [line for line in listing.stdout.splitlines() if line.startswith(test_name + ": test")]
    if len(matches) != 1:
        raise AssertionError(f"required test missing or ambiguous: {case['id']} {test_name}")
    cmd = [str(binary), test_name, "--exact", "--nocapture", "--test-threads=1"]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=240)
    output = result.stdout + result.stderr
    if SECRET in output:
        raise AssertionError(f"synthetic fixture secret leaked in {case['id']} output")
    (OUTPUT / f"{case['id']}.log").write_text(output)
    import re
    counts = re.findall(
        r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out",
        output,
    )
    if len(counts) != 1:
        raise AssertionError(f"missing result counts for {case['id']}")
    passed, failed, ignored, _, filtered = map(int, counts[0])
    succeeded = result.returncode == 0 and (passed, failed, ignored) == (1, 0, 0)
    proofs = []
    for line in output.splitlines():
        marker = "RELIABILITY_EVIDENCE "
        if marker in line:
            proofs.append(json.loads(line.split(marker, 1)[1]))
    evidence = {
        "id": case["id"], "tier": case["tier"], "test": test_name,
        "binary_sha256": digest(binary.read_bytes()),
        "exit_code": result.returncode, "passed": passed, "failed": failed,
        "ignored": ignored, "filtered_out": filtered, "output_sha256": digest(output.encode()),
        "outcome": ("FAIL" if not succeeded else
                    "BASELINE_DEFECT_OBSERVED" if case["tier"] == "baseline_observation"
                    else "GATE_ADMISSION_ONLY" if case["tier"] == "candidate_admission"
                    else "CANDIDATE_SUBCASE_PASS"),
        "proofs": proofs,
    }
    (OUTPUT / f"{case['id']}.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence), flush=True)
    return evidence


def run_scan_case(case):
    if SCANNER_CASES.get(case["id"]) != case["test"]:
        raise AssertionError(f"required scanner case renamed: {case['id']}")
    scanner = Path("/usr/local/bin/reliability_scan.py")
    with tempfile.TemporaryDirectory(prefix="evidence-scan-", dir=FIXTURE / "tmp") as temporary:
        root = Path(temporary)
        evidence_dir = root / "evidence"
        evidence_dir.mkdir()
        status = root / "status.json"

        def invoke():
            result = subprocess.run(
                [sys.executable, str(scanner), "--scan", str(evidence_dir),
                 "--status-file", str(status)], capture_output=True, text=True, timeout=15)
            output = result.stdout + result.stderr
            if SECRET in output:
                raise AssertionError("scanner output exposed synthetic canary")
            return result.returncode, json.loads(status.read_text()), output

        outcomes = []
        if case["id"] == "EVIDENCE_SCAN_CLEAN":
            (evidence_dir / "safe.txt").write_text("fixture evidence\n")
            outcomes.append(invoke())
            succeeded = outcomes[0][0] == 0 and outcomes[0][1]["result"] == "PASS"
        elif case["id"] == "EVIDENCE_SCAN_CANARY":
            (evidence_dir / "canary.txt").write_text(SECRET)
            outcomes.append(invoke())
            succeeded = (outcomes[0][0] != 0 and outcomes[0][1].get("reason") ==
                         "synthetic_canary_found")
        else:
            (evidence_dir / "escape").symlink_to("/etc")
            outcomes.append(invoke())
            (evidence_dir / "escape").unlink()
            status.unlink()
            unreadable = evidence_dir / "unreadable.txt"
            unreadable.write_text("fixture evidence\n")
            unreadable.chmod(0)
            outcomes.append(invoke())
            missing = subprocess.run(
                [sys.executable, str(root / "missing-scanner.py"), "--scan", str(evidence_dir)],
                capture_output=True, text=True, timeout=15)
            outcomes.append((missing.returncode,
                             {"result": "FAIL", "reason": "scanner_unavailable"},
                             "scanner unavailable\n"))
            succeeded = (all(code != 0 for code, _, _ in outcomes) and
                         outcomes[0][1].get("reason") == "symlink_in_evidence" and
                         outcomes[1][1].get("reason") == "file_read_error" and
                         outcomes[2][1].get("reason") == "scanner_unavailable")
        output = "".join(outcome[2] for outcome in outcomes)
        (OUTPUT / f"{case['id']}.log").write_text(output)
        proof = [{"exit_code": code, "scan_result": state["result"],
                  "reason": state.get("reason")} for code, state, _ in outcomes]
    result = {
        "id": case["id"], "tier": case["tier"], "test": case["test"],
        "binary_sha256": digest(scanner.read_bytes()),
        "exit_code": 0 if succeeded else 1, "passed": int(succeeded),
        "failed": int(not succeeded), "ignored": 0, "filtered_out": 0,
        "output_sha256": digest(output.encode()),
        "outcome": "CANDIDATE_SUBCASE_PASS" if succeeded else "FAIL", "proofs": proof,
    }
    (OUTPUT / f"{case['id']}.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)
    return result


def run_baseline(binaries):
    import re
    results = {}
    totals = {"passed": 0, "failed": 0, "ignored": 0, "filtered_out": 0}
    for name, filename in sorted(binaries.items()):
        binary = TESTS / filename
        result = subprocess.run([str(binary), "--nocapture", "--test-threads=1"],
                                capture_output=True, text=True, timeout=300)
        output = result.stdout + result.stderr
        if SECRET in output:
            raise AssertionError(f"synthetic fixture secret leaked in {name} output")
        (OUTPUT / f"baseline-{name}.log").write_text(output)
        count_rows = re.findall(
            r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out",
            output,
        )
        if len(count_rows) != 1:
            raise AssertionError(f"missing baseline counts for {name}")
        passed, failed, ignored, _, filtered = map(int, count_rows[0])
        for key, count in zip(("passed", "failed", "ignored", "filtered_out"),
                              (passed, failed, ignored, filtered)):
            totals[key] += count
        # With --nocapture, SVN/Git fixture output can appear between the
        # harness's `test name ... ` prefix and its result (sometimes on the
        # following line). The run is single-threaded, so delimit by the next
        # test prefix and require one standalone harness status in each chunk.
        starts = list(re.finditer(r"^test (\S+) \.\.\. ", output, re.M))
        matched = []
        for index, start in enumerate(starts):
            end = starts[index + 1].start() if index + 1 < len(starts) else output.find("test result:", start.end())
            chunk = output[start.end():end]
            statuses = re.findall(r"(?m)^(ok|FAILED|ignored)(?:, [^\n]*)?$", chunk)
            if len(statuses) != 1:
                raise AssertionError(f"ambiguous baseline result for {name}::{start.group(1)}")
            matched.append((start.group(1), statuses[0]))
        if len(matched) != passed + failed + ignored:
            raise AssertionError(f"baseline test catalog/count mismatch for {name}: {len(matched)} vs {passed + failed + ignored}")
        for test_name, status in matched:
            results[f"{name}::{test_name}"] = status
        print(json.dumps({"binary": name, "exit_code": result.returncode,
                          "passed": passed, "failed": failed, "ignored": ignored}), flush=True)
    if totals["passed"] + totals["failed"] == 0:
        raise AssertionError("baseline executed zero tests")
    report = {"mode": "baseline", "source_head": os.environ["REPOSYNC_SOURCE_HEAD"],
              "source_tree": os.environ["REPOSYNC_SOURCE_TREE"],
              "lock_sha256": os.environ["REPOSYNC_LOCK_SHA256"],
              "goal_sha256": os.environ["REPOSYNC_GOAL_SHA256"],
              "totals": totals, "tests": results}
    (OUTPUT / "baseline-results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"mode": "baseline", "source_head": report["source_head"],
                      "totals": totals, "catalog_count": len(results)}), flush=True)


def main():
    mode = sys.argv[1]
    assert mode in ("diagnostics", "candidate", "all", "baseline"), mode
    assert os.getcwd() == "/fixture", "runtime cwd must be fixture-owned"
    assert os.environ["TMPDIR"] == "/fixture/tmp", "temporary targets must be fixture-owned"
    assert os.environ["HOME"] == "/fixture/home", "home must be fixture-owned"
    assert not Path("/src").exists(), "source checkout was mounted into runtime"
    assert not Path("/var/run/docker.sock").exists(), "Docker socket was mounted into runtime"
    canaries = boundary_canaries()
    (OUTPUT / "canaries.json").write_text(json.dumps(canaries, indent=2) + "\n")
    manifest = json.loads(Path("/opt/reliability/required-cases.json").read_text())
    binaries = json.loads((TESTS / "binaries.json").read_text())
    if mode == "baseline":
        run_baseline(binaries)
        versions = subprocess.run(["git", "--version"], capture_output=True, text=True, check=True).stdout
        versions += subprocess.run(["svn", "--version", "--quiet"], capture_output=True, text=True, check=True).stdout
        versions += (TESTS / "build-toolchain.txt").read_text()
        (OUTPUT / "tool-versions.txt").write_text(versions)
        return
    for tier, mandatory in MANDATORY.items():
        actual = {c["id"] for c in manifest[tier]}
        assert mandatory <= actual, f"required {tier} case omitted: {sorted(mandatory - actual)}"
    omitted_r09 = {c["id"] for c in manifest["candidate"] if c["id"] != "R09_REPLACEMENT"}
    assert not MANDATORY["candidate"] <= omitted_r09, "R09 omission self-test failed"
    required = (manifest["diagnostics"] if mode in ("diagnostics", "all") else []) + \
               (manifest["candidate"] if mode in ("candidate", "all") else [])
    assert required, "zero required cases"
    assert len({c["id"] for c in required}) == len(required), "duplicate case IDs"
    cases = [run_case(case, binaries) for case in required]
    summary = {
        "mode": mode, "source_head": os.environ["REPOSYNC_SOURCE_HEAD"],
        "source_tree": os.environ["REPOSYNC_SOURCE_TREE"],
        "goal_sha256": os.environ["REPOSYNC_GOAL_SHA256"],
        "lock_sha256": os.environ["REPOSYNC_LOCK_SHA256"],
        "manifest_sha256": digest(Path("/opt/reliability/required-cases.json").read_bytes()),
        "required_ids": [c["id"] for c in required], "executed_ids": [c["id"] for c in cases],
        "baseline_observations": sum(c["tier"] == "baseline_observation" for c in cases),
        "candidate_regressions": sum(c["tier"] == "candidate_regression" for c in cases),
        "passed": sum(c["passed"] for c in cases), "failed": sum(c["failed"] for c in cases),
        "ignored": sum(c["ignored"] for c in cases),
        "filtered_out": sum(c["filtered_out"] for c in cases),
        "canaries": canaries,
        "required_r09_omission_rejected": True,
    }
    (OUTPUT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    versions = subprocess.run(["git", "--version"], capture_output=True, text=True, check=True).stdout
    versions += subprocess.run(["svn", "--version", "--quiet"], capture_output=True, text=True, check=True).stdout
    versions += (TESTS / "build-toolchain.txt").read_text()
    (OUTPUT / "tool-versions.txt").write_text(versions)
    # The container owns summary.json; the host runner may read it but cannot
    # rewrite it. Record the internal verified scan here, then require a
    # separate host scan of the complete mounted evidence after container exit.
    scanned = subprocess.run(
        [sys.executable, "/usr/local/bin/reliability_scan.py", "--scan", str(OUTPUT),
         "--status-file", str(OUTPUT / "internal-scan-status.json"),
         "--summary-file", str(OUTPUT / "summary.json")],
        capture_output=True, text=True, timeout=30)
    if scanned.returncode != 0:
        raise RuntimeError("internal evidence scan failed")
    summary = json.loads((OUTPUT / "summary.json").read_text())
    assert summary["evidence_scan"]["result"] == "PASS"
    print(json.dumps(summary), flush=True)
    if any(case["outcome"] == "FAIL" for case in cases):
        raise SystemExit("one or more exact required cases failed")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"FAIL: {error}", file=sys.stderr)
        raise
