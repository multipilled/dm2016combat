//! First-person view effects on the camera transform, from the user's own DOOMx64.exe (build 13954591):
//! screen shakes (idDeclViewShake, FX camera shakes), advanced shakes (idDeclAdvancedViewShake), the damage view kick
//! evaluation, the double-jump view pitch change and the hands' animated `camera` joint. Notes with VAs:
//! gamedata/re/VIEWFX.md.
//!
//! Order per frame (what the engine does to the first-person view):
//! 1. view angles: usercmd + weapon kick (weapons::kick) + [`double_jump_pitch`] (idPlayer 0x140e3f070; it changes
//!    the real view angles, so aim too);
//! 2. idPlayer::CalculateFirstPersonView 0x140e3beb0: origin = eye, axis = ToMat3(view angles), then
//!    [`apply_animated_camera`] with the hands rig's `camera` joint, then the step-up view spring on z;
//! 3. idView::RenderView 0x140e6b530 on that view: damage kick angles ([`ViewKick::angles`]), then (unless shakes are
//!    skipped) [`ViewFx::apply_shakes`]: the decl / FX screen shake (0x140e69be0) and the 4 advanced shake slots
//!    (0x140e68520).

use anyhow::{Context, Result};
use idres::decl::Block;
use idres::decldb::DeclDb;

use crate::handlayers::reactions::DeclTable;
use crate::handlayers::{inv_sqrt, IdMat3};
use crate::weapons::GameRng;

/// The engine timing struct (0x1436336e8, filled by 0x140211e20): ticks per second = msecPerFrame (16) * engineHz
/// (60) = 960 (+0x10), and its reciprocal (+0x1c). Game time ("now") counts these ticks.
pub const TICKS_PER_SEC: i32 = 960;

/// renderView_t vieworg (+0x60) and viewaxis (+0x6c; rows forward, left, up), game units, idTech axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub origin: [f32; 3],
    pub axis: IdMat3,
}

/// The exe's CRT expf (0x141ed3cb0, SSE2 path): computed in double, rounded to float.
fn crt_expf(x: f32) -> f32 {
    (x as f64).exp() as f32
}

/// The exe's CRT cosf (0x141ed2c70, SSE2 path): double-precision reduction and polynomial, rounded to float. (CPUs
/// with FMA3 take another CRT path, 0x141ed2f50; the oracle runs the SSE2 one.)
fn crt_cosf(x: f32) -> f32 {
    (x as f64).cos() as f32
}

/// idMat3::ToAngles (0x1402e8900): [pitch, yaw, roll] degrees.
pub fn mat3_to_angles(m: &IdMat3) -> [f32; 3] {
    let l2 = m[0][0] * m[0][0] + m[0][1] * m[0][1];
    let cp = inv_sqrt(l2) * l2;
    if cp > 1.192_092_9e-7 {
        [m[0][2].atan2(cp) * -57.295_776, m[0][1].atan2(m[0][0]) * 57.295_776, m[1][2].atan2(m[2][2]) * 57.295_776]
    } else {
        let p = if 0.0 > m[0][2] { 90.0 } else { -90.0 };
        [p, m[1][0].atan2(m[1][1]) * -57.295_776, 0.0]
    }
}

fn crt_sinf(x: f32) -> f32 {
    (x as f64).sin() as f32
}

/// idAngles::ToMat3 (0x1402d90c0) with the CRT's double-based sinf / cosf: [pitch, yaw, roll] degrees.
pub fn angles_to_mat3(a: [f32; 3]) -> IdMat3 {
    const DEG2RAD: f32 = 0.017_453_292;
    radians_to_mat3(a[0] * DEG2RAD, a[1] * DEG2RAD, a[2] * DEG2RAD)
}

/// idAngles::ToMat3 on radians (0x1402e6f60).
pub fn radians_to_mat3(pitch: f32, yaw: f32, roll: f32) -> IdMat3 {
    let (sy, cy) = (crt_sinf(yaw), crt_cosf(yaw));
    let (sp, cp) = (crt_sinf(pitch), crt_cosf(pitch));
    let (sr, cr) = (crt_sinf(roll), crt_cosf(roll));
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

/// viewShakeMethod_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShakeMethod {
    #[default]
    Constant = 0,
    ExponentialDecay = 1,
    LinearDecay = 2,
    LinearEaseInOut = 3,
}

/// viewShakeMode_t values (the decl names; note the exe applies PITCH/ROLL modes to the opposite angle slots).
pub const SHAKE_MODES: [&str; 15] = [
    "SHAKE_RANDOM_ANGLE_POS",
    "SHAKE_RANDOM_ANGLE",
    "SHAKE_RANDOM_POS",
    "SHAKE_PITCH_ANGLE",
    "SHAKE_ROLL_ANGLE",
    "SHAKE_YAW_ANGLE",
    "SHAKE_X_POS",
    "SHAKE_Y_POS",
    "SHAKE_Z_POS",
    "MOVE_PITCH_ANGLE",
    "MOVE_ROLL_ANGLE",
    "MOVE_YAW_ANGLE",
    "MOVE_X_POS",
    "MOVE_Y_POS",
    "MOVE_Z_POS",
];

/// idDeclViewShake::shakeViewInfo_t. blendingValue defaults to 1 (inferred: the exported decls write it only once,
/// as 0.5, and a 0 weight would disable the entry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShakeViewInfo {
    pub blending: f32,
    pub mode: u32,
    pub positive: bool,
    pub negative: bool,
}

