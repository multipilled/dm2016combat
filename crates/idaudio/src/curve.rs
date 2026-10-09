//! Wwise graph curves (RTPCs, attenuations): `(x, y, interpolation)` points plus a scaling mode.
//!
//! Interpolation ids (AkCurveInterpolation; the exe reverses a curve as `8 - id`, 0x141d575b0):
//! 0 Log3, 1 Sine, 2 Log1, 3 InvSCurve, 4 Linear, 5 SCurve, 6 Exp1, 7 SineRecip, 8 Exp3, 9 Constant.
//! The shape formulas below are the standard Wwise fade shapes; they are not yet checked against the
//! exe's evaluator.
//!
//! Scaling 0 = values used as-is (attenuation volume curves in dB, -200 = silent; LPF/HPF/spread 0..100;
//! pitch in cents). Scaling 2 = "dB" curves stored in linear amplitude minus one: -1 = silent, 0 = 0 dB,
//! +1 = +6.02 dB; interpolated in that domain and returned in dB. INFERRED from the data: every
//! scaling-2 attenuation volume curve spans [-1, 0] while scaling-0 ones span [-200, 0] dB, and the
//! `sound_volume` RTPC curve runs -1 → 0 → +1 over -60 → 0 → +60 dB.

use std::f32::consts::{FRAC_PI_2, PI};

pub const SCALING_NONE: u8 = 0;
pub const SCALING_DB: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    pub scaling: u8,
    /// (x, y, interpolation of the segment starting at this point)
    pub points: Vec<(f32, f32, u32)>,
}

/// Fraction 0..1 of the way from the segment's start value to its end value.
/// INTERIM: the standard Wwise fade shapes, not yet checked against the exe's curve evaluator (the volume cvars'
/// gain, ((dB + 60) / 60)^3 on the Exp3 segment, rests on shape 8).
pub fn shape(interp: u32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match interp {
        0 => 1.0 - (1.0 - t).powi(3),
        1 => (t * FRAC_PI_2).sin(),
        2 => 1.0 - (1.0 - t).powi(2),
        3 => {
            if t < 0.5 {
                0.5 * (PI * t).sin()
            } else {
                1.0 - 0.5 * (PI * t).sin()
            }
        }
        5 => 0.5 - 0.5 * (PI * t).cos(),
        6 => t * t,
        7 => 1.0 - (t * FRAC_PI_2).cos(),
        8 => t * t * t,
        9 => 0.0,
        _ => t,
    }
}

impl Curve {
    /// Raw curve value at `x` (before scaling), clamped to the end points.
    pub fn raw(&self, x: f32) -> f32 {
        let p = &self.points;
        match p.len() {
            0 => 0.0,
            1 => p[0].1,
            n => {
                if x <= p[0].0 {
                    return p[0].1;
                }
                if x >= p[n - 1].0 {
                    return p[n - 1].1;
                }
                let i = p.windows(2).position(|w| x < w[1].0).unwrap_or(n - 2);
                let (a, b) = (p[i], p[i + 1]);
                let span = b.0 - a.0;
                let t = if span > 0.0 { (x - a.0) / span } else { 1.0 };
                a.1 + (b.1 - a.1) * shape(a.2, t)
            }
        }
    }

    /// Curve value in output units (dB for scaling-2 curves).
    pub fn eval(&self, x: f32) -> f32 {
        let v = self.raw(x);
        match self.scaling {
            SCALING_DB => lin_minus_one_to_db(v),
            _ => v,
        }
    }
}

/// Scaling-2 value (linear amplitude - 1) to dB, floored at -200 dB (Wwise's silence).
pub fn lin_minus_one_to_db(v: f32) -> f32 {
    let g = v + 1.0;
    if g <= 1e-10 { -200.0 } else { (20.0 * g.log10()).max(-200.0) }
}

pub fn db_to_gain(db: f32) -> f32 {
    if db <= -200.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_hit_their_end_points() {
        for i in 0..9 {
            assert!(shape(i, 0.0).abs() < 1e-6, "{i}");
            assert!((shape(i, 1.0) - 1.0).abs() < 1e-6, "{i}");
            // The exe reverses curves as 8 - id: f_rev(t) = 1 - f(1 - t) for the asymmetric pairs.
            if i != 3 && i != 5 {
                assert!((shape(8 - i, 0.3) - (1.0 - shape(i, 0.7))).abs() < 1e-5, "{i}");
            }
        }
    }

    #[test]
    fn attenuation_amb_quiet_300_1200() {
        // Volume curve of the install's amb_quiet_300_1200 attenuation (scaling 2).
        let c = Curve { scaling: SCALING_DB, points: vec![(0.0, 0.0, 9), (300.0, 0.0, 5), (1200.0, -1.0, 4)] };
        assert_eq!(c.eval(100.0), 0.0);
        assert_eq!(c.eval(300.0), 0.0);
        assert!((c.eval(750.0) - (-6.0206)).abs() < 1e-3);
        assert_eq!(c.eval(5000.0), -200.0);
    }
}
