#!/usr/bin/env python3
"""Manage an opt-in, clone-local pair of Git hooks. Install and remove default to preview."""

import argparse
import collections
import contextlib
import hashlib
import json
import os
import shlex
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import fcntl
except ImportError:
    fcntl = None


DIRECTORY = "reposync-paired-branch-hooks"
INCLUDE = DIRECTORY + "/hooks.config"
GUARD = "paired_branch_guard.py"
EVENTS = ("pre-rebase", "pre-push")
PREFIX = "reposync-paired-branch-"
FILES = (GUARD, "settings.json", "hooks.config")
MANIFEST = "manifest.json"
SCOPE = "All worktrees of this clone share the registration."
RETIRING = DIRECTORY + ".removing"
RECEIPT = RETIRING + ".json"
GIT_ENV = {
    "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_NAMESPACE", "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_SHALLOW_FILE",
    "GIT_CONFIG", "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS", "GIT_QUARANTINE_PATH",
    "GIT_REPLACE_REF_BASE",
}


class Refused(Exception):
    pass


def git(repo, *args, allowed=(0,)):
    result = subprocess.run(
        ["git", "-C", os.fspath(repo), *args], stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        env=dict(os.environ, GIT_NO_LAZY_FETCH="1", GIT_NO_REPLACE_OBJECTS="1",
                 GIT_ALLOW_PROTOCOL="", GIT_OPTIONAL_LOCKS="0"),
    )
    if result.returncode not in allowed:
        # An unrelated Git config error can quote a command or a private path.
        raise Refused(f"Git could not complete {args[0]} (exit {result.returncode}).")
    return result


def git_path(repo, flag):
    out = git(repo, "rev-parse", "--path-format=absolute", flag).stdout
    return Path(os.fsdecode(out[:-1])).resolve()


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def hook_config(common, interpreter):
    helper = common / DIRECTORY / GUARD
    sections = []
    for event in EVENTS:
        command = " ".join(shlex.quote(arg) for arg in (interpreter, str(helper), event))
        if any(ord(c) < 32 for c in command):
            raise Refused("Control characters in the installed executable path are unsupported.")
        # This is a Git-config quoted value containing a shell-quoted command.
        value = command.replace("\\", "\\\\").replace('"', '\\"')
        sections.append(f'[hook "{PREFIX}{event}"]\n\tevent = {event}\n\tcommand = "{value}"\n')
    return "".join(sections).encode()


def worktrees(repo):
    fields = git(repo, "worktree", "list", "--porcelain", "-z").stdout.split(b"\0")
    paths = [Path(os.fsdecode(f[9:])) for f in fields if f.startswith(b"worktree ")]
    if not paths:
        raise Refused("No registered worktrees.")
    return paths


def config_entries(repo, pattern):
    result = git(repo, "config", "--includes", "--show-origin", "--null", "--get-regexp",
                 pattern, allowed=(0, 1))
    items = result.stdout.split(b"\0")[:-1]
    for origin, entry in zip(items[::2], items[1::2]):
        key, _, value = entry.partition(b"\n")
        yield os.fsdecode(origin), os.fsdecode(key), os.fsdecode(value)


