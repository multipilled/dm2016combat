//! Weapon spread: the player's per-fire-mode spread state and shot directions, decoded from DOOMx64.exe.

use glam::Vec3;

use super::interp::Interpolate;
use super::GameRng;

/// `idDeclWeapon::spreadParams_t` with the decl class defaults (ctor 0x1406f0780).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpreadParams {
    pub spread: f32,
    pub base_zoom: f32,
    pub base_crouch: f32,
    pub base_zoom_crouch: f32,
    pub increased_by_movement: f32,
    pub increased_by_movement_zoom: f32,
    pub increased_by_aiming: f32,
    pub addition_per_shot: f32,
    pub addition_max: f32,
    pub addition_per_shot_zoom: f32,
    pub addition_per_shot_moving: f32,
    pub addition_per_shot_zoom_moving: f32,
    pub addition_max_zoom: f32,
    pub return_delay: f32,
    pub return_time: f32,
    pub band_strength: i32,
    pub horizontal_scale: f32,
    pub horizontal_even_spacing_lerp: f32,
    pub vertical_scale: f32,
    pub vertical_even_spacing_lerp: f32,
}

impl Default for SpreadParams {
    fn default() -> Self {
        Self {
            spread: 0.0,
            base_zoom: 0.0,
            base_crouch: 0.0,
            base_zoom_crouch: 0.0,
            increased_by_movement: 1.0,
            increased_by_movement_zoom: 0.5,
            increased_by_aiming: 10.0,
            addition_per_shot: 0.25,
            addition_max: 3.0,
            addition_per_shot_zoom: 0.125,
            addition_per_shot_moving: 0.25,
            addition_per_shot_zoom_moving: 0.25,
            addition_max_zoom: 1.5,
            return_delay: 100.0,
            return_time: 250.0,
            band_strength: 0,
            horizontal_scale: 1.0,
            horizontal_even_spacing_lerp: 0.0,
            vertical_scale: 1.0,
            vertical_even_spacing_lerp: 0.0,
        }
    }
}

impl SpreadParams {
    /// idWeapon::GetBaseSpread 0x140f14c50 (no per-mode overrides without mods).
    pub fn base(&self, zoomed: bool, crouched: bool) -> f32 {
        match (zoomed, crouched) {
            (false, false) => self.spread,
            (false, true) => self.base_crouch,
            (true, false) => self.base_zoom,
            (true, true) => self.base_zoom_crouch,
        }
    }

    fn add_params(&self, zoomed: bool) -> (f32, f32, f32) {
        if zoomed {
            (self.addition_per_shot_zoom, self.addition_per_shot_zoom_moving, self.addition_max_zoom)
        } else {
            (self.addition_per_shot, self.addition_per_shot_moving, self.addition_max)
        }
    }
}

/// Player state the spread code reads.
#[derive(Debug, Clone, Copy)]
pub struct SpreadInput {
    /// |velocity| of the player physics.
    pub speed: f32,
    /// idPlayer+0x55c74, set from pm_runspeed.
    pub run_speed: f32,
    pub crouched: bool,
    pub zoomed: bool,
    pub view_forward: Vec3,
}

impl Default for SpreadInput {
    fn default() -> Self {
        Self { speed: 0.0, run_speed: 500.0, crouched: false, zoomed: false, view_forward: Vec3::X }
    }
}

/// Duration of the per-shot spread addition blend (0x142fa64f0).
pub const SPREAD_ADD_BLEND_MS: i32 = 67;

/// idPlayer spread for fire mode 0: base and per-shot-addition interpolators (player+0xcd84 / +0xcda0).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayerSpread {
    pub base: Interpolate,
    pub add: Interpolate,
    last_forward: Option<Vec3>,
    last_zoomed: bool,
}

impl PlayerSpread {
    fn move_fraction(inp: &SpreadInput) -> f32 {
        let f = inp.speed / inp.run_speed;
        if f <= 1.0 { f } else { 1.0 }
    }

    /// One mode of idPlayer::UpdateSpread 0x140e3f590 (called every frame before the hands fire).
    pub fn update(&mut self, now: i32, p: &SpreadParams, inp: &SpreadInput) {
        let mut base = p.base(inp.zoomed, inp.crouched);
        if 0.0 < base {
            let by_move = if inp.zoomed { p.increased_by_movement_zoom } else { p.increased_by_movement };
            let fwd = inp.view_forward;
            let last = self.last_forward.unwrap_or(fwd);
            let d = fwd.y * last.y + fwd.x * last.x + fwd.z * last.z;
            let aim = if -1.0 < d { if d < 1.0 { d.acos() } else { 0.0 } } else { std::f32::consts::PI };
            self.last_forward = Some(fwd);
            base = base + Self::move_fraction(inp) * by_move + aim * p.increased_by_aiming;
        }
        let prev = self.base.value(now);
        self.base.init(now, 0, prev, base);
        if self.add.end_time() <= now {
            let start = p.return_delay as i32 + now;
            let cur = self.add.value(now);
            self.add.init(start, p.return_time as i32, cur, 0.0);
        }
        self.last_zoomed = inp.zoomed;
    }

