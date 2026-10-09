//! idParticleParm: `{ val0, val1, variance, tableIdx, table2Idx, calcType }` (0x14 bytes) and its
//! evaluation (0x1415e7480, jump table at 0x1415e76f8) and maximum (0x1415e8350).
//!
//! Random draws advance the particle's idRandom seed in place, so the order of evaluations matters and
//! follows the engine's call order.

use crate::decl::{f32_or, i32_or, str_of};
use crate::table::Table;
use crate::{IdRandom, RANDOM_SCALE};
use idres::decl::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Calc {
    #[default]
    None,
    /// Curve: `table(t) * (1 + variance noise) * val0 + val1`.
    CurveGeneric,
    /// Random in [val0, val1] plus variance noise (two draws).
    Generic,
    /// table(t) * table2(t)
    CurveModCurve,
    /// table(t) + table2(t)
    CurveAddCurve,
    /// Linear val0 -> val1 over t plus variance noise.
    ParametricEval,
    /// Integral of the linear ramp from 0 to t plus variance noise.
    ParametricIntegrate,
    /// Random rate in [val0, val1], integrated.
    ParametricIntegrateMinMax,
}

pub const CALC_NAMES: [&str; 8] = [
    "PARTICLE_CALC_NONE",
    "PARTICLE_CALC_CURVE_GENERIC",
    "PARTICLE_CALC_GENERIC",
    "PARTICLE_CALC_CURVE_MOD_CURVE",
    "PARTICLE_CALC_CURVE_ADD_CURVE",
    "PARTICLE_CALC_PARAMETRIC_EVAL",
    "PARTICLE_CALC_PARAMETRIC_INTEGRATE",
    "PARTICLE_CALC_PARAMETRIC_INTEGRATE_MINMAX",
];

