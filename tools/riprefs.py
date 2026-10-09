"""riprefs.py <exe> <va>...: code sites whose rip-relative disp32 (insn ending right after it, or with a 1/4-byte imm)
points at <va>, with the owning .pdata function. Heuristic: prints the preceding bytes so the opcode can be checked."""
import struct, sys, bisect, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
text = next(s for s in pe.sections if s.Name.startswith(b".text"))
data = text.get_data()
tva = base + text.VirtualAddress
funcs = sorted((e.struct.BeginAddress + base, e.struct.EndAddress + base) for e in pe.DIRECTORY_ENTRY_EXCEPTION)
starts = [f[0] for f in funcs]
targets = [int(x, 16) for x in sys.argv[2:]]
for i in range(0, len(data) - 4):
    disp = struct.unpack_from("<i", data, i)[0]
    for imm in (0, 1, 4):
        t = tva + i + 4 + imm + disp
        if t in targets:
            va = tva + i
            k = bisect.bisect_right(starts, va) - 1
            f = funcs[k][0] if k >= 0 and funcs[k][0] <= va < funcs[k][1] else 0
            print(f"{t:#x} <- disp@{va:#x} imm{imm} func {f:#x} bytes {data[i-3:i].hex()}")
