"""Reference decodes of DOOM (2016) virtual-texture pages by running the engine's own code from the
user's DOOMx64.exe under Unicorn: the HD Photo image decoder FUN_141affcf0, the whole page decoder
FUN_141a64c70 (images, LZ plane, cover) and the LZ plane decompressor FUN_141a6cc90 (LZ4 / LZW).
Offline and read-only; the oracle for crates/jxr_sys and idres::vt (see tools/hdp_verify.py).

usage: hdp_emu.py <exe> <mega2> <slot> <image 0..2> <out.raw> [options]
  writes the image's 128*128*4 RGBA bytes (the engine's output layout) to out.raw
  --page            run the page decoder instead; out.raw gets the 0x50000-byte page buffer
  --stream <file>   also write the image codestream (input for crates/jxr_sys examples/jxr_dump)
  --dump <file>     intermediate buffers, records (i32 kind, ch, col, row, n, n x i32): kind 0/1
                    colour/alpha coefficients entering the inverse transform (FUN_141b3e0f0) per
                    macroblock and channel; kind 2/3 colour/alpha pixels entering colour conversion
                    (FUN_141b2e9b0) per macroblock; kind 9 the RGBA output
  --trace --deq --coef --pix   bitstream / dequantisation debug prints
"""
import struct, sys
import pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_64, UC_HOOK_MEM_UNMAPPED, UC_HOOK_CODE, UcError
from unicorn.x86_const import *

HDP_DECODE = 0x141affcf0
PAGE_DECODE = 0x141a64c70
LZ_DECOMPRESS = 0x141a6cc90
# Static constructors that fill the HD Photo SIMD constant tables in .bss (0x145b7c8a0..0x145b7c990).
CONST_INITS = [0x1402507d0, 0x1402507e0, 0x140250800, 0x140250810, 0x140250830, 0x140250840, 0x140250860,
               0x140250880, 0x1402508a0, 0x1402508c0, 0x1402508e0, 0x140250900, 0x140250920, 0x140250940, 0x140250960]
STACK, STACK_SZ = 0x7f0000000000, 0x400000
HEAP, HEAP_SZ = 0x10000000, 0x01000000
TEB = 0x7e0000000000
RET = STACK + 0x1000  # inside the stack map, never executed (emulation stops on reaching it)
SRC, DST, SCRATCH, CTX, HDR = HEAP, HEAP + 0x100000, HEAP + 0x200000, HEAP + 0x800000, HEAP + 0x900000
PAGE_BUF = 0x50000
SENTINEL = 0xcd  # fills the output before a run, so bytes the engine leaves untouched are visible
REGS = [UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9]


def read_page(mega, slot):
    """Raw page bytes (16-byte big-endian header + data) of a .mega2 slot."""
    with open(mega, "rb") as f:
        d = f.read(0x50)
        slot_ofs = struct.unpack_from("<Q", d, 0x38)[0]
        f.seek(slot_ofs + slot * 16)
        o, n = struct.unpack("<QQ", f.read(16))
        f.seek(o)
        return f.read(n)


def page_header(page):
    """(q0, q1, q2, flags, [size0, size1, size2, size3], lz_size, flags2, cover_fill)"""
    be = lambda o: struct.unpack_from(">H", page, o)[0]
    return page[0], page[1], page[2], page[3], [be(4), be(6), be(8), be(12)], be(10), page[14], page[15]


