"""Native oracle for the engine's virtual-texture page re-encode: maps the user's own DOOMx64.exe
image into this process (no game code runs except the called leaf kernels) and calls the Intel ISPC
Texture Compressor kernels linked into it, so results come from this machine's CPU exactly as the
game would compute them (the kernels use rcpps/rsqrtps and, on AVX2, FMA).

  CompressBlocksBC3 dispatcher 0x141ec2cf0 (FUN_141a69610, layers 0..2 of a page):
    ISA 0 sse2 0x1400ed690, 1 sse4 0x1400b1e80, 2/3 avx 0x140077740, 4 avx2 0x140031610
  surface = {u8* ptr, i32 width, i32 height, i32 stride}

usage: vt_bc_oracle.py <exe> vectors <out.bin> [--mega2 FILE[,FILE..] --slots N --synthetic N]   (test vectors for idres vtex)
"""
import ctypes, struct, sys
from ctypes import wintypes

import pefile

KERNELS_BC3 = {"sse2": 0x1400ed690, "sse4": 0x1400b1e80, "avx": 0x140077740, "avx2": 0x140031610}
ISA_DETECT = 0x140001000  # returns the ISPC ISA level (0 sse2, 1 sse4, 2/3 avx, 4 avx2)

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
k32.VirtualAlloc.restype = ctypes.c_void_p
k32.VirtualAlloc.argtypes = [ctypes.c_void_p, ctypes.c_size_t, wintypes.DWORD, wintypes.DWORD]


class Surface(ctypes.Structure):
    _fields_ = [("ptr", ctypes.c_void_p), ("width", ctypes.c_int32), ("height", ctypes.c_int32), ("stride", ctypes.c_int32)]


