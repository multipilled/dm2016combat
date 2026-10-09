"""Functions in the user's own DOOM exe that read given cvars (rip-relative into the cvar object).
usage: cvar_users.py <exe> <cvar> [<cvar>...]  -> prints '<func_va> <cvar>' lines (primary .pdata starts)"""
import re, sys, bisect
import numpy as np
import pefile, capstone

exe, names = sys.argv[1], set(sys.argv[2:])
objs = {}
for line in open("gamedata/exe/cvars.tsv", encoding="utf8"):
    p = line.rstrip("\n").split("\t")
    if len(p) >= 3 and p[0] in names:
        objs[p[0]] = int(p[2], 16)

pe = pefile.PE(exe, fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
data = pe.__data__
# map every .pdata range to its primary function start (follow chain info)
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
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)

found = set()
for name, obj in objs.items():
    lo, hi = obj - base, obj - base + 0x50
    cand = set()
    for tail in range(0, 5):
        tgt = tva + pos + 4 + tail + d
        cand.update(np.nonzero((tgt >= lo) & (tgt < hi))[0].tolist())
    for c in cand:
        rva = tva + c
        k = bisect.bisect_right(starts, rva) - 1
        if k < 0 or not ranges[k][0] <= rva < ranges[k][1]:
            continue
        b = ranges[k][0]
        for ins in md.disasm(code[b - tva:ranges[k][1] - tva], base + b):
            if ins.address <= base + rva < ins.address + ins.size:
                m = re.search(r"\[rip ([+-]) (0x[0-9a-f]+)\]", ins.op_str)
                if m:
                    t = ins.address + ins.size + int(m.group(2), 16) * (1 if m.group(1) == "+" else -1)
                    if obj <= t < obj + 0x50 and not ins.mnemonic.startswith("lea"):
                        found.add((base + ranges[k][2], name))
                break
for va, name in sorted(found):
    print(f"{va:#x} {name}")
