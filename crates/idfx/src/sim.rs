//! The engine's CPU particle path (idRenderModelParticle stage generation), producing world-space quads.
//!
//! - per-stage spawn timing, cycles, seeds: FUN_1419a56d0
//! - one particle (colour, fade, culling, orientation dispatch): FUN_1419a3fb0
//! - origin and velocity: FUN_1419ac350
//! - fade: 0x1419a38c0; texcoords/flip/rotate: FUN_1419ae320; frame animation: FUN_1419a3bc0
//! - quads: view/axis FUN_1419a8060, aimed FUN_1419a7690
//!
//! The engine works in emitter-local space with a context holding the emitter axis and the view
//! vectors expressed locally; here the same products are formed directly in world space.

use std::sync::Arc;

use glam::{Vec3, Vec4};

use crate::particle::{ParticleDecl, Stage};
use crate::table::Table;
use crate::{Axis, DEG2RAD, IdRandom, PI, RANDOM_SCALE, TWO_PI, normalize, sincos};

/// r_particlesMinAlpha (16): particles at or below this opacity are not emitted.
pub const MIN_ALPHA: f32 = 16.0;

#[derive(Debug, Clone, Copy)]
pub struct View {
    pub origin: Vec3,
    /// The billboard's first axis (the context's view vector at +0x90): screen right.
    pub right: Vec3,
    /// The second axis (+0x9c): screen up.
    pub up: Vec3,
}

/// Inputs the render model receives each frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// Render time (ms) and the previous frame's.
    pub time_ms: i32,
    pub prev_time_ms: i32,
    pub origin: Vec3,
    pub axis: Axis,
    pub view: View,
    /// Entity colour (shader parms 0..3).
    pub entity_color: Vec4,
    /// System fade (applied to RGB for additive stages, alpha for blended ones).
    pub fade: f32,
    /// FX size scale (applied to particle size and distribution size).
    pub size_scale: f32,
    pub wind: Vec3,
    /// Global shadow sample (lighting-dependent; 1 = fully lit).
    pub shadow: f32,
    /// Emitter velocity (world), for orientToWorldVel.
    pub velocity: Vec3,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Vertex {
    pub pos: Vec3,
    pub uv: [f32; 2],
    /// Next animation frame's texcoords (cross-fade).
    pub uv_next: [f32; 2],
    /// Vertex colour bytes (sRGB, DeGamma'd in the shader).
    pub color: [u8; 4],
    /// genericParm[0..3] bytes (vertex.normal in transsortblend).
    pub generic: [u8; 4],
    /// Cross-fade fraction (vertex.tangent.z).
    pub frame_frac: f32,
    /// Soft particle alpha scale (vertex.tangent.w).
    pub alpha_scale: f32,
    /// GPU stages: the linear (possibly overbright) colour gpuparticlerender outputs; `color` is unused then.
    pub color_f: Option<[f32; 4]>,
}

#[derive(Debug, Clone, Copy)]
pub struct Quad {
    pub stage: usize,
    pub verts: [Vertex; 4],
}

/// Per-particle state handed to every attribute evaluation (the engine's 0x1c-byte particleGen).
#[derive(Debug, Clone, Copy)]
struct Gen {
    index: i32,
    age: f32,
    life: f32,
    frac: f32,
    frac2: f32,
    seed: u32,
}

#[derive(Debug, Clone)]
struct StageState {
    /// Spawn time (ms since stage start) of each particle's current cycle; -1 = never spawned.
    spawn: Vec<i32>,
    /// GPU stages: the emitter slot (None = not allocated, before the first spawn or after the last cycle).
    gpu: Option<GpuEmitter>,
}

/// A GPU emitter's particle buffer ($particles) and the state the manager keeps per emitter.
#[derive(Debug, Clone)]
struct GpuEmitter {
    /// Last $gpuParticleEmitParms.x; a smaller value resets every particle (FUN_1419a36e0 sets +0x169).
    elapsed: f32,
    reset: bool,
    particles: Vec<GpuParticle>,
}

#[derive(Debug, Clone, Copy, Default)]
struct GpuParticle {
    cycle: i32,
    pos: Vec3,
    vel: Vec3,
    max_life: f32,
    life: f32,
}

/// One running particle system (an idRenderModelParticle instance).
#[derive(Debug, Clone)]
pub struct System {
    pub decl: Arc<ParticleDecl>,
    /// Start time in seconds (the start-time render parm).
    pub start: f32,
    /// Particles spawned at or after this time (ms) are not drawn; 0 = running.
    pub stop_ms: i32,
    /// Random base (diversity).
    pub seed: u32,
    stages: Vec<StageState>,
}

impl System {
    pub fn new(decl: Arc<ParticleDecl>, start_ms: i32, seed: u32) -> System {
        let stages = decl.stages.iter().map(|s| StageState { spawn: vec![-1; s.total_particles.max(s.lod_total).max(0) as usize], gpu: None }).collect();
        System { decl, start: start_ms as f32 * 0.001, stop_ms: 0, seed, stages }
    }

    /// True once every stage's particles are past their life (or the system was stopped and drained).
    pub fn finished(&self, time_ms: i32) -> bool {
        let elapsed = time_ms as f32 * 0.001 - self.start;
        self.decl.stages.iter().all(|s| {
            if s.gpu && !s.hidden && s.cycle_msec != 0 {
                let g = &s.gpu_stage;
                if g.total <= 0 {
                    return true;
                }
                if self.stop_ms != 0 {
                    return time_ms as f32 * 0.001 > self.stop_ms as f32 * 0.001 + s.max_particle_life;
                }
                // The emitter is freed once elapsed / period passes the cycle count (FUN_1415e9600).
                let elapsed_ms = ((time_ms as f32 * 0.001 - (self.start + g.time_offset)) * 1000.0) as i32;
                return g.cycles < 0 || (g.cycles != 0 && elapsed_ms / s.cycle_msec > g.cycles);
            }
            // Stages this path does not draw (hidden, lights) never hold a system alive.
            if s.hidden || s.cycle_msec == 0 || s.is_light || s.gpu || s.total_particles <= 0 {
                return true;
            }
            // Last spawn of the last cycle plus a full life.
            let last = s.time_offset + s.spawn_bunching * s.bunch_time + s.max_particle_life;
            if self.stop_ms != 0 {
                return time_ms as f32 * 0.001 > self.stop_ms as f32 * 0.001 + s.max_particle_life;
            }
            s.cycles != 0 && elapsed > (s.cycles as f32 - 1.0) * s.cycle_msec as f32 * 0.001 + last
        })
    }

    pub fn generate(&mut self, f: &Frame, out: &mut Vec<Quad>) {
        let decl = self.decl.clone();
        for (si, stage) in decl.stages.iter().enumerate() {
            if stage.hidden || stage.cycle_msec == 0 || stage.is_light {
                continue;
            }
            if stage.gpu {
                gpu_stage(self, si, stage, f, out);
                continue;
            }
            cpu_stage(self, si, stage, &decl.tables, f, &mut |g, count| particle(si, stage, &decl.tables, count, g, f, out));
        }
    }
}

