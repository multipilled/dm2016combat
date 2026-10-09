//! First-person hands layers that sit on top of the hands anim-web pose, from the user's own DOOMx64.exe
//! (Steam build 13954591). Notes with addresses: gamedata/re/HANDSLAYERS.md.
//!
//! Per frame, idHands::Update (0x140d6b2c0) runs, in this order:
//! 1. [`WeaponLag::update`]: the weapon-lag pendulum (0x140d8c7d0 -> 0x140d8d2c0), integrated in 1 ms sub-steps by
//!    [`euler_step`] (0x140d8b100) or [`rk4_step`] (0x140d8b720, the default), giving a position and a 3x3 basis.
//! 2. [`WeaponBob::update`]: the procedural weapon bob (0x140d8c240): a stride phase driven by the player's speed,
//!    sin/cos offsets and angles, and a footstep event on every half cycle. When the weapon's `weaponBob.enable` is
//!    set (every campaign weapon but the fists) the animated bob-cycle web is switched off (0x140d83e80).
//! 3. [`compose_joint_mods`]: both are turned into model-space joint mods (0x140d8c7e0, "joint mods for weapon
//!    lag") on lefthandattach, righthandattach, rig_arm_left_root and rig_arm_right_root.
//!
//! The renderer then places the hands model with [`hands_placement`] and projects it with [`hands_projection_fov`]
//! using [`hands_fov_scale`].
//!
//! Float operations follow the exe's order so results can be compared bit for bit with the engine code run under
//! Unicorn (tools/handlayers_emu.py). Reciprocal square roots are `rsqrtss` + two Newton steps in the engine;
//! here (and under Unicorn) the seed is the exact 1/sqrt, so real hardware differs in the last bits.

pub mod additive;
pub mod bobcycle;
pub mod reactions;

use anyhow::{Context, Result};
use glam::{Mat3, Quat, Vec3, Vec4};
use idres::decl::Block;
use idres::decldb::DeclDb;

/// Game units to metres (0x1422ae60c).
pub const UNITS_TO_METERS: f32 = 0.01905;
/// Metres to game units (0x140d8c7e0).
pub const METERS_TO_UNITS: f32 = 52.49344;
const DEG2RAD: f32 = 0.017453292;
const TWO_PI: f32 = 6.2831855;
const INV_TWO_PI: f32 = 0.15915494;
/// Smallest normal float: the engine clamps before every reciprocal square root.
const TINY: f32 = 1.175_494_4e-38;
/// Weapon-lag sub-step length in seconds (0x3a83126f).
pub const LAG_STEP: f32 = 0.001;

// ---- idMath helpers (exact operation order) ---------------------------------------------------------------------

/// idMath::InvSqrt: rsqrtss on max(x, FLT_MIN) and two Newton-Raphson steps.
#[inline]
pub(crate) fn inv_sqrt(x: f32) -> f32 {
    let m = if x > TINY { x } else { TINY };
    let y0 = 1.0 / m.sqrt();
    let y1 = (m * y0 * y0 - 3.0) * y0 * -0.5;
    (m * y1 * y1 - 3.0) * y1 * -0.5
}

/// Row-major idMat3 (`m[row][col]`); vectors multiply from the left (`p' = p * m`).
pub type IdMat3 = [[f32; 3]; 3];

pub const IDENTITY: IdMat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// idAngles::ToMat3 (0x1402d90c0): rows are forward, left, up. Angles are [pitch, yaw, roll] in degrees.
pub fn angles_to_mat3(a: [f32; 3]) -> IdMat3 {
    let (sy, cy) = ((a[1] * DEG2RAD).sin(), (a[1] * DEG2RAD).cos());
    let (sp, cp) = ((a[0] * DEG2RAD).sin(), (a[0] * DEG2RAD).cos());
    let (sr, cr) = ((a[2] * DEG2RAD).sin(), (a[2] * DEG2RAD).cos());
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

/// idAngles::ToForward (0x1402d9030).
pub fn angles_to_forward(a: [f32; 3]) -> [f32; 3] {
    let cp = (a[0] * DEG2RAD).cos();
    let cy = (a[1] * DEG2RAD).cos();
    let sy = (a[1] * DEG2RAD).sin();
    let sp = (a[0] * DEG2RAD).sin();
    [cy * cp, sy * cp, -sp]
}

/// idRotation::ToMat3 (0x1402ea9d0) for a rotation of `deg` degrees about the unit vector `v`.
pub fn rotation_to_mat3(v: [f32; 3], deg: f32) -> IdMat3 {
    let a = deg * 0.008726646;
    let (s, c) = (a.sin(), a.cos());
    let (x, y, z) = (s * v[0], s * v[1], s * v[2]);
    let (y2, z2) = (y + y, z + z);
    let wx = (x + x) * c;
    let xx = (x + x) * x;
    [
        [1.0 - (z2 * z + y2 * y), y2 * x - z2 * c, y2 * c + z2 * x],
        [z2 * c + y2 * x, 1.0 - (z2 * z + xx), z2 * y - wx],
        [z2 * x - y2 * c, wx + z2 * y, 1.0 - (y2 * y + xx)],
    ]
}

/// `m = m * R` where R rotates row vectors by `rad` radians about `axis` (0x1402e8600; the engine builds an
/// idRotation of `rad * -57.295776` degrees).
pub fn rotate_mat3(m: &mut IdMat3, axis: [f32; 3], rad: f32) {
    let r = rotation_to_mat3(axis, rad * -57.295776);
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (m[i][1] * r[1][j] + m[i][0] * r[0][j]) + m[i][2] * r[2][j];
        }
    }
    *m = out;
}

/// Converts an idMat3 (row vectors, `p' = p * m`) to the glam column-vector matrix with the same effect.
pub fn to_glam(m: &IdMat3) -> Mat3 {
    Mat3::from_cols(Vec3::from(m[0]), Vec3::from(m[1]), Vec3::from(m[2]))
}

// ---- decl data ---------------------------------------------------------------------------------------------------

/// idDeclWeapon::weaponLag_t (+0x13f0); defaults from the idDeclWeapon ctor 0x1406f0780.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponLagParams {
    pub enable: bool,
    /// Pendulum length, metres.
    pub pendulum_length: f32,
    /// Stokes friction constant.
    pub friction: f32,
    pub max_angle_degrees: f32,
    /// Clamp on the driving acceleration, m/s^2.
    pub max_acceleration: f32,
    /// m/s^2.
    pub gravity: f32,
    pub weapon_dip_forward: bool,
}

impl Default for WeaponLagParams {
    fn default() -> Self {
        Self {
            enable: false,
            pendulum_length: 0.5,
            friction: 5.0,
            max_angle_degrees: 15.0,
            max_acceleration: 1.0,
            gravity: 9.81,
            weapon_dip_forward: false,
        }
    }
}

/// idDeclWeapon::weaponBob_t (+0x140c); defaults from the idDeclWeapon ctor 0x1406f0780. Directions are
/// [forward_back, left_right, up_down] (weaponDir_t); angles are [pitch, yaw, roll] (idAngles).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponBobParams {
    pub enable: bool,
    /// Stride, metres of travel per radian of bob phase... per 2*pi it is one footstep pair.
    pub stride: f32,
    pub stride_crouched: f32,
    /// Metres.
    pub translation_amplitudes: [f32; 3],
    pub translation_angular_velocities: [f32; 3],
    /// Degrees.
    pub translation_phase_angles: [f32; 3],
    /// Degrees.
    pub rotational_amplitudes: [f32; 3],
    pub rotational_angular_velocities: [f32; 3],
    /// Degrees.
    pub rotational_phase_angles: [f32; 3],
}

impl Default for WeaponBobParams {
    fn default() -> Self {
        Self {
            enable: false,
            stride: 0.55,
            stride_crouched: 0.55,
            translation_amplitudes: [0.004, 0.002, 0.006],
            translation_angular_velocities: [0.5, 0.5, 1.0],
            translation_phase_angles: [0.0; 3],
            rotational_amplitudes: [1.1, 0.5, 1.2],
            rotational_angular_velocities: [0.5, 0.25, 0.25],
            rotational_phase_angles: [0.0; 3],
        }
    }
}

/// Everything this module reads from a weapon decl.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandsLayerDecl {
    pub lag: WeaponLagParams,
    pub bob: WeaponBobParams,
    /// idDeclInventory::handsFovScale (+0xf8); ctor 0x1406e8828 default 0.7.
    pub hands_fov_scale: f32,
    /// ironSightZoom.zoomedFOV / zoomedHandsFOV (+0xde8 / +0xdec), degrees, 0 = unset.
    pub zoomed_fov: f32,
    pub zoomed_hands_fov: f32,
    /// handsOffset (+0xc28, idVec3, forward/left/up game units) and handsOffsetAngles (+0xc34, pitch/yaw/roll).
    pub hands_offset: [f32; 3],
    pub hands_offset_angles: [f32; 3],
}

