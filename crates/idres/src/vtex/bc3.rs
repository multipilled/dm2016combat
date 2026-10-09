//! BC3 (DXT5) encoder matching the one the engine runs on decoded virtual-texture pages: Intel's
//! ISPC Texture Compressor `CompressBlocksBC3` (MIT; kernel.ispc) as linked into DOOMx64.exe
//! (dispatcher 0x141ec2cf0, called by the transcode job via 0x141a69610 for page layers 0..2).
//!
//! The kernel was built with fast math: divisions are `rcp` estimates refined by one Newton step,
//! `1/sqrt` is an `rsqrt` estimate plus one step, and the AVX2 variant fuses multiply-adds. So the
//! output depends on the CPU: like the game, this picks the variant from the same CPUID test (ISPC
//! ISA level >= 4 -> AVX2 + FMA, else the SSE/AVX build without FMA) and uses the CPU's own
//! `rcpss`/`rsqrtss`. Per block the arithmetic is scalar but in the kernel's exact operation order.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Floating-point primitives of one kernel build.
trait Math {
    /// a * b + c
    fn madd(a: f32, b: f32, c: f32) -> f32;
    /// c - a * b
    fn nmadd(a: f32, b: f32, c: f32) -> f32;
    /// a * b - c
    fn msub(a: f32, b: f32, c: f32) -> f32;
    fn rcp(x: f32) -> f32;
    fn rsqrt(x: f32) -> f32;
}

#[cfg(target_arch = "x86_64")]
fn rcp_est(x: f32) -> f32 {
    // SAFETY: SSE is part of the x86_64 baseline.
    unsafe { _mm_cvtss_f32(_mm_rcp_ss(_mm_set_ss(x))) }
}

#[cfg(target_arch = "x86_64")]
fn rsqrt_est(x: f32) -> f32 {
    // SAFETY: as above.
    unsafe { _mm_cvtss_f32(_mm_rsqrt_ss(_mm_set_ss(x))) }
}

/// AVX2 build: fused multiply-adds (correctly rounded, as `vfmadd*ps`).
struct Fused;
impl Math for Fused {
    #[inline(always)]
    fn madd(a: f32, b: f32, c: f32) -> f32 {
        a.mul_add(b, c)
    }
    #[inline(always)]
    fn nmadd(a: f32, b: f32, c: f32) -> f32 {
        (-a).mul_add(b, c)
    }
    #[inline(always)]
    fn msub(a: f32, b: f32, c: f32) -> f32 {
        a.mul_add(b, -c)
    }
    #[inline(always)]
    fn rcp(x: f32) -> f32 {
        // ISPC __rcp_varying_float: iv * (2 - v * iv), the inner term fused
        let iv = rcp_est(x);
        iv * Self::nmadd(x, iv, 2.0)
    }
    #[inline(always)]
    fn rsqrt(x: f32) -> f32 {
        // ISPC __rsqrt_varying_float: 0.5 * (is * (3 - (v * is) * is))
        let is = rsqrt_est(x);
        0.5 * (is * Self::nmadd(x * is, is, 3.0))
    }
}

/// SSE2 / SSE4 / AVX builds: separate multiply and add.
struct Unfused;
impl Math for Unfused {
    #[inline(always)]
    fn madd(a: f32, b: f32, c: f32) -> f32 {
        a * b + c
    }
    #[inline(always)]
    fn nmadd(a: f32, b: f32, c: f32) -> f32 {
        c - a * b
    }
    #[inline(always)]
    fn msub(a: f32, b: f32, c: f32) -> f32 {
        a * b - c
    }
    #[inline(always)]
    fn rcp(x: f32) -> f32 {
        let iv = rcp_est(x);
        iv * (2.0 - x * iv)
    }
    #[inline(always)]
    fn rsqrt(x: f32) -> f32 {
        let is = rsqrt_est(x);
        0.5 * (is * (3.0 - (x * is) * is))
    }
}

/// `minps` / `maxps` operand semantics.
#[inline(always)]
fn minf(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}
#[inline(always)]
fn maxf(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}
#[inline(always)]
fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    minf(maxf(v, lo), hi)
}
/// `cvttps2dq`: truncation, 0x80000000 for out-of-range input.
#[inline(always)]
fn trunc_i(v: f32) -> i32 {
    if v.is_nan() || v >= 2147483648.0 || v < -2147483648.0 { i32::MIN } else { v as i32 }
}

fn mul8bit(a: i32, b: i32) -> i32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

