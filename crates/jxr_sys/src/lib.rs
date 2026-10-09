//! Microsoft's jxrlib decoder (vendored, BSD-2-Clause, see `vendor/LICENSE-jxrlib.md`) with a shim
//! for headerless HD Photo codestreams as stored in DOOM (2016) virtual-texture pages.
//!
//! The vendored sources are patched (comments marked `dm2016combat`) so the output is byte-identical
//! to the engine's own HD Photo decoder (DOOMx64.exe 0x141affcf0): per-channel adaptive models, the
//! original HD Photo post-filter scaling without DC-leak compensation, the 2x2 corner filters,
//! unshifted chroma DC/LP quantisers, the engine's alpha rounding, and two upstream memory leaks fixed.

unsafe extern "C" {
    fn jxr_decode_headerless(data: *const u8, len: usize, width: u32, height: u32, overlap: u32, alpha: i32, out: *mut u8, stride: usize) -> i32;
}

/// Decodes a codestream that starts at its image-plane header (image header stripped) to RGBA8.
/// `alpha` says whether an interleaved alpha plane header follows the colour plane header.
pub fn decode_headerless(data: &[u8], width: u32, height: u32, overlap: u32, alpha: bool) -> Option<Vec<u8>> {
    let stride = width as usize * 4;
    let mut out = vec![0u8; stride * height as usize];
    // SAFETY: `out` holds `height` rows of `stride` bytes; the shim copies `data` before decoding.
    let rc = unsafe { jxr_decode_headerless(data.as_ptr(), data.len(), width, height, overlap, alpha as i32, out.as_mut_ptr(), stride) };
    (rc == 0).then_some(out)
}

/// Debug hook for differential testing against the engine (`tools/hdp_verify.py`): called with
/// kind 0/1 (colour/alpha coefficients entering the inverse transform, per macroblock and channel),
/// kind 2/3 (colour/alpha pixels entering colour conversion, per macroblock row) and kind 4/5
/// (previous/current row buffers after each inverse-transform call).
pub type DumpHook = extern "C" fn(kind: i32, ch: i32, col: i32, row: i32, data: *const i32, n: i32);

unsafe extern "C" {
    static mut jxr_dr_dump: Option<DumpHook>;
}

/// Installs (or clears) the debug hook. Not thread-safe; for offline tools only.
pub fn set_dump_hook(hook: Option<DumpHook>) {
    // SAFETY: plain store to a C global that is only read during decoding.
    unsafe { jxr_dr_dump = hook };
}
