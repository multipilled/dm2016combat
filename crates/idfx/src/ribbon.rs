//! Ribbons: `ribbon` decls (idDeclRibbon) and the engine's idRibbon simulation (nodes, helix / turbulence
//! offsets, electric-arc re-randomisation), the per-frame segment build (idRibbon render 0x141816700) and the
//! renderer's segment -> quad expansion (0x141858010). Hitscan tracers with `tracerInfo.ribbonDecls` (pistol,
//! heavy rifle, gauss rifle beams) drive them through [`RibbonSet`]. Addresses and derivations: FX.md "Ribbons".

use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decl::Block;
use idres::decldb::DeclDb;

use crate::{Axis, IdRandom, RANDOM_SCALE, TWO_PI};

/// Ring size of an idRibbon's nodes (0x78-byte nodes at +0x60).
pub const MAX_NODES: usize = 128;
const FLT_MIN: f32 = f32::MIN_POSITIVE;
const EPS: f32 = 1.192_092_9e-7;
const INV_TWO_PI: f32 = 0.159_154_94;

/// idDeclRibbon::helix_t (+0xd0).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Helix {
    pub rotation_scale: f32,
    pub radius: f32,
    pub velocity: f32,
    pub distortion0: [f32; 2],
    pub distortion1: [f32; 2],
    pub wave_offset: f32,
    pub use_length: bool,
}

/// idDeclRibbon::turbulence_t (+0xf4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Turbulence {
    pub frequency: f32,
    pub magnitude: f32,
    pub velocity: f32,
    pub magic: [f32; 4],
}

/// idDeclRibbon::ribbonVisibility_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Always,
    Normal,
    Quad,
}

/// idDeclRibbon (reflection fields; defaults from the ctor 0x141a15b90).
#[derive(Debug, Clone, PartialEq)]
pub struct RibbonDecl {
    pub name: String,
    pub material: String,
    /// Node duration (game time units, read as ms).
    pub duration: i32,
    pub max_length: f32,
    pub fade_in: f32,
    pub fade_out: f32,
    pub gravity: Vec3,
    /// Model space (node axis rows).
    pub velocity: Vec3,
    pub start_width: f32,
    pub end_width: f32,
    pub color: [f32; 4],
    pub view_oriented: bool,
    pub reorient_nodes: bool,
    pub texture_repeat: bool,
    pub texture_tangent_stretch: bool,
    pub texture_repeat_distance: f32,
    pub texture_t_min: f32,
    pub texture_t_max: f32,
    pub node_subdivision: f32,
    pub min_node_distance: f32,
    pub drag: f32,
    pub detach: bool,
    pub apply_start_variance: bool,
    pub helix: Helix,
    pub turbulence: Turbulence,
    pub electricity_time_min: f32,
    pub electricity_time_max: f32,
    pub turbulence_as_electricity: bool,
    pub anim_start_time: f32,
    pub visibility: Visibility,
}

impl Default for RibbonDecl {
    /// idDeclRibbon ctor 0x141a15b90.
    fn default() -> Self {
        RibbonDecl {
            name: String::new(),
            material: String::new(),
            duration: 0,
            max_length: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            gravity: Vec3::ZERO,
            velocity: Vec3::ZERO,
            start_width: 2.0,
            end_width: 32.0,
            color: [1.0; 4],
            view_oriented: true,
            reorient_nodes: false,
            texture_repeat: false,
            texture_tangent_stretch: true,
            texture_repeat_distance: 32.0,
            texture_t_min: 0.0,
            texture_t_max: 1.0,
            node_subdivision: 0.0,
            min_node_distance: 0.0,
            drag: 0.0,
            detach: false,
            apply_start_variance: true,
            helix: Helix::default(),
            turbulence: Turbulence { frequency: 0.0, magnitude: 0.0, velocity: 0.0, magic: [0.93, 1.0, 0.91, 0.73] },
            electricity_time_min: 0.2,
            electricity_time_max: 0.5,
            turbulence_as_electricity: false,
            anim_start_time: 0.0,
            visibility: Visibility::Always,
        }
    }
}

fn flag(b: &Block, k: &str, d: bool) -> bool {
    b.path(k).and_then(|v| v.as_bool()).unwrap_or(d)
}

fn vec3(b: &Block, k: &str, d: Vec3) -> Vec3 {
    match b.block(k) {
        Some(v) => Vec3::new(v.f32("x").unwrap_or(d.x), v.f32("y").unwrap_or(d.y), v.f32("z").unwrap_or(d.z)),
        None => d,
    }
}

