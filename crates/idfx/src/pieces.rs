//! Breakable pieces thrown by idPieceEmitter (shell casings): idDeclBreakable, idEffectPhysicsProperties,
//! the idEffectPhysicsRigidBody step and idEffectPhysicsPieceEmitter (spawn ring, life, collision decay).
//!
//! - decl parse 0x141997d10, ctor defaults 0x141996490; properties FUN_1419df0a0 + 0x1417a4440.
//! - pieces built by 0x1417a38b0 (one rigid body per model piece, trace model shrunk by 0.1875).
//! - emit 0x14097f050 -> spawn 0x1417a34b0 -> ApplyImpulse 0x1418e7e70.
//! - per frame 0x1417a4530 -> evaluate 0x1418e9b70: trace the previous step's motion, collision impulse
//!   0x1418e8510, velocity clamp 0x1418e7f40, derivatives 0x1418e9470, next motion 0x141650280.
//!
//! Matrices are idTech row-major [row][col]; orientations are rows = body axes in world space.

use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::Container;
use idres::decldb::DeclDb;

use crate::dmodel::{DModel, TraceModel};
use crate::{Axis, IdRandom, RANDOM_SCALE, TWO_PI};

pub type M3 = [[f32; 3]; 3];

fn m_identity() -> M3 {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

fn m_inverse(m: &M3) -> Option<M3> {
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let det = m[0][0] * c(1, 2, 1, 2) - m[0][1] * c(1, 2, 0, 2) + m[0][2] * c(1, 2, 0, 1);
    if det.abs() < 1e-30 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [c(1, 2, 1, 2) * inv, -c(0, 2, 1, 2) * inv, c(0, 1, 1, 2) * inv],
        [-c(1, 2, 0, 2) * inv, c(0, 2, 0, 2) * inv, -c(0, 1, 0, 2) * inv],
        [c(1, 2, 0, 1) * inv, -c(0, 2, 0, 1) * inv, c(0, 1, 0, 1) * inv],
    ])
}

/// x86 rsqrtss refined by two Newton steps as the engine writes it, with the FLT_MIN floor (_DAT_144144b50).
fn inv_len(len2: f32) -> f32 {
    let x = len2.max(f32::MIN_POSITIVE);
    let mut y = 1.0 / x.sqrt();
    y = (x * y * y - 3.0) * y * -0.5;
    y = (x * y * y - 3.0) * y * -0.5;
    y
}

/// idDeclBreakable (`generated/decls/breakable/<name>.decl`, `key value` lines). Offsets in the decl.
#[derive(Debug, Clone)]
pub struct BreakableDecl {
    pub name: String,
    pub model: String,
    pub linear_friction: f32,
    pub angular_friction: f32,
    pub contact_friction: f32,
    pub linear_friction_water: f32,
    pub angular_friction_water: f32,
    pub bouncyness: f32,
    pub gravity: Vec3,
    pub world_collision_only: bool,
    pub simple_point_collision: bool,
    pub crazy_bounce_chance: f32,
    pub max_simulation_time: f32,
    pub stop_speed: f32,
    pub max_linear_velocity: f32,
    pub max_angular_velocity: f32,
    pub clip_mask: u32,
    pub dampening_decay: f32,
}

impl BreakableDecl {
    /// Ctor 0x141996490 defaults: frictions 0, bouncyness 1, gravity (0 0 -250), maxSimulationTime 5,
    /// stopSpeed 10, maxLinearVelocity 5000, maxAngularVelocity pi*4, clipMask 0x100141.
    pub fn defaults(name: &str) -> BreakableDecl {
        BreakableDecl {
            name: name.to_string(),
            model: String::new(),
            linear_friction: 0.0,
            angular_friction: 0.0,
            contact_friction: 0.0,
            linear_friction_water: 0.0,
            angular_friction_water: 0.0,
            bouncyness: 1.0,
            gravity: Vec3::new(0.0, 0.0, -250.0),
            world_collision_only: false,
            simple_point_collision: false,
            crazy_bounce_chance: 0.0,
            max_simulation_time: 5.0,
            stop_speed: 10.0,
            max_linear_velocity: 5000.0,
            max_angular_velocity: std::f32::consts::PI * 4.0,
            clip_mask: 0x100141,
            dampening_decay: 0.0,
        }
    }

