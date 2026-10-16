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
                  "R10_OVERFLOW", "R17_SCOPE",
                  "R17_SVNSERVE_CONCURRENT_OVERLAP", "R17_SVNSERVE_CHECKPOINT_ISOLATION",
                  "R17_SVNSERVE_CREDENTIAL_ISOLATION", "R17_SVNSERVE_JOB_ISOLATION"},
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
    "R09_POLLING_ONLY", "R09_OBSERVED_REMOTE_REWRITE",
    "R09_OBSERVED_REMOTE_REWRITE_DURABLE", "R09_DURABLE_BLOCK", "R09_PERSONAL",
    "R09_WEBHOOK_FORCED_HINT",
})
MANDATORY["candidate"].update({
    "R10_DURABLE_UNSUPPORTED_BLOCK", "R10_DURABLE_BACKLOG_BLOCK",
    "R10_FETCH_HISTORY_BLOCK",
})
MANDATORY["candidate"].update({
    "R66_PERSONAL_LINEAR_ADMIT", "R66_PERSONAL_MERGE_DAG",
    "R66_TEAM_CONTINUATION_RESTART", "R66_CONTINUATION_MIXED_PENDING",
    "R66_PERSONAL_LINEAR_OVER_1000",
    "R66_PERSONAL_BACKLOG_RESTART",
})
MANDATORY["candidate"].update({
    "R01_OLD_INSTALL_UPGRADE", "R01_NO_TARGET_GIT_CURSOR",
    "R10_RETENTION_FRONTIER", "R10_RETENTION_PRESENT_KV",
    "R10_PRIOR_PRUNED_BASELINE",
})
MANDATORY["candidate"].update({
    "R10_POLICY_EQUAL_CURSOR", "R10_POLICY_SPLIT_CURSOR", "R10_POLICY_KV_ONLY",
    "R01_NO_TARGET_READ_FAILURE", "R01_NO_TARGET_STAGE_FAILURE",
    "R01_VERIFIED_NO_DELTA", "R10_PINNED_OLD_TOPOLOGY_INVENTORY",
    "R01_REPRESENTED_CONTENT",
    "R01_TARGET_MISMATCH",
    "R10_INVENTORY_AUTHORITY_CONFINEMENT",
})
MANDATORY["candidate"].update({
    "R01_FAILED_APPLY_BARRIER", "R01_FAILED_APPLY_RETRY",
    "R01_METADATA_ONLY", "R19_UNTOUCHED_TREE", "R19_EXPLICIT_DELETE",
})
MANDATORY["candidate"].update({'V13_EMPTY_USED', 'V14_UNQUALIFIED', 'V13_LARGE_SEQUENCE', 'V14_RESTART', 'K01_REJECTION', 'PINNED_UNQUALIFIED', 'V13_SPARSE_HIGH', 'MIGRATION_WRITE_FAILURE', 'MAPPING_NULL', 'K03_ORIGIN', 'MIGRATION_CRASH', 'K01_VALID', 'STARTUP_V12', 'MIGRATION_FORGED', 'V13_ROLLBACK', 'COPY_BOUNDARY', 'V13_ROWS', 'V14_OLD_TOPOLOGY', 'V13_NEVER_USED'})
MANDATORY["candidate"].update({'T54_LIST', 'T54_EMITTED', 'L03_HISTORY', 'T54_LOOKUP', 'T54_STATUS', 'T54_READONLY', 'L02_ADMISSION', 'L01_STORAGE'})
MANDATORY["candidate"].update({"M01_HISTORY", "S54_JSON_LOOKUP", "S54_JSON_HISTORY", "S54_JSON_PAGE", "S54_JSON_READONLY"})
MANDATORY["candidate"].update({"N01_FRONTIER", "S54_HTTP_COPY", "S54_SCOPED_HTTP"})
MANDATORY["candidate"].update({"64A_JOURNAL", "64A_PROCESS_TREE", "64A_ORDINARY", "64A_CONNECTING",
                               "64A_MID_IMPORT", "64A_LOST_PUSH", "64A_FINALIZER", "64A_CANCEL_WRITE", "64A_AUTH", "64A_SVN_CHILD", "64A_SCHEDULER", "64A_EXISTING_TARGET"})
MANDATORY["candidate"].update({"74_RESET_REFUSAL", "74_STDIN_FILE", "74_GIT_APPLY_STOP",
                               "74_APPLY_POSITIVE", "74_LFS_STOP", "74_LFS_POSITIVE"})
