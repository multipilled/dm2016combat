"""animweb_fscan2.py <exe> <lo> <hi> <floatOffs> <anyOffs> : functions using float [reg+o] for all floatOffs and any instr on [reg+o] for all anyOffs."""
import sys, re, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
lo, hi = int(sys.argv[2], 16), int(sys.argv[3], 16)
fo = [int(x, 16) for x in sys.argv[4].split(",")]
ao = [int(x, 16) for x in sys.argv[5].split(",")]
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
for b, e in sorted({(x.struct.BeginAddress, x.struct.EndAddress) for x in pe.DIRECTORY_ENTRY_EXCEPTION}):
    if not lo <= base + b < hi: continue
    sf, sa = set(), set()
    for i in md.disasm(pe.get_data(b, e - b), base + b):
        for o in fo:
            if i.mnemonic.endswith("ss") and re.search(r"\[r\w+ \+ %s\]" % hex(o), i.op_str): sf.add(o)
        for o in ao:
            if not i.mnemonic.endswith("ss") and re.search(r"\[r\w+ \+ %s\]" % hex(o), i.op_str): sa.add(o)
    if len(sf) == len(fo) and len(sa) == len(ao): print(hex(base + b), e - b)
