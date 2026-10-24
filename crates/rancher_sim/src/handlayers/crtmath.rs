//! The C runtime's `sinf` / `cosf` as the exe links them (0x141ed3170 / 0x141ed2c70), ported from their non-FMA
//! branch: the float widened to double, reduced by multiples of pi/2, a double polynomial, one rounding to float.
//! The float `sinf` of the dev machine's libm differs from these by 1 ULP on some arguments, which the weapon-lag
//! pendulum grows to 25 ULPs within 3 frames, so the hands code calls these instead.
//!
//! Every operation keeps the exe's order (no fused multiply-adds; Rust never contracts `a * b + c`).
//! `tools/crt_sincos_sweep.py` compares both against the exe under emulation.

/// pi/4 as double bits: at or below it no reduction (immediate in 0x141ed31b8 / 0x141ed2cb8).
const PIO4_BITS: u64 = 0x3fe9_21fb_5444_2d18;
/// sinf returns its argument below 2^-13 (0x141ed31e4) and uses the cubic term only below 2^-7 (0x141ed31d5).
const SIN_TINY_BITS: u64 = 0x3f20_0000_0000_0000;
const SIN_SMALL_BITS: u64 = 0x3f80_0000_0000_0000;

/// Medium-range reduction constants; sinf reads them at 0x142a6d0.., cosf at its own copies 0x142a6cf..
/// (same values).
const TWO_BY_PI: f64 = f64::from_bits(0x3fe4_5f30_6dc9_c883); // 0x142a6d070 / 0x142a6cf20
const HALF: f64 = 0.5; // 0x142a6d060 / 0x142a6cf10
const PIBY2_1: f64 = f64::from_bits(0x3ff9_21fb_5440_0000); // 0x142a6d080 / 0x142a6cf50
const PIBY2_1TAIL: f64 = f64::from_bits(0x3dd0_b461_1a62_6331); // 0x142a6d0a0 / 0x142a6cf60
const PIBY2_2: f64 = f64::from_bits(0x3dd0_b461_1a60_0000); // 0x142a6d0b0 / 0x142a6cf70
const PIBY2_2TAIL: f64 = f64::from_bits(0x3ba3_198a_2e03_7073); // 0x142a6d0c0 / 0x142a6cf80
const ONE: f64 = 1.0; // 0x142a6d040 / 0x142a6cf00
const SIXTH: f64 = f64::from_bits(0x3fc5_5555_5555_5555); // 0x142a6d090 / 0x142a6cf30
/// Arguments at or above these go through the large reduction: sinf 16000000.0 (0x142a6d138), cosf 500000.0
/// (0x142a6cf90).
const SIN_LARGE_BITS: u64 = 0x416e_8480_0000_0000;
const COS_LARGE_BITS: u64 = 0x411e_8480_0000_0000;

/// Polynomial coefficients shared by both (0x142a72f40..0x142a72f88).
const S1: f64 = f64::from_bits(0xbfc5_5555_5555_5555); // 0x142a72f70
const S2: f64 = f64::from_bits(0x3f81_1111_1111_1111); // 0x142a72f78
const S3: f64 = f64::from_bits(0xbf2a_01a0_1a01_a01a); // 0x142a72f80
const S4: f64 = f64::from_bits(0x3ec7_1de3_a556_c734); // 0x142a72f88
const C1: f64 = -0.5; // 0x142a72f40
const C2: f64 = f64::from_bits(0x3fa5_5555_5555_5555); // 0x142a72f48
const C3: f64 = f64::from_bits(0xbf56_c16c_16c1_6c16); // 0x142a72f50
const C4: f64 = f64::from_bits(0x3efa_01a0_1a01_a019); // 0x142a72f58
const C5: f64 = f64::from_bits(0xbe92_7e4f_b778_9f5c); // 0x142a72f60

/// The large reduction's pi/2 (0x142a72f90 / 0x142a72dd0).
const PIBY2: f64 = f64::from_bits(0x3ff9_21fb_5444_2d18);
/// The bits of 2/pi the large reduction reads (its tables 0x142a72fa0 / 0x142a72d10, bytes 0x77..0x9f, which hold
/// the first 256 bits of 2/pi, little-endian, then zeros). Index 0 is table byte 0x77.
const PIBITS_FIRST: usize = 0x77;
const PIBITS: [u8; 41] = {
    // 2/pi = 0x0.a2f9836e4e441529fc2757d1f534ddc0db6295993c439041fe5163abdebbc56..., i.e. these 256 bits
    // shifted right by 6 (0x028be60d...).
    let be: [u8; 32] = [
        0x02, 0x8b, 0xe6, 0x0d, 0xb9, 0x39, 0x10, 0x54, 0xa7, 0xf0, 0x9d, 0x5f, 0x47, 0xd4, 0xd3, 0x77, 0x03, 0x6d,
        0x8a, 0x56, 0x64, 0xf1, 0x0e, 0x41, 0x07, 0xf9, 0x45, 0x8e, 0xaf, 0x7a, 0xef, 0x15,
    ];
    let mut t = [0u8; 41];
    let mut i = 0;
    while i < 32 {
        t[i] = be[31 - i];
        i += 1;
    }
    t
};