    pub fn parse(name: &str, text: &str) -> BreakableDecl {
        let mut d = Self::defaults(name);
        let toks: Vec<&str> = text.split(|c: char| c.is_whitespace() || c == '{' || c == '}').filter(|t| !t.is_empty()).collect();
        let f = |i: usize| toks.get(i).and_then(|t| t.parse::<f32>().ok());
        let mut i = 0;
        while i < toks.len() {
            let k = toks[i].to_ascii_lowercase();
            let mut used = 1;
            match k.as_str() {
                "model" => {
                    d.model = toks.get(i + 1).unwrap_or(&"").trim_matches('"').to_string();
                    used = 2;
                }
                "linearfriction" => (d.linear_friction, used) = (f(i + 1).unwrap_or(d.linear_friction), 2),
                "angularfriction" => (d.angular_friction, used) = (f(i + 1).unwrap_or(d.angular_friction), 2),
                "contactfriction" => (d.contact_friction, used) = (f(i + 1).unwrap_or(d.contact_friction), 2),
                "linearfrictionwater" => (d.linear_friction_water, used) = (f(i + 1).unwrap_or(d.linear_friction_water), 2),
                "angularfrictionwater" => (d.angular_friction_water, used) = (f(i + 1).unwrap_or(d.angular_friction_water), 2),
                "bouncyness" => (d.bouncyness, used) = (f(i + 1).unwrap_or(d.bouncyness), 2),
                "dampeningdecay" => (d.dampening_decay, used) = (f(i + 1).unwrap_or(d.dampening_decay), 2),
                "gravity" => {
                    if let (Some(x), Some(y), Some(z)) = (f(i + 1), f(i + 2), f(i + 3)) {
                        d.gravity = Vec3::new(x, y, z);
                    }
                    used = 4;
                }
                "worldcollisiononly" => (d.world_collision_only, used) = (f(i + 1).map(|v| v != 0.0).unwrap_or(false), 2),
                "simplepointcollision" => (d.simple_point_collision, used) = (f(i + 1).map(|v| v != 0.0).unwrap_or(false), 2),
                "crazybouncechance" => (d.crazy_bounce_chance, used) = (f(i + 1).unwrap_or(d.crazy_bounce_chance), 2),
                "maxsimulationtime" => (d.max_simulation_time, used) = (f(i + 1).unwrap_or(d.max_simulation_time), 2),
                "stopspeed" => (d.stop_speed, used) = (f(i + 1).unwrap_or(d.stop_speed), 2),
                "maxlinearvelocity" => (d.max_linear_velocity, used) = (f(i + 1).unwrap_or(d.max_linear_velocity), 2),
                "maxangularvelocity" => (d.max_angular_velocity, used) = (f(i + 1).unwrap_or(d.max_angular_velocity), 2),
                _ => {}
            }
            i += used;
        }
        d
    }

    pub fn load(c: &Container, name: &str) -> Result<BreakableDecl> {
        let path = format!("generated/decls/breakable/{name}.decl");
        let text = String::from_utf8_lossy(&c.read_by_name(&path).with_context(|| path.clone())?).into_owned();
        Ok(Self::parse(name, &text))
    }
}

/// idPieceEmitter edit fields (entityDef), ctor 0x14097eaa0 defaults: pieceLifeSpan 0,
/// pieceAngularVelocity 50, pieceFriction 15, pieceMinBounceVelocity 40, decays 0.
#[derive(Debug, Clone)]
pub struct PieceEmitterDef {
    pub name: String,
    /// renderModelInfo.model, `<breakable decl>.break`.
    pub breakable: String,
    pub life_span: i32,
    pub angular_velocity: f32,
    pub friction: f32,
    pub min_bounce_velocity: f32,
    pub collision_age_decay: f32,
    pub collision_volume_decay: f32,
    pub impact_sound_table: String,
}

impl PieceEmitterDef {
    pub fn load(db: &DeclDb, name: &str) -> Result<PieceEmitterDef> {
        let b = db.get("entitydef", name).with_context(|| format!("entityDef {name}"))?;
        let e = b.block("edit");
        let f = |k: &str, d: f32| e.and_then(|e| e.f32(k)).unwrap_or(d);
        let model = e.and_then(|e| e.str("renderModelInfo.model")).unwrap_or("").to_string();
        Ok(PieceEmitterDef {
            name: name.to_string(),
            breakable: model.strip_suffix(".break").unwrap_or(&model).to_string(),
            life_span: f("pieceLifeSpan", 0.0) as i32,
            angular_velocity: f("pieceAngularVelocity", 50.0),
            friction: f("pieceFriction", 15.0),
            min_bounce_velocity: f("pieceMinBounceVelocity", 40.0),
            collision_age_decay: f("pieceCollisionAgeDecay", 0.0),
            collision_volume_decay: f("pieceCollisionVolumeDecay", 0.0),
            impact_sound_table: e.and_then(|e| e.str("impactSoundTable")).unwrap_or("").to_string(),
        })
    }
}