impl Default for HandsLayerDecl {
    fn default() -> Self {
        Self {
            lag: WeaponLagParams::default(),
            bob: WeaponBobParams::default(),
            hands_fov_scale: 0.7,
            zoomed_fov: 0.0,
            zoomed_hands_fov: 0.0,
            hands_offset: [0.0; 3],
            hands_offset_angles: [0.0; 3],
        }
    }
}

fn flag(b: &Block, key: &str, def: bool) -> bool {
    b.path(key).and_then(|v| v.as_bool()).unwrap_or(def)
}

/// Reads `keys` of a decl sub-block into `out`, keeping defaults for absent keys. Unknown keys (the legacy x/y/z
/// some parents still carry) are not fields of weaponDir_t / idAngles and are ignored, as by the engine.
fn triple(b: Option<&Block>, keys: [&str; 3], out: &mut [f32; 3]) {
    let Some(b) = b else { return };
    for (k, o) in keys.iter().zip(out.iter_mut()) {
        if let Some(v) = b.f32(k) {
            *o = v;
        }
    }
}

const DIR: [&str; 3] = ["forward_back", "left_right", "up_down"];
const ANG: [&str; 3] = ["pitch", "yaw", "roll"];

impl HandsLayerDecl {
    /// `name` as listed by the arsenal, e.g. "weapon/zion/player/sp/heavy_rifle_heavy_ar".
    pub fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("weapon", name).with_context(|| format!("weapon decl {name}"))?;
        let e = b.block("edit").cloned().unwrap_or_default();
        Ok(Self::from_edit(&e))
    }

    pub fn from_edit(e: &Block) -> Self {
        let mut d = Self::default();
        if let Some(l) = e.block("weaponLag") {
            let dl = d.lag;
            d.lag = WeaponLagParams {
                enable: flag(l, "enable", dl.enable),
                pendulum_length: l.f32("pendulumLength").unwrap_or(dl.pendulum_length),
                friction: l.f32("friction").unwrap_or(dl.friction),
                max_angle_degrees: l.f32("maxAngleDegrees").unwrap_or(dl.max_angle_degrees),
                max_acceleration: l.f32("maxAcceleration").unwrap_or(dl.max_acceleration),
                gravity: l.f32("gravity").unwrap_or(dl.gravity),
                weapon_dip_forward: flag(l, "weaponDipForward", dl.weapon_dip_forward),
            };
        }
        if let Some(w) = e.block("weaponBob") {
            let p = &mut d.bob;
            p.enable = flag(w, "enable", p.enable);
            p.stride = w.f32("stride").unwrap_or(p.stride);
            p.stride_crouched = w.f32("strideCrouched").unwrap_or(p.stride_crouched);
            triple(w.block("translationAmplitudes"), DIR, &mut p.translation_amplitudes);
            triple(w.block("translationAngularVelocities"), DIR, &mut p.translation_angular_velocities);
            triple(w.block("translationPhaseAngles"), DIR, &mut p.translation_phase_angles);
            triple(w.block("rotationalAmplitudes"), ANG, &mut p.rotational_amplitudes);
            triple(w.block("rotationalAngularVelocities"), ANG, &mut p.rotational_angular_velocities);
            triple(w.block("rotationalPhaseAngles"), ANG, &mut p.rotational_phase_angles);
        }
        d.hands_fov_scale = e.f32("handsFovScale").unwrap_or(d.hands_fov_scale);
        d.zoomed_fov = e.f32("ironSightZoom.zoomedFOV").unwrap_or(0.0);
        d.zoomed_hands_fov = e.f32("ironSightZoom.zoomedHandsFOV").unwrap_or(0.0);
        triple(e.block("handsOffset"), ["x", "y", "z"], &mut d.hands_offset);
        triple(e.block("handsOffsetAngles"), ANG, &mut d.hands_offset_angles);
        d
    }
}

/// Cvars read by these layers (exe defaults; the shipped configs do not set them).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandsLayerCvars {
    /// hands_weaponLagEnable (0x140d8c7d0).
    pub weapon_lag_enable: bool,
    /// hands_weaponLagIntegrationMethod: 0 Euler, otherwise RK4.
    pub weapon_lag_integration: i32,
    /// handsBobCycle_Enable: gates the procedural bob too (0x140d8c240).
    pub hands_bob_cycle_enable: bool,
    /// hands_updatePos: 0 makes GetHandsFovScale return 1.
    pub hands_update_pos: bool,
    /// hands_FovScale: > 0 overrides the hands FOV scale.
    pub hands_fov_scale: f32,
    /// hands_fovVerticalScaleHack (render, 0x141af88a0).
    pub hands_fov_vertical_scale_hack: f32,
    /// hands_offsetX/Y/Z (game units, view axes) and hands_offsetPitch/Yaw/Roll (degrees).
    pub hands_offset: [f32; 3],
    pub hands_offset_angles: [f32; 3],
    /// g_fov and its default string ("90"), whose ratio scales the hands FOV.
    pub g_fov: f32,
    pub g_fov_default: f32,
}

impl Default for HandsLayerCvars {
    fn default() -> Self {
        Self {
            weapon_lag_enable: true,
            weapon_lag_integration: 1,
            hands_bob_cycle_enable: true,
            hands_update_pos: true,
            hands_fov_scale: 0.0,
            hands_fov_vertical_scale_hack: 1.101,
            hands_offset: [0.0; 3],
            hands_offset_angles: [0.0; 3],
            g_fov: 90.0,
            g_fov_default: 90.0,
        }
    }
}

impl HandsLayerCvars {
    /// From the install's cvar table (exe defaults + shipped configs); `g_fov_default` is the exe default string.
    pub fn from_cvars(cv: &crate::config::CvarValues, g_fov_default: f32) -> Self {
        let d = Self::default();
        let get = |n: &str, def: f32| cv.0.get(n).and_then(|v| v.trim_end_matches('f').parse().ok()).unwrap_or(def);
        Self {
            weapon_lag_enable: get("hands_weaponLagEnable", 1.0) != 0.0,
            weapon_lag_integration: get("hands_weaponLagIntegrationMethod", 1.0) as i32,
            hands_bob_cycle_enable: get("handsBobCycle_Enable", 1.0) != 0.0,
            hands_update_pos: get("hands_updatePos", 1.0) != 0.0,
            hands_fov_scale: get("hands_FovScale", d.hands_fov_scale),
            hands_fov_vertical_scale_hack: get("hands_fovVerticalScaleHack", d.hands_fov_vertical_scale_hack),
            hands_offset: [get("hands_offsetX", 0.0), get("hands_offsetY", 0.0), get("hands_offsetZ", 0.0)],
            hands_offset_angles: [
                get("hands_offsetPitch", 0.0),
                get("hands_offsetYaw", 0.0),
                get("hands_offsetRoll", 0.0),
            ],
            g_fov: get("g_fov", d.g_fov),
            g_fov_default,
        }
    }
}

// ---- weapon lag pendulum -----------------------------------------------------------------------------------------

/// One pendulum sub-step's constants.
#[derive(Debug, Clone, Copy)]
pub struct PendulumParams {
    /// Pivot (0, 0, pendulumLength): the bob hangs at the origin.
    pub pivot: [f32; 3],
    pub dt: f32,
    pub friction: f32,
    pub max_angle_rad: f32,
    pub length: f32,
}

/// Angle of the arm from straight down and, past `max`, the swing back onto the cone (shared tail of both
/// integrators). `n` is the unit arm direction (bob - pivot).
fn clamp_angle(p: &mut [f32; 3], v: &mut [f32; 3], n: [f32; 3], pp: &PendulumParams) {
    let c = (n[0] * 0.0 + n[1] * 0.0) - n[2] * 1.0;
    let angle = if -1.0 < c { if c < 1.0 { c.acos() } else { 0.0 } } else { std::f32::consts::PI };
    if pp.max_angle_rad < angle {
        // cross(n, (0, 0, -1)); x * -1 is an exact negation.
        let mut ax = [-n[1] - n[2] * 0.0, n[2] * 0.0 - -n[0], n[0] * 0.0 - n[1] * 0.0];
        let s = inv_sqrt((ax[1] * ax[1] + ax[0] * ax[0]) + ax[2] * ax[2]);
        ax = [ax[0] * s, ax[1] * s, ax[2] * s];
        let mut m = IDENTITY;
        rotate_mat3(&mut m, ax, angle - pp.max_angle_rad);
        let pv = pp.pivot;
        let d = [p[0] - pv[0], p[1] - pv[1], p[2] - pv[2]];
        let q = [
            ((m[1][0] * d[1] + m[0][0] * d[0]) + m[2][0] * d[2]) + pv[0],
            ((m[0][1] * d[0] + m[1][1] * d[1]) + m[2][1] * d[2]) + pv[1],
            ((m[1][2] * d[1] + m[0][2] * d[0]) + m[2][2] * d[2]) + pv[2],
        ];
        *p = q;
        // The engine projects the velocity on the normalised bob position itself, not on the arm.
        let s = inv_sqrt((q[1] * q[1] + q[0] * q[0]) + q[2] * q[2]);
        let u = [q[0] * s, q[1] * s, q[2] * s];
        let vn = (u[1] * v[1] + u[0] * v[0]) + u[2] * v[2];
        *v = [v[0] - u[0] * vn, v[1] - u[1] * vn, v[2] - u[2] * vn];
    }
}

