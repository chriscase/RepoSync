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
# Reviewed main blobs; fallback scripts must match both Git blob and SHA-256.
FIXTURE_ANCHOR = {
    "source_head": "7d4c8f81a6faeb664956bbe4c56c4a0da2fb1c65",
    "source_tree": "0d9d37853e0df188bb5ec84752d0c78adda46caf",
    "fixtures": {
        "git_http_auth_server.py": {"git_blob_sha1": "3b856fdd546e005613eb73e7e60d2a9470ec224b", "sha256": "ed5eed6797a2ca2b35cac5645293a44e90d1f77e409b95353c1465ae15083f33"},
        "pair_refresh_http_server.py": {"git_blob_sha1": "e690a127cd40b5172ef9ea86d5e4379a6576fc6e", "sha256": "5417a82fa1e72a65b5e43d74aaca45cf61ba471a10583dfe17c15d5d94521bd9"},
    },
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def blob_identity(path):
    data = path.read_bytes()
    return {"sha256": hashlib.sha256(data).hexdigest(),
            "git_blob_sha1": hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()}


def stage_fixtures(context, source, receipt):
    target = context / "crates/core/tests/fixtures"
    if target.resolve() != context.resolve() / "crates/core/tests/fixtures":
        raise ValueError("fixture staging escaped source context")
    target.mkdir(parents=True, exist_ok=True)
    inputs = {"reviewed_anchor": FIXTURE_ANCHOR, "fixtures": {}}
    for name in FIXTURES:
        path = target / name
        if path.is_symlink():
            raise ValueError(f"symlink runtime fixture: {path}")
        origin = "source_archive"
        if not path.exists():
            fallback = source / "crates/core/tests/fixtures" / name
            if fallback.is_symlink() or not fallback.is_file():
                raise ValueError(f"invalid fallback runtime fixture: {fallback}")
            if blob_identity(fallback) != FIXTURE_ANCHOR["fixtures"][name]:
                raise ValueError(f"fallback fixture differs from reviewed head blob: {name}")
            shutil.copy2(fallback, path)
            origin = "reviewed_harness_fallback"
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"invalid runtime fixture: {path}")
        inputs["fixtures"][name] = {"origin": origin, **blob_identity(path)}
    serialized = json.dumps(inputs, indent=2, sort_keys=True) + "\n"
    receipt.write_text(serialized)
    (target / "reliability-inputs.json").write_text(serialized)


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
        provenance = json.loads((fixtures / "reliability-inputs.json").read_text())
        if provenance["reviewed_anchor"] != FIXTURE_ANCHOR or set(provenance["fixtures"]) != set(FIXTURES):
            raise ValueError("runtime fixture provenance does not match reviewed anchor")
        inputs = {"applications": {}, "fixtures": {}, "fixture_provenance": provenance}
        for name, item in selected.items():
            shutil.copy2(item["source"], destination / name)
            inputs["applications"][name] = {"path": name, "sha256": sha(destination / name), **item}
        for name in FIXTURES:
            path = fixtures / name
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"missing or invalid runtime fixture: {name}")
            identity = blob_identity(path)
            staged = provenance["fixtures"][name]
            if identity != {key: staged[key] for key in identity}:
                raise ValueError(f"runtime fixture differs from staged input: {name}")
            if staged["origin"] == "reviewed_harness_fallback" and identity != FIXTURE_ANCHOR["fixtures"][name]:
                raise ValueError(f"runtime fallback differs from reviewed blob: {name}")
            if staged["origin"] not in ("source_archive", "reviewed_harness_fallback"):
                raise ValueError(f"unknown runtime fixture origin: {name}")
            inputs["fixtures"][name] = identity
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
        fixture_source = Path(__file__).resolve().parents[1] / "crates/core/tests/fixtures"
        for name in FIXTURES:
            shutil.copy2(fixture_source / name, fixtures / name)
        # Staging records the exact archived bytes; this branch is synthetic
        # coverage. All four real historical sources instead need fallback.
        fixture_context = root / "fixture-context"
        staged_root = fixture_context / "crates/core/tests/fixtures"
        staged_root.mkdir(parents=True)
        for name in FIXTURES:
            shutil.copy2(fixtures / name, staged_root / name)
        stage_fixtures(fixture_context, root, root / "staged.json")
        fixtures = staged_root
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
        provenance_file = fixtures / "reliability-inputs.json"
        original_provenance = provenance_file.read_text()
        invalid_provenance = json.loads(original_provenance)
        invalid_provenance["reviewed_anchor"]["source_head"] = "unreviewed"
        provenance_file.write_text(json.dumps(invalid_provenance))
        rejects(records)
        provenance_file.write_text(original_provenance)
        fixture = fixtures / FIXTURES[0]
        original_fixture = fixture.read_bytes()
        fixture.write_text("changed since staging")
        rejects(records)
        fixture.write_bytes(original_fixture)
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
            shutil.copy2(Path(__file__).resolve().parents[1] / "crates/core/tests/fixtures" / name, fixture_source / name)
        context = root / "archive"
        existing = context / "crates/core/tests/fixtures" / FIXTURES[0]
        existing.parent.mkdir(parents=True)
        existing.write_text("original historical fixture")
        receipt = root / "fixture-inputs.json"
        stage_fixtures(context, harness, receipt)
        staged = json.loads(receipt.read_text())
        assert existing.read_text() == "original historical fixture"
        assert staged["fixtures"][FIXTURES[0]]["origin"] == "source_archive"
        assert staged["fixtures"][FIXTURES[1]]["origin"] == "reviewed_harness_fallback"
        (fixture_source / FIXTURES[1]).write_text("tampered fallback bytes")
        (context / "crates/core/tests/fixtures" / FIXTURES[1]).unlink()
        try:
            stage_fixtures(context, harness, receipt)
        except ValueError as error:
            assert "reviewed head blob" in str(error)
        else:
            raise AssertionError("changed fallback blob accepted")
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
