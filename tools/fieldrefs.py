"""Exact field-offset references in the user's own DOOM exe, using .pdata function bounds.
usage: fieldrefs.py <exe> <hexoff>[,<hexoff>...] [--funcs]   prints each referencing instruction and its function start."""
import re, struct, sys, bisect
import pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
funcs = sorted({(e.struct.BeginAddress, e.struct.EndAddress) for e in pe.DIRECTORY_ENTRY_EXCEPTION})
starts = [f[0] for f in funcs]
text = next(s for s in pe.sections if s.Name.startswith(b".text"))
code = bytes(pe.__data__[text.PointerToRawData:text.PointerToRawData + text.SizeOfRawData])
tva = text.VirtualAddress
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
offs = [int(x, 16) for x in sys.argv[2].split(",")]
hits = {}
for off in offs:
    needle = struct.pack("<I", off)
    for m in re.finditer(re.escape(needle), code):
        rva = tva + m.start()
        k = bisect.bisect_right(starts, rva) - 1
        if k >= 0 and funcs[k][0] <= rva < funcs[k][1]:
            hits.setdefault(funcs[k], set()).add(off)
for (b, e), found in sorted(hits.items()):
    body = code[b - tva:e - tva]
    lines = []
    for i in md.disasm(body, base + b):
        for off in found:
            if re.search(rf"\+ {off:#x}\]", i.op_str):
                lines.append(f"    {i.address:#x}: {i.mnemonic} {i.op_str}")
    if lines:
        print(f"func {base + b:#x}..{base + e:#x} ({e - b} bytes)")
        print("\n".join(lines))
