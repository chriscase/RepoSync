#!/usr/bin/env python3
"""Match the candidate's direct libc edge where archived baselines lack it.

This changes dependency metadata only; no baseline Rust source or package version.
"""

import hashlib
import json
import sys
from pathlib import Path


manifest = Path(sys.argv[1]) / "crates/core/Cargo.toml"
before = manifest.read_bytes()
needle = b'native-tls = "0.2"\n'
assert before.count(needle) == 1, "archived core manifest shape changed"
edge = b'libc = "0.2"\n'
assert before.count(edge) <= 1, "archived core declares libc more than once"
if edge in before:
    after = before
    change = "direct libc dependency already present; no overlay applied"
else:
    after = before.replace(needle, edge + needle)
    manifest.write_bytes(after)
    change = 'one direct libc = "0.2" dependency edge; no Rust source changed'
print(json.dumps({
    "manifest": str(manifest),
    "original_sha256": hashlib.sha256(before).hexdigest(),
    "overlaid_sha256": hashlib.sha256(after).hexdigest(),
    "change": change,
}))