/// One particle light (isLight stages, FUN_1419a47a0).
#[derive(Debug, Clone, Copy)]
pub struct ParticleLight {
    pub stage: usize,
    pub origin: Vec3,
    /// baseColor x entity colour (shader parms).
    pub color: Vec4,
    /// size[0].
    pub radius: f32,
    /// fade x brightness (x the light distance fade, INTERIM 1).
    pub intensity: f32,
    /// particleLightScale.
    pub scale: f32,
}

impl System {
    /// Lights of the isLight stages for this frame (same spawn and seed rules as the quads; evaluation
    /// order baseColor[0..3], origin, size[0], brightness, then fade).
    pub fn lights(&mut self, f: &Frame, out: &mut Vec<ParticleLight>) {
        let decl = self.decl.clone();
        for (si, stage) in decl.stages.iter().enumerate() {
            if stage.hidden || stage.cycle_msec == 0 || !stage.is_light || stage.gpu {
                continue;
            }
            let tables = &decl.tables;
            cpu_stage(self, si, stage, tables, f, &mut |g, count| {
                let ec = f.entity_color;
                let mut c = [0.0f32; 4];
                for (k, v) in c.iter_mut().enumerate() {
                    *v = stage.base_color[k].eval_at(tables, g.frac2, &mut g.seed);
                }
                let (pos, _) = origin(stage, tables, count, g, f);
                let radius = stage.size[0].eval_at(tables, g.frac2, &mut g.seed);
                let bright = stage.brightness.eval_at(tables, g.frac2, &mut g.seed);
                let intensity = fade(stage, g, count) * bright;
                if intensity > 0.001 {
                    out.push(ParticleLight { stage: si, origin: pos, color: Vec4::new(c[0] * ec.x, c[1] * ec.y, c[2] * ec.z, c[3] * ec.w), radius, intensity, scale: stage.light_scale });
                }
            });
        }
    }
}

/// Emission count with distance scaling (FUN_1419a3940); r_particlesEmissionScale_* defaults.
fn emission_count(stage: &Stage, count: i32, f: &Frame) -> i32 {
    let (mut far, mut near, mut far_scale, mut near_scale) = (3500.0f32, 3400.0f32, 0.0f32, 1.0f32);
    if stage.emission_far != 0.0 || stage.emission_near != 0.0 {
        far = stage.emission_far;
        near = stage.emission_near;
        far_scale = stage.emission_far_scale;
        near_scale = stage.emission_near_scale;
    }
    let mut scale = 1.0;
    if near < far {
        let off = Vec3::new(stage.offset[0].val0, stage.offset[1].val0, stage.offset[2].val0);
        let d = (f.origin + f.axis.to_parent(off) - f.view.origin).length();
        let t = ((d - near) / (far - near)).clamp(0.0, 1.0);
        let tiny = |v: f32| if v.abs() <= 1e-18 { 0.0 } else { v };
        scale = (1.0 - t) * tiny(near_scale) + tiny(far_scale) * t;
    }
    (count as f32 * scale) as i32
}

fn cpu_stage(sys: &mut System, si: usize, stage: &Stage, tables: &[Table], f: &Frame, each: &mut dyn FnMut(&mut Gen, i32)) {
    // LOD 0: the full particle count.
    let count = stage.total_particles as i32;
    if count <= 0 {
        return;
    }
    let cycle_ms = ((stage.max_dead_time + stage.max_particle_life) * 1000.0) as i32;
    if cycle_ms <= 0 {
        return;
    }
    let elapsed = ((f.time_ms as f32 * 0.001 - (stage.time_offset + sys.start)) * 1000.0) as i32;
    let prev_elapsed = ((f.prev_time_ms as f32 * 0.001 - (stage.time_offset + sys.start)) * 1000.0) as i32;
    let cur_cycle = elapsed / cycle_ms;
    let bunch_ms = (stage.spawn_bunching * stage.bunch_time * 1000.0) as i32;
    let emit = emission_count(stage, count, f);
    // Sorted stages start at the oldest particle.
    let mut first = 0;
    if stage.sort == 1 || stage.sort == 2 {
        let mut acc = bunch_ms;
        for i in 1..count {
            let o = elapsed - acc / count;
            if o >= 0 && elapsed - cur_cycle * cycle_ms < o % cycle_ms {
                first = i;
                break;
            }
            acc += bunch_ms;
        }
    }
    let cyc = if stage.random_on_cycle { cur_cycle as u32 } else { 0 };
    let base = (stage.diversity as u32).wrapping_add(sys.seed) & 0x7fff;
    let seed_cur0 = ((cyc & 0x1f) << 10) ^ base;
    let seed_prev0 = (cyc.wrapping_mul(0x400).wrapping_sub(0x400) & 0x7fff) ^ base;
    let (mut seed_cur, mut seed_prev) = (seed_cur0, seed_prev0);
    let state = &mut sys.stages[si];
    for idx in first..first + count {
        let mut i = idx;
        if stage.sort == 1 {
            i = first - 1 + (first + count - idx);
        }
        if stage.sort == 1 || stage.sort == 2 {
            i %= count;
            seed_cur = seed_cur0;
            seed_prev = seed_prev0;
            if i >= 0 {
                for _ in 0..=i {
                    seed_cur = IdRandom::step(seed_cur);
                    seed_prev = IdRandom::step(seed_prev);
                }
            }
        } else {
            seed_cur = IdRandom::step(seed_cur);
            seed_prev = IdRandom::step(seed_prev);
        }
        let offset = if emit < 1 { 0 } else { (i * bunch_ms) / emit };
        if elapsed - offset < 0 {
            continue;
        }
        let iu = i as usize;
        let Some(&prev_spawn) = state.spawn.get(iu) else { continue };
        if prev_spawn < 0 && first + emit <= idx {
            continue;
        }
        let cycles_done = (elapsed - offset) / cycle_ms;
        let mut ps = prev_spawn;
        if cycle_ms < elapsed - ps {
            ps = prev_elapsed - offset;
        }
        let prev_cycle = if ps < 0 { -1 } else { ps / cycle_ms };
        if prev_cycle < cycles_done && idx < first + emit {
            state.spawn[iu] = ((offset - elapsed % cycle_ms) + elapsed).max(0);
        }
        let spawn = state.spawn[iu];
        let p_cycle = spawn / cycle_ms;
        if stage.cycles != 0 && stage.cycles as i32 <= p_cycle {
            continue;
        }
        let age = elapsed - spawn;
        if sys.stop_ms != 0 && f.time_ms - age >= sys.stop_ms {
            continue;
        }
        let mut seed = if p_cycle == cur_cycle { seed_cur } else { seed_prev };
        let life = stage.particle_life.eval_at(tables, i as f32 / count as f32, &mut seed);
        if age < 0 || ((life * 1000.0) as i32) < age {
            continue;
        }
        let frac = if life <= 1.192_092_9e-07 { 1.0 } else { (age as f32 * 0.001) / life };
        let mut g = Gen { index: i, age: age as f32 * 0.001, life, frac, frac2: frac, seed };
        each(&mut g, count);
    }
}