impl Default for ShakeViewInfo {
    fn default() -> Self {
        Self { blending: 1.0, mode: 0, positive: false, negative: false }
    }
}

/// idDeclViewShake (ctor 0x1406f04b0 defaults; shakePercentage = shakeScalePower / 100 after parsing, 0x1406f0630).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewShakeDecl {
    pub player_max_shake_scale: f32,
    /// [pitch, yaw, roll] degrees.
    pub max_shake_angles: [f32; 3],
    pub max_shake_offset: [f32; 3],
    pub shake_scale_power: f32,
    /// Game ticks.
    pub shake_duration: i32,
    pub method: ShakeMethod,
    pub infos: Vec<ShakeViewInfo>,
    pub fade_intensity: bool,
    pub shake_percentage: f32,
}

impl Default for ViewShakeDecl {
    fn default() -> Self {
        Self {
            player_max_shake_scale: 1.0,
            max_shake_angles: [10.0; 3],
            max_shake_offset: [5.0; 3],
            shake_scale_power: 2.5,
            shake_duration: 250,
            method: ShakeMethod::Constant,
            infos: Vec::new(),
            fade_intensity: true,
            shake_percentage: 2.5 / 100.0,
        }
    }
}

fn vec3(b: Option<&Block>, keys: [&str; 3], def: [f32; 3]) -> [f32; 3] {
    let Some(b) = b else { return def };
    [0, 1, 2].map(|i| b.f32(keys[i]).unwrap_or(def[i]))
}

impl ViewShakeDecl {
    /// `name` as referenced, e.g. "screenviewshake/meleeleft".
    pub fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("screenviewshake", name).with_context(|| format!("screenViewShake {name}"))?;
        Ok(Self::from_edit(b.block("edit").unwrap_or(&Block::default())))
    }

    pub fn from_edit(e: &Block) -> Self {
        let mut d = Self::default();
        d.player_max_shake_scale = e.f32("playerMaxShakeScale").unwrap_or(d.player_max_shake_scale);
        d.max_shake_angles = vec3(e.block("maxShakeAngles"), ["pitch", "yaw", "roll"], d.max_shake_angles);
        d.max_shake_offset = vec3(e.block("maxShakeOffset"), ["x", "y", "z"], d.max_shake_offset);
        d.shake_scale_power = e.f32("shakeScalePower").unwrap_or(d.shake_scale_power);
        d.shake_duration = e.f32("shakeDuration").map(|v| v as i32).unwrap_or(d.shake_duration);
        d.method = match e.str("camShakeMethod") {
            Some("EXPONENTIAL_DECAY") => ShakeMethod::ExponentialDecay,
            Some("LINEAR_DECAY") => ShakeMethod::LinearDecay,
            Some("LINEAR_EASE_IN_OUT") => ShakeMethod::LinearEaseInOut,
            _ => ShakeMethod::Constant,
        };
        if let Some(v) = e.path("fadeIntensity").and_then(|v| v.as_bool()) {
            d.fade_intensity = v;
        }
        if let Some(list) = e.block("shakeViewInfoList") {
            let n = list.f32("num").unwrap_or(0.0) as usize;
            for i in 0..n {
                let Some(it) = list.block(&format!("item[{i}]")) else { continue };
                let mut s = ShakeViewInfo::default();
                s.blending = it.f32("blendingValue").unwrap_or(s.blending);
                if let Some(m) = it.str("camShakeMode") {
                    s.mode = SHAKE_MODES.iter().position(|x| *x == m).unwrap_or(0) as u32;
                }
                s.positive = it.path("shakePositive").and_then(|v| v.as_bool()).unwrap_or(false);
                s.negative = it.path("shakeNegative").and_then(|v| v.as_bool()).unwrap_or(false);
                d.infos.push(s);
            }
        }
        d.shake_percentage = d.shake_scale_power / 100.0;
        d
    }
}

/// idDeclAdvancedViewShake::shakeData_t (ctor 0x1406e1fe0 defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancedShakeData {
    pub amplitude_table: Option<DeclTable>,
    /// Translation: forward / left / up units; rotation: roll / pitch / yaw degrees.
    pub scale: [f32; 3],
    pub min_frequency_hz: f32,
    pub max_frequency_hz: f32,
    pub max_sample_randomization: f32,
    pub max_amplitude_randomization: f32,
}

impl Default for AdvancedShakeData {
    fn default() -> Self {
        Self {
            amplitude_table: None,
            scale: [0.0; 3],
            min_frequency_hz: 1.0,
            max_frequency_hz: 1.0,
            max_sample_randomization: 0.1,
            max_amplitude_randomization: 0.75,
        }
    }
}

impl AdvancedShakeData {
    fn read(db: &DeclDb, b: Option<&Block>) -> Result<Self> {
        let mut d = Self::default();
        let Some(b) = b else { return Ok(d) };
        if let Some(t) = b.str("amplitudeTable").filter(|t| !t.is_empty()) {
            d.amplitude_table = Some(DeclTable::load(db, t)?);
        }
        d.scale = vec3(b.block("scale"), ["x", "y", "z"], d.scale);
        d.min_frequency_hz = b.f32("minFrequencyHz").unwrap_or(d.min_frequency_hz);
        d.max_frequency_hz = b.f32("maxFrequencyHz").unwrap_or(d.max_frequency_hz);
        d.max_sample_randomization = b.f32("maxSampleRandomizationPercent").unwrap_or(d.max_sample_randomization);
        d.max_amplitude_randomization = b.f32("maxAmplitudeRandomizationPercent").unwrap_or(d.max_amplitude_randomization);
        Ok(d)
    }
}

