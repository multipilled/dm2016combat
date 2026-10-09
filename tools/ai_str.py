"""Print what sits at VAs of the user's own DOOM exe: a C string, or a pointer to one, or the f32/i32/qword value.
ai_str.py <exe> <va>...   (also accepts a decompile file with -f <file.c>: resolves every UNK_/DAT_ in it)"""
import re
import struct
import sys

import pefile

pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
data = pe.__data__


def off(va):
    return pe.get_offset_from_rva(va - base)


def cstr(va):
    try:
        o = off(va)
    except Exception:
        return None
    end = data.find(b'\0', o, o + 200)
    if end <= o:
        return None
    s = bytes(data[o:end])
    if all(32 <= c < 127 for c in s) and len(s) >= 3:
        return s.decode()
    return None


def show(va):
    s = cstr(va)
    if s:
        return f'"{s}"'
    try:
        o = off(va)
    except Exception:
        return '(bss)'
    qv = struct.unpack_from('<Q', data, o)[0]
    s = cstr(qv) if base <= qv < base + 0x20000000 else None
    if s:
        return f'-> "{s}"'
    f = struct.unpack_from('<f', data, o)[0]
    i = struct.unpack_from('<i', data, o)[0]
    return f'f32 {f:g} i32 {i} q 0x{qv:x}'


args = sys.argv[2:]
if args and args[0] == '-f':
    txt = open(args[1], encoding='utf-8', errors='replace').read()
    seen = sorted(set(int(m, 16) for m in re.findall(r'(?:UNK|DAT|PTR_DAT)_(1[0-9a-f]{8})', txt)))
    for va in seen:
        print(f'0x{va:x}: {show(va)}')
else:
    for a in args:
        va = int(a, 16)
        print(f'0x{va:x}: {show(va)}')
