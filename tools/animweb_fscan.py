"""animweb_fscan.py <exe> <lo> <hi> <off>[,<off>...] : list .pdata functions in [lo,hi) whose SSE float instructions use ALL given [reg+off] displacements."""
import sys, struct, re, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
lo, hi = int(sys.argv[2], 16), int(sys.argv[3], 16)
offs = [int(x, 16) for x in sys.argv[4].split(",")]
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
fs = sorted({(e.struct.BeginAddress, e.struct.EndAddress) for e in pe.DIRECTORY_ENTRY_EXCEPTION})
for b, e in fs:
    va = base + b
    if not lo <= va < hi: continue
    code = pe.get_data(b, e - b)
    seen = set()
    for i in md.disasm(code, va):
        if not i.mnemonic.endswith("ss") and i.mnemonic not in ("movd",): continue
        for o in offs:
            if re.search(r"\[r\w+ \+ %s\]" % hex(o), i.op_str): seen.add(o)
    if len(seen) == len(offs): print(hex(va), e - b)
