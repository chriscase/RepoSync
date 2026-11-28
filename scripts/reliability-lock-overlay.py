#!/usr/bin/env python3
"""Stage reviewed locks without changing historical manifests or Rust source.

The filename is retained for existing callers; the old libc overlay is retired.
"""
import argparse
import copy
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

FIXTURES = Path(__file__).resolve().parents[1] / "docs/reliability/fixtures"
BASELINES = {
    "87379741779a6259f7eeb52a68cc6f061174e5ef",
    "f74fce855a1f1d80dd631397f436ba33906272e6",
    "ab097580d9415ae2f8df44bed4041f740a1b38f2",
    "54e183484961bd9ad12bd03505b3b00610de7cd9",
}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_pins(fixtures=FIXTURES):
    pins = json.loads((fixtures / "baseline-locks.json").read_text())
    require(pins["schema"] == 1, "unknown baseline-lock schema")
    require(set(pins["sources"]) == BASELINES | {"candidate"},
            "missing or unexpected baseline-lock mapping")
    for source, pin in pins["sources"].items():
        require(Path(pin["lock"]).name == pin["lock"], "lock path escapes fixtures")
        require(source == "candidate" or source == pin["reviewed_source"],
                "baseline source/mapping mismatch")
        lock = fixtures / pin["lock"]
        require(sha256(lock.read_bytes()) == pin["lock_sha256"],
                f"pinned lock hash mismatch for {source}")
    return pins


def load_retired(fixtures=FIXTURES):
    """Reviewed old identities that are intentionally absent from the candidate."""
    data = json.loads((fixtures / "retired-test-identities.json").read_text())
    require(data["schema"] == 1, "unknown retired-identity schema")
    retired = data["retired"]
    for name, entry in retired.items():
        require(set(entry) == {"disposition", "successors", "reason"},
                f"malformed retired identity: {name}")
        require(entry["disposition"] in ("moved", "renamed", "superseded"),
                f"unknown retired disposition: {name}")
        successors = entry["successors"]
        require(isinstance(successors, list) and successors
                and all(isinstance(s, str) and s for s in successors)
                and len(set(successors)) == len(successors),
                f"retired identity needs distinct successors: {name}")
        require(name not in successors and not set(successors) & retired.keys(),
                f"retired identity names a retired successor: {name}")
        require(isinstance(entry["reason"], str) and entry["reason"].strip(),
                f"retired identity needs a reason: {name}")
    return retired


def stage(root, source, fixtures=FIXTURES):
    pins = load_pins(fixtures)
    require(source in pins["sources"], f"no pinned lock mapping for {source}")
    pin = pins["sources"][source]
    actual = {p.relative_to(root).as_posix() for p in root.glob("crates/*/Cargo.toml")}
    require(actual | {"Cargo.toml"} == set(pin["manifests"]),
            f"workspace manifest inventory mismatch for {source}")
    for path, expected in pin["manifests"].items():
        require(sha256((root / path).read_bytes()) == expected,
                f"manifest mismatch for {source}: {path}")
    shutil.copyfile(fixtures / pin["lock"], root / "Cargo.lock")
    return {"source_mapping": source, **pin, "manifest_overlay": "none"}


def registry_policy(fixtures, base_pin, candidate_pin):
    # Compare complete (name, version, source) identities: names may have
    # multiple versions. Shared packages must retain the exact checksum.
    def inventory(pin):
        data = tomllib.loads((fixtures / pin["lock"]).read_text())
        return {(p["name"], p["version"], p["source"]): p.get("checksum")
                for p in data["package"] if "source" in p}
    old, new = inventory(base_pin), inventory(candidate_pin)
    require(old.keys() <= new.keys(), "historical registry package/version missing in candidate lock")
    require(all(new[key] == value for key, value in old.items()),
            "shared registry checksum differs")
    return {"policy": "historical registry identities/checksums preserved; workspace edges differ",
            "shared_registry_packages": len(old),
            "candidate_only_registry_packages": [list(p) for p in sorted(new.keys() - old.keys())]}


