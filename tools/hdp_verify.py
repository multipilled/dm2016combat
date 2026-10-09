"""Batch verification of idres::vt::decode_page (doomx vt-page, i.e. jxr_sys + vt.rs) against the
engine's own page decoder FUN_141a64c70 run under Unicorn (tools/hdp_emu.py). Offline, read-only
on the game files; reports the byte-exact match rate per layer and overall.

usage: hdp_verify.py [--exe DOOMx64.exe] [--doom DIR] [--doomx doomx.exe] [--files sq1,sq11,...]
                     [--per-file N] [--per-variant M] [--seed S] [--jobs J] [--synthetic K] [--keep DIR]
  --per-file N     random slots per .mega2 file (default 24)
  --per-variant M  extra slots per (flags, flags2) combination found in each file (default 2)
  --synthetic K    also build K synthetic pages per file whose LZ plane is re-encoded with LZW
                   (flags bit 2 clear; no shipped page uses LZW) in a scratch .mega2 (default 4)
Compared per page: images 0..2 (RGBA; images absent from the page must be zero in the engine and
None in decode_page, image 2 always carries the cover byte), the LZ plane (absent -> zero).
"""
import argparse, glob, json, os, random, shutil, struct, subprocess, sys, tempfile, time
from multiprocessing import Pool

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hdp_emu

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def slot_table(path):
    with open(path, "rb") as f:
        h = f.read(0x170)
        so, = struct.unpack_from("<Q", h, 0x38)
        sc, = struct.unpack_from("<I", h, 0x48)
        f.seek(so)
        tab = f.read(sc * 16)
    return [struct.unpack_from("<QQ", tab, 16 * s) for s in range(sc)]


def scan_flags(path, cache_dir):
    """(flags, flags2) of every slot, cached in gamedata/vt."""
    cache = os.path.join(cache_dir, "scan_" + os.path.basename(path) + ".json")
    if os.path.exists(cache) and os.path.getmtime(cache) > os.path.getmtime(path):
        return json.load(open(cache))
    tab = slot_table(path)
    out = []
    with open(path, "rb") as f:
        for o, n in tab:
            f.seek(o)
            h = f.read(16)
            out.append([h[3], h[14]])
    os.makedirs(cache_dir, exist_ok=True)
    json.dump(out, open(cache, "w"))
    return out


# ---- LZW encoder (idLZWCompressor::Write / End, the engine's decoder 0x141914db0 is its inverse) ----
def lzw_compress(data):
    out, temp, temp_bits = bytearray(), 0, 0
    dic, nxt, bits, w = {}, 0x100, 9, -1

    def put(code, nbits):
        nonlocal temp, temp_bits
        temp |= code << temp_bits
        temp_bits += nbits
        while temp_bits >= 8:
            out.append(temp & 0xff)
            temp >>= 8
            temp_bits -= 8

    for k in data:
        if w == -1:
            w = k
            continue
        c = dic.get((w, k))
        if c is not None:
            w = c
            continue
        put(w, bits)
        bumped = False
        if nxt == 1 << bits:
            bits += 1
            if bits > 12:
                dic, nxt, bits, bumped = {}, 0x100, 9, True
        if not bumped:
            dic[(w, k)] = nxt
            nxt += 1
        w = k
    if w != -1:
        put(w, bits)
    if temp_bits:
        out.append(temp & 0xff)
    return bytes(out)


def make_synthetic(page, eng):
    """The page with its LZ plane re-encoded as LZW (flags bit 2 cleared), or None if it has none."""
    q0, q1, q2, flags, sizes, lz, flags2, fill = hdp_emu.page_header(page)
    if flags2 & 0x40 or not flags & 4:
        return None
    present = [flags2 & b == 0 for b in (1, 2, 4, 0x10)]
    at = 16 + sum(sizes[i] for i in range(4) if present[i])
    ok, plane = eng.decompress(page[at:at + lz], lz, True)
    enc = lzw_compress(plane)
    if len(enc) > 0xffff:
        return None
    hdr = bytearray(page[:16])
    hdr[3] = flags & ~4
    hdr[10:12] = struct.pack(">H", len(enc))
    return bytes(hdr) + page[16:at] + enc + page[at + lz:]