/// idEffectPhysicsProperties (0xa8 bytes).
#[derive(Debug, Clone)]
pub struct PieceProps {
    pub clip_mask: u32,
    pub linear_friction: f32,
    pub angular_friction: f32,
    pub contact_friction: f32,
    pub linear_friction_water: f32,
    pub angular_friction_water: f32,
    pub bouncyness: f32,
    pub gravity: Vec3,
    pub world_collision_only: bool,
    pub simple_point_collision: bool,
    pub crazy_bounce_chance: f32,
    pub mass: f32,
    pub inverse_mass: f32,
    pub center_of_mass: Vec3,
    pub inertia: M3,
    pub inverse_inertia: M3,
    pub stop_speed: f32,
    pub max_linear_velocity: f32,
    pub max_angular_velocity: f32,
}

impl PieceProps {
    /// FUN_1419df0a0(props, trace model, mass) + 0x1417a4440 (decl copies).
    pub fn new(decl: &BreakableDecl, trm: &TraceModel, mass: f32) -> PieceProps {
        let (tm_mass, com, it) = trm.mass_properties();
        let (mut m, com, mut i) = if tm_mass <= 0.0 || tm_mass.is_nan() {
            (1.0, Vec3::ZERO, m_identity())
        } else {
            (tm_mass, com, [[it[0].x, it[0].y, it[0].z], [it[1].x, it[1].y, it[1].z], [it[2].x, it[2].y, it[2].z]])
        };
        let s = mass / m;
        for r in &mut i {
            for v in r.iter_mut() {
                *v *= s;
            }
        }
        m = mass;
        // Inertia conditioning: when a principal moment exceeds 4x the smallest, the other two columns are
        // scaled so their diagonals become exactly 4x the smallest.
        let d = [i[0][0], i[1][1], i[2][2]];
        let min = if d[1] <= d[0] {
            if d[2] <= d[1] { 2 } else { 1 }
        } else if d[0] < d[2] {
            0
        } else {
            2
        };
        let ratios = [d[0] / d[min], d[1] / d[min], d[2] / d[min]];
        if ratios.iter().any(|&r| 4.0 < r) {
            let mut sc = ratios;
            for k in [(min + 1) % 3, (min + 2) % 3] {
                sc[k] = d[min] * 4.0 / d[k];
            }
            // I * diag(sc) with the min axis keeping its own ratio (1).
            sc[min] = ratios[min];
            for r in &mut i {
                for (c, v) in r.iter_mut().enumerate() {
                    *v *= sc[c];
                }
            }
        }
        let mut inv = m_inverse(&i).unwrap_or(m_identity());
        for r in &mut inv {
            for v in r.iter_mut() {
                *v *= 0.166_666_67;
            }
        }
        PieceProps {
            clip_mask: decl.clip_mask,
            linear_friction: decl.linear_friction,
            angular_friction: decl.angular_friction,
            contact_friction: decl.contact_friction,
            linear_friction_water: decl.linear_friction_water,
            angular_friction_water: decl.angular_friction_water,
            bouncyness: decl.bouncyness,
            gravity: decl.gravity,
            world_collision_only: decl.world_collision_only,
            simple_point_collision: decl.simple_point_collision,
            crazy_bounce_chance: decl.crazy_bounce_chance,
            mass: m,
            inverse_mass: 1.0 / m,
            center_of_mass: com,
            inertia: i,
            inverse_inertia: inv,
            stop_speed: decl.stop_speed,
            max_linear_velocity: decl.max_linear_velocity,
            max_angular_velocity: decl.max_angular_velocity,
        }
    }

    /// World inverse inertia as the engine forms it: R^T * Iinv^T * R (R rows = body axes).
    fn world_inverse_inertia(&self, r: &Axis) -> M3 {
        let rm = [r.x.to_array(), r.y.to_array(), r.z.to_array()];
        let ii = &self.inverse_inertia;
        let mut a = [[0.0f32; 3]; 3];
        for (row, ar) in a.iter_mut().enumerate() {
            for (j, v) in ar.iter_mut().enumerate() {
                *v = (0..3).map(|k| ii[k][row] * rm[k][j]).sum();
            }
        }
        let mut b = [[0.0f32; 3]; 3];
        for (row, br) in b.iter_mut().enumerate() {
            for (j, v) in br.iter_mut().enumerate() {
                *v = (0..3).map(|k| rm[k][row] * a[k][j]).sum();
            }
        }
        b
    }
}

