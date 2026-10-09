"""Names of idAtomicString globals in the user's own DOOM exe: atomicnames.py <exe> <va>...
Finds rip-relative references to each global and prints string literals loaded (lea) within the few
instructions around each reference (static initializers do `lea rdx, "name"; lea rcx, [global]; call ctor`)."""
import sys, re, struct, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__
t = next(s for s in pe.sections if s.Name.startswith(b".text"))
code = d[t.PointerToRawData:t.PointerToRawData + t.SizeOfRawData]
tva = base + t.VirtualAddress
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
def s(va):
    try: o = pe.get_offset_from_rva(va - base)
    except Exception: return None
    b = d[o:o + 80].split(b"\0")[0]
    return b.decode() if b and all(32 <= c < 127 for c in b) else None
for arg in sys.argv[2:]:
    tgt = int(arg, 16)
    names = set()
    # rip-relative disp32 at instruction offset p: tgt = tva + p_end + disp
    for p in range(0, len(code) - 8):
        pass
    import numpy as np
    c = np.frombuffer(code, dtype=np.uint8).astype(np.int64)
    disp = c[0:-3] | (c[1:-2] << 8) | (c[2:-1] << 16) | (c[3:] << 24)
    disp = np.where(disp >= 2**31, disp - 2**32, disp)
    pos = np.arange(len(disp))
    for k in (4, 5, 8):  # instruction end = disp pos + 4 (+ imm bytes)
        for p in np.nonzero(tva + pos + 4 + (k - 4) + disp == tgt)[0][:40]:
            st = max(0, p - 40)
            for ins in md.disasm(code[st:p + 40], tva + st):
                if ins.mnemonic == "lea" and "rip" in ins.op_str:
                    m = re.search(r"rip \+ (0x[0-9a-f]+)", ins.op_str)
                    if m:
                        sv = s(ins.address + ins.size + int(m.group(1), 16))
                        if sv: names.add((hex(ins.address), sv))
    print(arg, sorted(names, key=lambda x: x[0])[:12])
