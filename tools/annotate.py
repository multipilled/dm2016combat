"""Make Ghidra output from the user's own DOOM exe readable (local RE notes only, never committed):
- global reads that land inside a cvar object become CV_<name>.f / .i
- `this` field accesses become this->name (from tools/fields_*.tsv) or this->f_<byteoffset>
usage: annotate.py <in.c> <out.c> [fields.tsv]"""
import re, sys, bisect

cv = []
for line in open("gamedata/exe/cvars.tsv", encoding="utf8"):
    parts = line.rstrip("\n").split("\t")
    if len(parts) >= 3 and parts[2] != "0x0":
        cv.append((int(parts[2], 16), parts[0], parts[1]))
cv.sort()
keys = [c[0] for c in cv]

fields = {}
if len(sys.argv) > 3:
    for line in open(sys.argv[3], encoding="utf8"):
        if line.strip() and not line.startswith("#"):
            off, name = line.split()[:2]
            fields[int(off, 16)] = name

def cvar_name(va):
    k = bisect.bisect_right(keys, va) - 1
    if k >= 0 and va - keys[k] < 0x60:
        off = va - keys[k]
        return f"CV_{cv[k][1]}." + {0x34: "f", 0x30: "i"}.get(off, f"_{off:#x}")
    return None

def globals_(text):
    def rep(m):
        return cvar_name(int(m.group(2), 16)) or m.group(0)
    return re.sub(r"\b(_?DAT_|[a-z]{1,3}Ram0*)([0-9a-f]{9})\b", rep, text)

def field(off):
    return "this->" + fields.get(off, f"f_{off:x}")

def this_fields(block):
    sig = re.search(r"\(\s*longlong \*param_1", block)
    if not sig:
        return block
    # *(T *)((longlong)param_1 + 0xN)  -> this->f (byte offset N)
    block = re.sub(r"\*\(([\w ]+?) \*\)\(\(longlong\)param_1 \+ (0x[0-9a-f]+|\d+)\)",
                   lambda m: f"({field(int(m.group(2), 0))} /*{m.group(1)}*/)", block)
    # *(T *)(param_1 + 0xN) where param_1 is longlong* -> byte offset 8N
    block = re.sub(r"\*\(([\w ]+?) \*\)\(param_1 \+ (0x[0-9a-f]+|\d+)\)",
                   lambda m: f"({field(int(m.group(2), 0) * 8)} /*{m.group(1)}*/)", block)
    block = re.sub(r"\bparam_1\[(0x[0-9a-f]+|\d+)\]", lambda m: f"{field(int(m.group(1), 0) * 8)}", block)
    return block

src = open(sys.argv[1], encoding="utf8", errors="replace").read()
blocks = re.split(r"(?=// ==== )", src)
out = []
for b in blocks:
    b = this_fields(globals_(b))
    out.append(b)
    head = b.split("\n", 1)[0]
    used = sorted(set(re.findall(r"CV_([A-Za-z0-9_]+)\.", b)))
    if head.startswith("// ==== ") and used:
        print(head[8:].split(" @")[0], f"[{len(b)}b]", " ".join(used))
open(sys.argv[2], "w", encoding="utf8").write("".join(out))