class NativeExe:
    def __init__(self, exe):
        import numpy as np
        pe = pefile.PE(exe, fast_load=True)
        pref = pe.OPTIONAL_HEADER.ImageBase
        size = (pe.OPTIONAL_HEADER.SizeOfImage + 0xfff) & ~0xfff
        img = np.zeros(size, np.uint8)
        raw = np.frombuffer(pe.__data__, np.uint8)
        img[:pe.OPTIONAL_HEADER.SizeOfHeaders] = raw[:pe.OPTIONAL_HEADER.SizeOfHeaders]
        for sec in pe.sections:
            n = min(sec.SizeOfRawData, sec.Misc_VirtualSize or sec.SizeOfRawData)
            img[sec.VirtualAddress:sec.VirtualAddress + n] = raw[sec.PointerToRawData:sec.PointerToRawData + n]
        base = k32.VirtualAlloc(pref, size, 0x3000, 0x40)  # MEM_COMMIT|MEM_RESERVE, PAGE_EXECUTE_READWRITE
        if not base:
            base = k32.VirtualAlloc(None, size, 0x3000, 0x40)
            delta = (base - pref) & 0xffffffffffffffff
            d = pe.OPTIONAL_HEADER.DATA_DIRECTORY[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_BASERELOC"]]
            rel = bytes(img[d.VirtualAddress:d.VirtualAddress + d.Size])
            offs, o = [], 0
            while o + 8 <= len(rel):
                page, bsz = struct.unpack_from("<II", rel, o)
                if bsz < 8:
                    break
                ent = np.frombuffer(rel, np.uint16, (bsz - 8) // 2, o + 8)
                offs.append(page + (ent[(ent >> 12) == 10] & 0xfff).astype(np.int64))
                o += bsz
            offs = np.concatenate(offs)
            q = img.view(np.uint8)
            vals = np.zeros(len(offs), np.uint64)
            for k in range(8):
                vals |= q[offs + k].astype(np.uint64) << np.uint64(8 * k)
            vals = vals + np.uint64(delta)
            for k in range(8):
                q[offs + k] = ((vals >> np.uint64(8 * k)) & np.uint64(0xff)).astype(np.uint8)
        ctypes.memmove(base, img.ctypes.data, size)
        self.base, self.pref = base, pref
        self.isa = ctypes.CFUNCTYPE(ctypes.c_int)(self.va(ISA_DETECT))()

    def va(self, addr):
        return self.base + (addr - self.pref)

    def bc3(self, rgba, width, height, isa="avx2"):
        """rgba: width*height*4 bytes -> BC3 blocks (16 bytes per 4x4 block, row-major)"""
        src = ctypes.create_string_buffer(bytes(rgba), len(rgba))
        dst = ctypes.create_string_buffer((width // 4) * (height // 4) * 16)
        surf = Surface(ctypes.cast(src, ctypes.c_void_p), width, height, width * 4)
        fn = ctypes.CFUNCTYPE(None, ctypes.POINTER(Surface), ctypes.c_void_p)(self.va(KERNELS_BC3[isa]))
        fn(ctypes.byref(surf), ctypes.cast(dst, ctypes.c_void_p))
        return dst.raw


def synthetic_blocks(rng, n):
    """64x64 RGBA tiles that stress the encoder: flat, two-tone, gradients, noise, extremes."""
    out = []
    for t in range(n):
        kind = t % 6
        px = bytearray()
        base = [rng.randrange(256) for _ in range(4)]
        other = [rng.randrange(256) for _ in range(4)]
        for y in range(64):
            for x in range(64):
                if kind == 0:
                    c = base
                elif kind == 1:
                    c = base if (x // 4 + y // 4 + rng.randrange(2)) % 2 else other
                elif kind == 2:
                    c = [min(255, max(0, base[i] + (x * (other[i] - base[i])) // 64)) for i in range(4)]
                elif kind == 3:
                    c = [rng.randrange(256) for _ in range(4)]
                elif kind == 4:
                    c = [rng.choice((0, 255)) for _ in range(4)]
                else:
                    c = [min(255, max(0, base[i] + rng.randrange(-6, 7))) for i in range(4)]
                px += bytes(c)
        out.append(bytes(px))
    return out


def main():
    exe, mode = sys.argv[1], sys.argv[2]
    nat = NativeExe(exe)
    print(f"image at {nat.base:#x}, ISPC ISA level on this CPU: {nat.isa}")
    if mode == "vectors":
        import random, os, subprocess, tempfile
        import numpy as np
        from PIL import Image
        out = sys.argv[3]
        arg = lambda k, d: sys.argv[sys.argv.index(k) + 1] if k in sys.argv else d
        rng = random.Random(1)
        tiles = []  # (w, h, rgba)
        for t in synthetic_blocks(rng, int(arg("--synthetic", "48"))):
            tiles.append((64, 64, t))
        for mega in [m for m in (arg("--mega2", "") or "").split(",") if m]:
            doomx = arg("--doomx", os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "agent-hdp", "release", "doomx.exe"))
            tmp = tempfile.mkdtemp(prefix="vtbc_")
            with open(mega, "rb") as fh:
                count = struct.unpack_from("<I", fh.read(0x50), 0x48)[0]
            for s in rng.sample(range(count), min(count, int(arg("--slots", "40")))):
                subprocess.run([doomx, "vt-page", mega, "--slot", str(s), "--out", tmp], capture_output=True)
                for i in range(3):
                    p = os.path.join(tmp, f"{s}_img{i}.png")
                    if os.path.exists(p):
                        tiles.append((128, 128, np.asarray(Image.open(p).convert("RGBA")).tobytes()))
                        os.remove(p)
            import shutil
            shutil.rmtree(tmp, ignore_errors=True)
        with open(out, "wb") as f:
            for w, h, px in tiles:
                f.write(struct.pack("<II", w, h) + px)
                for isa in ("avx2", "sse4"):
                    f.write(nat.bc3(px, w, h, isa))
        print(f"{len(tiles)} tiles -> {out} (avx2 and sse4 references)")


if __name__ == "__main__":
    main()
