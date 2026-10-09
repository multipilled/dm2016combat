"""Disassemble the user's own DOOM exe at a VA: dis.py <exe> <va> [bytes]. Resolves rip-relative float constants."""
import re, struct, sys
import pefile, capstone
pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
va = int(sys.argv[2], 16)
n = int(sys.argv[3], 0) if len(sys.argv) > 3 else 0x100
off = pe.get_offset_from_rva(va - base)
code = pe.__data__[off:off + n]
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
def rd(rva, fmt):
    o = pe.get_offset_from_rva(rva)
    return struct.unpack_from(fmt, pe.__data__, o)[0]
for i in md.disasm(code, va):
    note = ""
    m = re.search(r"\[rip ([+-]) (0x[0-9a-f]+)\]", i.op_str)
    if m:
        t = i.address + i.size + int(m.group(2), 16) * (1 if m.group(1) == "+" else -1) - base
        try:
            if "dword" in i.op_str or "ss" in i.mnemonic:
                note = f"  ; [{t+base:#x}] = {rd(t, '<f'):g}f / {rd(t, '<I'):#x}"
            elif "qword" in i.op_str or "sd" in i.mnemonic:
                note = f"  ; [{t+base:#x}] = {rd(t, '<d'):g}"
            elif i.mnemonic == "lea":
                o = pe.get_offset_from_rva(t)
                s = pe.__data__[o:o + 80].split(b"\0")[0]
                if len(s) > 2 and all(32 <= c < 127 for c in s):
                    note = f'  ; "{s.decode()}"'
        except Exception:
            pass
    print(f"{i.address:#x}: {i.mnemonic} {i.op_str}{note}")
    if i.mnemonic in ("ret", "int3") and n <= 0x40:
        break
