"""Recursive reflection dump from the user's own DOOM exe: fx_typeinfo.py <exe> <typeName>...
Prints each struct's vars (offset, size, type, name, comment), recursing into member struct types, and the
values of every enum type met on the way (enum tables are {char* name, int64 value} runs)."""
import sys, struct, re, pefile

pe = pefile.PE(sys.argv[1], fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
d = pe.__data__


def q(va):
    return struct.unpack_from("<Q", d, pe.get_offset_from_rva(va - base))[0]


def s(va, n=400):
    if not (base <= va < base + 0x6000000):
        return None
    try:
        o = pe.get_offset_from_rva(va - base)
    except Exception:
        return None
    b = d[o:o + n].split(b"\0")[0]
    return b.decode() if b and all(32 <= c < 127 for c in b) else None


def va_of(off):
    return base + pe.get_rva_from_offset(off)


def records(name):
    """Type-info records {name*, super*, size, ?, vars*} for a struct name."""
    out = []
    for i in re.finditer(re.escape(b"\0" + name.encode() + b"\0"), d):
        name_va = va_of(i.start() + 1)
        for m in re.finditer(re.escape(struct.pack("<Q", name_va)), d):
            rec = va_of(m.start())
            try:
                sup, size, vars_ = q(rec + 8), q(rec + 16), q(rec + 32)
            except Exception:
                continue
            if size and size < 0x100000 and base < vars_ < base + 0x6000000 and s(q(vars_)):
                out.append((rec, sup, size, vars_))
    return out


def enum_values(name):
    """Enum value table: {char* 'VALUE', int64} entries; the type record holds a pointer to the table."""
    # enum type records: {name*, ?, values*} - find tables by scanning for the first value string pattern
    res = []
    for i in re.finditer(re.escape(b"\0" + name.encode() + b"\0"), d):
        name_va = va_of(i.start() + 1)
        for m in re.finditer(re.escape(struct.pack("<Q", name_va)), d):
            rec = m.start()
            for k in (8, 16, 24):
                try:
                    p = struct.unpack_from("<Q", d, rec + k)[0]
                    o = pe.get_offset_from_rva(p - base)
                except Exception:
                    continue
                vals = []
                while True:
                    try:
                        sp, v = struct.unpack_from("<Qq", d, o)
                    except Exception:
                        break
                    nm = s(sp, 120)
                    if not nm or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_:]*", nm):
                        break
                    vals.append((v, nm))
                    o += 16
                if len(vals) >= 2:
                    res.append(vals)
            if res:
                return res[0]
    return None


seen = set()


def dump(name, depth=0):
    if name in seen:
        return
    seen.add(name)
    recs = records(name)
    if not recs:
        ev = enum_values(name)
        if ev:
            print(f"== enum {name}: " + " ".join(f"{v}={n}" for v, n in ev))
        return
    rec, sup, size, vars_ = recs[0]
    print(f"== {name} : {s(sup)} size {size:#x} (rec {rec:#x})")
    v = vars_
    subs = []
    while True:
        t, nm, offsz, comment = q(v), q(v + 16), q(v + 24), q(v + 40)
        if t == 0 or s(t) is None:
            break
        ty = s(t)
        dflt = s(q(v + 32), 120) if q(v + 32) else None
        print(f"   +{offsz & 0xffffffff:#06x} [{offsz >> 32:#x}] {ty:40} {s(nm)}" + (f" = {dflt!r}" if dflt else "") + f"   // {(s(comment, 2000) or '').strip()[:300]}")
        subs.append(ty)
        v += 0x48
    for ty in subs:
        base_ty = re.sub(r"^(const )?", "", ty).strip(" *")
        m = re.match(r"idList < ([^,]+) ,", base_ty) or re.match(r"idArray < ([^,]+) ,", base_ty) or re.match(r"idStaticList < ([^,]+) ,", base_ty)
        if m:
            base_ty = m.group(1).strip(" *")
        if base_ty in ("int", "float", "bool", "short", "char", "unsigned char", "unsigned short", "unsigned int", "idVec2", "idVec3", "idVec4", "idMat3", "idAngles", "idAtomicString", "idStr"):
            continue
        dump(base_ty, depth + 1)


for cls in sys.argv[2:]:
    dump(cls)
