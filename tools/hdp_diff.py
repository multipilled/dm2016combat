"""Differential tests: the engine's HD Photo transform operators (run from the user's own DOOMx64.exe
under Unicorn) against Python ports of jxrlib's C operators. Offline and read-only.

usage: hdp_diff.py <exe> <test>   tests: stage2 stage1 post
"""
import random, struct, sys
import pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_64
from unicorn.x86_const import *

exe = sys.argv[1]
pe = pefile.PE(exe, fast_load=True)
BASE = pe.OPTIONAL_HEADER.ImageBase
image = pe.get_memory_mapped_image()
size = (max(len(image), pe.OPTIONAL_HEADER.SizeOfImage) + 0xfff) & ~0xfff
mu = Uc(UC_ARCH_X86, UC_MODE_64)
mu.mem_map(BASE, size)
mu.mem_write(BASE, image)
STACK, STACK_SZ = 0x7f0000000000, 0x100000
BUF = 0x20000000
mu.mem_map(STACK, STACK_SZ)
mu.mem_map(BUF, 0x100000)

# Static constructors that fill the HD Photo SIMD constant tables in .bss (0x145b7c8a0..0x145b7c990).
CONST_INITS = [0x1402507d0, 0x1402507e0, 0x140250800, 0x140250810, 0x140250830, 0x140250840, 0x140250860,
               0x140250880, 0x1402508a0, 0x1402508c0, 0x1402508e0, 0x140250900, 0x140250920, 0x140250940, 0x140250960]
def run_const_inits(mu, stack_top):
    from unicorn.x86_const import UC_X86_REG_RSP
    ret = stack_top - 0x800
    for fn in CONST_INITS:
        sp = stack_top - 0x1000
        mu.mem_write(sp, struct.pack("<Q", ret))
        mu.reg_write(UC_X86_REG_RSP, sp)
        mu.emu_start(fn, ret, count=1000)
run_const_inits(mu, STACK + STACK_SZ)
RET = STACK + 0x1000
REGS = [UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9]


def call(fn, *args):
    sp = STACK + STACK_SZ - 0x1000
    mu.mem_write(sp, struct.pack("<Q", RET))
    for k, a in enumerate(args):
        if k < 4:
            mu.reg_write(REGS[k], a)
        else:
            mu.mem_write(sp + 8 * (k + 1), struct.pack("<Q", a))
    mu.reg_write(UC_X86_REG_RSP, sp)
    mu.emu_start(fn, RET, count=10_000_000)


def put(vals, at=BUF):
    mu.mem_write(at, struct.pack(f"<{len(vals)}h", *vals))


def get(n, at=BUF):
    return list(struct.unpack(f"<{n}h", mu.mem_read(at, 2 * n)))


def s16(x):
    x &= 0xffff
    return x - 0x10000 if x & 0x8000 else x


TRACK = [0]  # largest |shift operand| seen (the engine keeps every lane value in int16)


def sh(x, k):
    """x >> k. The engine's SIMD code is exact while every lane value fits in int16 (it avoids
    overflow in roundings, e.g. (a + 1) >> 1 via pand), so the model uses exact integers and only
    records the magnitude."""
    if abs(x) > TRACK[0]:
        TRACK[0] = abs(x)
    return x >> k


# ---- jxrlib operators (PixelI arithmetic) ----
def dct2x2dn(p, a_, b_, c_, d_):
    a, b, C, d = p[a_], p[b_], p[c_], p[d_]
    a += d; b -= C; t = sh(a - b, 1); c = t - d; d = t - C; a -= d; b += c
    p[a_], p[b_], p[c_], p[d_] = a, b, c, d


def dct2x2up(p, a_, b_, c_, d_):
    a, b, C, d = p[a_], p[b_], p[c_], p[d_]
    a += d; b -= C; t = sh(a - b + 1, 1); c = t - d; d = t - C; a -= d; b += c
    p[a_], p[b_], p[c_], p[d_] = a, b, c, d


def irot2(a, b):
    a -= sh(b * 3 + 4, 3)
    b += sh(a * 3 + 4, 3)
    return a, b


def irot1(a, b):
    a -= sh(b + 1, 1)
    b += sh(a + 1, 1)
    return a, b


