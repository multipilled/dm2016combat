"""disasm_ann.py <va> <len> [stop]: disassemble the user's DOOMx64.exe with rip refs resolved to cvars (gamedata/exe/cvars.tsv), renderparm globals (parm:<name>), strings and floats. Run from the repo root."""
import os, re, struct, sys, bisect, pefile, capstone
pe = pefile.PE(os.environ.get("DOOM_EXE", r"C:/Program Files (x86)/Steam/steamapps/common\DOOM\DOOMx64.exe"), fast_load=True); base = pe.OPTIONAL_HEADER.ImageBase; data = pe.__data__
rows = []
for line in open("gamedata/exe/cvars.tsv", encoding="utf8"):
    p = line.rstrip("\n").split("\t")
    if len(p) >= 3:
        try: rows.append((int(p[2], 16), p[0]))
        except: pass
rows.sort(); ks = [r[0] for r in rows]
def sat(va):
    try:
        o = pe.get_offset_from_rva(va - base); s = data[o:o + 80].split(b"\0")[0]
        return s.decode() if len(s) > 2 and all(32 <= c < 127 for c in s) else None
    except Exception: return None
def name(t):
    i = bisect.bisect_right(ks, t) - 1
    if i >= 0 and t - ks[i] < 0x60: return f"{rows[i][1]}+{t-ks[i]:#x}"
    # renderparm table entry {type, name, ptr}
    try:
        q = struct.unpack("<Q", pe.get_data(t - base - 0x10, 8))[0]
        n = sat(q)
        if n: return f"parm:{n}"
    except Exception: pass
    s = sat(t)
    if s: return f'"{s}"'
    try: return f"{struct.unpack('<f', pe.get_data(t-base,4))[0]:g}f"
    except Exception: return ""
va, n = int(sys.argv[1], 16), int(sys.argv[2], 0)
code = pe.get_data(va - base, n)
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
for i in md.disasm(code, va):
    note = ""
    m = re.search(r"\[rip ([+-]) (0x[0-9a-f]+)\]", i.op_str)
    if m:
        t = i.address + i.size + int(m.group(2), 16) * (1 if m.group(1) == "+" else -1)
        note = f"  ; {t:#x} {name(t)}"
    print(f"{i.address:#x}: {i.mnemonic} {i.op_str}{note}")
    if i.mnemonic == "ret" and len(sys.argv) > 3 and sys.argv[3] == "stop": break
