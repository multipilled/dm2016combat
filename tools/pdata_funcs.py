"""Primary (non-chained) .pdata function starts in [lo, hi) of the user's own exe: pdata_funcs.py <exe> <lo> <hi>"""
import sys, struct, pefile
pe = pefile.PE(sys.argv[1], fast_load=True)
pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXCEPTION"]])
base = pe.OPTIONAL_HEADER.ImageBase
lo, hi = int(sys.argv[2], 16), int(sys.argv[3], 16)
for e in sorted(pe.DIRECTORY_ENTRY_EXCEPTION, key=lambda e: e.struct.BeginAddress):
    va = base + e.struct.BeginAddress
    if not lo <= va < hi:
        continue
    flags = pe.__data__[pe.get_offset_from_rva(e.struct.UnwindData)] >> 3
    if flags & 0x4:  # UNW_FLAG_CHAININFO: a fragment of another function
        continue
    print(hex(va))