MANDATORY["candidate"].update({"74_CLEANUP_CLASSIFIER", "74_UNCERTAIN_APPLY_TIMEOUT",
                               "74_UNCERTAIN_SVN_CANCEL", "74_UNCERTAIN_SVN_EARLY",
                               "74_UNCERTAIN_PREPARATION", "74_FINISHED_PATCH_FALLBACK"})
MANDATORY["candidate"].update({"64B_LOST_REPLY_COMPLETE", "64B_FINALIZER_RECOVERY",
                               "64B_REMOTE_MISSING", "64B_REMOTE_MISMATCH", "64B_REMOTE_ADVANCED",
                               "64B_INSPECTION_FAILURE", "64B_CONFIG_CHANGED",
                               "64B_PARTIAL_CONFIRMED", "64B_STALE_ID", "64B_AUTH", "64B_RACE"})
MANDATORY["candidate"].update({"64FENC_REFUSE_SECOND_HOST", "64FENC_STALE_TAKEOVER",
                               "64FENC_SUPERSEDED_FINALIZE", "64FENC_IMPORT_FINALIZE",
                               "64FENC_SYNC_CONFIRM", "64FENC_NO_IDENTITY_REFUSE",
                               "64FENC_FOREIGN_TTL_INERT", "64FENC_BEGIN_REFUSE",
                               "64FENC_CLEAN_RELEASE"})
MANDATORY["candidate"].update({"64B_LOCAL_MISSING", "64B_MALFORMED", "64B_ATOMIC_FAILURE",
                               "64B_CURSOR_CONFLICT"})
