"""Reference runs of the idHands weapon-lag pendulum integrators from the user's own DOOMx64.exe under Unicorn:
FUN_140d8b100 (hands_weaponLagIntegrationMethod 0, Euler) and FUN_140d8b720 (method 1, RK4, the default).
Offline and read-only; the oracle for crates/rancher_sim/src/handlayers.rs (see gamedata/re/HANDSLAYERS.md).

usage: handlayers_emu.py <exe> [--steps N] [--rust]
  Runs fixed synthetic scenarios (pendulum at rest, pushed by a sequence of forces, including ones large enough to
  hit the maxAngle clamp) and prints, per step, the f32 bit patterns of the position and velocity after the call.
  --rust prints the result as Rust array literals for the unit tests.
usage: handlayers_emu.py <exe> --frames [--rust]
  Runs the per-frame idHands layer code on a fake idHands / idPlayer / idDeclWeapon: 0x140d8c7d0 (weapon lag input,
  sub-steps, basis), 0x140d8c240 (weapon bob) and 0x140d8c7e0 (joint mods), for a scripted input sequence. Calls
  leaving that code (weapon/decl getters, view angles, frame time, joint transforms, crouch test, footstep, bob
  cycle web) are answered by hooks. Prints the idHands fields +0x5b20..+0x5b7c and the 4 joint mods per frame.
"""
import struct, sys
import pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_64, UC_HOOK_MEM_UNMAPPED, UC_HOOK_CODE
from unicorn.x86_const import UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9, UC_X86_REG_RSP, \
    UC_X86_REG_GS_BASE, UC_X86_REG_RAX, UC_X86_REG_RIP, UC_X86_REG_XMM0

FLT_MIN_INIT = 0x1400f8a70
EULER = 0x140d8b100
RK4 = 0x140d8b720
STACK, STACK_SZ = 0x7f0000000000, 0x100000
HEAP, HEAP_SZ = 0x10000000, 0x100000
TEB = 0x7e0000000000
RET = STACK + 0x1000
REGS = [UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9]
POS, VEL, PIVOT, ACCEL = HEAP, HEAP + 0x40, HEAP + 0x80, HEAP + 0xc0


def f2u(f):
    return struct.unpack("<I", struct.pack("<f", f))[0]


def u2f(u):
    return struct.unpack("<f", struct.pack("<I", u))[0]


class Engine:
    def __init__(self, exe):
        pe = pefile.PE(exe, fast_load=True)
        base = pe.OPTIONAL_HEADER.ImageBase
        image = pe.get_memory_mapped_image()
        size = (max(len(image), pe.OPTIONAL_HEADER.SizeOfImage) + 0xfff) & ~0xfff
        mu = self.mu = Uc(UC_ARCH_X86, UC_MODE_64)
        mu.mem_map(base, size)
        mu.mem_write(base, image)
        mu.mem_map(STACK, STACK_SZ)
        mu.mem_map(HEAP, HEAP_SZ)
        mu.mem_map(TEB, 0x10000)
        mu.reg_write(UC_X86_REG_GS_BASE, TEB)
        mu.hook_add(UC_HOOK_MEM_UNMAPPED, self._unmapped)
        # Static initialiser of the FLT_MIN x4 constant (0x144144b50) every InvSqrt clamps with.
        self.call(FLT_MIN_INIT)

    @staticmethod
    def _unmapped(uc, access, addr, sz, val, ud):
        print(f"unmapped access {access} at {addr:#x}", file=sys.stderr)
        return False

    def call(self, fn, *args):
        mu = self.mu
        sp = STACK + STACK_SZ - 0x10000
        mu.mem_write(sp, struct.pack("<Q", RET))
        for k, a in enumerate(args):
            if k < 4:
                mu.reg_write(REGS[k], a)
            else:
                mu.mem_write(sp + 8 * (k + 1), struct.pack("<Q", a))
        mu.reg_write(UC_X86_REG_RSP, sp)
        mu.emu_start(fn, RET, count=10_000_000)

    def vec(self, addr, v=None):
        if v is not None:
            self.mu.mem_write(addr, struct.pack("<3I", *(f2u(x) for x in v)))
        return list(struct.unpack("<3I", self.mu.mem_read(addr, 12)))

    def integrate(self, fn, pos_bits, vel_bits, pivot, accel, dt, friction, max_angle, length):
        mu = self.mu
        mu.mem_write(POS, struct.pack("<3I", *pos_bits))
        mu.mem_write(VEL, struct.pack("<3I", *vel_bits))
        self.vec(PIVOT, pivot)
        self.vec(ACCEL, accel)
        self.call(fn, POS, VEL, PIVOT, ACCEL, f2u(dt), f2u(friction), f2u(max_angle), f2u(length))
        return self.vec(POS), self.vec(VEL)


