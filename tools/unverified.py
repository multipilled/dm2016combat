"""Regenerates the "Open" table of UNVERIFIED.md from the INTERIM markers in the code.

Every stand-in in the code carries an `INTERIM` comment (see COORDINATION.md rule 6); this lists them with their
owner so nothing unverified hides. Usage (from the repo root):
  py -I tools/unverified.py          # rewrite the table between the markers in UNVERIFIED.md
  py -I tools/unverified.py --check  # exit 1 if UNVERIFIED.md is out of date
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BEGIN, END = "<!-- open:begin (tools/unverified.py) -->", "<!-- open:end -->"

# Path prefix -> owner session (COORDINATION.md); first match wins, longest prefixes first.
OWNERS = [
    ("crates/rancher_sim/src/weapons", "weapons-re"), ("crates/rancher/src/combat.rs", "weapons-re"),
    ("crates/rancher_sim/src/collision.rs", "movement-re"), ("crates/rancher_sim/src/physics.rs", "movement-re"),
    ("crates/rancher_sim/src/player.rs", "movement-re"), ("crates/rancher_sim/src/config.rs", "movement-re"),
    ("crates/rancher_sim/src/cmd.rs", "movement-re"), ("crates/rancher_sim/src/install.rs", "movement-re"),
    ("crates/rancher_sim/src/handlayers", "handlayers-re"), ("crates/rancher_sim/src/viewfx.rs", "handlayers-re"),
    ("crates/rancher_sim/src/demons", "demons"), ("crates/rancher/src/demons.rs", "demons"),
    ("crates/idswf", "hud-swf"), ("crates/rancher/src/swf_", "hud-swf"),
    ("crates/idfx", "fx-re"), ("crates/rancher/src/fx.rs", "fx-re"),
    ("crates/idres/src/bmodel.rs", "map-re"), ("crates/idres/src/bcm.rs", "map-re"),
    ("crates/idres/src/entities.rs", "map-re"), ("crates/rancher/src/map", "map-re"),
    ("crates/idres/src/vt", "hdp-exact"), ("crates/idres/src/bimage.rs", "hdp-exact"), ("crates/jxr_sys", "hdp-exact"),
    ("crates/rancher/src/vtmat", "hdp-exact"), ("crates/rancher/src/post", "hdp-exact"), ("crates/rancher/examples", "hdp-exact"),
    ("crates/idaudio", "audio"), ("crates/rancher/src/sound.rs", "audio"),
]


def owner(path):
    for prefix, who in OWNERS:
        if path.startswith(prefix):
            return who
    return "main"


def comment_text(lines, i):
    """The INTERIM comment from line i on: the marker's line plus the comment lines that continue it."""
    def strip(l):
        return re.sub(r"^\s*(///?!?|//|\*)\s?", "", l).strip()
    first = lines[i]
    if not re.match(r"^\s*//", first):  # a trailing comment after code
        return first[first.index("//") + 2:].strip() if "//" in first else first.strip()
    text = strip(first)
    # The marker mid-way through a comment: start at the comment's first line.
    j = i
    while j > 0 and j > i - 4 and re.match(r"^\s*//", lines[j - 1]) and not strip(lines[j - 1]).endswith((".", ":")):
        j -= 1
        text = strip(lines[j]) + " " + text
    for l in lines[i + 1:i + 4]:
        if not re.match(r"^\s*//", l) or "INTERIM" in l:
            break
        s = strip(l)
        if not s or s.startswith("#"):
            break
        text += " " + s
    return text


def rows():
    out = []
    for base in ("crates",):
        for dirpath, _, files in os.walk(os.path.join(ROOT, base)):
            if "target" in dirpath.split(os.sep):
                continue
            for f in sorted(files):
                if not f.endswith((".rs", ".wgsl")):
                    continue
                full = os.path.join(dirpath, f)
                rel = os.path.relpath(full, ROOT).replace(os.sep, "/")
                with open(full, encoding="utf8", errors="replace") as fh:
                    lines = fh.read().splitlines()
                for i, l in enumerate(lines):
                    if "INTERIM" in l:
                        t = comment_text(lines, i).replace("|", "\\|")
                        if len(t) > 400:
                            t = t[:397] + "..."
                        out.append(f"| `{rel}:{i + 1}` | {t} | {owner(rel)} |")
    return sorted(out, key=lambda r: (r.rsplit("|", 2)[-2], r))


def main():
    path = os.path.join(ROOT, "UNVERIFIED.md")
    with open(path, encoding="utf8") as fh:
        doc = fh.read()
    table = "\n".join([BEGIN, "| Where | Stand-in (the INTERIM comment) | Owner |", "|---|---|---|", *rows(), END])
    new = re.sub(re.escape(BEGIN) + r".*?" + re.escape(END), lambda _: table, doc, flags=re.S)
    if "--check" in sys.argv[1:]:
        if new != doc:
            print("UNVERIFIED.md is out of date: run py -I tools/unverified.py", file=sys.stderr)
            return 1
        return 0
    with open(path, "w", encoding="utf8", newline="\n") as fh:
        fh.write(new)
    print(f"{len(rows())} open items")
    return 0


if __name__ == "__main__":
    sys.exit(main())