/// Fade over life and index (0x1419a38c0).
fn fade(stage: &Stage, g: &Gen, count: i32) -> f32 {
    let mut fade = 1.0;
    if stage.fade_in > g.frac {
        fade = g.frac / stage.fade_in;
    }
    if stage.fade_out > 1.0 - g.frac {
        fade *= (1.0 - g.frac) / stage.fade_out;
    }
    if stage.fade_index != 0.0 {
        let ix = (count as f32 - g.index as f32) / count as f32;
        if stage.fade_index > ix {
            fade *= ix / stage.fade_index;
        }
    }
    fade
}

fn byte(v: f32) -> u8 {
    (v as i32).clamp(0, 255) as u8
}

/// FUN_1419a3fb0: one particle -> 0 or 4 vertices.
fn particle(si: usize, stage: &Stage, tables: &[Table], count: i32, g: &mut Gen, f: &Frame, out: &mut Vec<Quad>) {
    let (pos, vel) = origin(stage, tables, count, g, f);
    let fd = fade(stage, g, count);
    let nf = 1.0 - fd;
    let shadow = stage.min_shadow.max(f.shadow);
    let bright = stage.brightness.eval_at(tables, g.frac2, &mut g.seed) * shadow;
    let ecb = stage.entity_color_blend;
    let ec = f.entity_color;
    let ec = [if ec.x.abs() <= 1e-18 { 0.0 } else { ec.x }, if ec.y.abs() <= 1e-18 { 0.0 } else { ec.y }, if ec.z.abs() <= 1e-18 { 0.0 } else { ec.z }, if ec.w.abs() <= 1e-18 { 0.0 } else { ec.w }];
    let blended = alpha_blended(stage);
    let (rgb_fade, a_fade) = if blended { (1.0, f.fade) } else { (f.fade, 1.0) };
    let mut base = [0.0f32; 4];
    for (c, b) in base.iter_mut().enumerate() {
        *b = stage.base_color[c].eval_at(tables, g.frac2, &mut g.seed);
    }
    let fc = stage.fade_color;
    let fcs = [fc.x, fc.y, fc.z, fc.w];
    let mut col = [0u8; 4];
    for c in 0..3 {
        col[c] = byte((nf * fcs[c] + base[c] * fd) * ((1.0 - ecb) * 1.0 + ec[c] * ecb) * bright * rgb_fade * 255.0);
    }
    col[3] = byte((nf * fcs[3] + base[3] * fd) * a_fade * ((1.0 - ecb) * 1.0 + ec[3] * ecb) * 255.0);
    let mut gp = [0.0f32; 4];
    for (k, v) in gp.iter_mut().enumerate() {
        *v = stage.generic[k].eval_at(tables, g.frac2, &mut g.seed);
    }
    // Culling (r_particlesMinAlpha).
    let thr = MIN_ALPHA / 255.0;
    let alpha = col[3] as f32 / 255.0;
    if (blended && (gp[1] * 10.0 + 1.0) * alpha <= thr)
        || (!blended && ((col[0] as u32 + col[1] as u32 + col[2] as u32) as f32 * (gp[0] * 10.0 + 1.0) * 0.33) / 255.0 <= thr)
    {
        return;
    }
    // The stored alpha is renormalised above the threshold.
    col[3] = byte(((alpha - thr) / (1.0 - thr)) * 255.0);
    let generic = [byte(gp[0] * 255.0), byte(gp[1] * 255.0), byte(gp[2] * 255.0), byte(gp[3] * 255.0)];
    let alpha_scale = byte(stage.soft_alpha_scale * 255.0) as f32 / 255.0;
    let mut v = Vertex { color: col, generic, alpha_scale, ..Default::default() };
    v.pos = pos;
    match stage.orientation {
        2 => trail(si, stage, tables, count, g, f, pos, v, out),
        3 => out.push(Quad { stage: si, verts: aimed(stage, tables, g, f, pos, vel, v) }),
        _ => out.push(Quad { stage: si, verts: view_quad(stage, tables, g, f, pos, v) }),
    }
}

/// Transparency-sorted particle materials (transsortblend) blend with ONE, ONE_MINUS_SRC_ALPHA.
fn alpha_blended(_stage: &Stage) -> bool {
    true
}