/// hands_weaponLagIntegrationMethod 0 (0x140d8b100): symplectic Euler on the sphere.
pub fn euler_step(p: &mut [f32; 3], v: &mut [f32; 3], a: [f32; 3], pp: &PendulumParams) {
    let pv = pp.pivot;
    let dt = pp.dt;
    let d = [p[0] - pv[0], p[1] - pv[1], p[2] - pv[2]];
    let s = inv_sqrt((d[1] * d[1] + d[0] * d[0]) + d[2] * d[2]);
    let n = [d[0] * s, d[1] * s, d[2] * s];
    let an = (n[0] * a[0] + n[1] * a[1]) + n[2] * a[2];
    let w = [(a[0] - n[0] * an) * dt + v[0], (a[1] - n[1] * an) * dt + v[1], (a[2] - n[2] * an) * dt + v[2]];
    let vn = (w[1] * n[1] + w[0] * n[0]) + w[2] * n[2];
    *v = [w[0] - n[0] * vn, w[1] - n[1] * vn, w[2] - n[2] * vn];
    if 0.0 < pp.friction {
        *v = [v[0] - v[0] * pp.friction * dt, v[1] - v[1] * pp.friction * dt, v[2] - v[2] * pp.friction * dt];
    }
    *p = [dt * v[0] + p[0], dt * v[1] + p[1], dt * v[2] + p[2]];
    let d = [p[0] - pv[0], p[1] - pv[1], p[2] - pv[2]];
    let s = inv_sqrt((d[1] * d[1] + d[0] * d[0]) + d[2] * d[2]);
    let n = [d[0] * s, d[1] * s, d[2] * s];
    *p = [n[0] * pp.length + pv[0], n[1] * pp.length + pv[1], n[2] * pp.length + pv[2]];
    clamp_angle(p, v, n, pp);
}

/// 0x140d8c050: (velocity derivative, position derivative) at (p, v): both tangent to the sphere, with Stokes drag.
fn derivative(v: [f32; 3], p: [f32; 3], a: [f32; 3], pp: &PendulumParams) -> ([f32; 3], [f32; 3]) {
    let pv = pp.pivot;
    let d = [p[0] - pv[0], p[1] - pv[1], p[2] - pv[2]];
    let s = inv_sqrt((d[0] * d[0] + d[1] * d[1]) + d[2] * d[2]);
    let n = [d[0] * s, d[1] * s, d[2] * s];
    let vn = (v[0] * n[0] + v[1] * n[1]) + v[2] * n[2];
    let w = [v[0] - n[0] * vn, v[1] - n[1] * vn, v[2] - n[2] * vn];
    let an = (a[0] * n[0] + a[1] * n[1]) + a[2] * n[2];
    let dv = [
        (a[0] - n[0] * an) - w[0] * pp.friction,
        (a[1] - n[1] * an) - w[1] * pp.friction,
        (a[2] - n[2] * an) - w[2] * pp.friction,
    ];
    (dv, w)
}

/// hands_weaponLagIntegrationMethod 1, the default (0x140d8b720): classic RK4, then back onto the sphere.
pub fn rk4_step(p: &mut [f32; 3], v: &mut [f32; 3], a: [f32; 3], pp: &PendulumParams) {
    let (dt, h) = (pp.dt, pp.dt * 0.5);
    let (p0, v0) = (*p, *v);
    let step = |k: [f32; 3], base: [f32; 3], t: f32| [k[0] * t + base[0], k[1] * t + base[1], k[2] * t + base[2]];
    let (k1v, k1x) = derivative(v0, p0, a, pp);
    let (k2v, k2x) = derivative(step(k1v, v0, h), step(k1x, p0, h), a, pp);
    let (k3v, k3x) = derivative(step(k2v, v0, h), step(k2x, p0, h), a, pp);
    let (k4v, k4x) = derivative(step(k3v, v0, dt), step(k3x, p0, dt), a, pp);
    let sum = |k1: [f32; 3], k2: [f32; 3], k3: [f32; 3], k4: [f32; 3], i: usize| {
        (((k1[i] + (k2[i] + k2[i])) + (k3[i] + k3[i])) + k4[i]) * dt * 0.16666667
    };
    *v = [v0[0] + sum(k1v, k2v, k3v, k4v, 0), v0[1] + sum(k1v, k2v, k3v, k4v, 1), v0[2] + sum(k1v, k2v, k3v, k4v, 2)];
    let q =
        [sum(k1x, k2x, k3x, k4x, 0) + p0[0], sum(k1x, k2x, k3x, k4x, 1) + p0[1], sum(k1x, k2x, k3x, k4x, 2) + p0[2]];
    let pv = pp.pivot;
    let d = [q[0] - pv[0], q[1] - pv[1], q[2] - pv[2]];
    let s = inv_sqrt((d[1] * d[1] + d[0] * d[0]) + d[2] * d[2]);
    let n = [d[0] * s, d[1] * s, d[2] * s];
    *p = [n[0] * pp.length + pv[0], n[1] * pp.length + pv[1], n[2] * pp.length + pv[2]];
    let vn = (n[1] * v[1] + n[0] * v[0]) + n[2] * v[2];
    *v = [v[0] - n[0] * vn, v[1] - n[1] * vn, v[2] - n[2] * vn];
    clamp_angle(p, v, n, pp);
}

/// Per-frame inputs of the weapon lag (0x140d8d2c0).
#[derive(Debug, Clone, Copy)]
pub struct LagInput {
    /// The player's `playerPState_t.acceleration` (physics current +0x30, player +0x16934) in game units/s^2:
    /// (velocity - previous.velocity) / (framemsec * 0.001). The move code writes it in several places and SlideMove
    /// (0x1416b48f0) rewrites it last, so it is the frame's full velocity change: `PlayerPhysics::acceleration`.
    pub acceleration: Vec3,
    /// idUCmdTracker viewAngles / prevViewAngles ([pitch, yaw, roll] degrees; idPlayerController vslots 37/38).
    pub view_angles: [f32; 3],
    pub prev_view_angles: [f32; 3],
    /// Model-space origin of righthandattach (game units) from the hands' last built pose; zero if not found.
    pub right_hand_origin: Vec3,
    /// Game-timeline frame length in ms (the same msec the physics ran with).
    pub msec: i32,
}

/// Superseded by `PlayerPhysics::acceleration` (the engine's playerPState_t.acceleration): the velocity change over
/// the frame from two velocity samples.
pub fn velocity_delta_acceleration(prev_velocity: Vec3, velocity: Vec3, msec: i32) -> Vec3 {
    if msec == 0 { Vec3::ZERO } else { (velocity - prev_velocity) * (1.0 / (msec as f32 * 0.001)) }
}

/// idHands weaponLagPosition / weaponLagVelocity / weaponLagAxis (+0x5b20 / +0x5b2c / +0x5b38).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponLag {
    /// Bob position, metres, relative to its rest point (the pivot is (0, 0, pendulumLength)).
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    /// Basis built from the arm direction.
    pub axis: IdMat3,
}

impl Default for WeaponLag {
    fn default() -> Self {
        Self { pos: [0.0; 3], vel: [0.0; 3], axis: IDENTITY }
    }
}

