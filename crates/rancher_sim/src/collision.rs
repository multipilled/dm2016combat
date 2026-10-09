//! World collision for the player's clip model (an 8-sided cylinder, see `Hull::player_trace_model`).
//!
//! Two layers, both decoded from the game (addresses in gamedata/re/MOVEMENT.md):
//!
//! * The collision-model translation (`World::translate`, `World::contacts`): the idTech4-lineage
//!   feature test the game still runs. Trace-model vertices are swept through world polygons, trace-model
//!   edges against polygon edges, and polygon vertices back through trace-model polygons. Every feature
//!   stops `CLIP_EPSILON` short of its plane (0x141944e70); contacts are every feature hit within the
//!   query distance (0x141944310, at most 12).
//! * The slide/step solver SlideMove hands to the collision job (0x141581890 and helpers): up to four
//!   passes, each a move/up/across/down step trace, with the delta clipped against up to four planes and
//!   gravity integrated by a midpoint half step.
//!
//! Traces run against the world as the game's collision model presents it: the brushes' faces with the
//! parts inside or pressed against other brushes chopped away and flat/concave edges flagged internal
//! (`World::collision_models`).

use std::collections::HashSet;
use std::sync::Arc;

use glam::Vec3;
use idres::bcm::{CollisionModel, SubModel};

/// Re-exported for `World::add_cm` callers (rancher_sim's glam, not bevy's).
pub use glam::Mat3;

/// Distance every trace keeps from a surface (0x142014e74, read by the plane fraction 0x141944e70).
pub const CLIP_EPSILON: f32 = 0.25;
/// SlideMove's contact query after the slide: this far along the gravity direction (0x141581ba0).
pub const CONTACT_DISTANCE: f32 = 0.5;
/// Contacts kept per query (0x141944310).
pub const MAX_CONTACTS: usize = 12;
pub const OVERCLIP: f32 = 1.001;
/// CheckGround's walkable limit (up·n < 0.7 is a steep plane).
pub const MIN_WALK_NORMAL: f32 = 0.7;
/// Slide passes (0x141581890) and clip planes kept across them (0x141958ed0).
pub const SLIDE_PASSES: usize = 4;
pub const MAX_CLIP_PLANES: usize = 4;
/// The slide clips against a plane whenever the delta's component along it is below this (0x141957100).
pub const CLIP_THRESHOLD: f32 = 0.05;
/// After clipping, a delta whose component along the original direction is below this loses it (0x141958ed0).
pub const BACKWARD_THRESHOLD: f32 = 0.01;
/// The step-down trace lands on a floor only if its normal's world Z exceeds cos 45° (0x141958bc0).
pub const STEP_FLOOR_NORMAL_Z: f32 = 0.70710677;
/// Repeated planes (dot above this) are nudged instead of added (0x141958ed0).
const SAME_PLANE_DOT: f32 = 0.999;
/// idTraceModel::GenerateEdgeNormals: sharper edges get a capped outward normal.
const SHARP_EDGE_DOT: f32 = -0.7;
/// Trace-model polygons whose normal·dir exceeds this lead the sweep (0x141947890, convex models).
const USED_POLY_DOT: f32 = 0.0;
/// Broadphase slack around a sweep (only culls; the feature tests decide).
const BOUNDS_SLACK: f32 = 1.0;

#[derive(Debug, Clone)]
pub struct Poly {
    /// Outward unit normal and plane distance (n·p = dist on the plane).
    pub normal: Vec3,
    pub dist: f32,
    /// Vertex loop, counter-clockwise seen from outside.
    pub verts: Vec<usize>,
    pub edges: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub v: [usize; 2],
    /// Outward edge normal as idTraceModel::GenerateEdgeNormals builds it: sized so that the edge moved by
    /// `normal * d` lies `d` in front of both adjacent planes. Trace-model edges are shifted by
    /// `normal * CLIP_EPSILON` for the epsilon edge test (0x1419457c0).
    pub normal: Vec3,
    /// World edges where the surface is flat or concave (the collision model's edge flag 0x4000): a
    /// convex trace model cannot touch them before the polygons beside them, so the edge test
    /// (0x1419457c0) and the polygon-vertex test (0x141945350) skip them.
    pub internal: bool,
}

#[derive(Debug, Clone)]
pub struct Hull {
    pub verts: Vec<Vec3>,
    /// Outward face normals with the face's vertex loop (for rendering).
    pub faces: Vec<(Vec3, Vec<usize>)>,
    pub polys: Vec<Poly>,
    pub edges: Vec<Edge>,
    pub min: Vec3,
    pub max: Vec3,
}

