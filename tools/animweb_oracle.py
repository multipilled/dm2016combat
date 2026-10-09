"""Run small anim-web / md6 functions of the user's own DOOMx64.exe under Unicorn and compare them with the
formulas written in gamedata/re/ANIMWEB.md. Offline, read-only; nothing is written to the repo.

usage: animweb_oracle.py <exe>
Checks:
  alpha   idMD6Branch::UpdateAlpha 0x1415d6df0 (time, prevTime, ticks)
  ease    GenerateBranchAlpha 0x141a383e0 for every blendType
  sample  leaf pose sample position 0x141a38670 (frame, frac, loops) for CLAMP / REPEAT
  lerp    LERP pose kernel 0x14173e400 (per-joint weights, sign flip, nlerp, T/S lerp, weight bytes)
Note: Unicorn's rcpps/rsqrtps are exact, real CPUs return ~12-bit estimates (refined by one Newton step), so the
kernel check uses a tolerance."""
import math, struct, sys, random
import numpy as np
from unicorn.x86_const import UC_X86_REG_XMM1, UC_X86_REG_XMM0, UC_X86_REG_XMM2
sys.path.insert(0, __file__.rsplit("\\", 1)[0].rsplit("/", 1)[0])
from hdp_emu import Engine, HEAP

f32 = np.float32
BUF = HEAP + 0xA00000


def fbits(x):
    return struct.unpack("<I", struct.pack("<f", x))[0]


def xmm_float(mu, reg):
    return struct.unpack("<f", struct.pack("<I", mu.reg_read(reg) & 0xffffffff))[0]


def check_alpha(e):
    mu, bad, n = e.mu, 0, 0
    rnd = random.Random(1)
    for _ in range(2000):
        cur, tgt = f32(rnd.random()), f32(rnd.choice([0.0, 1.0, rnd.random()]))
        rate = f32(rnd.choice([1000 / 100, 1000 / 33, 1000 / 66, 0.0, rnd.random() * 40]))
        t1 = rnd.randint(0, 100000); dt = rnd.randint(-3, 70); ticks = 960
        br = BUF
        mu.mem_write(br, b"\0" * 0x40)
        mu.mem_write(br + 0x2c, struct.pack("<fff", cur, tgt, rate))
        e.call(0x1415d6df0, br, t1 + dt, t1, ticks)
        got = struct.unpack("<f", mu.mem_read(br + 0x2c, 4))[0]
        # ANIMWEB.md 2d
        want = cur
        if dt > 0:
            step = f32(f32(dt) / f32(ticks)); d = f32(tgt - cur)
            if d > 0:
                c = f32(f32(step * rate) + cur); want = tgt if c > f32(tgt - f32(1e-6)) else c
            elif d < 0:
                c = f32(cur - f32(step * rate)); want = tgt if f32(tgt + f32(1e-6)) > c else c
        n += 1
        if fbits(got) != fbits(want):
            bad += 1
            if bad < 5: print("alpha mismatch", cur, tgt, rate, dt, got, want)
    print(f"alpha: {n - bad}/{n} bit-exact")


def ease_model(c, T, bt):
    c, T = f32(c), f32(T)
    if bt == 0: return c
    sq, out = f32(c * c), f32(f32(c + c) - f32(c * c))
    if bt == 1: return out if c > T else sq
    if bt == 2: return sq if c > T else out
    t = f32(c + c)
    if t < f32(1.0): return f32(f32(t * f32(0.5)) * t)
    u = f32(t + f32(-1.0))
    return f32(f32(f32(f32(u + u) - f32(u * u)) + f32(1.0)) * f32(0.5))


