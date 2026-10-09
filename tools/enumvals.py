"""Enum value tables in the user's own DOOM exe: enumvals.py <exe> <oneValueName>...
Reflection enum constants are stored as 24-byte {char* type ("int"), char* name, char* value} records (the MAPS.md
"int NAME VALUE" triples). Prints the run of records sharing the given value's name prefix."""
import sys, struct, re, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
def off(va): return pe.get_offset_from_rva(va - base)
def s(va):
    if not (base <= va < base + 0x6000000): return None
    try: o = off(va)
    except Exception: return None
    b = d[o:o + 120].split(b"\0")[0]
    return b.decode() if b and all(32 <= c < 127 for c in b) else None
def rec(p):
    t, n, v = struct.unpack_from("<QQQ", d, p)
    return s(t), s(n), s(v)
for nm in sys.argv[2:]:
    i = d.find(b"\0" + nm.encode() + b"\0")
    if i < 0: print("no string", nm); continue
    sva = base + pe.get_rva_from_offset(i + 1)
    pre = nm.split("_")[0] + "_"
    for m in re.finditer(re.escape(struct.pack("<Q", sva)), d):
        o = m.start() - 8
        t, n, v = rec(o)
        if t != "int" or n != nm: continue
        st = o
        while True:
            t, n, v = rec(st - 24)
            if t != "int" or not n or not n.startswith(pre): break
            st -= 24
        print(f"== {nm}: table @ {base + pe.get_rva_from_offset(st):#x}")
        p = st
        while True:
            t, n, v = rec(p)
            if t != "int" or not n or not n.startswith(pre): break
            print(f"   {v:>12}  {n}")
            p += 24