impl RibbonDecl {
    /// The decl's `edit` block over the ctor defaults.
    pub fn parse(name: &str, e: &Block) -> RibbonDecl {
        let d = RibbonDecl::default();
        let f = |k: &str, v: f32| e.f32(k).unwrap_or(v);
        let c = e.block("color");
        let cf = |k: &str, v: f32| c.and_then(|c| c.f32(k)).unwrap_or(v);
        let h = e.block("helix").cloned().unwrap_or_default();
        let t = e.block("turbulence").cloned().unwrap_or_default();
        let m = t.block("magic");
        let mf = |k: &str, v: f32| m.and_then(|m| m.f32(k)).unwrap_or(v);
        let v2 = |b: &Block, k: &str| b.block(k).map_or([0.0; 2], |v| [v.f32("x").unwrap_or(0.0), v.f32("y").unwrap_or(0.0)]);
        let dm = d.turbulence.magic;
        RibbonDecl {
            name: name.to_string(),
            material: e.str("material").filter(|m| !m.eq_ignore_ascii_case("NULL")).unwrap_or("").to_string(),
            duration: f("duration", 0.0) as i32,
            max_length: f("maxLength", d.max_length),
            fade_in: f("fadeInFraction", d.fade_in),
            fade_out: f("fadeOutFraction", d.fade_out),
            gravity: vec3(e, "gravity", d.gravity),
            velocity: vec3(e, "velocity", d.velocity),
            start_width: f("startWidth", d.start_width),
            end_width: f("endWidth", d.end_width),
            color: [cf("x", 1.0), cf("y", 1.0), cf("z", 1.0), cf("w", 1.0)],
            view_oriented: flag(e, "viewOriented", d.view_oriented),
            reorient_nodes: flag(e, "reorientNodes", d.reorient_nodes),
            texture_repeat: flag(e, "textureRepeat", d.texture_repeat),
            texture_tangent_stretch: flag(e, "textureTangentStretch", d.texture_tangent_stretch),
            texture_repeat_distance: f("textureRepeatDistance", d.texture_repeat_distance),
            texture_t_min: f("textureTMin", d.texture_t_min),
            texture_t_max: f("textureTMax", d.texture_t_max),
            node_subdivision: f("nodeSubdivision", d.node_subdivision),
            min_node_distance: f("minNodeDistance", d.min_node_distance),
            drag: f("drag", d.drag),
            detach: flag(e, "detach", d.detach),
            apply_start_variance: flag(e, "applyStartVariance", d.apply_start_variance),
            helix: Helix {
                rotation_scale: h.f32("rotationScale").unwrap_or(0.0),
                radius: h.f32("radius").unwrap_or(0.0),
                velocity: h.f32("velocity").unwrap_or(0.0),
                distortion0: v2(&h, "distortion0"),
                distortion1: v2(&h, "distortion1"),
                wave_offset: h.f32("waveOffset").unwrap_or(0.0),
                use_length: flag(&h, "useLength", false),
            },
            turbulence: Turbulence {
                frequency: t.f32("frequency").unwrap_or(0.0),
                magnitude: t.f32("magnitude").unwrap_or(0.0),
                velocity: t.f32("velocity").unwrap_or(0.0),
                magic: [mf("x", dm[0]), mf("y", dm[1]), mf("z", dm[2]), mf("w", dm[3])],
            },
            electricity_time_min: f("turbulenceAsElectricityTimeMin", d.electricity_time_min),
            electricity_time_max: f("turbulenceAsElectricityTimeMax", d.electricity_time_max),
            turbulence_as_electricity: flag(e, "turbulenceAsElectricity", d.turbulence_as_electricity),
            anim_start_time: f("animStartTime", d.anim_start_time),
            visibility: match e.str("visibility").unwrap_or("") {
                "RIBBON_SHOW_NORMAL" => Visibility::Normal,
                "RIBBON_SHOW_QUAD" => Visibility::Quad,
                _ => Visibility::Always,
            },
        }
    }

    /// `generated/decls/ribbon/<name>.decl`.
    pub fn load(db: &DeclDb, name: &str) -> Result<RibbonDecl> {
        let b = db.get("ribbon", name).with_context(|| format!("ribbon {name}"))?;
        let e = b.block("edit").with_context(|| format!("ribbon {name}: no edit block"))?;
        Ok(RibbonDecl::parse(name, e))
    }
}

/// One idRibbon node (0x78 bytes).
#[derive(Debug, Clone, Copy)]
struct Node {
    origin: Vec3,
    origin2: Vec3,
    velocity: Vec3,
    original_velocity: Vec3,
    orient: Vec3,
    color: [f32; 4],
    spawn_time: i32,
    axis: Axis,
    length: f32,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            origin: Vec3::ZERO,
            origin2: Vec3::ZERO,
            velocity: Vec3::ZERO,
            original_velocity: Vec3::ZERO,
            orient: Vec3::ZERO,
            color: [0.0; 4],
            spawn_time: 0,
            axis: Axis::IDENTITY,
            length: 0.0,
        }
    }
}

/// How a ribbon's segments get their width (render surface mode at +0x3a20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// viewOriented: across the view (r_beamsOrientTravelDir 1: the whole ribbon's start -> end direction).
    View,
    /// Across each node's orientation vector.
    Orient,
    /// Both edges given explicitly (nodes added with a second position).
    Explicit,
}

/// One render segment (0x74-byte record of the ribbon surface).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: Vec3,
    pub end: Vec3,
    pub start2: Vec3,
    pub end2: Vec3,
    pub orient: Vec3,
    /// s at the start / end corners.
    pub s: [f32; 2],
    /// t at the two edges.
    pub t: [f32; 2],
    pub half_width: f32,
    pub color: [u8; 4],
    /// Per corner (start -, start +, end -, end +): packed tangent (vertex.tangent, unorm).
    pub tangents: [[u8; 3]; 4],
    /// Per corner: packed normalised start - end (vertex.normal, unorm).
    pub normals: [[u8; 3]; 4],
}