fn m_mul_v(m: &M3, v: Vec3) -> Vec3 {
    Vec3::new(m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z, m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z, m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z)
}
fn m_t_mul_v(m: &M3, v: Vec3) -> Vec3 {
    Vec3::new(m[0][0] * v.x + m[1][0] * v.y + m[2][0] * v.z, m[0][1] * v.x + m[1][1] * v.y + m[2][1] * v.z, m[0][2] * v.x + m[1][2] * v.y + m[2][2] * v.z)
}

/// A collision-world point trace (the SPObject query for simplePointCollision pieces).
#[derive(Debug, Clone, Copy)]
pub struct PieceTrace {
    pub fraction: f32,
    pub endpos: Vec3,
    pub point: Vec3,
    pub normal: Vec3,
    pub surface: i32,
}

pub trait PieceWorld {
    /// Moves a point from `start` to `end` against the world (contents `clip_mask`).
    fn trace_point(&self, start: Vec3, end: Vec3, clip_mask: u32) -> PieceTrace;
}

/// idSPObjectMotion: the move the next query performs.
#[derive(Debug, Clone, Copy)]
pub struct Motion {
    pub translation: Vec3,
    pub rotation_vec: Vec3,
    /// Degrees, sign as the derivative step writes it (negative = right-handed about rotation_vec).
    pub rotation_angle: f32,
}

/// idEffectPhysicsCollision.
#[derive(Debug, Clone, Copy, Default)]
pub struct Collision {
    pub point: Vec3,
    pub normal_velocity: Vec3,
    pub surface: i32,
}

/// idEffectPhysicsRigidBody.
#[derive(Debug, Clone)]
pub struct RigidBody {
    pub props: Arc<PieceProps>,
    pub position: Vec3,
    pub orientation: Axis,
    pub linear_momentum: Vec3,
    pub angular_momentum: Vec3,
    pub external_force: Vec3,
    pub external_torque: Vec3,
    pub motion: Option<Motion>,
    pub active: bool,
    pub settled: bool,
}

/// The global effect-physics random (0x1458de984, bss: starts at 0) used by crazy bounces.
#[derive(Debug, Clone, Default)]
pub struct EffectRandom(pub u32);

impl EffectRandom {
    fn step(&mut self) -> u32 {
        self.0 = IdRandom::step(self.0);
        self.0
    }
}

fn rf(s: u32) -> f32 {
    ((s >> 10) & 0x7fff) as f32 * RANDOM_SCALE
}

/// Rotates `v` about the unit `axis` by `angle` radians (FUN_1402eda80 as used for crazy bounces).
fn rotate(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}

impl RigidBody {
    pub fn new(props: Arc<PieceProps>) -> RigidBody {
        RigidBody {
            props,
            position: Vec3::ZERO,
            orientation: Axis::IDENTITY,
            linear_momentum: Vec3::ZERO,
            angular_momentum: Vec3::ZERO,
            external_force: Vec3::ZERO,
            external_torque: Vec3::ZERO,
            motion: None,
            active: false,
            settled: false,
        }
    }

    /// ApplyImpulse 0x1418e7e70.
    pub fn apply_impulse(&mut self, point: Vec3, impulse: Vec3) {
        self.settled = false;
        self.linear_momentum += impulse;
        self.angular_momentum += (point - self.position).cross(impulse);
    }

    fn angular_velocity(&self) -> Vec3 {
        m_mul_v(&self.props.world_inverse_inertia(&self.orientation), self.angular_momentum)
    }

    /// 0x1418e7f40: clamp linear and angular velocity by scaling the momenta.
    fn clamp_velocities(&mut self) {
        let p = &self.props;
        let v = self.linear_momentum * p.inverse_mass;
        let l2 = v.length_squared();
        if p.max_linear_velocity * p.max_linear_velocity < l2 {
            self.linear_momentum *= inv_len(l2) * p.max_linear_velocity;
        }
        let w = self.angular_velocity();
        let a2 = w.length_squared();
        if p.max_angular_velocity * p.max_angular_velocity < a2 {
            self.angular_momentum *= inv_len(a2) * p.max_angular_velocity;
        }
    }