fn enc_rgb565(c: [f32; 3]) -> i32 {
    let (r, g, b) = (trunc_i(c[0]), trunc_i(c[1]), trunc_i(c[2]));
    ((mul8bit(r, 31) << 11) + (mul8bit(g, 63) << 5) + mul8bit(b, 31)) & 0xffff
}

fn dec_rgb565(p: i32) -> [f32; 3] {
    let (c2, c1, c0) = (p & 31, (p >> 5) & 63, (p >> 11) & 31);
    [((c0 << 3) + (c0 >> 2)) as f32, ((c1 << 2) + (c1 >> 4)) as f32, ((c2 << 3) + (c2 >> 2)) as f32]
}

/// `block[p * 16 + k]` for channel p of texel k.
struct Block([f32; 64]);

fn alpha<M: Math>(b: &Block) -> [u32; 2] {
    let a = &b.0[48..64];
    let (mut ep0, mut ep1) = (255.0f32, 0.0f32);
    for &v in a {
        ep0 = minf(ep0, v);
        ep1 = maxf(ep1, v);
    }
    if ep0 == ep1 {
        ep1 = ep0 + 0.1;
    }
    let scale = 7.0 * M::rcp(ep1 - ep0);
    let mut q = [0u32; 2];
    for (k, &v) in a.iter().enumerate() {
        let proj = M::madd(v - ep0, scale, 0.5);
        let mut i = 7 - trunc_i(proj).clamp(0, 7);
        if i > 0 {
            i += 1;
        }
        if i == 8 {
            i = 1;
        }
        q[k / 8] |= (i as u32) << ((k % 8) * 3);
    }
    let mut d0 = (trunc_i(ep0).clamp(0, 255) * 256 + trunc_i(ep1).clamp(0, 255)) as u32;
    d0 |= q[0] << 16;
    [d0, (q[0] >> 16) | (q[1] << 8)]
}

/// x0*y0 + x1*y1 + x2*y2 as the kernel evaluates it: a running sum from 0.
#[inline(always)]
fn dot3<M: Math>(x: [f32; 3], y: [f32; 3]) -> f32 {
    M::madd(x[2], y[2], M::madd(x[1], y[1], M::madd(x[0], y[0], 0.0)))
}

fn covar_dc<M: Math>(b: &Block) -> ([f32; 6], [f32; 3]) {
    let mut dc = [0.0f32; 3];
    for (p, d) in dc.iter_mut().enumerate() {
        let acc: f32 = b.0[p * 16..p * 16 + 16].iter().sum();
        *d = acc * (1.0 / 16.0);
    }
    let mut c = [0.0f32; 6];
    for k in 0..16 {
        let r = [b.0[k] - dc[0], b.0[k + 16] - dc[1], b.0[k + 32] - dc[2]];
        if k == 0 {
            c = [r[0] * r[0], r[0] * r[1], r[0] * r[2], r[1] * r[1], r[1] * r[2], r[2] * r[2]];
        } else {
            c[0] = M::madd(r[0], r[0], c[0]);
            c[1] = M::madd(r[0], r[1], c[1]);
            c[2] = M::madd(r[0], r[2], c[2]);
            c[3] = M::madd(r[1], r[1], c[3]);
            c[4] = M::madd(r[1], r[2], c[4]);
            c[5] = M::madd(r[2], r[2], c[5]);
        }
    }
    (c, dc)
}

/// Covariance row times vector: x0*y0 + x1*y1 + x2*y2 contracted pairwise.
#[inline(always)]
fn row3<M: Math>(x: [f32; 3], y: [f32; 3]) -> f32 {
    M::madd(x[2], y[2], M::madd(x[0], y[0], x[1] * y[1]))
}

fn axis3<M: Math>(c: &[f32; 6]) -> [f32; 3] {
    // iteration 0 starts from (1, 1, 1)
    let mut v = [c[0] + c[1] + c[2], c[1] + c[3] + c[4], c[2] + c[4] + c[5]];
    for i in 1..4 {
        let a = [row3::<M>([c[0], c[1], c[2]], v), row3::<M>([c[1], c[3], c[4]], v), row3::<M>([c[2], c[4], c[5]], v)];
        v = a;
        if i % 2 == 1 {
            let rn = M::rsqrt(dot3::<M>(a, a));
            v = [a[0] * rn, a[1] * rn, a[2] * rn];
        }
    }
    v
}

