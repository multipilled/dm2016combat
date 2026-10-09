"""Dump reflection variable lists from the user's own DOOM exe: typeinfo.py <exe> <className>...
Finds the class name string, its type-info record (name ptr followed by super name, size, vars ptr) and prints vars."""
import sys, struct, re, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
def q(va):
    return struct.unpack_from("<Q", d, pe.get_offset_from_rva(va - base))[0]
def s(va):
    if not (base <= va < base + 0x6000000): return None
    try:
        o = pe.get_offset_from_rva(va - base)
    except Exception:
        return None
    b = d[o:o + 200].split(b"\0")[0]
    return b.decode() if b and all(32 <= c < 127 for c in b) else None
for cls in sys.argv[2:]:
    i = d.find(b"\0" + cls.encode() + b"\0")
    name_va = base + pe.get_rva_from_offset(i + 1)
    for m in re.finditer(re.escape(struct.pack("<Q", name_va)), d):
        rec = base + pe.get_rva_from_offset(m.start())
        sup, size, _, vars_ = q(rec + 8), q(rec + 16), q(rec + 24), q(rec + 32)
        if not (size and size < 0x100000 and base < vars_ < base + 0x6000000):
            continue
        print(f"== {cls} : {s(sup)} size {size:#x} vars @ {vars_:#x}")
        v = vars_
        while True:
            t, nm, offsz, comment = q(v), q(v + 16), q(v + 24), q(v + 40)
            if t == 0 or s(t) is None:
                break
            print(f"   +{offsz & 0xffffffff:#06x} [{offsz >> 32:#x}] {s(t):24} {s(nm)}   // {s(comment) or ''}")
            v += 0x48