    /// 0x1418e8510 against the static world. Returns the collision normal velocity.
    fn collision_impulse(&mut self, tr: &PieceTrace, decay: f32, rng: &mut EffectRandom) -> Vec3 {
        let p = self.props.clone();
        let tiny = |x: f32| if x.abs() <= 1e-18 { 0.0 } else { x };
        let v = Vec3::new(tiny(p.inverse_mass * self.linear_momentum.x), tiny(p.inverse_mass * self.linear_momentum.y), tiny(p.inverse_mass * self.linear_momentum.z));
        let b = p.world_inverse_inertia(&self.orientation);
        let w = m_mul_v(&b, self.angular_momentum);
        let mut n = tr.normal;
        if p.crazy_bounce_chance > 0.0 && p.crazy_bounce_chance >= rf(rng.step()) {
            let s1 = IdRandom::step(rng.0);
            let (s2, s3, s4) = {
                let a = IdRandom::step(s1);
                let b = IdRandom::step(a);
                (a, b, IdRandom::step(b))
            };
            let c = |s: u32| rf(s) * 2.0 - 1.0;
            let up = |s: u32| rf(s) * 0.9 + 0.1;
            let axis = match ((s1 >> 10) & 0x7fff) % 3 {
                0 => Vec3::new(up(s4), c(s3), c(s2)),
                1 => Vec3::new(c(s4), up(s3), c(s2)),
                _ => Vec3::new(c(s4), c(s3), up(s2)),
            };
            rng.0 = IdRandom::step(s4);
            let axis = axis * inv_len(axis.length_squared());
            let angle = (rf(rng.0) * 2.0 - 1.0) * 0.785_398_2 * (1.0 - decay);
            n = rotate(n, axis, angle);
        }
        let r = tr.point - self.position;
        let rel = w.cross(r) + v;
        let vn = -(n.dot(rel));
        let normal_velocity = n * vn;
        let num = if p.stop_speed <= vn { ((1.0 - decay) * p.bouncyness + 1.0) * vn } else { p.stop_speed * 0.5 };
        let mut denom = p.inverse_mass;
        if !p.simple_point_collision {
            let c = m_mul_v(&b, r.cross(n));
            denom += n.dot(c.cross(r));
        }
        let jn = num / denom;
        let t = -(rel + normal_velocity);
        let t2 = t.length_squared();
        let il = inv_len(t2);
        let th = t * il;
        let mut denom_t = p.inverse_mass;
        if !p.simple_point_collision {
            let c = m_mul_v(&b, r.cross(th));
            denom_t += th.dot(c.cross(r));
        }
        let jt = (il * t2 * p.contact_friction) / denom_t;
        let impulse = n * jn + th * jt;
        let keep = 1.0 - decay;
        self.linear_momentum += impulse;
        self.angular_momentum = (self.angular_momentum + r.cross(impulse)) * keep;
        if tr.fraction < 0.0001 {
            self.linear_momentum *= 0.5;
            self.angular_momentum *= 0.5;
        }
        normal_velocity
    }

    /// 0x1418e9b70: one step of `dt` seconds with collision decay `decay`.
    pub fn evaluate(&mut self, dt: f32, decay: f32, world: &dyn PieceWorld, rng: &mut EffectRandom) -> Collision {
        let mut col = Collision::default();
        if !self.active || self.settled {
            return col;
        }
        let mut maybe_rest = false;
        if let Some(m) = self.motion.take() {
            let start = self.position;
            let tr = world.trace_point(start, start + m.translation, self.props.clip_mask);
            self.position = tr.endpos;
            // INTERIM: the async query's rotation handling is not decoded; like idClip::Motion the
            // rotation is applied only when the translation completes (a point cannot collide rotating).
            if tr.fraction >= 1.0 && m.rotation_angle != 0.0 {
                let ang = -m.rotation_angle.to_radians();
                let a = self.orientation;
                self.orientation = Axis { x: rotate(a.x, m.rotation_vec, ang), y: rotate(a.y, m.rotation_vec, ang), z: rotate(a.z, m.rotation_vec, ang) };
                self.orientation = orthonormalize(self.orientation);
            }
            if tr.fraction < 1.0 {
                col.normal_velocity = self.collision_impulse(&tr, decay, rng);
                col.point = tr.point;
                col.surface = tr.surface;
                let pxy2 = self.linear_momentum.x * self.linear_momentum.x + self.linear_momentum.y * self.linear_momentum.y;
                let len = inv_len(pxy2) * pxy2;
                if len <= 10.0 && self.linear_momentum.z < 0.0 {
                    if 2.0 <= len {
                        maybe_rest = true;
                    } else {
                        self.settled = true;
                    }
                }
            }
        }
        self.clamp_velocities();
        // Derivatives 0x1418e9470.
        let p = self.props.clone();
        let translation = self.linear_momentum * p.inverse_mass * dt;
        let w = self.angular_velocity();
        let w2 = w.length_squared();
        let il = inv_len(w2);
        let angle = -(il * w2 * 57.295_776) * dt;
        let rotation_vec = if angle != 0.0 { w * il } else { Vec3::Z };
        let lin_fr = p.linear_friction_water * 0.0 + p.linear_friction;
        let ang_fr = p.angular_friction_water * 0.0 + p.angular_friction;
        let dp = ((self.external_force - self.linear_momentum * lin_fr) + p.gravity * p.mass) * dt;
        let dl = (self.external_torque - self.angular_momentum * ang_fr) * dt;
        self.motion = Some(Motion { translation, rotation_vec, rotation_angle: angle });
        if maybe_rest && translation.x * translation.x + translation.y * translation.y < 0.0001 {
            self.settled = true;
        }
        if !self.settled {
            self.linear_momentum += dp;
            self.angular_momentum += dl;
        } else {
            self.motion = None;
            self.linear_momentum = Vec3::ZERO;
            self.angular_momentum = Vec3::ZERO;
            col = Collision::default();
        }
        self.external_force = Vec3::ZERO;
        self.external_torque = Vec3::ZERO;
        col
    }
}

