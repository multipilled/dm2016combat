"""Explore a .bmd6anim from the user's install: animexp.py <file>"""
import struct, sys
d = open(sys.argv[1], "rb").read()
o = 12; l = struct.unpack("<I", d[o:o+4])[0]; skel = d[o+4:o+4+l].decode(); o += 4 + l + 24
S = o + 4
be16 = lambda a: struct.unpack(">H", d[a:a+2])[0]
names = ["size","flags","numFrames","frameRate","numFrameSets","frameSetTbl","frameSetOfsTbl","constR","constS","constT","constU","nextSize","jointWeights"]
h = {n: be16(S + 12 + 2*i) for i, n in enumerate(names)}
h["total"] = struct.unpack(">I", d[S+8:S+12])[0]
print(skel); print(h)
print("S+0x90..constR:", d[S+0x90:S+h["constR"]].hex(" "))
for k, a, b in [("constR", "constR", "constS"), ("constS", "constS", "constT"), ("constT", "constT", "constU"), ("constU", "constU", "frameSetTbl"), ("fsTbl", "frameSetTbl", "frameSetOfsTbl"), ("fsOfsTbl", "frameSetOfsTbl", "size")]:
    seg = d[S+h[a]:S+h[b]]
    print(f"{k} [{len(seg)}]:", seg.hex(" ")[:600])
base = S + h["size"]
for fs in range(h["numFrameSets"]):
    hdr = struct.unpack(">19H", d[base:base+38])
    print(f"frameset {fs} @ {base - S:#x}: first R{hdr[0]:#x} S{hdr[1]:#x} T{hdr[2]:#x} U{hdr[3]:#x} range R{hdr[4]:#x} S{hdr[5]:#x} T{hdr[6]:#x} U{hdr[7]:#x} bits R{hdr[8]:#x} S{hdr[9]:#x} T{hdr[10]:#x} U{hdr[11]:#x} next R{hdr[12]:#x} S{hdr[13]:#x} T{hdr[14]:#x} U{hdr[15]:#x} total {hdr[16]:#x} start {hdr[17]} range {hdr[18]}")
    body = d[base+0x30: base + hdr[16]]
    print("   body:", body.hex(" ")[:900])
    base += hdr[16]
print("remaining:", d[base:].hex(" ")[:300])