impl Calc {
    pub fn from_index(i: usize) -> Calc {
        [
            Calc::None,
            Calc::CurveGeneric,
            Calc::Generic,
            Calc::CurveModCurve,
            Calc::CurveAddCurve,
            Calc::ParametricEval,
            Calc::ParametricIntegrate,
            Calc::ParametricIntegrateMinMax,
        ]
        .get(i)
        .copied()
        .unwrap_or(Calc::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parm {
    pub val0: f32,
    pub val1: f32,
    pub variance: f32,
    pub table: i16,
    pub table2: i16,
    pub calc: Calc,
}

impl Parm {
    /// idParticleParm::Set (0x141a5ca60): {v0, v0, variance, -1, -1, GENERIC}.
    pub const fn generic(v: f32) -> Parm {
        Parm { val0: v, val1: v, variance: 0.0, table: -1, table2: -1, calc: Calc::Generic }
    }
    /// 0x141a5cab0: {v0, v1, variance, -1, -1, PARAMETRIC_EVAL}.
    pub const fn eval(v0: f32, v1: f32) -> Parm {
        Parm { val0: v0, val1: v1, variance: 0.0, table: -1, table2: -1, calc: Calc::ParametricEval }
    }
    /// 0x141a5cae0: {v0, v1, variance, -1, -1, PARAMETRIC_INTEGRATE}.
    pub const fn integrate(v0: f32, v1: f32) -> Parm {
        Parm { val0: v0, val1: v1, variance: 0.0, table: -1, table2: -1, calc: Calc::ParametricIntegrate }
    }
    /// 0x141a5cb10: {v0, v1, 0, -1, -1, GENERIC} (range).
    pub const fn range(v0: f32, v1: f32) -> Parm {
        Parm { val0: v0, val1: v1, variance: 0.0, table: -1, table2: -1, calc: Calc::Generic }
    }

    /// Reads a parm block over its default.
    pub fn read(v: Option<&Value>, d: Parm) -> Parm {
        let b = v.and_then(Value::as_block);
        let calc = str_of(b, "calcType").and_then(|s| CALC_NAMES.iter().position(|n| n.eq_ignore_ascii_case(s))).map(Calc::from_index).unwrap_or(d.calc);
        Parm {
            val0: f32_or(b, "val0", d.val0),
            val1: f32_or(b, "val1", d.val1),
            variance: f32_or(b, "variance", d.variance),
            table: i32_or(b, "tableIdx", d.table as i32) as i16,
            table2: i32_or(b, "table2Idx", d.table2 as i32) as i16,
            calc,
        }
    }

    fn table<'a>(tables: &'a [Table], i: i16) -> Option<&'a Table> {
        if i < 0 { None } else { tables.get(i as usize) }
    }

    /// `v + crandom * v * variance`, one draw (the variance noise every non-generic calc applies).
    #[inline]
    fn noise(v: f32, variance: f32, seed: &mut u32) -> f32 {
        *seed = IdRandom::step(*seed);
        let r = ((*seed >> 10) & 0x7fff) as f32 * RANDOM_SCALE;
        ((r + r) - 1.0) * (v * variance) + v
    }

    /// 0x1415e7480 (parm, tables, t, seed).
    pub fn eval_at(&self, tables: &[Table], t: f32, seed: &mut u32) -> f32 {
        match self.calc {
            Calc::None => 0.0,
            Calc::CurveGeneric => {
                // 0x1415e7ed0: missing table -> 0 without touching the seed.
                let c = match Self::table(tables, self.table) {
                    Some(tb) => Self::noise(tb.lookup(t), self.variance, seed),
                    None => 0.0,
                };
                c * self.val0 + self.val1
            }
            Calc::Generic => {
                *seed = IdRandom::step(*seed);
                let r1 = ((*seed >> 10) & 0x7fff) as f32 * RANDOM_SCALE;
                let base = r1 * (self.val1 - self.val0) + self.val0;
                Self::noise(base, self.variance, seed)
            }
            Calc::CurveModCurve => {
                let a = Self::table(tables, self.table).map(|t2| t2.lookup(t)).unwrap_or(0.0);
                let b = Self::table(tables, self.table2).map(|t2| t2.lookup(t)).unwrap_or(0.0);
                b * a
            }
            Calc::CurveAddCurve => {
                let a = Self::table(tables, self.table).map(|t2| t2.lookup(t)).unwrap_or(0.0);
                let b = Self::table(tables, self.table2).map(|t2| t2.lookup(t)).unwrap_or(0.0);
                b + a
            }
            Calc::ParametricEval => {
                let v = (self.val1 - self.val0) * t + self.val0;
                Self::noise(v, self.variance, seed)
            }
            Calc::ParametricIntegrate => {
                let v = ((self.val1 - self.val0) * t * 0.5 + self.val0) * t;
                Self::noise(v, self.variance, seed)
            }
            Calc::ParametricIntegrateMinMax => {
                // 0x1415e85b0: rate = random in [val0, val1]; integrate a ramp from it to itself.
                *seed = IdRandom::step(*seed);
                let r = ((*seed >> 10) & 0x7fff) as f32 * RANDOM_SCALE;
                let rate = r * (self.val1 - self.val0) + self.val0;
                let v = ((rate - rate) * t * 0.5 + rate) * t;
                Self::noise(v, self.variance, seed)
            }
        }
    }

    /// Largest value the parm can take (0x1415e8350, jump table 0x1415e8504).
    pub fn max(&self, tables: &[Table]) -> f32 {
        let tmax = |i: i16| Self::table(tables, i).map(|t| t.max).unwrap_or(0.0);
        match self.calc {
            Calc::None => 0.0,
            Calc::CurveGeneric => (tmax(self.table) * self.variance + tmax(self.table)) * self.val0 + self.val1,
            Calc::Generic => {
                let m = self.val1.max(self.val0);
                m * self.variance + m
            }
            Calc::CurveModCurve => tmax(self.table2) * tmax(self.table),
            Calc::CurveAddCurve => tmax(self.table2) + tmax(self.table),
            Calc::ParametricEval => self.val1 * self.variance + self.val1,
            Calc::ParametricIntegrate => (self.val1 * self.variance + self.val1 + self.val0 + self.val0 * self.variance) * 0.5,
            Calc::ParametricIntegrateMinMax => {
                let m = self.val1.max(self.val0);
                (m * self.variance + m + m + m * self.variance) * 0.5
            }
        }
    }
}

/// idParticleParmSimple (GPU stages): `{ val0, val1, variance, calcType (CONSTANT | MINMAX) }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimpleParm {
    pub val0: f32,
    pub val1: f32,
    pub variance: f32,
    pub minmax: bool,
}

impl SimpleParm {
    /// 0x141a5ca90: {v0, v0, variance, CONSTANT}.
    pub const fn constant(v: f32) -> SimpleParm {
        SimpleParm { val0: v, val1: v, variance: 0.0, minmax: false }
    }
    pub fn read(v: Option<&Value>, d: SimpleParm) -> SimpleParm {
        let b = v.and_then(Value::as_block);
        SimpleParm {
            val0: f32_or(b, "val0", d.val0),
            val1: f32_or(b, "val1", d.val1),
            variance: f32_or(b, "variance", d.variance),
            minmax: str_of(b, "calcType").map(|s| s.eq_ignore_ascii_case("TYPE_MINMAX")).unwrap_or(d.minmax),
        }
    }
    /// The stage derive's maximum (0x1417cdef0): CONSTANT -> v0 + v0*variance, MINMAX -> max(v0, v1).
    pub fn max(&self) -> f32 {
        if self.minmax { self.val1.max(self.val0) } else { self.val0 * self.variance + self.val0 }
    }
    /// The GPU emitter's (min, max) pair (FUN_1415e6bc0): CONSTANT -> (v0 - v0*variance, v0*variance + v0),
    /// MINMAX -> (v0, v1).
    pub fn range(&self) -> (f32, f32) {
        if self.minmax { (self.val0, self.val1) } else { (self.val0 - self.val0 * self.variance, self.val0 * self.variance + self.val0) }
    }
}