impl Hull {
    pub fn cuboid(min: Vec3, max: Vec3) -> Self {
        let mut verts = Vec::with_capacity(8);
        for i in 0..8 {
            verts.push(Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            ));
        }
        Self::from_points(&verts)
    }

    /// Convex hull of a small point set (brute force; fine for level brushes of a few dozen points).
    pub fn from_points(points: &[Vec3]) -> Self {
        let n = points.len();
        let mut planes: Vec<(Vec3, Vec<usize>)> = Vec::new();
        for i in 0..n {
            for j in i + 1..n {
                for k in j + 1..n {
                    let nrm = (points[j] - points[i]).cross(points[k] - points[i]);
                    if nrm.length_squared() < 1e-8 {
                        continue;
                    }
                    let nrm = nrm.normalize();
                    let d = nrm.dot(points[i]);
                    let (mut above, mut below) = (false, false);
                    for p in points {
                        let s = nrm.dot(*p) - d;
                        above |= s > 1e-3;
                        below |= s < -1e-3;
                    }
                    if above && below {
                        continue;
                    }
                    let outward = if above { -nrm } else { nrm };
                    if planes.iter().any(|(f, _)| f.dot(outward) > 0.9999) {
                        continue;
                    }
                    let on: Vec<usize> = (0..n).filter(|&m| (outward.dot(points[m]) - outward.dot(points[i])).abs() < 1e-3).collect();
                    planes.push((outward, on));
                }
            }
        }
        let mut polys = Vec::with_capacity(planes.len());
        for (normal, on) in planes {
            let loop_ = convex_loop(points, normal, &on);
            let dist = normal.dot(points[loop_[0]]);
            polys.push(Poly { normal, dist, verts: loop_, edges: Vec::new() });
        }
        // Undirected edges from the polygon loops.
        let mut edges: Vec<Edge> = Vec::new();
        for p in polys.iter_mut() {
            for k in 0..p.verts.len() {
                let (a, b) = (p.verts[k], p.verts[(k + 1) % p.verts.len()]);
                let e = match edges.iter().position(|e| (e.v[0] == a && e.v[1] == b) || (e.v[0] == b && e.v[1] == a)) {
                    Some(e) => e,
                    None => {
                        edges.push(Edge { v: [a, b], normal: Vec3::ZERO, internal: false });
                        edges.len() - 1
                    }
                };
                p.edges.push(e);
            }
        }
        // idTraceModel::GenerateEdgeNormals.
        for p in &polys {
            for k in 0..p.verts.len() {
                let (a, b) = (p.verts[k], p.verts[(k + 1) % p.verts.len()]);
                let e = &mut edges[p.edges[k]];
                if e.normal == Vec3::ZERO {
                    e.normal = p.normal;
                } else {
                    let dot = e.normal.dot(p.normal);
                    if dot < SHARP_EDGE_DOT {
                        let dir = points[b] - points[a];
                        let m = e.normal.cross(dir) + p.normal.cross(-dir);
                        e.normal = m * ((0.5 / (0.5 + 0.5 * SHARP_EDGE_DOT)) / m.length());
                    } else {
                        e.normal = (e.normal + p.normal) * (0.5 / (0.5 + 0.5 * dot));
                    }
                }
            }
        }
        let faces = polys.iter().map(|p| (p.normal, p.verts.clone())).collect();
        let min = points.iter().copied().reduce(Vec3::min).unwrap();
        let max = points.iter().copied().reduce(Vec3::max).unwrap();
        Self { verts: points.to_vec(), faces, polys, edges, min, max }
    }

    /// A cylinder of `radius` from z = 0 to z = `height` (SetupCylinder of the matching bounds).
    pub fn cylinder(radius: f32, height: f32, sides: usize) -> Self {
        Self::setup_cylinder(Vec3::new(-radius, -radius, 0.0), Vec3::new(radius, radius, height), sides)
    }

    /// idTraceModel::SetupCylinder (0x1402d0100): `sides` vertices per cap at angle 2π·i/sides
    /// (vertex 0 on +X), scaled to the bounds' half extents.
    pub fn setup_cylinder(min: Vec3, max: Vec3, sides: usize) -> Self {
        let c = (min + max) * 0.5;
        let h = max - c;
        let mut pts = Vec::with_capacity(sides * 2);
        for z in [c.z - h.z, c.z + h.z] {
            for i in 0..sides {
                let a = (i as f32 * 6.2831855) / sides as f32;
                pts.push(Vec3::new(a.cos() * h.x + c.x, a.sin() * h.y + c.y, z));
            }
        }
        Self::from_points(&pts)
    }

    /// idTraceModel::SetupPencil (0x1402d13d0): a cylinder whose lower `taper_height` narrows to a
    /// ring of `taper_radius` (or to a point when that is 0).
    pub fn setup_pencil(min: Vec3, max: Vec3, sides: usize, taper_height: f32, taper_radius: f32) -> Self {
        let c = (min + max) * 0.5;
        let h = max - c;
        let taper_height = taper_height.min(max.z - min.z).max(0.0);
        let taper_radius = taper_radius.min((max.x - min.x).max(max.y - min.y)).max(0.0);
        let mut pts = Vec::new();
        for i in 0..sides {
            let a = (i as f32 * 6.2831855) / sides as f32;
            let (s, co) = a.sin_cos();
            pts.push(Vec3::new(co * h.x + c.x, s * h.y + c.y, c.z + h.z));
            pts.push(Vec3::new(co * h.x + c.x, s * h.y + c.y, c.z - h.z + taper_height));
            if taper_radius > 0.0 {
                pts.push(Vec3::new((taper_radius - c.x) * co + c.x, (taper_radius - c.y) * s + c.y, c.z - h.z));
            }
        }
        if taper_radius <= 0.0 {
            pts.push(Vec3::new(c.x, c.y, c.z - h.z));
        }
        Self::from_points(&pts)
    }

    /// The player's trace model as physics SetClipModel (0x1416b2aa0) builds it from the clip bounds:
    /// pm_playerCollisionStyle 1 → eight-sided cylinder, 2 → six-sided pencil whose taper height is
    /// tan(clamp(angle, 0, 89°))·(half width − taper radius), anything else → the box.
    pub fn player_trace_model(style: i32, min: Vec3, max: Vec3, pencil_angle: f32, pencil_taper_radius: f32) -> Self {
        match style {
            1 => Self::setup_cylinder(min, max, 8),
            2 => {
                let angle = pencil_angle.clamp(0.0, 89.0);
                let half = (max.x - min.x).max(max.y - min.y) * 0.5;
                let taper = pencil_taper_radius.min(half).max(0.0);
                Self::setup_pencil(min, max, 6, (angle * 0.017453292).tan() * (half - taper), taper)
            }
            _ => Self::cuboid(min, max),
        }
    }

    /// The hull with every point and normal multiplied by `m` (a rotation).
    pub fn rotated(&self, m: Mat3) -> Hull {
        let verts: Vec<Vec3> = self.verts.iter().map(|&v| m * v).collect();
        let polys = self.polys.iter().map(|p| Poly { normal: m * p.normal, ..p.clone() }).collect();
        let edges = self.edges.iter().map(|e| Edge { normal: m * e.normal, ..e.clone() }).collect();
        let faces = self.faces.iter().map(|(n, l)| (m * *n, l.clone())).collect();
        let min = verts.iter().copied().reduce(Vec3::min).unwrap_or(Vec3::ZERO);
        let max = verts.iter().copied().reduce(Vec3::max).unwrap_or(Vec3::ZERO);
        Hull { verts, faces, polys, edges, min, max }
    }

    /// The trace model idClip keeps at +0x20 for generic traces (0x14162f1b6): a box of ±0.01.
    pub fn trace_box() -> Self {
        Self::cuboid(Vec3::splat(-0.01), Vec3::splat(0.01))
    }

    fn project(&self, offset: Vec3, axis: Vec3) -> (f32, f32) {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for v in &self.verts {
            let d = (*v + offset).dot(axis);
            lo = lo.min(d);
            hi = hi.max(d);
        }
        (lo, hi)
    }

    /// Whether `self` placed at `origin` strictly overlaps `other` (separating-axis test).
    pub fn overlaps(&self, origin: Vec3, other: &Hull) -> bool {
        let separated = |axis: Vec3| {
            if axis.length_squared() < 1e-12 {
                return false;
            }
            let (a0, a1) = self.project(origin, axis);
            let (b0, b1) = other.project(Vec3::ZERO, axis);
            a1 <= b0 || b1 <= a0
        };
        if self.polys.iter().any(|p| separated(p.normal)) || other.polys.iter().any(|p| separated(p.normal)) {
            return false;
        }
        for ea in &self.edges {
            let da = self.verts[ea.v[1]] - self.verts[ea.v[0]];
            for eb in &other.edges {
                let db = other.verts[eb.v[1]] - other.verts[eb.v[0]];
                if separated(da.cross(db).normalize_or_zero()) {
                    return false;
                }
            }
        }
        true
    }
}