MANDATORY["candidate"].update({
    "RS_C02_FILTERED_THEN_ALLOWED", "RS_C02_MIXED_AND_SIBLING",
    "RS_C02_OUT_OF_SCOPE_DELETE", "RS_C02_RENAME_ALLOWED_TO_BLOCKED",
    "RS_C02_RENAME_BLOCKED_TO_ALLOWED", "RS_C02_RENAME_BOTH_ALLOWED",
    "RS_C02_RENAME_BOTH_BLOCKED", "RS_C02_RENAME_NO_PATH_RULES",
    "RS_C02_RENAME_SOURCE_PATH_CONFLICT",
})
SCANNER_CASES = {
    "EVIDENCE_SCAN_CLEAN": "evidence_scan_clean",
    "EVIDENCE_SCAN_CANARY": "evidence_scan_canary",
    "EVIDENCE_SCAN_ERROR": "evidence_scan_error",
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def assert_runtime_source_isolation(src=Path("/src"), tests=TESTS):
    """Permit only UI assets, two fixture scripts and two executable aliases."""
    fixtures = ("git_http_auth_server.py", "pair_refresh_http_server.py")
    applications = ("reposync", "reposync-personal")
    def directory(path, names):
        assert not path.is_symlink() and path.is_dir(), f"invalid runtime directory: {path}"
        assert sorted(p.name for p in path.iterdir()) == sorted(names), f"unexpected runtime content: {path}"
    directory(src, ("crates", "target", "web-ui"))
    directory(src / "crates", ("cli", "core", "personal", "web"))
    for name in ("cli", "personal", "web"):
        directory(src / "crates" / name, ())
    directory(src / "crates/core", ("tests",))
    directory(src / "crates/core/tests", ("fixtures",))
    fixture_root = src / "crates/core/tests/fixtures"
    directory(fixture_root, fixtures)
    directory(src / "target", ("debug",))
    directory(src / "target/debug", applications)
    assert not tests.is_symlink() and tests.is_dir(), "invalid application package root"
    inputs = json.loads((tests / "runtime-inputs.json").read_text())
    assert set(inputs) == {"applications", "fixtures", "fixture_provenance"}, "unknown runtime input group"
    assert set(inputs["applications"]) == set(applications), "missing/extra runtime application"
    assert set(inputs["fixtures"]) == set(fixtures), "missing/extra runtime fixture"
    provenance = inputs["fixture_provenance"]
    anchor = provenance["reviewed_anchor"]
    assert anchor["source_head"] == "7d4c8f81a6faeb664956bbe4c56c4a0da2fb1c65", "unexpected fixture anchor"
    assert anchor["source_tree"] == "0d9d37853e0df188bb5ec84752d0c78adda46caf", "unexpected fixture anchor tree"
    assert set(anchor["fixtures"]) == set(provenance["fixtures"]) == set(fixtures), "invalid fixture provenance names"
    for name in fixtures:
        path = fixture_root / name
        assert not path.is_symlink() and path.is_file(), f"invalid fixture: {name}"
        data = path.read_bytes()
        identity = {"sha256": digest(data), "git_blob_sha1": hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()}
        assert identity == inputs["fixtures"][name], f"fixture digest mismatch: {name}"
        staged = provenance["fixtures"][name]
        assert identity == {key: staged[key] for key in identity}, "fixture differs from staged bytes"
        assert staged["origin"] in ("source_archive", "reviewed_harness_fallback"), "invalid fixture origin"
        if staged["origin"] == "reviewed_harness_fallback":
            assert identity == anchor["fixtures"][name], "fallback differs from reviewed blob"
    for name in applications:
        item = inputs["applications"][name]
        binary = tests / name
        assert item["path"] == name, "application path escaped package"
        assert not binary.is_symlink() and binary.is_file(), f"invalid packaged application: {name}"
        assert os.access(binary, os.X_OK), f"application is not executable: {name}"
        assert digest(binary.read_bytes()) == item["sha256"], f"application digest mismatch: {name}"
        alias = src / "target/debug" / name
        assert alias.is_symlink() and os.readlink(alias) == str(binary), f"application alias escaped package: {name}"
    return inputs


def persist_prerequisites(receipt):
    (OUTPUT / "runtime-prerequisites.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")


def require_runtime_integrity(check):
    """Persist an integrity refusal before stopping; never manufacture results."""
    receipt = {"integrity": "STARTED", "observations": {}}
    persist_prerequisites(receipt)
    try:
        result = check()
    except Exception as error:
        receipt.update(integrity="FAIL", error_type=type(error).__name__, error=str(error))
        persist_prerequisites(receipt)
        raise
    receipt["integrity"] = "PASS"
    persist_prerequisites(receipt)
    return result


def runtime_prerequisites(inputs, checks=None):
    """Observation failures are durable evidence and never replace test results."""
    import pwd
    import stat
    import uuid
    import shutil
    def native(data):
        identity = pwd.getpwuid(os.getuid())
        data.update(uid=os.getuid(), gid=os.getgid(), name=identity.pw_name)
        assert identity.pw_name == "fixture", "synthetic account name unavailable"
        machine = Path("/etc/machine-id")
        info = machine.lstat()
        value = machine.read_text().strip()
        assert stat.S_ISREG(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022
        assert value == "21300000000040008000000000000001", "synthetic native identity unavailable"
        data["machine_id_sha256"] = digest(value.encode())
        boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        assert uuid.UUID(boot).int != 0
        namespace = Path("/proc/self/ns/pid").stat()
        data.update(boot_id_sha256=digest(boot.encode()), pid_namespace_device=namespace.st_dev,
                    pid_namespace_inode=namespace.st_ino)
    def tools(data):
        data.update({name: shutil.which(name) for name in
                     ("git", "svn", "svnadmin", "svnserve", "openssl", "python3", "node")})
        assert all(data.values()), f"missing runtime tool: {data}"
    def mounts(data):
        data["fixture_mounts"] = [line for line in Path("/proc/mounts").read_text().splitlines()
                                  if line.split()[1] in ("/fixture", "/tmp")]
    def failure_mounts():
        try:
            raw = Path("/proc/mounts").read_bytes()
            return {"proc_mounts": raw.decode("utf-8"), "sha256": digest(raw)}
        except Exception as error:
            return {"error_type": type(error).__name__, "error": str(error)}
    def execution(data):
        with tempfile.TemporaryDirectory(prefix="prerequisite-probe-", dir=FIXTURE / "tmp") as temporary:
            root = Path(temporary)
            def run(args, **kwargs):
                try:
                    child = subprocess.run(args, capture_output=True, text=True, timeout=15, **kwargs)
                    return {"exit_code": child.returncode, "stdout": child.stdout, "stderr": child.stderr}
                except (OSError, subprocess.TimeoutExpired) as error:
                    details = {"error": str(error), "error_type": type(error).__name__,
                               "failure_mounts": failure_mounts()}
                    if isinstance(error, OSError):
                        details.update(errno=error.errno, strerror=error.strerror,
                                       filename=error.filename, filename2=error.filename2)
                    else:
                        details.update(command=error.cmd, timeout=error.timeout)
                    return details
            script = root / "fixture-exec-probe"
            script.write_text("#!/bin/sh\nprintf 'owned-execution-probe\n'\nexit 23\n")
            script.chmod(0o755)
            data["direct"] = run([str(script)])
            data["shell_control"] = run(["/bin/sh", str(script)])
            data["path"] = run([script.name], env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]})
            # Invoke the hook's Python shebang through the kernel at the same
            # owned path/mode; its result is diagnostic and never gates cases.
            hook_script = "#!/usr/bin/env python3\nraise SystemExit(29)\n"
            script.write_text(hook_script)
            data["hook_shebang"] = {"shebang": hook_script.splitlines()[0],
                                     "script_sha256": digest(script.read_bytes()),
                                     "mode": stat.S_IMODE(script.stat().st_mode),
                                     "expected_exit_code": 29, "result": run([str(script)])}
            data["git_setup"] = []
            bare, work = root / "bare.git", root / "work"
            for command in (["git", "init", "--bare", str(bare)], ["git", "init", str(work)],
                            ["git", "-C", str(work), "commit", "--allow-empty", "-m", "synthetic prerequisite control"]):
                step = run(command)
                data["git_setup"].append(step)
                persist_prerequisites(receipt)
                if step.get("exit_code") != 0:
                    raise RuntimeError("Git fixture setup failed; inspect retained step")
            data["default_identity"] = run(["git", "-C", str(work), "var", "GIT_AUTHOR_IDENT"])
            data["explicit_identity"] = run(["git", "-C", str(work), "-c", "user.name=Explicit Fixture", "-c",
                                             "user.email=explicit@example.invalid", "var", "GIT_AUTHOR_IDENT"])
            marker = root / "hook-ran"
            hook = bare / "hooks/pre-receive"
            hook.write_text(f"#!/bin/sh\nprintf 'rejected\n' > '{marker}'\nexit 1\n")
            hook.chmod(0o755)
            data["git_rejection"] = run(["git", "-C", str(work), "push", str(bare), "HEAD:refs/heads/main"])
            data["hook_ran"] = marker.exists()
            data["hook_rejection_effective"] = marker.exists() and data["git_rejection"].get("exit_code") not in (None, 0)
            if not data["hook_rejection_effective"]:
                data["ineffective_hook_mounts"] = failure_mounts()
            persist_prerequisites(receipt)
            assert data["shell_control"].get("exit_code") == 23, "shell execution control failed"
            assert data["default_identity"].get("stdout", "").startswith("Fixture <fixture@example.invalid>"), "default Git identity missing"
            assert data["explicit_identity"].get("stdout", "").startswith("Explicit Fixture <explicit@example.invalid>"), "fixture default overrode explicit test identity"
    receipt = {"integrity": "PASS", "inputs": inputs, "observations": {}}
    persist_prerequisites(receipt)
    for name, check in (checks if checks is not None else
                        (("native_identity", native), ("tools", tools), ("mounts", mounts), ("execution_probe", execution))):
        observation = {"status": "STARTED", "data": {}}
        receipt["observations"][name] = observation
        persist_prerequisites(receipt)
        try:
            check(observation["data"])
        except Exception as error:
            observation.update(status="FAIL", error_type=type(error).__name__, error=str(error),
                               failure_mounts=failure_mounts())
        else:
            observation["status"] = "PASS"
        persist_prerequisites(receipt)
    return receipt


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
    def capture_failure_mounts(label):
        diagnostic = {}
        try:
            raw = Path("/proc/mounts").read_bytes()
            filename = label + ".failure-mounts.log"
            (OUTPUT / filename).write_bytes(raw)
            diagnostic.update(proc_mounts_file=filename, sha256=digest(raw))
        except Exception as error:
            diagnostic.update(error_type=type(error).__name__, error=str(error))
        try:
            (OUTPUT / (label + ".failure-diagnostics.json")).write_text(json.dumps(diagnostic, indent=2) + "\n")
        except Exception as error:
            print(f"Diagnostic mount capture failed: {type(error).__name__}: {error}", file=sys.stderr)
    binary = TESTS / binaries[case["binary"]]
    test_name = case["test"]
    environment = {**os.environ, "RUST_BACKTRACE": "1"}
    try:
        listing = subprocess.run([str(binary), "--list"], capture_output=True, text=True, check=True, env=environment)
    except Exception:
        capture_failure_mounts(case["id"])
        raise
    matches = [line for line in listing.stdout.splitlines() if line.startswith(test_name + ": test")]
    if len(matches) != 1:
        raise AssertionError(f"required test missing or ambiguous: {case['id']} {test_name}")
    cmd = [str(binary), test_name, "--exact", "--nocapture", "--test-threads=1"]
    if case["id"] == "S54_HTTP_COPY":
        environment["REPOSYNC_COPY_WIRE_OUTPUT"] = str(OUTPUT / "S54_HTTP_COPY-wire.json")
    if case["id"] == "S54_SCOPED_HTTP":
        environment["REPOSYNC_SCOPED_WIRE_OUTPUT"] = str(OUTPUT / "S54_SCOPED_HTTP-wire.json")
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=240, env=environment)
    except Exception:
        capture_failure_mounts(case["id"])
        raise
    if result.returncode != 0:
        capture_failure_mounts(case["id"])
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
    if evidence["outcome"] == "FAIL" and result.returncode == 0:
        capture_failure_mounts(case["id"])
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