RECORD_EVERY = 10


def scenarios():
    """(name, params, forces): params = (length, friction, maxAngleDeg, gravity); forces = per-step (x, y, z) before the
    gravity term, i.e. the clamped view-space acceleration the caller negates. Steps are 1 ms."""
    out = []
    # HAR-like: pushed forward then left, released.
    f = [(0.5, 0.0, 0.0)] * 300 + [(0.0, -0.5, 0.1)] * 300 + [(0.0, 0.0, 0.0)] * 400
    out.append(("har", (0.125, 15.0, 8.0, 3.8), f))
    # Gauss-like (friction 20, max 4 deg) with an acceleration large enough to hit the angle clamp.
    f = [(0.0, 0.0, 0.0)] * 5 + [(1.0, 0.8, -0.3)] * 495 + [(-1.0, 0.0, 0.0)] * 300 + [(0.0, 0.0, 0.0)] * 200
    out.append(("clamp", (0.125, 20.0, 4.0, 3.8), f))
    return out


def run(exe, steps=None):
    eng = Engine(exe)
    res = []
    for name, (length, friction, max_deg, gravity), forces in scenarios():
        if steps:
            forces = forces[:steps]
        for fn, tag in ((EULER, "euler"), (RK4, "rk4")):
            pos = [0, 0, 0]
            vel = [0, 0, 0]
            rows = []
            for fx, fy, fz in forces:
                # float32 like the caller (0x140d8d733): 0 - x, 0 - y, -gravity - z
                f32 = lambda x: u2f(f2u(x))
                accel = (f32(0.0 - f32(fx)), f32(0.0 - f32(fy)), f32(f32(-gravity) - f32(fz)))
                pos, vel = eng.integrate(fn, pos, vel, (0.0, 0.0, length), accel, u2f(0x3a83126f), friction,
                                         max_deg * u2f(0x3c8efa35), length)
                rows.append((pos, vel))
            rows = rows[RECORD_EVERY - 1::RECORD_EVERY]
            res.append((name, tag, (length, friction, max_deg, gravity), forces, rows))
    return res


# ---- per-frame layer code on fake objects ----
LAG_FRAME = 0x140d8c7d0      # hands_weaponLagEnable test, then 0x140d8d2c0
BOB_FRAME = 0x140d8c240
COMPOSE = 0x140d8c7e0
GET_DECL = 0x140f12c10       # idWeapon -> idDeclWeapon for the current mode
GET_CONTROLLER = 0x140dd18c0  # idPlayer -> idPlayerController
JOINT_XFORM = 0x1415ef8c0    # animator, 1, joint, &origin, &axis
IS_CROUCHED = 0x1416b04c0
FOOTSTEP = 0x14074cc40
BOB_WEB = 0x140d83e80
GAMELOCAL_PTR = 0x144414d50
CV_LAG_ENABLE = 0x1444b5170
CV_LAG_METHOD = 0x1444b5530
CV_BOB_ENABLE = 0x1444b3c70

HANDS, PLAYER, DECL, WEAPON = HEAP + 0x10000, HEAP + 0x30000, HEAP + 0x50000, HEAP + 0x52000
CONTROLLER, GAMELOCAL, LAGANIM = HEAP + 0x53000, HEAP + 0x53100, HEAP + 0x53200
BRANCH, LEAF = HEAP + 0x53300, HEAP + 0x53400
MODS0, MODS1, ANIMATOR, VT = HEAP + 0x54000, HEAP + 0x54200, HEAP + 0x54400, HEAP + 0x58000
STUBS = 0x20000000