/// Orders the coplanar points `on` into a convex loop, counter-clockwise around `normal`, dropping
/// points that lie inside an edge.
fn convex_loop(points: &[Vec3], normal: Vec3, on: &[usize]) -> Vec<usize> {
    let c = on.iter().map(|&i| points[i]).sum::<Vec3>() / on.len() as f32;
    let t = (points[on[0]] - c).normalize_or(normal.any_orthonormal_vector());
    let bt = normal.cross(t);
    let mut sorted: Vec<usize> = on.to_vec();
    let ang = |i: usize| (points[i] - c).dot(bt).atan2((points[i] - c).dot(t));
    sorted.sort_by(|&a, &b| ang(a).partial_cmp(&ang(b)).unwrap());
    // Remove points that do not turn (collinear).
    let mut out: Vec<usize> = Vec::with_capacity(sorted.len());
    for k in 0..sorted.len() {
        let prev = sorted[(k + sorted.len() - 1) % sorted.len()];
        let next = sorted[(k + 1) % sorted.len()];
        let cur = sorted[k];
        let turn = (points[cur] - points[prev]).cross(points[next] - points[cur]).dot(normal);
        if turn > 1e-4 {
            out.push(cur);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContactKind {
    #[default]
    None,
    /// A trace-model vertex hit a world polygon (normal = the polygon's).
    TrmVertex,
    /// A trace-model edge hit a world edge (normal = their cross product, facing against the motion).
    Edge,
    /// A world vertex hit a trace-model polygon (normal = the inverted trace-model polygon normal).
    ModelVertex,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Contact {
    pub kind: ContactKind,
    pub point: Vec3,
    pub normal: Vec3,
    pub dist: f32,
    /// Index into `World::brushes`, or `brushes.len() + i` for `World::cms[i]`.
    pub brush: usize,
    /// The hit surface's `surface_flags` (collision models only; brushes have none).
    pub surface_flags: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Trace {
    pub fraction: f32,
    pub endpos: Vec3,
    pub c: Contact,
}

impl Trace {
    pub fn hit(&self) -> bool {
        self.c.kind != ContactKind::None
    }
}

#[derive(Debug, Clone)]
pub struct World {
    /// The convex brushes as added (position tests, rays, rendering).
    pub brushes: Vec<Hull>,
    /// Per brush, the polygons traces see once the world is merged (built on first use).
    cm: std::sync::OnceLock<Vec<Hull>>,
    /// Placed collision models from map data (`.bcm`), already merged by the game's tools.
    pub cms: Vec<CmInstance>,
    /// Contents mask of the clip model's traces: the player's clip mask 0x100409 (SOLID | PLAYERCLIP |
    /// AI | SOLIDPUSHABLE), which idPlayer's movement-type update (0x140e2cbc0) gives the physics.
    pub clip_mask: u32,
    /// Contents mask for `ray` (hitscan). INTERIM: SOLID | SHOTCLIP; the game's shot mask is not decoded here.
    pub shot_mask: u32,
}

impl Default for World {
    fn default() -> Self {
        Self {
            brushes: Vec::new(),
            cm: std::sync::OnceLock::new(),
            cms: Vec::new(),
            clip_mask: PLAYER_CLIP_MASK,
            shot_mask: idres::bcm::contents::SOLID | idres::bcm::contents::SHOTCLIP,
        }
    }
}

/// The player's clip mask (physics SetClipMask from 0x140e2cbc0).
pub const PLAYER_CLIP_MASK: u32 = 0x100409;

/// A `.bcm` collision model placed in the world. `axis` columns are the model's axes in world space.
#[derive(Debug, Clone)]
pub struct CmInstance {
    pub cm: Arc<CollisionModel>,
    pub origin: Vec3,
    pub axis: Mat3,
    /// World-space bounds.
    pub min: Vec3,
    pub max: Vec3,
    /// Set for movers (an entity with idPhysics_Parametric); None = part of the world for the player.
    pub mover: Option<MoverProps>,
}

/// The mover physics flags player physics reads from the ground entity (via physics +0x2218):
/// idPhysics_Parametric::isPusher (+0x530, vtbl+0x210) and idPhysics_DynamicBase preventPlayerJump
/// (+0xdc, vtbl+0x258) / dislodgePlayer (+0xdd, vtbl+0x260).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoverProps {
    pub is_pusher: bool,
    /// "prevent the player from jumping on this phys type".
    pub prevent_player_jump: bool,
    /// "try to dislodge the player in the event of being stuck/crushed".
    pub dislodge_player: bool,
}

impl Default for MoverProps {
    fn default() -> Self {
        Self { is_pusher: true, prevent_player_jump: false, dislodge_player: false }
    }
}

/// Which collision models a trace sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceFilter {
    All,
    /// Everything but this `World::cms` index (a pusher tracing what it pushes).
    Without(usize),
    /// Only this `World::cms` index.
    Only(usize),
}

impl CmInstance {
    fn is_identity(&self) -> bool {
        self.origin == Vec3::ZERO && self.axis == Mat3::IDENTITY
    }
    fn to_model(&self, p: Vec3) -> Vec3 {
        self.axis.transpose() * (p - self.origin)
    }
    fn to_world(&self, p: Vec3) -> Vec3 {
        self.origin + self.axis * p
    }
}

fn v3(a: [f32; 3]) -> Vec3 {
    Vec3::from_array(a)
}

/// Whether integer polygon/brush bounds touch `lo..hi`.
fn ibounds_touch(b: &[[i16; 3]; 2], lo: Vec3, hi: Vec3) -> bool {
    let bmin = Vec3::new(b[0][0] as f32, b[0][1] as f32, b[0][2] as f32);
    let bmax = Vec3::new(b[1][0] as f32, b[1][1] as f32, b[1][2] as f32);
    !(bmax.cmplt(lo).any() || bmin.cmpgt(hi).any())
}

/// Polygons (and with `brushes`, brushes) of the submodel's KD nodes the box `lo..hi` reaches, in walk
/// order, each once. Like the game's walk (0x141955f30) every node can hold polygons, children[0] is
/// the positive side of `dist`, and axis -1 ends a branch; the box test is the conservative form of
/// its segment clipping.
fn kd_candidates(sm: &SubModel, lo: Vec3, hi: Vec3, polys: &mut Vec<u16>, mut brushes: Option<&mut Vec<u16>>) {
    polys.clear();
    if let Some(b) = brushes.as_deref_mut() {
        b.clear();
    }
    if sm.nodes.is_empty() {
        polys.extend((0..sm.polygons.len()).map(|i| i as u16));
        if let Some(b) = brushes {
            b.extend((0..sm.brushes.len()).map(|i| i as u16));
        }
        return;
    }
    let mut seen_p: HashSet<u16> = HashSet::new();
    let mut seen_b: HashSet<u16> = HashSet::new();
    let mut stack = vec![0usize];
    while let Some(ni) = stack.pop() {
        let Some(n) = sm.nodes.get(ni) else { continue };
        let first = n.first as usize;
        for &r in sm.refs.iter().skip(first).take(n.num_polygons as usize) {
            if seen_p.insert(r) && sm.polygons.get(r as usize).is_some_and(|p| ibounds_touch(&p.bounds, lo, hi)) {
                polys.push(r);
            }
        }
        if let Some(b) = brushes.as_deref_mut() {
            for &r in sm.refs.iter().skip(first + n.num_polygons as usize).take(n.num_brushes as usize) {
                if seen_b.insert(r) && sm.brushes.get(r as usize).is_some_and(|x| ibounds_touch(&x.bounds, lo, hi)) {
                    b.push(r);
                }
            }
        }
        if !(0..3).contains(&n.axis) {
            continue;
        }
        let a = n.axis as usize;
        if lo[a] <= n.dist + BOUNDS_SLACK {
            stack.push(n.children[1] as usize);
        }
        if hi[a] >= n.dist - BOUNDS_SLACK {
            stack.push(n.children[0] as usize);
        }
    }
}

/// The submodels of `cm` the box can reach under `mask` (contents 0 is never skipped, 0x141955d10).
fn submodels_in<'a>(cm: &'a CollisionModel, lo: Vec3, hi: Vec3, mask: u32) -> impl Iterator<Item = &'a SubModel> + 'a {
    cm.submodels.iter().filter(move |sm| {
        (sm.contents == 0 || sm.contents & mask != 0)
            && !(v3(sm.bounds[1]).cmplt(lo).any() || v3(sm.bounds[0]).cmpgt(hi).any())
    })
}

fn polygon_points(sm: &SubModel, p: &idres::bcm::Polygon) -> Vec<Vec3> {
    sm.polygon_verts(p).map(|v| v3(sm.verts[v])).collect()
}

/// Whether the line `p + t·dir` crosses the plane of the convex loop `pts` inside it.
fn line_through_points(pts: &[Vec3], normal: Vec3, dist: f32, p: Vec3, dir: Vec3) -> bool {
    let denom = normal.dot(dir);
    if denom == 0.0 {
        return false;
    }
    let x = p + dir * ((dist - normal.dot(p)) / denom);
    (0..pts.len()).all(|k| (pts[(k + 1) % pts.len()] - pts[k]).cross(x - pts[k]).dot(normal) >= 0.0)
}

/// CM_TranslationPlaneFraction (0x141944e70): the fraction at which `start → end` comes within
/// `CLIP_EPSILON` of the plane, or 1 when it never does or is behind it.
fn plane_fraction(n: Vec3, dist: f32, start: Vec3, end: Vec3) -> f32 {
    let d2 = n.dot(end) - dist;
    if d2 >= CLIP_EPSILON {
        return 1.0;
    }
    let d1 = n.dot(start) - dist;
    if d1 < 0.0 || d2 >= d1 {
        return 1.0;
    }
    (d1 - CLIP_EPSILON) / (d1 - d2)
}

/// Whether the line `p + t·dir` crosses the plane of the convex loop `loop_` inside it.
fn line_through_loop(verts: &[Vec3], offset: Vec3, loop_: &[usize], normal: Vec3, dist: f32, p: Vec3, dir: Vec3) -> bool {
    let denom = normal.dot(dir);
    if denom == 0.0 {
        return false;
    }
    let x = p + dir * ((dist - normal.dot(p)) / denom);
    for k in 0..loop_.len() {
        let a = verts[loop_[k]] + offset;
        let b = verts[loop_[(k + 1) % loop_.len()]] + offset;
        if (b - a).cross(x - a).dot(normal) < 0.0 {
            return false;
        }
    }
    true
}

/// One translation of the trace model `trm` (placed at `start`) to `end`. With `contacts`, every
/// feature hit is recorded and the fraction reset (CM_AddContact), as the game's contact query does.
struct TraceWork<'a> {
    trm: &'a Hull,
    start: Vec3,
    dir: Vec3,
    used_poly: Vec<bool>,
    used_vert: Vec<bool>,
    used_edge: Vec<bool>,
    trm_planes: Vec<f32>,
    fraction: f32,
    c: Contact,
    contacts: Option<Vec<Contact>>,
}

impl<'a> TraceWork<'a> {
    fn new(trm: &'a Hull, start: Vec3, end: Vec3, gather: bool) -> Self {
        let dir = end - start;
        let used_poly: Vec<bool> = trm.polys.iter().map(|p| p.normal.dot(dir) > USED_POLY_DOT).collect();
        let mut used_vert = vec![false; trm.verts.len()];
        let mut used_edge = vec![false; trm.edges.len()];
        for (p, &u) in trm.polys.iter().zip(&used_poly) {
            if u {
                p.verts.iter().for_each(|&v| used_vert[v] = true);
                p.edges.iter().for_each(|&e| used_edge[e] = true);
            }
        }
        let trm_planes = trm.polys.iter().map(|p| p.dist + p.normal.dot(start)).collect();
        Self {
            trm,
            start,
            dir,
            used_poly,
            used_vert,
            used_edge,
            trm_planes,
            fraction: 1.0,
            c: Contact::default(),
            contacts: gather.then(Vec::new),
        }
    }

    fn record(&mut self, f: f32, c: Contact) {
        self.fraction = f.max(0.0);
        self.c = c;
        if let Some(list) = &mut self.contacts {
            if list.len() < MAX_CONTACTS {
                list.push(c);
                self.fraction = 1.0;
            }
        }
    }

    /// TranslateTrmThroughPolygon for every polygon of one convex brush.
    fn brush(&mut self, bi: usize, b: &Hull) {
        let mut edge_checked = vec![false; b.edges.len()];
        let mut vert_checked = vec![false; b.verts.len()];
        let trm = self.trm;
        let dir = self.dir;
        for poly in &b.polys {
            // Only polygons approached from the front.
            if poly.normal.dot(dir) > 0.0 {
                continue;
            }
            // Trace-model vertices through the polygon (0x1419462b0).
            for (vi, &v) in trm.verts.iter().enumerate() {
                if !self.used_vert[vi] {
                    continue;
                }
                let p = self.start + v;
                let f = plane_fraction(poly.normal, poly.dist, p, p + dir);
                if f < self.fraction && line_through_loop(&b.verts, Vec3::ZERO, &poly.verts, poly.normal, poly.dist, p, dir) {
                    let point = p + dir * f.max(0.0);
                    self.record(f, Contact { kind: ContactKind::TrmVertex, point, normal: poly.normal, dist: poly.dist, brush: bi, surface_flags: 0 });
                }
            }
            // Trace-model edges through the polygon's edges (0x1419457c0).
            for (ei, e) in trm.edges.iter().enumerate() {
                if !self.used_edge[ei] {
                    continue;
                }
                let a0 = self.start + trm.verts[e.v[0]];
                let a1 = self.start + trm.verts[e.v[1]];
                for &pe in &poly.edges {
                    if edge_checked[pe] || b.edges[pe].internal {
                        continue;
                    }
                    let b0 = b.verts[b.edges[pe].v[0]];
                    let b1 = b.verts[b.edges[pe].v[1]];
                    self.edge_edge(bi, 0, a0, a1, e.normal, b0, b1);
                }
            }
            // Polygon vertices through the trace-model polygons (0x141945350); marks edges and vertices.
            for &pe in &poly.edges {
                if edge_checked[pe] {
                    continue;
                }
                edge_checked[pe] = true;
                if b.edges[pe].internal {
                    continue;
                }
                for k in 0..2 {
                    let vi = b.edges[pe].v[k];
                    if vert_checked[vi] {
                        continue;
                    }
                    vert_checked[vi] = true;
                    self.vertex_through_trm(bi, 0, b.verts[vi]);
                }
            }
        }
    }

    /// A world vertex through the trace-model polygons (0x141945350's inner test).
    fn vertex_through_trm(&mut self, bi: usize, surface_flags: u32, v: Vec3) {
        let trm = self.trm;
        let dir = self.dir;
        for (pi, tp) in trm.polys.iter().enumerate() {
            if !self.used_poly[pi] {
                continue;
            }
            let tdist = self.trm_planes[pi];
            let f = plane_fraction(tp.normal, tdist, v, v - dir);
            if f < self.fraction && line_through_loop(&trm.verts, self.start, &tp.verts, tp.normal, tdist, v, -dir) {
                let point = v - dir * f.max(0.0);
                self.record(f, Contact { kind: ContactKind::ModelVertex, point, normal: -tp.normal, dist: -tdist, brush: bi, surface_flags });
            }
        }
    }

    /// Every polygon of the collision model's submodels the sweep box `lo..hi` (model space) reaches.
    fn collision_model(&mut self, bi: usize, cm: &CollisionModel, lo: Vec3, hi: Vec3, mask: u32) {
        let mut polys = Vec::new();
        for sm in submodels_in(cm, lo, hi, mask) {
            kd_candidates(sm, lo, hi, &mut polys, None);
            let mut edge_checked: HashSet<usize> = HashSet::new();
            let mut vert_checked: HashSet<usize> = HashSet::new();
            for &pi in &polys {
                self.cm_polygon(bi, sm, &sm.polygons[pi as usize], mask, &mut edge_checked, &mut vert_checked);
                if self.contacts.is_none() && self.fraction == 0.0 {
                    return;
                }
            }
        }
    }

    /// TranslateTrmThroughPolygon for one collision-model polygon (0x141945f50): contents, facing, then
    /// the three feature tests; internal edges (EDGE_INTERNAL) are skipped by the edge and vertex tests.
    fn cm_polygon(&mut self, bi: usize, sm: &SubModel, p: &idres::bcm::Polygon, mask: u32, edge_checked: &mut HashSet<usize>, vert_checked: &mut HashSet<usize>) {
        let Some(surf) = sm.surfaces.get(p.surface as usize) else { return };
        if surf.contents & mask == 0 || p.num_edges < 3 {
            return;
        }
        let pl = sm.polygon_plane(p);
        let normal = Vec3::new(pl[0], pl[1], pl[2]);
        let dist = -pl[3];
        let dir = self.dir;
        if normal.dot(dir) > 0.0 {
            return;
        }
        let flags = surf.surface_flags;
        let trm = self.trm;
        let pts = polygon_points(sm, p);
        for (vi, &v) in trm.verts.iter().enumerate() {
            if !self.used_vert[vi] {
                continue;
            }
            let q = self.start + v;
            let f = plane_fraction(normal, dist, q, q + dir);
            if f < self.fraction && line_through_points(&pts, normal, dist, q, dir) {
                let point = q + dir * f.max(0.0);
                self.record(f, Contact { kind: ContactKind::TrmVertex, point, normal, dist, brush: bi, surface_flags: flags });
            }
        }
        let refs = sm.polygon_edge_refs(p);
        for (ei, e) in trm.edges.iter().enumerate() {
            if !self.used_edge[ei] {
                continue;
            }
            let a0 = self.start + trm.verts[e.v[0]];
            let a1 = self.start + trm.verts[e.v[1]];
            for &r in refs {
                let (edge, _, internal) = SubModel::edge_ref(r);
                if internal || edge_checked.contains(&edge) {
                    continue;
                }
                let (b0, b1) = sm.edge_verts(r);
                self.edge_edge(bi, flags, a0, a1, e.normal, v3(sm.verts[b0]), v3(sm.verts[b1]));
            }
        }
        for &r in refs {
            let (edge, _, internal) = SubModel::edge_ref(r);
            if !edge_checked.insert(edge) || internal {
                continue;
            }
            let (b0, b1) = sm.edge_verts(r);
            for vi in [b0, b1] {
                if vert_checked.insert(vi) {
                    self.vertex_through_trm(bi, flags, v3(sm.verts[vi]));
                }
            }
        }
    }

    /// TranslateTrmEdgeThroughPolygon for one edge pair: the moving edge a0a1 against the static edge b0b1.
    fn edge_edge(&mut self, bi: usize, surface_flags: u32, a0: Vec3, a1: Vec3, a_normal: Vec3, b0: Vec3, b1: Vec3) {
        let dir = self.dir;
        let ea = a1 - a0;
        let eb = b1 - b0;
        let c = ea.cross(eb);
        let denom = dir.dot(c);
        if denom == 0.0 {
            return;
        }
        // Time the edge lines meet; the segments must actually cross there.
        let f1 = (b0 - a0).dot(c) / denom;
        let cc = c.length_squared();
        if cc == 0.0 {
            return;
        }
        let w = b0 - (a0 + dir * f1);
        let u = w.cross(eb).dot(c) / cc;
        let v = w.cross(ea).dot(c) / cc;
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return;
        }
        if f1 < 0.0 {
            return;
        }
        // Same with the trace-model edge pushed out by the epsilon along its edge normal.
        let s = a_normal * CLIP_EPSILON;
        let f2 = (b0 - (a0 + s)).dot(c) / denom;
        if f2 > 1.0 || f1 < f2 {
            return;
        }
        if f2.max(0.0) < self.fraction {
            let mut n = eb.cross(ea).normalize_or_zero();
            if n.dot(dir) > 0.0 {
                n = -n;
            }
            let point = b0 + eb * v;
            self.record(f2, Contact { kind: ContactKind::Edge, point, normal: n, dist: n.dot(b0), brush: bi, surface_flags });
        }
    }
}

