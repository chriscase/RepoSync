"""Check the history independently with svnlook; no RepoSync behavior is implied."""

import subprocess
import tempfile
import unittest
from pathlib import Path

from seed import seed


class MonorepoHistory(unittest.TestCase):
    def test_native_history(self):
        with tempfile.TemporaryDirectory() as temp:
            dest = Path(temp) / "fixture with spaces"
            repo = seed(dest)

            def look(command, *args):
                return subprocess.check_output(["svnlook", command, *args, repo])

            self.assertEqual(look("youngest").strip(), b"3")
            r1 = {
                "trunk/team-a/readme.txt": b"alpha v1\n",
                "trunk/team-a/build.txt": b"build v1\n",
                "trunk/team-b/readme.txt": b"beta v1\n",
                "trunk/team-c/readme.txt": b"gamma v1\n",
                "trunk/docs/index.txt": b"shared v1\n",
            }
            r2 = {**r1, "trunk/team-a/readme.txt": b"alpha selected revision two\n",
                  "trunk/team-b/readme.txt": b"beta mixed revision two\n"}
            r3 = {**r2, "trunk/team-b/readme.txt": b"beta excluded-only final revision\n"}
            for rev, expected in zip(("1", "2", "3"), (r1, r2, r3)):
                with self.subTest(revision=rev):
                    tree = look("tree", "--full-paths", "-r", rev).decode().splitlines()
                    self.assertEqual({p for p in tree if not p.endswith("/")}, set(expected))
                    for path, value in expected.items():
                        data = subprocess.check_output(["svnlook", "cat", "-r", rev, repo, path])
                        self.assertEqual(data, value, path)
            for rev, paths in (("2", ["trunk/team-a/readme.txt", "trunk/team-b/readme.txt"]),
                               ("3", ["trunk/team-b/readme.txt"])):
                changed = look("changed", "-r", rev).decode().splitlines()
                self.assertEqual(sorted(changed), ["U   " + path for path in paths])
            # Refuse a second seed rather than appending or overwriting history.
            with self.assertRaises(FileExistsError):
                seed(dest)
            self.assertEqual(look("youngest").strip(), b"3")


if __name__ == "__main__":
    unittest.main()