def check_ease(e):
    mu, bad, n = e.mu, 0, 0
    rnd = random.Random(2)
    for _ in range(2000):
        c, T, bt = f32(rnd.random()), f32(rnd.choice([0.0, 1.0, rnd.random()])), rnd.randint(0, 3)
        mu.mem_write(BUF, b"\0" * 0x40)
        mu.mem_write(BUF + 0x2c, struct.pack("<ff", c, T))
        mu.mem_write(BUF + 0x38, struct.pack("<i", bt))
        e.call(0x141a383e0, BUF)
        got = xmm_float(mu, UC_X86_REG_XMM0)
        want = ease_model(c, T, bt)
        n += 1
        if fbits(got) != fbits(want):
            bad += 1
            if bad < 5: print("ease mismatch", c, T, bt, got, want)
    print(f"ease: {n - bad}/{n} bit-exact")


def check_sample(e):
    """leaf: anim (+0x10) -> animData (+0x58): numFrames +0x10, frameRate +0x12. LeafPlay type 2."""
    mu, bad, n = e.mu, 0, 0
    rnd = random.Random(3)
    leaf, anim, data = BUF, BUF + 0x100, BUF + 0x200
    for _ in range(3000):
        N = rnd.randint(2, 120); fr = rnd.choice([30, 30, 60, 24]); wrap = rnd.randint(0, 1)
        rate = f32(rnd.choice([1.0, 0.25, 1.5, rnd.random() * 3]))
        start = rnd.randint(0, 50000); t = start + rnd.randint(-50, 20000); ticks = 960
        mu.mem_write(leaf, b"\0" * 0x48); mu.mem_write(anim, b"\0" * 0x90); mu.mem_write(data, b"\0" * 0x90)
        mu.mem_write(leaf + 8, bytes([2]))
        mu.mem_write(leaf + 0x10, struct.pack("<Q", anim))
        mu.mem_write(leaf + 0x1d, bytes([wrap]))
        mu.mem_write(leaf + 0x28, struct.pack("<iff", start, -1.0, rate))
        mu.mem_write(anim + 0x58, struct.pack("<Q", data))
        mu.mem_write(data + 0x10, struct.pack("<HH", N, fr))
        out = BUF + 0x400
        mu.mem_write(out, b"\0" * 32)
        e.call(0x141a38670, leaf, t, out, out + 8, out + 16, ticks)
        gf = struct.unpack("<h", mu.mem_read(out, 2))[0]
        gx = struct.unpack("<f", mu.mem_read(out + 8, 4))[0]
        gl = struct.unpack("<i", mu.mem_read(out + 16, 4))[0]
        # ANIMWEB.md 3a
        el = 0 if t < start else int(f32(f32(t - start) * rate))
        loops = ((el * fr) // ticks) // (N - 1)
        f = f32(f32(f32(f32(1.0) / f32(ticks)) * f32(el)) * f32(fr))
        if wrap == 0:
            if loops != 0: f = f32(N - 1)
            if f < 0: wf, wx = 0, f32(0)
            elif f < f32(N): wf, wx = int(f), f32(f - f32(int(f)))
            else: wf, wx = N - 1, f32(0)
        else:
            u = int(f); wx = f32(f - f32(u)); wf = u % (N - 1)
        n += 1
        if gf != wf or fbits(gx) != fbits(wx) or gl != loops:
            bad += 1
            if bad < 5: print("sample mismatch", N, fr, wrap, rate, t - start, (gf, gx, gl), (wf, wx, loops))
    print(f"sample: {n - bad}/{n} exact")


def check_lerp(e):
    mu = e.mu
    for fn in (0x1402223b0, 0x140222430):  # .bss SIMD constants (sign mask, zero) used by the kernel
        e.call(fn)
    rnd = random.Random(4)
    worst, n = 0.0, 0
    for _ in range(300):
        J = 4
        def quat():
            q = np.array([rnd.gauss(0, 1) for _ in range(4)], dtype=np.float32); return (q / np.linalg.norm(q)).astype(np.float32)
        qL = [quat() for _ in range(J)]; qR = [quat() for _ in range(J)]
        tL = [np.array([rnd.uniform(-50, 50) for _ in range(4)], np.float32) for _ in range(J)]
        tR = [np.array([rnd.uniform(-50, 50) for _ in range(4)], np.float32) for _ in range(J)]
        sL = [np.array([rnd.uniform(0.5, 2) for _ in range(4)], np.float32) for _ in range(J)]
        sR = [np.array([rnd.uniform(0.5, 2) for _ in range(4)], np.float32) for _ in range(J)]
        F = [rnd.choice([255, 255, 128, 0]) for _ in range(J)]
        A = [rnd.choice([255, 255, 0, 100]) for _ in range(J)]
        B = [rnd.choice([255, 255, 0, 200]) for _ in range(J)]
        alpha = f32(rnd.random())
        lay = {}
        off = BUF
        for name, arr in (("qL", qL), ("qR", qR), ("tL", tL), ("tR", tR), ("sL", sL), ("sR", sR)):
            lay[name] = off; mu.mem_write(off, b"".join(a.tobytes() for a in arr)); off += 0x100
        for name, arr in (("F", F), ("A", A), ("B", B)):
            lay[name] = off; mu.mem_write(off, bytes(arr) + b"\0" * 12); off += 0x100
        for name in ("oq", "ot", "os", "ow"):
            lay[name] = off; mu.mem_write(off, b"\xcd" * 0x100); off += 0x100
        mu.reg_write(UC_X86_REG_XMM1, fbits(alpha))
        e.call(0x14173e400, J, fbits(alpha), lay["F"], lay["qL"], lay["tL"], lay["sL"], lay["A"], lay["qR"],
               lay["tR"], lay["sR"], lay["B"], lay["oq"], lay["ot"], lay["os"], lay["ow"])
        oq = np.frombuffer(bytes(mu.mem_read(lay["oq"], 64)), np.float32).reshape(4, 4)
        ot = np.frombuffer(bytes(mu.mem_read(lay["ot"], 64)), np.float32).reshape(4, 4)
        os_ = np.frombuffer(bytes(mu.mem_read(lay["os"], 64)), np.float32).reshape(4, 4)
        ow = bytes(mu.mem_read(lay["ow"], 4))
        for j in range(J):
            Ap, Bp = f32(f32(A[j]) * f32(F[j])), f32(f32(B[j]) * f32(F[j]))
            a = Ap if Ap > f32(1.1754944e-38) else f32(1.0)
            b = Bp if Bp > f32(1.1754944e-38) else f32(1.0)
            t = f32(f32(f32(Ap * alpha) + Bp) - Ap) / b if Ap < Bp else f32(Bp * alpha) / a
            d = float(np.dot(qL[j].astype(np.float64), qR[j].astype(np.float64)))
            s = -t if d < 0 else t
            q = (qL[j] - qL[j] * t) + qR[j] * s
            q = q / math.sqrt(float(np.dot(q, q)))
            want_t = (tR[j] - tL[j]) * t + tL[j]
            want_s = (sR[j] - sL[j]) * t + sL[j]
            err = max(np.max(np.abs(oq[j] - q)), np.max(np.abs(ot[j] - want_t)) / 50, np.max(np.abs(os_[j] - want_s)))
            worst = max(worst, float(err)); n += 1
            if ow[j] != min(255, A[j] + B[j]):
                print("weight byte mismatch", A[j], B[j], ow[j])
    print(f"lerp: {n} joints, worst abs err vs ANIMWEB.md 3 formula {worst:.2e}")


def check_leaf_time(e):
    """Restart 0x1415d4e50(leaf, t, ticks, startFrame, wrap), SetRate int-mode 0x1415d5a20(leaf, t, rate),
    int frame 0x1415d3140(leaf, t, ticks)."""
    mu, bad, n = e.mu, 0, 0
    rnd = random.Random(5)
    leaf, anim, data = BUF, BUF + 0x100, BUF + 0x200
    for _ in range(3000):
        N = rnd.randint(2, 120); fr = rnd.choice([30, 60, 24]); ticks = 960
        rate = f32(rnd.choice([1.0, 0.25, 1.5, rnd.random() * 3 + 0.01])); wrap = rnd.randint(0, 1)
        t = rnd.randint(0, 100000); sf = rnd.randint(0, N - 1)
        mu.mem_write(leaf, b"\0" * 0x48); mu.mem_write(anim, b"\0" * 0x90); mu.mem_write(data, b"\0" * 0x90)
        mu.mem_write(leaf + 8, bytes([2])); mu.mem_write(leaf + 0x10, struct.pack("<Q", anim))
        mu.mem_write(leaf + 0x30, struct.pack("<f", rate))
        mu.mem_write(anim + 0x58, struct.pack("<Q", data)); mu.mem_write(data + 0x10, struct.pack("<HH", N, fr))
        mu.mem_write(BUF + 0x400, struct.pack("<Q", wrap))
        # Restart: 5th arg (wrap) on the stack
        e.call(0x1415d4e50, leaf, t, ticks, sf, wrap)
        st = struct.unpack("<i", mu.mem_read(leaf + 0x28, 4))[0]
        want = int(f32(f32(t) - f32(f32((sf * ticks) // fr) / rate)))
        n += 1
        if st != want:
            bad += 1
            if bad < 5: print("restart mismatch", t, sf, fr, rate, st, want)
        # SetRate
        t2 = t + rnd.randint(0, 5000); nr = f32(rnd.choice([1.0, 0.5, 2.0, -1.0, rnd.random() * 2 + 0.05]))
        mu.reg_write(UC_X86_REG_XMM2, fbits(nr))
        e.call(0x1415d5a20, leaf, t2, fbits(nr))
        st2 = struct.unpack("<i", mu.mem_read(leaf + 0x28, 4))[0]
        r2 = struct.unpack("<f", mu.mem_read(leaf + 0x30, 4))[0]
        newr = nr if nr >= 0 else f32(1.0)
        if nr != rate:
            el = f32(f32(t2 - st) * rate) if st <= t2 else f32(0)
            want2 = int(f32(f32(t2) - f32(el / newr)))
        else:
            want2 = st
        n += 1
        if st2 != want2 or (nr != rate and fbits(r2) != fbits(newr)):
            bad += 1
            if bad < 10: print("setrate mismatch", t2 - st, rate, nr, st2, want2, r2)
        # int frame
        mu.mem_write(leaf + 0x1d, bytes([wrap]))
        t3 = st2 + rnd.randint(-10, 30000)
        got = e.call(0x1415d3140, leaf, t3, ticks) & 0xffff
        el = 0 if t3 < st2 else int(f32(f32(t3 - st2) * f32(r2)))
        fi = (el * fr) // ticks
        wantf = (fi % (N - 1)) & 0xffff if wrap else (fi if fi < N else N - 1)
        n += 1
        if got != wantf:
            bad += 1
            if bad < 15: print("intframe mismatch", N, wrap, el, got, wantf)
    print(f"leaf time: {n - bad}/{n} exact")


def qmul(a, b):
    """Hamilton product a (x) b, components (x, y, z, w)."""
    ax, ay, az, aw = a; bx, by, bz, bw = b
    return np.array([aw * bx + ax * bw + ay * bz - az * by,
                     aw * by + ay * bw + az * bx - ax * bz,
                     aw * bz + az * bw + ax * by - ay * bx,
                     aw * bw - ax * bx - ay * by - az * bz], np.float64)


def check_additive(e, fn, sub):
    """ADD kernel 0x141736c90 / SUB kernel 0x14173f650: same argument layout as the LERP kernel; left = base, right = delta.
    Channel 1 (args 5/9/13) = scale (multiplicative), channel 2 (args 6/10/14) = translation (additive)."""
    mu = e.mu
    for f in (0x1402223b0, 0x140222430, 0x140222350):
        e.call(f)
    rnd = random.Random(6 + sub)
    worst, n, wbad = 0.0, 0, 0
    c255 = f32(1.0) / f32(255.0)
    for _ in range(300):
        J = 4
        def quat():
            q = np.array([rnd.gauss(0, 1) for _ in range(4)], dtype=np.float32); return (q / np.linalg.norm(q)).astype(np.float32)
        qB = [quat() for _ in range(J)]; qA = [quat() for _ in range(J)]
        sB = [np.array([rnd.uniform(0.5, 2) for _ in range(4)], np.float32) for _ in range(J)]
        sA = [np.array([rnd.uniform(0.5, 2) for _ in range(4)], np.float32) for _ in range(J)]
        tB = [np.array([rnd.uniform(-50, 50) for _ in range(4)], np.float32) for _ in range(J)]
        tA = [np.array([rnd.uniform(-5, 5) for _ in range(4)], np.float32) for _ in range(J)]
        F = [rnd.choice([255, 255, 128, 0]) for _ in range(J)]
        A = [rnd.choice([255, 254, 0, 100]) for _ in range(J)]
        B = [rnd.choice([255, 254, 0, 200]) for _ in range(J)]
        alpha = f32(rnd.random())
        lay, off = {}, BUF
        for name, arr in (("qB", qB), ("qA", qA), ("sB", sB), ("sA", sA), ("tB", tB), ("tA", tA)):
            lay[name] = off; mu.mem_write(off, b"".join(a.tobytes() for a in arr)); off += 0x100
        for name, arr in (("F", F), ("A", A), ("B", B)):
            lay[name] = off; mu.mem_write(off, bytes(arr) + b"\0" * 12); off += 0x100
        for name in ("oq", "o1", "o2", "ow"):
            lay[name] = off; mu.mem_write(off, b"\xcd" * 0x100); off += 0x100
        mu.reg_write(UC_X86_REG_XMM1, fbits(alpha))
        e.call(fn, J, fbits(alpha), lay["F"], lay["qB"], lay["sB"], lay["tB"], lay["A"], lay["qA"],
               lay["sA"], lay["tA"], lay["B"], lay["oq"], lay["o1"], lay["o2"], lay["ow"])
        oq = np.frombuffer(bytes(mu.mem_read(lay["oq"], 64)), np.float32).reshape(4, 4)
        o1 = np.frombuffer(bytes(mu.mem_read(lay["o1"], 64)), np.float32).reshape(4, 4)
        o2 = np.frombuffer(bytes(mu.mem_read(lay["o2"], 64)), np.float32).reshape(4, 4)
        ow = bytes(mu.mem_read(lay["ow"], 4))
        for j in range(J):
            t = f32(f32(f32(f32(F[j]) * c255) * f32(B[j])) * f32(alpha * c255))
            a = qA[j].astype(np.float64)
            if sub: a = a * np.array([-1, -1, -1, 1])
            p = qmul(a, qB[j].astype(np.float64))
            d = float(np.dot(qB[j].astype(np.float64), p))
            s = -t if d < 0 else t
            q = (qB[j] - qB[j] * t) + p * s
            q = q / math.sqrt(float(np.dot(q, q)))
            sa = sA[j].astype(np.float64).copy(); sa[3] = 1.0 if sub else 0.0
            m = (1.0 / sa) if sub else sa
            want1 = sB[j] - (sB[j] - sB[j] * m) * t
            want2 = (tB[j] - tA[j] * t) if sub else (tB[j] + tA[j] * t)
            err = max(np.max(np.abs(oq[j] - q)), np.max(np.abs(o1[j] - want1)), np.max(np.abs(o2[j] - want2)) / 50)
            worst = max(worst, float(err)); n += 1
            if ow[j] != A[j]: wbad += 1
    print(f"{'sub' if sub else 'add'}: {n} joints, worst abs err {worst:.2e}, weight byte != base byte: {wbad}")


if __name__ == "__main__":
    e = Engine(sys.argv[1])
    check_additive(e, 0x141736c90, 0)
    check_additive(e, 0x14173f650, 1)
    check_leaf_time(e)
    check_alpha(e)
    check_ease(e)
    check_sample(e)
    check_lerp(e)
