#!/usr/bin/env python3
"""Print the panic output of failed candidate catalog tests to the job log.

The evidence archive holds each test binary's captured stderr. This prints
only the panic blocks from the candidate comparison run, so a failure can be
read from the job log without downloading the archive. The workflow runs it
only after the evidence scan has passed.
"""
import argparse
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
    shown = 0
    for log in logs:
        blocks = panic_blocks(log.read_text(errors="replace"))
        if not blocks:
            continue
        name = log.name[len("baseline-"):-len(".stderr.log")]
        print(f"::group::{name}: {len(blocks)} panic(s)", file=out)
        for block in blocks:
            print("\n".join(block), file=out)
            print(file=out)
            shown += 1
        print("::endgroup::", file=out)
    print(f"{shown} panic block(s) in {len(logs)} candidate stderr log(s)", file=out)
    return shown


def self_test():
    import io
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        candidate = root / "20260101T000000Z" / "candidate"
        candidate.mkdir(parents=True)
        (candidate / "baseline-quiet.stderr.log").write_text("warning: nothing\n")
        (candidate / "baseline-loud.stderr.log").write_text(
            "noise\nthread 'a::b' (7) panicked at src/x.rs:1:2:\nfirst\nsecond\n\nignored\n"
            "thread 'c' (8) panicked at src/y.rs:3:4:\n" + "z" * 5000 + "\nstack backtrace:\n   0: frame\n")
        other = root / "20260101T000000Z" / "old-base"
        other.mkdir()
        (other / "baseline-loud.stderr.log").write_text("thread 'old' (1) panicked at a:1:1:\nold\n")
        out = io.StringIO()
        assert report(root, out) == 2
        text = out.getvalue()
        assert "::group::loud: 2 panic(s)" in text and "quiet" not in text
        assert "first\nsecond\n" in text and "ignored" not in text and "frame" not in text
        assert "'old'" not in text, "only the candidate run is reported"
        assert "z" * MAX_LINE_CHARS in text and "z" * (MAX_LINE_CHARS + 1) not in text
        assert "2 panic block(s) in 2 candidate stderr log(s)" in text
        empty = io.StringIO()
        assert report(root / "missing", empty) == 0
    print("PASS: candidate panic blocks are extracted, bounded, and limited to the candidate run")


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