def write_mega2(path, template, pages):
    """A minimal .mega2 holding `pages` in slots 0..n-1 (header copied from a real file)."""
    with open(template, "rb") as f:
        h = bytearray(f.read(0x170))
    n = len(pages)
    data_ofs = 0x170
    blob = b"".join(pages)
    slot_ofs = data_ofs + len(blob)
    index_ofs = slot_ofs + 16 * n
    struct.pack_into("<QQII", h, 0x38, slot_ofs, index_ofs, n, n)
    slots, o = bytearray(), data_ofs
    for p in pages:
        slots += struct.pack("<QQ", o, len(p))
        o += len(p)
    with open(path, "wb") as f:
        f.write(bytes(h) + blob + bytes(slots) + struct.pack(f"<{n}I", *range(n)))


# ---- comparison ----
def ours(doomx, mega, slot, outdir):
    os.makedirs(outdir, exist_ok=True)
    r = subprocess.run([doomx, "vt-page", os.path.abspath(mega), "--slot", str(slot), "--out", outdir], capture_output=True, text=True)
    if r.returncode != 0 or "slot" not in r.stdout or "PageHeader" not in r.stdout:
        return None, (r.stdout + r.stderr).strip()
    res = {}
    for i in range(3):
        p = os.path.join(outdir, f"{slot}_img{i}.png")
        res[f"img{i}"] = np.asarray(Image.open(p).convert("RGBA")).tobytes() if os.path.exists(p) else None
    p = os.path.join(outdir, f"{slot}_lz.png")
    res["lz"] = np.asarray(Image.open(p)).tobytes() if os.path.exists(p) else None
    for f in glob.glob(os.path.join(outdir, f"{slot}_*.png")):
        os.remove(f)
    return res, None


def compare(engine_buf, mine):
    """-> {layer: (equal, ndiff, maxdiff)}"""
    res = {}
    for i in range(3):
        e = engine_buf[i * 0x10000:(i + 1) * 0x10000]
        m = mine[f"img{i}"] if mine[f"img{i}"] is not None else bytes(0x10000)
        d = np.abs(np.frombuffer(e, np.uint8).astype(int) - np.frombuffer(m, np.uint8).astype(int))
        res[f"img{i}"] = (bool(d.max() == 0), int(np.count_nonzero(d)), int(d.max()))
    e = engine_buf[0x30000:0x34000]
    m = mine["lz"] if mine["lz"] is not None else bytes(0x4000)
    d = np.abs(np.frombuffer(e, np.uint8).astype(int) - np.frombuffer(m, np.uint8).astype(int))
    res["lz"] = (bool(d.max() == 0), int(np.count_nonzero(d)), int(d.max()))
    return res


ENG = None


def init_worker(exe):
    global ENG
    ENG = hdp_emu.Engine(exe)


def job(args):
    label, mega, slot, doomx, tmp = args
    page = hdp_emu.read_page(mega, slot)
    try:
        ebuf = ENG.decode_page(page)
    except Exception as e:  # noqa: BLE001
        return label, slot, page[:16].hex(), None, f"engine: {e}"
    mine, err = ours(doomx, mega, slot, os.path.join(tmp, f"w{os.getpid()}"))
    if mine is None:
        return label, slot, page[:16].hex(), None, f"doomx: {err}"
    return label, slot, page[:16].hex(), compare(ebuf, mine), None