/// FUN_1403d8360 stand-in: Gram-Schmidt keeping x.
fn orthonormalize(a: Axis) -> Axis {
    let x = a.x.normalize_or(Vec3::X);
    let y = (a.y - x * x.dot(a.y)).normalize_or(Vec3::Y);
    let z = x.cross(y);
    Axis { x, y, z }
}

/// idEffectPhysicsPieceEmitter::idBreakablePiece.
#[derive(Debug, Clone)]
pub struct Piece {
    pub body: RigidBody,
    pub collision: Collision,
    pub emit_time: i32,
    pub first_collision_time: i32,
    /// Rest pose of this piece in the model (joint matrix): model point = rest * local.
    pub rest: crate::dmodel::JointMat,
}

/// idEffectPhysicsPieceEmitter plus the idPieceEmitter emit math.
#[derive(Debug, Clone)]
pub struct PieceEmitter {
    pub def: PieceEmitterDef,
    pub decl: Arc<BreakableDecl>,
    pub model: Arc<DModel>,
    pub pieces: Vec<Piece>,
    pub piece_index: usize,
    pub in_use: i32,
    /// pieceMass: idEffectPhysicsPieceEmitter ctor 0x1417a29e0 sets 10 (not an editable field).
    pub piece_mass: f32,
    pub rng: EffectRandom,
}

/// idTraceModel::Shrink amount the piece builder applies (0x1417a38b0: 0x3e400000).
pub const PIECE_SHRINK: f32 = 0.1875;
pub const PIECE_MASS: f32 = 10.0;

impl PieceEmitter {
    pub fn load(db: &DeclDb, entity_def: &str) -> Result<PieceEmitter> {
        let def = PieceEmitterDef::load(db, entity_def)?;
        let decl = Arc::new(BreakableDecl::load(db.container(), &def.breakable)?);
        let model = Arc::new(DModel::load(db.container(), &decl.model)?);
        Ok(Self::new(def, decl, model))
    }

    /// 0x1417a38b0: one rigid body per model piece at its rest pose, momentum 0.
    pub fn new(def: PieceEmitterDef, decl: Arc<BreakableDecl>, model: Arc<DModel>) -> PieceEmitter {
        let pieces = model
            .trace_models
            .iter()
            .enumerate()
            .map(|(i, trm)| {
                let mut t = trm.clone();
                t.shrink(PIECE_SHRINK);
                let props = Arc::new(PieceProps::new(&decl, &t, PIECE_MASS));
                let rest = model.transforms.get(i).copied().unwrap_or(crate::dmodel::JointMat { rows: [Vec3::X, Vec3::Y, Vec3::Z], t: Vec3::ZERO });
                let mut body = RigidBody::new(props);
                body.position = rest.t;
                body.orientation = Axis { x: rest.rows[0], y: rest.rows[1], z: rest.rows[2] };
                Piece { body, collision: Collision::default(), emit_time: 0, first_collision_time: 0, rest }
            })
            .collect();
        PieceEmitter { def, decl, model, pieces, piece_index: 0, in_use: 0, piece_mass: PIECE_MASS, rng: EffectRandom::default() }
    }