def compare(base_path, candidate_path, source, candidate_sha, output, fixtures=FIXTURES):
    pins = load_pins(fixtures)
    require(source in BASELINES, f"no baseline comparison mapping for {source}")
    base_pin, candidate_pin = pins["sources"][source], pins["sources"]["candidate"]
    policy = registry_policy(fixtures, base_pin, candidate_pin)
    base = json.loads(base_path.read_text())
    candidate = json.loads(candidate_path.read_text())
    for report, expected_head, pin in ((base, source, base_pin),
                                       (candidate, candidate_sha, candidate_pin)):
        require(report["source_head"] == expected_head, "execution source does not match comparison source")
        require(report["lock_sha256"] == pin["lock_sha256"], "execution lock does not match reviewed pin")
        require(report["mode"] == "baseline" and report["tests"], "missing baseline execution catalog")
        if report is base:
            require(report["source_tree"] == pin["source_tree"], "baseline execution tree mismatch")
    require(base["goal_sha256"] == candidate["goal_sha256"], "goal contract differs")
    old, new = base["tests"], candidate["tests"]
    retired = load_retired(fixtures)
    # A retired identity must really be gone; one that still runs is compared as usual.
    stale = sorted(retired.keys() & new.keys())
    require(not stale, f"retired identity is still in the candidate catalog: {stale}")
    accounted, regressions = {}, []
    for name, status in sorted(old.items()):
        if status != "ok" or new.get(name) == "ok":
            continue
        entry = retired.get(name)
        # Only an absence can be accounted for, and only by successors that passed in this run.
        if entry and name not in new and all(new.get(s) == "ok" for s in entry["successors"]):
            accounted[name] = {"disposition": entry["disposition"], "successors": entry["successors"]}
        else:
            regressions.append(name)
    candidate_failures = sorted(name for name, status in new.items() if status == "FAILED")
    report = {
        "outcome": "FAIL" if regressions or candidate_failures else "PASS",
        "base_head": source, "base_tree": base["source_tree"],
        "candidate_head": candidate_sha, "candidate_tree": candidate["source_tree"],
        "base_lock_sha256": base_pin["lock_sha256"],
        "candidate_lock_sha256": candidate_pin["lock_sha256"],
        "dependency_policy": policy,
        "base_totals": base["totals"], "candidate_totals": candidate["totals"],
        "base_catalog_count": len(old), "candidate_catalog_count": len(new),
        "newly_added_test_count": len(new.keys() - old.keys()),
        "regressions": regressions, "candidate_failures": candidate_failures,
        "retired_identities_accounted": accounted,
        "known_base_failures": {n: new.get(n, "MISSING") for n, s in old.items() if s == "FAILED"},
        "known_base_ignored": {n: new.get(n, "MISSING") for n, s in old.items() if s == "ignored"},
        "limitation": "Exact test identities only. A reviewed retired identity is accepted when its listed successors passed; no other renamed-assertion equivalence or complete scenario qualification is inferred.",
    }
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({k: report[k] for k in ("outcome", "base_head", "candidate_head", "regressions", "candidate_failures")}))
    return report["outcome"] == "PASS"