/// FUN_1419ac350: particle position (world) and orientation velocity (world).
fn origin(stage: &Stage, tables: &[Table], count: i32, g: &mut Gen, f: &Frame) -> (Vec3, Vec3) {
    let ds = Vec3::splat(f.size_scale);
    let sx = stage.dist_size[0].eval_at(tables, g.frac2, &mut g.seed) * ds.x;
    let sy = stage.dist_size[1].eval_at(tables, g.frac2, &mut g.seed) * ds.y;
    let sz = stage.dist_size[2].eval_at(tables, g.frac2, &mut g.seed) * ds.z;
    let mut rng = IdRandom(g.seed);
    let mut p = match stage.dist_type {
        0 => {
            let r = if stage.dist_random {
                let z = rng.crandom_float();
                let y = rng.crandom_float();
                let x = rng.crandom_float();
                Vec3::new(x, y, z)
            } else {
                Vec3::ONE
            };
            Vec3::new(sx * r.x, sy * r.y, sz * r.z)
        }
        1 => {
            let (hi, lo) = if stage.dist_random {
                let a = rng.crandom_float();
                let b = rng.crandom_float();
                (a, b)
            } else {
                (1.0, 1.0)
            };
            let (s, c) = sincos(lo * TWO_PI);
            Vec3::new(s * sx, c * sy, sz * hi)
        }
        2 | 5 => {
            let n = if stage.dist_type == 5 || stage.dist_random {
                let x = rng.crandom_float();
                let y = rng.crandom_float();
                let z = rng.crandom_float();
                normalize(Vec3::new(x, y, z))
            } else {
                Vec3::ONE
            };
            Vec3::new(sx * n.x, sy * n.y, sz * n.z)
        }
        3 => {
            let z = rng.crandom_float();
            let y = rng.crandom_float();
            let x = rng.crandom_float();
            let mut v = Vec3::new(x, y, z);
            match rng.next_int() % 6 {
                0 => v.x = 1.0,
                1 => v.y = 1.0,
                2 => v.z = 1.0,
                3 => v.x = -1.0,
                4 => v.y = -1.0,
                _ => v.z = -1.0,
            }
            Vec3::new(sx * v.x, sy * v.y, sz * v.z)
        }
        4 => {
            let z = rng.crandom_float();
            let a = rng.crandom_float() * TWO_PI;
            let (s, c) = sincos(a);
            let n = 1.0 / (s * s + c * c).max(1e-30).sqrt();
            Vec3::new(s * n * sx, c * n * sy, sz * z)
        }
        _ => {
            let corners = [Vec3::new(-sx, -sy, sz), Vec3::new(sx, -sy, sz), Vec3::new(sx, sy, sz), Vec3::new(-sx, sy, sz)];
            corners[(g.index & 3) as usize]
        }
    };
    g.seed = rng.0;
    // customPath (helix/flies/orbit/drip) is not used by the weapon FX decoded so far.
    let sl = g.index as f32 / (count as f32).max(1.0);
    let spawn = Vec3::new(
        stage.spawn_location[0].eval_at(tables, sl, &mut g.seed),
        stage.spawn_location[1].eval_at(tables, sl, &mut g.seed),
        stage.spawn_location[2].eval_at(tables, sl, &mut g.seed),
    );
    p += spawn;
    let off = Vec3::new(
        stage.offset[0].eval_at(tables, g.frac2, &mut g.seed),
        stage.offset[1].eval_at(tables, g.frac2, &mut g.seed),
        stage.offset[2].eval_at(tables, g.frac2, &mut g.seed),
    );
    p += off;
    let mut fr = [0.0f32; 3];
    for (k, v) in fr.iter_mut().enumerate() {
        *v = stage.friction[k].eval_at(tables, g.frac2, &mut g.seed).clamp(0.0, 1.0);
    }
    let speed = Vec3::new(
        stage.speed[0].eval_at(tables, g.frac - fr[0] * 0.5 * g.frac * g.frac2, &mut g.seed),
        stage.speed[1].eval_at(tables, g.frac - fr[1] * 0.5 * g.frac * g.frac2, &mut g.seed),
        stage.speed[2].eval_at(tables, g.frac - fr[2] * 0.5 * g.frac * g.frac2, &mut g.seed),
    );
    let mut rng = IdRandom(g.seed);
    let (mut d, mul) = match stage.dir_type {
        0 => {
            let a = rng.crandom_float() * stage.dir_parms[0] * DEG2RAD;
            let b = rng.crandom_float() * PI;
            let (sa, ca) = sincos(a);
            let (sb, cb) = sincos(b);
            (Vec3::new(cb * sa, sb * sa, ca), speed)
        }
        1 => {
            let n = normalize(p);
            (Vec3::new(n.x, n.y, n.z + stage.dir_parms[0]), speed)
        }
        2 => (normalize(p), speed),
        3 => (normalize(speed), speed.abs()),
        _ => (speed, speed),
    };
    g.seed = rng.0;
    d *= mul;
    if stage.dir_type == 0 {
        d = stage.cone_axis.to_parent(d);
    }
    if stage.dir_world {
        d = f.axis.to_local(d);
    }
    let vlife = d * g.life;
    let mut acc = Vec3::new(
        stage.acceleration[0].eval_at(tables, g.frac2, &mut g.seed),
        stage.acceleration[1].eval_at(tables, g.frac2, &mut g.seed),
        stage.acceleration[2].eval_at(tables, g.frac2, &mut g.seed),
    );
    if stage.accel_world {
        acc = f.axis.to_local(acc);
    }
    let gv = -stage.gravity.eval_at(tables, g.frac2, &mut g.seed);
    let grav = if stage.gravity_world { f.axis.to_local(Vec3::new(0.0, 0.0, gv)) } else { Vec3::new(0.0, 0.0, gv) };
    let wind = stage.wind_bias.eval_at(tables, g.frac2, &mut g.seed);
    let disp = vlife + g.age * (f.axis.to_local(f.wind) * wind + acc + g.age * grav);
    p += disp;
    let mut w = f.origin + f.axis.to_parent(p);
    if stage.camera_offset != 0.0 {
        w -= normalize(f.view.origin - w) * stage.camera_offset;
    }
    let mut vel = if stage.orient_to_vel_only { vlife } else { disp };
    vel = f.axis.to_parent(vel);
    if stage.orient_to_world_vel {
        vel += f.velocity;
    }
    (w, vel)
}

/// Texcoords for the four corners (FUN_1419ae320 with FUN_1419a3bc0). Returns (uv, uv_next, frame_frac)
/// per corner index after the rotate permutation.
fn texcoords(stage: &Stage, tables: &[Table], g: &mut Gen, rand_frame: i32, rand_row: i32, sr: [f32; 2], tr: [f32; 2]) -> ([[f32; 2]; 4], [[f32; 2]; 4], f32) {
    let rot = stage.tex_rotate;
    let (mut s0, mut t0, mut ds, mut dt) = (sr[0], tr[0], sr[1] - sr[0], tr[1] - tr[0]);
    if rot == 1 || rot == 3 {
        std::mem::swap(&mut s0, &mut t0);
        std::mem::swap(&mut ds, &mut dt);
    }
    let odd = g.index & 1 != 0;
    let mut flip_s = stage.flip_s == 2 || (stage.flip_s == 1 && odd);
    let mut flip_t = stage.flip_t == 2 || (stage.flip_t == 1 && odd);
    if rot == 1 || rot == 2 {
        flip_s = !flip_s;
        flip_t = !flip_t;
    }
    if flip_s {
        ds = -ds;
        s0 = 1.0 - s0;
    }
    if flip_t {
        dt = -dt;
        t0 = 1.0 - t0;
    }
    let mut frame_frac = 0u8;
    let mut last_col = false;
    let cols = stage.columns.max(1) as i32;
    let rows = stage.rows.max(1) as i32;
    if stage.columns > 1 || stage.rows > 1 {
        let rows_used = if stage.random_row { 1 } else { rows };
        let frames = rows_used * cols;
        let start = if stage.start_frame < 0 { rand_frame } else { stage.start_frame as i32 };
        let fr = match stage.anim_type {
            0 => {
                let r = stage.anim_rate.eval_at(tables, g.frac2, &mut g.seed);
                let k = if stage.anim_rate.calc == crate::parm::Calc::Generic { g.age } else { 1.0 };
                k * r + start as f32
            }
            1 => {
                let r = stage.anim_rate.eval_at(tables, g.frac2, &mut g.seed);
                let k = if stage.anim_rate.calc == crate::parm::Calc::Generic { g.frac } else { 1.0 };
                (k * r + start as f32).min(frames as f32 - 1.0)
            }
            2 => (frames as f32 / g.life) * g.age + start as f32,
            _ => 0.0,
        };
        let frame = fr as i32;
        let row = if stage.random_row { rand_row } else { (frame % frames) / cols };
        let ic = 1.0 / cols as f32;
        let ir = 1.0 / rows as f32;
        ds *= ic;
        s0 = ((frame % cols) as f32 + s0) * ic;
        dt *= ir;
        t0 = (row as f32 + t0) * ir;
        if stage.frame_blending {
            frame_frac = byte((fr - frame as f32) * 255.0);
            last_col = !(rows < 2 || stage.random_row || frame % cols != cols - 1);
        }
    }
    let s = [s0, s0 + ds];
    let t = [t0, t0 + dt];
    let eps_s = if flip_s { 1e-6 } else { 0.0 };
    let eps_t = if flip_t { 1e-6 } else { 0.0 };
    let fract = |v: f32| v - v.floor();
    let mut ns0 = if !last_col { fract((1.0 / cols as f32 + s0) - eps_s) } else { (1.0 / cols as f32) * s0 };
    let mut ns1 = ns0 + ds;
    ns0 = ns0.clamp(0.0, 1.0);
    ns1 = ns1.clamp(0.0, 1.0);
    let mut nt0 = if last_col { fract((1.0 / rows as f32 + t0) - eps_t) } else { t0 };
    nt0 = nt0.clamp(0.0, 1.0);
    let nt1 = (nt0 + dt).clamp(0.0, 1.0);
    let order: [usize; 4] = if rot == 1 || rot == 3 { [2, 0, 3, 1] } else { [0, 1, 2, 3] };
    let mut uv = [[0.0; 2]; 4];
    let mut uvn = [[0.0; 2]; 4];
    for (k, &idx) in order.iter().enumerate() {
        uv[k] = [s[idx & 1], t[idx >> 1]];
        uvn[k] = [[ns0, ns1][idx & 1], [nt0, nt1][idx >> 1]];
    }
    (uv, uvn, frame_frac as f32 / 255.0)
}