impl WeaponLag {
    /// 0x140d8bfe0, called by idWeapon on every equip (0x140f1be68) together with the bob reset.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The driving acceleration in view space (x forward, y left, z up), after the maxAcceleration clamp and
    /// before the negation (0x140d8d401..0x140d8d72e).
    pub fn view_acceleration(&self, prm: &WeaponLagParams, inp: &LagInput) -> [f32; 3] {
        let a = [
            inp.acceleration.x * UNITS_TO_METERS,
            inp.acceleration.y * UNITS_TO_METERS,
            inp.acceleration.z * UNITS_TO_METERS,
        ];
        let m = angles_to_mat3(inp.view_angles);
        let dot = |r: [f32; 3], v: [f32; 3]| (r[0] * v[0] + r[1] * v[1]) + r[2] * v[2];
        let (lx, ly, lz) = (dot(m[0], a), dot(m[1], a), dot(m[2], a));
        let prev = angles_to_forward(inp.prev_view_angles);
        let cur = angles_to_forward(inp.view_angles);
        let (fp, lp) = (dot(m[0], prev), dot(m[1], prev));
        let (fc, lc) = (dot(m[0], cur), dot(m[1], cur));
        let dt = inp.msec as f32 * 0.001;
        let o = inp.right_hand_origin;
        let r = [
            (o.x + self.pos[0]) * UNITS_TO_METERS,
            (o.y + self.pos[1]) * UNITS_TO_METERS,
            (o.z + self.pos[2]) * UNITS_TO_METERS,
        ];
        let rsq = (r[1] * r[1] + r[0] * r[0]) + r[2] * r[2];
        // Turning moves the hands sideways at |r| * (sin of the yaw change) / dt.
        let turn = (1.0 / dt) * (lc * fp - fc * lp);
        let mut f = [lx + 0.0, inv_sqrt(rsq) * rsq * turn + ly, lz + 0.0];
        let lsq = (f[1] * f[1] + f[0] * f[0]) + f[2] * f[2];
        let max = prm.max_acceleration;
        if lsq > max * max {
            let s = inv_sqrt(lsq);
            f = [f[0] * s * max, f[1] * s * max, f[2] * s * max];
        }
        f
    }

    /// One frame of 0x140d8d2c0 (called through 0x140d8c7d0, which checks hands_weaponLagEnable; weaponLag.enable
    /// is checked here too). Nothing changes when either is off.
    pub fn update(&mut self, prm: &WeaponLagParams, cv: &HandsLayerCvars, inp: &LagInput) {
        if !cv.weapon_lag_enable || !prm.enable {
            return;
        }
        let f = self.view_acceleration(prm, inp);
        let force = [0.0 - f[0], 0.0 - f[1], -prm.gravity - f[2]];
        let pp = PendulumParams {
            pivot: [0.0, 0.0, prm.pendulum_length],
            dt: LAG_STEP,
            friction: prm.friction,
            max_angle_rad: DEG2RAD * prm.max_angle_degrees,
            length: prm.pendulum_length,
        };
        let dt = inp.msec as f32 * 0.001;
        let steps = (dt / LAG_STEP) as i32;
        for _ in 0..steps.max(0) {
            if cv.weapon_lag_integration == 0 {
                euler_step(&mut self.pos, &mut self.vel, force, &pp);
            } else {
                rk4_step(&mut self.pos, &mut self.vel, force, &pp);
            }
        }
        self.axis = lag_axis(self.pos, prm);
    }
}

/// The basis 0x140d8d881..: row 2 points from the bob up the arm; without weaponDipForward row 0 stays +x, so the
/// lag only rolls the weapon (forward/back swing moves it without tilting).
pub fn lag_axis(pos: [f32; 3], prm: &WeaponLagParams) -> IdMat3 {
    let d = [pos[0] - 0.0, pos[1] - 0.0, pos[2] - prm.pendulum_length];
    let s = inv_sqrt((d[1] * d[1] + d[0] * d[0]) + d[2] * d[2]);
    let n = [-(d[0] * s), -(d[1] * s), -(d[2] * s)];
    let s = inv_sqrt((n[1] * n[1] + n[0] * n[0]) + n[2] * n[2]);
    let n = [n[0] * s, n[1] * s, n[2] * s];
    let r = [n[1] * 0.0 - n[2] * 0.0, n[2] - n[0] * 0.0, n[0] * 0.0 - n[1]];
    let s = inv_sqrt((r[1] * r[1] + r[0] * r[0]) + r[2] * r[2]);
    let r = [r[0] * s, r[1] * s, r[2] * s];
    if !prm.weapon_dip_forward {
        let c = [r[2] * 0.0 - r[1] * 0.0, r[0] * 0.0 - r[2], r[1] - r[0] * 0.0];
        let s = inv_sqrt((c[1] * c[1] + c[0] * c[0]) + c[2] * c[2]);
        [[1.0, 0.0, 0.0], r, [c[0] * s, c[1] * s, c[2] * s]]
    } else {
        let b = [r[1] * n[2] - r[2] * n[1], r[2] * n[0] - r[0] * n[2], r[0] * n[1] - r[1] * n[0]];
        let s = inv_sqrt((b[1] * b[1] + b[0] * b[0]) + b[2] * b[2]);
        [[b[0] * s, b[1] * s, b[2] * s], r, n]
    }
}

// ---- weapon bob --------------------------------------------------------------------------------------------------

/// Per-frame inputs of the weapon bob (0x140d8c240).
#[derive(Debug, Clone, Copy)]
pub struct BobInput {
    /// `playerPState_t.nonPushedVelocity` (physics current +0x24, player +0x16928), game units/s.
    pub velocity: Vec3,
    /// `playerPState_t.groundPlane` (player +0x16990).
    pub on_ground: bool,
    /// The physics crouch test 0x1416b04c0 (ducked).
    pub crouched: bool,
    pub msec: i32,
}

/// A footstep the bob asks the player to play (idPlayer vslot 0xc08 = 0x140e30080 with (foot, 0, 1)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BobStep {
    /// `numSteps & 1`.
    pub odd: bool,
}

/// idHands weaponBobPosition / weaponBobAngle / weaponBobOrientAngles / weaponBobNumSteps (+0x5b5c..+0x5b78).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WeaponBob {
    /// Phase in radians; grows without wrapping.
    pub angle: f32,
    pub num_steps: i32,
    /// [forward, left, up] metres.
    pub position: [f32; 3],
    /// [roll, pitch, yaw] radians, applied about x, y, z in that order.
    pub orient: [f32; 3],
}

#[inline]
fn wrap_two_pi(a: f32) -> f32 {
    if a < 0.0 || a >= TWO_PI { a - (a * INV_TWO_PI).floor() * TWO_PI } else { a }
}

impl WeaponBob {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// One frame of 0x140d8c240. Runs only when handsBobCycle_Enable and weaponBob.enable are on; otherwise the
    /// last values stay in place (and are still composed into the joint mods). Returns a footstep when the phase
    /// crosses a multiple of 2*pi while not crouched.
    pub fn update(&mut self, prm: &WeaponBobParams, cv: &HandsLayerCvars, inp: &BobInput) -> Option<BobStep> {
        if !cv.hands_bob_cycle_enable || !prm.enable {
            return None;
        }
        let v = [inp.velocity.x * UNITS_TO_METERS, inp.velocity.y * UNITS_TO_METERS, inp.velocity.z * UNITS_TO_METERS];
        let dt = inp.msec as f32 * 0.001;
        let sq = (v[1] * v[1] + v[0] * v[0]) + v[2] * v[2];
        let speed = inv_sqrt(sq) * sq;
        if inp.on_ground {
            let stride = if inp.crouched { prm.stride_crouched } else { prm.stride };
            self.angle += speed * dt / stride;
        }
        let mut step = None;
        let n = (self.angle / TWO_PI) as i32;
        if n != self.num_steps && !inp.crouched {
            self.num_steps = n;
            step = Some(BobStep { odd: n & 1 != 0 });
        }
        let a = self.angle;
        let ph = |vel: f32, phase_deg: f32| wrap_two_pi(a * vel + phase_deg * DEG2RAD);
        let (ta, tv, tp) =
            (prm.translation_amplitudes, prm.translation_angular_velocities, prm.translation_phase_angles);
        self.position =
            [ph(tv[0], tp[0]).cos() * ta[0], ph(tv[1], tp[1]).sin() * ta[1], ph(tv[2], tp[2]).cos() * ta[2]];
        let (ra, rv, rp) = (prm.rotational_amplitudes, prm.rotational_angular_velocities, prm.rotational_phase_angles);
        // idAngles order is [pitch, yaw, roll]; the orient is stored [roll, pitch, yaw].
        self.orient = [
            ph(rv[2], rp[2]).cos() * ra[2] * DEG2RAD,
            ph(rv[0], rp[0]).cos() * ra[0] * DEG2RAD,
            ph(rv[1], rp[1]).cos() * ra[1] * DEG2RAD,
        ];
        step
    }
}

/// True when the animated bob-cycle web (idHandsBobCycle, hands+0x3d78) runs: 0x140d83e80 zeroes its root alpha
/// whenever the weapon's weaponBob.enable is set, and returns early unless handsBobCycle_Enable, pm_doom4BobCycle
/// and !pm_noBob.
pub fn bob_cycle_web_active(
    prm: &WeaponBobParams,
    cv: &HandsLayerCvars,
    pm_doom4_bob_cycle: bool,
    pm_no_bob: bool,
) -> bool {
    cv.hands_bob_cycle_enable && pm_doom4_bob_cycle && !pm_no_bob && !prm.enable
}