def self_test():
    pins = load_pins()
    for source in BASELINES:
        registry_policy(FIXTURES, pins["sources"][source], pins["sources"]["candidate"])
    def rejected(action, description):
        try:
            action()
        except (ValueError, FileNotFoundError):
            print(f"PASS: rejected {description}")
        else:
            raise AssertionError(f"accepted {description}")
    with tempfile.TemporaryDirectory(prefix="reposync-lock-controls-") as temporary:
        root = Path(temporary)
        fixtures = root / "fixtures"
        shutil.copytree(FIXTURES, fixtures)
        changed = copy.deepcopy(pins)
        del changed["sources"][next(iter(BASELINES))]
        (fixtures / "baseline-locks.json").write_text(json.dumps(changed))
        rejected(lambda: load_pins(fixtures), "missing baseline-lock mapping")
        (fixtures / "baseline-locks.json").write_text(json.dumps(pins))
        pin = pins["sources"][next(iter(BASELINES))]
        (fixtures / pin["lock"]).write_bytes((FIXTURES / "Cargo.lock").read_bytes())
        rejected(lambda: load_pins(fixtures), "candidate lock substituted for historical lock")
        rejected(lambda: stage(root, "unmapped-source"), "unmapped source")
        rejected(lambda: stage(root, "candidate"), "missing source manifests")
        source = next(iter(BASELINES))
        base_pin = pins["sources"][source]
        base = {"mode": "baseline", "source_head": source, "source_tree": base_pin["source_tree"],
                "lock_sha256": base_pin["lock_sha256"], "goal_sha256": "fixture-goal",
                "tests": {"ordinary::positive": "ok"}, "totals": {"passed": 1, "failed": 0}}
        candidate = {**base, "source_head": "candidate-source", "source_tree": "candidate-tree",
                     "lock_sha256": pins["sources"]["candidate"]["lock_sha256"]}
        old, new, output = root / "base.json", root / "candidate.json", root / "comparison.json"
        old.write_text(json.dumps(base)); new.write_text(json.dumps(candidate))
        require(compare(old, new, source, "candidate-source", output), "positive comparison failed")
        candidate["lock_sha256"] = base["lock_sha256"]
        new.write_text(json.dumps(candidate))
        rejected(lambda: compare(old, new, source, "candidate-source", output), "mismatched execution lock")
        candidate["lock_sha256"] = pins["sources"]["candidate"]["lock_sha256"]
        candidate["tests"] = {"ordinary::positive": "ok", "new::failure": "FAILED"}
        new.write_text(json.dumps(candidate))
        require(not compare(old, new, source, "candidate-source", output), "candidate-only failure escaped")
        candidate["tests"] = {"different::test": "ok"}; new.write_text(json.dumps(candidate))
        require(not compare(old, new, source, "candidate-source", output), "missing old test escaped")

        # Retired identities: only a listed absence whose successors all passed is accepted.
        load_retired()
        fixtures = root / "retired-fixtures"
        shutil.copytree(FIXTURES, fixtures)
        registry = fixtures / "retired-test-identities.json"
        def retire(entries):
            registry.write_text(json.dumps({"schema": 1, "retired": entries}))
        def entry(*successors, disposition="renamed", reason="fixture"):
            return {"disposition": disposition, "successors": list(successors), "reason": reason}
        def run(tests):
            candidate["tests"] = tests; new.write_text(json.dumps(candidate))
            passed = compare(old, new, source, "candidate-source", output, fixtures)
            return passed, json.loads(output.read_text())
        retire({"ordinary::positive": entry("renamed::positive", "second::positive")})
        passed, report = run({"renamed::positive": "ok", "second::positive": "ok"})
        require(passed and report["regressions"] == [] and report["retired_identities_accounted"] == {
            "ordinary::positive": {"disposition": "renamed", "successors": ["renamed::positive", "second::positive"]}},
                "accounted retired identity was not accepted")
        for description, tests in (
                ("retired identity with a missing successor", {"renamed::positive": "ok"}),
                ("retired identity with a failed successor", {"renamed::positive": "ok", "second::positive": "FAILED"}),
                ("retired identity with an ignored successor", {"renamed::positive": "ok", "second::positive": "ignored"})):
            passed, report = run(tests)
            require(not passed and report["regressions"] == ["ordinary::positive"]
                    and report["retired_identities_accounted"] == {}, f"accepted {description}")
            print(f"PASS: rejected {description}")
        for status in ("ok", "FAILED", "ignored"):
            rejected(lambda: run({"ordinary::positive": status, "renamed::positive": "ok", "second::positive": "ok"}),
                     f"retired identity still present as {status}")
        retire({"unrelated::old": entry("renamed::positive")})
        passed, report = run({"renamed::positive": "ok"})
        require(not passed and report["regressions"] == ["ordinary::positive"], "unlisted missing old test escaped")
        print("PASS: rejected unlisted missing old test beside an unrelated retired entry")
        for description, entries in (
                ("retired identity without successors", {"ordinary::positive": entry()}),
                ("retired identity succeeding itself", {"ordinary::positive": entry("ordinary::positive")}),
                ("retired identity with a retired successor", {"ordinary::positive": entry("other::old"), "other::old": entry("renamed::positive")}),
                ("retired identity with duplicate successors", {"ordinary::positive": entry("renamed::positive", "renamed::positive")}),
                ("retired identity without a reason", {"ordinary::positive": entry("renamed::positive", reason=" ")}),
                ("retired identity with an unknown disposition", {"ordinary::positive": entry("renamed::positive", disposition="waived")}),
                ("retired identity with an extra field", {"ordinary::positive": {**entry("renamed::positive"), "waive": True}})):
            retire(entries)
            rejected(lambda: load_retired(fixtures), description)
        registry.write_text(json.dumps({"schema": 2, "retired": {}}))
        rejected(lambda: load_retired(fixtures), "unknown retired-identity schema")
        registry.unlink()
        rejected(lambda: load_retired(fixtures), "missing retired-identity registry")
    driver_self_test()
    print("Pinned-lock and driver controls: PASS (Docker stub only; no Cargo compilation)")