fn random_frame_row(stage: &Stage, g: &mut Gen) -> (i32, i32) {
    let cols = stage.columns.max(1) as i32;
    let rows = stage.rows.max(1) as i32;
    let rows_used = if stage.random_row { 1 } else { rows };
    let mut rng = IdRandom(g.seed);
    let mut frame = 0;
    if rows_used * cols - 1 > 0 {
        frame = (rng.next_int() as i32) % (rows_used * cols);
    }
    let mut row = 0;
    if rows - 1 > 0 {
        row = (rng.next_int() as i32) % rows;
    }
    g.seed = rng.0;
    (frame, row)
}

/// Distance size scaling (distanceScale near/far).
fn dist_scale(stage: &Stage, f: &Frame, pos: Vec3) -> f32 {
    if stage.dist_scale_near < stage.dist_scale_far {
        let d = (pos - f.view.origin).length();
        let t = ((d - stage.dist_scale_near) / (stage.dist_scale_far - stage.dist_scale_near)).clamp(0.0, 1.0);
        let tiny = |v: f32| if v.abs() <= 1e-18 { 0.0 } else { v };
        return (1.0 - t) * tiny(stage.dist_scale_near_scale) + tiny(stage.dist_scale_far_scale) * t;
    }
    1.0
}

/// Edge-on fade toward the fade colour for non-view orientations (aimedViewFade < 1).
fn view_fade(stage: &Stage, f: &Frame, pos: Vec3, a: Vec3, b: Vec3, v: &mut Vertex) {
    if stage.aimed_view_fade >= 1.0 {
        return;
    }
    let d = normalize(pos - f.view.origin);
    let k = if (4..=7).contains(&stage.orientation) { 1.0 - a.cross(b).dot(d).abs() } else { d.dot(b).abs() };
    let k = (k - stage.aimed_view_fade).max(0.0) / (1.0 - stage.aimed_view_fade);
    let ec = f.entity_color;
    let fc = stage.fade_color;
    let target = [ec.x * fc.x, ec.y * fc.y, ec.z * fc.z, ec.w * fc.w];
    for c in 0..4 {
        v.color[c] = byte(((v.color[c] as f32 / 255.0) * (1.0 - k) + k * target[c]) * 255.0);
    }
}

/// FUN_1419a8060: view-facing (and axis-locked) quads.
fn view_quad(stage: &Stage, tables: &[Table], g: &mut Gen, f: &Frame, pos: Vec3, mut v: Vertex) -> [Vertex; 4] {
    let (rf, rr) = random_frame_row(stage, g);
    let (uv, uvn, ff) = texcoords(stage, tables, g, rf, rr, [0.0, 1.0], [0.0, 1.0]);
    let width = stage.size[0].eval_at(tables, g.frac2, &mut g.seed) * f.size_scale * dist_scale(stage, f, pos);
    let height = stage.aspect.eval_at(tables, g.frac2, &mut g.seed) * width;
    let (a, b) = if stage.orientation != 7 {
        let init = stage.initial_angle[0].eval_at(tables, g.frac2, &mut g.seed);
        let rate = stage.rotation[0].eval_at(tables, g.frac2, &mut g.seed);
        let ang = if g.index & 1 != 0 && stage.allow_rot_dir_override { init - rate * g.life } else { init + rate * g.life } * DEG2RAD;
        let (s, c) = sincos(ang);
        let z = f.axis.z;
        match stage.orientation {
            0 => (f.view.right * c + f.view.up * s, f.view.up * c - f.view.right * s),
            1 => (f.view.right * c + z * s, z * c - f.view.right * s),
            4 => (f.axis.to_parent(Vec3::new(0.0, c, s)), f.axis.to_parent(Vec3::new(0.0, -s, c))),
            5 => (f.axis.to_parent(Vec3::new(c, 0.0, s)), f.axis.to_parent(Vec3::new(-s, 0.0, c))),
            6 => (f.axis.to_parent(Vec3::new(s, c, 0.0)), f.axis.to_parent(Vec3::new(c, -s, 0.0))),
            _ => (f.view.right, f.view.up),
        }
    } else {
        let i0 = stage.initial_angle[0].eval_at(tables, g.frac2, &mut g.seed);
        let i1 = stage.initial_angle[1].eval_at(tables, g.frac2, &mut g.seed);
        let i2 = stage.initial_angle[2].eval_at(tables, g.frac2, &mut g.seed);
        let r0 = stage.rotation[0].eval_at(tables, g.frac2, &mut g.seed);
        let r1 = stage.rotation[1].eval_at(tables, g.frac2, &mut g.seed);
        let r2 = stage.rotation[2].eval_at(tables, g.frac2, &mut g.seed);
        let (s0, c0) = sincos((i0 + r0 * g.life) * DEG2RAD);
        let (s1, c1) = sincos((i1 + r1 * g.life) * DEG2RAD);
        let (s2, c2) = sincos((i2 + r2 * g.life) * DEG2RAD);
        let k = s1 * s0;
        let b = Vec3::new(k * c2 - s2 * c0, c2 * c0 + k * s2, c1 * s0);
        let a = Vec3::new(c2 * c1, s2 * c1, -s1);
        (f.axis.to_parent(a), f.axis.to_parent(b))
    };
    view_fade(stage, f, pos, a, b, &mut v);
    let a = a * width;
    let b = b * height;
    let pivot = a * stage.pivot.x + b * stage.pivot.y;
    let corners = [pos - a + b + pivot, pos + a + b + pivot, pos - a - b + pivot, pos + a - b + pivot];
    let mut out = [v; 4];
    for k in 0..4 {
        out[k].pos = corners[k];
        out[k].uv = uv[k];
        out[k].uv_next = uvn[k];
        out[k].frame_frac = ff;
    }
    out
}

