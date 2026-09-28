#!/usr/bin/env python3
"""Fail-closed scan of synthetic reliability evidence before publication."""

import argparse
import json
import os
import stat
import sys
from pathlib import Path


CANARY = b"REPOSYNC_SYNTHETIC_SECRET_CANARY_72"
MAX_FILES = 10000
MAX_BYTES = 512 * 1024 * 1024


class ScanFailure(Exception):
    pass


def scan_tree(root: Path) -> tuple[int, int]:
    if root.is_symlink() or not root.is_dir():
        raise ScanFailure("evidence_root_unavailable")
    count = 0
    size = 0
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise ScanFailure("directory_read_error") from error
        for entry in entries:
            try:
                mode = entry.stat(follow_symlinks=False).st_mode
            except OSError as error:
                raise ScanFailure("entry_stat_error") from error
            if stat.S_ISLNK(mode):
                raise ScanFailure("symlink_in_evidence")
            if stat.S_ISDIR(mode):
                pending.append(Path(entry.path))
                continue
            if not stat.S_ISREG(mode):
                raise ScanFailure("nonregular_evidence_entry")
            count += 1
            if count > MAX_FILES:
                raise ScanFailure("file_limit_exceeded")
            try:
                with open(entry.path, "rb") as evidence:
                    previous = b""
                    while chunk := evidence.read(65536):
                        size += len(chunk)
                        if size > MAX_BYTES:
                            raise ScanFailure("byte_limit_exceeded")
                        if CANARY in previous + chunk:
                            raise ScanFailure("synthetic_canary_found")
                        previous = chunk[-(len(CANARY) - 1):]
            except OSError as error:
                raise ScanFailure("file_read_error") from error
    if count == 0:
        raise ScanFailure("no_evidence_files")
    return count, size


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scan", type=Path, required=True)
    parser.add_argument("--status-file", type=Path, required=True)
    parser.add_argument("--summary-file", type=Path)
    args = parser.parse_args()
    try:
        files, total_bytes = scan_tree(args.scan)
        result = {"executed": True, "result": "PASS", "files_scanned": files,
                  "bytes_scanned": total_bytes}
        if args.summary_file is not None and args.summary_file.exists():
            summary = json.loads(args.summary_file.read_text())
            summary["evidence_scan"] = result
            args.summary_file.write_text(json.dumps(summary, indent=2) + "\n")
        args.status_file.write_text(json.dumps(result, indent=2) + "\n")
        # Include the newly written status and summary in the verified tree.
        scan_tree(args.scan)
        print(f"EVIDENCE_SCAN PASS files={files} bytes={total_bytes}")
        return 0
    except (ScanFailure, OSError, ValueError, json.JSONDecodeError) as error:
        reason = error.args[0] if isinstance(error, ScanFailure) else "scan_execution_error"
        result = {"executed": True, "result": "FAIL", "reason": reason}
        try:
            args.status_file.write_text(json.dumps(result, indent=2) + "\n")
        except OSError:
            pass
        print(f"EVIDENCE_SCAN FAIL reason={reason}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