def synth_job(args):
    mega, slots, tmp, idx = args
    pages = []
    for s in slots:
        p = make_synthetic(hdp_emu.read_page(mega, s), ENG)
        if p is not None:
            pages.append(p)
    if not pages:
        return None
    path = os.path.join(tmp, f"synth{idx}_{os.path.basename(mega)}")
    write_mega2(path, mega, pages)
    return path, len(pages)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--doom", default=os.environ.get("DOOM_DIR", r"C:/Program Files (x86)/Steam/steamapps/common\DOOM"))
    ap.add_argument("--exe")
    builds = [os.path.join(ROOT, "target", d, "release", "doomx.exe") for d in ("agent-hdp", ".")]
    ap.add_argument("--doomx", default=next((b for b in builds if os.path.exists(b)), builds[-1]))
    ap.add_argument("--files", default="all")
    ap.add_argument("--per-file", type=int, default=24)
    ap.add_argument("--per-variant", type=int, default=2)
    ap.add_argument("--synthetic", type=int, default=4)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--jobs", type=int, default=max(1, min(8, (os.cpu_count() or 4) // 2)))
    ap.add_argument("--keep")
    a = ap.parse_args()
    exe = a.exe or os.path.join(a.doom, "DOOMx64.exe")
    vt = os.path.join(a.doom, "virtualtextures")
    files = sorted(glob.glob(os.path.join(vt, "*.mega2")), key=lambda p: int(os.path.basename(p)[8:-6]))
    if a.files != "all":
        want = {f"_vmtr_{x}.mega2" for x in a.files.split(",")}
        files = [f for f in files if os.path.basename(f) in want]
    rng = random.Random(a.seed)
    tmp = a.keep or tempfile.mkdtemp(prefix="hdp_verify_")
    os.makedirs(tmp, exist_ok=True)
    cache = os.path.join(ROOT, "gamedata", "vt")

    jobs, synth_req = [], []
    for f in files:
        flags = scan_flags(f, cache)
        n = len(flags)
        chosen = set(rng.sample(range(n), min(a.per_file, n)))
        by_variant = {}
        for s, fl in enumerate(flags):
            by_variant.setdefault(tuple(fl), []).append(s)
        for v, slots in by_variant.items():
            chosen.update(rng.sample(slots, min(a.per_variant, len(slots))))
        jobs += [(os.path.basename(f), f, s, a.doomx, tmp) for s in sorted(chosen)]
        lz4_slots = [s for s, fl in enumerate(flags) if fl[0] & 4 and not fl[1] & 0x40]
        if a.synthetic and lz4_slots:
            synth_req.append((f, rng.sample(lz4_slots, min(a.synthetic, len(lz4_slots))), tmp, len(synth_req)))

    t0 = time.time()
    with Pool(a.jobs, initializer=init_worker, initargs=(exe,)) as pool:
        for r in pool.map(synth_job, synth_req):
            if r:
                path, k = r
                jobs += [("synthLZW:" + os.path.basename(path), path, s, a.doomx, tmp) for s in range(k)]
        print(f"{len(jobs)} pages ({sum(1 for j in jobs if j[0].startswith('synth'))} synthetic LZW) from {len(files)} files, {a.jobs} workers")
        results = pool.map(job, jobs, chunksize=1)

    layers = ["img0", "img1", "img2", "lz"]
    tot = {k: [0, 0] for k in layers}
    pages_ok, errors, bad = 0, [], []
    per_variant = {}
    for label, slot, hdr, res, err in results:
        h = bytes.fromhex(hdr)
        key = f"flags {h[3]:#04x} flags2 {h[14]:#04x}" + (" LZW" if label.startswith("synth") else "")
        pv = per_variant.setdefault(key, [0, 0])
        pv[1] += 1
        if err:
            errors.append((label, slot, err))
            continue
        ok = all(res[k][0] for k in layers)
        pages_ok += ok
        pv[0] += ok
        for k in layers:
            tot[k][0] += res[k][0]
            tot[k][1] += 1
        if not ok:
            bad.append((label, slot, hdr, {k: v for k, v in res.items() if not v[0]}))
    n = len(results)
    print(f"done in {time.time() - t0:.0f}s")
    for k in layers:
        print(f"  {k:5s} byte-exact {tot[k][0]}/{tot[k][1]}")
    for k, (ok, m) in sorted(per_variant.items()):
        print(f"  {k:28s} {ok}/{m}")
    print(f"PAGES BYTE-EXACT: {pages_ok}/{n} ({100.0 * pages_ok / max(n, 1):.2f}%), errors {len(errors)}")
    for b in bad[:20]:
        print("  mismatch", b)
    for e in errors[:20]:
        print("  error", e)
    if not a.keep:
        shutil.rmtree(tmp, ignore_errors=True)  # synthetic .mega2 files hold copies of game data
    return 0 if pages_ok == n else 1


if __name__ == "__main__":
    sys.exit(main())
