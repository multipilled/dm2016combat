//! DOOM (2016) weapon/impact FX: `fx` decls (idDeclFX, the FX manager's actions and their timing) and
//! `particle` decls (idDeclParticle stages) with the engine's CPU particle path reproduced bit-for-bit
//! where decoded. Every rule here comes from DOOMx64.exe (Steam build 13954591); addresses and
//! derivations are in gamedata/re/FX.md.
//!
//! Coordinates are idTech's (x forward, y left, z up, units = inches). An [`Axis`] holds the three
//! axis rows of an idMat3 (forward, left, up) expressed in the parent space.

pub mod decl;
pub mod dmodel;
pub mod env;
pub mod fx;
pub mod impact;
pub mod material;
pub mod names;
pub mod parm;
pub mod particle;
pub mod pieces;
pub mod ribbon;
pub mod sim;
pub mod table;
pub mod tags;

pub use glam::Vec3;

/// idRandom: `seed = 1664525 * seed + 1013904223`; RandomInt = `(seed >> 10) & 0x7fff`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdRandom(pub u32);

/// 1 / 32767 as the engine stores it (0x141fff340 = 0x38000100).
pub const RANDOM_SCALE: f32 = f32::from_bits(0x3800_0100);

impl IdRandom {
    #[inline]
    pub fn step(seed: u32) -> u32 {
        seed.wrapping_mul(0x0019_660d).wrapping_add(0x3c6e_f35f)
    }
    #[inline]
    pub fn next_int(&mut self) -> u32 {
        self.0 = Self::step(self.0);
        (self.0 >> 10) & 0x7fff
    }
    /// [0, 1]
    #[inline]
    pub fn random_float(&mut self) -> f32 {
        self.next_int() as f32 * RANDOM_SCALE
    }
    /// [-1, 1]
    #[inline]
    pub fn crandom_float(&mut self) -> f32 {
        let r = self.random_float();
        (r + r) - 1.0
    }
}

pub const TWO_PI: f32 = 6.283_185_5;
pub const PI: f32 = 3.141_592_7;
const HALF_PI: f32 = 1.570_796_4;
const THREE_HALF_PI: f32 = 4.712_389;
const INV_TWO_PI: f32 = 0.159_154_94;
pub const DEG2RAD: f32 = 0.017_453_292;

/// idMath::SinCos as inlined throughout the particle code: wrap to [0, 2pi) with floor, fold into
/// [-pi/2, pi/2], then the engine's degree-11 / degree-10 polynomials.
#[inline]
pub fn sincos(a: f32) -> (f32, f32) {
    let mut a = a;
    if a < 0.0 || TWO_PI <= a {
        a -= (a * INV_TWO_PI).floor() * TWO_PI;
    }
    let sign;
    if PI <= a {
        if a <= THREE_HALF_PI {
            sign = -1.0;
            a = PI - a;
        } else {
            a -= TWO_PI;
            sign = 1.0;
        }
    } else if HALF_PI < a {
        sign = -1.0;
        a = PI - a;
    } else {
        sign = 1.0;
    }
    let a2 = a * a;
    let s = (((((2.7526e-06 - a2 * 2.39e-08) * a2 - 0.000_198_409) * a2 + 0.008_333_332) * a2 - 0.166_666_67) * a2 + 1.0) * a;
    let c = (((((2.476_09e-05 - a2 * 2.605e-07) * a2 - 0.001_388_839_7) * a2 + 0.041_666_64) * a2 - 0.5) * a2 + 1.0) * sign;
    (s, c)
}

/// The engine's guarded normalise: length squared clamped from below before the reciprocal square root.
#[inline]
pub fn normalize(v: Vec3) -> Vec3 {
    let l2 = v.length_squared().max(1e-30);
    v * (1.0 / l2.sqrt())
}

/// An idMat3 as its three rows (the local axes expressed in the parent space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub x: Vec3,
    pub y: Vec3,
    pub z: Vec3,
}

impl Default for Axis {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Axis {
    pub const IDENTITY: Axis = Axis { x: Vec3::X, y: Vec3::Y, z: Vec3::Z };

    /// `v * mat` in idTech's row-vector convention: local -> parent.
    #[inline]
    pub fn to_parent(&self, v: Vec3) -> Vec3 {
        self.x * v.x + self.y * v.y + self.z * v.z
    }
    /// `mat * v`: parent -> local.
    #[inline]
    pub fn to_local(&self, v: Vec3) -> Vec3 {
        Vec3::new(self.x.dot(v), self.y.dot(v), self.z.dot(v))
    }
    /// idMat3 product `self * other` (rows of self expressed through other's rows).
    pub fn mul(&self, o: &Axis) -> Axis {
        Axis { x: o.to_parent(self.x), y: o.to_parent(self.y), z: o.to_parent(self.z) }
    }
    /// idAngles::ToMat3 (pitch, yaw, roll in degrees).
    pub fn from_angles(pitch: f32, yaw: f32, roll: f32) -> Axis {
        let (sp, cp) = sincos(pitch * DEG2RAD);
        let (sy, cy) = sincos(yaw * DEG2RAD);
        let (sr, cr) = sincos(roll * DEG2RAD);
        Axis {
            x: Vec3::new(cp * cy, cp * sy, -sp),
            y: Vec3::new(sr * sp * cy + cr * -sy, sr * sp * sy + cr * cy, sr * cp),
            z: Vec3::new(cr * sp * cy + -sr * -sy, cr * sp * sy + -sr * cy, cr * cp),
        }
    }
}
