"""animweb_evflags.py <exe> <name>... : for each anim event def name, find its static-init registration (lea r8,"name") and
print the r9d immediate (eventFlags_t: 1 CANSKIP, 2 CANSUPPRESS, 4 CLIENTSAFE, 8 NOSCRIPT, 16 PASSIVEMARKER)."""
import sys, re, struct, numpy as np, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
t = next(s for s in pe.sections if s.Name.startswith(b".text"))
tva = t.VirtualAddress
raw = bytes(d[t.PointerToRawData:t.PointerToRawData + t.SizeOfRawData])
code = np.frombuffer(raw, dtype=np.uint8).astype(np.int64)
rel = code[:-3] | (code[1:-2] << 8) | (code[2:-1] << 16) | (code[3:] << 24)
rel = np.where(rel >= 2**31, rel - 2**32, rel)
pos = np.arange(len(rel), dtype=np.int64)
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
for name in sys.argv[2:]:
    i = d.find(b"\0" + name.encode() + b"\0")
    if i < 0: print(f"{name:40} ?"); continue
    sva = base + pe.get_rva_from_offset(i + 1)
    hits = np.nonzero(tva + pos + 4 + rel == sva - base)[0]
    flags = []
    for h in hits:
        st = h - 3  # lea r8, [rip+x] = 4c 8d 05 disp32
        if raw[st:st + 3] != b"\x4c\x8d\x05": continue
        for ins in md.disasm(raw[st:st + 0x60], base + tva + st):
            m = re.match(r"r9d, (0x[0-9a-f]+|\d+)$", ins.op_str)
            if ins.mnemonic == "mov" and m: flags.append(int(m.group(1), 0)); break
            if ins.mnemonic == "xor" and ins.op_str == "r9d, r9d": flags.append(0); break
            if ins.mnemonic == "call": break
    print(f"{name:40} {flags}")
