"""animweb_insn.py <exe> <lo> <hi> <regex> : instructions (mnemonic + ops) matching regex inside .pdata functions in [lo,hi)."""
import sys, re, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
lo, hi, rx = int(sys.argv[2], 16), int(sys.argv[3], 16), re.compile(sys.argv[4])
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
for b, e in sorted({(x.struct.BeginAddress, x.struct.EndAddress) for x in pe.DIRECTORY_ENTRY_EXCEPTION}):
    if not lo <= base + b < hi: continue
    for i in md.disasm(pe.get_data(b, e - b), base + b):
        s = f"{i.mnemonic} {i.op_str}"
        if rx.search(s): print(f"{base+b:#x} {i.address:#x}: {s}")