/// FUN_1419a7690: a quad stretched along the particle's velocity.
fn aimed(stage: &Stage, tables: &[Table], g: &mut Gen, f: &Frame, pos: Vec3, vel: Vec3, mut v: Vertex) -> [Vertex; 4] {
    let width = stage.size[0].eval_at(tables, g.frac2, &mut g.seed) * f.size_scale * dist_scale(stage, f, pos);
    let length = stage.aspect.eval_at(tables, g.frac2, &mut g.seed) * stage.segment_length;
    let dir = normalize(vel);
    let to_view = normalize(pos - f.view.origin);
    let side = normalize(dir.cross(to_view)) * width;
    let head = pos + dir * length;
    if stage.aimed_view_fade < 1.0 {
        let k = (normalize(head - pos).dot(to_view).abs() - stage.aimed_view_fade).max(0.0) / (1.0 - stage.aimed_view_fade);
        let ec = f.entity_color;
        let fc = stage.fade_color;
        let target = [ec.x * fc.x, ec.y * fc.y, ec.z * fc.z, ec.w * fc.w];
        for c in 0..4 {
            v.color[c] = byte(((v.color[c] as f32 / 255.0) * (1.0 - k) + k * target[c]) * 255.0);
        }
    }
    let (rf, rr) = random_frame_row(stage, g);
    let (uv, uvn, ff) = texcoords(stage, tables, g, rf, rr, [0.0, 1.0], [0.0, 1.0]);
    let corners = [head - side, head + side, pos - side, pos + side];
    let mut out = [v; 4];
    for k in 0..4 {
        out[k].pos = corners[k];
        out[k].uv = uv[k];
        out[k].uv_next = uvn[k];
        out[k].frame_frac = ff;
    }
    out
}

/// FUN_1419aa8c0: numTrails+1 segments; segment k re-evaluates the origin (same seed) at the earlier age
/// age - (k+1)*span, span = aspect * segmentLength / (numTrails + 1) seconds; ribbon width = size * FX size.
#[allow(clippy::too_many_arguments)]
fn trail(si: usize, stage: &Stage, tables: &[Table], count: i32, g: &mut Gen, f: &Frame, head: Vec3, v: Vertex, out: &mut Vec<Quad>) {
    let width = stage.size[0].eval_at(tables, g.frac2, &mut g.seed) * f.size_scale;
    let aspect = stage.aspect.eval_at(tables, g.frac2, &mut g.seed);
    let span = (1.0 / (stage.num_trails as f32 + 1.0)) * aspect * stage.segment_length;
    let (age, frac, seed0) = (g.age, g.frac, g.seed);
    let inv_life = 1.0 / g.life;
    let mut prev = head;
    let mut prev_side = Vec3::ZERO;
    let mut last_frac = 0.0f32;
    let first = out.len();
    let mut k = 0i32;
    while k <= stage.num_trails as i32 {
        let t = age - k as f32 * span;
        if t < 0.0 {
            break;
        }
        let mut tn = t - span;
        if tn < 0.0 {
            last_frac = 1.0 - t / span;
            tn = 0.0;
        }
        let mut sg = Gen { age: tn, seed: seed0, frac: tn * inv_life, frac2: tn * inv_life, ..*g };
        let (p, _) = origin(stage, tables, count, &mut sg, f);
        let dir = normalize(prev - p);
        let to_view = normalize(p - f.view.origin);
        let axis = if stage.orient_to_vel_only { f.axis.x } else { dir };
        let side = normalize(axis.cross(to_view)) * width;
        let (a0, a1) = if k == 0 { (prev - side, prev + side) } else { (prev - prev_side, prev + prev_side) };
        let mut verts = [v; 4];
        verts[0].pos = a0;
        verts[1].pos = a1;
        verts[2].pos = p - side;
        verts[3].pos = p + side;
        if stage.aimed_view_fade < 1.0 {
            let kk = if (4..=7).contains(&stage.orientation) { 1.0 - side.normalize_or_zero().cross(dir).dot(to_view).abs() } else { to_view.dot(dir).abs() };
            let kk = (kk - stage.aimed_view_fade).max(0.0) / (1.0 - stage.aimed_view_fade);
            let ec = f.entity_color;
            let fc = stage.fade_color;
            let target = [ec.x * fc.x, ec.y * fc.y, ec.z * fc.z, ec.w * fc.w];
            for vv in verts.iter_mut() {
                for c in 0..4 {
                    vv.color[c] = byte(((v.color[c] as f32 / 255.0) * (1.0 - kk) + kk * target[c]) * 255.0);
                }
            }
        }
        out.push(Quad { stage: si, verts });
        prev = p;
        prev_side = side;
        k += 1;
    }
    g.age = age;
    let (rf, rr) = random_frame_row(stage, g);
    let segs = (out.len() - first) as i32;
    let mut t0 = 0.0f32;
    for (n, q) in out[first..].iter_mut().enumerate() {
        let t1 = if n as i32 == segs - 1 { 1.0 } else { t0 + 1.0 / (segs as f32 - last_frac) };
        let (uv, uvn, ff) = texcoords(stage, tables, g, rf, rr, [0.0, 1.0], [t0, t1]);
        for c in 0..4 {
            q.verts[c].uv = uv[c];
            q.verts[c].uv_next = uvn[c];
            q.verts[c].frame_frac = ff;
        }
        t0 = t1;
    }
    g.frac = frac;
    g.seed = seed0;
}

/// PARTICLE_MAX_RANDOM: the $particleRandom table size (gpuparticles.inc).
const GPU_RANDOM: usize = 8192;

/// The process-wide idRandom state at 0x1440110f0 (initial value 666 in .data). The GPU particle manager
/// fills $particleRandom from it at init (FUN_1419a2630: 8192 x {x, y, z, w} draws) and then draws four
/// values per active emitter per frame for $gpuParticleEmitSeed (FUN_1419a2f60). Nothing else uses it.
static GPU_SEED: std::sync::Mutex<u32> = std::sync::Mutex::new(666);
static GPU_TABLE: std::sync::OnceLock<Vec<Vec4>> = std::sync::OnceLock::new();

fn gpu_table() -> &'static [Vec4] {
    GPU_TABLE.get_or_init(|| {
        let mut s = GPU_SEED.lock().unwrap_or_else(|e| e.into_inner());
        let mut r = IdRandom(*s);
        let t = (0..GPU_RANDOM).map(|_| Vec4::new(r.random_float(), r.random_float(), r.random_float(), r.random_float())).collect();
        *s = r.0;
        t
    })
}