    /// idPieceEmitter emit 0x14097f050 with the game random: speed = r*delta + base, a cone of
    /// `delta_angle` degrees around axis.x (roll r*delta_angle*2pi), plus `base_velocity`; the impulse
    /// (velocity * pieceMass) hits a random point within the model's maxRadius of the origin.
    #[allow(clippy::too_many_arguments)]
    pub fn emit(&mut self, time_ms: i32, rng: &mut IdRandom, origin: Vec3, axis: &Axis, base_velocity: Vec3, base_speed: f32, delta_speed: f32, delta_angle: f32) {
        if self.pieces.is_empty() {
            return;
        }
        let speed = rng.random_float() * delta_speed + base_speed;
        let a = rng.random_float() * delta_angle * TWO_PI;
        let (sa, ca) = (a.sin(), a.cos());
        let pch = rng.random_float() * delta_angle * crate::DEG2RAD;
        let (sp, cp) = (pch.sin(), pch.cos());
        let dir = (axis.y * ca + axis.z * sa) * sp + axis.x * cp;
        let impulse = (dir * speed + base_velocity) * self.piece_mass;
        let (r4, r5, r6) = (rng.random_float(), rng.random_float(), rng.random_float());
        let rad = self.model.max_radius;
        let offset = Vec3::new((r6 + r6 - 1.0) * rad, (r5 + r5 - 1.0) * rad, (r4 + r4 - 1.0) * rad);
        // Spawn 0x1417a34b0.
        let piece = &mut self.pieces[self.piece_index];
        if !piece.body.active {
            self.in_use += 1;
        }
        piece.emit_time = time_ms;
        piece.first_collision_time = 0;
        piece.body.position = origin;
        piece.body.orientation = *axis;
        piece.body.motion = None;
        piece.body.linear_momentum = Vec3::ZERO;
        piece.body.angular_momentum = Vec3::ZERO;
        piece.body.apply_impulse(origin + offset, impulse);
        piece.body.active = true;
        self.piece_index = if self.piece_index + 1 < self.pieces.len() { self.piece_index + 1 } else { 0 };
    }

    /// 0x1417a4530: advance every live piece (`frame_ms` = this game frame's length).
    pub fn update(&mut self, time_ms: i32, frame_ms: i32, world: &dyn PieceWorld) {
        let dt = frame_ms as f32 * 0.001;
        let life = self.def.life_span;
        for piece in &mut self.pieces {
            if !piece.body.active {
                continue;
            }
            if time_ms >= piece.emit_time + life {
                piece.emit_time = 0;
                piece.first_collision_time = 0;
                piece.body.active = false;
                self.in_use -= 1;
                continue;
            }
            let mut decay = 0.0f32;
            if piece.first_collision_time > 0 && life as f32 > 0.0 {
                decay = (((time_ms as f32) - piece.first_collision_time as f32) * self.def.collision_age_decay / life as f32).clamp(0.0, 1.0);
            }
            piece.collision = piece.body.evaluate(dt, decay, world, &mut self.rng);
            if !piece.body.settled && piece.collision.normal_velocity != Vec3::ZERO {
                if piece.first_collision_time == 0 {
                    piece.first_collision_time = time_ms;
                }
                // Impact sounds (pieceMinBounceVelocity, impactSoundTable) are not played here.
                // Collision-frame angular damping: w = Iinv^T L (body inverse inertia), |w| -= dt*pieceFriction,
                // L = I^T w.
                let p = &piece.body.props;
                let w = m_t_mul_v(&p.inverse_inertia, piece.body.angular_momentum);
                if w != Vec3::ZERO {
                    let w2 = w.length_squared();
                    let il = inv_len(w2);
                    let len = (il * w2 - dt * self.def.friction).max(0.0);
                    piece.body.angular_momentum = m_t_mul_v(&p.inertia, w * il * len);
                }
            }
        }
    }

