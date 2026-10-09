"""Run the anim web blend-space functions of the user's own DOOMx64.exe under Unicorn and compare them with the
model in crates/rancher_sim/src/animweb.rs (sort_space / bracket / blenda_pick). Offline, read-only.

usage: animweb_space_oracle.py <exe>
Checks:
  sort     0x1415d39f0 (N = 1, with and without the 'y' mirror): lower-bound insert + mirrored ends
  bracket  0x1415d1e10 sorted path (param_2 = 1), fresh and with a cached bracket
  blenda   0x1415d1d20 nearest-delta pick and alpha
  cases    prints the exe's results for the zombie walk coordinates (pasted into the Rust install test)
"""
import random
import struct
import sys

import numpy as np

sys.path.insert(0, __file__.rsplit("\\", 1)[0].rsplit("/", 1)[0])
from hdp_emu import Engine, HEAP

f32 = np.float32
BUF = HEAP + 0xA00000
LEAF0 = 0x7000_0000_0000  # fake leaf pointers: LEAF0 + index


def fbits(x):
    return struct.unpack("<I", struct.pack("<f", float(x)))[0]


def rd_f(mu, a):
    return struct.unpack("<f", mu.mem_read(a, 4))[0]


def rd_q(mu, a):
    return struct.unpack("<Q", mu.mem_read(a, 8))[0]