/// `sin` on a reduced argument |r| <= pi/4 (0x141ed342b..0x141ed346f).
#[inline]
fn sin_poly(r: f64) -> f64 {
    let x2 = r * r;
    let x4 = x2 * x2;
    let a = (S4 * x2 + S3) * x4;
    let b = S2 * x2 + S1;
    r + (a + b) * (r * x2)
}

/// `cos` on a reduced argument |r| <= pi/4 (0x141ed3480..0x141ed34d0).
#[inline]
fn cos_poly(r: f64) -> f64 {
    let x2 = r * r;
    let x4 = x2 * x2;
    let a = (C5 * x2 + C4) * x4;
    let b = C3 * x2 + C2;
    (C1 * x2 + ONE) + (a + b) * x4
}

/// The quadrant `n` and remainder `r = x - n * pi/2` for 0 <= x below the large threshold (0x141ed32c6..
/// 0x141ed3366), and the exponent of the first remainder, which picks the polynomial.
fn reduce_medium(x: f64) -> (f64, u32, u64) {
    let n = (x * TWO_BY_PI + HALF) as i32;
    let dn = n as f64;
    let xexp = x.to_bits() >> 52;
    let mut rhead = x - PIBY2_1 * dn;
    let mut r = rhead - PIBY2_1TAIL * dn;
    let rexp = (r.to_bits() << 1) >> 53;
    if xexp as i64 - rexp as i64 > 15 {
        // Cancellation: a second, longer pi/2 split.
        let t = rhead;
        let rtail = PIBY2_2 * dn;
        rhead = t - rtail;
        let rtail = PIBY2_2TAIL * dn - ((t - rhead) - rtail);
        r = rhead - rtail;
    }
    (r, n as u32, rexp)
}

/// Payne-Hanek reduction for large x (0x141ee7890 for sinf, 0x141ee74d0 for cosf): x times 2/pi in 192-bit
/// fixed point, the quadrant from the integer bits, the fraction times pi/2.
fn reduce_large(x: f64) -> (f64, u32) {
    let read = |off: usize| {
        let i = off - PIBITS_FIRST;
        u64::from_le_bytes(PIBITS[i..i + 8].try_into().unwrap())
    };
    let bits = x.to_bits();
    let e = (bits >> 52) as i64 - 0x3ff;
    let off = (0x86 - (e >> 3)) as usize;
    let m = (bits & ((1 << 52) - 1)) | (1 << 52);
    let p0 = read(off) as u128 * m as u128;
    let p1 = read(off + 8) as u128 * m as u128;
    let mut w0 = p0 as u64;
    let (lo, carry) = (p1 as u64).overflowing_add((p0 >> 64) as u64);
    let mut w1 = lo;
    let mut w2 = ((p1 >> 64) as u64 + carry as u64).wrapping_add(read(off + 16).wrapping_mul(m));
    let e7 = (e & 7) as u32;
    let shift = 54 - e7;
    let quadrant = w2 >> shift;
    let round = (w2 >> (shift - 1)) & 1;
    let mut sign = 0;
    if round == 1 {
        // Past the half: count from the next quadrant, with the fraction negated.
        w2 = !w2;
        w1 = !w1;
        w0 = !w0;
        sign = 1 << 63;
    }
    let region = ((quadrant + round) & 3) as u32;
    let c = e7 + 10;
    w2 = (w2 << c) >> c;
    let mut exp = c as i64 - 64;
    if w2 == 0 {
        w2 = w1;
        w1 = w0;
        exp -= 64;
    }
    let top = 63 - w2.leading_zeros() as i64;
    exp += top;
    let s = top - 52;
    if s > 0 {
        w2 >>= s;
    } else if s < 0 {
        let c = -s as u32;
        w2 = (w2 << c) | (w1 >> (64 - c));
    }
    let f = (w2 & !(1 << 52)) | sign | (((exp + 0x3ff) as u64) << 52);
    (f64::from_bits(f) * PIBY2, region)
}

