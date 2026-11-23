"""Create a disposable local SVN history for partial-sync tests (#60).

Usage: python3 tests/fixtures/monorepo/seed.py NEW_FIXTURE_DIRECTORY
The destination must not exist. The repository is created at NEW_FIXTURE_DIRECTORY/repo.
"""

import argparse
import subprocess
import tempfile
from pathlib import Path


def seed(root):
    # Never load data into a caller-supplied repository or reuse a working copy.
    root = root.absolute()
    root.mkdir()
    repo = root / "repo"
    subprocess.run(["svnadmin", "create", repo], check=True)
    with tempfile.TemporaryDirectory(dir=root) as temp:
        work = Path(temp)
        config = work / "config"
        config.mkdir()
        source = work / "seed"
        trunk = source / "trunk"
        for name, value in (
            ("team-a/readme.txt", "alpha v1"),
            ("team-a/build.txt", "build v1"),
            ("team-b/readme.txt", "beta v1"),
            ("team-c/readme.txt", "gamma v1"),
            ("docs/index.txt", "shared v1"),
        ):
            path = trunk / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((value + "\n").encode("ascii"))

        def svn(*args):
            subprocess.run(
                ["svn", "--non-interactive", "--no-auth-cache", "--config-dir", config, *args],
                check=True,
                stdout=subprocess.DEVNULL,
            )

        # Import one tree atomically; the following commits only edit tracked files.
        svn("import", "-m", "r1: seed three teams and shared docs", source, repo.as_uri())
        wc = work / "wc"
        svn("checkout", repo.as_uri() + "/trunk", wc)
        (wc / "team-a/readme.txt").write_bytes(b"alpha selected revision two\n")
        (wc / "team-b/readme.txt").write_bytes(b"beta mixed revision two\n")
        svn("commit", "-m", "r2: mixed team-a and team-b edit", wc)
        (wc / "team-b/readme.txt").write_bytes(b"beta excluded-only final revision\n")
        svn("commit", "-m", "r3: team-b only", wc)
    return repo


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        print(seed(args.destination))
    except FileExistsError:
        parser.exit(1, f"refusing to reuse an existing path: {args.destination}\n")
