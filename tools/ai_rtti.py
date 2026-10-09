"""MSVC RTTI vftable(s) of a class in the user's own DOOM exe and the code that stores them (ctors / dtors).
ai_rtti.py <exe> <ClassName>"""
import re, struct, sys
import pefile, numpy as np
pe = pefile.PE(sys.argv[1], fast_load=True); base = pe.OPTIONAL_HEADER.ImageBase; d = pe.__data__
secs = {s.Name.rstrip(b'\0').decode(): s for s in pe.sections}
def raw(n): s = secs[n]; return bytes(d[s.PointerToRawData:s.PointerToRawData + s.SizeOfRawData]), s.VirtualAddress
data, dva = raw('.data'); rdata, rva = raw('.rdata'); text, tva = raw('.text')
name = b'.?AV' + sys.argv[2].encode() + b'@@\0'
i = data.find(name)
td_rva = dva + i - 0x10
print(f'type descriptor 0x{base + td_rva:x}')
for m in re.finditer(re.escape(struct.pack('<I', td_rva)), rdata):
    col = m.start() - 0xc
    sig, off = struct.unpack_from('<II', rdata, col)
    if sig != 1:
        continue
    col_va = base + rva + col
    for v in re.finditer(re.escape(struct.pack('<Q', col_va)), rdata):
        vft = base + rva + v.start() + 8
        print(f'COL 0x{col_va:x} offset {off} vftable 0x{vft:x}')
        arr = np.frombuffer(text, dtype=np.uint8)
        # lea r64, [rip+disp32]: 48/4c 8d modrm(05|0d|15|..) disp
        for k in range(len(text) - 7):
            pass
        t = np.frombuffer(text[:len(text) // 4 * 4], dtype=np.uint8).astype(np.int64)
        disp = t[3:-4] | (t[4:-3] << 8) | (t[5:-2] << 16) | (t[6:-1] << 24)
        disp = np.where(disp >= 2**31, disp - 2**32, disp)
        idx = np.nonzero(((t[:-7] == 0x48) | (t[:-7] == 0x4c)) & (t[1:-6] == 0x8d) & ((t[2:-5] & 0xc7) == 5)
                         & (tva + np.arange(len(disp)) + 7 + disp == vft - base))[0]
        for j in idx[:12]:
            print(f'   lea at 0x{base + tva + j:x}')
