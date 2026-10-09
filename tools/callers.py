"""Direct call sites (E8 rel32 / E9 jmp) of a target VA in the user's own DOOM exe, with owning function: callers.py <exe> <va>"""
import sys, bisect, numpy as np, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base, data = pe.OPTIONAL_HEADER.ImageBase, pe.__data__
t = next(s for s in pe.sections if s.Name.startswith(b".text"))
tva = t.VirtualAddress
code = np.frombuffer(bytes(data[t.PointerToRawData:t.PointerToRawData + t.SizeOfRawData]), dtype=np.uint8)
target = int(sys.argv[2], 16) - base
c = code.astype(np.int64)
rel = c[1:-3] | (c[2:-2] << 8) | (c[3:-1] << 16) | (c[4:] << 24)
rel = np.where(rel >= 2**31, rel - 2**32, rel)
pos = np.arange(len(rel), dtype=np.int64)
hits = np.nonzero(((code[:-4] == 0xE8) | (code[:-4] == 0xE9)) & (tva + pos + 5 + rel == target))[0]
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
for h in hits.tolist():
    rva = tva + h
    k = bisect.bisect_right(starts, rva) - 1
    owner = base + primary(rs[k][2], rs[k][0]) if k >= 0 and rs[k][0] <= rva < rs[k][1] else 0
    print(f"{'call' if code[h] == 0xE8 else 'jmp '} at {base + rva:#x} in {owner:#x}")