impl World {
    pub fn add(&mut self, hull: Hull) -> usize {
        self.brushes.push(hull);
        self.cm = std::sync::OnceLock::new();
        self.brushes.len() - 1
    }

    /// Places a `.bcm` collision model (all its submodels). Traces use its polygons as they are (the
    /// game's tools already merged them and flagged internal edges), filtered by `clip_mask`.
    pub fn add_cm(&mut self, cm: Arc<CollisionModel>, origin: Vec3, axis: Mat3) -> usize {
        let (lo, hi) = (v3(cm.bounds[0]), v3(cm.bounds[1]));
        let corners = (0..8).map(|i| {
            origin + axis * Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z })
        });
        let (min, max) = corners.fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), c| (a.min(c), b.max(c)));
        self.cms.push(CmInstance { cm, origin, axis, min, max, mover: None });
        self.cms.len() - 1
    }

    /// Places a collision model without pushing anything (teleport). Use `Player::push_mover` to move a
    /// mover with the game's push and carry rules.
    pub fn set_cm_transform(&mut self, id: usize, origin: Vec3, axis: Mat3) {
        let inst = &mut self.cms[id];
        let (lo, hi) = (v3(inst.cm.bounds[0]), v3(inst.cm.bounds[1]));
        let corners = (0..8).map(|i| {
            origin + axis * Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z })
        });
        let (min, max) = corners.fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), c| (a.min(c), b.max(c)));
        inst.origin = origin;
        inst.axis = axis;
        inst.min = min;
        inst.max = max;
    }

    /// A placed collision model's current origin and axis.
    pub fn cm_transform(&self, id: usize) -> (Vec3, Mat3) {
        (self.cms[id].origin, self.cms[id].axis)
    }

    /// Marks a collision model as a mover (player physics treats standing on it as riding a pusher).
    pub fn set_cm_mover(&mut self, id: usize, props: MoverProps) {
        self.cms[id].mover = Some(props);
    }

    /// The collision model a contact's `brush` index refers to, if it is one.
    pub fn cm_of_contact(&self, brush: usize) -> Option<(usize, &CmInstance)> {
        let ci = brush.checked_sub(self.brushes.len())?;
        self.cms.get(ci).map(|c| (ci, c))
    }

    /// What traces collide with: each brush's faces with the parts inside or flush against other
    /// brushes removed, and flat/concave edges flagged internal. The game's world collision model is
    /// built this way offline (idTech4 lineage: chop polygons by brushes, merge, find internal edges),
    /// so separate touching brushes behave like one surface instead of snagging at their seams.
    pub fn collision_models(&self) -> &[Hull] {
        self.cm.get_or_init(|| build_collision_models(&self.brushes))
    }

    fn sweep<'a>(&self, trm: &'a Hull, start: Vec3, end: Vec3, gather: bool) -> TraceWork<'a> {
        self.sweep_filtered(trm, start, end, gather, TraceFilter::All)
    }

    fn sweep_filtered<'a>(&self, trm: &'a Hull, start: Vec3, end: Vec3, gather: bool, filter: TraceFilter) -> TraceWork<'a> {
        let mut tw = TraceWork::new(trm, start, end, gather);
        let lo = trm.min + start.min(end) - Vec3::splat(BOUNDS_SLACK + CLIP_EPSILON);
        let hi = trm.max + start.max(end) + Vec3::splat(BOUNDS_SLACK + CLIP_EPSILON);
        let brushes: &[Hull] = if matches!(filter, TraceFilter::Only(_)) { &[] } else { self.collision_models() };
        for (bi, b) in brushes.iter().enumerate() {
            if b.max.cmplt(lo).any() || b.min.cmpgt(hi).any() {
                continue;
            }
            tw.brush(bi, b);
            if tw.contacts.is_none() && tw.fraction == 0.0 {
                return tw;
            }
        }
        for (ci, inst) in self.cms.iter().enumerate() {
            let wanted = match filter {
                TraceFilter::All => true,
                TraceFilter::Without(x) => x != ci,
                TraceFilter::Only(x) => x == ci,
            };
            if !wanted || inst.max.cmplt(lo).any() || inst.min.cmpgt(hi).any() {
                continue;
            }
            let bi = self.brushes.len() + ci;
            if inst.is_identity() {
                tw.collision_model(bi, &inst.cm, lo, hi, self.clip_mask);
            } else {
                // In model space with the trace model rotated into it; results come back to world space.
                let to_model = inst.axis.transpose();
                let trm_m = trm.rotated(to_model);
                let (s, e) = (inst.to_model(start), inst.to_model(end));
                let mut sub = TraceWork::new(&trm_m, s, e, gather);
                sub.fraction = tw.fraction;
                let mlo = trm_m.min + s.min(e) - Vec3::splat(BOUNDS_SLACK + CLIP_EPSILON);
                let mhi = trm_m.max + s.max(e) + Vec3::splat(BOUNDS_SLACK + CLIP_EPSILON);
                sub.collision_model(bi, &inst.cm, mlo, mhi, self.clip_mask);
                let back = |c: Contact| Contact {
                    point: inst.to_world(c.point),
                    normal: inst.axis * c.normal,
                    dist: (inst.axis * c.normal).dot(inst.to_world(c.point)),
                    ..c
                };
                match (&mut tw.contacts, sub.contacts) {
                    (Some(list), Some(found)) => {
                        for c in found {
                            if list.len() < MAX_CONTACTS {
                                list.push(back(c));
                            }
                        }
                    }
                    _ => {
                        if sub.fraction < tw.fraction {
                            tw.fraction = sub.fraction;
                            tw.c = back(sub.c);
                        }
                    }
                }
            }
            if tw.contacts.is_none() && tw.fraction == 0.0 {
                break;
            }
        }
        tw
    }

    /// Sweeps the clip model from `start` to `end` (idClip::Translation → collision model translation).
    /// A zero-length move does not trace at all (0x141944690).
    pub fn translate(&self, trm: &Hull, start: Vec3, end: Vec3) -> Trace {
        self.translate_filtered(trm, start, end, TraceFilter::All)
    }

    /// `translate` against a subset of the collision models (brushes count as world: excluded by Only).
    pub fn translate_filtered(&self, trm: &Hull, start: Vec3, end: Vec3, filter: TraceFilter) -> Trace {
        if start == end {
            return Trace { fraction: 1.0, endpos: start, c: Contact::default() };
        }
        let tw = self.sweep_filtered(trm, start, end, false, filter);
        // 0x141944410: endpos = (end - start)·fraction + start; a fraction that does not move becomes 0.
        let mut fraction = tw.fraction;
        let endpos = (end - start) * fraction + start;
        if fraction < 1.0 && fraction > 0.0 && endpos == start {
            fraction = 0.0;
        }
        Trace { fraction, endpos, c: if fraction < 1.0 { tw.c } else { Contact::default() } }
    }

    /// Position test (idClip::Contents with a trace model): whether `trm` at `origin` is clear of
    /// every brush. Touching does not count as blocked.
    pub fn position_clear(&self, trm: &Hull, origin: Vec3) -> bool {
        let lo = trm.min + origin;
        let hi = trm.max + origin;
        if self.brushes.iter().any(|b| !(b.max.cmplt(lo).any() || b.min.cmpgt(hi).any()) && trm.overlaps(origin, b)) {
            return false;
        }
        self.cms.iter().all(|inst| {
            if inst.max.cmplt(lo).any() || inst.min.cmpgt(hi).any() {
                return true;
            }
            let (m, o) = if inst.is_identity() { (trm.clone(), origin) } else { (trm.rotated(inst.axis.transpose()), inst.to_model(origin)) };
            !cm_overlaps(&inst.cm, &m, o, self.clip_mask)
        })
    }

    /// Contacts of the clip model at `start` when moved by `dir·depth` (idClip::Contacts): every
    /// feature that would come within `CLIP_EPSILON`, at most `MAX_CONTACTS`.
    pub fn contacts(&self, trm: &Hull, start: Vec3, dir: Vec3, depth: f32) -> Vec<Contact> {
        let end = start + dir * depth;
        if start == end {
            return Vec::new();
        }
        self.sweep(trm, start, end, true).contacts.unwrap_or_default()
    }
}