def parse_baseline_stdout(name, stdout, returncode):
    """Validate the authoritative parent harness channel, including failures."""
    import re
    from collections import Counter
    headers = list(re.finditer(r"(?m)^running (\d+) tests?\n", stdout))
    summaries = list(re.finditer(
        r"(?m)^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
        r"(\d+) measured; (\d+) filtered out; finished in [0-9]+(?:\.[0-9]+)?s\n", stdout))
    assert len(headers) == len(summaries) == 1, f"missing or ambiguous parent harness for {name}"
    header, summary = headers[0], summaries[0]
    assert header.end() <= summary.start() and not stdout[summary.end():].strip(), f"truncated or trailing parent output for {name}"
    outcome, *counts = summary.groups()
    passed, failed, ignored, measured, filtered = map(int, counts)
    assert measured == filtered == 0, f"incomplete baseline selection for {name}"
    assert int(header[1]) == passed + failed + ignored, f"parent announced/count mismatch for {name}"
    assert (outcome == "ok" and failed == 0 and returncode == 0) or (
        outcome == "FAILED" and failed > 0 and returncode == 101), f"parent exit/count mismatch for {name}"
    starts = list(re.finditer(r"(?m)^test (\S+) \.\.\. ", stdout))
    matched = {}
    for index, start in enumerate(starts):
        assert header.end() <= start.start() < summary.start(), f"test outside parent catalog for {name}"
        end = starts[index + 1].start() if index + 1 < len(starts) else summary.start()
        statuses = re.findall(r"(?m)^(ok|FAILED|ignored)(?:, [^\n]*)?$", stdout[start.end():end])
        assert len(statuses) == 1 and start[1] not in matched, f"ambiguous baseline result for {name}::{start[1]}"
        matched[start[1]] = statuses[0]
    actual = Counter(matched.values())
    assert len(matched) == passed + failed + ignored and (actual["ok"], actual["FAILED"], actual["ignored"]) == (
        passed, failed, ignored), f"baseline catalog/status/count mismatch for {name}"
    return dict(passed=passed, failed=failed, ignored=ignored, filtered_out=filtered), matched


