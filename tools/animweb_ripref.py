"""animweb_ripref.py <exe> <va>... : code sites with a rip-relative reference to any of the VAs (exact or +0..7), with owning .pdata function."""
import sys, bisect, numpy as np, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base, data = pe.OPTIONAL_HEADER.ImageBase, pe.__data__
t = next(s for s in pe.sections if s.Name.startswith(b".text"))
tva = t.VirtualAddress
code = np.frombuffer(bytes(data[t.PointerToRawData:t.PointerToRawData + t.SizeOfRawData]), dtype=np.uint8).astype(np.int64)
rel = code[:-3] | (code[1:-2] << 8) | (code[2:-1] << 16) | (code[3:] << 24)
rel = np.where(rel >= 2**31, rel - 2**32, rel)
pos = np.arange(len(rel), dtype=np.int64)
rs = sorted((e.struct.BeginAddress, e.struct.EndAddress) for e in pe.DIRECTORY_ENTRY_EXCEPTION)
st = [r[0] for r in rs]
for a in sys.argv[2:]:
    target = int(a, 16) - base
    # disp32 at pos; instruction end is pos+4+k (k = 0..5 immediate bytes)
    for k in (0, 1, 4):
        hits = np.nonzero(tva + pos + 4 + k + rel == target)[0]
        for h in hits:
            va = base + tva + int(h)
            i = bisect.bisect_right(st, va - base) - 1
            f = base + rs[i][0] if i >= 0 and rs[i][0] <= va - base < rs[i][1] else 0
            print(f"{a}: disp@{va:#x} (+{k}) in func {f:#x}")