// ---- joint mods --------------------------------------------------------------------------------------------------

/// The joints idHands modifies (0x140d8ae10): names on the hands model, resolved at weapon equip.
pub const JOINT_LEFT_HAND: &str = "lefthandattach";
pub const JOINT_RIGHT_HAND: &str = "righthandattach";
pub const JOINT_LEFT_SHOULDER: &str = "rig_arm_left_root";
pub const JOINT_RIGHT_SHOULDER: &str = "rig_arm_right_root";

/// One idMD6Blend::jointMod_t with flags 0x4b = DRIVER_MODEL | DRIVER_ROTATION | DRIVER_TRANSLATION (+ pass 0x40).
/// The engine applies the four to a COPY of the hands' local pose at full strength (0x141737280, see
/// [`apply_model_joint_mod`]: model-space pre-multiply of the rotation about the joint's own origin, model-space
/// translation, both expressed back in local space), then merges that copy into the pose with the REF_LERP LERP
/// kernel at alpha [`LAG_MERGE_ALPHA`] (see [`lag_merge`]): the lag and bob land at 95%. The arm IK runs afterwards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointMod {
    pub joint: &'static str,
    pub rot: IdMat3,
    /// Game units, hands model space.
    pub translation: Vec3,
}

impl JointMod {
    pub fn rotation_quat(&self) -> Quat {
        Quat::from_mat3(&to_glam(&self.rot))
    }
}

/// A joint's local transform (game units), as in an md6 pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalJoint {
    pub t: Vec3,
    pub q: Quat,
    pub s: Vec3,
}

/// The parent's model transform the engine uses for MODEL mods (0x141738f80): pos accumulates `rot(Q[a], pos) +
/// T[a]` up the ancestors without scaling, rot = Q[a] * rot, scale multiplies. Identity for children of the root.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParentModel {
    pub pos: Vec3,
    pub rot: Quat,
    pub scale: Vec3,
}

impl ParentModel {
    pub const IDENTITY: Self = Self { pos: Vec3::ZERO, rot: Quat::IDENTITY, scale: Vec3::ONE };
}

/// idHands' jointModLagAnimator merge alpha (0x14204133c): REF_LERP, blendType LINEAR.
pub const LAG_MERGE_ALPHA: f32 = 0.95;

/// 0x141737280 for a MODEL | ROTATION | TRANSLATION mod at full strength: the joint's model rotation is pre-multiplied
/// by the mod rotation (q' = normalize(conj(P) * (M * (P * q)))) and the mod translation is added to its model
/// position (t' = rot(conj(P), ((rot(P, sc * t) + pos + m.t) - pos) / sc)). Scale is untouched.
pub fn apply_model_joint_mod(local: LocalJoint, parent: &ParentModel, m: &JointMod) -> LocalJoint {
    let p = parent.rot;
    let mq = m.rotation_quat();
    let q = (p.conjugate() * (mq * (p * local.q))).normalize();
    let x = ((p * (parent.scale * local.t) + parent.pos + m.translation) - parent.pos) * (Vec3::ONE / parent.scale);
    LocalJoint { t: p.conjugate() * x, q, s: local.s }
}

/// The LERP kernel (0x14173e400) between the unlagged joint and its lagged copy, both at full weight 255:
/// t = alpha, q = normalize((qL - qL*t) + qR*(+-t)) (sign from dot(qL, qR)), translation/scale (R - L)*t + L.
pub fn lag_merge(unlagged: LocalJoint, lagged: LocalJoint, alpha: f32) -> LocalJoint {
    let (l, r) = (unlagged.q, lagged.q);
    let s = if l.dot(r) < 0.0 { -alpha } else { alpha };
    let q = Quat::from_vec4((Vec4::from(l) - Vec4::from(l) * alpha) + Vec4::from(r) * s).normalize();
    LocalJoint {
        t: (lagged.t - unlagged.t) * alpha + unlagged.t,
        q,
        s: (lagged.s - unlagged.s) * alpha + unlagged.s,
    }
}

/// What the hands pose gets per mod joint: the mod at full strength on a copy, merged back at [`LAG_MERGE_ALPHA`].
pub fn lagged_local(local: LocalJoint, parent: &ParentModel, m: &JointMod) -> LocalJoint {
    lag_merge(local, apply_model_joint_mod(local, parent, m), LAG_MERGE_ALPHA)
}

/// Model-space origins (game units) of the four joints in the hands pose the mods are computed against.
#[derive(Debug, Clone, Copy)]
pub struct JointOrigins {
    pub left_hand: Vec3,
    pub right_hand: Vec3,
    pub left_shoulder: Vec3,
    pub right_shoulder: Vec3,
}

/// The rotation the hands get: lag basis, then the bob roll/pitch/yaw (0x140d8c7e0).
pub fn hands_rotation(lag: &WeaponLag, bob: &WeaponBob) -> IdMat3 {
    let mut m = lag.axis;
    rotate_mat3(&mut m, [1.0, 0.0, 0.0], bob.orient[0]);
    rotate_mat3(&mut m, [0.0, 1.0, 0.0], bob.orient[1]);
    rotate_mat3(&mut m, [0.0, 0.0, 1.0], bob.orient[2]);
    m
}

/// The hands translation: bob offset plus pendulum displacement, metres -> units.
pub fn hands_translation(lag: &WeaponLag, bob: &WeaponBob) -> Vec3 {
    Vec3::new(
        (bob.position[0] + lag.pos[0]) * METERS_TO_UNITS,
        (bob.position[1] + lag.pos[1]) * METERS_TO_UNITS,
        (bob.position[2] + lag.pos[2]) * METERS_TO_UNITS,
    )
}

/// 0x140d8c7e0: the hands (both attach joints) turn rigidly about righthandattach by [`hands_rotation`] and move by
/// [`hands_translation`]; the shoulder IK roots only get the matching displacement (no rotation).
pub fn compose_joint_mods(lag: &WeaponLag, bob: &WeaponBob, j: &JointOrigins) -> [JointMod; 4] {
    let t = hands_translation(lag, bob);
    let m = hands_rotation(lag, bob);
    let about_pivot = |p: Vec3| {
        let d = p - j.right_hand;
        Vec3::new(
            (((d.x * m[0][0] + d.y * m[1][0]) + d.z * m[2][0]) - d.x) + t.x,
            (((d.x * m[0][1] + d.y * m[1][1]) + d.z * m[2][1]) - d.y) + t.y,
            (((d.x * m[0][2] + d.y * m[1][2]) + d.z * m[2][2]) - d.z) + t.z,
        )
    };
    [
        JointMod { joint: JOINT_LEFT_HAND, rot: m, translation: about_pivot(j.left_hand) },
        JointMod { joint: JOINT_RIGHT_HAND, rot: m, translation: t },
        JointMod { joint: JOINT_LEFT_SHOULDER, rot: IDENTITY, translation: about_pivot(j.left_shoulder) },
        JointMod { joint: JOINT_RIGHT_SHOULDER, rot: IDENTITY, translation: about_pivot(j.right_shoulder) },
    ]
}

/// The per-frame driver: lag, then bob, then the joint mods, as idHands::Update does them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HandsLayers {
    pub lag: WeaponLag,
    pub bob: WeaponBob,
}

impl HandsLayers {
    /// Weapon equip (idWeapon 0x140f1ba10 -> 0x140d8bfe0).
    pub fn reset(&mut self) {
        self.lag.reset();
        self.bob.reset();
    }

    pub fn update(
        &mut self,
        d: &HandsLayerDecl,
        cv: &HandsLayerCvars,
        lag: &LagInput,
        bob: &BobInput,
    ) -> Option<BobStep> {
        self.lag.update(&d.lag, cv, lag);
        self.bob.update(&d.bob, cv, bob)
    }

    pub fn joint_mods(&self, j: &JointOrigins) -> [JointMod; 4] {
        compose_joint_mods(&self.lag, &self.bob, j)
    }
}

// ---- placement and FOV -------------------------------------------------------------------------------------------

/// Where the hands model goes (0x140d7ccd0, written to the hands render entity's origin and axis).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandsPlacement {
    pub origin: Vec3,
    pub axis: IdMat3,
}