def run_baseline(binaries):
    def capture_failure_mounts(label):
        diagnostic = {}
        try:
            raw = Path("/proc/mounts").read_bytes()
            filename = label + ".failure-mounts.log"
            (OUTPUT / filename).write_bytes(raw)
            diagnostic.update(proc_mounts_file=filename, sha256=digest(raw))
        except Exception as error:
            diagnostic.update(error_type=type(error).__name__, error=str(error))
        try:
            (OUTPUT / (label + ".failure-diagnostics.json")).write_text(json.dumps(diagnostic, indent=2) + "\n")
        except Exception as error:
            print(f"Diagnostic mount capture failed: {type(error).__name__}: {error}", file=sys.stderr)
    environment = {**os.environ, "RUST_BACKTRACE": "1"}
    results = {}
    totals = {"passed": 0, "failed": 0, "ignored": 0, "filtered_out": 0}
    processes = {}
    for name, filename in sorted(binaries.items()):
        binary = TESTS / filename
        try:
            result = subprocess.run([str(binary), "--nocapture", "--test-threads=1"],
                                    capture_output=True, text=True, timeout=300, env=environment)
        except Exception:
            capture_failure_mounts("baseline-" + name)
            raise
        if result.returncode != 0:
            capture_failure_mounts("baseline-" + name)
        output = result.stdout + result.stderr
        if SECRET in output:
            raise AssertionError(f"synthetic fixture secret leaked in {name} output")
        (OUTPUT / f"baseline-{name}.log").write_text(output)
        (OUTPUT / f"baseline-{name}.stdout.log").write_text(result.stdout)
        (OUTPUT / f"baseline-{name}.stderr.log").write_text(result.stderr)
        process = {"exit_code": result.returncode, "authoritative_channel": "stdout",
                   "stdout_sha256": digest(result.stdout.encode()),
                   "stderr_sha256": digest(result.stderr.encode())}
        (OUTPUT / f"baseline-{name}.process.json").write_text(json.dumps(process, indent=2) + "\n")
        counts, matched = parse_baseline_stdout(name, result.stdout, result.returncode)
        processes[name] = {**process, "counts": counts, "catalog_count": len(matched)}
        for key, count in counts.items():
            totals[key] += count
        for test_name, status in matched.items():
            results[f"{name}::{test_name}"] = status
        print(json.dumps({"binary": name, "exit_code": result.returncode,
                          **counts}), flush=True)
    if totals["passed"] + totals["failed"] == 0:
        raise AssertionError("baseline executed zero tests")
    report = {"mode": "baseline", "source_head": os.environ["REPOSYNC_SOURCE_HEAD"],
              "source_tree": os.environ["REPOSYNC_SOURCE_TREE"],
              "lock_sha256": os.environ["REPOSYNC_LOCK_SHA256"],
              "goal_sha256": os.environ["REPOSYNC_GOAL_SHA256"],
              "totals": totals, "tests": results, "processes": processes}
    (OUTPUT / "baseline-results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"mode": "baseline", "source_head": report["source_head"],
                      "totals": totals, "catalog_count": len(results)}), flush=True)


