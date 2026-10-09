"""Which reflected class/struct declares a field? varowner.py <exe> <fieldName>...
Finds every reflection var record naming the field, walks back to the start of its var list and prints the owning
type-info record (class name, super, size) with the field's offset, type and comment. Read-only on the user's own exe."""
import sys, struct, re, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
def off(va): return pe.get_offset_from_rva(va - base)
def q(va): return struct.unpack_from("<Q", d, off(va))[0]
def s(va):
    if not (base <= va < base + 0x6000000): return None
    try: o = off(va)
    except Exception: return None
    b = d[o:o + 200].split(b"\0")[0]
    return b.decode() if b and all(32 <= c < 127 for c in b) else None
def valid(rec):
    try: return s(q(rec)) is not None and s(q(rec + 16)) is not None
    except Exception: return False
owners = {}
for name in sys.argv[2:]:
    for m in re.finditer(b"\0" + re.escape(name.encode()) + b"\0", d):
        sva = base + pe.get_rva_from_offset(m.start() + 1)
        for r in re.finditer(re.escape(struct.pack("<Q", sva)), d):
            rec = base + pe.get_rva_from_offset(r.start()) - 16
            if not valid(rec): continue
            start = rec
            while valid(start - 0x48): start -= 0x48
            if start not in owners:
                owners[start] = None
                pat = re.escape(struct.pack("<Q", start))
                for c in re.finditer(pat, d):
                    crec = base + pe.get_rva_from_offset(c.start()) - 32
                    try:
                        nm, sup, size = s(q(crec)), s(q(crec + 8)), q(crec + 16)
                    except Exception: continue
                    if nm: owners[start] = (nm, sup, size); break
            o = owners[start]
            offsz = q(rec + 24)
            print(f"{name}: {o[0] if o else '?'} : {o[1] if o else ''} size {o[2] if o else 0:#x}  +{offsz & 0xffffffff:#x} {s(q(rec))}  // {s(q(rec + 40)) or ''}")