/// idDeclAdvancedViewShake (ctor 0x1406e1fe0; normalizedRotationScale = rotation scale / 360 after parsing,
/// 0x1406e20e0).
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancedViewShakeDecl {
    /// Milliseconds (converted to ticks with 0.96 when started).
    pub shake_time_ms: i32,
    pub rotation: AdvancedShakeData,
    pub translation: AdvancedShakeData,
    pub normalized_rotation_scale: [f32; 3],
}

impl AdvancedViewShakeDecl {
    /// `name` as referenced, e.g. "screenviewshake/sp/gauss_rifle".
    pub fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("advancedscreenviewshake", name).with_context(|| format!("advancedScreenViewShake {name}"))?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let rotation = AdvancedShakeData::read(db, e.block("rotationShakeParam"))?;
        let translation = AdvancedShakeData::read(db, e.block("translationShakeParam"))?;
        let c = 0.002_777_777_8f32;
        let normalized_rotation_scale = [rotation.scale[0] * c, rotation.scale[1] * c, rotation.scale[2] * c];
        Ok(Self { shake_time_ms: e.f32("shakeTimeMS").map(|v| v as i32).unwrap_or(1000), rotation, translation, normalized_rotation_scale })
    }
}

/// idView::advanceViewShakeDetails_t (0x68).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AdvancedSlot {
    pub normalized_magnitude: f32,
    pub axis_offsets: [f32; 3],
    pub rotation_frequencies: [f32; 3],
    pub translation_frequencies: [f32; 3],
    pub start_time: i32,
    pub end_time: i32,
    pub decl: Option<std::sync::Arc<AdvancedViewShakeDecl>>,
}

/// The idView shake state (idView +0xe20..+0xe9c and +0xea0 advancedViewShakeDetails[4]; ctor 0x140e66a20).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewFx {
    pub decl: Option<ViewShakeDecl>,
    pub normalized_magnitude: f32,
    /// FX camera shake magnitude (idPlayer 0x140e3b3e0); stays until set again.
    pub camera_shake: f32,
    pub player_max_shake_scale: f32,
    pub max_shake_angles: [f32; 3],
    pub max_shake_offset: [f32; 3],
    pub start_time: i32,
    pub decay: f32,
    /// Shake from sounds (sound world vslot 0x178, every frame in 0x140e6ebd0); 0 without shaking sounds.
    pub shake_volume: f32,
    pub fx_position: [f32; 3],
    pub fade_start: f32,
    pub fade_end: f32,
    pub last_applied: f32,
    pub last_offset: [f32; 3],
    pub last_angles: IdMat3,
    pub advanced: [AdvancedSlot; 4],
}

const IDENTITY: IdMat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Default for ViewFx {
    fn default() -> Self {
        Self {
            decl: None,
            normalized_magnitude: 0.0,
            camera_shake: 0.0,
            player_max_shake_scale: 1.0,
            max_shake_angles: [10.0; 3],
            max_shake_offset: [6.0; 3],
            start_time: 0,
            decay: 0.0,
            shake_volume: 0.0,
            fx_position: [0.0; 3],
            fade_start: 0.0,
            fade_end: 0.0,
            last_applied: 0.0,
            last_offset: [0.0; 3],
            last_angles: IDENTITY,
            advanced: Default::default(),
        }
    }
}

fn clamp01(v: f32) -> f32 {
    let v = if 1.0 <= v { 1.0 } else { v };
    if v <= 0.0 { 0.0 } else { v }
}

/// r' = r * M for every row (the decl / FX screen shake).
fn rows_times(axis: &mut IdMat3, m: &IdMat3) {
    for r in axis.iter_mut() {
        let (x, y, z) = (r[0], r[1], r[2]);
        *r = [y * m[1][0] + x * m[0][0] + z * m[2][0], y * m[1][1] + x * m[0][1] + z * m[2][1], x * m[0][2] + y * m[1][2] + z * m[2][2]];
    }
}

impl ViewFx {
    /// idView::StartViewShake 0x140e6fa50 (ae_startScreenShake / ae_startCameraShake via idPlayer 0x140e3ef50 with
    /// magnitude 1, idPlayer 0x140e3b4f0). Ignored while another decl shake runs, or for a decl with no duration /
    /// power. Returns whether it started.
    pub fn start_view_shake(&mut self, decl: &ViewShakeDecl, magnitude: f32, now: i32) -> bool {
        if self.decl.is_some() || decl.shake_duration <= 0 || decl.shake_scale_power <= 0.0 {
            return false;
        }
        self.normalized_magnitude = clamp01(magnitude);
        self.start_time = now;
        self.decay = decl.shake_percentage / decl.shake_duration as f32;
        self.decl = Some(decl.clone());
        true
    }

    /// idPlayer 0x140e3b3e0: an FX camera shake (FX_SCREEN_SHAKE with magnitude; its maxAngles / maxOffset
    /// arguments are ignored, the view keeps 10 deg / 6 units): magnitude, FX position and the fade distances.
    pub fn set_camera_shake(&mut self, magnitude: f32, fx_position: [f32; 3], fade_start: f32, fade_end: f32) {
        self.camera_shake = clamp01(magnitude);
        self.fx_position = fx_position;
        self.fade_start = fade_start;
        self.fade_end = fade_end;
    }

