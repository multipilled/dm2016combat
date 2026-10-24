#!/usr/bin/env python3
"""Fails if repo files (or the staged commit) use the agent tooling's wording for the dev machine.

Repo files talk about "the dev machine" and "the file copy tool". The tooling's own names leak in easily:
the copy tool's function name, and a short tooling word for the machine the agents work on (with "on the",
"onto the", "from the" in front, a possessive followed by shell / clock / filesystem, a "-local" suffix, or
the machine's home path). Those are blocked. The same word as geometry or UI stays allowed (collision box,
bounding box, hitbox, clip box, text box, check box, skybox, Aabb / BoundingBox, "the box's first axis"):
a phrase is blocked only when its words point at a machine. The patterns below spell the word in pieces so
this file and its tests pass their own check.

Usage:
  python tools/check_wording.py            # every tracked file
  python tools/check_wording.py --staged   # what is about to be committed
Tests: python3 -B -m unittest discover -s tools -p "test_*.py"
"""
import re
import subprocess
import sys

_B = "b" "ox"  # the word, in pieces

# The machine-side nouns that make "box" mean the dev machine.
_MACHINE = (r"(?:shell|terminal|browser|desktop|clock|timezone|filesystem|file\s?system|disk|workspace|machine|"
            r"local|session|login|toolchain)")

BLOCKED = [
    # The copy tools' function names, in any spelling.
    (re.compile(r"copy_?(?:to|from)_?" + _B, re.IGNORECASE), "the file copy tool's internal name"),
    # The machine's home path.
    (re.compile(r"/home/" + _B + r"\b"), "the dev machine's home path"),
    # <word>-local, <word> clock, <word> shell ...
    (re.compile(r"\b" + _B + r"[-\s]" + _MACHINE + r"\b", re.IGNORECASE), "'box' meaning the dev machine"),
    # the <word>'s shell / clock / filesystem ...
    (re.compile(r"\b(?:the|this|my|your|our)\s+" + _B + r"['\u2019]s\s+" + _MACHINE + r"\b", re.IGNORECASE),
     "'box' meaning the dev machine"),
    # on / onto / from / to / into / off the <word> (the noun right after the article; "on the clip box" is fine)
    (re.compile(r"\b(?:on|onto|from|to|into|off)\s+(?:the|this|my|your|our)\s+" + _B + r"\b(?!['\u2019-])",
                re.IGNORECASE), "'box' meaning the dev machine"),
    # dev / agent / linux / build / remote / shared <word>
    (re.compile(r"\b(?:dev|agent|agents'|linux|build|remote|shared)\s+" + _B + r"(?:es)?\b", re.IGNORECASE),
     "'box' meaning the dev machine"),
]

# Geometry and UI words that stay allowed (kept explicit so a reader sees them; the regexes above already
# need the article right before the word).
ALLOWED_EXAMPLES = ["collision box", "bounding box", "hitbox", "clip box", "text box", "check box", "skybox"]


def problems_in_text(text: str) -> list[tuple[int, str, str]]:
    found = []
    for n, line in enumerate(text.splitlines(), 1):
        for rx, why in BLOCKED:
            m = rx.search(line)
            if m:
                found.append((n, m.group(0), why))
    return found


def git(*args: str) -> bytes:
    return subprocess.run(["git", *args], check=True, capture_output=True).stdout


def files_to_check(staged: bool) -> list[str]:
    out = git("diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z") if staged else git("ls-files", "-z")
    return [p for p in out.decode("utf-8").split("\0") if p]


def read(path: str, staged: bool) -> bytes:
    if staged:
        return git("show", f":{path}")
    with open(path, "rb") as f:
        return f.read()


def main() -> int:
    staged = "--staged" in sys.argv[1:]
    bad = []
    for path in files_to_check(staged):
        try:
            data = read(path, staged)
        except (OSError, subprocess.CalledProcessError):
            continue
        if b"\0" in data[:8192]:
            continue  # binary
        text = data.decode("utf-8", errors="replace")
        bad += [f"  {path}:{n}: {hit!r}: {why}" for n, hit, why in problems_in_text(text)]
    if bad:
        print("Blocked: tooling wording in repo files.", file=sys.stderr)
        print("\n".join(bad), file=sys.stderr)
        print("Say 'the dev machine' and 'the file copy tool' instead.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
