"""Primary function owning each VA (follows .pdata chain info) in the user's own DOOM exe: owner.py <exe> <va>..."""
import sys, bisect, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base, data = pe.OPTIONAL_HEADER.ImageBase, pe.__data__
rs = sorted((e.struct.BeginAddress, e.struct.EndAddress, e.struct.UnwindData) for e in pe.DIRECTORY_ENTRY_EXCEPTION)
starts = [r[0] for r in rs]
def primary(u, b):
    for _ in range(8):
        o = pe.get_offset_from_rva(u)
        if not (data[o] >> 3) & 0x4:
            return b
        co = o + 4 + ((data[o + 2] + 1) & ~1) * 2
        b, _, u = (int.from_bytes(data[co + k:co + k + 4], "little") for k in (0, 4, 8))
    return b
for a in sys.argv[2:]:
    rva = int(a, 16) - base
    k = bisect.bisect_right(starts, rva) - 1
    b, e, u = rs[k]
    print(f"{a} -> chunk {base+b:#x}..{base+e:#x} primary {base + primary(u, b):#x}")
