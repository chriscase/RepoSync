#!/usr/bin/env python3
"""Stage compiled tests and the exact application/fixture inputs they require."""
import argparse
import hashlib
import json
import shutil
import stat
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("source", type=Path)
parser.add_argument("destination", type=Path)
parser.add_argument("--runtime-inputs", nargs=2, metavar=("SOURCE_ROOT", "APPLICATION_MESSAGES"))
args = parser.parse_args()
source, destination = args.source, args.destination
destination.mkdir(parents=True, exist_ok=True)
manifest = {}
for line in source.read_text().splitlines():
    try:
        item = json.loads(line)
    except json.JSONDecodeError:
        continue
    if item.get("reason") != "compiler-artifact" or not item.get("profile", {}).get("test"):
        continue
    executable = item.get("executable")
    if not executable:
        continue
    name = item["target"]["name"]
    if name in manifest:
        raise SystemExit(f"ambiguous test executable name: {name}")
    target = destination / Path(executable).name
    shutil.copy2(executable, target)
    manifest[name] = target.name
    # Cargo names lib-test binaries with underscores; manifest cases use crate names.
    if name == "reposync_core":
        manifest["reposync-core"] = target.name
if not manifest:
    raise SystemExit("no Rust test executables were produced")
(destination / "binaries.json").write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n")
print(f"Packaged {len(manifest)} Rust test executables")

if args.runtime_inputs:
    root, applications = map(Path, args.runtime_inputs)
    staging = destination.parent / "reliability-runtime-inputs"
    # ../core fixture paths still traverse their compile-time crate directory.
    for crate in ("core", "cli", "personal", "web"):
        (staging / "crates" / crate).mkdir(parents=True, exist_ok=True)
    files = {}
    required_applications = {"reposync", "reposync-personal"}
    fixture_paths = {
        "crates/core/tests/fixtures/pair_refresh_http_server.py",
        "crates/core/tests/fixtures/git_http_auth_server.py",
        "crates/cli/tests/fixtures/startup_provider.py",
    }

    def stage(path, relative, kind, **metadata):
        for ancestor in (path, *path.parents):
            if ancestor == root.parent:
                break
            if ancestor.is_symlink():
                raise SystemExit(f"runtime input is a symlink: {ancestor}")
        if not path.is_file():
            raise SystemExit(f"runtime input missing or not a regular file: {path}")
        mode = stat.S_IMODE(path.stat().st_mode)
        if kind == "application" and not mode & 0o111:
            raise SystemExit(f"application subject is not executable: {path}")
        target = staging / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
        data = target.read_bytes()
        files[relative] = {"kind": kind, "bytes": len(data),
                           "sha256": hashlib.sha256(data).hexdigest(),
                           "mode": mode, **metadata}

    found = set()
    for line in applications.read_text().splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (item.get("reason") != "compiler-artifact" or
                item.get("target", {}).get("kind") != ["bin"] or
                item.get("profile", {}).get("test") is not False):
            continue
        name = item["target"]["name"]
        if name not in required_applications:
            continue
        if name in found:
            raise SystemExit(f"ambiguous application subject: {name}")
        relative = f"target/debug/{name}"
        path = root / relative
        if item.get("executable") != str(path):
            raise SystemExit(f"unexpected application path for {name}: {item.get('executable')}")
        stage(path, relative, "application", cargo_target=name,
              cargo_package_id=item["package_id"], features=item["features"])
        found.add(name)
    if found != required_applications:
        raise SystemExit(f"missing application subjects: {sorted(required_applications - found)}")
    absent = []
    for relative in sorted(fixture_paths):
        path = root / relative
        # Historical contexts retain their own source bytes; do not overlay
        # candidate fixture implementations into sources where they never existed.
        if not path.exists() and not path.is_symlink():
            absent.append(relative)
            continue
        stage(path, relative, "fixture")
    inputs = {"schema": 1, "files": files, "absent_fixtures": absent}
    (destination / "runtime-inputs.json").write_text(json.dumps(inputs, sort_keys=True, indent=2) + "\n")
    print(f"Packaged {len(files)} runtime inputs; absent source fixtures: {absent}")
