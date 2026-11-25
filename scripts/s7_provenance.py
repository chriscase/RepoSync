"""Verify one file revision from the synthetic personal-mode squash-PR canary."""
import argparse
from pathlib import Path
import re
import sys
import xml.etree.ElementTree as ET

MAX_LOG_BYTES = 1024 * 1024
SHA = re.compile(r"[a-fA-F0-9]{40}")
NUMBER = re.compile(r"[1-9][0-9]*")


def check(log, revision, baseline, pr, member_sha, merge_sha):
    if not NUMBER.fullmatch(revision) or not re.fullmatch(r"0|[1-9][0-9]*", baseline):
        raise ValueError("missing or malformed SVN revision")
    if int(revision) <= int(baseline):
        raise ValueError("file revision predates this PR")
    if not NUMBER.fullmatch(pr) or not SHA.fullmatch(member_sha) or not SHA.fullmatch(merge_sha):
        raise ValueError("missing or malformed S5/S6 PR identity")
    if len(log) > MAX_LOG_BYTES:
        raise ValueError("SVN log exceeds the one-revision size limit")
    document = log.decode("utf-8-sig")
    if "<!DOCTYPE" in document:
        raise ValueError("unexpected SVN log document type")

    root = ET.fromstring(document)
    entries = root.findall("logentry")
    if root.tag != "log" or len(root) != 1 or len(entries) != 1 or entries[0].get("revision") != revision:
        raise ValueError("expected one log entry for the selected file revision")
    messages = entries[0].findall("msg")
    if len(messages) != 1 or len(messages[0]) or not messages[0].text:
        raise ValueError("missing or malformed revision message")

    # Personal's default formatter appends this paragraph after the original Git
    # message. Ignore any field-looking text in the original subject or body.
    paragraph = re.split(r"\n[ \t]*\n", messages[0].text.rstrip())[-1].splitlines()
    if not paragraph or paragraph[0] != "[reposync] Synced from Git":
        raise ValueError("missing Personal replay trailer block")
    expected = {
        "git-sha": member_sha.lower(),
        "pr-number": "#" + pr,
        "reposync-git-sha": member_sha.lower(),
    }
    observed = {}
    for line in paragraph[1:]:
        label, separator, text = line.partition(":")
        if not separator or not re.fullmatch(r"[A-Za-z][A-Za-z0-9-]*", label) or not text.strip():
            raise ValueError("malformed trailer in the Personal replay block")
        key = label.lower()
        if key in expected:
            if key in observed:
                raise ValueError(f"duplicate {label} trailer")
            value = text.strip(" \t")
            observed[key] = value if key == "pr-number" else value.lower()
    for key, value in expected.items():
        if key not in observed:
            raise ValueError(f"missing {key} trailer")
        if observed[key] != value:
            raise ValueError(f"malformed or mismatched {key} trailer")
    return f"r{revision}, PR=#{pr}, replayed={member_sha.lower()}, merge={merge_sha.lower()}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("log", "revision", "baseline", "pr", "member_sha", "merge_sha"):
        parser.add_argument(name)
    args = parser.parse_args()
    try:
        with Path(args.log).open("rb") as source:
            log = source.read(MAX_LOG_BYTES + 1)
        print(check(log, args.revision, args.baseline, args.pr, args.member_sha, args.merge_sha))
    except (OSError, ValueError, ET.ParseError) as error:
        print(f"S7 provenance failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
