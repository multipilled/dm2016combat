"""Reference runs of idView's camera shakes from the user's own DOOMx64.exe under Unicorn (offline, read-only);
the oracle for crates/rancher_sim/src/viewfx.rs (notes: gamedata/re/VIEWFX.md).

usage: viewfx_emu.py <exe> [--rust]
  Starts shakes with the engine's own functions (0x140e6fa50 StartViewShake, 0x140e6f690 StartAdvancedViewShake) on a
  fake idView, then per frame rebuilds a renderView (fixed origin / axis) and runs 0x140e69be0 (decl / FX screen
  shake) and 0x140e68520 for slots 0..3 (advanced shakes). Game time, the gameLocal LCG (+0x285be8) and the engine
  timing struct (0x1436336e8: 960 ticks/s) are faked; sounds are off. Prints per frame the renderView origin + axis
  bits and the LCG state.
"""
import struct, sys
from unicorn.x86_const import UC_X86_REG_RAX, UC_X86_REG_RDX, UC_X86_REG_RSP, UC_X86_REG_RIP, UC_X86_REG_XMM2
from unicorn import UC_HOOK_CODE
from handlayers_emu import Engine, HEAP, f2u, u2f

START_VIEW = 0x140e6fa50
START_ADV = 0x140e6f690
APPLY_VIEW = 0x140e69be0
APPLY_ADV = 0x140e68520
GAMELOCAL_PTR = 0x144414d50
TIMING_PTR = 0x1436336e0
CV_DEBUG_SHAKE = 0x1444d4250
RENDER_CHECK = 0x141566eb0

VIEW, RV, DECL0, DECL1, ITEMS, VT = HEAP + 0x10000, HEAP + 0x20000, HEAP + 0x30000, HEAP + 0x30400, HEAP + 0x31000, HEAP + 0x40000
RCHK, RCHK_OBJ, CVZ, TIMING = HEAP + 0x41000, HEAP + 0x41100, HEAP + 0x41200, HEAP + 0x41300
GAMELOCAL, GAMELOCAL_SZ = 0x30000000, 0x290000
STUBS = 0x20000000
RNG_OFF = 0x285be8