    /// idView::StartAdvancedViewShake 0x140e6f690 (ae_startAdvancedScreenShake with 1, FX advancedShakeDecl,
    /// projectile impacts / gore via [`advanced_distance_magnitude`]). Takes a free slot or the oldest one
    /// (0x140e6d330). Nine game-RNG draws.
    pub fn start_advanced_shake(&mut self, decl: std::sync::Arc<AdvancedViewShakeDecl>, magnitude: f32, now: i32, rng: &mut GameRng) -> bool {
        if decl.shake_time_ms <= 0 {
            return false;
        }
        let mut idx = 0;
        let mut oldest = self.advanced[0].start_time;
        let mut free = None;
        for (i, s) in self.advanced.iter().enumerate() {
            if s.decl.is_none() {
                free = Some(i);
                break;
            }
            if s.start_time < oldest {
                oldest = s.start_time;
                idx = i;
            }
        }
        let s = &mut self.advanced[free.unwrap_or(idx)];
        let ms = decl.shake_time_ms;
        s.normalized_magnitude = clamp01(magnitude);
        s.start_time = now;
        let ticks_per_sec = TICKS_PER_SEC as f32;
        s.end_time = now - ((ms as f32 * -0.001 * ticks_per_sec) as i32);
        for k in 0..3 {
            s.axis_offsets[k] = rng.next01() * std::f32::consts::PI + 0.0;
        }
        let span = |d: &AdvancedShakeData| {
            let lo = if d.max_frequency_hz <= d.min_frequency_hz { d.max_frequency_hz } else { d.min_frequency_hz };
            let hi = if d.min_frequency_hz <= d.max_frequency_hz { d.max_frequency_hz } else { d.min_frequency_hz };
            (lo, hi - lo)
        };
        let (rlo, rspan) = span(&decl.rotation);
        let (tlo, tspan) = span(&decl.translation);
        for k in 0..3 {
            s.rotation_frequencies[k] = rng.next01() * rspan + rlo;
        }
        for k in 0..3 {
            s.translation_frequencies[k] = rng.next01() * tspan + tlo;
        }
        s.decl = Some(decl);
        true
    }

    /// idView 0x140e6b530 shake part: the decl / FX screen shake (0x140e69be0), then advanced slots 0..3 (0x140e68520).
    /// Skipped by the engine when view_skipShakes / g_stopTime / g_freezeTime or in arcade mode.
    pub fn apply_shakes(&mut self, view: &mut View, now: i32, rng: &mut GameRng) {
        self.apply_screen_shake(view, now, rng);
        for i in 0..4 {
            self.apply_advanced(i, view, now, rng);
        }
    }

    fn reset_last(&mut self) {
        self.last_offset = [0.0; 3];
        self.last_angles = IDENTITY;
    }

