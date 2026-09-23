#!/usr/bin/env python3
"""Copy only compiled test executables into the runtime image."""
import json
import shutil
import sys
from pathlib import Path

source, destination = Path(sys.argv[1]), Path(sys.argv[2])
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
if not manifest:
    raise SystemExit("no Rust test executables were produced")
(destination / "binaries.json").write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n")
print(f"Packaged {len(manifest)} Rust test executables")