def driver_self_test():
    """Exercise shell failure retention and cache identities without Docker."""
    repo = FIXTURES.parents[2]
    previous = "f74fce855a1f1d80dd631397f436ba33906272e6"
    with tempfile.TemporaryDirectory(prefix="reposync-driver-controls-") as temporary:
        root = Path(temporary)
        binaries = root / "bin"
        binaries.mkdir()
        real_git = shutil.which("git")
        require(real_git is not None, "git unavailable for driver controls")
        stubs = {
            "docker": '''import json, os, sys
if sys.argv[1] == "info":
    raise SystemExit(0)
with open(os.environ["REPOSYNC_CONTROL_CALLS"], "a") as output:
    output.write(json.dumps(sys.argv[1:]) + "\\n")
raise SystemExit(41)
''',
            "git": f'''import os, sys
if len(sys.argv) > 2 and sys.argv[1] == "archive" and sys.argv[2] == os.environ.get("REPOSYNC_CONTROL_ARCHIVE"):
    print("CONTROL: archive failure", file=sys.stderr)
    raise SystemExit(42)
os.execv({real_git!r}, [{real_git!r}, *sys.argv[1:]])
''',
            "python3": f'''import os, sys
if len(sys.argv) > 5 and sys.argv[1].endswith("reliability-lock-overlay.py") and sys.argv[2] == "stage":
    if "--source" in sys.argv and sys.argv[sys.argv.index("--source") + 1] == os.environ.get("REPOSYNC_CONTROL_STAGE"):
        print("CONTROL: lock staging failure", file=sys.stderr)
        raise SystemExit(43)
os.execv({sys.executable!r}, [{sys.executable!r}, *sys.argv[1:]])
''',
        }
        for name, code in stubs.items():
            path = binaries / name
            path.write_text(f"#!{sys.executable}\n" + code)
            path.chmod(0o755)
        clean = {k: v for k, v in os.environ.items() if not k.startswith("REPOSYNC_")}
        env = {**clean, "PATH": str(binaries) + os.pathsep + os.environ["PATH"]}
        cases = [
            ("build", {}, "scripts/reliability-compare.sh", 1, 5),
            ("archive", {"REPOSYNC_CONTROL_ARCHIVE": previous}, "scripts/reliability-compare.sh", 1, 4),
            ("staging", {"REPOSYNC_CONTROL_STAGE": previous}, "scripts/reliability-compare.sh", 1, 4),
            ("wrapper-archive", {"REPOSYNC_CONTROL_ARCHIVE": "87379741779a6259f7eeb52a68cc6f061174e5ef"},
             "scripts/reliability-container.sh", 42, 0),
            ("wrapper-staging", {"REPOSYNC_CONTROL_STAGE": "candidate"}, "scripts/reliability-container.sh", 43, 4),
            ("wrapper-copy", {"REPOSYNC_BUILD_CONTEXT": str(root / "missing"), "REPOSYNC_LOCK_SOURCE": "candidate"},
             "scripts/reliability-container.sh", 1, 4),
        ]
        first_cache = None
        for name, extra, script, expected_status, expected_builds in cases:
            case = root / name
            case.mkdir()
            calls, evidence = case / "calls.jsonl", case / "comparison"
            case_env = {**env, "REPOSYNC_CONTROL_CALLS": str(calls),
                        "REPOSYNC_COMPARISON_DIR": str(evidence),
                        "REPOSYNC_ARTIFACT_DIR": str(case / "diagnostic"), **extra}
            args = ["bash", script] + (["--all"] if script.endswith("container.sh") else [])
            result = subprocess.run(args, cwd=repo, env=case_env, capture_output=True, text=True, timeout=45)
            require(result.returncode == expected_status,
                    f"{name}: unexpected driver status {result.returncode}: {result.stderr}")
            builds = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            require(len(builds) == expected_builds and all(b[0] == "build" for b in builds),
                    f"{name}: missing later build attempts")
            reports = list(evidence.glob("*-comparison.json"))
            require(len(reports) == 4 and all(json.loads(p.read_text())["outcome"] == "NOT_RUN" for p in reports),
                    f"{name}: missing NOT_RUN reports")
            require(all((evidence / f"{label}-exit-code.txt").exists()
                        for label in ("base", "previous", "immediate", "merged", "candidate")),
                    f"{name}: missing source exit receipts")
            if name in ("archive", "staging"):
                expected = 42 if name == "archive" else 43
                require((evidence / "previous-setup-exit-code.txt").read_text().strip() == str(expected),
                        f"{name}: setup failure hidden")
            if script.endswith("container.sh"):
                receipt = json.loads((case / "diagnostic/driver-exit.json").read_text())
                require(receipt["exit_code"] == expected_status, f"{name}: original error hidden")
            if name == "build":
                identities = [json.loads((evidence / label / "build-identity.json").read_text())
                              for label in ("base", "previous", "immediate", "merged", "candidate")]
                keys = {i["target_cache"] for i in identities}
                require(len(keys) == 5, "different source builds share a target cache")
                require(len({i["old_target_cache"] for i in identities}) == 1,
                        "pinned old generator identity unexpectedly changed")
                first_cache = identities[-1]["target_cache"]
            print(f"PASS: {name} failure retained; later sources attempted; four NOT_RUN reports")
        # Changing only the declared source must change the candidate cache ID.
        case = root / "different-source"
        case.mkdir()
        result = subprocess.run(["bash", "scripts/reliability-container.sh", "--baseline"], cwd=repo,
            env={**env, "REPOSYNC_CONTROL_CALLS": str(case / "calls.jsonl"),
                 "REPOSYNC_ARTIFACT_DIR": str(case), "REPOSYNC_SOURCE_HEAD_OVERRIDE": "0" * 40},
            capture_output=True, text=True, timeout=45)
        require(result.returncode == 41, "different-source control did not reach stub build")
        require(json.loads((case / "build-identity.json").read_text())["target_cache"] != first_cache,
                "a different candidate source reused the target cache")
        dockerfile = (repo / "Dockerfile.reliability").read_text()
        require("id=rust-1.98.1-bookworm-${RELIABILITY_TARGET_CACHE},target=/src/target" in dockerfile,
                "candidate target cache identity not used by Docker")
        require("id=rust-1.98.1-bookworm-old-${RELIABILITY_OLD_TARGET_CACHE},target=/src/target" in dockerfile,
                "old generator target cache not isolated by Docker")
        print("PASS: five source-specific target cache IDs; changed candidate source changes ID; generator cache is separate")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("stage")
    prepare.add_argument("root", type=Path)
    prepare.add_argument("--source", required=True)
    prepare.add_argument("--receipt", type=Path, required=True)
    comparison = commands.add_parser("compare")
    comparison.add_argument("base", type=Path)
    comparison.add_argument("candidate", type=Path)
    comparison.add_argument("output", type=Path)
    comparison.add_argument("--source", required=True)
    comparison.add_argument("--candidate-sha", required=True)
    commands.add_parser("self-test")
    args = parser.parse_args()
    if args.command == "stage":
        receipt = stage(args.root, args.source)
        args.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    elif args.command == "compare":
        if not compare(args.base, args.candidate, args.source, args.candidate_sha, args.output):
            raise SystemExit(1)
    else:
        self_test()


if __name__ == "__main__":
    main()
