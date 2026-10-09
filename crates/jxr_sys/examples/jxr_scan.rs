//! Headroom scan over every page image of an install: the engine runs the HD Photo inverse
//! transform in 16-bit lanes (exact while every lane value fits in i16; products are split to avoid
//! overflow), jxrlib in i32. This decodes every image with jxr_sys and reports the largest
//! dequantised coefficient and the largest stage-2 lane value (the only stage near the limit), so
//! any image where the two decoders could diverge is listed. Used by `tools/hdp_verify.py`.
//!
//! `jxr_scan <virtualtextures dir> [threads] [threshold]`
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    static COEF: Cell<i64> = const { Cell::new(0) };
    static LANE: Cell<i64> = const { Cell::new(0) };
}

/// strIDCT4x4Stage2 (DOOMx64 0x141b3dbb0) in exact arithmetic, returning the largest |lane| value.
fn stage2_lanes(p: &mut [i64; 256]) -> i64 {
    let mut m = 0i64;
    let mut t = |v: i64| {
        m = m.max(v.abs());
        v
    };
    for [ia, ib, ic, id] in [[32, 48, 96, 112], [128, 192, 144, 208]] {
        // invOdd
        let (mut a, mut b, mut c, mut d) = (p[ia], p[ib], p[ic], p[id]);
        b = t(b + d);
        a = t(a - c);
        d = t(d - (b >> 1));
        c = t(c + ((a + 1) >> 1));
        a = t(a - ((b * 3 + 4) >> 3));
        b = t(b + ((a * 3 + 4) >> 3));
        c = t(c - ((d * 3 + 4) >> 3));
        d = t(d + ((c * 3 + 4) >> 3));
        c = t(c - ((b + 1) >> 1));
        d = t(((a + 1) >> 1) - d);
        b = t(b + c);
        a = t(a - d);
        (p[ia], p[ib], p[ic], p[id]) = (a, b, c, d);
    }
    {
        // invOddOdd(160, 224, 176, 240)
        let (mut a, mut b, mut c, mut d) = (p[160], p[224], p[176], p[240]);
        d = t(d + a);
        c = t(c - b);
        let t1 = t(d >> 1);
        a = t(a - t1);
        let t2 = t(c >> 1);
        b = t(b + t2);
        a = t(a - ((b * 3 + 3) >> 3));
        b = t(b + ((a * 3 + 3) >> 2));
        a = t(a - ((b * 3 + 4) >> 3));
        b = t(b - t2);
        a = t(a + t1);
        c = t(c + b);
        d = t(d - a);
        (p[160], p[224], p[176], p[240]) = (a, -b, -c, d);
    }
    // strDCT2x2up, then FOURBUTTERFLY (strDCT2x2dn x4)
    for (q, up) in [([0, 64, 16, 80], 1), ([0, 192, 48, 240], 0), ([64, 128, 112, 176], 0), ([16, 208, 32, 224], 0), ([80, 144, 96, 160], 0)] {
        let (mut a, mut b, cc, mut d) = (p[q[0]], p[q[1]], p[q[2]], p[q[3]]);
        a = t(a + d);
        b = t(b - cc);
        let tt = t((a - b + up) >> 1);
        let c = t(tt - d);
        d = t(tt - cc);
        a = t(a - d);
        b = t(b + c);
        (p[q[0]], p[q[1]], p[q[2]], p[q[3]]) = (a, b, c, d);
    }
    m
}

extern "C" fn hook(kind: i32, _ch: i32, _col: i32, _row: i32, data: *const i32, n: i32) {
    if kind > 1 {
        return;
    }
    // SAFETY: the decoder passes `n` PixelI values.
    let v = unsafe { std::slice::from_raw_parts(data, n as usize) };
    let mut p = [0i64; 256];
    let mut cm = 0i64;
    for (d, &s) in p.iter_mut().zip(v) {
        *d = s as i64;
        cm = cm.max((s as i64).abs());
    }
    COEF.set(COEF.get().max(cm));
    LANE.set(LANE.get().max(stage2_lanes(&mut p)));
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

#[cfg(windows)]
fn read_at_impl(f: &std::fs::File, buf: &mut [u8], ofs: u64) -> usize {
    use std::os::windows::fs::FileExt;
    f.seek_read(buf, ofs).unwrap()
}
#[cfg(unix)]
fn read_at_impl(f: &std::fs::File, buf: &mut [u8], ofs: u64) -> usize {
    use std::os::unix::fs::FileExt;
    f.read_at(buf, ofs).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&args[1]);
    let threads: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let threshold: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30000);
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "mega2")).collect();
    files.sort();
    jxr_sys::set_dump_hook(Some(hook));
    let (mut gc, mut gl, mut gimages, mut gfail) = (0i64, 0i64, 0usize, 0usize);
    for f in files {
        let file = std::fs::File::open(&f).unwrap();
        let read_at = |ofs: u64, len: usize| {
            let mut b = vec![0u8; len];
            let mut done = 0;
            while done < len {
                done += read_at_impl(&file, &mut b[done..], ofs + done as u64);
            }
            b
        };
        let head = read_at(0, 0x170);
        let (slot_ofs, nslots) = (le64(&head, 0x38), le32(&head, 0x48) as usize);
        let slots = read_at(slot_ofs, nslots * 16);
        let next = AtomicUsize::new(0);
        let results: Vec<(i64, i64, usize, usize, Vec<String>)> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let (mut c, mut l, mut n, mut fail, mut hot) = (0i64, 0i64, 0usize, 0usize, Vec::new());
                        loop {
                            let slot = next.fetch_add(1, Ordering::Relaxed);
                            if slot >= nslots {
                                break;
                            }
                            let page = read_at(le64(&slots, 16 * slot), le64(&slots, 16 * slot + 8) as usize);
                            let be = |k: usize| u16::from_be_bytes([page[k], page[k + 1]]) as usize;
                            let flags2 = page[14];
                            if flags2 & 0x80 != 0 {
                                continue;
                            }
                            let sizes = [be(4), be(6), be(8)];
                            let mut at = 16;
                            for (i, &size) in sizes.iter().enumerate() {
                                if flags2 & (1 << i) != 0 {
                                    continue;
                                }
                                COEF.set(0);
                                LANE.set(0);
                                let ok = jxr_sys::decode_headerless(&page[at..at + size], 128, 128, 1, true).is_some();
                                at += size;
                                n += 1;
                                fail += !ok as usize;
                                let (ci, li) = (COEF.get(), LANE.get());
                                if ci >= threshold || li >= threshold || !ok {
                                    hot.push(format!("{} slot {slot} image {i}: coef {ci} lane {li} ok {ok}", f.file_name().unwrap().to_string_lossy()));
                                }
                                c = c.max(ci);
                                l = l.max(li);
                            }
                        }
                        (c, l, n, fail, hot)
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let (mut c, mut l, mut n, mut fail) = (0, 0, 0, 0);
        for (rc, rl, rn, rf, hot) in results {
            c = c.max(rc);
            l = l.max(rl);
            n += rn;
            fail += rf;
            for h in hot {
                println!("  {h}");
            }
        }
        println!("{}: {n} images, {fail} failed, max coef {c}, max stage-2 lane {l}", f.file_name().unwrap().to_string_lossy());
        (gc, gl, gimages, gfail) = (gc.max(c), gl.max(l), gimages + n, gfail + fail);
    }
    println!("ALL: {gimages} images, {gfail} failed, max coef {gc}, max stage-2 lane {gl} (i16 limit 32767)");
}
