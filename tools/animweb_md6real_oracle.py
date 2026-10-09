"""Decode REAL .bmd6anim files with the user's own DOOMx64.exe under Unicorn and compare every animated channel, every
frame and several fractions with a Python port of crates/idres/src/md6anim.rs (Md6Anim::parse + sample).
Offline, read-only; nothing is written to the repo.

usage: animweb_md6real_oracle.py <exe> <file.bmd6anim>...
Steps: copy the file's idMD6AnimData (big-endian) into emulator memory, run the exe's own loader byte swap
0x1415ceef0(obj, 1) (obj+0x58 = animData), then per frame set call
  0x14173af10 rotations, 0x14173b770 scales / translations, 0x14173ba70 user channels,
  0x14173a4e0 constant rotations,
with an identity joint list (channel c -> output slot c), and compare with md6anim.rs's keys / interpolation.
Note: Unicorn's rcpps/rsqrtps are exact; real CPUs return ~12-bit estimates refined by one Newton step."""
import struct, sys
import numpy as np
sys.path.insert(0, __file__.rsplit("\\", 1)[0].rsplit("/", 1)[0])
from hdp_emu import Engine, HEAP
from animweb_keys_oracle import init_bss_consts

f32 = np.float32
AD = HEAP + 0xC00000          # animData (16-byte aligned: frame-set offsets are in 16-byte units)
OBJ, JL, OUT = HEAP + 0xBF0000, HEAP + 0xBF1000, HEAP + 0xBF2000
ROT, VEC, USR, CROT, SWAP = 0x14173af10, 0x14173b770, 0x14173ba70, 0x14173a4e0, 0x1415ceef0


# ---- Python port of md6anim.rs (file side, big-endian) ----
def be16(b, o): return struct.unpack_from(">H", b, o)[0]
def bef(b, o): return f32(struct.unpack_from(">f", b, o)[0])


def decode_quat_rs(b, o):
    u0, u1, u2 = be16(b, o), be16(b, o + 2), be16(b, o + 4)
    idx = ((((u0 >> 1) | (u1 & 0x8000)) >> 14)) & 3
    c = lambda u: f32(f32(u & 0x7fff) * f32(4.315969e-05)) - f32(0.70710677)
    a, bb, cc = c(u0), c(u1), c(u2)
    d = f32(np.sqrt(max(f32(f32(f32(f32(1.0) - a * a) - bb * bb) - cc * cc), f32(0))))
    tmp = [a, bb, cc, d]
    return np.array([tmp[(idx + k) & 3] for k in range(4)], np.float32)


def joint_list(b, o):
    total, out, p = b[o], [], o + 1
    while len(out) < total:
        cnt, first = b[p], b[p + 1]
        out += list(range(first, first + cnt)); p += 2
    return out


