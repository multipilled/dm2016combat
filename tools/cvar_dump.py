"""Recover idCVar defaults from the user's own DOOM (2016) exe, offline.

Static initialisers build each cvar as:
    lea r8,[value]; lea rdx,[name]; lea rcx,[this]; call idCVar::idCVar   (flags in r9d, description on stack)
Output (tab separated): name, default, cvar object VA, description.
Written to the git-ignored gamedata/ folder; nothing from the exe is committed.
"""
import re, struct, sys

exe, out = sys.argv[1], sys.argv[2]
data = open(exe, "rb").read()
pe = struct.unpack_from("<I", data, 0x3C)[0]
nsec = struct.unpack_from("<H", data, pe + 6)[0]
optsize = struct.unpack_from("<H", data, pe + 20)[0]
image_base = struct.unpack_from("<Q", data, pe + 24 + 24)[0]
secs = []
for i in range(nsec):
    o = pe + 24 + optsize + i * 40
    name = data[o:o + 8].rstrip(b"\0").decode()
    vsize, va, rawsize, rawptr = struct.unpack_from("<IIII", data, o + 8)
    secs.append((name, va, rawptr, rawsize))

def rva_to_off(rva):
    for _, va, rp, rs in secs:
        if va <= rva < va + rs:
            return rp + rva - va

def cstr(rva):
    o = rva_to_off(rva)
    if o is None:
        return None
    e = data.find(b"\0", o, o + 4096)
    try:
        return data[o:e].decode("ascii")
    except UnicodeDecodeError:
        return None

_, tva, trp, trs = next(s for s in secs if s[0] == ".text")
code = data[trp:trp + trs]

def lea_target(i):
    return tva + i + 7 + struct.unpack_from("<i", code, i + 3)[0]

LEA_RDX, LEA_RCX, LEA_R8, LEA_RAX = b"\x48\x8d\x15", b"\x48\x8d\x0d", b"\x4c\x8d\x05", b"\x48\x8d\x05"
name_re = re.compile(r"^[a-z][a-z0-9]*_[A-Za-z0-9_]+$")
rows = {}
def find_back(i, pat, window):
    for j in range(i - 7, i - window, -1):
        if code[j:j + 3] == pat:
            return j
def find_fwd(i, pat, window):
    for j in range(i + 7, i + window):
        if code[j:j + 3] == pat:
            return j

for m in re.finditer(re.escape(LEA_RDX), code):
    i = m.start()
    name = cstr(lea_target(i))
    if not name or not name_re.match(name):
        continue
    # plain ctor: lea r8; lea rdx; lea rcx.  min/max ctor interleaves movss/xorps between them.
    jr8, jrcx = find_back(i, LEA_R8, 40), find_fwd(i, LEA_RCX, 24)
    if jr8 is None or jrcx is None:
        continue
    value = cstr(lea_target(jr8))
    if value is None or len(value) > 64:
        continue
    obj = lea_target(jrcx)
    # description: the lea rax shortly before, stored to [rsp+0x20]
    desc = ""
    for j in range(i - 48, i - 7):
        if code[j:j + 3] == LEA_RAX:
            d = cstr(lea_target(j))
            if d and " " in d:
                desc = d
    rows.setdefault(name, (value, obj, desc))

with open(out, "w", encoding="utf8") as f:
    for name in sorted(rows, key=str.lower):
        value, obj, desc = rows[name]
        clean = lambda s: s.replace("\t", "\\t").replace("\n", "\\n").replace("\r", "\\r")
        f.write(f"{name}\t{clean(value)}\t{image_base + obj:#x}\t{clean(desc)}\n")
print(len(rows), "cvars")