/// The engine's float -> unorm byte packing ((v + 1) * 127.5 + 0.5, saturated).
fn pack(v: f32) -> u8 {
    (((v + 1.0) * 127.5 + 0.5) as i32).clamp(0, 255) as u8
}

fn pack3(v: [f32; 3]) -> [u8; 3] {
    [pack(v[0]), pack(v[1]), pack(v[2])]
}

fn to_byte(v: f32) -> u8 {
    (v as i32).clamp(0, 255) as u8
}

/// idMath::InvSqrt as inlined (rsqrtss + two Newton steps, length squared clamped to FLT_MIN).
#[inline]
fn inv_sqrt(x: f32) -> f32 {
    1.0 / x.max(FLT_MIN).sqrt()
}

/// The exe's CRT sinf / cosf (0x141ed3170 / 0x141ed2c70): double-precision evaluation rounded to float.
#[inline]
fn sinf(x: f32) -> f32 {
    (x as f64).sin() as f32
}
#[inline]
fn cosf(x: f32) -> f32 {
    (x as f64).cos() as f32
}

/// The range reduction the helix / turbulence code applies before sin / cos.
#[inline]
fn wrap(a: f32) -> f32 {
    if a < 0.0 || TWO_PI <= a { a - (a * INV_TWO_PI).floor() * TWO_PI } else { a }
}

/// |x| <= 1e-18 -> 0 (the render code's snapping of lerp factors).
#[inline]
fn snap(x: f32) -> f32 {
    if x.abs() <= 1e-18 { 0.0 } else { x }
}

/// `v * mat`: x * row0 + y * row1 + z * row2, in the exe's summation order.
#[inline]
fn rows(a: &Axis, v: Vec3) -> Vec3 {
    Vec3::new(v.x * a.x.x + v.y * a.y.x + v.z * a.z.x, v.x * a.x.y + v.y * a.y.y + v.z * a.z.y, v.x * a.x.z + v.y * a.y.z + v.z * a.z.z)
}

/// idVec3::ToMat3 (0x1402edde0): forward = dir normalised, left = (-y, x, 0) / |xy|, up = forward x left.
pub fn dir_to_axis(v: Vec3) -> Axis {
    let d = v.x * v.x + v.y * v.y;
    if d.abs() <= FLT_MIN {
        let s = if 0.0 > v.z { -1.0 } else { 1.0 };
        return Axis { x: Vec3::new(0.0, 0.0, s), y: Vec3::X, z: Vec3::new(0.0, s, 0.0) };
    }
    let inv_d = inv_sqrt(d);
    let x = v * inv_sqrt(v.x * v.x + v.y * v.y + v.z * v.z);
    let y = Vec3::new(-(inv_d * v.y), inv_d * v.x, 0.0);
    Axis { x, y, z: x.cross(y) }
}

/// idRibbon (0x3c68 bytes): a ring of up to 128 nodes.
#[derive(Debug, Clone)]
pub struct Ribbon {
    pub decl: Arc<RibbonDecl>,
    /// 0 active, 1 stopping, 2 free.
    pub state: i32,
    spawn_org: Vec3,
    spawn_time: i32,
    anim_time: i32,
    anim_random_time: f32,
    pub num_active: i32,
    head: i32,
    start_variance: f32,
    total_length: f32,
    prev_time: i32,
    prev_origin: Vec3,
    prev_origin2: Vec3,
    pub global_color: [f32; 4],
    nodes: Box<[Node; MAX_NODES]>,
    explicit: bool,
}

impl Ribbon {
    /// idRibbon ctor 0x141815160 (state 2 = free).
    pub fn new(decl: Arc<RibbonDecl>) -> Ribbon {
        Ribbon {
            decl,
            state: 2,
            spawn_org: Vec3::ZERO,
            spawn_time: 0,
            anim_time: 0,
            anim_random_time: 0.0,
            num_active: 0,
            head: -1,
            start_variance: 0.0,
            total_length: 0.0,
            prev_time: -1,
            prev_origin: Vec3::ZERO,
            prev_origin2: Vec3::ZERO,
            global_color: [1.0; 4],
            nodes: Box::new([Node::default(); MAX_NODES]),
            explicit: false,
        }
    }

    #[inline]
    fn ring(i: i32) -> usize {
        i.rem_euclid(MAX_NODES as i32) as usize
    }

    /// 0x141816590: free the ribbon and clear its nodes (prevOrigin is kept).
    fn reset(&mut self) {
        self.state = 2;
        self.spawn_time = 0;
        self.num_active = 0;
        self.explicit = false;
        self.prev_time = -1;
        self.head = -1;
        for n in self.nodes.iter_mut() {
            let length = n.length;
            *n = Node { length, ..Node::default() };
        }
    }