def baseline_self_test():
    """Parent-channel, writer-input and empty-UI-layout controls; no Rust build."""
    import contextlib
    import io
    from unittest.mock import patch
    stdout = ("\nrunning 3 tests\n"
              "test first ... ok\n"
              "test second ... fixture output\nFAILED\n"
              "test third ... ignored, fixture reason\n\n"
              "test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n")
    stderr = ("child stderr:\nrunning 1 test\ntest child ... FAILED\n"
              "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n")
    counts, tests = parse_baseline_stdout("fixture", stdout, 101)
    assert counts == dict(passed=1, failed=1, ignored=1, filtered_out=0)
    assert tests == {"first": "ok", "second": "FAILED", "third": "ignored"}
    healthy = stdout.replace("running 3 tests", "running 1 test").replace(
        "test second ... fixture output\nFAILED\n", "").replace("test third ... ignored, fixture reason\n", "").replace(
        "FAILED. 1 passed; 1 failed; 1 ignored", "ok. 1 passed; 0 failed; 0 ignored")
    assert parse_baseline_stdout("healthy", healthy, 0)[1] == {"first": "ok"}
    def reject(text, code=101):
        try:
            parse_baseline_stdout("invalid", text, code)
        except AssertionError:
            return
        raise AssertionError("malformed/inconsistent parent execution accepted")
    for text in (
        stdout.split("test result:")[0],
        stdout.rstrip("\n"),
        stdout + stderr,
        stdout + "trailing output\n",
        stdout.replace("running 3 tests", "running 4 tests"),
        stdout.replace("test first ... ok\n", ""),
        stdout.replace("test third ...", "test first ..."),
        stdout.replace("fixture output\nFAILED", "fixture output\nok"),
        stdout.replace("0 filtered out", "1 filtered out"),
        stdout.replace("0 measured", "1 measured"),
        stdout.replace("running 3 tests\n", ""),
        stdout.replace("1 failed", "malformed failed"),
        stdout.replace("; finished in 0.01s", ""),
        stdout.replace("0.01s", "...s"),
    ):
        reject(text)
    reject(stdout, 0)
    reject(stdout, -15)
    reject(healthy, 101)
    with tempfile.TemporaryDirectory(prefix="reposync-parent-catalog-") as temporary:
        root = Path(temporary)
        binary = root / "fixture"
        binary.write_text(f"#!{sys.executable}\nimport sys\nsys.stdout.write({stdout!r})\nsys.stderr.write({stderr!r})\nraise SystemExit(101)\n")
        binary.chmod(0o755)
        evidence = root / "evidence"
        evidence.mkdir()
        with patch.dict(globals(), TESTS=root, OUTPUT=evidence), patch.dict(os.environ, {
            "REPOSYNC_SOURCE_HEAD": "fixture", "REPOSYNC_SOURCE_TREE": "fixture",
            "REPOSYNC_LOCK_SHA256": "fixture", "REPOSYNC_GOAL_SHA256": "fixture",
        }), contextlib.redirect_stdout(io.StringIO()):
            run_baseline({"fixture": "fixture"})
        report = json.loads((evidence / "baseline-results.json").read_text())
        assert report["totals"] == counts and report["tests"] == {f"fixture::{n}": s for n, s in tests.items()}
        assert report["processes"]["fixture"]["exit_code"] == 101
        assert (evidence / "baseline-fixture.log").read_text() == stdout + stderr
        assert (evidence / "baseline-fixture.stdout.log").read_text() == stdout
        assert (evidence / "baseline-fixture.stderr.log").read_text() == stderr
        # Probe/setup failures stay durable and cannot suppress a real catalog.
        with patch.dict(globals(), TESTS=root, OUTPUT=evidence), patch.dict(os.environ, {
            "REPOSYNC_SOURCE_HEAD": "fixture", "REPOSYNC_SOURCE_TREE": "fixture",
            "REPOSYNC_LOCK_SHA256": "fixture", "REPOSYNC_GOAL_SHA256": "fixture",
        }), contextlib.redirect_stdout(io.StringIO()):
            def fail_probe(data):
                data["partial_step"] = "retained"
                raise OSError("injected prerequisite setup failure")
            def final_probe(data):
                data["later_probe_ran"] = True
            observed = runtime_prerequisites({}, checks=[(name, fail_probe) for name in
                ("native_identity", "tools", "shell", "git_setup")] + [("later", final_probe)])
            stored = json.loads((evidence / "runtime-prerequisites.json").read_text())
            assert stored == observed
            assert all(stored["observations"][name]["status"] == "FAIL" for name in
                       ("native_identity", "tools", "shell", "git_setup"))
            assert stored["observations"]["git_setup"]["data"]["partial_step"] == "retained"
            assert stored["observations"]["later"]["data"]["later_probe_ran"]
            run_baseline({"fixture": "fixture"})
            assert json.loads((evidence / "baseline-results.json").read_text())["tests"] == report["tests"]
            try:
                require_runtime_integrity(lambda: fail_probe({}))
            except OSError:
                pass
            else:
                raise AssertionError("integrity stop was downgraded to observation")
            refusal = json.loads((evidence / "runtime-prerequisites.json").read_text())
            assert refusal["integrity"] == "FAIL" and refusal["error_type"] == "OSError"
        # The allowlist admits exactly two fixture scripts and executable aliases.
        src = root / "src"
        (src / "web-ui").mkdir(parents=True)
        for directory in ("crates/web", "crates/cli", "crates/personal", "crates/core/tests/fixtures", "target/debug"):
            (src / directory).mkdir(parents=True, exist_ok=True)
        inputs = {"applications": {}, "fixtures": {}}
        for name in ("reposync", "reposync-personal"):
            (root / name).write_text("synthetic application control")
            (root / name).chmod(0o755)
            (src / "target/debug" / name).symlink_to(root / name)
            inputs["applications"][name] = {"path": name, "sha256": digest((root / name).read_bytes())}
        for name in ("git_http_auth_server.py", "pair_refresh_http_server.py"):
            path = src / "crates/core/tests/fixtures" / name
            path.write_text("# synthetic fixture control")
            inputs["fixtures"][name] = {"sha256": digest(path.read_bytes())}
        inputs["fixtures"] = {name: {"sha256": digest((src / "crates/core/tests/fixtures" / name).read_bytes()),
                              "git_blob_sha1": hashlib.sha1(b"blob " + str((src / "crates/core/tests/fixtures" / name).stat().st_size).encode() + b"\0" + (src / "crates/core/tests/fixtures" / name).read_bytes()).hexdigest()}
                              for name in inputs["fixtures"]}
        inputs["fixture_provenance"] = {"reviewed_anchor": {"source_head": "7d4c8f81a6faeb664956bbe4c56c4a0da2fb1c65",
            "source_tree": "0d9d37853e0df188bb5ec84752d0c78adda46caf", "fixtures": inputs["fixtures"]},
            "fixtures": {name: {**item, "origin": "source_archive"} for name, item in inputs["fixtures"].items()}}
        (root / "runtime-inputs.json").write_text(json.dumps(inputs))
        assert_runtime_source_isolation(src, root)
        def reject_layout():
            try:
                assert_runtime_source_isolation(src, root)
            except (AssertionError, FileNotFoundError):
                return
            raise AssertionError("invalid runtime source/package accepted")
        for forbidden in (src / "crates/web/lib.rs", src / "Cargo.toml", src / "crates/core/tests/fixtures/owned_process_group.rs"):
            forbidden.write_text("forbidden runtime content")
            reject_layout()
            forbidden.unlink()
        fixture = src / "crates/core/tests/fixtures/git_http_auth_server.py"
        original = fixture.read_bytes()
        fixture.write_text("wrong digest")
        reject_layout()
        fixture.unlink()
        reject_layout()
        fixture.symlink_to(root / "reposync")
        reject_layout()
        fixture.unlink()
        fixture.write_bytes(original)
        app = root / "reposync"
        original = app.read_bytes()
        app.write_text("wrong application digest")
        reject_layout()
        app.write_bytes(original)
        alias = src / "target/debug/reposync"
        alias.unlink()
        alias.symlink_to("/bin/sh")
        reject_layout()
        alias.unlink()
        alias.symlink_to(app)
        web = src / "crates/web"
        web.rmdir()
        web.symlink_to(src / "web-ui", target_is_directory=True)
        reject_layout()

    print("Parent parser/writer and runtime prerequisite boundary controls: PASS")


