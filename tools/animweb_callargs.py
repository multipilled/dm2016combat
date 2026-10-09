"""animweb_callargs.py <exe> <target> [n] : for each direct call to target, print the stack-arg stores [rsp+0x20..0x48] and edx/r8d/r9d immediates in the ~20 instructions before it."""
import sys, re, numpy as np, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
t = next(s for s in pe.sections if s.Name.startswith(b".text"))
tva = t.VirtualAddress
raw = bytes(pe.__data__[t.PointerToRawData:t.PointerToRawData + t.SizeOfRawData])
code = np.frombuffer(raw, dtype=np.uint8).astype(np.int64)
target = int(sys.argv[2], 16) - base
rel = code[1:-3] | (code[2:-2] << 8) | (code[3:-1] << 16) | (code[4:] << 24)
rel = np.where(rel >= 2**31, rel - 2**32, rel)
pos = np.arange(len(rel), dtype=np.int64)
hits = np.nonzero((code[:-4] == 0xE8) & (tva + pos + 5 + rel == target))[0]
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
n = int(sys.argv[3]) if len(sys.argv) > 3 else 9999
for h in hits[:n]:
    start = max(0, h - 0x60)
    ins = list(md.disasm(raw[start:h + 5], base + tva + start))
    ins = [i for i in ins if i.address <= base + tva + h][-22:]
    args = [f"{i.mnemonic} {i.op_str}" for i in ins if re.search(r"rsp \+ 0x(2|3|4)[0-9a-f]\], |^(edx|r8d|r9d|r8w|r9w|dx), ", i.op_str)]
    print(f"{base+tva+h:#x}: " + " | ".join(args))