def verify_config(checked, common, active, installing):
    """Check included config as Git sees it in each linked worktree."""
    target = common / INCLUDE
    for worktree in checked:
        owned_keys = collections.Counter()
        for origin, key, value in config_entries(worktree, r"^(hook\.reposync-paired-branch-pre-(rebase|push)\.|include\.path$|includeif\..*\.path$)"):
            file = None
            if origin.startswith("file:"):
                file = (worktree / origin[5:]).resolve()
            if key.startswith("hook."):
                if file != target or not active:
                    raise Refused("A reserved hook name is already configured outside this installation.")
                owned_keys[key] += 1
            else:
                path = Path(value).expanduser()
                if not path.is_absolute() and file:
                    path = file.parent / path
                if path.is_absolute() and path.resolve() == target:
                    if file != common / "config" or key != "include.path" or value != INCLUDE:
                        raise Refused("Another Git config entry refers to the managed hook include.")
        if active:
            expected = {f"hook.{PREFIX}{event}.{field}" for event in EVENTS for field in ("event", "command")}
            if set(owned_keys) != expected or any(count != 1 for count in owned_keys.values()):
                raise Refused("The owned hook definitions are missing or included more than once.")
        for event in EVENTS:
            # An ephemeral list-only capability probe never invokes an executable.
            name = PREFIX + "capability-probe"
            listed = git(worktree, "-c", f"hook.{name}.command=false",
                         "-c", f"hook.{name}.event={event}", "hook", "list", "-z", event,
                         allowed=(0, 1, 129))
            entries = listed.stdout.split(b"\0")
            if not any(entry.split(b"\t")[-1] == name.encode() for entry in entries):
                raise Refused("Git with native config-defined hooks and 'git hook list' is required.")
            if installing and name.encode() not in entries:
                raise Refused(f"Git has disabled {event} in a worktree.")
            if active and installing and (PREFIX + event).encode() not in entries:
                raise Refused(f"The managed {event} hook is inactive in a worktree.")


def inspect_installation(directory):
    if not os.path.lexists(directory):
        return None
    if not stat.S_ISDIR(directory.lstat().st_mode) or stat.S_IMODE(directory.stat().st_mode) != 0o700:
        raise Refused("The private hook directory is a collision or has changed permissions.")
    if set(os.listdir(directory)) != set(FILES) | {MANIFEST}:
        raise Refused("The private hook directory contains missing, changed or unowned files.")
    contents = {name: private_file(directory / name) for name in (*FILES, MANIFEST)}
    try:
        manifest = json.loads(contents[MANIFEST])
        common = Path(manifest["common_dir"])
        interpreter = manifest["interpreter"]
        settings = json.loads(contents["settings.json"])
        if (not common.is_absolute() or not Path(interpreter).is_absolute()
                or set(settings) != {"version", "branch", "remote"} or settings["version"] != 1
                or not all(isinstance(settings[k], str) for k in ("branch", "remote"))
                or contents["hooks.config"] != hook_config(common, interpreter)
                or contents[MANIFEST] != manifest_bytes(common, interpreter, contents, directory.stat())):
            raise ValueError
    except (KeyError, TypeError, ValueError):
        raise Refused("The managed hook manifest or its files have changed.") from None
    return settings, common, interpreter


def private_file(path):
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or stat.S_IMODE(info.st_mode) != 0o600):
        raise Refused("A managed file is linked, replaced or has changed permissions.")
    return path.read_bytes()


def manifest_bytes(common, interpreter, contents, info):
    record = {
        "format": "reposync-paired-branch-hooks/1",
        "common_dir": str(common), "interpreter": interpreter,
        "device": info.st_dev, "inode": info.st_ino,
        "sha256": {name: sha(contents[name]) for name in FILES},
    }
    return json_bytes(dict(record, record_sha256=sha(json_bytes(record))))


def validate_pins(repo, branch, remote):
    for name, prefix, suffix in ((branch, "refs/heads/", ""), (remote, "refs/remotes/", "/x")):
        if not name or name in ("HEAD", "@") or name.startswith(("-", "refs/")):
            raise Refused("Supply an exact short branch and a named Git remote.")
        if git(repo, "check-ref-format", prefix + name + suffix, allowed=(0, 1)).returncode:
            raise Refused("The selected branch or remote name is invalid.")
    exists = git(repo, "show-ref", "--verify", "--quiet", "refs/heads/" + branch, allowed=(0, 1))
    if exists.returncode:
        raise Refused("The selected local branch does not exist.")
    if os.fsencode(remote) not in git(repo, "remote").stdout.splitlines():
        raise Refused("The selected remote is not configured.")