    /// Start 0x141816620: reset, then spawn time / origin, the electric-arc period and the start variance
    /// from the ribbon random (global 0x145016e18).
    pub fn start(&mut self, time: i32, origin: Vec3, rng: &mut IdRandom) {
        self.reset();
        self.spawn_time = time;
        self.spawn_org = origin;
        self.anim_time = time;
        let (lo, hi) = (self.decl.electricity_time_min, self.decl.electricity_time_max);
        let r = rng.next_int() as f32;
        self.state = 0;
        self.anim_random_time = r * RANDOM_SCALE * (hi - lo) + lo;
        self.start_variance = if self.decl.apply_start_variance { rng.next_int() as f32 * RANDOM_SCALE } else { 0.0 };
        self.total_length = 0.0;
    }

    /// Turbulence offset 0x141815d20 (time = the ribbon's spawn time unless turbulenceAsElectricity).
    fn turbulence(&self, time: i32, p: Vec3, axis: &Axis) -> Vec3 {
        let tb = &self.decl.turbulence;
        let (freq, mag) = (tb.frequency, tb.magnitude);
        let [mx, my, mz, mw] = tb.magic;
        if mag <= EPS {
            return Vec3::ZERO;
        }
        let t = if self.decl.turbulence_as_electricity { time } else { self.spawn_time } as f32;
        // Each component: sin of one coordinate at magic x / y, cos of the next at magic z / w, phase t (+0.5, +1).
        let term = |a: f32, b: f32, off: f32| {
            let s1 = sinf(wrap(freq * a * mx + t + off));
            let s2 = sinf(wrap(freq * a * my + t + off));
            let c1 = cosf(wrap(freq * b * mz + t + off));
            let c2 = cosf(wrap(freq * b * mw + t + off));
            c2 + c1 + s2 + s1
        };
        let scale = mag * 0.5;
        let x = term(p.y, p.z, 0.0) * scale;
        let y = term(p.z, p.x, 0.5) * scale;
        let z = term(p.x, p.y, 1.0) * scale;
        Vec3::new(y * axis.y.x + x * axis.x.x + z * axis.z.x, y * axis.y.y + x * axis.x.y + z * axis.z.y, y * axis.y.z + x * axis.x.z + z * axis.z.z)
    }

    /// Helix offset 0x141815a20: a circle of `radius` across the node axis, growing in over the first 200 units
    /// from the spawn origin, phase = (length or time) * rotationScale + spawn time + waveOffset.
    fn helix(&self, time: i32, p: Vec3, axis: &Axis) -> Vec3 {
        let h = &self.decl.helix;
        if h.radius <= EPS {
            return Vec3::ZERO;
        }
        let d = p - self.spawn_org;
        let dsq = d.x * d.x + d.y * d.y + d.z * d.z;
        let dist = inv_sqrt(dsq) * dsq;
        let t = if h.use_length { dist } else { time as f32 };
        let angle = t * h.rotation_scale + self.spawn_time as f32 + h.wave_offset;
        let (s, c) = (sinf(angle), cosf(angle));
        let ramp = 1.0f32.min(dist * 0.005);
        let ca = cosf(wrap((p.x + self.spawn_org.x) * h.distortion0[0]));
        let u = (ca * h.distortion1[0] + s * h.radius) * ramp;
        let sb = sinf(wrap((p.y + self.spawn_org.y) * h.distortion0[1]));
        let v = (sb * h.distortion1[1] + c * h.radius) * ramp;
        Vec3::new(u * axis.y.x + axis.x.x * 0.0 + v * axis.z.x, u * axis.y.y + axis.x.y * 0.0 + v * axis.z.y, u * axis.y.z + axis.x.z * 0.0 + v * axis.z.z)
    }

    /// AddNode 0x1418152c0.
    fn add_node(&mut self, time: i32, pos: Vec3, pos2: Option<Vec3>, axis: &Axis, vel: Vec3, color: [f32; 4]) {
        self.explicit = pos2.is_some();
        let turb = self.turbulence(time, pos, axis);
        let helix = self.helix(time, pos, axis);
        let second = pos2.map(|p2| (p2, self.turbulence(time, p2, axis), self.helix(time, p2, axis)));
        self.head = Self::ring(self.head + 1) as i32;
        let d = self.decl.clone();
        let local = rows(axis, d.velocity);
        let prev = self.nodes[Self::ring(self.head + 127)].origin;
        let n = &mut self.nodes[self.head as usize];
        n.spawn_time = time;
        n.origin = turb + pos + helix;
        match second {
            None => n.velocity = helix * d.helix.velocity + local + vel + turb * d.turbulence.velocity,
            Some((p2, t2, h2)) => {
                n.origin2 = t2 + p2 + h2;
                n.velocity = (t2 + turb) * d.turbulence.velocity * 0.5 + local + vel + (h2 + helix) * d.helix.velocity;
            }
        }
        n.original_velocity = local + vel;
        n.color = [d.color[0] * color[0], d.color[1] * color[1], d.color[2] * color[2], d.color[3] * color[3]];
        n.axis = *axis;
        n.orient = axis.x;
        if d.max_length != 0.0 || d.texture_repeat {
            if self.num_active == 0 {
                n.length = 0.0;
            } else {
                let e = pos - prev;
                let dsq = e.y * e.y + e.x * e.x + e.z * e.z;
                let len = inv_sqrt(dsq) * dsq;
                n.length = len;
                self.total_length += len;
            }
        }
        if self.num_active < MAX_NODES as i32 {
            self.num_active += 1;
        }
    }