def invodd(p, a_, b_, c_, d_):
    a, b, c, d = p[a_], p[b_], p[c_], p[d_]
    b += d; a -= c; d -= sh(b, 1); c += sh(a + 1, 1)
    a, b = irot2(a, b); c, d = irot2(c, d)
    c -= sh(b + 1, 1); d = (sh(a + 1, 1)) - d; b += c; a -= d
    p[a_], p[b_], p[c_], p[d_] = a, b, c, d


def invoddodd(p, a_, b_, c_, d_):
    a, b, c, d = p[a_], p[b_], p[c_], p[d_]
    d += a; c -= b; t1 = sh(d, 1); a -= t1; t2 = sh(c, 1); b += t2
    a -= sh(b * 3 + 3, 3); b += sh(a * 3 + 3, 2); a -= sh(b * 3 + 4, 3)
    b -= t2; a += t1; c += b; d -= a
    p[a_], p[b_], p[c_], p[d_] = a, -b, -c, d


def invoddoddpost(p, a_, b_, c_, d_):
    a, b, c, d = p[a_], p[b_], p[c_], p[d_]
    d += a; c -= b; t1 = sh(d, 1); a -= t1; t2 = sh(c, 1); b += t2
    a -= sh(b * 3 + 6, 3); b += sh(a * 3 + 2, 2); a -= sh(b * 3 + 4, 3)
    b -= t2; a += t1; c += b; d -= a
    p[a_], p[b_], p[c_], p[d_] = a, b, c, d


def hstdec1(p, a_, d_):
    a, d = p[a_], p[d_]
    a += d; d = (sh(a, 1)) - d; a += sh(d * 3, 3); d += sh(a * 3, 4)
    p[a_], p[d_] = a, d


def hstdec(p, a_, b_, c_, d_):
    a, b, c, d = p[a_], p[b_], p[c_], p[d_]
    b -= c; a += sh(d * 3 + 4, 3); d -= sh(b, 1); c = (sh(a - b, 1)) - c
    p[c_] = d; p[d_] = c; p[a_] = a - c; p[b_] = b + d


def stage1(p, o):
    dct2x2up(p, o + 0, o + 1, o + 2, o + 3)
    invodd(p, o + 5, o + 4, o + 7, o + 6)
    invodd(p, o + 10, o + 8, o + 11, o + 9)
    invoddodd(p, o + 15, o + 14, o + 13, o + 12)
    for k in range(4):
        dct2x2dn(p, o + k, o + 4 + k, o + 8 + k, o + 12 + k)


def stage2(p, o=0):
    invodd(p, o + 32, o + 48, o + 96, o + 112)
    invodd(p, o + 128, o + 192, o + 144, o + 208)
    invoddodd(p, o + 160, o + 224, o + 176, o + 240)
    dct2x2up(p, o + 0, o + 64, o + 16, o + 80)
    for q in ((0, 192, 48, 240), (64, 128, 112, 176), (16, 208, 32, 224), (80, 144, 96, 160)):
        dct2x2dn(p, *(o + x for x in q))


def post4x4stage1split(p, p0, p1, off):
    p2 = p0 + 72 - off
    p3 = p1 + 64 - off
    p0 += 12
    p1 += 4
    for k in range(4):
        dct2x2dn(p, p0 + k, p2 + k, p1 + k, p3 + k)
    invoddoddpost(p, p3 + 0, p3 + 1, p3 + 2, p3 + 3)
    p[p1 + 2], p[p1 + 3] = irot1(p[p1 + 2], p[p1 + 3])
    p[p1 + 0], p[p1 + 1] = irot1(p[p1 + 0], p[p1 + 1])
    p[p2 + 1], p[p2 + 3] = irot1(p[p2 + 1], p[p2 + 3])
    p[p2 + 0], p[p2 + 2] = irot1(p[p2 + 0], p[p2 + 2])
    for k in range(4):
        hstdec1(p, p0 + k, p3 + k)
    for k in range(4):
        hstdec(p, p0 + k, p2 + k, p1 + k, p3 + k)


