#!/usr/bin/env python3
"""Update only the binaries of an existing, private, user-owned installation.

Invoked by install.sh --offline-update. This mode never starts/stops a process,
opens the database, obtains credentials, contacts a server or installs a service.
The caller must use a stopped installation and a checksum-pinned native package.
It is not a transaction across process crashes or an ABI compatibility check.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import tarfile
import tempfile


BINARIES = ("reposync", "reposync-daemon")
MAX_PACKAGE_BYTES = 512 * 1024 * 1024


class CleanupFailure(RuntimeError):
    """A secondary cleanup error must not hide the observed update outcome."""

    def __init__(self, stage, cleanup, primary, receipt, rollback):
        super().__init__(str(primary) if primary is not None else
                         "update completed but staging cleanup failed")
        self.result = {"error": str(self), "cleanup_error": str(cleanup),
                       "cleanup_completed": False, "staging_path": str(stage),
                       "update_outcome": "completed" if receipt is not None else "failed",
                       "rollback_outcome": rollback, "activation_performed": False}
        if receipt is not None:
            self.result["update_receipt"] = receipt


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(stream):
    value = hashlib.sha256()
    while block := stream.read(1024 * 1024):
        value.update(block)
    return value.hexdigest()


def owned_node(path, directory=False):
    info = path.lstat()
    require(info.st_uid == os.getuid() and
            (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)),
            f"not an owned {'directory' if directory else 'regular file'}: {path}")
    return info


def update(root, archive, expected):
    require(re.fullmatch(r"[0-9a-f]{64}", expected) is not None,
            "expected package SHA256 must contain exactly 64 lowercase hex digits")
    root = Path(root)
    require(root.is_absolute() and root.resolve(strict=True) == root,
            "root must be an existing absolute canonical path without symlinks")
    info = owned_node(root, directory=True)
    require(stat.S_IMODE(info.st_mode) & 0o077 == 0,
            "offline root must already be private to its owner")
    for name in ("bin", "config", "data"):
        owned_node(root / name, directory=True)
    owned_node(root / "config/config.toml")
    owned_node(root / "data/reposync.db")
    modes = {}
    for name in BINARIES:
        old = owned_node(root / "bin" / name)
        modes[name] = stat.S_IMODE(old.st_mode)
        require(modes[name] & 0o100 and not modes[name] & 0o6000,
                "existing binaries must be owner-executable without privileged mode bits")

    archive = Path(archive)
    owned_node(archive)
    require(archive.stat().st_size <= MAX_PACKAGE_BYTES, "compressed package is too large")
    # Validate checksum and every member before creating anything in the root.
    with archive.open("rb") as stream:
        require(digest(stream) == expected, "package checksum mismatch")
        stream.seek(0)
        with tarfile.open(fileobj=stream, mode="r:gz") as package:
            members = {}
            for member in package:
                require(member.name in BINARIES and member.name not in members and
                        member.isfile() and 0 < member.size <= MAX_PACKAGE_BYTES and
                        member.mode & 0o100 and not member.mode & 0o6000,
                        "package must contain exactly two regular, executable binary members")
                members[member.name] = member
                require(sum(item.size for item in members.values()) <= MAX_PACKAGE_BYTES,
                        "expanded package is too large")
            require(set(members) == set(BINARIES), "package binary inventory is incomplete")
            lock = root / "bin/.reposync-offline-update.lock"
            if lock.exists() or lock.is_symlink():
                owned_node(lock)
            flags = os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW
            with os.fdopen(os.open(lock, flags, 0o600), "r+b") as owner:
                fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
                # Recheck targets after acquiring the updater's own lock.
                for name in BINARIES:
                    current = owned_node(root / "bin" / name)
                    require(stat.S_IMODE(current.st_mode) == modes[name],
                            "existing binary mode changed during package validation")
                stage = Path(tempfile.mkdtemp(prefix=".reposync-update-", dir=root / "bin"))
                retain = False
                primary = None
                receipt = None
                rollback = "not_attempted"
                try:
                    hashes = {}
                    for name in BINARIES:
                        payload = stage / name
                        with package.extractfile(members[name]) as source, payload.open("xb") as target:
                            shutil.copyfileobj(source, target)
                            target.flush()
                            os.fsync(target.fileno())
                        require(payload.stat().st_size == members[name].size, "short package member")
                        payload.chmod(modes[name])
                        with payload.open("rb") as source:
                            hashes[name] = digest(source)
                        shutil.copy2(root / "bin" / name, stage / (name + ".previous"))
                    stream.seek(0)
                    require(digest(stream) == expected, "package changed during validation")
                    changed = []
                    try:
                        for name in BINARIES:
                            with (root / "bin" / name).open("rb") as source:
                                if digest(source) == hashes[name]:
                                    continue
                            os.replace(stage / name, root / "bin" / name)
                            changed.append(name)
                    except OSError as cause:
                        errors = []
                        for name in reversed(changed):
                            try:
                                os.replace(stage / (name + ".previous"), root / "bin" / name)
                            except OSError as error:
                                errors.append(f"{name}: {error}")
                        if errors:
                            retain = True
                            raise RuntimeError(f"update failed: {cause}; rollback uncertain: {errors}; "
                                               f"retain recovery files at {stage}") from cause
                        rollback = "completed"
                        raise
                    receipt = {"mode": "offline-owned-root-update", "root": str(root),
                               "package_sha256": expected, "binary_sha256": hashes,
                               "changed": changed, "activation_performed": False,
                               "configuration_or_database_opened": False}
                    return receipt
                except Exception as error:
                    primary = error
                    raise
                finally:
                    if not retain:
                        try:
                            shutil.rmtree(stage)
                        except OSError as cleanup:
                            raise CleanupFailure(stage, cleanup, primary, receipt, rollback) from (
                                primary if primary is not None else cleanup)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--sha256", required=True)
    args = parser.parse_args(argv)
    try:
        print(json.dumps(update(args.root, args.archive, args.sha256), sort_keys=True))
        return 0
    except CleanupFailure as error:
        print(json.dumps(error.result, sort_keys=True))
        return 1
    except (OSError, ValueError, RuntimeError, tarfile.TarError) as error:
        print(json.dumps({"error": str(error), "activation_performed": False}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