    /// Update 0x141817bd0: skip nodes closer than minNodeDistance to the last one; with nodeSubdivision split
    /// the step from the last node into (distance / nodeSubdivision + 1) nodes (at most 128) with lerped times.
    pub fn update(&mut self, time: i32, pos: Vec3, pos2: Option<Vec3>, axis: &Axis, vel: Vec3, color: [f32; 4]) {
        if self.state != 0 {
            return;
        }
        let d = self.decl.clone();
        let e = self.prev_origin - pos;
        let dsq = e.x * e.x + e.y * e.y + e.z * e.z;
        if d.min_node_distance > 0.0 && dsq <= d.min_node_distance * d.min_node_distance {
            return;
        }
        let mut done = false;
        if d.node_subdivision > 0.0 && self.prev_time > -1 {
            let n = ((inv_sqrt(dsq) * dsq / d.node_subdivision) as i32 + 1).min(MAX_NODES as i32);
            if n > 1 {
                for i in 0..n {
                    let f = (i as f32 + 1.0) * (1.0 / n as f32);
                    let lerp = |a: Vec3, b: Vec3| {
                        let w = 1.0 - f;
                        Vec3::new(snap(a.x) * w + snap(b.x) * f, snap(a.y) * w + snap(b.y) * f, snap(a.z) * w + snap(b.z) * f)
                    };
                    let p = lerp(self.prev_origin, pos);
                    let p2 = pos2.map(|q| lerp(self.prev_origin2, q));
                    let t = ((time as f32 - self.prev_time as f32) * f + self.prev_time as f32) as i32;
                    self.add_node(t, p, p2, axis, vel, color);
                }
                done = true;
            }
        }
        if !done {
            self.add_node(time, pos, pos2, axis, vel, color);
        }
        self.prev_time = time;
        self.prev_origin = pos;
        if let Some(p2) = pos2 {
            self.prev_origin2 = p2;
        }
    }

    /// Expire 0x141816470 (run by the ribbon manager's update wrapper, not by the tracer path): drop nodes past
    /// maxLength and past their duration (or one per call once stopping, without a duration); free when empty.
    pub fn expire(&mut self, time: i32) {
        if self.num_active == 0 {
            return;
        }
        let d = self.decl.clone();
        if d.max_length != 0.0 {
            let mut acc = 0.0f32;
            let first = self.head - self.num_active + 1;
            for i in first..=self.head {
                acc += self.nodes[Self::ring(i)].length;
                if d.max_length <= acc && acc != d.max_length {
                    self.num_active -= 1;
                }
            }
        }
        if d.duration < 1 {
            if self.state == 1 {
                self.num_active -= 1;
            }
        } else {
            let first = self.head - self.num_active + 1;
            for i in first..=self.head {
                if self.nodes[Self::ring(i)].spawn_time + d.duration < time {
                    self.num_active -= 1;
                }
            }
        }
        if self.num_active < 1 {
            self.num_active = 0;
            self.state = 2;
        }
    }