    /// 0x140e69be0.
    pub fn apply_screen_shake(&mut self, view: &mut View, now: i32, rng: &mut GameRng) {
        let mut s;
        match &self.decl {
            None => s = clamp01(self.camera_shake + self.shake_volume),
            Some(d) => {
                let sp = d.shake_percentage;
                let el = (now - self.start_time) as f32;
                s = match d.method {
                    ShakeMethod::Constant => sp,
                    ShakeMethod::ExponentialDecay => crt_expf(-(el * self.decay)) * sp,
                    ShakeMethod::LinearDecay => sp - el * self.decay,
                    ShakeMethod::LinearEaseInOut => {
                        let x = el * self.decay;
                        let x = x + x;
                        if sp < x { sp - (x - sp) } else { x }
                    }
                };
                if s < 0.0 || self.start_time + d.shake_duration < now {
                    self.decl = None;
                    s = 0.0;
                    self.reset_last();
                }
                s += self.shake_volume;
            }
        }
        if s <= 0.0 {
            self.reset_last();
            self.last_applied = s;
            return;
        }
        let rnd = |rng: &mut GameRng, lo: f32, span: f32| rng.next01() * span + lo;
        match self.decl.clone() {
            None => {
                if 0.0 < self.fade_start && 0.0 < self.fade_end {
                    let dy = self.fx_position[1] - view.origin[1];
                    let dx = self.fx_position[0] - view.origin[0];
                    let dz = self.fx_position[2] - view.origin[2];
                    let l2 = dy * dy + dx * dx + dz * dz;
                    let dist = inv_sqrt(l2) * l2;
                    if self.fade_end < dist {
                        return;
                    }
                    let f = if dist <= self.fade_start { 1.0 } else { clamp01(1.0 - (dist - self.fade_start) / (self.fade_end - self.fade_start)) };
                    s *= f;
                }
                let sc = s * self.player_max_shake_scale;
                let r = rng.next01();
                let pitch = ((r + r) - 1.0) * self.max_shake_angles[0] * sc;
                let r = rng.next01();
                let yaw = ((r + r) - 1.0) * self.max_shake_angles[1] * sc;
                let r = rng.next01();
                let roll = ((r + r) - 1.0) * self.max_shake_angles[2] * sc;
                let mut o = [0.0f32; 3];
                for (k, ok) in o.iter_mut().enumerate() {
                    let r = rng.next01();
                    *ok = ((r + r) - 1.0) * self.max_shake_offset[k] * sc;
                }
                self.last_offset = o;
                self.last_angles = angles_to_mat3([pitch, yaw, roll]);
                rows_times(&mut view.axis, &self.last_angles);
                for (v, d) in view.origin.iter_mut().zip(o) {
                    *v += d;
                }
            }
            Some(d) => {
                if !d.infos.is_empty() {
                    let total: f32 = d.infos.iter().fold(0.0, |a, i| a + i.blending);
                    // Angle and offset accumulators persist across entries (modes that do not set them re-apply
                    // the previous entry's values), as in the exe.
                    let mut ang = [0.0f32; 3];
                    let (mut ox, mut oy, mut oz) = (0.0f32, 0.0f32, 0.0f32);
                    for info in &d.infos {
                        if info.blending == 0.0 {
                            continue;
                        }
                        let mut w = info.blending / total;
                        if d.fade_intensity {
                            w *= self.normalized_magnitude;
                        }
                        let amp = s * d.player_max_shake_scale * w;
                        let (mut lo, mut hi) = (-1.0f32, 1.0f32);
                        if info.positive {
                            lo = 0.0;
                        } else if info.negative {
                            hi = 0.0;
                        }
                        let span = hi - lo;
                        let ma = d.max_shake_angles;
                        let mo = d.max_shake_offset;
                        let pms = d.player_max_shake_scale;
                        let axis_move = |row: [f32; 3], f: f32| {
                            (f * mo[0] * s * pms * row[0] * w, f * mo[1] * s * pms * row[1] * w, f * mo[2] * s * pms * row[2] * w)
                        };
                        match info.mode {
                            0 => {
                                ang[0] = rnd(rng, lo, span) * ma[0] * amp;
                                ang[1] = rnd(rng, lo, span) * ma[1] * amp;
                                ang[2] = rnd(rng, lo, span) * ma[2] * amp;
                                ox = rnd(rng, lo, span) * mo[0] * amp;
                                oy = rnd(rng, lo, span) * mo[1] * amp;
                                oz = rnd(rng, lo, span) * mo[2] * amp;
                            }
                            1 => {
                                ang[0] = rnd(rng, lo, span) * ma[0] * amp;
                                ang[1] = rnd(rng, lo, span) * ma[1] * amp;
                                ang[2] = rnd(rng, lo, span) * ma[2] * amp;
                            }
                            2 => {
                                ox = rnd(rng, lo, span) * mo[0] * amp;
                                oy = rnd(rng, lo, span) * mo[1] * amp;
                                oz = rnd(rng, lo, span) * mo[2] * amp;
                            }
                            3 => ang[2] = rnd(rng, lo, span) * ma[2] * amp,
                            4 => ang[0] = rnd(rng, lo, span) * ma[0] * amp,
                            5 => ang[1] = rnd(rng, lo, span) * ma[1] * amp,
                            6..=8 => {
                                let f = rnd(rng, lo, span);
                                (ox, oy, oz) = axis_move(view.axis[(info.mode - 6) as usize], f);
                            }
                            9..=11 => {
                                let v = if hi != 0.0 { hi } else { lo };
                                let k = [2usize, 0, 1][(info.mode - 9) as usize];
                                ang[k] = v * ma[k] * amp;
                            }
                            12..=14 => {
                                let v = if hi == 0.0 { lo } else { hi };
                                (ox, oy, oz) = axis_move(view.axis[(info.mode - 12) as usize], v);
                            }
                            _ => {}
                        }
                        self.last_offset = [ox, oy, oz];
                        self.last_angles = angles_to_mat3(ang);
                        rows_times(&mut view.axis, &self.last_angles);
                        view.origin[0] += ox;
                        view.origin[1] += oy;
                        view.origin[2] += oz;
                    }
                }
            }
        }
        self.last_applied = s;
    }

    /// 0x140e68520 for slot `i`.
    pub fn apply_advanced(&mut self, i: usize, view: &mut View, now: i32, rng: &mut GameRng) {
        let slot = &mut self.advanced[i];
        let Some(d) = slot.decl.clone() else { return };
        if slot.end_time <= now {
            slot.decl = None;
            return;
        }
        const EPS: f32 = 1.192_092_9e-7;
        const TAU: f32 = 6.283_185_5;
        let t = (now - slot.start_time) as f32 / (slot.end_time - slot.start_time) as f32;
        let spt = 1.0 / TICKS_PER_SEC as f32;
        let phase = (now as f32 * spt - slot.start_time as f32 * spt) * TAU;
        let sample = |rng: &mut GameRng, rand: f32, freq: f32, off: f32| {
            let lo = 1.0 - rand;
            let hi = rand + 1.0;
            crt_cosf((rng.next01() * (hi - lo) + lo) * (phase * freq) + off)
        };
        let mut rs = [0.0f32; 3];
        for (k, v) in rs.iter_mut().enumerate() {
            if EPS < d.normalized_rotation_scale[k] {
                *v = sample(rng, d.rotation.max_sample_randomization, slot.rotation_frequencies[k], slot.axis_offsets[k]);
            }
        }
        let mut ts = [0.0f32; 3];
        for (k, v) in ts.iter_mut().enumerate() {
            if EPS < d.translation.scale[k] {
                *v = sample(rng, d.translation.max_sample_randomization, slot.translation_frequencies[k], slot.axis_offsets[k]);
            }
        }
        let m = slot.normalized_magnitude;
        let mut rot = [0, 1, 2].map(|k| rs[k] * d.normalized_rotation_scale[k] * m);
        let mut tr = [0, 1, 2].map(|k| ts[k] * d.translation.scale[k] * m);
        if let Some(tab) = &d.rotation.amplitude_table {
            let a = tab.curve(t);
            rot = rot.map(|v| v * a);
        }
        if let Some(tab) = &d.translation.amplitude_table {
            let a = tab.curve(t);
            tr = tr.map(|v| v * a);
        }
        let lo = 1.0 - d.translation.max_amplitude_randomization;
        let f = rng.next01() * (1.0 - lo) + lo;
        tr = tr.map(|v| v * f);
        let a = view.axis;
        view.origin[0] += tr[1] * a[1][0] + tr[0] * a[0][0] + tr[2] * a[2][0];
        view.origin[2] += tr[1] * a[1][2] + tr[0] * a[0][2] + tr[2] * a[2][2];
        view.origin[1] += tr[1] * a[1][1] + tr[0] * a[0][1] + tr[2] * a[2][1];
        let lo = 1.0 - d.rotation.max_amplitude_randomization;
        let span = 1.0 - lo;
        let r_roll = rng.next01();
        let r_yaw = rng.next01();
        let r_pitch = rng.next01();
        let mm = radians_to_mat3((r_pitch * span + lo) * (rot[1] * TAU), rot[2] * TAU * (r_yaw * span + lo), rot[0] * TAU * (r_roll * span + lo));
        // axis = M * axis.
        let mut out = [[0.0f32; 3]; 3];
        for (r, o) in out.iter_mut().enumerate() {
            for c in 0..3 {
                o[c] = mm[r][0] * a[0][c] + a[1][c] * mm[r][1] + a[2][c] * mm[r][2];
            }
        }
        view.axis = out;
    }
}