def image_stream(page, img):
    q0, q1, q2, flags, sizes, lz, flags2, fill = page_header(page)
    present = [flags2 & b == 0 for b in (1, 2, 4, 0x10)]
    start = 16 + sum(sizes[i] for i in range(img) if present[i])
    return page[start:start + sizes[img]]


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
        # TEB/TLS for ThreadLocalStoragePointer use (gs:[0x58]) if any.
        mu.mem_map(TEB, 0x10000)
        mu.reg_write(UC_X86_REG_GS_BASE, TEB)
        mu.mem_write(TEB + 0x58, struct.pack("<Q", TEB + 0x1000))
        mu.mem_write(TEB + 0x1000, struct.pack("<Q", TEB + 0x2000) * 64)
        mu.hook_add(UC_HOOK_MEM_UNMAPPED, self._unmapped)
        for fn in CONST_INITS:
            self.call(fn, count=1000)

    @staticmethod
    def _unmapped(uc, access, addr, sz, val, ud):
        print(f"unmapped access {access} at {addr:#x} (rip {uc.reg_read(UC_X86_REG_RIP):#x})")
        return False

    def call(self, fn, *args, count=400_000_000):
        mu = self.mu
        sp = STACK + STACK_SZ - 0x10000
        mu.mem_write(sp, struct.pack("<Q", RET))
        for k, a in enumerate(args):
            if k < 4:
                mu.reg_write(REGS[k], a)
            else:
                mu.mem_write(sp + 8 * (k + 1), struct.pack("<Q", a))
        mu.reg_write(UC_X86_REG_RSP, sp)
        mu.emu_start(fn, RET, count=count)
        return mu.reg_read(UC_X86_REG_RAX)

    def decode_image(self, page, img):
        """FUN_141affcf0 on page image `img` with the qualities the page decoder passes."""
        q = page[0:3]
        stream = image_stream(page, img)
        mu = self.mu
        mu.mem_write(SRC, stream + b"\0" * 64)
        mu.mem_write(DST, bytes([SENTINEL]) * (128 * 128 * 4))
        # ctx: q, q2, scratch ptr, scratch size, then zeroed ROI fields
        mu.mem_write(CTX, struct.pack("<iiQi", q[img], [0, q[1], q[1]][img], SCRATCH, 0xc000) + b"\0" * 0x40)
        rax = self.call(HDP_DECODE, CTX, SRC, DST, 128, 128, len(stream), 0)
        return rax, bytes(mu.mem_read(DST, 128 * 128 * 4))

    def decode_page(self, page):
        """FUN_141a64c70(header, data, 0, out): the engine's 0x50000-byte page buffer (images 0..2 RGBA
        at 0/0x10000/0x20000, LZ plane at 0x30000, raw page at 0x40000). The decoder reads a native
        (little-endian) header struct; the .mega2 header is big-endian."""
        q0, q1, q2, flags, sizes, lz, flags2, fill = page_header(page)
        hdr = bytes([q0, q1, q2, flags]) + struct.pack("<5H", sizes[0], sizes[1], sizes[2], lz, sizes[3]) + bytes([flags2, fill])
        mu = self.mu
        mu.mem_write(HDR, hdr)
        mu.mem_write(SRC, page[16:] + b"\0" * 64)
        mu.mem_write(DST, bytes([SENTINEL]) * PAGE_BUF)
        self.call(PAGE_DECODE, HDR, SRC, 0, DST)
        return bytes(mu.mem_read(DST, PAGE_BUF))

    def decompress(self, src, src_size, lz4, out_size=0x4000):
        """FUN_141a6cc90(src, srcSize, dstSize, dst, isLZ4) -> (ok, out)"""
        mu = self.mu
        mu.mem_write(SRC, src + b"\0" * 64)
        mu.mem_write(DST, bytes([SENTINEL]) * out_size)
        ok = self.call(LZ_DECOMPRESS, SRC, src_size, out_size, DST, int(lz4)) & 0xff
        return ok, bytes(mu.mem_read(DST, out_size))


# ---- debug hooks (CLI) ----
state = {}
DUMP = []


def rec(kind, ch, col, row, vals):
    DUMP.append(struct.pack("<5i", kind, ch, col, row, len(vals)) + struct.pack(f"<{len(vals)}i", *vals))


