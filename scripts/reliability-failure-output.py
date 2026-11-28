#!/usr/bin/env python3
"""Print the panic output of failed candidate catalog tests to the job log.

The evidence archive holds each test binary's captured stderr. This prints
only the panic blocks of candidate binaries that exited non-zero, so a failure
can be read from the job log without downloading the archive. The workflow
runs it only after the evidence scan has passed. Runner workflow commands are
stopped while test output is printed.
"""
import argparse
import json
import secrets
import sys
import tempfile
from pathlib import Path

MAX_BLOCK_LINES = 40
MAX_LINE_CHARS = 2000


def panic_blocks(text):
    """Return each `thread '...' panicked at` line with the lines that follow it."""
    blocks, current = [], None
    for line in text.splitlines():
        if line.startswith("thread '") and " panicked at " in line:
            current = [line]
            blocks.append(current)
        elif current is not None:
            if not line.strip() or line.startswith("stack backtrace:"):
                current = None
            elif len(current) < MAX_BLOCK_LINES:
                current.append(line)
    return [[line[:MAX_LINE_CHARS] for line in block] for block in blocks]


def report(root, out):
    logs = sorted(root.glob("*/candidate/baseline-*.stderr.log"))
    shown = failed = 0
    for log in logs:
        name = log.name[len("baseline-"):-len(".stderr.log")]
        try:
            process = json.loads((log.parent / f"baseline-{name}.process.json").read_text())
            exit_code = process["exit_code"]
        except (OSError, ValueError, KeyError, TypeError):
            exit_code = None
        if exit_code == 0:
            continue
        failed += 1
        blocks = panic_blocks(log.read_text(errors="replace"))
        print(f"::group::{name}: exit code {exit_code}, {len(blocks)} panic(s)", file=out)
        # Test output is data. Nothing in it may act as a runner command.
        token = secrets.token_hex(16)
        print(f"::stop-commands::{token}", file=out)
        for block in blocks:
            print("\n".join(line.replace(token, "") for line in block), file=out)
            print(file=out)
            shown += 1
        print(f"::{token}::", file=out)
        print("::endgroup::", file=out)
    print(f"{shown} panic block(s) from {failed} failed of {len(logs)} candidate binaries", file=out)
    return shown


def self_test():
    import io
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        candidate = root / "20260101T000000Z" / "candidate"
        candidate.mkdir(parents=True)
        (candidate / "baseline-quiet.stderr.log").write_text("warning: nothing\n")
        (candidate / "baseline-quiet.process.json").write_text('{"exit_code": 0}')
        (candidate / "baseline-expected.stderr.log").write_text(
            "thread 'should_panic' (2) panicked at a:1:1:\nexpected\n")
        (candidate / "baseline-expected.process.json").write_text('{"exit_code": 0}')
        (candidate / "baseline-loud.process.json").write_text('{"exit_code": 101}')
        (candidate / "baseline-unrecorded.stderr.log").write_text(
            "thread 'u' (3) panicked at b:1:1:\n::error::injected\n")
        (candidate / "baseline-loud.stderr.log").write_text(
            "noise\nthread 'a::b' (7) panicked at src/x.rs:1:2:\nfirst\nsecond\n\nignored\n"
            "thread 'c' (8) panicked at src/y.rs:3:4:\n" + "z" * 5000 + "\nstack backtrace:\n   0: frame\n")
        other = root / "20260101T000000Z" / "old-base"
        other.mkdir()
        (other / "baseline-loud.stderr.log").write_text("thread 'old' (1) panicked at a:1:1:\nold\n")
        out = io.StringIO()
        assert report(root, out) == 3
        text = out.getvalue()
        assert "::group::loud: exit code 101, 2 panic(s)" in text and "quiet" not in text
        assert "should_panic" not in text, "a binary that exited zero is not reported"
        assert "::group::unrecorded: exit code None, 1 panic(s)" in text
        lines = text.splitlines()
        injected = lines.index("::error::injected")
        stops = [i for i, line in enumerate(lines) if line.startswith("::stop-commands::")]
        opened = max(i for i in stops if i < injected)
        resume = "::" + lines[opened][len("::stop-commands::"):] + "::"
        assert resume in lines[injected:], "commands resume only after the test output"
        assert resume not in lines[opened:injected]
        assert "first\nsecond\n" in text and "ignored" not in text and "frame" not in text
        assert "'old'" not in text, "only the candidate run is reported"
        assert "z" * MAX_LINE_CHARS in text and "z" * (MAX_LINE_CHARS + 1) not in text
        assert "3 panic block(s) from 2 failed of 4 candidate binaries" in text
        empty = io.StringIO()
        assert report(root / "missing", empty) == 0
    print("PASS: panic blocks of failed candidate binaries are extracted, bounded, and printed as data")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path)
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    if arguments.self_test:
        self_test()
        return 0
    if arguments.root is None:
        parser.error("the comparison directory is required")
    report(arguments.root, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