/// idPlayer 0x140e3acb0: the magnitude an advanced shake starts with at `distance` from its source
/// (projectileImpactEffect viewShakeStartDistance / viewShakeDistance, e.g. gauss 150 / 600).
pub fn advanced_distance_magnitude(distance: f32, start: f32, end: f32) -> f32 {
    if start < distance {
        let span = end - start;
        let span = if span <= 0.001 { 0.001 } else { span };
        clamp01(1.0 - (distance - start) / span)
    } else {
        1.0
    }
}

/// The double-jump view pitch change (idPlayer::UpdateViewAngles 0x140e3f070): added to the view pitch (positive =
/// down), replacing the previous frame's value. `last_double_jump` is playerPState lastDJTime (physics +0x1e94; -1 =
/// never, so pass None before the first double jump). change / duration: the jump boots decl when owned
/// (jumpboots/base: 0.5 degrees, 1000 ticks), else pm_doubleJumpViewPitchChange 10 / Duration 1000.
pub fn double_jump_pitch(last_double_jump: Option<i32>, now: i32, change: f32, duration: i32) -> f32 {
    let Some(t0) = last_double_jump.filter(|t| *t >= 0) else { return 0.0 };
    if t0 > now || t0 + duration < now {
        return 0.0;
    }
    let mut x = (((now - t0) as f32 * 0.001) / (duration as f32 * 0.001)) * std::f32::consts::PI;
    if x < 0.0 || 6.283_185_5 <= x {
        x -= (x * 0.159_154_94).floor() * 6.283_185_5;
    }
    x.sin() * change
}

/// p_applyAnimatedCamera (0x140e3beb0, with hands_updatePos): the hands rig's `camera` joint moves the eye. `cam_t` /
/// `cam_r`: that joint's transform in hands-model space INCLUDING the fp_hands md6Def offset (-12.6 0 -83.5), i.e.
/// zero / identity in the bind pose, from the last built frame's final hands pose (all layers; FUN_1416fc700 on
/// +0x2f0). origin += t along the view axis rows; if R has nonzero angles, the view angles become ToAngles(axis) +
/// ToAngles(R) with pitch clamped to +-89. (Not oracle-checked.)
pub fn apply_animated_camera(view: &mut View, cam_t: [f32; 3], cam_r: &IdMat3) {
    let a = view.axis;
    let t = cam_t;
    view.origin = [
        t[1] * a[1][0] + t[0] * a[0][0] + t[2] * a[2][0] + view.origin[0],
        t[0] * a[0][1] + t[1] * a[1][1] + t[2] * a[2][1] + view.origin[1],
        t[0] * a[0][2] + t[1] * a[1][2] + t[2] * a[2][2] + view.origin[2],
    ];
    let ra = mat3_to_angles(cam_r);
    if 0.0 < ra[0] * ra[0] + ra[1] * ra[1] + ra[2] * ra[2] {
        let va = mat3_to_angles(&view.axis);
        let p = (va[0] + ra[0]).clamp(-89.0, 89.0);
        view.axis = angles_to_mat3([p, va[1] + ra[1], va[2] + ra[2]]);
    }
}

/// idView view kick (+0xdf0 kickFinishTime, +0xdf4 kickTime, +0xdf8 kickAngles; set by the damage impulse
/// 0x140e6c1f0, see VIEWFX.md). Evaluation 0x140e6d380 with view_mpViewKick 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewKick {
    pub finish_time: i32,
    pub kick_time: f32,
    pub angles: [f32; 3],
}

impl Default for ViewKick {
    fn default() -> Self {
        Self { finish_time: -1, kick_time: 0.0, angles: [0.0; 3] }
    }
}

impl ViewKick {
    /// The angles added to the view (ToAngles(axis) + kick -> ToMat3): (finish - now)^2 * kickAngles, each clamped
    /// to +-70 degrees; zero once finished.
    pub fn angles(&self, now: i32) -> [f32; 3] {
        if self.finish_time <= now {
            return [0.0; 3];
        }
        let d = self.finish_time - now;
        let f = (d * d) as f32;
        self.angles.map(|a| (f * a).clamp(-70.0, 70.0))
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop)]
mod tests {
    use super::*;