# ---- engine macroblock inverse transform (FUN_141b3e0f0): jxrlib's invTransformMacroblock for
# 444 / overlap 1 plus the corner filters; buffer offsets as in jxrlib (int16 lanes in the engine).
def post4(p, a_, b_, c_, d_):
    a, b, c, d = p[a_], p[b_], p[c_], p[d_]
    a += d; b += c
    d -= sh(a + 1, 1); c -= sh(b + 1, 1)
    c -= sh(d + 1, 1); d += sh(c + 1, 1)
    d += sh(a + 1, 1); c += sh(b + 1, 1)
    a -= d - (sh(d * 3 + 16, 5)); b -= c - (sh(c * 3 + 16, 5))
    d += sh(a * 3 + 8, 4); c += sh(b * 3 + 8, 4)
    a += sh(d * 3 + 16, 5); b += sh(c * 3 + 16, 5)
    p[a_], p[b_], p[c_], p[d_] = a, b, c, d

def hst1_id(p, a_, d_):
    a, d = p[a_], p[d_]
    a += d; d = (sh(a, 1)) - d
    a += sh(3 * d + 8, 4); d += sh(3 * a + 4, 5); a += sh(3 * d + 8, 4)
    p[a_], p[d_] = a, d

def hstdec_id(p, a_, b_, c_, d_):
    a, b, c, d = p[a_], p[b_], p[c_], p[d_]
    b -= c; d -= sh(b, 1); c = (sh(a - b, 1)) - c
    p[c_] = d; p[d_] = c; p[a_] = a - c; p[b_] = b + d

def split(p, p0, p1):
    p2 = p0 + 72; p3 = p1 + 64; p0 += 12; p1 += 4
    for k in range(4):
        dct2x2dn(p, p0 + k, p2 + k, p1 + k, p3 + k)
    invoddoddpost(p, p3 + 0, p3 + 1, p3 + 2, p3 + 3)
    p[p1 + 2], p[p1 + 3] = irot1(p[p1 + 2], p[p1 + 3])
    p[p1 + 0], p[p1 + 1] = irot1(p[p1 + 0], p[p1 + 1])
    p[p2 + 1], p[p2 + 3] = irot1(p[p2 + 1], p[p2 + 3])
    p[p2 + 0], p[p2 + 2] = irot1(p[p2 + 0], p[p2 + 2])
    for k in range(4):
        hst1_id(p, p0 + k, p3 + k)
    for k in range(4):
        hstdec_id(p, p0 + k, p2 + k, p1 + k, p3 + k)

def model(p, P0, P1, left, right, top, bottom, corners):
    if not (bottom or right):
        stage2(p, P1)
    if not top:
        for j in range(32 if left else -96, 32 if right else 160, 64):
            stage1(p, P0 + j); stage1(p, P0 + j + 16)
    if not bottom:
        for j in range(0 if left else -128, 0 if right else 128, 64):
            stage1(p, P1 + j); stage1(p, P1 + j + 16)
    if left or right:
        j = 10 if left else -50
        if not top:
            q = P0 + 16 + j
            post4(p, q, q - 2, q + 6, q + 8); post4(p, q + 1, q - 1, q + 7, q + 9)
            post4(p, q + 16, q + 14, q + 22, q + 24); post4(p, q + 17, q + 15, q + 23, q + 25)
        if not bottom:
            q = P1 + j
            post4(p, q, q - 2, q + 6, q + 8); post4(p, q + 1, q - 1, q + 7, q + 9)
        if not (top or bottom):
            post4(p, P0 + 48 + j, P0 + 46 + j, P1 - 10 + j, P1 - 8 + j)
            post4(p, P0 + 49 + j, P0 + 47 + j, P1 - 9 + j, P1 - 7 + j)
    rng = range(0 if left else -192, -64 if right else 64, 64)
    if top:
        for j in rng:
            q = P1 + j
            post4(p, q + 5, q + 4, q + 64, q + 65); post4(p, q + 7, q + 6, q + 66, q + 67)
            split(p, P1 + j, P1 + j + 16)
    elif bottom:
        for j in rng:
            split(p, P0 + 16 + j, P0 + 32 + j); split(p, P0 + 32 + j, P0 + 48 + j)
            q = P0 + 48 + j
            post4(p, q + 15, q + 14, q + 74, q + 75); post4(p, q + 13, q + 12, q + 72, q + 73)
    else:
        for j in rng:
            split(p, P0 + 16 + j, P0 + 32 + j); split(p, P0 + 32 + j, P0 + 48 + j)
            split(p, P0 + 48 + j, P1 + j); split(p, P1 + j, P1 + j + 16)
    if corners:
        for cond, q in ((top and left, P1), (top and right, P1 - 60), (bottom and left, P0 + 56), (bottom and right, P0 - 4)):
            if cond:
                post4(p, q, q + 1, q + 2, q + 3)


