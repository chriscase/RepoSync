#!/usr/bin/env python3
"""Package compiled tests and explicit runtime prerequisites, never a checkout."""
import argparse
import hashlib
import json
import shutil
import sys
import tempfile
from pathlib import Path

APPLICATIONS = ("reposync", "reposync-personal")
FIXTURES = ("git_http_auth_server.py", "pair_refresh_http_server.py")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage_fixtures(context, source, receipt):
    target = context / "crates/core/tests/fixtures"
    target.mkdir(parents=True, exist_ok=True)
    if target.resolve() != context.resolve() / "crates/core/tests/fixtures":
        raise ValueError("fixture staging escaped source context")
    inputs = {}
    for name in FIXTURES:
        path = target / name
        if path.is_symlink():
            raise ValueError(f"symlink runtime fixture: {path}")
        origin = "source_archive"
        if not path.exists():
            fallback = source / "crates/core/tests/fixtures" / name
            if fallback.is_symlink() or not fallback.is_file():
                raise ValueError(f"invalid fallback runtime fixture: {fallback}")
            shutil.copy2(fallback, path)
            origin = "reviewed_harness_fallback"
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"invalid runtime fixture: {path}")
        inputs[name] = {"origin": origin, "sha256": sha(path)}
    receipt.write_text(json.dumps(inputs, indent=2, sort_keys=True) + "\n")


def artifacts(source):
    for line in source.read_text().splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if item.get("reason") == "compiler-artifact" and item.get("executable"):
            yield item


def package(source, destination, applications=None, fixtures=None):
    destination.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for item in artifacts(source):
        if not item.get("profile", {}).get("test"):
            continue
        name = item["target"]["name"]
        if name in manifest:
            raise ValueError(f"ambiguous test executable name: {name}")
        executable = Path(item["executable"])
        relative = Path("deps") / executable.name if applications else Path(executable.name)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(executable, target)
        manifest[name] = relative.as_posix()
        if name == "reposync_core":
            manifest["reposync-core"] = relative.as_posix()
    if not manifest:
        raise ValueError("no Rust test executables were produced")
    if applications:
        selected = {}
        # Cargo test can rebuild applications with feature unification. Its
        # final artifact wins only at the same canonical application path.
        for item in list(artifacts(applications)) + list(artifacts(source)):
            name = item["target"]["name"]
            if name not in APPLICATIONS or item.get("profile", {}).get("test"):
                continue
            if item["target"].get("kind") != ["bin"]:
                continue
            executable = Path(item["executable"])
            if executable.name != name or executable.parent.name != "debug" or executable.is_symlink():
                raise ValueError(f"unexpected application artifact: {executable}")
            if name in selected and selected[name]["source"] != str(executable):
                raise ValueError(f"ambiguous application artifact: {name}")
            selected[name] = {"source": str(executable), "features": item.get("features", [])}
        if set(selected) != set(APPLICATIONS):
            raise ValueError(f"missing required application: {sorted(set(APPLICATIONS) - set(selected))}")
        inputs = {"applications": {}, "fixtures": {}}
        for name, item in selected.items():
            shutil.copy2(item["source"], destination / name)
            inputs["applications"][name] = {"path": name, "sha256": sha(destination / name), **item}
        for name in FIXTURES:
            path = fixtures / name
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"missing or invalid runtime fixture: {name}")
            inputs["fixtures"][name] = {"sha256": sha(path)}
        (destination / "runtime-inputs.json").write_text(json.dumps(inputs, indent=2, sort_keys=True) + "\n")
    (destination / "binaries.json").write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n")
    print(f"Packaged {len(manifest)} Rust test identities")


def self_test():
    with tempfile.TemporaryDirectory(prefix="reposync-package-") as temporary:
        root = Path(temporary)
        debug = root / "target/debug"
        (debug / "deps").mkdir(parents=True)
        fixtures = root / "fixtures"
        fixtures.mkdir()
        for name in FIXTURES:
            (fixtures / name).write_text("# synthetic packaging control\n")
        test = debug / "deps/reposync_core-123"
        test.write_text("test binary control")
        def artifact(name, path, test=False):
            return {"reason": "compiler-artifact", "target": {"name": name, "kind": ["bin"]},
                    "profile": {"test": test}, "executable": str(path), "features": []}
        records = []
        for name in APPLICATIONS:
            (debug / name).write_text(name)
            records.append(artifact(name, debug / name))
        source, apps = root / "tests.jsonl", root / "apps.jsonl"
        source.write_text(json.dumps(artifact("reposync_core", test, True)) + "\n")
        def write_apps(items):
            apps.write_text("\n".join(json.dumps(i) for i in items) + "\n")
        write_apps(records)
        package(source, root / "valid", apps, fixtures)
        manifest = json.loads((root / "valid/binaries.json").read_text())
        assert manifest == {"reposync_core": "deps/reposync_core-123", "reposync-core": "deps/reposync_core-123"}
        package(source, root / "legacy")
        assert json.loads((root / "legacy/binaries.json").read_text())["reposync_core"] == test.name
        def rejects(items, fixture_root=fixtures):
            write_apps(items)
            try:
                package(source, root / "rejected", apps, fixture_root)
            except (ValueError, FileNotFoundError):
                return
            raise AssertionError("invalid prerequisite accepted")
        rejects(records[:1])
        other = root / "other/debug/reposync"
        other.parent.mkdir(parents=True)
        other.write_text("ambiguous")
        rejects(records + [artifact("reposync", other)])
        missing = fixtures / FIXTURES[0]
        missing.unlink()
        rejects(records)
        missing.symlink_to(fixtures / FIXTURES[1])
        rejects(records)
        harness = root / "harness"
        fixture_source = harness / "crates/core/tests/fixtures"
        fixture_source.mkdir(parents=True)
        for name in FIXTURES:
            (fixture_source / name).write_text("reviewed harness fixture")
        context = root / "archive"
        existing = context / "crates/core/tests/fixtures" / FIXTURES[0]
        existing.parent.mkdir(parents=True)
        existing.write_text("original historical fixture")
        receipt = root / "fixture-inputs.json"
        stage_fixtures(context, harness, receipt)
        staged = json.loads(receipt.read_text())
        assert existing.read_text() == "original historical fixture"
        assert staged[FIXTURES[0]]["origin"] == "source_archive"
        assert staged[FIXTURES[1]]["origin"] == "reviewed_harness_fallback"
    print("Runtime prerequisite packaging controls: PASS")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif sys.argv[1:2] == ["--stage-fixtures"]:
        stage_fixtures(*(Path(arg) for arg in sys.argv[2:]))
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("source", type=Path)
        parser.add_argument("destination", type=Path)
        parser.add_argument("--applications", type=Path)
        parser.add_argument("--fixtures", type=Path)
        args = parser.parse_args()
        if bool(args.applications) != bool(args.fixtures):
            parser.error("--applications and --fixtures must be supplied together")
        package(args.source, args.destination, args.applications, args.fixtures)
