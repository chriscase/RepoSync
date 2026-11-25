"""Exercise the S7 matcher using only synthetic SVN log XML."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

from s7_provenance import check, MAX_LOG_BYTES

MEMBER = "abc123def456789012345678901234567890abcd"
MERGE = "f" * 40
TRAILERS = (
    "[reposync] Synced from Git\n"
    f"Git-SHA: {MEMBER}\nPR-Number: #42\nPR-Branch: validation/canary\n"
    f"RepoSync-Operation: synthetic-operation\nRepoSync-Git-SHA: {MEMBER}"
)
MESSAGE = "Synthetic canary\n\n" + TRAILERS


def xml(*entries):
    root = ET.Element("log")
    for revision, message in entries:
        entry = ET.SubElement(root, "logentry", revision=revision)
        ET.SubElement(entry, "msg").text = message
    return ET.tostring(root)


def validate(data, rev="100", baseline="99", pr="42", member=MEMBER, merge=MERGE):
    return check(data, rev, baseline, pr, member, merge)


class ReplayProof(unittest.TestCase):
    def test_current_personal_replay_and_recorded_squash(self):
        proof = validate(xml(("100", MESSAGE)))
        self.assertIn(f"replayed={MEMBER}", proof)
        self.assertIn(f"merge={MERGE}", proof)

    def test_normalized_full_hex_and_xml_escapes(self):
        message = "<canary> & test\n\n" + TRAILERS.replace(MEMBER, MEMBER.upper())
        self.assertIn("r100", validate(xml(("100", message))))

    def test_rejects_inexact_field_values(self):
        for old, new in [
            ("PR-Number: #42", "PR-Number: #420"),
            ("PR-Number: #42", "PR-Number: #42extra"),
            ("PR-Number: #42", "Body: mentions PR #42"),
            ("Git-SHA: " + MEMBER, "Git-SHA: " + MERGE),
            ("Git-SHA: " + MEMBER, "Git-SHA: " + MEMBER + "a"),
            ("Git-SHA: " + MEMBER, "Git-SHA: " + MEMBER[:8]),
            ("RepoSync-Git-SHA: " + MEMBER, "RepoSync-Git-SHA: " + MERGE),
        ]:
            with self.subTest(new=new), self.assertRaises(ValueError):
                validate(xml(("100", MESSAGE.replace(old, new, 1))))

    def test_requires_one_of_each_typed_trailer(self):
        for line in [f"Git-SHA: {MEMBER}", "PR-Number: #42", f"RepoSync-Git-SHA: {MEMBER}"]:
            for replacement, error in [("", "missing"), (line + "\n" + line, "duplicate")]:
                lines = MESSAGE.splitlines()
                idx = lines.index(line)
                lines[idx:idx + 1] = [replacement] if replacement else []
                with self.subTest(line=line, error=error), self.assertRaisesRegex(ValueError, error):
                    validate(xml(("100", "\n".join(lines))))

    def test_refuses_body_mentions_and_cross_revision_matches(self):
        partial = TRAILERS.replace("PR-Number: #42", "PR-Number: #99")
        samples = [
            xml(("100", MESSAGE + "\n\nFurther prose")),
            xml(("100", "PR-Number: #42\n\n" + partial)),
            xml(("100", f"Git-SHA: {MEMBER}; this references #42")),
            xml(("100", partial), ("98", MESSAGE)),
            xml(("100", partial), ("98", "PR-Number: #42")),
        ]
        for data in samples:
            with self.subTest(xml=data), self.assertRaises(ValueError):
                validate(data)

    def test_rejects_missing_and_stale_revision(self):
        for data, baseline in [(xml(), "99"), (xml(("98", MESSAGE)), "99"),
                               (xml(("100", MESSAGE)), "100"), (xml(("100", MESSAGE)), "101")]:
            with self.subTest(xml=data, baseline=baseline), self.assertRaises(ValueError):
                validate(data, baseline=baseline)

    def test_rejects_missing_and_malformed_expected_identifiers(self):
        for key in ["pr", "member", "merge", "rev", "baseline"]:
            for value in ["", "?", "1foo"]:
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    validate(xml(("100", MESSAGE)), **{key: value})

    def test_rejects_malformed_missing_and_oversized_xml(self):
        for data in [b"<log>", b"<other/>", b"<log><logentry revision='100'/></log>",
                     b"<!DOCTYPE log><log/>", b"x" * (MAX_LOG_BYTES + 1)]:
            with self.subTest(size=len(data)), self.assertRaises((ValueError, ET.ParseError)):
                validate(data)

    def test_live_command_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "log.xml"
            command = [sys.executable, str(Path(__file__).with_name("s7_provenance.py")),
                       str(path), "100", "99", "42", MEMBER, MERGE]
            for data, expected in [(xml(("100", MESSAGE)), 0), (b"<log>", 1)]:
                path.write_bytes(data)
                result = subprocess.run(command, capture_output=True, check=False)
                self.assertEqual(result.returncode, expected, result.stderr)


if __name__ == "__main__":
    unittest.main()
