"""Every decodable instruction (anywhere in .text, including leaf functions) touching [reg+disp] for given offsets.
rawrefs.py <exe> <hexoff>[,<hexoff>] [--writes]"""
import re, struct, sys, pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
t = next(s for s in pe.sections if s.Name.startswith(b".text"))
code = bytes(pe.__data__[t.PointerToRawData:t.PointerToRawData + t.SizeOfRawData])
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
writes = "--writes" in sys.argv
for off in (int(x, 16) for x in sys.argv[2].split(",")):
    seen = set()
    for m in re.finditer(re.escape(struct.pack("<I", off)), code):
        p = m.start()
        for back in range(2, 9):
            s = p - back
            ins = next(md.disasm(code[s:s + 16], base + t.VirtualAddress + s), None)
            if not ins or ins.address in seen or not (s + ins.size >= p + 4):
                continue
            if f"+ {off:#x}]" not in ins.op_str:
                continue
            seen.add(ins.address)
            dst = ins.op_str.split(",")[0]
            if writes and f"+ {off:#x}]" not in dst:
                break
            print(f"{off:#x} {ins.address:#x}: {ins.mnemonic} {ins.op_str}")
            break