/// One emitter's $gpuParticleEmitSeed: four draws stored w, z, y, x (the dispatch's stack order).
fn gpu_emit_seed() -> Vec4 {
    gpu_table();
    let mut s = GPU_SEED.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = IdRandom(*s);
    let (w, z, y, x) = (r.random_float(), r.random_float(), r.random_float(), r.random_float());
    *s = r.0;
    Vec4::new(x, y, z, w)
}

/// FetchRandom / FetchRandomWithSeed (gpuparticles.inc).
fn fetch_random(table: &[Vec4], index: i32, seed: Vec4) -> Vec4 {
    let v = table[(index & (GPU_RANDOM as i32 - 1)) as usize] + seed + Vec4::splat(0.1234 * (index as f32 * (1.0 / GPU_RANDOM as f32)).floor());
    v - v.floor()
}

/// Distribute (gpuparticles.inc), by prtDistributionTypeGPU_t.
fn distribute(kind: usize, r: Vec4) -> Vec3 {
    let sphere_surface = |r: Vec4| {
        let a = 2.0 * 3.141592 * r.x;
        let z = r.y * 2.0 - 1.0;
        let k = (1.0 - z * z).sqrt();
        Vec3::new(k * a.cos(), k * a.sin(), z)
    };
    let cylinder_surface = |r: Vec4| {
        let a = 2.0 * 3.141592 * r.x;
        Vec3::new(a.cos(), a.sin(), r.y)
    };
    match kind {
        0 => Vec3::new(r.x * 2.0 - 1.0, r.y * 2.0 - 1.0, 0.0),
        1 => Vec3::new(r.x * 2.0 - 1.0, r.y * 2.0 - 1.0, r.z * 2.0 - 1.0),
        2 => {
            let (a, b) = (r.x * 2.0 - 1.0, r.y * 2.0 - 1.0);
            if r.z < 1.0 / 3.0 {
                Vec3::new(if r.z < 1.0 / 6.0 { -1.0 } else { 1.0 }, a, b)
            } else if r.z < 2.0 / 3.0 {
                Vec3::new(a, if r.z < 3.0 / 6.0 { -1.0 } else { 1.0 }, b)
            } else {
                Vec3::new(a, b, if r.z < 5.0 / 6.0 { -1.0 } else { 1.0 })
            }
        }
        3 => sphere_surface(r) * r.w.sqrt(),
        4 => sphere_surface(r),
        5 => {
            let c = cylinder_surface(r);
            let s = r.z.sqrt();
            Vec3::new(c.x * s, c.y * s, c.z)
        }
        _ => cylinder_surface(r),
    }
}

/// GLSL-style mod (x - y * floor(x / y)).
fn fmod_floor(x: f32, y: f32) -> f32 {
    x - y * (x / y).floor()
}

/// AnimateTexCoord (gpuparticles.inc); the random row uses numFrames as the shader does.
fn animate_tc(tc: [f32; 2], frame: f32, frames: f32, cols: f32, rows: f32, random_row: bool, rv: f32) -> [f32; 2] {
    let col = fmod_floor(frame, cols).floor();
    let row = if random_row { (rv * frames - 1e-6).floor() } else { (fmod_floor(frame, frames) / cols).floor() };
    [(tc[0] + col) / cols, (tc[1] + row) / rows]
}

/// FUN_140277620: sRGB -> linear on all four components, clamped to [0, 1] (< FLT_MIN and NaN -> 0).
fn srgb_to_linear4(c: Vec4) -> Vec4 {
    let f = |v: f32| {
        if v.is_nan() || v < f32::MIN_POSITIVE {
            0.0
        } else if v < 1.0 {
            if v > 0.04045 { ((v + 0.055) / 1.055).powf(2.4) } else { v / 12.92 }
        } else {
            1.0
        }
    };
    Vec4::new(f(c.x), f(c.y), f(c.z), f(c.w))
}

/// GPU stages (gpuStage = true): the compute pass gpuparticlesimulate and the vertex pass gpuparticlerender
/// reproduced on the CPU, with the emitter parameters the engine uploads.
///
/// - emitter timing (FUN_1415e9600): elapsed ms = (int)((time - (start + timeOffset)) * 1000); period ms =
///   (int)((maxDeadTime + maxParticleLife) * 1000); the emitter lives while elapsed >= 0 and (cycles == 0 or
///   elapsed / period <= cycles), else it is freed (its particles reset when it is allocated again).
/// - $gpuParticleEmitParms = (elapsed s, period s, (int)(spawnBunching * bunchTime * 1000) * 0.001 / total,
///   total); EmitParms2 = (orientation, cycles, reset); EmitLifetime = particleLife range; the emit axis/origin
///   are the render entity's.
/// - colours (FUN_1415e6bc0): initial/final = SRGB->linear(colour) with rgb * overbright; fadeColor raw.
///
/// INTERIM: depth-buffer collision and vector fields are not simulated (no depth/normal buffers here), every
/// particle is committed (no clip-space Z test), $gpuParticleTimeParms.x is the frame delta, $particleVel is 0,
/// and a stopped system hides particles whose current cycle spawned at or after the stop (as the CPU path).
fn gpu_stage(sys: &mut System, si: usize, stage: &Stage, f: &Frame, out: &mut Vec<Quad>) {
    let g = &stage.gpu_stage;
    let total = g.total.max(0);
    let elapsed_ms = ((f.time_ms as f32 * 0.001 - (sys.start + g.time_offset)) * 1000.0) as i32;
    let period_ms = stage.cycle_msec;
    let st = &mut sys.stages[si];
    if total == 0 || period_ms <= 0 || elapsed_ms < 0 || g.cycles < 0 || (g.cycles != 0 && elapsed_ms / period_ms > g.cycles) {
        st.gpu = None;
        return;
    }
    let elapsed = elapsed_ms as f32 * 0.001;
    let period = period_ms as f32 * 0.001;
    let bunch = (g.spawn_bunching * stage.bunch_time * 1000.0) as i32 as f32 * 0.001;
    let interval = bunch / total as f32;
    let em = st.gpu.get_or_insert_with(|| GpuEmitter { elapsed, reset: true, particles: vec![GpuParticle::default(); total as usize] });
    if em.elapsed > elapsed {
        em.reset = true;
    }
    em.elapsed = elapsed;
    let reset = std::mem::take(&mut em.reset);

    let table = gpu_table();
    let seed = gpu_emit_seed();
    let (life_min, life_max) = g.life.range();
    let (rate_min, rate_max) = g.anim_rate.range();
    let accel = g.acceleration;
    let dt = (f.time_ms - f.prev_time_ms).max(0) as f32 * 0.001;
    let initial = {
        let c = srgb_to_linear4(g.initial_color);
        Vec4::new(c.x * g.initial_overbright, c.y * g.initial_overbright, c.z * g.initial_overbright, c.w)
    };
    let fin = {
        let c = srgb_to_linear4(g.final_color);
        Vec4::new(c.x * g.final_overbright, c.y * g.final_overbright, c.z * g.final_overbright, c.w)
    };
    let stop = if sys.stop_ms != 0 { Some(sys.stop_ms as f32 * 0.001) } else { None };

    for index in 0..total {
        let p = &mut em.particles[index as usize];
        if reset {
            p.cycle = -1;
            p.life = -1.0;
        }
        let age = elapsed - index as f32 * interval;
        if age < 0.0 {
            continue;
        }
        let cycle = (age / period) as i32;
        let lifetime = age - cycle as f32 * period;
        if lifetime > life_max || (g.cycles > 0 && cycle >= g.cycles) {
            continue;
        }
        if cycle != p.cycle {
            // EmitParticle
            let r = fetch_random(table, index, seed);
            let lp = distribute(g.dist_type, r) * g.dist_scale + g.dist_offset;
            let lv = distribute(g.vel_type, r) * g.vel_scale + g.vel_offset;
            let mut pos = f.axis.to_parent(lp) + f.origin;
            let mut vel = f.axis.to_parent(lv);
            pos += vel * lifetime + accel * (lifetime * lifetime * 0.5);
            vel += accel * lifetime;
            *p = GpuParticle { cycle, pos, vel, max_life: life_min + (life_max - life_min) * r.w, life: p.life };
        }
        if lifetime > p.max_life {
            continue;
        }
        // SimulateParticle (no vector fields or collision): exact constant-acceleration step.
        p.vel += accel * dt;
        p.pos += (p.vel - accel * dt * 0.5) * dt;
        p.life = lifetime;
        if let Some(stop) = stop {
            if sys.start + g.time_offset + cycle as f32 * period + index as f32 * interval >= stop {
                continue;
            }
        }
        out.push(gpu_quad(si, g, index, p, table, f, initial, fin, (rate_min, rate_max)));
    }
}

