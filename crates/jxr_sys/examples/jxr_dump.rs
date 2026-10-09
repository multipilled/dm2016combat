//! Decodes headerless HD Photo page images and reports the decoder's intermediate buffers, for
//! `tools/hdp_verify.py`.
//!
//! `jxr_dump <stream.bin> <out.bin>` writes records `i32 kind, ch, col, row, n` followed by `n` i32
//! values (kinds as in `jxr_sys::DumpHook`, plus 4/5 = previous/current row buffers after each
//! inverse transform call); the RGBA output follows as kind 9.
//! `jxr_dump --stats <stream.bin>...` prints, per stream, the largest magnitude seen in the
//! coefficients, the row buffers after each transform call and the pixels before colour conversion
//! (the engine runs the transform in 16-bit lanes, so these must stay well inside i16).
use std::io::Write;
use std::sync::Mutex;

static OUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static MAX: Mutex<[i32; 6]> = Mutex::new([0; 6]);

fn record(kind: i32, ch: i32, col: i32, row: i32, vals: &[i32]) {
    let mut o = OUT.lock().unwrap();
    for v in [kind, ch, col, row, vals.len() as i32] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for v in vals {
        o.extend_from_slice(&v.to_le_bytes());
    }
}

extern "C" fn hook(kind: i32, ch: i32, col: i32, row: i32, data: *const i32, n: i32) {
    // SAFETY: the decoder passes a buffer of `n` PixelI values.
    let vals = unsafe { std::slice::from_raw_parts(data, n as usize) };
    record(kind, ch, col, row, vals);
}

extern "C" fn stats_hook(kind: i32, _ch: i32, _col: i32, _row: i32, data: *const i32, n: i32) {
    // SAFETY: as above.
    let vals = unsafe { std::slice::from_raw_parts(data, n as usize) };
    let m = vals.iter().map(|v| v.unsigned_abs() as i32).max().unwrap_or(0);
    let mut mx = MAX.lock().unwrap();
    mx[kind as usize] = mx[kind as usize].max(m);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[1] == "--stats" {
        jxr_sys::set_dump_hook(Some(stats_hook));
        for f in &args[2..] {
            *MAX.lock().unwrap() = [0; 6];
            let data = std::fs::read(f).expect("read stream");
            let ok = jxr_sys::decode_headerless(&data, 128, 128, 1, true).is_some();
            let m = *MAX.lock().unwrap();
            println!("{f} ok={ok} coef={} transform={} pixels={}", m[0].max(m[1]), m[4].max(m[5]), m[2].max(m[3]));
        }
        return;
    }
    let data = std::fs::read(&args[1]).expect("read stream");
    jxr_sys::set_dump_hook(Some(hook));
    let rgba = jxr_sys::decode_headerless(&data, 128, 128, 1, true).expect("decode failed");
    jxr_sys::set_dump_hook(None);
    record(9, 0, 0, 0, &rgba.iter().map(|&b| b as i32).collect::<Vec<_>>());
    std::fs::File::create(&args[2]).unwrap().write_all(&OUT.lock().unwrap()).unwrap();
}