/// Infinity gives the default NaN, a NaN comes back quiet (0x141ee7330).
fn special(u: u32) -> f32 {
    if u & 0x007f_ffff == 0 { f32::from_bits(0xffc0_0000) } else { f32::from_bits(u | 0x0040_0000) }
}

/// The exe's `sinf` (0x141ed3170, non-FMA branch).
pub fn sinf(xf: f32) -> f32 {
    let u = xf.to_bits();
    if u & 0x7f80_0000 == 0x7f80_0000 {
        return special(u);
    }
    let x = xf as f64;
    let ax = x.to_bits() & !(1 << 63);
    if ax <= PIO4_BITS {
        if ax < SIN_TINY_BITS {
            return xf;
        }
        if ax < SIN_SMALL_BITS {
            return (x - x * x * x * SIXTH) as f32;
        }
        return sin_poly(x) as f32;
    }
    let neg = x.is_sign_negative();
    let xa = if neg { 0.0 - x } else { x };
    let (r, n) = if ax >= SIN_LARGE_BITS {
        let (r, n) = reduce_large(xa);
        (sin_cos_full(r, n), n)
    } else {
        let (r, n, rexp) = reduce_medium(xa);
        let odd = n & 1 != 0;
        let y = if rexp >= 0x3f2 {
            sin_cos_full(r, n)
        } else if rexp > 0x3de {
            let x2 = r * r;
            if odd { ONE - x2 * HALF } else { r - SIXTH * r * x2 }
        } else if odd {
            ONE
        } else {
            r
        };
        (y, n)
    };
    let y = if ((n >> 1) & 1 != 0) != neg { 0.0 - r } else { r };
    y as f32
}

/// The exe's `cosf` (0x141ed2c70, non-FMA branch).
pub fn cosf(xf: f32) -> f32 {
    let u = xf.to_bits();
    if u & 0x7f80_0000 == 0x7f80_0000 {
        return special(u);
    }
    let x = xf as f64;
    let ax = x.to_bits() & !(1 << 63);
    let (y, n) = if ax <= PIO4_BITS {
        (cos_poly(x), 0)
    } else if ax >= COS_LARGE_BITS {
        let (r, n) = reduce_large(f64::from_bits(ax));
        (cos_sin_full(r, n), n)
    } else {
        let (r, n, rexp) = reduce_medium(f64::from_bits(ax));
        let odd = n & 1 != 0;
        let y = if rexp >= 0x3f2 {
            cos_sin_full(r, n)
        } else if rexp > 0x3de {
            let x2 = r * r;
            if odd { r - SIXTH * r * x2 } else { ONE - x2 * HALF }
        } else if odd {
            r
        } else {
            ONE
        };
        (y, n)
    };
    let y = if n.wrapping_add(1) & 2 != 0 { 0.0 - y } else { y };
    y as f32
}

/// sinf's full polynomial for quadrant `n`: sine on even, cosine on odd (0x141ed341e).
#[inline]
fn sin_cos_full(r: f64, n: u32) -> f64 {
    if n & 1 == 0 { sin_poly(r) } else { cos_poly(r) }
}

/// cosf's full polynomial for quadrant `n`: cosine on even, sine on odd (0x141ed2e60).
#[inline]
fn cos_sin_full(r: f64, n: u32) -> f64 {
    if n & 1 == 0 { cos_poly(r) } else { sin_poly(r) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bulk evaluation for tools/crt_sincos_sweep.py, outside the normal test run: reads one f32 bit pattern
    /// (hex) per line from $CRT_SWEEP_IN and writes "sinf cosf" bit patterns per line to $CRT_SWEEP_OUT.
    #[test]
    #[ignore]
    fn crt_sweep_eval() {
        let (Ok(inp), Ok(out)) = (std::env::var("CRT_SWEEP_IN"), std::env::var("CRT_SWEEP_OUT")) else {
            panic!("set CRT_SWEEP_IN and CRT_SWEEP_OUT");
        };
        let text = std::fs::read_to_string(inp).expect("reading CRT_SWEEP_IN");
        let mut s = String::with_capacity(text.len() * 2);
        for line in text.lines().filter(|l| !l.is_empty()) {
            let x = f32::from_bits(u32::from_str_radix(line.trim(), 16).expect("hex f32 bits"));
            s.push_str(&format!("{:08x} {:08x}\n", sinf(x).to_bits(), cosf(x).to_bits()));
        }
        std::fs::write(out, s).expect("writing CRT_SWEEP_OUT");
    }
}