    /// Render 0x141816700: one segment per pair of consecutive nodes (newest first). `orient` is the caller's
    /// axis row 0 (used with reorientNodes), `offset` moves every node first (zero for tracers).
    pub fn segments(&mut self, time: i32, orient: Vec3, offset: Vec3, rng: &mut IdRandom, out: &mut Vec<Segment>) -> Mode {
        let d = self.decl.clone();
        let mode = if self.explicit {
            Mode::Explicit
        } else if d.view_oriented {
            Mode::View
        } else {
            Mode::Orient
        };
        let n = self.num_active;
        if n <= 1 {
            return mode;
        }
        let inv_dur = 1.0 / (d.duration as f32 * 0.001);
        let inv_fade_in = 1.0 / d.fade_in;
        let inv_fade_out = 1.0 / d.fade_out;
        let (mut run, mut inv_total, mut s_prev) = (0.0f32, 0.0f32, 0.0f32);
        if offset.to_array().iter().any(|v| v.to_bits() & 0x7fff_ffff != 0) || d.max_length != 0.0 {
            let mut sum = 0.0f32;
            for i in 0..n - 1 {
                let node = &mut self.nodes[Self::ring(self.head - i)];
                sum += node.length;
                node.origin += offset;
            }
            inv_total = 1.0 / sum;
        }
        let mut remaining = self.total_length;
        let inv_n = 1.0 / (n - 1) as f32;
        let inv_rep = 1.0 / if d.texture_repeat_distance <= FLT_MIN { FLT_MIN } else { d.texture_repeat_distance };
        let elec = d.turbulence_as_electricity;
        if elec && self.anim_random_time <= (time - self.anim_time) as f32 * 0.001 {
            self.anim_time = time;
            let r = rng.next_int() as f32;
            self.anim_random_time = r * RANDOM_SCALE * (d.electricity_time_max - d.electricity_time_min) + d.electricity_time_min;
            for i in 0..n {
                let k = Self::ring(self.head - i);
                let (o, o2, ax, ov) = (self.nodes[k].origin, self.nodes[k].origin2, self.nodes[k].axis, self.nodes[k].original_velocity);
                let (t1, h1) = (self.turbulence(time, o, &ax), self.helix(time, o, &ax));
                self.nodes[k].velocity = if !self.explicit {
                    t1 * d.turbulence.velocity + ov + h1 * d.helix.velocity
                } else {
                    let (t2, h2) = (self.turbulence(time, o2, &ax), self.helix(time, o2, &ax));
                    (h2 + h1) * d.helix.velocity + (t2 + t1) * d.turbulence.velocity * 0.5 + ov
                };
            }
        }
        let place = |o: Vec3, v: Vec3, frac: f32, t: f32, j: f32| {
            let w = snap(1.0 - frac);
            let k = (1.0 - d.drag) * 1.0 + w * d.drag;
            let tt = t + d.anim_start_time;
            Vec3::new(j * (tt * (k * v.x - d.gravity.x)) + o.x, j * (tt * (k * v.y - d.gravity.y)) + o.y, j * (tt * (k * v.z - d.gravity.z)) + o.z)
        };
        let anim_age = (time - self.anim_time) as f32 * 0.001;
        for i in 0..n - 1 {
            let a = self.nodes[Self::ring(self.head - i)];
            let b = self.nodes[Self::ring(self.head - i - 1)];
            let age = (time - a.spawn_time) as f32 * 0.001;
            let (mut ta, mut tb) = (age, (time - b.spawn_time) as f32 * 0.001);
            let (mut ja, mut jb) = (1.0f32, 1.0f32);
            if elec {
                ta = anim_age;
                tb = anim_age;
                let pa = (i as f32 * inv_n + i as f32 * inv_n - 1.0).abs();
                let pb = ((i + 1) as f32 * inv_n + (i + 1) as f32 * inv_n - 1.0).abs();
                ja = 1.0 - pa * pa;
                jb = 1.0 - pb * pb;
            }
            let (mut fa_alpha, mut fa, mut fb) = (0.5f32, 0.5f32, 0.5f32);
            if d.duration > 0 {
                fa_alpha = (age * inv_dur).min(1.0).max(0.0);
                fa = (ta * inv_dur).min(1.0).max(0.0);
                fb = (tb * inv_dur).min(1.0).max(0.0);
            }
            let start = place(a.origin, a.velocity, fa, ta, ja);
            let end = place(b.origin, b.velocity, fb, tb, jb);
            let (sw, ew) = (snap(d.start_width), snap(d.end_width));
            let half_width = ((1.0 - fa) * sw + ew * fa) * 0.5;
            let (start2, end2) = if self.explicit { (place(a.origin2, a.velocity, fa, ta, ja), place(b.origin2, b.velocity, fb, tb, jb)) } else { (Vec3::ZERO, Vec3::ZERO) };
            let gc = self.global_color;
            let mut alpha = a.color[3] * gc[3];
            if d.fade_in > 0.0 && fa_alpha < d.fade_in {
                alpha = fa_alpha * inv_fade_in;
            } else if d.fade_out > 0.0 && 1.0 - fa_alpha < d.fade_out {
                alpha = (1.0 - fa_alpha) * inv_fade_out;
            }
            let color = [to_byte(gc[0] * a.color[0] * 255.0), to_byte(a.color[1] * gc[1] * 255.0), to_byte(a.color[2] * gc[2] * 255.0), to_byte(alpha * 255.0)];
            let e = start - end;
            let lsq = e.y * e.y + e.x * e.x + e.z * e.z;
            let dir = e * inv_sqrt(lsq);
            let nrm = pack3(dir.to_array());
            let end = if d.detach { start + dir * 0.1 } else { end };
            let orient_v = if d.reorient_nodes { orient } else { a.orient };
            let sv = self.start_variance;
            let mut tangents = [pack3([0.0, 0.0, sv]); 4];
            if d.texture_tangent_stretch {
                let along_a = i as f32 * inv_n * 2.0 - 1.0;
                let z = sv * 2.0 - 1.0;
                let along_b = if d.detach { along_a } else { (i as f32 * inv_n + inv_n) * 2.0 - 1.0 };
                tangents = [[pack(along_a), 0, pack(z)], [pack(along_a), 255, pack(z)], [pack(along_b), 0, pack(z)], [pack(along_b), 255, pack(z)]];
            }
            let s = if !d.texture_repeat {
                if d.max_length == 0.0 {
                    [i as f32 * inv_n, i as f32 * inv_n + inv_n]
                } else {
                    run += a.length;
                    let s1 = run * inv_total;
                    let s0 = s_prev;
                    s_prev = s1;
                    [s0, s1]
                }
            } else if d.texture_repeat_distance > 0.0 {
                let s0 = inv_rep * remaining;
                remaining -= a.length;
                [s0, remaining * inv_rep]
            } else {
                [0.0, 1.0]
            };
            out.push(Segment {
                start,
                end,
                start2,
                end2,
                orient: orient_v,
                s,
                t: [d.texture_t_min, d.texture_t_max],
                half_width,
                color,
                tangents,
                normals: [nrm; 4],
            });
        }
        mode
    }
}

