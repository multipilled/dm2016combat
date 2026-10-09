"""animweb_vtbl.py <exe> <ClassName> [n] : RTTI '.?AV<Class>@@' -> complete object locator(s) -> vftable; prints first n slots.
Notes-only helper for the anim web RE (gamedata/re/ANIMWEB.md)."""
import sys, struct, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
n = int(sys.argv[3]) if len(sys.argv) > 3 else 80
def rva_of_off(o): return pe.get_rva_from_offset(o)
def off(rva): return pe.get_offset_from_rva(rva)
name = b".?AV" + sys.argv[2].encode() + b"@@\0"
i = d.find(name)
if i < 0: sys.exit("no RTTI name")
td_rva = rva_of_off(i) - 0x10          # TypeDescriptor: vfptr, spare, name
# COL: sig(4)=1, offset, cdOffset, pTypeDescriptor(rva), pClassHierarchy(rva), pSelf(rva)
pat = struct.pack("<I", td_rva)
j = d.find(pat)
while j >= 0:
    col_off = j - 12
    sig, offs, cdo = struct.unpack_from("<III", d, col_off)
    if sig == 1 and struct.unpack_from("<I", d, col_off + 20)[0] == rva_of_off(col_off):
        col_va = base + rva_of_off(col_off)
        k = d.find(struct.pack("<Q", col_va))
        while k >= 0:
            vt = base + rva_of_off(k) + 8
            print(f"== {sys.argv[2]} COL {col_va:#x} (this-offset {offs:#x}) vftable {vt:#x}")
            for s in range(n):
                f = struct.unpack_from("<Q", d, k + 8 + 8 * s)[0]
                if not (base + 0x1000 <= f < base + 0x2000000): break
                print(f"   [{s:3d}] +{8*s:#05x} {f:#x}")
            k = d.find(struct.pack("<Q", col_va), k + 8)
    j = d.find(pat, j + 4)