def main():
    os.environ.setdefault("REPOSYNC_NODE_BIN", "/usr/bin/nodejs")
    os.environ.setdefault("PATH", "/usr/local/bin:/usr/bin:/bin")
    mode = sys.argv[1]
    assert mode in ("diagnostics", "candidate", "all", "baseline"), mode
    def preflight():
        assert os.getcwd() == "/fixture", "runtime cwd must be fixture-owned"
        assert os.environ["TMPDIR"] == "/fixture/tmp", "temporary targets must be fixture-owned"
        assert os.environ["HOME"] == "/fixture/home", "home must be fixture-owned"
        assert (os.getuid(), os.getgid()) == (65532, 65532), "numeric runtime identity changed"
        inputs = assert_runtime_source_isolation()
        assert not Path("/var/run/docker.sock").exists(), "Docker socket was mounted into runtime"
        return inputs, boundary_canaries()
    inputs, canaries = require_runtime_integrity(preflight)
    (OUTPUT / "canaries.json").write_text(json.dumps(canaries, indent=2) + "\n")
    prerequisites = runtime_prerequisites(inputs)
    manifest = json.loads(Path("/opt/reliability/required-cases.json").read_text())
    binaries = json.loads((TESTS / "binaries.json").read_text())
    if mode == "baseline":
        run_baseline(binaries)
        versions = subprocess.run(["git", "--version"], capture_output=True, text=True, check=True).stdout
        versions += subprocess.run(["svn", "--version", "--quiet"], capture_output=True, text=True, check=True).stdout
        versions += (TESTS / "build-toolchain.txt").read_text()
        (OUTPUT / "tool-versions.txt").write_text(versions)
        return
    old_tests = Path("/opt/reliability-old-tests")
    old_binaries = json.loads((old_tests / "binaries.json").read_text())
    old_generator = old_tests / old_binaries["legacy_import_generator"]
    assert old_generator.is_file(), "pinned old import generator missing"
    os.environ["REPOSYNC_OLD_GENERATOR"] = str(old_generator)
    os.environ["REPOSYNC_INVENTORY_SCRIPT"] = "/usr/local/bin/reliability-inventory.py"
    os.environ["REPOSYNC_INVENTORY_PROBES_SCRIPT"] = "/usr/local/bin/reliability-inventory-probes.py"
    (OUTPUT / "old-generator-provenance.json").write_text(json.dumps({
        "old_code_sha": "87379741779a6259f7eeb52a68cc6f061174e5ef",
        "generator_sha256": digest(old_generator.read_bytes()),
        "adapter_sha256": (old_tests / "overlay.sha256").read_text().split()[0],
        "lock_sha256": os.environ["REPOSYNC_LOCK_SHA256"],
    }, indent=2) + "\n")
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
        "prerequisite_observations": {name: item["status"] for name, item in prerequisites["observations"].items()},
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
        if sys.argv[1:] == ["--self-test"]:
            baseline_self_test()
        else:
            main()
    except Exception as error:
        print(f"FAIL: {error}", file=sys.stderr)
        raise