    /// World position of a model-space point of piece `i` (vertex skinning by the piece's pose).
    pub fn piece_point(&self, i: usize, model_point: Vec3) -> Vec3 {
        let p = &self.pieces[i];
        let r = &p.rest;
        let d = model_point - r.t;
        // local = R^T (model - t)
        let local = r.rows[0] * d.x + r.rows[1] * d.y + r.rows[2] * d.z;
        p.body.position + p.body.orientation.to_parent(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_trm(h: Vec3) -> TraceModel {
        let v = |x: f32, y: f32, z: f32| Vec3::new(x * h.x, y * h.y, z * h.z);
        let verts = vec![v(-1., 1., -1.), v(1., 1., -1.), v(1., -1., -1.), v(-1., -1., -1.), v(-1., -1., 1.), v(-1., 1., 1.), v(1., -1., 1.), v(1., 1., 1.)];
        let mut edges: Vec<[u16; 2]> = vec![[0, 1], [1, 2], [2, 3], [3, 0], [3, 4], [4, 5], [5, 0], [4, 6], [6, 7], [7, 5], [1, 7], [6, 2]];
        edges.resize(32, [0, 0]);
        let poly = |n: Vec3, d: f32, e: &[(usize, usize)]| crate::dmodel::TrmPoly { normal: n, w: -d, edges: e.to_vec() };
        let polys = vec![
            poly(-Vec3::Z, h.z, &[(0, 0), (1, 0), (2, 0), (3, 0)]),
            poly(-Vec3::X, h.x, &[(4, 0), (5, 0), (6, 0), (3, 1)]),
            poly(Vec3::Z, h.z, &[(5, 1), (7, 0), (8, 0), (9, 0)]),
            poly(Vec3::X, h.x, &[(10, 0), (8, 1), (11, 0), (1, 1)]),
            poly(-Vec3::Y, h.y, &[(11, 1), (7, 1), (4, 1), (2, 1)]),
            poly(Vec3::Y, h.y, &[(6, 1), (9, 1), (10, 1), (0, 1)]),
        ];
        TraceModel { kind: 10, verts, edges, polys, max_poly_edges: 4, offset: Vec3::ZERO, bounds: [-h, h], radius: h.x, convex: true }
    }

    #[test]
    fn box_shrink_and_mass() {
        let mut t = box_trm(Vec3::new(0.44, 1.888, 0.5));
        t.shrink(0.1875);
        for v in &t.verts {
            assert!((v.x.abs() - 0.2525).abs() < 1e-4 && (v.y.abs() - 1.7005).abs() < 1e-4 && (v.z.abs() - 0.3125).abs() < 1e-4, "{v}");
        }
        let (vol, com, it) = t.mass_properties();
        let (a, b, c) = (0.2525f32, 1.7005f32, 0.3125f32);
        assert!((vol - 8.0 * a * b * c).abs() < 1e-4);
        assert!(com.length() < 1e-5);
        assert!((it[1].y - vol * (a * a + c * c) / 3.0).abs() < 1e-4);
        assert!(it[0].y.abs() < 1e-5);
    }

    struct Floor;
    impl PieceWorld for Floor {
        fn trace_point(&self, start: Vec3, end: Vec3, _: u32) -> PieceTrace {
            if end.z >= 0.0 || start.z < 0.0 {
                return PieceTrace { fraction: 1.0, endpos: end, point: end, normal: Vec3::Z, surface: 0 };
            }
            let f = start.z / (start.z - end.z);
            let p = start + (end - start) * f;
            PieceTrace { fraction: f, endpos: p, point: p, normal: Vec3::Z, surface: 0 }
        }
    }

    #[test]
    fn shell_falls_bounces_and_settles() {
        let mut decl = BreakableDecl::defaults("t");
        decl.gravity = Vec3::new(0.0, 0.0, -750.0);
        decl.bouncyness = 0.35;
        decl.contact_friction = 0.5;
        decl.linear_friction = 0.3;
        decl.angular_friction = 0.1;
        decl.simple_point_collision = true;
        decl.max_angular_velocity = 94.0;
        let props = Arc::new(PieceProps::new(&decl, &box_trm(Vec3::new(0.25, 1.7, 0.31)), PIECE_MASS));
        let mut b = RigidBody::new(props);
        b.position = Vec3::new(0.0, 0.0, 40.0);
        b.active = true;
        b.apply_impulse(Vec3::new(0.5, 0.0, 40.0), Vec3::new(1500.0, 0.0, 500.0));
        let mut rng = EffectRandom::default();
        let mut bounced = false;
        // As the piece emitter: decay = (t - firstCollision) * pieceCollisionAgeDecay / pieceLifeSpan.
        let (mut first, mut rest_x) = (None::<i32>, 0.0f32);
        for frame in 0..600 {
            let t = frame * 16;
            let decay = first.map_or(0.0, |f| ((t - f) as f32 * 6.0 / 10000.0).clamp(0.0, 1.0));
            let c = b.evaluate(0.016, decay, &Floor, &mut rng);
            if c.normal_velocity != Vec3::ZERO {
                bounced = true;
                first.get_or_insert(t);
            }
            assert!(b.position.z >= -1e-3, "fell through: {}", b.position);
            if frame == 500 {
                rest_x = b.position.x;
            }
        }
        assert!(bounced);
        // At 60 Hz the approach speed per step (750 * 0.016 = 12) stays above stopSpeed, so the engine's rest
        // test never fires; the piece sits on the floor instead (no horizontal drift, at the surface).
        assert!(b.position.z < 0.5 && (b.position.x - rest_x).abs() < 0.01, "still moving at {} p {}", b.position, b.linear_momentum);
    }
}