def rnd(n, lo=-int(__import__("os").environ.get("RMAG","800")), hi=int(__import__("os").environ.get("RMAG","800"))):
    return [random.randint(lo, hi) for _ in range(n)]


test = sys.argv[2]
random.seed(1)
if test == "stage2":
    bad = 0
    for trial in range(200):
        v = rnd(256)
        put(v)
        call(0x141b3dbb0, BUF)
        got = get(256)
        ref = list(v)
        stage2(ref)
        ref = [s16(x) for x in ref]
        if got != ref:
            bad += 1
            if bad <= 2:
                d = [(i, ref[i], got[i]) for i in range(256) if ref[i] != got[i]]
                print("mismatch", len(d), d[:12])
    print("stage2 mismatching trials:", bad, "/ 200")
elif test == "stage1":
    # FUN_141b3d360(a, b): find which 4x4 blocks it transforms.
    v = rnd(512)
    put(v)
    call(0x141b3d360, BUF + 2 * 128, BUF + 2 * 256)
    got = get(512)
    changed = sorted({i for i in range(512) if got[i] != v[i]})
    blocks = sorted({(i // 16) * 16 for i in changed})
    print("changed blocks (short offsets):", blocks)
    ref = list(v)
    for b in blocks:
        stage1(ref, b)
    ref = [s16(x) for x in ref]
    d = [(i, ref[i], got[i]) for i in range(512) if ref[i] != got[i]]
    print("stage1 mismatches:", len(d), d[:12])
    bad = 0
    for trial in range(200):
        v = rnd(512)
        put(v)
        call(0x141b3d360, BUF + 2 * 128, BUF + 2 * 256)
        got = get(512)
        ref = list(v)
        for b in blocks:
            stage1(ref, b)
        if got != [s16(x) for x in ref]:
            bad += 1
    print("stage1 mismatching trials:", bad, "/ 200")
elif test == "post":
    # FUN_141b3f800(a, b, c, d, e, f, g, h, stride): 4x4 post filters, two positions `stride` apart.
    P0, P1 = 512, 1536  # short offsets of the two macroblock rows inside the buffer
    bad = 0
    for trial in range(300):
        v = rnd(2048)
        put(v)
        A = lambda o: BUF + 2 * o
        call(0x141b3f800, A(P0 - 176), A(P0 - 160), A(P0 - 160), A(P0 - 144), A(P0 - 144), A(P1 - 192), A(P1 - 192), A(P1 - 176), 64)
        got = get(2048)
        ref = list(v)
        for j in (-192, -128):
            post4x4stage1split(ref, P0 + 16 + j, P0 + 32 + j, 0)
            post4x4stage1split(ref, P0 + 32 + j, P0 + 48 + j, 0)
            post4x4stage1split(ref, P0 + 48 + j, P1 + j, 0)
            post4x4stage1split(ref, P1 + j, P1 + j + 16, 0)
        ref = [s16(x) for x in ref]
        if got != ref:
            bad += 1
            if bad <= 2:
                d = [(i, ref[i], got[i]) for i in range(2048) if ref[i] != got[i]]
                print("mismatch", len(d), d[:10])
    print("post mismatching trials:", bad, "/ 300")
elif test == "postmap":
    P0, P1 = 512, 1536
    v = rnd(2048)
    put(v)
    A = lambda o: BUF + 2 * o
    call(0x141b3f800, A(P0 - 176), A(P0 - 160), A(P0 - 160), A(P0 - 144), A(P0 - 144), A(P1 - 192), A(P1 - 192), A(P1 - 176), 64)
    got = get(2048)
    idc = sorted(i for i in range(2048) if got[i] != v[i])
    ref = list(v)
    for j in (-192, -128):
        post4x4stage1split(ref, P0 + 16 + j, P0 + 32 + j, 0)
        post4x4stage1split(ref, P0 + 32 + j, P0 + 48 + j, 0)
        post4x4stage1split(ref, P0 + 48 + j, P1 + j, 0)
        post4x4stage1split(ref, P1 + j, P1 + j + 16, 0)
    jxc = sorted(i for i in range(2048) if ref[i] != v[i])
    rel = lambda l: [(("p0" if i < 1024 else "p1"), i - (P0 if i < 1024 else P1)) for i in l]
    print("id changed ", len(idc), rel(idc))
    print("jxr changed", len(jxc), rel(jxc))
    print("only id ", rel(sorted(set(idc) - set(jxc))))
    print("only jxr", rel(sorted(set(jxc) - set(idc))))
elif test == "postvar":
    import itertools
    P0, P1 = 512, 1536
    A = lambda o: BUF + 2 * o
    trials = []
    for t in range(20):
        v = rnd(2048)
        put(v)
        call(0x141b3f800, A(P0 - 176), A(P0 - 160), A(P0 - 160), A(P0 - 144), A(P0 - 144), A(P1 - 192), A(P1 - 192), A(P1 - 176), 64)
        trials.append((v, get(2048)))

    def hst1_r(p, a_, d_, r1, r2):
        a, d = p[a_], p[d_]
        a += d; d = (a >> 1) - d; a += (d * 3 + r1) >> 3; d += (a * 3 + r2) >> 4
        p[a_], p[d_] = a, d

    def split_var(p, p0, p1, off, opts):
        p2 = p0 + 72 - off; p3 = p1 + 64 - off; p0 += 12; p1 += 4
        for k in range(4):
            (dct2x2up if opts["up"] else dct2x2dn)(p, p0 + k, p2 + k, p1 + k, p3 + k)
        (invoddoddpost if opts["post"] else invoddodd)(p, p3 + 0, p3 + 1, p3 + 2, p3 + 3)
        rot = irot1 if opts["rot1"] else irot2
        p[p1 + 2], p[p1 + 3] = rot(p[p1 + 2], p[p1 + 3])
        p[p1 + 0], p[p1 + 1] = rot(p[p1 + 0], p[p1 + 1])
        p[p2 + 1], p[p2 + 3] = rot(p[p2 + 1], p[p2 + 3])
        p[p2 + 0], p[p2 + 2] = rot(p[p2 + 0], p[p2 + 2])
        if opts["hst1"]:
            for k in range(4):
                hst1_r(p, p0 + k, p3 + k, opts["r1"], opts["r2"])
        if opts["hst"]:
            for k in range(4):
                hstdec(p, p0 + k, p2 + k, p1 + k, p3 + k)

    keys = dict(up=[0, 1], post=[0, 1], rot1=[0, 1], hst1=[0, 1], hst=[0, 1], r1=[0, 4], r2=[0, 8])
    best = []
    for combo in itertools.product(*keys.values()):
        opts = dict(zip(keys.keys(), combo))
        score = 0
        for v, got in trials:
            ref = list(v)
            for j in (-192, -128):
                split_var(ref, P0 + 16 + j, P0 + 32 + j, 0, opts)
                split_var(ref, P0 + 32 + j, P0 + 48 + j, 0, opts)
                split_var(ref, P0 + 48 + j, P1 + j, 0, opts)
                split_var(ref, P1 + j, P1 + j + 16, 0, opts)
            score += sum(1 for i in range(2048) if s16(ref[i]) == got[i])
        best.append((score, opts))
    best.sort(key=lambda x: -x[0])
    for s, o in best[:6]:
        print(s, "/", 20 * 2048, o)
elif test == "post2":
    P0, P1 = 512, 1536
    A = lambda o: BUF + 2 * o

    def hst1_id(p, a_, d_):
        a, d = p[a_], p[d_]
        a += d; d = (a >> 1) - d
        a += (3 * d + 8) >> 4
        d += (3 * a + 4) >> 5
        a += (3 * d + 8) >> 4
        p[a_], p[d_] = a, d

    def hstdec_id(p, a_, b_, c_, d_):
        a, b, c, d = p[a_], p[b_], p[c_], p[d_]
        b -= c; d -= b >> 1; c = ((a - b) >> 1) - c
        p[c_] = d; p[d_] = c; p[a_] = a - c; p[b_] = b + d

    def split_id(p, p0, p1, off):
        p2 = p0 + 72 - off; p3 = p1 + 64 - off; p0 += 12; p1 += 4
        for k in range(4):
            dct2x2dn(p, p0 + k, p2 + k, p1 + k, p3 + k)
        invoddoddpost(p, p3 + 0, p3 + 1, p3 + 2, p3 + 3)
        p[p1 + 2], p[p1 + 3] = irot1(p[p1 + 2], p[p1 + 3])
        p[p1 + 0], p[p1 + 1] = irot1(p[p1 + 0], p[p1 + 1])
        p[p2 + 1], p[p2 + 3] = irot1(p[p2 + 1], p[p2 + 3])
        p[p2 + 0], p[p2 + 2] = irot1(p[p2 + 0], p[p2 + 2])
        for k in range(4):
            hst1_id(p, p0 + k, p3 + k)
        for k in range(4):
            hstdec_id(p, p0 + k, p2 + k, p1 + k, p3 + k)

    bad = 0
    for trial in range(300):
        v = rnd(2048)
        put(v)
        call(0x141b3f800, A(P0 - 176), A(P0 - 160), A(P0 - 160), A(P0 - 144), A(P0 - 144), A(P1 - 192), A(P1 - 192), A(P1 - 176), 64)
        got = get(2048)
        ref = list(v)
        for j in (-192, -128):
            split_id(ref, P0 + 16 + j, P0 + 32 + j, 0)
            split_id(ref, P0 + 32 + j, P0 + 48 + j, 0)
            split_id(ref, P0 + 48 + j, P1 + j, 0)
            split_id(ref, P1 + j, P1 + j + 16, 0)
        ref = [s16(x) for x in ref]
        if got != ref:
            bad += 1
            if bad <= 2:
                d = [(i, ref[i], got[i]) for i in range(2048) if ref[i] != got[i]]
                print("mismatch", len(d), d[:10])
    print("post2 mismatching trials:", bad, "/ 300")
elif test == "mb":
    # Whole-macroblock inverse transform driver FUN_141b3e0f0(pSC, col, row) for all 9 position cases,
    # one channel, against jxrlib's invTransformMacroblock (444, overlap 1) + the engine's corner filters.
    SC = BUF + 0x40000
    N = 4096  # shorts: row 0 buffer [0, 2048), row 1 buffer [2048, 4096)
    P0, P1 = 1024, 3072
    W = H = 8
    mu.mem_write(SC, b"\0" * 0x1000)
    mu.mem_write(SC + 0x4e8, struct.pack("<QQ", W, H))
    mu.mem_write(SC + 0x350, struct.pack("<i", 3))
    mu.mem_write(SC + 0x358, struct.pack("<Q", 1))
    mu.mem_write(SC + 0x478, struct.pack("<Q", 0))
    mu.mem_write(SC + 0x608, struct.pack("<Q", BUF + 2 * P0))
    mu.mem_write(SC + 0x688, struct.pack("<Q", BUF + 2 * P1))
    corners = "--nocorners" not in sys.argv
    for row in (0, 3, H):
        for col in (0, 3, W):
            bad = 0
            for trial in range(60):
                v = rnd(N)
                put(v)
                call(0x141b3e0f0, SC, col, row)
                got = get(N)
                ref = list(v)
                model(ref, P0, P1, col == 0, col == W, row == 0, row == H, corners)
                ref = [s16(x) for x in ref]
                if got != ref:
                    bad += 1
                    if bad == 1:
                        d = [(i - (P0 if i < 2048 else P1), "p0" if i < 2048 else "p1", ref[i], got[i]) for i in range(N) if ref[i] != got[i]]
                        print(f"  row {row} col {col}: {len(d)} diffs (ofs, buf, model, engine)", d[:16])
            print(f"row {row} col {col}: mismatching trials {bad} / 60")