/// `view_origin` / `view_axis` are the player's first-person view (idPlayer vslots 0x370 / 0x368, rows forward,
/// left, up). `spring_delta` = stepUpHandsSpring.pos - stepUpViewSpring.pos (player +0x14bb0 - +0x14b80), added
/// along world z. Offsets are hands_offset* cvars plus the weapon's handsOffset / handsOffsetAngles. leanRoll
/// (+0x14bdc) and extraWorldTranslation / extraWorldRotation are zero / identity in the campaign and left out.
pub fn hands_placement(
    view_origin: Vec3,
    view_axis: &IdMat3,
    spring_delta: f32,
    d: &HandsLayerDecl,
    cv: &HandsLayerCvars,
) -> HandsPlacement {
    let off = [
        cv.hands_offset[0] + d.hands_offset[0],
        cv.hands_offset[1] + d.hands_offset[1],
        cv.hands_offset[2] + d.hands_offset[2],
    ];
    let ang = [
        cv.hands_offset_angles[0] + d.hands_offset_angles[0],
        cv.hands_offset_angles[1] + d.hands_offset_angles[1],
        cv.hands_offset_angles[2] + d.hands_offset_angles[2],
    ];
    let a = view_axis;
    let origin = Vec3::new(
        view_origin.x + 0.0 + off[0] * a[0][0] + off[1] * a[1][0] + off[2] * a[2][0],
        view_origin.y + 0.0 + off[0] * a[0][1] + off[1] * a[1][1] + off[2] * a[2][1],
        view_origin.z + (spring_delta + 0.0) + off[0] * a[0][2] + off[1] * a[1][2] + off[2] * a[2][2],
    );
    // Pitch about the view's left axis, yaw about its up axis, roll about its forward axis (idRotation, degrees),
    // combined as axis * Ryaw * Rpitch * Rroll.
    let rp = rotation_to_mat3(a[1], ang[0]);
    let ry = rotation_to_mat3(a[2], ang[1]);
    let rr = rotation_to_mat3(a[0], ang[2]);
    let mul = |x: &IdMat3, y: &IdMat3| {
        let mut o = [[0.0f32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                o[i][j] = x[i][0] * y[0][j] + x[i][1] * y[1][j] + x[i][2] * y[2][j];
            }
        }
        o
    };
    let axis = mul(a, &mul(&mul(&ry, &rp), &rr));
    HandsPlacement { origin, axis }
}

/// Inputs of idHands GetHandsFovScale (0x140d5fe60).
#[derive(Debug, Clone, Copy)]
pub struct FovInput {
    /// Whether a weapon is held (right or left hand item); without one the scale is 0.86.
    pub has_item: bool,
    /// The player's zoom fraction 0..1 (0x140e416c0).
    pub zoom_fraction: f32,
    /// Current value of zoomHandsWeaponFovRatio (hands +0x10358), see [`zoom_hands_fov_target`].
    pub zoom_ratio: f32,
    /// forceHandsFOVScale (hands +0x103b4, from anim events); <= 0 when unused.
    pub force_scale: f32,
    /// The owner is the local first-person player (idPlayer vslot 0x7e8).
    pub local_view: bool,
}

/// GetHandsFovScale (0x140d5fe60): the factor the hands' horizontal FOV gets relative to the view's.
pub fn hands_fov_scale(d: &HandsLayerDecl, cv: &HandsLayerCvars, inp: &FovInput) -> f32 {
    if !inp.local_view || !cv.hands_update_pos {
        return 1.0;
    }
    let (mut base, mut zoomed) = if inp.has_item {
        let base = d.hands_fov_scale;
        (base, if 0.0 < d.zoomed_hands_fov { inp.zoom_ratio } else { base })
    } else {
        (0.86, 0.86)
    };
    if zoomed.abs() <= 1e-18 {
        zoomed = 0.0;
    }
    if base.abs() <= 1e-18 {
        base = 0.0;
    }
    let z = inp.zoom_fraction;
    let mut s = (1.0 - z) * base + zoomed * z;
    if 0.0 < inp.force_scale {
        s = inp.force_scale;
    }
    if 0.0 < cv.hands_fov_scale {
        s = cv.hands_fov_scale;
    }
    // atof(g_fov's default string) / g_fov, cached by the engine.
    (cv.g_fov_default / cv.g_fov) * s
}

/// The value zoomHandsWeaponFovRatio blends to when zooming (0x140d69a90): zoomedHandsFOV / zoomedFOV when the
/// weapon sets zoomedHandsFOV, else handsFovScale.
pub fn zoom_hands_fov_target(d: &HandsLayerDecl) -> f32 {
    if 0.0 < d.zoomed_hands_fov { d.zoomed_hands_fov / d.zoomed_fov } else { d.hands_fov_scale }
}

