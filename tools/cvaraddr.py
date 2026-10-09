"""Map global VAs (reads into cvar objects) to cvar names: cvaraddr.py <va>..."""
import sys, bisect
cv = []
for line in open("gamedata/exe/cvars.tsv", encoding="utf8"):
    p = line.rstrip("\n").split("\t")
    if len(p) >= 3 and p[2] != "0x0": cv.append((int(p[2], 16), p[0], p[1]))
cv.sort(); keys = [c[0] for c in cv]
for a in sys.argv[1:]:
    v = int(a, 16); k = bisect.bisect_right(keys, v) - 1
    print(f"{a} = {cv[k][1]} (default {cv[k][2]}) +{v - cv[k][0]:#x}")