def dump_coef(uc, addr, sz, ud):
    sc = uc.reg_read(UC_X86_REG_RBX)
    col, row = uc.reg_read(UC_X86_REG_RDX) & 0xffffffff, uc.reg_read(UC_X86_REG_R8) & 0xffffffff
    w, h = struct.unpack("<QQ", uc.mem_read(sc + 0x4e8, 16))
    if col >= w or row >= h:
        return
    state.setdefault("first", sc)
    nch = 1 if struct.unpack("<i", uc.mem_read(sc + 0x350, 4))[0] == 2 else struct.unpack("<Q", uc.mem_read(sc + 0x358, 8))[0]
    for ch in range(nch):
        buf = struct.unpack("<Q", uc.mem_read(sc + 0x688 + 8 * ch, 8))[0]
        rec(0 if sc == state["first"] else 1, ch, col, row, struct.unpack("<256h", uc.mem_read(buf, 512)))


def dump_pix(uc, addr, sz, ud):
    sc = uc.reg_read(UC_X86_REG_RCX)
    io = uc.reg_read(UC_X86_REG_RDX)
    dstp, _, col, stride = struct.unpack("<4Q", uc.mem_read(io, 32))
    row = (dstp - DST) // (16 * stride)
    for ch, ofs in enumerate((0x508, 0x510, 0x518)):
        buf = struct.unpack("<Q", uc.mem_read(sc + ofs, 8))[0] + col * 512
        rec(2, ch, col, row, struct.unpack("<256h", uc.mem_read(buf, 512)))
    nxt = struct.unpack("<Q", uc.mem_read(sc + 0x9b0, 8))[0]
    if nxt:
        buf = struct.unpack("<Q", uc.mem_read(nxt + 0x508, 8))[0] + col * 512
        rec(3, 0, col, row, struct.unpack("<256h", uc.mem_read(buf, 512)))


def iobits(uc, io):
    left = struct.unpack("<Q", uc.mem_read(io + 8, 8))[0]
    b38, = struct.unpack("<i", uc.mem_read(io + 0x38, 4))
    b28, = struct.unpack("<i", uc.mem_read(io + 0x28, 4))
    return (b38 + b28) * 8 - left


STAGES = {0x141b417f0: "start", 0x141b41875: "dc", 0x141b41892: "lp", 0x141b418b8: "hp"}


def stage(uc, addr, sz, ud):
    if addr not in STAGES:
        return
    sc = uc.reg_read(UC_X86_REG_RCX) if addr == 0x141b417f0 else uc.reg_read(UC_X86_REG_RBX)
    io = struct.unpack("<Q", uc.mem_read(sc + 0x498, 8))[0]
    state.setdefault("first", sc)
    if state.get("n", 0) < 40:
        print(f"  {'A' if sc != state['first'] else 'C'} {STAGES[addr]} {iobits(uc, io)}")
        state["n"] = state.get("n", 0) + 1


HP = {0x141b42b1b: "cbp", 0x141b42b2c: "predcbp", 0x141b42c25: "blk", 0x141b42c50: "flex", 0x141b42c20: "blkcall"}


def hp(uc, addr, sz, ud):
    if addr not in HP or state.get("hpn", 0) > 400:
        return
    io = struct.unpack("<Q", uc.mem_read(uc.reg_read(UC_X86_REG_R14) + 0x10, 8))[0]
    extra = ""
    if addr == 0x141b42b2c:
        cb = struct.unpack("<3I", uc.mem_read(uc.reg_read(UC_X86_REG_RBP) + 0x200, 12))
        extra = "cbp " + " ".join(f"{v:x}" for v in cb)
    if addr == 0x141b42c20:
        extra = f"cbpbit {uc.reg_read(UC_X86_REG_RSI) & 1} model {uc.reg_read(UC_X86_REG_R12) & 0xffffffff} trim {uc.reg_read(UC_X86_REG_R15) & 0xffffffff}"
    if addr == 0x141b42c25:
        extra = f"nz {uc.reg_read(UC_X86_REG_RAX) & 0xffffffff}"
    print(f"    {HP[addr]} {iobits(uc, io)} {extra}")
    state["hpn"] = state.get("hpn", 0) + 1