fn pick_endpoints<M: Math>(b: &Block, axis: [f32; 3], dc: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let (mut lo, mut hi) = (65536.0f32, 0.0f32);
    for k in 0..16 {
        let d = dot3::<M>([b.0[k] - dc[0], b.0[k + 16] - dc[1], b.0[k + 32] - dc[2]], axis);
        lo = minf(lo, d);
        hi = maxf(hi, d);
    }
    if hi - lo < 1.0 {
        lo -= 0.5;
        hi += 0.5;
    }
    let rn = M::rcp(dot3::<M>(axis, axis));
    let (s0, s1) = (lo * rn, hi * rn);
    let mut c0 = [0.0; 3];
    let mut c1 = [0.0; 3];
    for p in 0..3 {
        c0[p] = clampf(M::madd(s0, axis[p], dc[p]), 0.0, 255.0);
        c1[p] = clampf(M::madd(s1, axis[p], dc[p]), 0.0, 255.0);
    }
    (c0, c1)
}

fn fast_quant<M: Math>(b: &Block, p0: i32, p1: i32) -> u32 {
    let c0 = dec_rgb565(p0);
    let c1 = dec_rgb565(p1);
    let mut dir = [c1[0] - c0[0], c1[1] - c0[1], c1[2] - c0[2]];
    let s = M::rcp(dot3::<M>(dir, dir)) * 3.0;
    for d in &mut dir {
        *d *= s;
    }
    let mut bias = 0.5f32;
    for p in 0..3 {
        bias = M::nmadd(c0[p], dir[p], bias);
    }
    let mut bits = 0u32;
    for k in 0..16 {
        let dot = dot3::<M>([b.0[k], b.0[k + 16], b.0[k + 32]], dir);
        let q = trunc_i(dot + bias).clamp(0, 3) as u32;
        bits |= q << (2 * k);
    }
    bits
}

fn refine<M: Math>(b: &Block, bits: u32, dc: [f32; 3]) -> [i32; 2] {
    let (c0, c1) = if (bits ^ bits.wrapping_mul(4)) < 4 {
        (dc, dc)
    } else {
        let mut atb1 = [0.0f32; 3];
        let (mut sum_q, mut sum_qq) = (0.0f32, 0.0f32);
        for k in 0..16 {
            let q = ((bits >> (2 * k)) & 3) as f32;
            let x = 3.0 - q;
            if k == 0 {
                sum_q = q;
                sum_qq = q * q;
                for p in 0..3 {
                    atb1[p] = x * b.0[k + p * 16];
                }
            } else {
                sum_q += q;
                sum_qq = M::madd(q, q, sum_qq);
                for p in 0..3 {
                    atb1[p] = M::madd(x, b.0[k + p * 16], atb1[p]);
                }
            }
        }
        let mut atb2 = [0.0f32; 3];
        for p in 0..3 {
            atb2[p] = M::msub(3.0, dc[p] * 16.0, atb1[p]);
        }
        let cxx = M::nmadd(6.0, sum_q, 144.0) + sum_qq;
        let cyy = sum_qq;
        let cxy = M::msub(3.0, sum_q, sum_qq);
        let scale = 3.0 * M::rcp(M::msub(cxx, cyy, cxy * cxy));
        let mut c0 = [0.0; 3];
        let mut c1 = [0.0; 3];
        for p in 0..3 {
            c0[p] = clampf(M::msub(atb1[p], cyy, atb2[p] * cxy) * scale, 0.0, 255.0);
            c1[p] = clampf(M::msub(atb2[p], cxx, atb1[p] * cxy) * scale, 0.0, 255.0);
        }
        (c0, c1)
    };
    [enc_rgb565(c0), enc_rgb565(c1)]
}

fn fix_qbits(q: u32) -> u32 {
    let q0 = q & 0x5555_5555;
    let q1 = q & 0xaaaa_aaaa;
    (q1 >> 1).wrapping_add(q1 ^ (q0 << 1))
}

fn color<M: Math>(b: &Block) -> [u32; 2] {
    let (mut c, dc) = covar_dc::<M>(b);
    c[0] += 0.001;
    c[3] += 0.001;
    c[5] += 0.001;
    let axis = axis3::<M>(&c);
    let (e0, e1) = pick_endpoints::<M>(b, axis, dc);
    let mut p = [enc_rgb565(e0), enc_rgb565(e1)];
    if p[0] < p[1] {
        p.swap(0, 1);
    }
    let mut bits = fast_quant::<M>(b, p[0], p[1]);
    // one refinement iteration
    p = refine::<M>(b, bits, dc);
    if p[0] < p[1] {
        p.swap(0, 1);
    }
    let d0 = ((p[1] as u32) << 16).wrapping_add(p[0] as u32);
    bits = fast_quant::<M>(b, p[0], p[1]);
    [d0, fix_qbits(bits)]
}