class FrameEmu(Engine):
    def __init__(self, exe):
        super().__init__(exe)
        mu = self.mu
        mu.mem_map(STUBS, 0x1000)
        mu.mem_write(STUBS, b"\xc3" * 0x1000)
        self.handlers = {}
        self.stub_n = 0
        self.inp = {}
        self.steps = []
        mu.hook_add(UC_HOOK_CODE, self._hook)
        # vtables: player (0xb28 weapon), controller (0x128 view, 0x130 prev view), gameLocal (0x240 frame
        # seconds), lag animator (0x88 merge branch)
        self.vtable(PLAYER, VT, {0xb28: lambda: self.ret(rax=WEAPON)})
        self.vtable(CONTROLLER, VT + 0x1000, {0x128: lambda: self.angles("view"), 0x130: lambda: self.angles("prev")})
        self.vtable(GAMELOCAL, VT + 0x2000, {0x240: self.frame_seconds})
        self.vtable(LAGANIM, VT + 0x3000, {0x88: lambda: self.ret(rax=BRANCH)})
        mu.mem_write(GAMELOCAL_PTR, struct.pack("<Q", GAMELOCAL))
        self.handlers[GET_DECL] = lambda: self.ret(rax=DECL)
        self.handlers[GET_CONTROLLER] = lambda: self.ret(rax=CONTROLLER)
        self.handlers[JOINT_XFORM] = self.joint_xform
        self.handlers[IS_CROUCHED] = lambda: self.ret(rax=int(self.inp["crouched"]))
        self.handlers[FOOTSTEP] = self.footstep
        self.handlers[BOB_WEB] = lambda: self.ret()
        for a, v in ((CV_LAG_ENABLE, 1), (CV_LAG_METHOD, 1), (CV_BOB_ENABLE, 1)):
            mu.mem_write(a, struct.pack("<i", v))
        w = lambda a, fmt, *v: mu.mem_write(a, struct.pack(fmt, *v))
        w(HANDS + 0x2b0, "<Q", PLAYER)
        w(HANDS + 0x2b8, "<Q", ANIMATOR)
        mu.mem_write(HANDS + 0x5ab0, struct.pack("<Q", VT + 0x3000))
        w(HANDS + 0x5b08, "<4i", 0, 1, 2, 3)
        w(HANDS + 0x5b18, "<4h", 0, 1, 2, 3)
        w(HANDS + 0x5b38, "<9f", 1, 0, 0, 0, 1, 0, 0, 0, 1)
        w(PLAYER + 0x4288, "<Q", 0)
        w(WEAPON + 0x8d4, "<i", 0)
        w(BRANCH + 0x18, "<Q", LEAF)
        w(LEAF + 0x28, "<i", 0)
        w(LEAF + 0x30, "<Q", MODS0)
        w(LEAF + 0x48, "<Q", MODS1)
        w(ANIMATOR + 0xf8, "<3f", 1, 1, 1)

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

    def ret(self, rax=None, xmm0=None):
        mu = self.mu
        if rax is not None:
            mu.reg_write(UC_X86_REG_RAX, rax)
        if xmm0 is not None:
            mu.reg_write(UC_X86_REG_XMM0, xmm0)
        sp = mu.reg_read(UC_X86_REG_RSP)
        mu.reg_write(UC_X86_REG_RIP, struct.unpack("<Q", mu.mem_read(sp, 8))[0])
        mu.reg_write(UC_X86_REG_RSP, sp + 8)

    def frame_seconds(self):
        # idGameTimeManagerLocal vslot 0x108 (0x140362ac0): (float)msec * 0.001f
        self.ret(xmm0=f2u(u2f(f2u(float(self.inp["msec"]))) * u2f(0x3a83126f)))

    def angles(self, which):
        out = self.mu.reg_read(UC_X86_REG_RDX)
        self.vec(out, self.inp[which])
        self.ret(rax=out)

    def joint_xform(self):
        mu = self.mu
        joint = mu.reg_read(UC_X86_REG_R8) & 0xffff
        origin = mu.reg_read(UC_X86_REG_R9)
        sp = mu.reg_read(UC_X86_REG_RSP)
        axis = struct.unpack("<Q", mu.mem_read(sp + 0x28, 8))[0]
        self.vec(origin, self.inp["joints"][joint])
        mu.mem_write(axis, struct.pack("<9f", 1, 0, 0, 0, 1, 0, 0, 0, 1))
        self.ret(rax=1)

    def footstep(self):
        self.steps.append(self.mu.reg_read(UC_X86_REG_RDX) & 0xff)
        self.ret()

    def set_decl(self, lag, bob):
        """lag = (enable, length, friction, maxDeg, maxAccel, gravity, dip); bob = (enable, stride, strideCrouched,
        tAmp3, tVel3, tPhase3, rAmp3 (pitch, yaw, roll), rVel3, rPhase3)"""
        w = lambda a, fmt, *v: self.mu.mem_write(a, struct.pack(fmt, *v))
        w(DECL + 0x13f0, "<B3x5fB", int(lag[0]), *lag[1:6], int(lag[6]))
        w(DECL + 0x140c, "<B3x2f", int(bob[0]), bob[1], bob[2])
        w(DECL + 0x1418, "<18f", *[x for t in bob[3:9] for x in t])

    def frame(self, inp):
        mu = self.mu
        self.inp = inp
        w = lambda a, fmt, *v: mu.mem_write(a, struct.pack(fmt, *v))
        w(PLAYER + 0x16934, "<3f", *inp["accel"])
        w(PLAYER + 0x16928, "<3f", *inp["vel"])
        w(PLAYER + 0x16990, "<B", int(inp["ground"]))
        self.steps = []
        self.call(LAG_FRAME, HANDS)
        self.call(BOB_FRAME, HANDS)
        self.call(COMPOSE, HANDS)
        fields = list(struct.unpack("<24I", mu.mem_read(HANDS + 0x5b20, 0x60)))
        leaf_n = struct.unpack("<i", mu.mem_read(LEAF + 0x28, 4))[0]
        mods = MODS1 if leaf_n & 1 else MODS0
        m = list(struct.unpack("<64I", mu.mem_read(mods, 0x100)))
        mod_bits = [m[k * 16:k * 16 + 12] for k in range(4)]
        return fields, mod_bits, list(self.steps)


