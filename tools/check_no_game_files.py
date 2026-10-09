#!/usr/bin/env python3
"""Fails if the repo (or the staged commit) contains anything from the original game.

A remake repo only ever holds new code. Game data is read from the player's own copy at
runtime, so a game file, a save, or decompiled game code in the repo is a bug.

Adapt the GAME SETTINGS block for each game (container extensions, save magic bytes, the
game's own namespaces), then copy this file to tools/ and the pre-commit hook to .githooks/.

Usage:
  python tools/check_no_game_files.py            # every tracked file
  python tools/check_no_game_files.py --staged   # what is about to be committed
"""
import re
import subprocess
import sys

# ---- GAME SETTINGS (DOOM 2016 / idTech 6) ------------------------------------------------------
MAX_BYTES = 5 * 1024 * 1024

# File names that only come from the game or a build: idTech 6 containers and their resources
# (.resources / .index / .pindex / .streamdb / .mega2 / .pages), decls and binary files, md6
# models / anims / skeletons, maps, SWF, Wwise banks and media, binaries.
BLOCKED_NAME = re.compile(
    r"(\.(resources|index|pindex|streamdb|mega2|pages|decl|bfile|bimage|bmd6model|bmd6anim|bmd6skl|md6model|md6anim|md6skl"
    r"|bmodel|bcm|entities|bswf|swf|bnk|wem|snd|dll|exe|pdb|so)$)",
    re.IGNORECASE,
)

# Magic bytes at the start of the game's own containers.
GAME_MAGIC: list[bytes] = [b"IDCL"]

# The game's own code identifiers only appear in decompiled code (the remake cites addresses instead).
GAME_NAMESPACES: list[str] = []
# ----------------------------------------------------------------------------------------------

DECOMPILED_MARKERS = [
    re.compile(rb"ICSharpCode\.Decompiler"),
    re.compile(rb"<Private" rb"ImplementationDetails>"),  # split so this file passes its own check
    re.compile(rb"//\s*Decompiled with", re.IGNORECASE),
    re.compile(rb"/\*\s*WARNING: Decompiled", re.IGNORECASE),  # Ghidra
    re.compile(rb"/\*\s*WARNING: Globals starting with"),  # Ghidra (gamedata/re decompiles)
    re.compile(rb"^// ==== FUN_[0-9a-f]+ @", re.MULTILINE),  # tools/*_decomp.sh output
] + [re.compile(rb"^\s*(namespace|using|package|import)\s+" + re.escape(n.encode()) + rb"\b", re.MULTILINE)
     for n in GAME_NAMESPACES]


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


def is_unity_serialized(data: bytes) -> bool:
    if len(data) < 64:
        return False
    fmt = int.from_bytes(data[8:12], "big")
    return 9 <= fmt <= 30 and re.search(rb"\d{4}\.\d+\.\d+[abfp]\d+\0", data[16:64]) is not None


def problems_in(path: str, data: bytes) -> list[str]:
    found = []
    if BLOCKED_NAME.search(path):
        found.append("file type that only comes from the game or a build")
    if len(data) > MAX_BYTES:
        found.append(f"larger than {MAX_BYTES // (1024 * 1024)} MB")
    head = data[:64]
    if head.startswith(b"UnityFS\0") or head.startswith(b"UnityWeb") or is_unity_serialized(data):
        found.append("Unity asset data")
    if b"FSB5" in head[:8] or head.startswith(b"BKHD") or head.startswith(b"RIFF") and b"WAVE" in head[:16] and path.endswith(".wem"):
        found.append("sound bank")
    if any(head.startswith(m) for m in GAME_MAGIC):
        found.append("the game's save or data file")
    if any(m.search(data) for m in DECOMPILED_MARKERS):
        found.append("looks like decompiled game code")
    return found


def main() -> int:
    staged = "--staged" in sys.argv[1:]
    bad = []
    for path in files_to_check(staged):
        try:
            data = read(path, staged)
        except (OSError, subprocess.CalledProcessError):
            continue
        bad += [f"  {path}: {p}" for p in problems_in(path, data)]
    if bad:
        print("Blocked: these files must not be in the repo.", file=sys.stderr)
        print("\n".join(bad), file=sys.stderr)
        print("Game data stays on the player's PC; the importer reads it at runtime.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