fn block<M: Math>(rgba: &[u8], stride: usize, bx: usize, by: usize, out: &mut [u8]) {
    let mut b = Block([0.0; 64]);
    for y in 0..4 {
        for x in 0..4 {
            let o = (by * 4 + y) * stride + (bx * 4 + x) * 4;
            for p in 0..4 {
                b.0[p * 16 + y * 4 + x] = rgba[o + p] as f32;
            }
        }
    }
    let a = alpha::<M>(&b);
    let c = color::<M>(&b);
    for (i, w) in [a[0], a[1], c[0], c[1]].into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
}

/// Which kernel build the game would run on this CPU (ISPC ISA detection, DOOMx64 0x140001000).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// ISA level >= 4 (AVX2 + F16C + RDRAND): fused multiply-adds.
    Avx2,
    /// SSE2 / SSE4 / AVX builds.
    Sse,
}

impl Variant {
    pub fn detect() -> Variant {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: cpuid/xgetbv are available on every x86_64 CPU (xgetbv only behind OSXSAVE).
            unsafe {
                let l1 = __cpuid(1);
                let l7 = __cpuid_count(7, 0);
                let osxsave = l1.ecx & (1 << 27) != 0;
                if osxsave && l1.ecx & 0x1800_0000 == 0x1800_0000 && _xgetbv(0) & 6 == 6 && l1.ecx & 0x6000_0000 == 0x6000_0000 && l7.ebx & (1 << 5) != 0 {
                    return Variant::Avx2;
                }
                // AVX-512 levels (5/6) also select the AVX2 build
                if osxsave && l7.ebx & 0x10020 == 0x10020 && _xgetbv(0) & 0xe6 == 0xe6 {
                    return Variant::Avx2;
                }
            }
        }
        Variant::Sse
    }
}

/// Compresses a `width` x `height` RGBA8 image (multiples of 4, rows `stride` bytes apart) to BC3
/// blocks, row-major, 16 bytes per 4x4 block.
pub fn compress(rgba: &[u8], width: usize, height: usize, stride: usize, variant: Variant) -> Vec<u8> {
    let (bw, bh) = (width / 4, height / 4);
    let mut out = vec![0u8; bw * bh * 16];
    for by in 0..bh {
        for bx in 0..bw {
            let o = (by * bw + bx) * 16;
            match variant {
                Variant::Avx2 => block::<Fused>(rgba, stride, bx, by, &mut out[o..o + 16]),
                Variant::Sse => block::<Unfused>(rgba, stride, bx, by, &mut out[o..o + 16]),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks against references produced by the engine's own kernels on this machine
    /// (`tools/vt_bc_oracle.py <exe> vectors gamedata/vt/bc3_vectors.bin`); skipped without them.
    #[test]
    fn matches_engine_kernels() {
        let path = std::env::var("VT_BC3_VECTORS").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../gamedata/vt/bc3_vectors.bin").into());
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("no {path}; skipped");
            return;
        };
        let (mut o, mut tiles) = (0usize, 0);
        let mut bad = [0usize; 2];
        let mut blocks = 0;
        while o < data.len() {
            let w = u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
            let h = u32::from_le_bytes(data[o + 4..o + 8].try_into().unwrap()) as usize;
            o += 8;
            let px = &data[o..o + w * h * 4];
            o += w * h * 4;
            let n = (w / 4) * (h / 4) * 16;
            for (i, v) in [Variant::Avx2, Variant::Sse].into_iter().enumerate() {
                let reference = &data[o..o + n];
                o += n;
                let got = compress(px, w, h, w * 4, v);
                for (b, (g, r)) in got.chunks(16).zip(reference.chunks(16)).enumerate() {
                    if g != r {
                        if bad[i] < 4 {
                            eprintln!("{v:?} tile {tiles} block {b}: got {:02x?} want {:02x?}", g, r);
                        }
                        bad[i] += 1;
                    }
                }
            }
            blocks += n / 16;
            tiles += 1;
        }
        eprintln!("{tiles} tiles, {blocks} blocks: avx2 mismatches {}, sse mismatches {}", bad[0], bad[1]);
        assert_eq!(bad, [0, 0]);
    }
}
