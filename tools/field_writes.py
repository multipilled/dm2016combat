"""List instructions in the user's DOOM exe that touch [reg+disp] for given field offsets: field_writes.py <exe> <off> [<off>...]"""
import re, struct, sys
import pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
text = next(s for s in pe.sections if s.Name.startswith(b".text"))
code = bytes(pe.__data__[text.PointerToRawData:text.PointerToRawData + text.SizeOfRawData])
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
def rd(va, fmt):
    return struct.unpack_from(fmt, pe.__data__, pe.get_offset_from_rva(va - base))[0]
for off in (int(a, 16) for a in sys.argv[2:]):
    needle = struct.pack("<I", off)
    seen = set()
    for m in re.finditer(re.escape(needle), code):
        p = m.start()
        for back in range(2, 8):  # disp32 sits 2..7 bytes into the instruction
            s = p - back
            ins = next(md.disasm(code[s:s + 15], base + text.VirtualAddress + s), None)
            if ins and ins.address not in seen and f"+ {off:#x}]" in ins.op_str and s + ins.size > p + 3:
                seen.add(ins.address)
                note = ""
                if ins.op_str.startswith("dword ptr [") and ins.mnemonic in ("movss", "mov") and "," in ins.op_str:
                    src = ins.op_str.split(",", 1)[1].strip()
                    if src.startswith("0x"):
                        v = int(src, 16)
                        note = f"  ; imm = {struct.unpack('<f', struct.pack('<I', v & 0xffffffff))[0]:g}f"
                print(f"{off:#x}  {ins.address:#x}: {ins.mnemonic} {ins.op_str}{note}")
                break
