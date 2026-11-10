#!/usr/bin/env python3
"""Fail if a product Dockerfile COPY input is excluded by .dockerignore.

Does not build or run an image. This host has no Docker daemon, and the
Dockerfile builder is pinned to rust:1.83, which cannot parse the locked
edition2024 crates. Those limits are recorded by the caller.
"""
import re
import sys
from pathlib import Path


def ignored(path, rules):
    matched = False
    for rule in rules:
        negate = rule.startswith("!")
        pattern = rule[1:] if negate else rule
        pattern = pattern.rstrip("/")
        if pattern == "*":
            hit = True
        elif pattern.endswith("/**"):
            prefix = pattern[:-3]
            hit = path == prefix or path.startswith(prefix + "/")
        elif pattern.endswith("/**") is False and pattern.endswith("*"):
            hit = path.startswith(pattern[:-1])
        else:
            hit = path == pattern or path.startswith(pattern + "/")
        if hit:
            matched = not negate
    return matched


def copy_inputs(dockerfile):
    inputs = []
    for line in dockerfile.splitlines():
        stripped = line.strip()
        if not stripped.startswith("COPY ") or "--from=" in stripped:
            continue
        parts = stripped.split()
        # COPY src... dest
        sources = parts[1:-1]
        inputs.extend(sources)
    return inputs


def main():
    root = Path(__file__).resolve().parents[1]
    rules = [
        line.strip()
        for line in (root / ".dockerignore").read_text().splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]
    sources = copy_inputs((root / "Dockerfile").read_text())
    missing = []
    for source in sources:
        # A trailing glob is optional when the repo does not track that file.
        if source.endswith("*"):
            literal = source[:-1]
            candidate = root / literal
            if candidate.exists() and ignored(literal, rules):
                missing.append(f"dockerignore excludes COPY input: {source}")
            continue
        if not (root / source).exists():
            missing.append(f"missing COPY input: {source}")
        elif ignored(source, rules):
            missing.append(f"dockerignore excludes COPY input: {source}")
    if missing:
        print("\n".join(missing), file=sys.stderr)
        return 1
    print("docker context includes every product COPY input")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