/// Removes the component into `n`, overclipping by 1.001 as the game does.
pub fn clip_velocity(v: Vec3, n: Vec3) -> Vec3 {
    let d = v.dot(n);
    let d = if d >= 0.0 { d / OVERCLIP } else { d * OVERCLIP };
    v - n * d
}

/// Clips `v` against every plane it heads into (0x141957100). Returns true when boxed in (v zeroed).
fn clip_to_planes(v: &mut Vec3, planes: &[Vec3]) -> bool {
    for i in 0..planes.len() {
        if v.dot(planes[i]) >= CLIP_THRESHOLD {
            continue;
        }
        *v = clip_velocity(*v, planes[i]);
        for j in 0..planes.len() {
            if j == i || planes[j].dot(*v) >= CLIP_THRESHOLD {
                continue;
            }
            *v = clip_velocity(*v, planes[j]);
            if v.dot(planes[i]) < 0.0 {
                // Slide along the crease of the two planes.
                let crease = planes[i].cross(planes[j]).normalize_or_zero();
                *v = crease * crease.dot(*v);
                for k in 0..planes.len() {
                    if k != i && k != j && v.dot(planes[k]) < CLIP_THRESHOLD {
                        *v = Vec3::ZERO;
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Result of one step trace, or of the whole slide.
#[derive(Debug, Clone, Copy)]
pub struct StepTrace {
    pub fraction: f32,
    pub endpos: Vec3,
    pub c: Contact,
    /// World-Z change from the step up/down.
    pub step: f32,
}

/// One slide pass (0x141582300): move, then up by `step_up`, across, and down by `step_total`;
/// FUN_141958bc0 picks the stepped result unless the down trace lands on a slope steeper than 45°.
pub fn step_trace(world: &World, trm: &Hull, start: Vec3, end: Vec3, gdir: Vec3, step_up: f32, step_total: f32) -> StepTrace {
    let a = world.translate(trm, start, end);
    let b = world.translate(trm, a.endpos, a.endpos - gdir * step_up);
    let c = world.translate(trm, b.endpos, end - gdir * (b.fraction * step_up));
    let d = world.translate(trm, c.endpos, c.endpos + gdir * step_total);
    let down_hit = d.fraction < 1.0;
    let floor = down_hit && d.c.normal.z > STEP_FLOOR_NORMAL_Z;
    if down_hit && !floor {
        return StepTrace { fraction: a.fraction, endpos: a.endpos, c: a.c, step: 0.0 };
    }
    let fraction = if a.fraction >= 1.0 || c.fraction >= 1.0 { 1.0 } else { (1.0 - a.fraction) * c.fraction + a.fraction };
    StepTrace { fraction, endpos: d.endpos, c: c.c, step: d.endpos.z - a.endpos.z }
}

/// The slide state carried across passes (0x141957580 init, 0x141958ed0 per pass, 0x141957470 finish).
struct SlideState {
    /// Delta still to apply this frame (with half the gravity displacement).
    delta: Vec3,
    /// The delta with the full gravity displacement; reported at the end, clipped by every plane.
    full: Vec3,
    first: Contact,
    remaining: f32,
    step: f32,
    dir: Vec3,
    planes: Vec<Vec3>,
}

impl SlideState {
    fn new(delta: Vec3, gvec: Vec3) -> Self {
        let mut s = Self {
            delta,
            full: Vec3::ZERO,
            first: Contact::default(),
            remaining: 1.0,
            step: 0.0,
            dir: delta.normalize_or_zero(),
            planes: Vec::with_capacity(MAX_CLIP_PLANES),
        };
        // gvec = gravityNormal + gravity·dt² when gravity applies: its length past 1 is the fall this frame.
        let len = gvec.length();
        if len > 1.0 + f32::MIN_POSITIVE {
            let fall = len - 1.0;
            s.full = gvec / len * fall + delta;
            s.delta = (s.full + delta) * 0.5;
        }
        s
    }

    /// Returns true when the slide is finished. Otherwise `tr.c.normal` is replaced by the next delta.
    fn pass(&mut self, tr: &StepTrace) -> Option<Vec3> {
        self.remaining -= self.remaining * tr.fraction;
        self.step += tr.step;
        if tr.fraction >= 1.0 {
            self.remaining = 0.0;
            return None;
        }
        if self.first.kind == ContactKind::None {
            self.first = tr.c;
        }
        let n = tr.c.normal;
        if self.planes.iter().any(|p| n.dot(*p) > SAME_PLANE_DOT) {
            self.delta = clip_velocity(self.delta, n);
            return Some(self.delta);
        }
        if self.planes.len() < MAX_CLIP_PLANES {
            self.planes.push(n);
            if !clip_to_planes(&mut self.delta, &self.planes) {
                if self.delta.dot(self.dir) < BACKWARD_THRESHOLD {
                    self.delta -= self.dir * self.dir.dot(self.delta);
                }
                return Some(self.delta);
            }
        }
        self.delta = Vec3::ZERO;
        self.remaining = 0.0;
        None
    }

    fn finish(&mut self) {
        if self.delta != Vec3::ZERO && self.full != Vec3::ZERO {
            clip_to_planes(&mut self.full, &self.planes);
            self.delta = self.full;
        }
    }
}

/// Output of SlideMoveContacts.
#[derive(Debug, Clone)]
pub struct SlideResult {
    /// The last pass's fraction.
    pub fraction: f32,
    pub endpos: Vec3,
    /// First surface hit this frame (kind None when the slide never collided).
    pub first: Contact,
    /// The frame's delta after clipping (SlideMove turns it back into velocity).
    pub displacement: Vec3,
    /// Summed world-Z step of all passes.
    pub step: f32,
    /// Contacts gathered at the end position, `CONTACT_DISTANCE` along gravity.
    pub contacts: Vec<Contact>,
}

/// SlideMoveContacts (0x141633c30 → 0x141633ef0 → 0x141581ba0 → 0x141581890): slides `trm` from `origin`
/// by `delta` with up to four step-trace passes. `gvec` is the gravity normal, plus gravity·dt² when the
/// caller integrates gravity; `step_total` is step-up plus step-down height.
pub fn slide_move_contacts(world: &World, trm: &Hull, origin: Vec3, delta: Vec3, gvec: Vec3, step_up: f32, step_total: f32) -> SlideResult {
    let mut s = SlideState::new(delta, gvec);
    let gdir = gvec.normalize_or_zero();
    let mut start = origin;
    let mut end = origin + s.delta;
    let mut last = StepTrace { fraction: 1.0, endpos: end, c: Contact::default(), step: 0.0 };
    for _ in 0..SLIDE_PASSES {
        last = step_trace(world, trm, start, end, gdir, step_up, step_total);
        match s.pass(&last) {
            None => break,
            Some(next) => {
                start = last.endpos;
                end = last.endpos + next * s.remaining;
            }
        }
    }
    s.finish();
    let contacts = world.contacts(trm, last.endpos, gdir, CONTACT_DISTANCE);
    SlideResult { fraction: last.fraction, endpos: last.endpos, first: s.first, displacement: s.delta, step: s.step, contacts }
}

impl World {
    /// Ray against all brushes (Cyrus–Beck on face planes). Returns distance, surface normal, brush index.
    pub fn ray(&self, start: Vec3, dir: Vec3, max_dist: f32) -> Option<(f32, Vec3, usize)> {
        let mut best: Option<(f32, Vec3, usize)> = None;
        for (i, b) in self.brushes.iter().enumerate() {
            let (mut t0, mut t1) = (0.0f32, max_dist);
            let mut n0 = Vec3::ZERO;
            let mut ok = true;
            for p in &b.polys {
                let dist = p.normal.dot(start) - p.dist;
                let denom = p.normal.dot(dir);
                if denom.abs() < 1e-9 {
                    if dist > 0.0 {
                        ok = false;
                        break;
                    }
                    continue;
                }
                let t = -dist / denom;
                if denom < 0.0 {
                    if t > t0 {
                        t0 = t;
                        n0 = p.normal;
                    }
                } else if t < t1 {
                    t1 = t;
                }
                if t0 > t1 {
                    ok = false;
                    break;
                }
            }
            if ok && n0 != Vec3::ZERO && best.is_none_or(|(bt, _, _)| t0 < bt) {
                best = Some((t0, n0, i));
            }
        }
        let end = start + dir * max_dist;
        for (ci, inst) in self.cms.iter().enumerate() {
            let (lo, hi) = (start.min(end), start.max(end));
            if inst.max.cmplt(lo).any() || inst.min.cmpgt(hi).any() {
                continue;
            }
            let (s, d) = (inst.to_model(start), inst.axis.transpose() * dir);
            let e = s + d * max_dist;
            let (mlo, mhi) = (s.min(e) - Vec3::splat(BOUNDS_SLACK), s.max(e) + Vec3::splat(BOUNDS_SLACK));
            let mut polys = Vec::new();
            for sm in submodels_in(&inst.cm, mlo, mhi, self.shot_mask) {
                kd_candidates(sm, mlo, mhi, &mut polys, None);
                for &pi in &polys {
                    let p = &sm.polygons[pi as usize];
                    if sm.surfaces.get(p.surface as usize).is_none_or(|x| x.contents & self.shot_mask == 0) || p.num_edges < 3 {
                        continue;
                    }
                    let pl = sm.polygon_plane(p);
                    let n = Vec3::new(pl[0], pl[1], pl[2]);
                    let denom = n.dot(d);
                    if denom >= 0.0 {
                        continue;
                    }
                    let t = -(n.dot(s) + pl[3]) / denom;
                    if !(0.0..=max_dist).contains(&t) || best.is_some_and(|(bt, _, _)| t >= bt) {
                        continue;
                    }
                    if line_through_points(&polygon_points(sm, p), n, -pl[3], s, d) {
                        best = Some((t, inst.axis * n, self.brushes.len() + ci));
                    }
                }
            }
        }
        best
    }
}

// ---- World collision build ----------------------------------------------------------------------

/// Plane-side tolerance of the build.
const CHOP_EPSILON: f32 = 0.01;

/// Splits the convex loop `pts` by the plane `n·x = d`; points on the plane go to both sides.
fn split_loop(pts: &[Vec3], n: Vec3, d: f32) -> (Vec<Vec3>, Vec<Vec3>) {
    let dist: Vec<f32> = pts.iter().map(|p| n.dot(*p) - d).collect();
    let (mut front, mut back) = (Vec::new(), Vec::new());
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        let (dp, dq) = (dist[i], dist[j]);
        if dp >= -CHOP_EPSILON {
            front.push(pts[i]);
        }
        if dp <= CHOP_EPSILON {
            back.push(pts[i]);
        }
        if (dp > CHOP_EPSILON && dq < -CHOP_EPSILON) || (dp < -CHOP_EPSILON && dq > CHOP_EPSILON) {
            let m = pts[i] + (pts[j] - pts[i]) * (dp / (dp - dq));
            front.push(m);
            back.push(m);
        }
    }
    (front, back)
}

fn loop_area(pts: &[Vec3], n: Vec3) -> f32 {
    let mut a = Vec3::ZERO;
    for i in 1..pts.len().saturating_sub(1) {
        a += (pts[i] - pts[0]).cross(pts[i + 1] - pts[0]);
    }
    0.5 * a.dot(n)
}

/// The parts of the face `pts` (of brush `i`, normal `n`) outside brush `j`. A face lying on one of
/// `j`'s sides facing the other way is inside it (two brushes pressed together); one facing the same
/// way is outside, except where it duplicates `j`'s face, which the lower-numbered brush keeps.
fn chop_by_brush(pts: Vec<Vec3>, n: Vec3, i: usize, j: usize, brush: &Hull) -> Vec<Vec<Vec3>> {
    let mut outside = Vec::new();
    let mut inside = pts;
    let mut duplicate = false;
    for side in &brush.polys {
        let dist: Vec<f32> = inside.iter().map(|p| side.normal.dot(*p) - side.dist).collect();
        if dist.iter().all(|d| d.abs() <= CHOP_EPSILON) {
            if side.normal.dot(n) > 0.0 {
                duplicate = true;
            }
            continue;
        }
        if dist.iter().all(|&d| d >= -CHOP_EPSILON) {
            outside.push(inside);
            return outside;
        }
        if dist.iter().all(|&d| d <= CHOP_EPSILON) {
            continue;
        }
        let (front, back) = split_loop(&inside, side.normal, side.dist);
        if front.len() >= 3 && loop_area(&front, n) > CHOP_EPSILON {
            outside.push(front);
        }
        inside = back;
        if inside.len() < 3 || loop_area(&inside, n) <= CHOP_EPSILON {
            return outside;
        }
    }
    if duplicate && i < j {
        outside.push(inside);
    }
    outside
}

fn build_collision_models(brushes: &[Hull]) -> Vec<Hull> {
    let mut models: Vec<Hull> = Vec::with_capacity(brushes.len());
    for (i, b) in brushes.iter().enumerate() {
        let lo = b.min - Vec3::splat(CHOP_EPSILON);
        let hi = b.max + Vec3::splat(CHOP_EPSILON);
        let mut verts: Vec<Vec3> = Vec::new();
        let mut polys: Vec<Poly> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        for face in &b.polys {
            let mut pieces = vec![face.verts.iter().map(|&v| b.verts[v]).collect::<Vec<Vec3>>()];
            for (j, other) in brushes.iter().enumerate() {
                if j == i || other.max.cmplt(lo).any() || other.min.cmpgt(hi).any() {
                    continue;
                }
                pieces = pieces.into_iter().flat_map(|pts| chop_by_brush(pts, face.normal, i, j, other)).collect();
            }
            for pts in pieces {
                let mut loop_: Vec<usize> = Vec::with_capacity(pts.len());
                for p in pts {
                    let vi = match verts.iter().position(|v| v.distance_squared(p) <= CHOP_EPSILON * CHOP_EPSILON) {
                        Some(vi) => vi,
                        None => {
                            verts.push(p);
                            verts.len() - 1
                        }
                    };
                    if loop_.last() != Some(&vi) && loop_.first() != Some(&vi) {
                        loop_.push(vi);
                    }
                }
                if loop_.len() < 3 {
                    continue;
                }
                let mut pe = Vec::with_capacity(loop_.len());
                for k in 0..loop_.len() {
                    let (a, c) = (loop_[k], loop_[(k + 1) % loop_.len()]);
                    let e = match edges.iter().position(|e| (e.v[0] == a && e.v[1] == c) || (e.v[0] == c && e.v[1] == a)) {
                        Some(e) => e,
                        None => {
                            edges.push(Edge { v: [a, c], normal: Vec3::ZERO, internal: false });
                            edges.len() - 1
                        }
                    };
                    pe.push(e);
                }
                polys.push(Poly { normal: face.normal, dist: face.dist, verts: loop_, edges: pe });
            }
        }
        let (min, max) = if verts.is_empty() {
            (b.min, b.min)
        } else {
            (verts.iter().copied().reduce(Vec3::min).unwrap(), verts.iter().copied().reduce(Vec3::max).unwrap())
        };
        models.push(Hull { verts, faces: Vec::new(), polys, edges, min, max });
    }
    mark_internal_edges(&mut models);
    models
}

/// An edge is internal when, for every polygon using it, the rest of its length is continued by
/// polygons (of any brush) that lie flat beside it or rise in front of it.
fn mark_internal_edges(models: &mut [Hull]) {
    let mut flags: Vec<Vec<bool>> = Vec::with_capacity(models.len());
    for (mi, m) in models.iter().enumerate() {
        let mut f = vec![false; m.edges.len()];
        for (ei, e) in m.edges.iter().enumerate() {
            let (a, b) = (m.verts[e.v[0]], m.verts[e.v[1]]);
            let owners: Vec<usize> = (0..m.polys.len()).filter(|&pi| m.polys[pi].edges.contains(&ei)).collect();
            f[ei] = !owners.is_empty() && owners.iter().all(|&pi| edge_covered(models, mi, pi, a, b));
        }
        flags.push(f);
    }
    for (m, f) in models.iter_mut().zip(flags) {
        for (e, internal) in m.edges.iter_mut().zip(f) {
            e.internal = internal;
        }
    }
}

/// Whether the segment `a..b` of polygon `pi` in model `mi` is continued along its whole length by
/// polygons that do not make it a convex (exposed) edge.
fn edge_covered(models: &[Hull], mi: usize, pi: usize, a: Vec3, b: Vec3) -> bool {
    let p = &models[mi].polys[pi];
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= 0.0 {
        return false;
    }
    let on_line = |x: Vec3| (x - a).cross(ab).length_squared() <= CHOP_EPSILON * CHOP_EPSILON * len2;
    let lo = a.min(b) - Vec3::splat(CHOP_EPSILON);
    let hi = a.max(b) + Vec3::splat(CHOP_EPSILON);
    let mut spans: Vec<(f32, f32)> = Vec::new();
    for (mj, m) in models.iter().enumerate() {
        if m.max.cmplt(lo).any() || m.min.cmpgt(hi).any() {
            continue;
        }
        for (qj, q) in m.polys.iter().enumerate() {
            if mj == mi && qj == pi {
                continue;
            }
            let centroid = q.verts.iter().map(|&v| m.verts[v]).sum::<Vec3>() / q.verts.len() as f32;
            let s = p.normal.dot(centroid) - p.dist;
            let exposed = if s < -CHOP_EPSILON {
                true
            } else if s <= CHOP_EPSILON {
                q.normal.dot(p.normal) < 0.99
            } else {
                false
            };
            if exposed {
                continue;
            }
            for &qe in &q.edges {
                let (c, d) = (m.verts[m.edges[qe].v[0]], m.verts[m.edges[qe].v[1]]);
                if on_line(c) && on_line(d) {
                    let (t0, t1) = ((c - a).dot(ab) / len2, (d - a).dot(ab) / len2);
                    spans.push((t0.min(t1), t0.max(t1)));
                }
            }
        }
    }
    spans.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let tol = CHOP_EPSILON / len2.sqrt();
    let mut reach = 0.0f32;
    for (t0, t1) in spans {
        if t0 > reach + tol {
            break;
        }
        reach = reach.max(t1);
    }
    reach >= 1.0 - tol
}

/// Whether the trace model at `origin` (model space) overlaps solid collision-model geometry under
/// `mask`: a trace-model vertex inside a brush, a polygon edge through a trace-model face, a trace-model
/// edge through a polygon, or a polygon vertex inside the trace model. Touching does not count.
fn cm_overlaps(cm: &CollisionModel, trm: &Hull, origin: Vec3, mask: u32) -> bool {
    const EPS: f32 = 1e-3;
    let lo = trm.min + origin - Vec3::splat(EPS);
    let hi = trm.max + origin + Vec3::splat(EPS);
    let tverts: Vec<Vec3> = trm.verts.iter().map(|&v| v + origin).collect();
    let tplanes: Vec<f32> = trm.polys.iter().map(|p| p.dist + p.normal.dot(origin)).collect();
    let inside_trm = |x: Vec3| trm.polys.iter().zip(&tplanes).all(|(p, &d)| p.normal.dot(x) - d < -EPS);
    // Segment a..b crossing the convex loop (points `pts`, plane n·x = d) strictly inside.
    let crosses = |a: Vec3, b: Vec3, pts: &[Vec3], n: Vec3, d: f32| {
        let (da, db) = (n.dot(a) - d, n.dot(b) - d);
        if (da > -EPS && db > -EPS) || (da < EPS && db < EPS) {
            return false;
        }
        let x = a + (b - a) * (da / (da - db));
        (0..pts.len()).all(|k| (pts[(k + 1) % pts.len()] - pts[k]).cross(x - pts[k]).dot(n) > EPS)
    };
    let tfaces: Vec<Vec<Vec3>> = trm.polys.iter().map(|p| p.verts.iter().map(|&v| tverts[v]).collect()).collect();
    let (mut polys, mut brushes) = (Vec::new(), Vec::new());
    for sm in submodels_in(cm, lo, hi, mask) {
        kd_candidates(sm, lo, hi, &mut polys, Some(&mut brushes));
        for &bi in &brushes {
            let b = &sm.brushes[bi as usize];
            if sm.surfaces.get(b.surface as usize).is_none_or(|x| x.contents & mask == 0) {
                continue;
            }
            let planes = &sm.planes[b.first_plane as usize..][..b.num_planes as usize];
            if tverts.iter().any(|&v| planes.iter().all(|pl| pl[0] * v.x + pl[1] * v.y + pl[2] * v.z + pl[3] < -EPS)) {
                return true;
            }
        }
        for &pi in &polys {
            let p = &sm.polygons[pi as usize];
            if sm.surfaces.get(p.surface as usize).is_none_or(|x| x.contents & mask == 0) || p.num_edges < 3 {
                continue;
            }
            let pts = polygon_points(sm, p);
            if pts.iter().any(|&x| inside_trm(x)) {
                return true;
            }
            for k in 0..pts.len() {
                let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                if trm.polys.iter().zip(&tplanes).zip(&tfaces).any(|((tp, &d), f)| crosses(a, b, f, tp.normal, d)) {
                    return true;
                }
            }
            let pl = sm.polygon_plane(p);
            let (n, d) = (Vec3::new(pl[0], pl[1], pl[2]), -pl[3]);
            if trm.edges.iter().any(|e| crosses(tverts[e.v[0]], tverts[e.v[1]], &pts, n, d)) {
                return true;
            }
        }
    }
    false
}