def parse_rs(b):
    n = struct.unpack_from("<I", b, 12)[0]
    s = 16 + n + 24 + 4
    h = lambda i: be16(b, s + 12 + i * 2)
    A = dict(s=s, nf=h(2), rate=h(3), nsets=h(4), fsofs=h(6), cr=h(7), cs=h(8), ct=h(9), cu=h(10), flags=h(1))
    info = s + 0x90
    lists = [joint_list(b, info + be16(b, info + 4 + i * 2)) for i in range(8)]
    A["lists"] = lists
    keys = {k: [[] for _ in lists[4 + i]] for i, k in enumerate("RSTU")}
    A["sets"] = []
    for st in range(A["nsets"]):
        fs = s + be16(b, s + A["fsofs"] + st * 2) * 16
        f = lambda i: be16(b, fs + i * 2)
        start, rng = f(17), f(18)
        A["sets"].append((fs - s, start, rng))
        mb = (rng + 7) // 8
        has = lambda bits, ch, fr: b[fs + bits + ch * mb + fr // 8] & (0x80 >> (fr % 8)) != 0
        for ci, (k, vs) in enumerate((("R", 6), ("S", 12), ("T", 12), ("U", 4))):
            rd = (lambda o: decode_quat_rs(b, o)) if k == "R" else \
                 (lambda o: np.array([bef(b, o), bef(b, o + 4), bef(b, o + 8)], np.float32)) if vs == 12 else \
                 (lambda o: bef(b, o))
            p = fs + f(4 + ci)
            for ch in range(len(lists[4 + ci])):
                keys[k][ch].append((start, rd(fs + f(ci) + ch * vs)))
                for fr in range(1, rng):
                    if has(f(8 + ci), ch, fr):
                        keys[k][ch].append((start + fr, rd(p))); p += vs
    A["keys"] = keys
    A["constR"] = [decode_quat_rs(b, s + A["cr"] + i * 6) for i in range(len(lists[0]))]
    return A


def bracket(keys, frame):
    if frame <= f32(keys[0][0]) or len(keys) == 1:
        return keys[0][1], keys[0][1], f32(0)
    i = sum(1 for k in keys if f32(k[0]) <= frame)
    if i >= len(keys):
        return keys[-1][1], keys[-1][1], f32(0)
    (fa, a), (fb, bv) = keys[i - 1], keys[i]
    return a, bv, f32(f32(frame - f32(fa)) / f32(max(fb - fa, 1)))


def nlerp_rs(a, b, t):
    s = f32(-1) if float(np.dot(a.astype(np.float64), b.astype(np.float64))) < 0 else f32(1)
    q = (a * f32(f32(1) - t) + b * s * t).astype(np.float32)
    return (q / max(f32(np.sqrt(np.dot(q, q))), f32(1e-12))).astype(np.float32)


# ---- exe side ----
def main():
    e = Engine(sys.argv[1]); init_bss_consts(e, sys.argv[1]); mu = e.mu
    for path in sys.argv[2:]:
        b = open(path, "rb").read()
        A = parse_rs(b)
        s = A["s"]
        total = struct.unpack_from(">I", b, s + 8)[0]
        mu.mem_write(AD, b[s:s + total] + b"\0" * 0x100)
        mu.mem_write(OBJ, b"\0" * 0x100 + struct.pack("<Q", 0))
        mu.mem_write(OBJ + 0x58, struct.pack("<Q", AD))
        e.call(SWAP, OBJ, 1)
        nf_mem = struct.unpack("<H", bytes(mu.mem_read(AD + 0x10, 2)))[0]
        assert nf_mem == A["nf"], (nf_mem, A["nf"])
        cnt = [len(A["lists"][4 + i]) for i in range(4)]
        mu.mem_write(JL, bytes(range(256)))
        worst = {k: 0.0 for k in "RSTU"}; n = {k: 0 for k in "RSTU"}
        for (fsofs, start, rng) in A["sets"]:
            fs = AD + fsofs
            hdr = struct.unpack("<19H", bytes(mu.mem_read(fs, 38)))
            assert hdr[17] == start and hdr[18] == rng, (hdr[17:19], start, rng)
            for f in range(rng):
                for frac in (0.0, 0.25, 0.5, 0.999):
                    frac = f32(frac)
                    gframe = f32(f32(start + f) + frac)
                    if gframe > f32(A["nf"] - 1): continue
                    for ci, (k, fn) in enumerate((("R", ROT), ("S", VEC), ("T", VEC), ("U", USR))):
                        if cnt[ci] == 0: continue
                        mu.mem_write(OUT, b"\xcd" * 0x10 * (cnt[ci] + 8))
                        e.call(fn, JL, cnt[ci], f, rng, struct.unpack("<I", frac.tobytes())[0],
                               fs + hdr[ci], fs + hdr[4 + ci], fs + hdr[12 + ci], fs + hdr[8 + ci], OUT)
                        stride = 4 if k == "U" else 16
                        raw = bytes(mu.mem_read(OUT, stride * cnt[ci]))
                        for ch in range(cnt[ci]):
                            a, bv, t = bracket(A["keys"][k][ch], gframe)
                            if k == "R":
                                want = nlerp_rs(a, bv, t)
                                got = np.frombuffer(raw[ch * 16:ch * 16 + 16], np.float32)
                                if float(np.dot(want, got)) < 0: want = -want
                                err = float(np.max(np.abs(got - want)))
                            elif k == "U":
                                want = a + (bv - a) * t
                                got = np.frombuffer(raw[ch * 4:ch * 4 + 4], np.float32)[0]
                                err = abs(float(got - want)) / max(1.0, abs(float(want)))
                            else:
                                want = a + (bv - a) * t
                                got = np.frombuffer(raw[ch * 16:ch * 16 + 12], np.float32)
                                err = float(np.max(np.abs(got - want))) / max(1.0, float(np.max(np.abs(want))))
                            worst[k] = max(worst[k], err); n[k] += 1
        # constant rotations
        nc = len(A["lists"][0]); cworst = 0.0
        if nc:
            cr = struct.unpack("<H", bytes(mu.mem_read(AD + 0x1a, 2)))[0]
            mu.mem_write(OUT, b"\xcd" * 0x10 * (nc + 8))
            e.call(CROT, JL, nc, AD + cr, OUT)
            for i in range(nc):
                got = np.frombuffer(bytes(mu.mem_read(OUT + 16 * i, 16)), np.float32)
                cworst = max(cworst, float(np.max(np.abs(got - A["constR"][i]))))
        name = path.replace("\\", "/").split("/")[-1]
        print(f"{name}: frames {A['nf']} sets {A['nsets']} flags {A['flags']:#x} channels R/S/T/U {cnt} const R {nc}")
        print("   worst |exe - md6anim.rs|: " + ", ".join(f"{k} {worst[k]:.2e} ({n[k]} samples)" for k in "RSTU")
              + f", constR {cworst:.2e}")


if __name__ == "__main__":
    main()
