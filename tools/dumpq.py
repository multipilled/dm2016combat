"""Dump qwords around a VA of the user's own DOOM exe, resolving pointers to strings: dumpq.py <exe> <va> <count>"""
import sys, struct, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
va, n = int(sys.argv[2], 16), int(sys.argv[3])
def s_at(p):
    try:
        o = pe.get_offset_from_rva(p - base)
        s = d[o:o + 100].split(b"\0")[0]
        return s.decode() if s and all(32 <= c < 127 for c in s) else None
    except Exception:
        return None
for i in range(n):
    a = va + 8 * i
    q = struct.unpack_from("<Q", d, pe.get_offset_from_rva(a - base))[0]
    lo, hi = q & 0xffffffff, q >> 32
    s = s_at(q) if base <= q < base + 0x6000000 else None
    print(f"{a:#x}: {q:#018x}  " + (f'"{s}"' if s else f"lo={lo:#x}({struct.unpack('<f', struct.pack('<I', lo))[0]:g}) hi={hi:#x}"))
