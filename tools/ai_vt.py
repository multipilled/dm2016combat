"""Vtables of registered idTech 6 classes (AI FSM states / transitions etc.) in the user's own DOOM exe.

ai_vt.py <exe> <className>... [-n slots]
Finds the class registration record {name*, super*, size, ?, typeinfo*, create()} by its name string, reads the
vtable the create() function stores (`lea rax,[rip+X]; mov [rbx],rax`) and prints the slots; with two or more
classes, slots that differ from the first class's vtable are marked '*'.
"""
import re
import struct
import sys

import capstone
import pefile

args = sys.argv[1:]
n = 48
if '-n' in args:
    i = args.index('-n')
    n = int(args[i + 1])
    del args[i:i + 2]
pe = pefile.PE(args[0], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
data = pe.__data__
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)


def off(va):
    return pe.get_offset_from_rva(va - base)


def q(va):
    return struct.unpack_from('<Q', data, off(va))[0]


def cstr(va):
    o = off(va)
    return bytes(data[o:data.find(b'\0', o)]).decode('latin1')


rdata = [s for s in pe.sections if s.Name.startswith((b'.rdata', b'.data'))]


def find_string(s):
    pat = s.encode() + b'\0'
    for sec in rdata:
        raw = data[sec.PointerToRawData:sec.PointerToRawData + sec.SizeOfRawData]
        for m in re.finditer(re.escape(pat), raw):
            if m.start() == 0 or raw[m.start() - 1] == 0:
                yield base + sec.VirtualAddress + m.start()


def find_ptr(va):
    pat = struct.pack('<Q', va)
    for sec in rdata:
        raw = data[sec.PointerToRawData:sec.PointerToRawData + sec.SizeOfRawData]
        for m in re.finditer(re.escape(pat), raw):
            if m.start() % 8 == 0:
                yield base + sec.VirtualAddress + m.start()


def vtable_of(create, depth=0):
    """The vtable a create() / ctor stores at [obj]: the last `lea reg,[rip+X]; mov [r..], reg` before ret; when the
    create() only calls a ctor, follow the calls after the allocation."""
    code = data[off(create):off(create) + 0x400]
    lea, found, calls = {}, None, []
    for ins in md.disasm(code, create):
        if ins.mnemonic == 'lea' and 'rip + ' in ins.op_str:
            reg = ins.op_str.split(',')[0]
            lea[reg] = ins.address + ins.size + int(ins.op_str.split('rip + ')[1].rstrip(']'), 16)
        elif ins.mnemonic == 'mov' and ins.op_str.startswith('qword ptr [r') and '+' not in ins.op_str.split(',')[0]:
            src = ins.op_str.split(', ')[1]
            if src in lea:
                found = lea[src]
        elif ins.mnemonic == 'call' and ins.op_str.startswith('0x'):
            calls.append(int(ins.op_str, 16))
        if ins.mnemonic == 'ret':
            break
    if found or depth > 1:
        return found
    for c in calls[1:] if depth == 0 else calls:
        v = vtable_of(c, depth + 1)
        if v:
            found = v
    return found


def record(name):
    for s in find_string(name):
        for p in find_ptr(s):
            sup = q(p + 8)
            try:
                supname = cstr(sup)
            except Exception:
                continue
            if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', supname) or q(p + 40) >> 32 != 1:
                continue
            size = q(p + 16) & 0xffffffff
            create = q(p + 40)
            return p, supname, size, create
    return None


vts = []
for name in args[1:]:
    r = record(name)
    if not r:
        print(f'{name}: no registration record')
        continue
    p, sup, size, create = r
    vt = vtable_of(create)
    print(f'{name}: record 0x{p:x} super {sup} size 0x{size:x} create 0x{create:x} vtable '
          f'{"0x%x" % vt if vt else None}')
    if vt:
        vts.append((name, vt))
if vts:
    first = vts[0][1]
    for i in range(n):
        row = []
        for name, vt in vts:
            f = q(vt + 8 * i)
            mark = '*' if vt != first and f != q(first + 8 * i) else ' '
            row.append(f'{mark}0x{f:x}')
        print(f'  [{i:2}] +0x{8 * i:03x} ' + ' '.join(row))
