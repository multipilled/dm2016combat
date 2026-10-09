"""mods_enum.py <exe> <valueName>... : print the reflection enum table containing <valueName>.
Like gamedata/re/aw/enum2.py, but checks every pointer to the name and only accepts runs of 16-byte
{char* name, int64 value} entries with small values (skips the generic constants tables)."""
import struct, sys, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
def s(va):
    if not (base <= va < base + 0x6000000): return None
    try: o = pe.get_offset_from_rva(va - base)
    except Exception: return None
    b = d[o:o + 120].split(b"\0")[0]
    return b.decode() if b and all(32 <= c < 127 for c in b) else None
def ok(o):
    p, v = struct.unpack_from("<Qq", d, o)
    n = s(p)
    return n if n and n.replace("_", "").isalnum() and -100000 < v < 100000 else None
for nm in sys.argv[2:]:
    i = d.find(b"\0" + nm.encode() + b"\0")
    if i < 0: print("no str", nm); continue
    sva = base + pe.get_rva_from_offset(i + 1)
    m = d.find(struct.pack("<Q", sva)); done = False
    while m >= 0 and not done:
        if ok(m) == nm:
            st = m
            while st >= 16 and ok(st - 16): st -= 16
            en = m
            while ok(en + 16): en += 16
            if en > st:
                print(f"== {nm}: table @ {base + pe.get_rva_from_offset(st):#x}")
                for o in range(st, en + 16, 16):
                    p, v = struct.unpack_from("<Qq", d, o); print(f"   {v:6d}  {s(p)}")
                done = True
        m = d.find(struct.pack("<Q", sva), m + 1)
    if not done: print("no table", nm)
