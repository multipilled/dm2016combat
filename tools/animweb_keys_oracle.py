"""Run the md6 anim key decoders of the user's own DOOMx64.exe under Unicorn on synthetic frame sets and compare them
with the model written in gamedata/re/ANIMWEB.md 3b (key bracketing, t, lerp / nlerp, smallest-three quats).
Offline, read-only; nothing is written to the repo.

usage: animweb_keys_oracle.py <exe>
  rot  0x14173af10(jointList, count, frameInSet, range, frac, first, rangeData, next, bits, outQ)
  vec  0x14173b770(... same ..., outVec4)   (scale and translation channels)
Both call 0x14173a910 to find each channel's previous / next key.
Note: Unicorn's rcpps/rsqrtps are exact; real CPUs return ~12-bit estimates refined by one Newton step."""
import re, struct, sys, random
import numpy as np
sys.path.insert(0, __file__.rsplit("\\", 1)[0].rsplit("/", 1)[0])
from hdp_emu import Engine, HEAP

f32 = np.float32
BUF = HEAP + 0xB00000
ROT, VEC = 0x14173af10, 0x14173b770


def init_bss_consts(e, pe_path):
    """Run-free equivalent of the tiny static ctors 'mov{dqa,aps,ups} xmm0, [rip+src]; mov... [rip+dst], xmm0; ret'
    that fill the engine's .bss SIMD constants: copy every such 16-byte constant."""
    import pefile
    pe = pefile.PE(pe_path, fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    text = next(s for s in pe.sections if s.Name.rstrip(b"\0") == b".text")
    data = text.get_data()
    tva = base + text.VirtualAddress
    n = 0
    pats = [(rb"\x66\x0f\x6f\x05(.{4})\x66\x0f\x7f\x05(.{4})\xc3", 8),
            (rb"\x0f\x28\x05(.{4})\x0f\x29\x05(.{4})\xc3", 7),
            (rb"\x0f\x10\x05(.{4})\x0f\x11\x05(.{4})\xc3", 7)]
    for pat, ln in pats:
        for m in re.finditer(pat, data, re.S):
            va = tva + m.start()
            src = va + ln + struct.unpack("<i", m.group(1))[0]
            dst = va + 2 * ln + struct.unpack("<i", m.group(2))[0]
            e.mu.mem_write(dst, bytes(e.mu.mem_read(src, 16)))
            n += 1
    return n


def rdf(e, va):
    return f32(struct.unpack("<f", bytes(e.mu.mem_read(va, 4)))[0])


def decode_quat(u, SC, OF, THREE, MHALF):
    """exe smallest-three decode (0x14173af10): c = (u & 0x7fff)*SC + OF; x = ((1 - a*a) - b*b) - c*c;
    w = ((((y*x)*y) - 3)*y*-0.5)*x with y = rsqrt(x); result = [a,b,c,w] rotated by idx."""
    u0, u1, u2 = u
    a, b, c = (f32(f32(f32(v & 0x7fff) * SC) + OF) for v in u)
    x = f32(f32(f32(f32(1) - a * a) - b * b) - c * c)
    y = f32(1 / np.sqrt(np.float64(x)))
    w = f32(f32(f32(f32(f32(y * x) * y) - THREE) * y) * MHALF) * x
    tmp = [a, b, c, f32(w)]
    idx = (u0 >> 15) | ((u1 >> 15) << 1)
    return np.array([tmp[(idx + k) & 3] for k in range(4)], np.float32)


def check(e, which, trials=400):
    mu = e.mu
    SC, OF = rdf(e, 0x144fd9920), rdf(e, 0x144fd98e0)
    THREE, MHALF = rdf(e, 0x142883c70), rdf(e, 0x142883c80)
    rnd = random.Random(11 if which == "rot" else 12)
    vs = 6 if which == "rot" else 12
    worst, n, bad_pick = 0.0, 0, 0
    for trial in range(trials):
        C = rnd.choice([1, 3, 4, 5, 8])
        R = rnd.choice([1, 2, 5, 8, 9, 16, 31, 62, 64])
        mb = (R + 7) // 8
        keyfr = []  # per channel sorted key frames (0 and R always)
        for ch in range(C):
            ks = [0] + [k for k in range(1, R) if rnd.random() < 0.3] + [R]
            keyfr.append(ks)
        def val():
            if which == "rot":
                return [rnd.randrange(0, 0x10000) for _ in range(3)]
            return np.array([rnd.uniform(-40, 40) for _ in range(3)], np.float32)
        vals = [{k: val() for k in ks} for ks in keyfr]
        if which == "rot":  # keep the decoded 4th component real (sum of squares of the three <= 1)
            for ch in range(C):
                for k in vals[ch]:
                    while 1 - sum(float(f32(f32(v & 0x7fff) * SC) + OF) ** 2 for v in vals[ch][k]) <= 0.25:
                        vals[ch][k] = val()
        def enc(v):
            return struct.pack("<3H", *v) if which == "rot" else v.tobytes()
        first = b"".join(enc(vals[ch][0]) for ch in range(C))
        nxt = b"".join(enc(vals[ch][R]) for ch in range(C))
        rng = b"".join(enc(vals[ch][k]) for ch in range(C) for k in keyfr[ch][1:-1])
        # in-memory mask: per channel the little-endian integer of its mb bytes, frame k at bit (8*mb - 1 - k)
        # (= the file's big-endian MSB-first mask with each channel's bytes reversed)
        bits = bytearray(C * mb)
        for ch in range(C):
            m = 0
            for k in keyfr[ch][1:-1]:
                m |= 1 << (8 * mb - 1 - k)
            bits[ch * mb:(ch + 1) * mb] = m.to_bytes(mb, "little")
        lay, off = {}, BUF
        for name, blob in (("first", first), ("range", rng), ("next", nxt), ("bits", bytes(bits))):
            off += 0x40  # readable slack before each block (the bit scan reads 8 bytes ending at a channel's end)
            lay[name] = off
            mu.mem_write(off - 0x40, b"\0" * 0x40 + blob + b"\0" * 0x40)
            off += len(blob) + 0x80
        jl = off; joints = list(range(C)); mu.mem_write(jl, bytes(joints + [C] * (16 - C % 8 + 8)))
        out = jl + 0x40
        f = rnd.randrange(R)
        frac = f32(rnd.choice([0.0, rnd.random(), 0.5, 0.999]))
        mu.mem_write(out, b"\xcd" * 0x10 * 24)
        e.call(ROT if which == "rot" else VEC, jl, C, f, R, struct.unpack("<I", frac.tobytes())[0],
               lay["first"], lay["range"], lay["next"], lay["bits"], out)
        got = np.frombuffer(bytes(mu.mem_read(out, 0x10 * C)), np.float32).reshape(C, 4)
        x = f32(f32(f) + frac)
        for ch in range(C):
            ks = keyfr[ch]
            pf = max(k for k in ks if k <= f)
            nf = min(k for k in ks if k > f)
            d = f32(f32(nf) - f32(pf))
            r = f32(1) / d
            r1 = f32(f32(r + r) - f32(f32(r * d) * r))
            t = f32(r1 * f32(x - f32(pf)))
            if which == "rot":
                p = decode_quat(vals[ch][pf], SC, OF, THREE, MHALF)
                q = decode_quat(vals[ch][nf], SC, OF, THREE, MHALF)
                dot = float(np.dot(p.astype(np.float64), q.astype(np.float64)))
                if dot < 0: q = -q
                w = (q - p) * t + p
                nn = f32(np.dot(w, w))
                y = f32(1 / np.sqrt(np.float64(nn)))
                y1 = f32(f32(f32(f32(y * nn) * y) - THREE) * y) * MHALF
                want = (w * f32(y1)).astype(np.float32)
            else:
                p, q = vals[ch][pf], vals[ch][nf]
                want = np.append((q - p) * t + p, f32(0))
            err = float(np.max(np.abs(got[ch] - want)))
            if which == "vec": err /= 40
            if err > 1e-4: bad_pick += 1
            worst = max(worst, err); n += 1
    print(f"{which}: {n} channels, worst abs err {worst:.2e}, channels off by > 1e-4: {bad_pick}  "
          f"(consts: scale {SC!r} off {OF!r} three {THREE!r} mhalf {MHALF!r})")


if __name__ == "__main__":
    e = Engine(sys.argv[1])
    print("bss constants copied:", init_bss_consts(e, sys.argv[1]))
    check(e, "vec")
    check(e, "rot")