def idlist(mu, at, data_at, data, cap, count=None):
    """idList {ptr, int count, int cap, ...}: writes the header at `at` and the payload at `data_at`."""
    mu.mem_write(at, struct.pack("<QiiI", data_at, len(data) // 4 if count is None else count, cap, 0x1050001))
    if data:
        mu.mem_write(data_at, data)


# ---- models (the Rust code's formulas) ----

def model_sort(cs, y):
    out = []
    for i, c in enumerate(cs):
        k = sum(1 for e in out if e[1] < c)
        out.insert(k, (i, c))
    if y and len(out) > 1:
        n = len(out)
        mx = max(abs(out[0][1]), abs(out[-1][1]))
        extra, i, j = [], 0, n - 1
        while i < j:
            a, b = abs(out[i][1]), abs(out[j][1])
            if a != mx and b != mx:
                break
            if a != b:
                e = out[j] if a < b else out[i]
                extra.append((e[0], f32(-e[1])))
            i += 1
            j -= 1
        for e in extra:
            k = sum(1 for x in out if x[1] < e[1])
            out.insert(k, e)
    return out


def model_alpha(x, lo, hi):
    if f32(0.0) < f32(hi - lo):
        v = x
        if hi <= v:
            v = hi
        if v <= lo:
            v = lo
        return f32(f32(v - lo) / f32(hi - lo))
    return f32(0.0)


def model_bracket(cs, x, cache):
    if cache is not None:
        lo, hi = cache
        if cs[lo] <= x <= cs[hi]:
            return lo, hi, model_alpha(x, cs[lo], cs[hi])
    for i in range(1, len(cs)):
        if x <= cs[i]:
            return i - 1, i, model_alpha(x, cs[i - 1], cs[i])
    n = len(cs) - 1
    return n, n, f32(0.0)


def model_blenda(cs, x):
    sel = 0
    if len(cs) > 1:
        d = int(abs(f32(cs[0] - x)))
        for i in range(1, len(cs)):
            di = int(abs(f32(cs[i] - x)))
            if d <= di:
                break
            d, sel = di, i
    c = cs[sel]
    lo, hi = (c, f32(0.0)) if c < 0 else (f32(0.0), c)
    v = x
    if hi <= v:
        v = hi
    if v <= lo:
        v = lo
    return sel, f32(v / c)


# ---- exe calls ----

def exe_sort(e, cs, y):
    mu = e.mu
    n = len(cs)
    node = BUF
    mu.mem_write(node, b"\0" * 0x200)
    keys, coords, leaves = BUF + 0x1000, BUF + 0x2000, BUF + 0x3000
    okeys, ocoords, oleaves, ocount = BUF + 0x4000, BUF + 0x5000, BUF + 0x6000, BUF + 0x7000
    idlist(mu, keys, keys + 0x40, struct.pack(f"<{n}i", *range(n)), 64)
    idlist(mu, coords, coords + 0x40, struct.pack(f"<{n}f", *cs), 64)
    idlist(mu, leaves, leaves + 0x40, struct.pack(f"<{n}Q", *[LEAF0 + i for i in range(n)]), 64, count=n)
    for h in (okeys, ocoords, oleaves):
        idlist(mu, h, h + 0x40, b"", 64, count=0)
    mu.mem_write(ocount, b"\0" * 4)
    e.call(0x1415d39f0, node, 1, keys, coords, leaves, y, okeys, ocoords, oleaves, ocount)
    m = struct.unpack("<i", mu.mem_read(oleaves + 8, 4))[0]
    out = []
    for k in range(m):
        out.append((rd_q(mu, oleaves + 0x40 + 8 * k) - LEAF0, f32(rd_f(mu, ocoords + 0x40 + 4 * k))))
    return out


def exe_bracket(e, cs, x, cache):
    mu = e.mu
    n = len(cs)
    node = BUF
    mu.mem_write(node, b"\0" * 0x200)
    xp, cachep, leaves, coords, outs = BUF + 0x800, BUF + 0x900, BUF + 0x1000, BUF + 0x2000, BUF + 0x3000
    mu.mem_write(xp, struct.pack("<f", float(x)))
    mu.mem_write(node + 0x80, struct.pack("<Q", xp))
    mu.mem_write(node + 0x13C, b"\x01")
    lo, hi = cache if cache is not None else (0, 0)
    mu.mem_write(cachep, struct.pack("<ii", lo, hi))
    # cache idList at +200: count 2 = valid; count 0 with capacity 2 = the first-use resize without allocating.
    mu.mem_write(node + 200, struct.pack("<Qii", cachep, 2 if cache is not None else 0, 2))
    idlist(mu, leaves, leaves + 0x40, struct.pack(f"<{n}Q", *[LEAF0 + i for i in range(n)]), 64, count=n)
    idlist(mu, coords, coords + 0x40, struct.pack(f"<{n}f", *cs), 64)
    mu.mem_write(outs, b"\0" * 0x20)
    e.call(0x1415d1e10, node, 1, leaves, coords, outs, outs + 8, outs + 0x10)
    return rd_q(mu, outs) - LEAF0, rd_q(mu, outs + 8) - LEAF0, f32(rd_f(mu, outs + 0x10))


def exe_blenda(e, cs, x):
    mu = e.mu
    n = len(cs)
    node = BUF
    mu.mem_write(node, b"\0" * 0x200)
    coords, leaves, outs = BUF + 0x1000, BUF + 0x2000, BUF + 0x3000
    mu.mem_write(node + 0x70, struct.pack("<f", float(x)))
    mu.mem_write(coords, struct.pack(f"<{n}f", *cs))
    mu.mem_write(leaves, struct.pack(f"<{n}Q", *[LEAF0 + i for i in range(n)]))
    mu.mem_write(node + 0x40, struct.pack("<Q", coords))
    mu.mem_write(node + 0x58, struct.pack("<Qi", leaves, n))
    mu.mem_write(outs, b"\0" * 0x10)
    e.call(0x1415d1d20, node, outs, outs + 8)
    return rd_q(mu, outs) - LEAF0, f32(rd_f(mu, outs + 8))


def same(a, b):
    return [(i, fbits(c)) for i, c in a] == [(i, fbits(c)) for i, c in b]


def main():
    e = Engine(sys.argv[1])
    rnd = random.Random(7)
    pool = [0, 45, -45, 90, -90, 55, -55, 130, -130, 180, -180, 140, -140, 70, -70, 30, 15]

    bad = n = 0
    for _ in range(400):
        cs = [f32(rnd.choice(pool)) for _ in range(rnd.randint(1, 14))]
        for y in (0, 1):
            got, want = exe_sort(e, cs, y), model_sort(cs, y)
            n += 1
            if not same(got, want):
                bad += 1
                if bad < 4:
                    print("sort mismatch", y, cs, got, want)
    print(f"sort: {n - bad}/{n} identical")

    bad = n = 0
    for _ in range(3000):
        cs = sorted(f32(rnd.choice(pool)) for _ in range(rnd.randint(2, 12)))
        x = f32(rnd.choice([rnd.uniform(-200, 200), rnd.choice(pool)]))
        cache = None
        if rnd.random() < 0.5:
            lo = rnd.randrange(len(cs))
            cache = (lo, min(len(cs) - 1, lo + rnd.choice([0, 1])))
        got, want = exe_bracket(e, cs, x, cache), model_bracket(cs, x, cache)
        n += 1
        if got[:2] != want[:2] or fbits(got[2]) != fbits(want[2]):
            bad += 1
            if bad < 4:
                print("bracket mismatch", cs, x, cache, got, want)
    print(f"bracket: {n - bad}/{n} bit-exact")

    bad = n = 0
    for _ in range(3000):
        cs = [f32(rnd.choice([-70, 70, -30, 30, 45, -45, 90, -90, 10])) for _ in range(rnd.randint(1, 4))]
        x = f32(rnd.uniform(-120, 120))
        got, want = exe_blenda(e, cs, x), model_blenda(cs, x)
        n += 1
        if got[0] != want[0] or fbits(got[1]) != fbits(want[1]):
            bad += 1
            if bad < 4:
                print("blenda mismatch", cs, x, got, want)
    print(f"blenda: {n - bad}/{n} bit-exact")

    # Zombie hands_combat/walk, randomTags variant 0 (forward 0/45/-45 + the shared strafe / back aliases).
    zc = [f32(c) for c in (0, 45, -45, 90, 55, 130, -90, -55, -130, 180, 140, -140)]
    s = exe_sort(e, zc, 1)
    print("zombie sorted:", [(i, float(c)) for i, c in s])
    cs = [c for _, c in s]
    # Init resolves at coordinate 0 (fresh), leaving its bracket cached; the first frame then resolves x.
    lo0, hi0, _ = exe_bracket(e, cs, f32(0), None)
    for x in (0, 30, 45, -45, 180, -180, 100, -100):
        lo, hi, a = exe_bracket(e, cs, f32(x), (lo0, hi0))
        print(f"zombie x={x}: lo {s[lo][0]} ({float(cs[lo])}) hi {s[hi][0]} ({float(cs[hi])}) alpha {float(a)!r}")
    for x in (0, -35, -70, 35, 70, 100):
        i, a = exe_blenda(e, [f32(-70), f32(70)], f32(x))
        print(f"blenda(-70, 70) x={x}: delta {i} alpha {float(a)!r}")


if __name__ == "__main__":
    main()
