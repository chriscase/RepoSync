#!/usr/bin/env python3
"""RepoSync's opt-in, clone-local pre-rebase and pre-push checks."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


class Refusal(Exception):
    """The paired branch would be rewritten, or its safety could not be checked."""


def git(*args):
    # An advertised OID must be proved from local, unmodified objects. In
    # particular, a missing promisor object must never result in a fetch.
    env = os.environ.copy()
    env.update(
        GIT_ALLOW_PROTOCOL="",
        GIT_GRAFT_FILE=os.devnull,
        GIT_NO_LAZY_FETCH="1",
        GIT_NO_REPLACE_OBJECTS="1",
        GIT_OPTIONAL_LOCKS="0",
    )
    return subprocess.run(
        ["git", "--no-lazy-fetch", "--no-replace-objects", *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
        check=False,
    )


def settings():
    folder = Path(__file__).resolve().parent
    common = git("rev-parse", "--path-format=absolute", "--git-common-dir")
    if common.returncode or not common.stdout:
        raise Refusal("cannot identify the current clone")
    common_dir = Path(os.fsdecode(common.stdout.removesuffix(b"\n")))
    if common_dir.resolve() != folder.parent:
        raise Refusal("this guard does not belong to the current clone")
    path = folder / "settings.json"
    if path.is_symlink() or not path.is_file():
        raise Refusal("the paired-branch settings must be a regular file")
    with path.open(encoding="utf-8") as file:
        pins = json.load(file)
    if (
        not isinstance(pins, dict)
        or set(pins) != {"version", "branch", "remote"}
        or type(pins["version"]) is not int
        or pins["version"] != 1
    ):
        raise Refusal("invalid paired-branch settings")
    branch, remote = pins["branch"], pins["remote"]
    for name in (branch, remote):
        if (
            not isinstance(name, str)
            or not name
            or name in {"@", "HEAD"}
            or name.startswith(("-", "refs/"))
        ):
            raise Refusal("invalid paired-branch or named-remote setting")
    ref = f"refs/heads/{branch}"
    for name in (ref, f"refs/remotes/{remote}/x"):
        if git("check-ref-format", name).returncode:
            raise Refusal("invalid paired-branch or named-remote setting")
    return ref, remote


def pre_rebase(args, ref):
    if len(args) not in (1, 2):
        raise Refusal("expected the upstream and optional branch from Git")
    # The upstream is not the branch being rebased. Git supplies a second
    # argument only when the caller named a target. HEAD is worktree-specific.
    target = args[1] if len(args) == 2 else "HEAD"
    if target in ("HEAD", "@"):
        head = git("symbolic-ref", "--quiet", "HEAD")
        if head.returncode == 1:  # Detached HEAD has no local branch to rewrite.
            return
        if head.returncode:
            raise Refusal("cannot identify the current worktree's branch")
        target = os.fsdecode(head.stdout.removesuffix(b"\n"))
    if target in (ref, ref.removeprefix("refs/heads/")):
        raise Refusal("rebasing the paired branch is blocked")


def prove_push(old, new, width):
    if any(len(oid) != width or not re.fullmatch(rb"[0-9a-fA-F]+", oid) for oid in (old, new)):
        raise Refusal("Git supplied an invalid object ID for the paired branch")
    zero = b"0" * width
    if new == zero:
        raise Refusal("deleting the paired remote branch is blocked")
    if old == zero:
        raise Refusal("creating or recreating the paired remote branch is blocked")
    for oid in (old, new):
        kind = git("cat-file", "-t", oid)
        if kind.returncode or kind.stdout != b"commit\n":
            raise Refusal("cannot verify both paired-branch commits from local objects")
    if git("merge-base", "--is-ancestor", old, new).returncode:
        raise Refusal("cannot prove a fast-forward of the paired remote branch")


def pre_push(args, ref, remote, updates):
    if len(args) != 2:
        raise Refusal("expected the remote name and location from Git")
    # Only the remote name participates. The location can contain a credential.
    selected = args[0] == remote
    destination = os.fsencode(ref)
    width = None
    refusal = None
    # Drain every advertised update, even if one already vetoed the whole push.
    for line in updates:
        if not selected or refusal is not None:
            continue
        fields = line.removesuffix(b"\n").rsplit(b" ", 3)
        if len(fields) != 4 or not all(fields):
            refusal = Refusal("cannot identify a Git pre-push update")
            continue
        _, new, remote_ref, old = fields
        if remote_ref != destination:
            continue
        try:
            if width is None:
                fmt = git("rev-parse", "--show-object-format=storage")
                width = {b"sha1\n": 40, b"sha256\n": 64}.get(fmt.stdout)
                if fmt.returncode or width is None:
                    raise Refusal("cannot identify the local object format")
            prove_push(old, new, width)
        except Refusal as error:
            refusal = error
    if refusal is not None:
        raise refusal


def main():
    try:
        if len(sys.argv) < 2:
            raise Refusal("expected pre-rebase or pre-push")
        event, *args = sys.argv[1:]
        ref, remote = settings()
        if event == "pre-rebase":
            pre_rebase(args, ref)
        elif event == "pre-push":
            pre_push(args, ref, remote, sys.stdin.buffer)
        else:
            raise Refusal("expected pre-rebase or pre-push")
    except Refusal as error:
        print(f"RepoSync paired branch: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError):
        print("RepoSync paired branch: cannot read settings or run local Git", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