/// gpuparticlerender's vertex program for one committed particle.
#[allow(clippy::too_many_arguments)]
fn gpu_quad(si: usize, g: &crate::particle::GpuStage, index: i32, p: &GpuParticle, table: &[Vec4], f: &Frame, initial: Vec4, fin: Vec4, rate: (f32, f32)) -> Quad {
    let life_n = p.life / p.max_life;
    // "we don't want to use the exact same index that was used for position"
    let r = fetch_random(table, index + 1, Vec4::ZERO);
    let mut size = g.size_initial + (g.size_final - g.size_initial) * life_n;
    size += size * g.size_variation * (r.x * 2.0 - 1.0);

    let (right, up) = (f.view.right, f.view.up);
    let corner: Box<dyn Fn(f32, f32) -> Vec3> = if g.orientation == 1 {
        // POR_GPU_TRAIL: x along the view-space velocity (unnormalised), y along its perpendicular.
        let vv = glam::Vec2::new(p.vel.dot(right), p.vel.dot(up));
        let t = glam::Vec2::new(-vv.y, vv.x).normalize_or_zero();
        Box::new(move |qx: f32, qy: f32| {
            let o = vv * (qx * size.x) + t * (qy * size.y);
            right * o.x + up * o.y
        })
    } else {
        Box::new(move |qx: f32, qy: f32| right * (qx * size.x) + up * (qy * size.y))
    };

    let fade_out_time = 1.0 - g.fade_out;
    let fade_blend = if life_n < g.fade_in {
        1.0 - life_n / g.fade_in
    } else if life_n > fade_out_time {
        (life_n - fade_out_time) / g.fade_out
    } else {
        0.0
    };
    let color = initial + (fin - initial) * life_n;
    let color = color + (g.fade_color - color) * fade_blend;

    let random_row = g.random_row;
    let (rows, cols) = (g.rows as f32, g.columns as f32);
    let frames = if random_row { cols } else { cols * rows };
    let mut frame = 0.0;
    if frames > 1.0 {
        let start = g.start_frame as f32;
        frame = if start >= 0.0 { start } else { r.z * frames };
        match g.anim_type {
            0 => frame += p.life * (rate.0 + (rate.1 - rate.0) * r.y),
            1 => {
                frame += p.life * (rate.0 + (rate.1 - rate.0) * r.y);
                frame = frame.min(frames - 1.0);
            }
            _ => frame += life_n * frames,
        }
    }
    // quadVertices (-1,-1) (-1,1) (1,1) (1,-1), laid out here as the CPU path's quads: v0 v1 across, v2 below v0.
    let mut verts = [Vertex::default(); 4];
    for (v, (qx, qy)) in verts.iter_mut().zip([(-1.0f32, 1.0f32), (1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)]) {
        let tc = [qx * 0.5 + 0.5, qy * -0.5 + 0.5];
        let (uv, uv_next, frac) = if frames > 1.0 {
            let t0 = animate_tc(tc, frame, frames, cols, rows, random_row, r.w);
            if g.frame_blending { (t0, animate_tc(tc, frame + 1.0, frames, cols, rows, random_row, r.w), frame - frame.floor()) } else { (t0, t0, 0.0) }
        } else {
            (tc, tc, 0.0)
        };
        *v = Vertex { pos: p.pos + corner(qx, qy), uv, uv_next, frame_frac: frac, alpha_scale: 1.0, color_f: Some(color.to_array()), ..Default::default() };
    }
    Quad { stage: si, verts }
}

#[allow(dead_code)]
fn _unused() -> f32 {
    RANDOM_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_random_table_starts_from_seed_666() {
        let t = gpu_table();
        assert_eq!(t.len(), GPU_RANDOM);
        let mut r = IdRandom(666);
        let first = Vec4::new(r.random_float(), r.random_float(), r.random_float(), r.random_float());
        assert_eq!(t[0], first);
        // FetchRandom wraps with a 0.1234 shift per table period and stays in [0, 1).
        let a = fetch_random(t, 5, Vec4::ZERO);
        let b = fetch_random(t, 5 + GPU_RANDOM as i32, Vec4::ZERO);
        let e = t[5] + Vec4::splat(0.1234);
        assert_eq!(b, e - e.floor());
        assert!(a.max_element() < 1.0 && a.min_element() >= 0.0);
    }

    #[test]
    fn simple_parm_ranges() {
        let c = crate::parm::SimpleParm { val0: 0.05, val1: 0.05, variance: 0.6, minmax: false };
        let (lo, hi) = c.range();
        assert!((lo - 0.02).abs() < 1e-6 && (hi - 0.08).abs() < 1e-6);
        let m = crate::parm::SimpleParm { val0: 1.0, val1: 3.0, variance: 0.0, minmax: true };
        assert_eq!(m.range(), (1.0, 3.0));
    }
}