def deq(uc, addr, sz, ud):
    if state.get("dq", 0) >= 8:
        return
    sc = uc.reg_read(UC_X86_REG_RCX)
    nch = struct.unpack("<Q", uc.mem_read(sc + 0x358, 8))[0]
    vals = struct.unpack(f"<{16*nch}h", uc.mem_read(sc, 32 * nch))
    for c in range(nch):
        print(f"  DQ{state.get('dq',0)} ch{c} " + " ".join(str(v) for v in vals[16*c:16*c+16]))
    if state.get("dq", 0) < 2:
        tile = struct.unpack("<Q", uc.mem_read(sc + 0x4c0, 8))[0] + struct.unpack("<Q", uc.mem_read(sc + 0x4d8, 8))[0] * 0x200
        for band, off in (("DC", 0), ("LP", 0x80), ("HP", 0x100)):
            for c in range(nch):
                q = struct.unpack("<Q", uc.mem_read(tile + off + 8 * c, 8))[0]
                if q:
                    print(f"    Q{band} ch{c} {uc.mem_read(q, 16).hex(' ', 2)}")
    state["dq"] = state.get("dq", 0) + 1


def coef(uc, addr, sz, ud):
    """dequantized coefficients of each alpha macroblock (after 0x141b3cc20 returns)"""
    sc = uc.reg_read(UC_X86_REG_RBX)
    if struct.unpack("<Q", uc.mem_read(sc + 0x358, 8))[0] == 1:
        buf = struct.unpack("<Q", uc.mem_read(sc + 0x688, 8))[0]
        print("COEF " + " ".join(map(str, struct.unpack("<256h", uc.mem_read(buf, 512)))))


def pix(uc, addr, sz, ud):
    sc = uc.reg_read(UC_X86_REG_RCX)
    nxt = struct.unpack("<Q", uc.mem_read(sc + 0x9b0, 8))[0]
    w = struct.unpack("<Q", uc.mem_read(sc + 0x4e8, 8))[0]
    buf = struct.unpack("<Q", uc.mem_read(nxt + 0x508, 8))[0]
    print("PIX " + " ".join(map(str, struct.unpack(f"<{256*w}h", uc.mem_read(buf, 512 * w)))))


def main():
    exe, mega, slot, img, out = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
    arg = lambda k: sys.argv[sys.argv.index(k) + 1]
    page = read_page(mega, slot)
    if "--stream" in sys.argv:
        open(arg("--stream"), "wb").write(image_stream(page, img))
    eng = Engine(exe)
    mu = eng.mu
    if "--dump" in sys.argv:
        mu.hook_add(UC_HOOK_CODE, dump_coef, begin=0x141b2d571, end=0x141b2d571)
        mu.hook_add(UC_HOOK_CODE, dump_pix, begin=0x141b2e9b0, end=0x141b2e9b0)
    if "--pix" in sys.argv:
        mu.hook_add(UC_HOOK_CODE, pix, begin=0x141b2e9b0, end=0x141b2e9b0)
    if "--coef" in sys.argv:
        mu.hook_add(UC_HOOK_CODE, coef, begin=0x141b2d54f, end=0x141b2d54f)
    if "--deq" in sys.argv:
        mu.hook_add(UC_HOOK_CODE, deq, begin=0x141b3cc20, end=0x141b3cc20)
    if "--trace" in sys.argv:
        mu.hook_add(UC_HOOK_CODE, stage, begin=0x141b417f0, end=0x141b418b8)
        mu.hook_add(UC_HOOK_CODE, hp, begin=0x141b42b1b, end=0x141b42c50)
    try:
        if "--page" in sys.argv:
            res = eng.decode_page(page)
        else:
            rax, res = eng.decode_image(page, img)
            print("rax", hex(rax))
    except UcError as e:
        print("emulation error:", e, hex(mu.reg_read(UC_X86_REG_RIP)))
        sys.exit(1)
    open(out, "wb").write(res)
    if "--dump" in sys.argv:
        with open(arg("--dump"), "wb") as f:
            f.write(b"".join(DUMP))
            if "--page" not in sys.argv:
                f.write(struct.pack("<5i", 9, 0, 0, 0, len(res)) + struct.pack(f"<{len(res)}i", *res))


if __name__ == "__main__":
    main()