/// One expanded ribbon vertex (0x30-byte draw vert: position, st, normal, tangent, colour).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RibbonVertex {
    pub pos: Vec3,
    pub st: [f32; 2],
    pub normal: [u8; 3],
    pub tangent: [u8; 3],
    pub color: [u8; 4],
}

/// Segment -> quad expansion 0x141858010 for one ribbon's segments (4 vertices each: start -, start +,
/// end -, end +). View mode with r_beamsOrientTravelDir 1 (default) widens across (segment midpoint - eye) x
/// (last end - first start); each segment's start edge reuses the previous segment's side vector.
pub fn expand(segs: &[Segment], mode: Mode, eye: Vec3, out: &mut Vec<RibbonVertex>) {
    let Some(first) = segs.first() else { return };
    let last = segs[segs.len() - 1];
    let mut prev_side = Vec3::ZERO;
    for (k, s) in segs.iter().enumerate() {
        let (v0, v1, v2, v3) = match mode {
            Mode::Explicit => (s.start, s.start2, s.end, s.end2),
            _ => {
                let c = match mode {
                    Mode::Orient => {
                        let (d, o) = (s.end - s.start, s.orient);
                        Vec3::new(o.z * d.y - o.y * d.z, o.x * d.z - o.z * d.x, o.y * d.x - o.x * d.y)
                    }
                    _ => {
                        // r_beamsOrientTravelDir.
                        let m = (s.start + s.end) * 0.5 - eye;
                        let dd = last.end - first.start;
                        Vec3::new(dd.z * m.y - dd.y * m.z, dd.x * m.z - dd.z * m.x, dd.y * m.x - dd.x * m.y)
                    }
                };
                let lsq = c.y * c.y + c.x * c.x + c.z * c.z;
                let inv = inv_sqrt(lsq);
                let side = Vec3::new(c.x * inv * s.half_width, c.y * inv * s.half_width, c.z * inv * s.half_width);
                let ps = if k != 0 { prev_side } else { side };
                prev_side = side;
                (s.start - ps, s.start + ps, s.end - side, s.end + side)
            }
        };
        let st = [[s.s[0], s.t[0]], [s.s[0], s.t[1]], [s.s[1], s.t[0]], [s.s[1], s.t[1]]];
        for (c, p) in [v0, v1, v2, v3].into_iter().enumerate() {
            out.push(RibbonVertex { pos: p, st: st[c], normal: s.normals[c], tangent: s.tangents[c], color: s.color });
        }
    }
}

/// The ribbons one hitscan tracer spawns (idWeapon tracer ribbon set at +0x2d0 + fireMode * 0x140): started
/// by 0x140f03060 and drawn each frame by 0x140f27e40 until `expire` (spawn time + tracerLifetime).
#[derive(Debug, Clone)]
pub struct RibbonSet {
    pub expire: i32,
    pub ribbons: Vec<Ribbon>,
}

impl RibbonSet {
    /// 0x140f03060: for each ribbon decl (with its offset): Start at start + offset, then nodes at start + offset
    /// and at `end`, both at `time`, axis = (end - start).ToMat3, no velocity, white.
    pub fn fire(&mut self, decls: &[(Arc<RibbonDecl>, Vec3)], time: i32, start: Vec3, end: Vec3, lifetime: i32, rng: &mut IdRandom) {
        let axis = dir_to_axis(end - start);
        self.expire = time + lifetime;
        // Ribbons come from the decl's pool (0x141718210); a set's slots keep their previous ribbon's prevOrigin.
        while self.ribbons.len() < decls.len() {
            self.ribbons.push(Ribbon::new(decls[self.ribbons.len()].0.clone()));
        }
        self.ribbons.truncate(decls.len());
        for (r, (decl, offset)) in self.ribbons.iter_mut().zip(decls) {
            if !Arc::ptr_eq(&r.decl, decl) {
                *r = Ribbon::new(decl.clone());
            }
            let p = start + *offset;
            r.start(time, p, rng);
            r.update(time, p, None, &axis, Vec3::ZERO, [1.0; 4]);
            r.update(time, end, None, &axis, Vec3::ZERO, [1.0; 4]);
        }
    }