class ViewEmu(Engine):
    def __init__(self, exe):
        super().__init__(exe)
        mu = self.mu
        mu.mem_map(STUBS, 0x1000)
        mu.mem_write(STUBS, b"\xc3" * 0x1000)
        mu.mem_map(GAMELOCAL, GAMELOCAL_SZ)
        self.handlers, self.stub_n, self.now = {}, 0, 0
        mu.hook_add(UC_HOOK_CODE, self._hook)
        self.vtable(GAMELOCAL, VT, {0x220: lambda: self.ret(rax=self.now), 0x810: self.time_to_buf})
        mu.mem_write(GAMELOCAL_PTR, struct.pack("<Q", GAMELOCAL))
        # "renderer / options" check in 0x140e69be0: object whose vslot 0xb8 returns a struct with two cvar
        # pointers (+0x88, +0x80); both 0 -> shakes allowed.
        self.vtable(RCHK, VT + 0x1000, {0xb8: lambda: self.ret(rax=RCHK_OBJ)})
        mu.mem_write(RCHK_OBJ + 0x80, struct.pack("<2Q", CVZ, CVZ))
        mu.mem_write(CVZ + 0x30, struct.pack("<i", 0))
        self.handlers[RENDER_CHECK] = lambda: self.ret(rax=RCHK)
        mu.mem_write(CV_DEBUG_SHAKE, struct.pack("<i", 0))
        # engine timing: +0x10 ticks/s 960, +0x1c 1/960 (as 0x140211e20 computes it)
        mu.mem_write(TIMING + 0x10, struct.pack("<i", 960))
        mu.mem_write(TIMING + 0x1c, struct.pack("<I", f2u(1.0 / 960.0)))
        mu.mem_write(TIMING_PTR, struct.pack("<Q", TIMING))

    def vtable(self, obj, vt, slots):
        self.mu.mem_write(obj, struct.pack("<Q", vt))
        for off, fn in slots.items():
            stub = STUBS + 0x10 * self.stub_n
            self.stub_n += 1
            self.mu.mem_write(vt + off, struct.pack("<Q", stub))
            self.handlers[stub] = fn

    def _hook(self, uc, addr, size, ud):
        h = self.handlers.get(addr)
        if h:
            h()

    def ret(self, rax=None):
        mu = self.mu
        if rax is not None:
            mu.reg_write(UC_X86_REG_RAX, rax)
        sp = mu.reg_read(UC_X86_REG_RSP)
        mu.reg_write(UC_X86_REG_RIP, struct.unpack("<Q", mu.mem_read(sp, 8))[0])
        mu.reg_write(UC_X86_REG_RSP, sp + 8)

    def time_to_buf(self):
        out = self.mu.reg_read(UC_X86_REG_RDX)
        self.mu.mem_write(out, struct.pack("<i", self.now))
        self.ret(rax=out)

    def seed(self, s):
        self.mu.mem_write(GAMELOCAL + RNG_OFF, struct.pack("<I", s))

    def rng(self):
        return struct.unpack("<I", self.mu.mem_read(GAMELOCAL + RNG_OFF, 4))[0]

    def reset_view(self):
        """idView ctor (0x140e66a20) shake fields."""
        mu = self.mu
        mu.mem_write(VIEW, b"\0" * 0x3260)
        w = lambda a, fmt, *v: mu.mem_write(VIEW + a, struct.pack(fmt, *v))
        w(0xe30, "<f", 1.0)
        w(0xe34, "<3f", 10.0, 10.0, 10.0)
        w(0xe40, "<3f", 6.0, 6.0, 6.0)
        w(0xe7c, "<9f", 1, 0, 0, 0, 1, 0, 0, 0, 1)

    def write_view_decl(self, at, d):
        mu = self.mu
        w = lambda a, fmt, *v: mu.mem_write(at + a, struct.pack(fmt, *v))
        mu.mem_write(at, b"\0" * 0xd0)
        w(0x68, "<f", d["max_scale"])
        w(0x6c, "<3f", *d["angles"])
        w(0x78, "<3f", *d["offset"])
        w(0x84, "<f", d["power"])
        w(0x88, "<ii", d["duration"], d["method"])
        items = ITEMS + (at - DECL0)
        for k, (b, m, p, n) in enumerate(d["infos"]):
            mu.mem_write(items + 0xc * k, struct.pack("<fIBB2x", b, m, p, n))
        w(0x90, "<Qii", items, len(d["infos"]), len(d["infos"]))
        w(0xa8, "<BB", int(d["fade"]), 0)
        w(0xc0, "<f", 1.0)
        w(0xc4, "<I", f2u(u2f(f2u(d["power"])) / 100.0))

    def write_adv_decl(self, at, d):
        mu = self.mu
        w = lambda a, fmt, *v: mu.mem_write(at + a, struct.pack(fmt, *v))
        mu.mem_write(at, b"\0" * 0xd0)
        w(0x68, "<i", d["time_ms"])
        for base, p in ((0x70, d["rot"]), (0x98, d["trans"])):
            w(base, "<Q", 0)
            w(base + 8, "<3f", *p["scale"])
            w(base + 0x14, "<4f", p["min_hz"], p["max_hz"], p["sample_rand"], p["amp_rand"])
        c = u2f(0x3b360b61)
        w(0xc0, "<3I", *[f2u(u2f(f2u(s)) * c) for s in d["rot"]["scale"]])

    def start_view(self, mag):
        self.mu.reg_write(UC_X86_REG_XMM2, f2u(mag))
        self.call(START_VIEW, VIEW, DECL0)

    def start_adv(self, decl, mag):
        self.mu.reg_write(UC_X86_REG_XMM2, f2u(mag))
        self.call(START_ADV, VIEW, decl)

    def frame(self, origin, axis):
        mu = self.mu
        mu.mem_write(RV, b"\0" * 0x100)
        mu.mem_write(RV + 0x60, struct.pack("<3f", *origin))
        mu.mem_write(RV + 0x6c, struct.pack("<9f", *[x for r in axis for x in r]))
        self.call(APPLY_VIEW, VIEW, RV)
        for i in range(4):
            self.call(APPLY_ADV, VIEW, RV, i, 0)
        out = list(struct.unpack("<12I", mu.mem_read(RV + 0x60, 48)))
        return out + [self.rng()]


def axis_of(p, y, r):
    import math
    d = math.pi / 180.0
    sp, cp, sy, cy, sr, cr = math.sin(p * d), math.cos(p * d), math.sin(y * d), math.cos(y * d), math.sin(r * d), math.cos(r * d)
    m = [[cp * cy, cp * sy, -sp], [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp], [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp]]
    return [[u2f(f2u(x)) for x in row] for row in m]


ORIGIN = (100.0, 200.0, 50.0)
AXIS = axis_of(10.0, 30.0, 0.0)

