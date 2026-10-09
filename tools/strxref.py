"""Who references a string in the user's own DOOM exe? strxref.py <exe> <exact string> [...]
Prints code sites (rip-relative, with their primary .pdata function) and data sites (absolute pointers)."""
import re, struct, sys, bisect
import numpy as np
import pefile

exe = sys.argv[1]
pe = pefile.PE(exe, fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
data = pe.__data__
ranges = []
for e in pe.DIRECTORY_ENTRY_EXCEPTION:
    b, en, u = e.struct.BeginAddress, e.struct.EndAddress, e.struct.UnwindData
    primary = b
    for _ in range(8):
        o = pe.get_offset_from_rva(u)
        flags, ncodes = data[o] >> 3, data[o + 2]
        if not flags & 0x4:
            break
        co = o + 4 + ((ncodes + 1) & ~1) * 2
        primary, _, u = (int.from_bytes(data[co + k:co + k + 4], "little") for k in (0, 4, 8))
    ranges.append((b, en, primary))
ranges.sort()
starts = [r[0] for r in ranges]
text = next(s for s in pe.sections if s.Name.startswith(b".text"))
tva = text.VirtualAddress
code = bytes(data[text.PointerToRawData:text.PointerToRawData + text.SizeOfRawData])
arr = np.frombuffer(code, dtype=np.uint8).astype(np.int64)
d = arr[:-3] | (arr[1:-2] << 8) | (arr[2:-1] << 16) | (arr[3:] << 24)
d = np.where(d >= 2**31, d - 2**32, d)
pos = np.arange(len(d), dtype=np.int64)

for s in sys.argv[2:]:
    needle = b"\0" + s.encode() + b"\0"
    i = data.find(needle)
    while i != -1:
        rva = pe.get_rva_from_offset(i + 1)
        print(f"== '{s}' at {base + rva:#x}")
        tgt = tva + pos + 4 + d
        for c in np.nonzero(tgt == rva)[0].tolist():
            if c >= 3 and code[c - 3:c - 1] in (b"\x48\x8d", b"\x4c\x8d"):
                k = bisect.bisect_right(starts, tva + c) - 1
                fn = base + ranges[k][2] if k >= 0 and ranges[k][0] <= tva + c < ranges[k][1] else 0
                print(f"   code {base + tva + c - 3:#x} in func {fn:#x}")
        for m in re.finditer(re.escape(struct.pack("<Q", base + rva)), data):
            print(f"   data ptr at {base + pe.get_rva_from_offset(m.start()):#x}")
        i = data.find(needle, i + 1)