    /// Weapon vfunc 0x630: the first set whose first ribbon is idle, else the one expiring first.
    pub fn pick(sets: &[RibbonSet]) -> usize {
        sets.iter().position(|s| s.ribbons.first().is_none_or(|r| r.state != 0)).unwrap_or_else(|| sets.iter().enumerate().min_by_key(|(_, s)| s.expire).map_or(0, |(i, _)| i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauss_main() -> RibbonDecl {
        RibbonDecl {
            name: "main".into(),
            material: "textures/ribbons/gauss_beam".into(),
            duration: 250,
            fade_out: 0.5,
            gravity: Vec3::new(0.0, 0.0, -1.0),
            velocity: Vec3::new(100.0, 0.0, 0.0),
            start_width: 30.0,
            end_width: 10.0,
            texture_repeat_distance: 1500.0,
            node_subdivision: 128.0,
            min_node_distance: 15.0,
            drag: 0.2,
            turbulence: Turbulence { frequency: 0.05, magnitude: 0.1, velocity: 60.0, magic: [1.73, 1.37, 1.13, 1.73] },
            electricity_time_max: 1.0,
            ..RibbonDecl::default()
        }
    }

    #[test]
    fn pack_matches_engine() {
        assert_eq!(pack(-1.0), 0);
        assert_eq!(pack(0.0), 128);
        assert_eq!(pack(1.0), 255);
        assert_eq!(pack(2.0), 255);
        assert_eq!(pack(-3.0), 0);
    }

    #[test]
    fn tracer_subdivides_and_fades() {
        let decl = Arc::new(gauss_main());
        let mut set = RibbonSet { expire: 0, ribbons: Vec::new() };
        let mut rng = IdRandom(0);
        let (start, end) = (Vec3::new(10.0, 0.0, 50.0), Vec3::new(1010.0, 0.0, 50.0));
        set.fire(&[(decl.clone(), Vec3::ZERO)], 1000, start, end, 2000, &mut rng);
        let r = &mut set.ribbons[0];
        // 1 node at the start + 1000 / 128 + 1 = 8 subdivided nodes.
        assert_eq!(r.num_active, 9);
        assert_eq!(set.expire, 3000);
        let mut segs = Vec::new();
        let mode = r.segments(1000, Vec3::X, Vec3::ZERO, &mut rng, &mut segs);
        assert_eq!(mode, Mode::View);
        assert_eq!(segs.len(), 8);
        // Newest node first: the end of the shot.
        assert!((segs[0].start - end).length() < 1.0, "{:?}", segs[0].start);
        assert!((segs[7].end - start).length() < 1.0, "{:?}", segs[7].end);
        assert_eq!(segs[0].half_width, 15.0);
        assert_eq!(segs[0].color[3], 255);
        // Half way through the fade-out (fraction 0.75 -> alpha (1 - 0.75) / 0.5).
        segs.clear();
        r.segments(1000 + 187, Vec3::X, Vec3::ZERO, &mut rng, &mut segs);
        let frac = 0.187f32 / 0.25;
        assert_eq!(segs[0].color[3], ((1.0 - frac) * (1.0 / 0.5) * 255.0) as u8);
        let mut verts = Vec::new();
        expand(&segs, mode, Vec3::new(0.0, -200.0, 50.0), &mut verts);
        assert_eq!(verts.len(), 32);
        // Width across the view: the beam runs along x, the eye looks along +y, so the quad spans z.
        let w = verts[1].pos - verts[0].pos;
        assert!(w.z.abs() > w.x.abs() && w.z.abs() > w.y.abs(), "{w:?}");
    }

    #[test]
    fn gauss_ribbon_decls() {
        let Some(doom) = idres::find_install() else { return };
        let Ok(c) = idres::Container::open(&doom.join("base"), "gameresources") else { return };
        let db = DeclDb::new(Arc::new(c));
        let m = RibbonDecl::load(&db, "ca_mp_gauss_rifle_ribbon/ca_mp_gauss_rifle_main").unwrap();
        assert_eq!(m.material, "textures/ribbons/gauss_beam");
        assert_eq!((m.duration, m.start_width, m.end_width, m.fade_out), (250, 30.0, 10.0, 0.5));
        assert_eq!((m.gravity, m.velocity), (Vec3::new(0.0, 0.0, -1.0), Vec3::new(100.0, 0.0, 0.0)));
        assert!(m.view_oriented && m.texture_tangent_stretch && !m.texture_repeat && !m.turbulence_as_electricity);
        assert_eq!(m.turbulence.magic, [1.73, 1.37, 1.13, 1.73]);
        assert_eq!((m.electricity_time_min, m.electricity_time_max), (0.2, 1.0));
        let h = RibbonDecl::load(&db, "ca_mp_gauss_rifle_ribbon/ca_mp_gauss_rifle_helix_01").unwrap();
        assert_eq!(h.color, [0.6, 0.95, 1.0, 0.5]);
        assert!(h.turbulence_as_electricity && h.texture_repeat && h.helix.use_length);
        assert_eq!(h.visibility, Visibility::Always);
        let q = RibbonDecl::load(&db, "ca_mp_gauss_rifle_ribbon/ca_mp_gauss_rifle_main_quad").unwrap();
        assert_eq!(q.visibility, Visibility::Quad);
        let t = RibbonDecl::load(&db, "assault_rifle_tracer").unwrap();
        assert_eq!((t.max_length, t.node_subdivision, t.helix.distortion1), (100.0, 4.0, [1.0, 0.0]));
        assert_eq!(t.turbulence.magic, [0.0, 0.0, 0.1, 0.0]);
    }

    #[test]
    fn min_node_distance_skips_close_nodes() {
        let mut d = gauss_main();
        d.min_node_distance = 500.0;
        let decl = Arc::new(d);
        let mut set = RibbonSet { expire: 0, ribbons: Vec::new() };
        let mut rng = IdRandom(0);
        set.fire(&[(decl, Vec3::ZERO)], 0, Vec3::new(1000.0, 0.0, 0.0), Vec3::new(1300.0, 0.0, 0.0), 2000, &mut rng);
        assert_eq!(set.ribbons[0].num_active, 1);
    }
}