# (name, decl, magnitude); decl fields as idDeclViewShake: infos = (blendingValue, mode, positive, negative)
VIEW_SCENARIOS = [
    ("melee", dict(max_scale=1.0, angles=(25.0, 25.0, 25.0), offset=(5.0, 5.0, 5.0), power=10.0, duration=100, method=0,
                   infos=[(1.0, 0, 0, 1)], fade=1), 1.0),
    ("meleeleft", dict(max_scale=1.0, angles=(10.0, 10.0, 10.0), offset=(5.0, 5.0, 5.0), power=2.5, duration=200, method=3,
                       infos=[(1.0, 6, 1, 0), (1.0, 13, 1, 0)], fade=1), 1.0),
    ("meleeright", dict(max_scale=1.0, angles=(10.0, 10.0, 10.0), offset=(5.0, 5.0, 5.0), power=2.5, duration=200, method=3,
                        infos=[(0.5, 6, 1, 0), (1.0, 13, 0, 1)], fade=1), 1.0),
    ("allmodes_exp", dict(max_scale=0.8, angles=(12.0, 7.0, 3.0), offset=(4.0, 2.0, 6.0), power=30.0, duration=300, method=1,
                          infos=[(0.5 + 0.1 * k, k, int(k % 3 == 1), int(k % 3 == 2)) for k in range(16)], fade=1), 0.7),
    ("linear_nofade", dict(max_scale=1.0, angles=(3.0, 4.0, 5.0), offset=(1.0, 2.0, 3.0), power=50.0, duration=150, method=2,
                           infos=[(1.0, 1, 0, 0), (2.0, 12, 0, 1), (0.0, 0, 0, 0), (1.0, 7, 0, 0)], fade=0), 0.4),
]
# FX camera shakes: (name, camera_shake, shake_volume, fx_pos, fade_start, fade_end)
FX_SCENARIOS = [
    ("fx_near", 0.6, 0.1, (150.0, 230.0, 60.0), 128.0, 512.0),
    ("fx_mid", 0.9, 0.0, (400.0, 200.0, 50.0), 128.0, 512.0),
    ("fx_far", 0.9, 0.0, (900.0, 200.0, 50.0), 128.0, 512.0),
    ("fx_nofade", 1.0, 0.0, (0.0, 0.0, 0.0), 0.0, 0.0),
]
ADV_DECLS = [
    dict(time_ms=250, rot=dict(scale=(0.0, 2.0, 2.0), min_hz=12.0, max_hz=12.0, sample_rand=0.0, amp_rand=0.0),
         trans=dict(scale=(0.0, 0.0, 0.0), min_hz=40.0, max_hz=40.0, sample_rand=0.0, amp_rand=0.0)),
    dict(time_ms=400, rot=dict(scale=(1.5, 2.5, 0.5), min_hz=16.0, max_hz=10.0, sample_rand=0.1, amp_rand=0.75),
         trans=dict(scale=(1.0, 2.0, 3.0), min_hz=50.0, max_hz=70.0, sample_rand=0.2, amp_rand=0.3)),
]
FRAMES = 24


def run(exe):
    eng = ViewEmu(exe)
    out = []
    for name, d, mag in VIEW_SCENARIOS:
        eng.reset_view()
        eng.seed(0x12345678)
        eng.now = 1000
        eng.write_view_decl(DECL0, d)
        eng.start_view(mag)
        rows = []
        for f in range(FRAMES):
            eng.now += 16
            rows.append(eng.frame(ORIGIN, AXIS))
        out.append((name, rows))
    for name, cs, vol, pos, fs, fe in FX_SCENARIOS:
        eng.reset_view()
        eng.seed(0x2468ace0)
        eng.now = 5000
        w = lambda a, fmt, *v: eng.mu.mem_write(VIEW + a, struct.pack(fmt, *v))
        w(0xe2c, "<f", cs)
        w(0xe54, "<f", vol)
        w(0xe58, "<3f", *pos)
        w(0xe64, "<2f", fs, fe)
        rows = []
        for f in range(6):
            eng.now += 16
            rows.append(eng.frame(ORIGIN, AXIS))
        out.append((name, rows))
    eng.reset_view()
    eng.seed(0x0badf00d)
    eng.now = 2000
    eng.write_adv_decl(DECL0, ADV_DECLS[0])
    eng.write_adv_decl(DECL1, ADV_DECLS[1])
    eng.start_adv(DECL0, 1.0)
    eng.now += 16
    eng.start_adv(DECL1, 0.8)
    rows = []
    for f in range(FRAMES + 4):
        eng.now += 16
        rows.append(eng.frame(ORIGIN, AXIS))
    out.append(("advanced", rows))
    return out


def main():
    exe = sys.argv[1]
    res = run(exe)
    if "--rust" not in sys.argv:
        for name, rows in res:
            o = [round(u2f(u), 4) for u in rows[0][:3]]
            print(name, "frame0 origin", o, "last origin", [round(u2f(u), 4) for u in rows[-1][:3]], "rng", hex(rows[-1][12]))
        return
    print("// generated by tools/viewfx_emu.py from DOOMx64.exe (0x140e6fa50/0x140e6f690 starts, 0x140e69be0 + 0x140e68520 x4");
    print("// per frame); per frame: renderView origin (3) + axis (9) f32 bits, then the LCG state")
    for name, rows in res:
        print(f"pub const {name.upper()}: [[u32; 13]; {len(rows)}] = [")
        for r in rows:
            print("    [" + ", ".join(f"0x{u:08x}" for u in r) + "],")
        print("];")
    print("pub const AXIS: [u32; 9] = [" + ", ".join(f"0x{f2u(x):08x}" for row in AXIS for x in row) + "];")


if __name__ == "__main__":
    main()
