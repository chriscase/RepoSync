#!/usr/bin/env python3
"""Stage reviewed locks without changing historical manifests or Rust source.

The filename is retained for existing callers; the old libc overlay is retired.
"""
import argparse
import copy
import hashlib
import json
import shutil
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
    regressions = sorted(name for name, status in old.items()
                         if status == "ok" and new.get(name) != "ok")
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
        "known_base_failures": {n: new.get(n, "MISSING") for n, s in old.items() if s == "FAILED"},
        "known_base_ignored": {n: new.get(n, "MISSING") for n, s in old.items() if s == "ignored"},
        "limitation": "Exact test identities only; no renamed-assertion equivalence or complete scenario qualification inferred.",
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
    print("Pinned-lock controls: PASS (no Cargo compilation)")


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
