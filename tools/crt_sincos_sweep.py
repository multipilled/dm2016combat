#!/usr/bin/env python3
"""Acceptance check for the ported C runtime sinf / cosf (crates/rancher_sim/src/handlayers/crtmath.rs).

Runs the exe's own sinf (0x141ed3170) and cosf (0x141ed2c70) from the user's DOOMx64.exe under Unicorn on the
non-FMA branch (the FMA3 flag at 0x1521ace24 stays 0 because the C runtime start-up never runs) and compares
them bit for bit with the Rust port, evaluated in bulk by the ignored test `crtmath::tests::crt_sweep_eval`.
Offline and read-only; nothing from the exe is written anywhere. Exits 1 on any mismatch.

usage (from the repo root; needs cargo, Python with pefile + unicorn, and the exe):
  python3 -B tools/crt_sincos_sweep.py [<DOOMx64.exe>] [--quick] [--large]
  The exe comes from the argument, else $DOOM_EXE, else $DOOM_DIR/DOOMx64.exe.
  The default run is 1,234,669 arguments per function in 5 categories (fixed seed).
  --quick runs every category at 1/20 of its size; --large adds arguments above 4pi up to the float maximum
  (both large-reduction thresholds, NaNs), which the game code never passes.
Not part of cargo test or the unittest discovery: run it by hand after touching crtmath.rs.
"""
import math
import os
import random
import struct
import sys
import time

import pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_64
from unicorn.x86_const import UC_X86_REG_RSP, UC_X86_REG_XMM0, UC_X86_REG_GS_BASE

SINF, COSF = 0x141ed3170, 0x141ed2c70
FMA3_FLAG = 0x1521ace24
DEG2RAD = 0x3c8efa35  # the f32 the code multiplies degrees by (0.017453292)
STACK, STACK_SZ, TEB = 0x7f0000000000, 0x100000, 0x7e0000000000
RET = STACK + 0x1000


def f2u(f):
    return struct.unpack("<I", struct.pack("<f", f))[0]


def u2f(u):
    return struct.unpack("<f", struct.pack("<I", u))[0]


class Crt:
    def __init__(self, exe):
        pe = pefile.PE(exe, fast_load=True)
        base = pe.OPTIONAL_HEADER.ImageBase
        image = pe.get_memory_mapped_image()
        size = (max(len(image), pe.OPTIONAL_HEADER.SizeOfImage) + 0xfff) & ~0xfff
        mu = self.mu = Uc(UC_ARCH_X86, UC_MODE_64)
        mu.mem_map(base, size)
        mu.mem_write(base, image)
        mu.mem_map(STACK, STACK_SZ)
        mu.mem_map(TEB, 0x10000)
        mu.reg_write(UC_X86_REG_GS_BASE, TEB)
        flag = struct.unpack("<i", mu.mem_read(FMA3_FLAG, 4))[0]
        assert flag == 0, f"FMA3 flag is {flag}: this sweep covers the non-FMA branch only"
        self.sp = STACK + STACK_SZ - 0x10000
        mu.mem_write(self.sp, struct.pack("<Q", RET))

    def run(self, fn, u):
        mu = self.mu
        mu.reg_write(UC_X86_REG_RSP, self.sp)
        mu.reg_write(UC_X86_REG_XMM0, u)
        mu.emu_start(fn, RET, count=100_000)
        return mu.reg_read(UC_X86_REG_XMM0) & 0xffffffff


def categories(scale):
    deg2rad = u2f(DEG2RAD)
    f32 = lambda x: u2f(f2u(x))
    cats = {}
    # 1. degrees within +-720, as the code converts them: f32(degrees) * DEG2RAD in f32.
    n = 720_000 // scale
    cats["degrees +-720 (step 0.002 deg) * DEG2RAD"] = [
        f2u(f32(f32(-720.0 + 1440.0 * k / n) * deg2rad)) for k in range(n + 1)]
    # 2. bob phases within +-2pi.
    n = 200_000 // scale
    cats["bob phases +-2pi"] = [f2u(f32(-2 * math.pi + 4 * math.pi * k / n)) for k in range(n + 1)]
    # 3. +-0, subnormals and tiny normals (below 1e-3).
    sub = list(range(1, 0x800000, 997 * scale))
    tiny = list(range(0x800000, f2u(1e-3), 20011 * scale))
    cats["+-0, subnormals, tiny"] = [0, 0x80000000] + sub + [u | 0x80000000 for u in sub] + tiny + \
        [u | 0x80000000 for u in tiny]
    # 4. multiples of pi/2 up to 4pi (the 720 degree range), each with +-16 neighbouring floats.
    mult = []
    for k in range(-8, 9):
        for c in {f2u(f32(k * math.pi / 2)), f2u(f32(f32(k * 90.0) * deg2rad))}:
            for d in range(-16, 17):
                if c in (0, 0x80000000):
                    mult += [d if d >= 0 else 0x80000000 | -d]
                else:
                    mult.append(c + d)
    cats["multiples of pi/2 (k=-8..8) +-16 floats"] = sorted(set(mult))
    # 5. fixed-seed random floats within +-4pi.
    rng = random.Random(20261010)
    n = 200_000 // scale
    cats["random +-4pi (seed 20261010)"] = [f2u(f32(rng.uniform(-4 * math.pi, 4 * math.pi))) for _ in range(n)]
    return cats


