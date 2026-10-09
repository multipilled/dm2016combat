"""Survey the blend spaces (blend1 / blendy1 / blenda) of the extracted monster anim webs.

usage: python tools/animweb_blendspaces.py [decl dir]   (default gamedata/raw/generated/decls/animweb/zion)
Prints, per tree whose blendEq uses a blend space: web, node, blendEq, alias coordinates (sorted, with the
'y' mirror rule of 0x1415d39f0 applied) so asymmetric yaw spaces and duplicate coordinates stand out.
"""
import os
import re
import sys


def parse(text):
    out = []
    node = None
    cur = None
    for m in re.finditer(r'node "([^"]+)"|blendEq "([^"]*)"|(alias \{)|coordinate \(([^)]*)\)', text):
        if m.group(1):
            node = m.group(1)
        elif m.group(2) is not None:
            cur = [node, m.group(2), []]
            out.append(cur)
        elif m.group(3):
            if cur is not None:
                cur[2].append(None)
        elif m.group(4) is not None and cur is not None and cur[2]:
            cur[2][-1] = [float(x) for x in m.group(4).split()]
    return out


def mirror(cs):
    """0x1415d39f0 with the 'y' flag: mirror the larger-|c| end while an end sits at max |c|."""
    s = sorted(cs)
    if len(s) < 2:
        return s
    mx = max(abs(s[0]), abs(s[-1]))
    extra = []
    i, j = 0, len(s) - 1
    while i < j:
        a, b = abs(s[i]), abs(s[j])
        if a != mx and b != mx:
            break
        if a != b:
            extra.append(-(s[j] if a < b else s[i]))
        i += 1
        j -= 1
    return sorted(s + extra)


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "gamedata/raw/generated/decls/animweb/zion"
    for dp, _, fs in os.walk(root):
        for f in sorted(fs):
            if not f.endswith(".decl"):
                continue
            text = open(os.path.join(dp, f), encoding="utf-8", errors="replace").read()
            for node, eq, al in parse(text):
                if not re.search(r"(?i)blend(y?1|a)\(", eq.replace(" ", "")):
                    continue
                cs = [c[0] if c else None for c in al]
                flat = [c for c in cs if c is not None]
                y = "Y" if re.search(r"(?i)blendy1", eq) else " "
                m = mirror(flat) if y == "Y" else sorted(flat)
                dup = "DUP" if len(set(flat)) != len(flat) else "   "
                asym = "ASYM" if y == "Y" and len(m) != len(flat) else "    "
                print(f"{f[:-5]:18} {node:28} {y} {dup} {asym} n={len(al):2} {cs} | {eq[:110]}")


if __name__ == "__main__":
    main()
