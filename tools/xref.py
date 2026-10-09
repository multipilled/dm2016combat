"""Find code that reads given cvars in the user's own DOOM (2016) exe and disassemble around it.

usage: xref.py <exe> <cvar_name> [<cvar_name>...]   (prints to stdout; nothing is written to the repo)
"""
import re, struct, sys
import numpy as np
import pefile, capstone

exe, names = sys.argv[1], sys.argv[2:]
pe = pefile.PE(exe, fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
data = pe.__data__
text = next(s for s in pe.sections if s.Name.startswith(b".text"))
tva, traw, tsize = text.VirtualAddress, text.PointerToRawData, text.SizeOfRawData
code = bytes(data[traw:traw + tsize])
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
md.detail = False

def rva_to_off(rva):
    return pe.get_offset_from_rva(rva)

def find_str_rva(s):
    needle = b"\0" + s.encode() + b"\0"
    i = data.find(needle)
    out = []
    while i != -1:
        out.append(pe.get_rva_from_offset(i + 1))
        i = data.find(needle, i + 1)
    return out

arr = np.frombuffer(code, dtype=np.uint8)
disp_all = np.frombuffer(code[: len(code) // 4 * 4], dtype="<i4")  # aligned view not enough; build per offset
pos = np.arange(len(code) - 4, dtype=np.int64)
disps = (arr[:-4].astype(np.int64) | (arr[1:-3].astype(np.int64) << 8) | (arr[2:-2].astype(np.int64) << 16) | (arr[3:-1].astype(np.int64) << 24))
disps = np.where(disps >= 2**31, disps - 2**32, disps)

def refs_to(lo, hi):
    hits = set()
    for tail in range(0, 5):
        tgt = tva + pos + 4 + tail + disps
        m = np.nonzero((tgt >= lo) & (tgt < hi))[0]
        hits.update(int(x) for x in m)
    return sorted(hits)

def real_ref(at, lo, hi):
    for ins in dis(at, before=0x40, after=0x10):
        if ins.address <= base + at < ins.address + ins.size and "rip" in ins.op_str:
            m = re.search(r"rip ([+-]) (0x[0-9a-f]+|\d+)", ins.op_str)
            if m:
                d = int(m.group(2), 0) * (1 if m.group(1) == "+" else -1)
                t = ins.address + ins.size + d - base
                return lo <= t < hi and not ins.mnemonic.startswith("lea")
    return False

def dis(at_rva, before=0x60, after=0x120):
    start = max(0, at_rva - tva - before)
    chunk = code[start:at_rva - tva + after]
    # resync: try start offsets until we decode through at_rva
    for skew in range(0, 16):
        ins = list(md.disasm(chunk[skew:], base + tva + start + skew))
        if any(i.address == base + at_rva for i in ins) or any(i.address <= base + at_rva < i.address + i.size for i in ins):
            return ins
    return list(md.disasm(chunk, base + tva + start))

for name in names:
    for srva in find_str_rva(name):
        # static init: lea rdx,[name] (48 8D 15) then lea rcx,[obj] (48 8D 0D) nearby
        objs = set()
        for m in re.finditer(rb"\x48\x8d\x15", code):
            i = m.start()
            if tva + i + 7 + struct.unpack_from("<i", code, i + 3)[0] != srva:
                continue
            for j in range(max(0, i - 64), i + 64):
                if code[j:j + 3] == b"\x48\x8d\x0d":
                    objs.add(tva + j + 7 + struct.unpack_from("<i", code, j + 3)[0])
        for obj in sorted(objs):
            sites = [x for x in refs_to(obj, obj + 0x40) if real_ref(tva + x, obj, obj + 0x40)]
            print(f"=== {name}: obj rva {obj:#x}, {len(sites)} candidate refs")
            for s in sites[:12]:
                at = tva + s
                ins = dis(at)
                print(f"--- ref near {base + at:#x}")
                for i in ins:
                    mark = ">>" if i.address <= base + at < i.address + i.size else "  "
                    print(f"{mark} {i.address:#x}: {i.mnemonic} {i.op_str}")