def prepare(common, settings):
    interpreter = str(Path(sys.executable).resolve())
    contents = {
        GUARD: Path(__file__).resolve().with_name(GUARD).read_bytes(),
        "settings.json": json_bytes(settings),
        "hooks.config": hook_config(common, interpreter),
    }
    # Stage complete, fsynced files before publishing the directory. Git cannot
    # select this registration until the subsequent atomic config-file update.
    with tempfile.TemporaryDirectory(dir=common, prefix=".reposync-hooks-stage-") as scratch:
        staged = Path(scratch) / DIRECTORY
        staged.mkdir(mode=0o700)
        contents[MANIFEST] = manifest_bytes(common, interpreter, contents, staged.stat())
        for name, data in contents.items():
            fd = os.open(staged / name, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            with os.fdopen(fd, "wb") as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
        with directory_fd(staged) as fd:
            os.fsync(fd)
        if os.path.lexists(common / DIRECTORY):
            raise Refused("A hook directory appeared during installation.")
        staged.rename(common / DIRECTORY)


@contextlib.contextmanager
def directory_fd(directory):
    fd = os.open(directory, os.O_DIRECTORY | os.O_RDONLY)
    try:
        yield fd
    finally:
        os.close(fd)


def check_local_config(common):
    info = (common / "config").lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise Refused("Common Git config is linked or non-regular; it must be private to this clone.")


def flush_config(common, fd):
    with (common / "config").open("rb") as config:
        os.fsync(config.fileno())
    os.fsync(fd)


def finish_removal(common, fd, apply):
    # The verified directory is renamed here only after unregistering it. Move
    # its manifest out before unlinking anything, so a retry can identify the
    # original directory and check every surviving file after a process stop.
    retired = common / RETIRING
    receipt = common / RECEIPT
    if os.path.lexists(receipt):
        raw = private_file(receipt)
    else:
        inspect_installation(retired)
        raw = private_file(retired / MANIFEST)
        if apply:
            (retired / MANIFEST).rename(receipt)
            with directory_fd(retired) as retired_fd:
                os.fsync(retired_fd)
            os.fsync(fd)
    try:
        manifest = json.loads(raw)
        hashes = manifest["sha256"]
        checksum = manifest.pop("record_sha256")
        if (manifest["format"] != "reposync-paired-branch-hooks/1" or set(hashes) != set(FILES)
                or not all(isinstance(h, str) and len(h) == 64 for h in hashes.values())
                or checksum != sha(json_bytes(manifest))
                or raw != json_bytes(dict(manifest, record_sha256=checksum))):
            raise ValueError
    except (ValueError, TypeError, KeyError):
        raise Refused("The removal receipt is unrecognized or changed.") from None
    if os.path.lexists(retired):
        info = retired.lstat()
        if (not stat.S_ISDIR(info.st_mode) or (info.st_dev, info.st_ino) !=
                (manifest.get("device"), manifest.get("inode"))):
            raise Refused("The removal directory is not the recorded directory.")
        remaining = set(os.listdir(retired))
        if remaining == set(FILES) | {MANIFEST} and not os.path.lexists(receipt):
            return  # A preview after the directory rename, before moving its manifest.
        if not remaining.issubset(hashes) or any(sha(private_file(retired / n)) != hashes[n] for n in remaining):
            raise Refused("Unowned or edited files remain in the removal directory.")
        if apply:
            for name in remaining:
                (retired / name).unlink()
            retired.rmdir()
            os.fsync(fd)
    if apply:
        receipt.unlink()
        os.fsync(fd)


def manage(args):
    if fcntl is None or not hasattr(os, "O_DIRECTORY"):
        raise Refused("This installer requires POSIX directory locking.")
    if any(k in GIT_ENV or k.startswith(("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_")) for k in os.environ):
        raise Refused("Unset Git repository-routing or config-injection environment variables before selecting --repo.")
    repo = git_path(args.repo, "--show-toplevel")
    if args.repo.resolve() != repo:
        raise Refused("--repo must name the worktree root, not one of its subdirectories.")
    common = git_path(repo, "--git-common-dir")
    directory = common / DIRECTORY
    apply = getattr(args, "apply", False)
    with directory_fd(common) as fd:
        fcntl.flock(fd, (fcntl.LOCK_EX if apply else fcntl.LOCK_SH) | fcntl.LOCK_NB)
        check_local_config(common)
        listed = worktrees(repo)
        checked = [path for path in listed if path.is_dir() and git_path(path, "--git-common-dir") == common]
        if args.action != "remove" and listed != checked:
            raise Refused("A registered worktree is unavailable. Its effective hooks cannot be checked.")
        includes = git(repo, "config", "--local", "--no-includes", "--null", "--get-all",
                       "include.path", allowed=(0, 1)).stdout.split(b"\0")
        count = includes.count(INCLUDE.encode())
        if count > 1:
            raise Refused("The managed include appears more than once in local config.")
        installed = inspect_installation(directory)
        if count and not installed:
            raise Refused("The local Git config includes missing managed files.")
        active = bool(count)
        removing = any(os.path.lexists(common / n) for n in (RETIRING, RECEIPT))
        if removing and (installed or active):
            raise Refused("Active or staged hook files collide with a removal in progress.")
        verify_config(checked, common, active, args.action != "remove" and (active or args.action == "install"))
        if removing:
            if args.action == "install":
                raise Refused("Finish the previous removal before installing.")
            finish_removal(common, fd, apply)
        settings = installed[0] if installed else {}
        if installed and args.action != "remove":
            if installed[1] != common or not os.access(installed[2], os.X_OK):
                raise Refused("This installation has moved or lost its interpreter. Remove and reinstall it.")
        state = "installed" if active else "staged" if installed else "absent"
        if removing and not apply:
            state = "removing"
        changed = removing and args.action == "remove"
        if args.action == "install":
            validate_pins(repo, args.branch, args.remote)
            requested = {"version": 1, "branch": args.branch, "remote": args.remote}
            if installed and settings != requested:
                raise Refused("Different pins are already installed. Remove them before changing pins.")
            settings = requested
            changed = not active
            if apply and changed:
                if not installed:
                    prepare(common, settings)
                    os.fsync(fd)
                inspect_installation(directory)
                check_local_config(common)
                git(repo, "config", "--local", "--add", "include.path", INCLUDE)
                try:
                    verify_config(checked, common, True, True)
                    inspect_installation(directory)
                    flush_config(common, fd)
                except (Refused, OSError, RuntimeError):
                    # --unset with an exact value refuses ambiguous duplicate entries.
                    try:
                        check_local_config(common)
                        git(repo, "config", "--local", "--fixed-value", "--unset", "include.path", INCLUDE)
                        flush_config(common, fd)
                    except (Refused, OSError):
                        raise Refused("Activation did not verify; inspect the common config and retained hook files.")
                    raise Refused("Activation did not verify. The include was removed; files remain staged for removal.")
                state = "installed"
        elif args.action == "remove" and installed:
            changed = True
            if apply:
                if active:
                    check_local_config(common)
                    git(repo, "config", "--local", "--fixed-value", "--unset", "include.path", INCLUDE)
                    flush_config(common, fd)
                verify_config(checked, common, False, False)
                inspect_installation(directory)
                directory.rename(common / RETIRING)
                os.fsync(fd)
                finish_removal(common, fd, True)
                state = "absent"
        return {
            "action": args.action, "applied": apply, "changed": changed, "state": state,
            "repo": str(repo), "common_dir": str(common), "worktrees": list(map(str, checked)),
            "unavailable_worktrees": [str(path) for path in listed if path not in checked],
            "scope": SCOPE, "include_path": INCLUDE, "events": list(EVENTS),
            "branch": settings.get("branch"), "remote": settings.get("remote"),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    for action in ("status", "install", "remove"):
        cmd = commands.add_parser(action)
        cmd.add_argument("--repo", required=True, type=Path)
        if action == "install":
            cmd.add_argument("--branch", required=True)
            cmd.add_argument("--remote", required=True)
        if action != "status":
            cmd.add_argument("--apply", action="store_true")
    try:
        result = manage(parser.parse_args())
    except (Refused, OSError, RuntimeError) as exc:
        print(f"paired branch hooks: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