    /// idPlayer::GetSpread 0x140e40df0 (g_weaponSpreadScale 1, g_spread_noSpread 0).
    pub fn value(&mut self, now: i32) -> f32 {
        1.0 * (self.base.value(now) + self.add.value(now))
    }

    /// Spread addition of idPlayer::WeaponFired 0x140e47b80.
    pub fn on_fire(&mut self, now: i32, p: &SpreadParams, inp: &SpreadInput) {
        let mv = Self::move_fraction(inp);
        let (per_shot, per_shot_moving, max) = p.add_params(inp.zoomed);
        let per_shot = if per_shot.abs() <= 1e-18 { 0.0 } else { per_shot };
        let per_shot_moving = if per_shot_moving.abs() <= 1e-18 { 0.0 } else { per_shot_moving };
        let cur = self.add.value(now);
        let mut target = cur + (1.0 - mv) * per_shot + per_shot_moving * mv;
        if target > max {
            target = max;
        }
        self.add.init(now, SPREAD_ADD_BLEND_MS, cur, target);
        self.last_zoomed = inp.zoomed;
    }
}

/// Approximate gaussian of the game RNG (0x140430790): mean of `n` uniform values in [-1, 1].
pub fn rand_gaussian(rng: &mut GameRng, n: i32) -> f32 {
    let mut sum = 0.0f32;
    for _ in 0..n {
        let r = rng.next01();
        sum += (r + r) - 1.0;
    }
    sum / n as f32
}

/// Spread pattern inputs from the projectile decl.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpreadPattern {
    pub count: i32,
    pub fixed_rings: bool,
    pub num_circles: i32,
    pub dartboard: bool,
}

/// Direction of trace `shot` of `count` (0x140ee3ef0, damage traces, no aim magnetism).
/// `fwd`/`left`/`up` are the fire axis rows (idAngles::ToMat3: forward, left, up). `spread_deg` is the
/// player's GetSpread value; `gaussian` is g_useGaussianAimSpread (default 1).
#[allow(clippy::too_many_arguments)]
pub fn shot_direction(rng: &mut GameRng, fwd: Vec3, left: Vec3, up: Vec3, spread_deg: f32, p: &SpreadParams, pat: &SpreadPattern, shot: i32, gaussian: bool) -> Vec3 {
    let spread = spread_deg * p_scale() * 0.017_453_292;
    let count = pat.count;
    if count >= 2 && pat.fixed_rings {
        // Quake 4 style fixed rings.
        let rings = if count < 11 { 2 } else { 3 };
        let counts = [4, if count < 11 { count - 4 } else { 5 }, count - 9];
        let (ring, idx) = if shot < 4 {
            (0usize, shot)
        } else if shot < counts[1] + 4 {
            (1, shot - 4)
        } else {
            (2, shot - counts[1] - 4)
        };
        let step = 360.0 / counts[ring] as f32;
        let off = if ring & 1 == 1 { step * 0.5 } else { 0.0 };
        let r = (ring as i32 + 1) as f32 * (1.0 / rings as f32) * spread;
        let sr = r.sin();
        let th = (idx as f32 * step + off) * 0.017_453_292;
        let h = th.cos() * sr * p.horizontal_scale;
        let v = th.sin() * sr * p.vertical_scale;
        return (fwd + up * v - left * h).normalize();
    }
    if count >= 2 && (p.horizontal_scale != 1.0 || p.vertical_scale != 1.0) {
        // Even-spacing lerp path (rotations about up, then left); unused by the default SP arsenal.
        let t = (shot as f32 + shot as f32) / (count - 1) as f32 - 1.0;
        let g = { let r = rng.next01(); (r + r) - 1.0 };
        let yaw = spread_deg * ((1.0 - p.horizontal_even_spacing_lerp) * g + p.horizontal_even_spacing_lerp * t) * p.horizontal_scale * 0.017_453_292;
        let d = rotate(fwd, up, yaw);
        let g2 = { let r = rng.next01(); (r + r) - 1.0 };
        let pitch = spread_deg * ((1.0 - p.vertical_even_spacing_lerp) * g2 + p.vertical_even_spacing_lerp * t) * p.vertical_scale * 0.017_453_292;
        return rotate(d, left, pitch);
    }
    // Single trace (also each trace of multi-trace weapons without rings/circles, e.g. the chaingun).
    if gaussian {
        let s = spread.sin();
        let h = rand_gaussian(rng, 3) * s * p.horizontal_scale;
        let s2 = spread.sin();
        let v = rand_gaussian(rng, 3) * s2 * p.vertical_scale;
        (fwd + up * v - left * h).normalize()
    } else {
        let r = rng.next01() * spread;
        let sr = r.sin();
        let th = rng.next01() * std::f32::consts::TAU;
        let h = th.cos() * sr;
        let v = th.sin() * sr;
        (fwd + up * v - left * h).normalize()
    }
}

/// fp.spreadScale(+0x204): 1 unless a charge/mod changes it.
fn p_scale() -> f32 {
    1.0
}

fn rotate(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    glam::Quat::from_axis_angle(axis.normalize(), angle) * v
}
