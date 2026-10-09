"""animweb_fn.py <file.c> <hexva>... : print the decompiled function(s) at VA from a DecompileList output file."""
import sys, re
txt = open(sys.argv[1], encoding="utf-8", errors="replace").read()
parts = re.split(r"(?m)^// ==== ", txt)
idx = {}
for p in parts[1:]:
    m = re.match(r"\S+ @ ([0-9a-f]+)", p)
    if m: idx[int(m.group(1), 16)] = "// ==== " + p
for a in sys.argv[2:]:
    print(idx.get(int(a, 16), f"// {a} not in file"))