def frame_script():
    """Scripted inputs: walk forward accelerating while turning, strafe, crouch-walk, stop; msec 15..17."""
    joints = [(18.0, 9.5, -12.0), (21.0, -6.0, -10.5), (-2.0, 10.0, 4.0), (-2.0, -10.0, 4.0)]
    out = []
    yaw, pitch = 10.0, -5.0
    for i in range(90):
        msec = (16, 17, 15)[i % 3]
        prev = (pitch, yaw, 0.0)
        if i < 30:
            accel, vel, crouch = (1500.0, 300.0, 0.0), (min(320.0, 25.0 * i), 4.0 * i, 0.0), False
            yaw += 1.5
        elif i < 55:
            accel, vel, crouch = (-200.0, -900.0, 30.0), (150.0, -280.0, 0.0), False
            pitch += 0.7
        elif i < 75:
            accel, vel, crouch = (0.0, 0.0, 0.0), (90.0, 30.0, 0.0), True
            yaw -= 3.0
        else:
            accel, vel, crouch = (-2500.0, 0.0, 0.0), (0.0, 0.0, 0.0), False
        out.append({"msec": msec, "view": (pitch, yaw, 0.0), "prev": prev, "accel": accel, "vel": vel,
                    "ground": i < 80, "crouched": crouch, "joints": joints})
    return out


FRAME_LAG = (True, 0.125, 15.0, 8.0, 0.5, 3.8, False)
FRAME_BOB = (True, 0.5, 0.2, (0.0, 0.0, 0.003), (0.0, 0.0, 0.9), (0.0, 0.0, 0.0), (0.5, 0.4, 0.0),
             (0.9, 0.45, 0.0), (90.0, 0.0, 0.0))


def run_frames(exe, dip=False, method=1):
    eng = FrameEmu(exe)
    eng.mu.mem_write(CV_LAG_METHOD, struct.pack("<i", method))
    lag = FRAME_LAG[:6] + (dip,)
    eng.set_decl(lag, FRAME_BOB)
    return [eng.frame(inp) for inp in frame_script()]


def main():
    args = sys.argv[1:]
    exe = args[0]
    if "--frames" in args:
        rows = run_frames(exe)
        if "--rust" in args:
            for name, (dip, method) in (("FRAMES", (False, 1)), ("FRAMES_DIP_EULER", (True, 0))):
                rows = run_frames(exe, dip, method)
                print(f"// {name}: weaponDipForward {dip}, hands_weaponLagIntegrationMethod {method}; per frame idHands")
                print("// +0x5b20..+0x5b7c (24 u32), the 4 joint mods (12 u32 each), the footsteps")
                print(f"pub const {name}: [Frame; {len(rows)}] = [")
                for fields, mods, steps in rows:
                    fs = ", ".join(f"0x{u:08x}" for u in fields)
                    ms = ", ".join("[" + ", ".join(f"0x{u:08x}" for u in m) + "]" for m in mods)
                    print(f"    ([{fs}], [{ms}], &{steps}),")
                print("];")
        else:
            for k, (fields, mods, steps) in enumerate(rows):
                print(k, "lagpos", [round(u2f(u), 6) for u in fields[0:3]],
                      "bob", [round(u2f(u), 5) for u in fields[15:23]], "steps", steps)
        return
    steps = int(args[args.index("--steps") + 1]) if "--steps" in args else None
    res = run(exe, steps)
    if "--rust" in args:
        for name, tag, params, forces, rows in res:
            print(f"// {name} / {tag}: params {params}; [pos bits, vel bits] after every {RECORD_EVERY}th 1 ms step")
            print(f"const {name.upper()}_{tag.upper()}: [[u32; 6]; {len(rows)}] = [")
            for pos, vel in rows:
                print("    [" + ", ".join(f"0x{u:08x}" for u in pos + vel) + "],")
            print("];")
        return
    for name, tag, params, forces, rows in res:
        pos, vel = rows[-1]
        print(name, tag, params, "final pos", [u2f(u) for u in pos], "vel", [u2f(u) for u in vel])


if __name__ == "__main__":
    main()