/// The hands projection (render 0x141af88a0, entity fov scale != 1 path): horizontal FOV = scale * view FOV, and the
/// vertical FOV keeps the view's tan ratio times hands_fovVerticalScaleHack. Degrees in, degrees out.
pub fn hands_projection_fov(scale: f32, view_fov_x: f32, view_fov_y: f32, vertical_scale_hack: f32) -> (f32, f32) {
    let half_tan = |deg: f32| ((deg as f64 * 0.5 * 0.01745329238474369) as f32).tan();
    let fov_x = scale * view_fov_x;
    let ratio = half_tan(view_fov_x) / half_tan(view_fov_y);
    let deg = (half_tan(fov_x) / ratio).atan() * 57.29578;
    let fov_y = ((deg as f64 + deg as f64) * vertical_scale_hack as f64) as f32;
    (fov_x, fov_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits3(v: [f32; 3]) -> [u32; 3] {
        [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
    }

    /// Replays tools/handlayers_emu.py's scenarios and compares with the engine's own integrators run under Unicorn.
    fn replay(
        golden: &[[u32; 6]],
        params: (f32, f32, f32, f32),
        forces: &[(usize, [f32; 3])],
        rk4: bool,
    ) -> (usize, f32) {
        let (length, friction, max_deg, gravity) = params;
        let pp = PendulumParams {
            pivot: [0.0, 0.0, length],
            dt: f32::from_bits(0x3a83126f),
            friction,
            max_angle_rad: max_deg * f32::from_bits(0x3c8efa35),
            length,
        };
        let (mut p, mut v) = ([0.0f32; 3], [0.0f32; 3]);
        let (mut exact, mut worst, mut step) = (0, 0.0f32, 0);
        for &(n, f) in forces {
            let a = [0.0 - f[0], 0.0 - f[1], -gravity - f[2]];
            for _ in 0..n {
                if rk4 {
                    rk4_step(&mut p, &mut v, a, &pp);
                } else {
                    euler_step(&mut p, &mut v, a, &pp);
                }
                step += 1;
                if step % 10 == 0 {
                    let g = &golden[step / 10 - 1];
                    let got = [bits3(p), bits3(v)].concat();
                    if got[..] == g[..] {
                        exact += 1;
                    }
                    for k in 0..6 {
                        let e = f32::from_bits(g[k]);
                        let x = f32::from_bits(got[k]);
                        worst = worst.max((x - e).abs() / e.abs().max(1e-6));
                    }
                }
            }
        }
        (exact, worst)
    }

    const HAR: (f32, f32, f32, f32) = (0.125, 15.0, 8.0, 3.8);
    const HAR_FORCES: [(usize, [f32; 3]); 3] =
        [(300, [0.5, 0.0, 0.0]), (300, [0.0, -0.5, 0.1]), (400, [0.0, 0.0, 0.0])];
    const CLAMP: (f32, f32, f32, f32) = (0.125, 20.0, 4.0, 3.8);
    const CLAMP_FORCES: [(usize, [f32; 3]); 4] =
        [(5, [0.0, 0.0, 0.0]), (495, [1.0, 0.8, -0.3]), (300, [-1.0, 0.0, 0.0]), (200, [0.0, 0.0, 0.0])];

    #[test]
    fn euler_matches_engine() {
        let (exact, worst) = replay(&golden::HAR_EULER, HAR, &HAR_FORCES, false);
        assert_eq!(exact, golden::HAR_EULER.len(), "bit-exact samples (worst rel err {worst})");
        let (exact, worst) = replay(&golden::CLAMP_EULER, CLAMP, &CLAMP_FORCES, false);
        // The 4 degree clamp engages here (acos / idRotation sin+cos): still bit-exact against the engine's CRT.
        assert_eq!(exact, 100, "clamp scenario bit-exact samples (worst rel err {worst})");
    }

    #[test]
    fn rk4_matches_engine() {
        let (exact, worst) = replay(&golden::HAR_RK4, HAR, &HAR_FORCES, true);
        assert_eq!(exact, golden::HAR_RK4.len(), "bit-exact samples (worst rel err {worst})");
        let (exact, worst) = replay(&golden::CLAMP_RK4, CLAMP, &CLAMP_FORCES, true);
        // The 4 degree clamp engages here (acos / idRotation sin+cos): still bit-exact against the engine's CRT.
        assert_eq!(exact, 100, "clamp scenario bit-exact samples (worst rel err {worst})");
    }

    #[test]
    fn clamp_holds_max_angle() {
        let pp = PendulumParams {
            pivot: [0.0, 0.0, 0.125],
            dt: LAG_STEP,
            friction: 20.0,
            max_angle_rad: 4.0 * DEG2RAD,
            length: 0.125,
        };
        let (mut p, mut v) = ([0.0f32; 3], [0.0f32; 3]);
        for _ in 0..2000 {
            rk4_step(&mut p, &mut v, [-1.0, -0.8, -3.5], &pp);
        }
        let d = Vec3::new(p[0], p[1], p[2] - 0.125);
        let angle = (-d.z / d.length()).acos();
        assert!((d.length() - 0.125).abs() < 1e-6);
        assert!(angle <= 4.0 * DEG2RAD + 1e-4, "angle {angle}");
    }

    #[test]
    fn rest_is_identity() {
        let mut lag = WeaponLag::default();
        let prm = WeaponLagParams {
            enable: true,
            pendulum_length: 0.125,
            friction: 15.0,
            max_angle_degrees: 8.0,
            max_acceleration: 0.5,
            gravity: 3.8,
            weapon_dip_forward: false,
        };
        let inp = LagInput {
            acceleration: Vec3::ZERO,
            view_angles: [0.0, 30.0, 0.0],
            prev_view_angles: [0.0, 30.0, 0.0],
            right_hand_origin: Vec3::new(20.0, -5.0, -6.0),
            msec: 16,
        };
        lag.update(&prm, &HandsLayerCvars::default(), &inp);
        assert_eq!(lag.pos, [0.0; 3]);
        assert_eq!(lag.axis, IDENTITY);
    }

    #[test]
    fn forward_acceleration_swings_back_without_tilt() {
        let mut lag = WeaponLag::default();
        let prm = WeaponLagParams {
            enable: true,
            pendulum_length: 0.125,
            friction: 15.0,
            max_angle_degrees: 8.0,
            max_acceleration: 0.5,
            gravity: 3.8,
            weapon_dip_forward: false,
        };
        let cv = HandsLayerCvars::default();
        // Accelerating along the view forward (yaw 90 = +y world).
        let inp = LagInput {
            acceleration: Vec3::new(0.0, 2000.0, 0.0),
            view_angles: [0.0, 90.0, 0.0],
            prev_view_angles: [0.0, 90.0, 0.0],
            right_hand_origin: Vec3::ZERO,
            msec: 16,
        };
        for _ in 0..10 {
            lag.update(&prm, &cv, &inp);
        }
        assert!(lag.pos[0] < -1e-4, "hands lag behind: {:?}", lag.pos);
        assert!(lag.pos[1].abs() < 1e-6);
        // No dip: forward swing does not rotate.
        assert!((lag.axis[0][0] - 1.0).abs() < 1e-6 && lag.axis[1][2].abs() < 1e-6);
    }

    #[test]
    fn turning_rolls_the_weapon() {
        let mut lag = WeaponLag::default();
        let prm = WeaponLagParams {
            enable: true,
            pendulum_length: 0.125,
            friction: 15.0,
            max_angle_degrees: 8.0,
            max_acceleration: 0.5,
            gravity: 3.8,
            weapon_dip_forward: false,
        };
        let cv = HandsLayerCvars::default();
        let mut yaw = 0.0f32;
        for _ in 0..10 {
            let inp = LagInput {
                acceleration: Vec3::ZERO,
                view_angles: [0.0, yaw + 2.0, 0.0],
                prev_view_angles: [0.0, yaw, 0.0],
                right_hand_origin: Vec3::new(20.0, -8.0, -10.0),
                msec: 16,
            };
            lag.update(&prm, &cv, &inp);
            yaw += 2.0;
        }
        // Turning left pushes the hands right (negative y) and rolls the basis.
        assert!(lag.pos[1] < 0.0, "{:?}", lag.pos);
        assert!(lag.axis[1][2].abs() > 1e-4);
    }

    #[test]
    fn bob_steps_and_offsets() {
        let prm = WeaponBobParams {
            enable: true,
            stride: 0.5,
            stride_crouched: 0.2,
            translation_amplitudes: [0.0, 0.0, 0.003],
            translation_angular_velocities: [0.0, 0.0, 0.9],
            translation_phase_angles: [0.0; 3],
            rotational_amplitudes: [0.5, 0.4, 0.0],
            rotational_angular_velocities: [0.9, 0.45, 0.0],
            rotational_phase_angles: [90.0, 0.0, 0.0],
        };
        let cv = HandsLayerCvars::default();
        let mut bob = WeaponBob::default();
        let inp = BobInput { velocity: Vec3::new(320.0, 0.0, 0.0), on_ground: true, crouched: false, msec: 16 };
        let mut steps = 0;
        for _ in 0..200 {
            if bob.update(&prm, &cv, &inp).is_some() {
                steps += 1;
            }
        }
        // 3.2 s at 320 u/s = 19.5 m over a 0.5 m stride: phase 39 rad, 6 crossings of 2*pi.
        assert!((bob.angle - 320.0 * 0.01905 * 0.016 * 200.0 / 0.5).abs() < 0.01, "{}", bob.angle);
        assert_eq!(steps, 6);
        assert_eq!(bob.num_steps, 6);
        let a = wrap_two_pi(bob.angle * 0.9);
        assert!((bob.position[2] - a.cos() * 0.003).abs() < 1e-7);
        // pitch phase 90 degrees: cos(a + pi/2)
        let p = wrap_two_pi(bob.angle * 0.9 + 90.0 * DEG2RAD);
        assert!((bob.orient[1] - p.cos() * 0.5 * DEG2RAD).abs() < 1e-7);
        assert_eq!(bob.orient[0], 0.0);
    }

    #[test]
    fn joint_mods_rotate_about_right_hand() {
        let lag = WeaponLag { pos: [0.0, 0.01, 0.0], ..Default::default() };
        let bob = WeaponBob { orient: [0.0, 0.0, 0.1], ..Default::default() };
        let j = JointOrigins {
            left_hand: Vec3::new(10.0, 5.0, 0.0),
            right_hand: Vec3::new(10.0, -5.0, 0.0),
            left_shoulder: Vec3::new(0.0, 8.0, 10.0),
            right_shoulder: Vec3::new(0.0, -8.0, 10.0),
        };
        let mods = compose_joint_mods(&lag, &bob, &j);
        let t = Vec3::new(0.0, 0.01 * METERS_TO_UNITS, 0.0);
        assert!((mods[1].translation - t).length() < 1e-5);
        // The left hand ends up where rotating it about the right hand by the yaw and adding t puts it.
        let r = to_glam(&mods[0].rot);
        let expect = j.right_hand + r * (j.left_hand - j.right_hand) + t;
        assert!((j.left_hand + mods[0].translation - expect).length() < 1e-4);
        assert_eq!(mods[2].rot, IDENTITY);
        let expect = j.right_hand + r * (j.left_shoulder - j.right_hand) + t;
        assert!((j.left_shoulder + mods[2].translation - expect).length() < 1e-4);
    }

    #[test]
    fn lag_lands_at_95_percent() {
        let m = JointMod { joint: JOINT_RIGHT_HAND, rot: rotation_to_mat3([1.0, 0.0, 0.0], 10.0), translation: Vec3::new(0.0, 2.0, 0.0) };
        let local = LocalJoint { t: Vec3::new(30.0, 0.0, 65.0), q: Quat::IDENTITY, s: Vec3::ONE };
        let full = apply_model_joint_mod(local, &ParentModel::IDENTITY, &m);
        // Under the root: plain pre-multiply and add.
        assert!((full.t - Vec3::new(30.0, 2.0, 65.0)).length() < 1e-5);
        assert!(full.q.angle_between(m.rotation_quat()) < 1e-5);
        let out = lagged_local(local, &ParentModel::IDENTITY, &m);
        assert!((out.t - Vec3::new(30.0, 1.9, 65.0)).length() < 1e-5);
        let angle = out.q.angle_between(Quat::IDENTITY).to_degrees();
        assert!((angle - 9.5).abs() < 0.01, "nlerp of 10 degrees at 0.95: {angle}");
        // Under a rotated, translated parent the model-space effect is the same.
        let parent = ParentModel { pos: Vec3::new(5.0, 1.0, 2.0), rot: Quat::from_rotation_z(0.7), scale: Vec3::ONE };
        let full = apply_model_joint_mod(local, &parent, &m);
        let model = |j: LocalJoint| (parent.pos + parent.rot * j.t, parent.rot * j.q);
        let (p0, r0) = model(local);
        let (p1, r1) = model(full);
        assert!((p1 - (p0 + m.translation)).length() < 1e-4);
        assert!((r1 * r0.conjugate()).angle_between(m.rotation_quat()) < 1e-5);
    }

    #[test]
    fn fov_scale_and_projection() {
        let d = HandsLayerDecl { hands_fov_scale: 0.75, ..Default::default() };
        let mut cv = HandsLayerCvars::default();
        let inp = FovInput { has_item: true, zoom_fraction: 0.0, zoom_ratio: 0.0, force_scale: 0.0, local_view: true };
        assert_eq!(hands_fov_scale(&d, &cv, &inp), 0.75);
        cv.g_fov = 120.0;
        assert!((hands_fov_scale(&d, &cv, &inp) - 0.75 * 0.75).abs() < 1e-7);
        let (fx, fy) = hands_projection_fov(0.75, 90.0, 73.74, 1.0);
        assert_eq!(fx, 67.5);
        // Same aspect: tan(fy/2) / tan(fx/2) = tan(73.74/2) / tan(45).
        let r = (fy.to_radians() * 0.5).tan() / (fx.to_radians() * 0.5).tan();
        assert!((r - (73.74f32.to_radians() * 0.5).tan()).abs() < 1e-4);
    }

    #[test]
    fn decl_defaults_and_overrides() {
        let src = r#"{ edit = { handsFovScale = 0.75; weaponLag = { enable = true; pendulumLength = 0.125; }
            weaponBob = { enable = true; translationAmplitudes = { up_down = 0.003; x = 0.001; } rotationalPhaseAngles = { pitch = 90; } }
            ironSightZoom = { zoomedFOV = 75; zoomedHandsFOV = 40; } } }"#;
        let b = idres::decl::parse(src).unwrap();
        let d = HandsLayerDecl::from_edit(b.block("edit").unwrap());
        assert!(d.lag.enable && d.lag.pendulum_length == 0.125 && d.lag.friction == 5.0 && d.lag.gravity == 9.81);
        assert_eq!(d.bob.translation_amplitudes, [0.004, 0.002, 0.003]);
        assert_eq!(d.bob.rotational_phase_angles, [90.0, 0.0, 0.0]);
        assert_eq!(d.bob.stride, 0.55);
        assert_eq!(zoom_hands_fov_target(&d), 40.0 / 75.0);
        let none = HandsLayerDecl::from_edit(&Block::default());
        assert_eq!(none.hands_fov_scale, 0.7);
        assert!(!none.lag.enable && !none.bob.enable);
    }

    /// tools/handlayers_emu.py --frames: 90 frames of idHands lag + bob + joint mods run in the engine under
    /// Unicorn (fake player / decl / view), replayed here through [`HandsLayers`].
    #[test]
    fn frames_match_engine() {
        replay_frames(&golden::FRAMES, false, 1);
    }

    #[test]
    fn frames_dip_euler_match_engine() {
        replay_frames(&golden::FRAMES_DIP_EULER, true, 0);
    }

    fn replay_frames(frames: &[Frame], dip: bool, method: i32) {
        let d = HandsLayerDecl {
            lag: WeaponLagParams {
                enable: true,
                pendulum_length: 0.125,
                friction: 15.0,
                max_angle_degrees: 8.0,
                max_acceleration: 0.5,
                gravity: 3.8,
                weapon_dip_forward: dip,
            },
            bob: WeaponBobParams {
                enable: true,
                stride: 0.5,
                stride_crouched: 0.2,
                translation_amplitudes: [0.0, 0.0, 0.003],
                translation_angular_velocities: [0.0, 0.0, 0.9],
                translation_phase_angles: [0.0; 3],
                rotational_amplitudes: [0.5, 0.4, 0.0],
                rotational_angular_velocities: [0.9, 0.45, 0.0],
                rotational_phase_angles: [90.0, 0.0, 0.0],
            },
            ..Default::default()
        };
        let cv = HandsLayerCvars { weapon_lag_integration: method, ..Default::default() };
        let j = JointOrigins {
            left_hand: Vec3::new(18.0, 9.5, -12.0),
            right_hand: Vec3::new(21.0, -6.0, -10.5),
            left_shoulder: Vec3::new(-2.0, 10.0, 4.0),
            right_shoulder: Vec3::new(-2.0, -10.0, 4.0),
        };
        let mut hl = HandsLayers::default();
        let (mut yaw, mut pitch) = (10.0f64, -5.0f64);
        for (i, (fields, mods, steps)) in frames.iter().enumerate() {
            let msec = [16, 17, 15][i % 3];
            let prev = [pitch as f32, yaw as f32, 0.0];
            let (accel, vel, crouched) = if i < 30 {
                yaw += 1.5;
                ([1500.0, 300.0, 0.0], [(25.0 * i as f32).min(320.0), 4.0 * i as f32, 0.0], false)
            } else if i < 55 {
                pitch += 0.7;
                ([-200.0, -900.0, 30.0], [150.0, -280.0, 0.0], false)
            } else if i < 75 {
                yaw -= 3.0;
                ([0.0; 3], [90.0, 30.0, 0.0], true)
            } else {
                ([-2500.0, 0.0, 0.0], [0.0; 3], false)
            };
            let lag = LagInput {
                acceleration: Vec3::from(accel),
                view_angles: [pitch as f32, yaw as f32, 0.0],
                prev_view_angles: prev,
                right_hand_origin: j.right_hand,
                msec,
            };
            let bob = BobInput { velocity: Vec3::from(vel), on_ground: i < 80, crouched, msec };
            let step = hl.update(&d, &cv, &lag, &bob);
            let a = hl.lag.axis;
            let b = hl.bob;
            let got: [u32; 23] = [
                hl.lag.pos[0].to_bits(),
                hl.lag.pos[1].to_bits(),
                hl.lag.pos[2].to_bits(),
                hl.lag.vel[0].to_bits(),
                hl.lag.vel[1].to_bits(),
                hl.lag.vel[2].to_bits(),
                a[0][0].to_bits(),
                a[0][1].to_bits(),
                a[0][2].to_bits(),
                a[1][0].to_bits(),
                a[1][1].to_bits(),
                a[1][2].to_bits(),
                a[2][0].to_bits(),
                a[2][1].to_bits(),
                a[2][2].to_bits(),
                b.position[0].to_bits(),
                b.position[1].to_bits(),
                b.position[2].to_bits(),
                b.angle.to_bits(),
                b.orient[0].to_bits(),
                b.orient[1].to_bits(),
                b.orient[2].to_bits(),
                b.num_steps as u32,
            ];
            assert_eq!(&got[..], &fields[..23], "idHands fields, frame {i}");
            assert_eq!(step.map(|s| s.odd as u8).as_slice(), *steps, "footsteps, frame {i}");
            for (k, m) in hl.joint_mods(&j).iter().enumerate() {
                let r = m.rot;
                let t = m.translation;
                let jm =
                    [r[0][0], r[1][0], r[2][0], t.x, r[0][1], r[1][1], r[2][1], t.y, r[0][2], r[1][2], r[2][2], t.z];
                let jm: Vec<u32> = jm.iter().map(|x| x.to_bits()).collect();
                assert_eq!(&jm[..], &mods[k][..], "joint mod {k} ({}), frame {i}", m.joint);
            }
        }
    }

    /// The campaign weapons' decls from the user's install (skipped without one).
    #[test]
    fn install_decls() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let har = HandsLayerDecl::from_decl(&inst.decls, "weapon/zion/player/sp/heavy_rifle_heavy_ar").unwrap();
        assert_eq!(
            har.lag,
            WeaponLagParams {
                enable: true,
                pendulum_length: 0.125,
                friction: 15.0,
                max_angle_degrees: 8.0,
                max_acceleration: 0.5,
                gravity: 3.8,
                weapon_dip_forward: false
            }
        );
        // weapon/zion/player/sp/assault_rifle_assaultrifle's legacy x/y/z keys must not leak in.
        assert!(har.bob.enable);
        assert_eq!(har.bob.translation_amplitudes, [0.0, 0.0, 0.003]);
        assert_eq!(har.bob.rotational_amplitudes, [0.5, 0.4, 0.0]);
        assert_eq!(har.bob.rotational_phase_angles, [90.0, 0.0, 0.0]);
        assert_eq!(har.hands_fov_scale, 0.75);
        // Rocket launcher and fists leave handsFovScale to the idDeclInventory default.
        let rl = HandsLayerDecl::from_decl(&inst.decls, "weapon/zion/player/sp/rocket_launcher").unwrap();
        assert_eq!(rl.hands_fov_scale, 0.7);
        let fists = HandsLayerDecl::from_decl(&inst.decls, "weapon/zion/player/sp/fists").unwrap();
        assert!(!fists.bob.enable, "fists use the animated bob cycle");
        let cv = HandsLayerCvars::from_cvars(&inst.cvars, 90.0);
        assert_eq!(cv, HandsLayerCvars::default());
    }

    /// (idHands +0x5b20..+0x5b7c, the 4 joint mods as idJointMat rows, footsteps) per frame.
    type Frame = ([u32; 24], [[u32; 12]; 4], &'static [u8]);

    #[allow(clippy::large_const_arrays)]
    mod golden {
        use super::Frame;
        include!("handlayers_golden.in");
    }
}