def large(scale):
    """Arguments the game code never passes: above 4pi up to the float maximum, both large-reduction thresholds
    (cosf 500000, sinf 16000000) with their neighbours, and NaNs."""
    rng = random.Random(20261011)
    xs = [rng.randrange(f2u(4 * math.pi), 0x7f800000) for _ in range(200_000 // scale)]
    for t in (500000.0, 16000000.0):
        xs += [f2u(t) + d for d in range(-64, 65)]
    xs += [0x7fc00000, 0xffc00000, 0x7f800001, 0x7fa00000]
    return xs + [u | 0x80000000 for u in xs]


def port(inputs):
    """{input bits: (sinf bits, cosf bits)} from the Rust port, through the ignored bulk test."""
    import subprocess
    import tempfile
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    with tempfile.TemporaryDirectory() as d:
        i, o = os.path.join(d, "in.txt"), os.path.join(d, "out.txt")
        with open(i, "w") as f:
            f.write("".join(f"{u:08x}\n" for u in inputs))
        env = dict(os.environ, CRT_SWEEP_IN=i, CRT_SWEEP_OUT=o)
        subprocess.run(["cargo", "test", "-q", "-p", "rancher_sim", "--release", "--lib", "crt_sweep_eval", "--",
                        "--ignored", "--exact", "handlayers::crtmath::tests::crt_sweep_eval"],
                       cwd=root, env=env, check=True, stdout=subprocess.DEVNULL)
        with open(o) as f:
            rows = [tuple(int(h, 16) for h in line.split()) for line in f]
    return dict(zip(inputs, rows))


def main():
    args = sys.argv[1:]
    scale = 20 if "--quick" in args else 1
    cats = categories(scale)
    if "--large" in args:
        cats["large arguments (> 4pi), thresholds, NaNs"] = large(scale)
    args = [a for a in args if not a.startswith("--")]
    exe = args[0] if args else os.environ.get("DOOM_EXE") or (
        os.path.join(os.environ["DOOM_DIR"], "DOOMx64.exe") if os.environ.get("DOOM_DIR") else None)
    if not exe or not os.path.exists(exe):
        sys.exit("give the exe path, or set DOOM_EXE / DOOM_DIR")
    t0 = time.time()
    ours = port(sorted({u for xs in cats.values() for u in xs}))
    t1 = time.time()
    crt = Crt(exe)
    total = {"sinf": [0, 0], "cosf": [0, 0]}
    bad = []
    for name, xs in cats.items():
        line = [f"{name}: {len(xs)} inputs"]
        for k, (fname, addr) in enumerate((("sinf", SINF), ("cosf", COSF))):
            miss = 0
            for u in xs:
                e = crt.run(addr, u)
                m = ours[u][k]
                if e != m:
                    miss += 1
                    if len(bad) < 10:
                        bad.append((fname, u, e, m))
            total[fname][0] += len(xs)
            total[fname][1] += miss
            line.append(f"{fname} {miss} mismatches")
        print(", ".join(line), flush=True)
    for fname, (n, miss) in total.items():
        print(f"{fname}: {miss} mismatches of {n}")
    for fname, u, e, m in bad:
        print(f"  {fname}({u:#010x}): exe {e:#010x}, port {m:#010x}")
    print(f"port {t1 - t0:.0f} s, exe and compare {time.time() - t1:.0f} s")
    return 1 if any(m for _, m in total.values()) else 0


if __name__ == "__main__":
    sys.exit(main())