    #[test]
    fn to_angles_round_trips() {
        let a = [12.0f32, -40.0, 7.5];
        let m = angles_to_mat3(a);
        let b = mat3_to_angles(&m);
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() < 1e-3, "{a:?} {b:?}");
        }
        let r = radians_to_mat3(a[0].to_radians(), a[1].to_radians(), a[2].to_radians());
        for i in 0..3 {
            for j in 0..3 {
                assert!((r[i][j] - m[i][j]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn double_jump_pitch_curve() {
        assert_eq!(double_jump_pitch(None, 500, 0.5, 1000), 0.0);
        assert!((double_jump_pitch(Some(1000), 1500, 0.5, 1000) - 0.5).abs() < 1e-6);
        assert_eq!(double_jump_pitch(Some(1000), 2001, 0.5, 1000), 0.0);
        assert!(double_jump_pitch(Some(1000), 1100, 0.5, 1000) > 0.0);
    }

    #[allow(clippy::large_const_arrays)]
    mod golden {
        include!("viewfx_golden.in");
    }

    const ORIGIN: [f32; 3] = [100.0, 200.0, 50.0];

    fn base_view() -> View {
        let a = golden::AXIS.map(f32::from_bits);
        View { origin: ORIGIN, axis: [[a[0], a[1], a[2]], [a[3], a[4], a[5]], [a[6], a[7], a[8]]] }
    }

    fn frame_bits(v: &View, rng: &GameRng) -> [u32; 13] {
        let mut o = [0u32; 13];
        for k in 0..3 {
            o[k] = v.origin[k].to_bits();
        }
        for r in 0..3 {
            for c in 0..3 {
                o[3 + r * 3 + c] = v.axis[r][c].to_bits();
            }
        }
        o[12] = rng.0;
        o
    }

    fn view_decl(max_scale: f32, angles: [f32; 3], offset: [f32; 3], power: f32, duration: i32, method: ShakeMethod, infos: &[(f32, u32, bool, bool)], fade: bool) -> ViewShakeDecl {
        ViewShakeDecl {
            player_max_shake_scale: max_scale,
            max_shake_angles: angles,
            max_shake_offset: offset,
            shake_scale_power: power,
            shake_duration: duration,
            method,
            infos: infos.iter().map(|&(blending, mode, positive, negative)| ShakeViewInfo { blending, mode, positive, negative }).collect(),
            fade_intensity: fade,
            shake_percentage: power / 100.0,
        }
    }

    fn run_view(decl: &ViewShakeDecl, mag: f32, want: &[[u32; 13]], name: &str) {
        let mut fx = ViewFx::default();
        let mut rng = GameRng(0x12345678);
        let mut now = 1000;
        assert!(fx.start_view_shake(decl, mag, now));
        for (f, w) in want.iter().enumerate() {
            now += 16;
            let mut v = base_view();
            fx.apply_shakes(&mut v, now, &mut rng);
            assert_eq!(frame_bits(&v, &rng), *w, "{name} frame {f}");
        }
    }

    #[test]
    fn view_shakes_match_engine() {
        use ShakeMethod::*;
        let all: Vec<(f32, u32, bool, bool)> = (0..16).map(|k| ((0.5 + 0.1 * k as f64) as f32, k, k % 3 == 1, k % 3 == 2)).collect();
        let cases: [(&str, ViewShakeDecl, f32, &[[u32; 13]]); 5] = [
            ("melee", view_decl(1.0, [25.0; 3], [5.0; 3], 10.0, 100, Constant, &[(1.0, 0, false, true)], true), 1.0, &golden::MELEE),
            ("meleeleft", view_decl(1.0, [10.0; 3], [5.0; 3], 2.5, 200, LinearEaseInOut, &[(1.0, 6, true, false), (1.0, 13, true, false)], true), 1.0, &golden::MELEELEFT),
            ("meleeright", view_decl(1.0, [10.0; 3], [5.0; 3], 2.5, 200, LinearEaseInOut, &[(0.5, 6, true, false), (1.0, 13, false, true)], true), 1.0, &golden::MELEERIGHT),
            ("allmodes_exp", view_decl(0.8, [12.0, 7.0, 3.0], [4.0, 2.0, 6.0], 30.0, 300, ExponentialDecay, &all, true), 0.7, &golden::ALLMODES_EXP),
            ("linear_nofade", view_decl(1.0, [3.0, 4.0, 5.0], [1.0, 2.0, 3.0], 50.0, 150, LinearDecay, &[(1.0, 1, false, false), (2.0, 12, false, true), (0.0, 0, false, false), (1.0, 7, false, false)], false), 0.4, &golden::LINEAR_NOFADE),
        ];
        for (name, d, mag, want) in &cases {
            run_view(d, *mag, want, name);
        }
    }

    #[test]
    fn fx_camera_shakes_match_engine() {
        let cases: [(&str, f32, f32, [f32; 3], f32, f32, &[[u32; 13]]); 4] = [
            ("fx_near", 0.6, 0.1, [150.0, 230.0, 60.0], 128.0, 512.0, &golden::FX_NEAR),
            ("fx_mid", 0.9, 0.0, [400.0, 200.0, 50.0], 128.0, 512.0, &golden::FX_MID),
            ("fx_far", 0.9, 0.0, [900.0, 200.0, 50.0], 128.0, 512.0, &golden::FX_FAR),
            ("fx_nofade", 1.0, 0.0, [0.0; 3], 0.0, 0.0, &golden::FX_NOFADE),
        ];
        for (name, cs, vol, pos, fs, fe, want) in cases {
            let mut fx = ViewFx::default();
            fx.set_camera_shake(cs, pos, fs, fe);
            fx.shake_volume = vol;
            let mut rng = GameRng(0x2468ace0);
            let mut now = 5000;
            for (f, w) in want.iter().enumerate() {
                now += 16;
                let mut v = base_view();
                fx.apply_shakes(&mut v, now, &mut rng);
                assert_eq!(frame_bits(&v, &rng), *w, "{name} frame {f}");
            }
        }
    }

    #[test]
    fn advanced_shakes_match_engine() {
        let decl = |ms: i32, rot: ([f32; 3], f32, f32, f32, f32), tr: ([f32; 3], f32, f32, f32, f32)| {
            let data = |p: ([f32; 3], f32, f32, f32, f32)| AdvancedShakeData {
                amplitude_table: None,
                scale: p.0,
                min_frequency_hz: p.1,
                max_frequency_hz: p.2,
                max_sample_randomization: p.3,
                max_amplitude_randomization: p.4,
            };
            let c = 0.002_777_777_8f32;
            std::sync::Arc::new(AdvancedViewShakeDecl {
                shake_time_ms: ms,
                normalized_rotation_scale: rot.0.map(|s| s * c),
                rotation: data(rot),
                translation: data(tr),
            })
        };
        let d0 = decl(250, ([0.0, 2.0, 2.0], 12.0, 12.0, 0.0, 0.0), ([0.0; 3], 40.0, 40.0, 0.0, 0.0));
        let d1 = decl(400, ([1.5, 2.5, 0.5], 16.0, 10.0, 0.1, 0.75), ([1.0, 2.0, 3.0], 50.0, 70.0, 0.2, 0.3));
        let mut fx = ViewFx::default();
        let mut rng = GameRng(0x0badf00d);
        let mut now = 2000;
        assert!(fx.start_advanced_shake(d0, 1.0, now, &mut rng));
        now += 16;
        assert!(fx.start_advanced_shake(d1, 0.8, now, &mut rng));
        for (f, w) in golden::ADVANCED.iter().enumerate() {
            now += 16;
            let mut v = base_view();
            fx.apply_shakes(&mut v, now, &mut rng);
            assert_eq!(frame_bits(&v, &rng), *w, "advanced frame {f}");
        }
    }

    /// The campaign's shake decls load: the fists melee camera shakes and the SP advanced shakes (incl. a spline
    /// amplitude table).
    #[test]
    fn install_shake_decls() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("install");
        let left = ViewShakeDecl::from_decl(&inst.decls, "screenviewshake/meleeleft").unwrap();
        assert_eq!((left.shake_duration, left.method, left.max_shake_angles, left.max_shake_offset), (200, ShakeMethod::LinearEaseInOut, [10.0; 3], [5.0; 3]));
        assert_eq!(left.infos, vec![ShakeViewInfo { blending: 1.0, mode: 6, positive: true, negative: false }, ShakeViewInfo { blending: 1.0, mode: 13, positive: true, negative: false }]);
        let right = ViewShakeDecl::from_decl(&inst.decls, "screenviewshake/meleeright").unwrap();
        assert_eq!((right.infos[0].blending, right.infos[1].negative), (0.5, true));
        let melee = ViewShakeDecl::from_decl(&inst.decls, "screenviewshake/melee").unwrap();
        assert_eq!((melee.shake_duration, melee.shake_scale_power, melee.max_shake_angles), (100, 10.0, [25.0; 3]));
        let gauss = AdvancedViewShakeDecl::from_decl(&inst.decls, "screenviewshake/sp/gauss_rifle").unwrap();
        assert_eq!((gauss.shake_time_ms, gauss.rotation.scale, gauss.rotation.min_frequency_hz), (250, [0.0, 2.0, 2.0], 12.0));
        assert!(gauss.rotation.amplitude_table.is_some() && gauss.translation.amplitude_table.is_some());
        for n in ["sp/generic_explosion", "sp/generic_weak_shake", "sp/gibs/berserk", "sp/gibs/full_body_gibs", "sp/gibs/half_body_gibs", "sp/missions/polar_core/vega_random_shake"] {
            AdvancedViewShakeDecl::from_decl(&inst.decls, &format!("screenviewshake/{n}")).unwrap_or_else(|e| panic!("{n}: {e:#}"));
        }
        // A melee shake on the install's decl: ease in and out over 200 ticks, then it ends.
        let mut fx = ViewFx::default();
        let mut rng = GameRng(1);
        assert!(fx.start_view_shake(&left, 1.0, 0));
        let mut moved = 0;
        for t in (16..=240).step_by(16) {
            let mut v = View { origin: [0.0; 3], axis: IDENTITY };
            fx.apply_shakes(&mut v, t, &mut rng);
            if v.origin != [0.0; 3] {
                moved += 1;
            }
        }
        assert!(moved >= 10 && fx.decl.is_none(), "moved {moved}");
    }

    #[test]
    fn advanced_magnitude_by_distance() {
        assert_eq!(advanced_distance_magnitude(100.0, 150.0, 600.0), 1.0);
        assert!((advanced_distance_magnitude(375.0, 150.0, 600.0) - 0.5).abs() < 1e-6);
        assert_eq!(advanced_distance_magnitude(700.0, 150.0, 600.0), 0.0);
    }
}
