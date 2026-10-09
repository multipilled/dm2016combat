//! Discrete animation models (`generated/discreteanimation/<lwo path>.dmodel`, idDiscreteAnimationModelData):
//! the breakable piece models effects throw around (shell casings, gibs). Big-endian except the idStr
//! lengths (u32 little-endian):
//!
//! - u32 0x02024d44, u32 sourceTimestamp, f32 maxRadius, idBounds bounds
//! - u32 n, idTraceModel[n] (each the raw 0x600-byte struct: vertsX/Y/Z[32], edgeNormalX/Y/Z[32],
//!   polyPlaneX/Y/Z/W[16], polyEdges[16][16] (bit 7 picks the edge's vertex, low 7 bits the edge),
//!   numPolyEdges[16], edges[32] {u16 v[2]}, type, numVerts, numEdges, numPolys, maxPolyEdges, offset,
//!   bounds, radius, isConvex) — one per piece, in piece-local space
//! - u32 n, idStr traceMaterials[n]
//! - u32 n, idJointMat transforms[n] (3x4 rows: rotation | translation), the pieces' rest poses
//! - u32 n render surfaces: idStr material, i32 jointOffset, numJoints, numVerts, numIndexes,
//!   idDrawVert[numVerts] (48 bytes: xyz, st, normal u8x4, tangent u8x4 ([3] = polarity), colour u8x4 =
//!   the vertex's piece, vmtrTC, vmtrSB u16x4), u16 indexes[numIndexes]
//! - u32 n source surfaces: idStr material, u32 checksum, i32 renderSurface, firstVertex, lastVertex
//! - u8 baseModel present

use anyhow::{Context, Result, bail, ensure};
use glam::{Vec2, Vec3};

pub const MAGIC: u32 = 0x0202_4d44;

/// One idTraceModel polygon: plane n·p + w = 0 (n outward) and its edge loop (edge index, vertex side).
#[derive(Debug, Clone, PartialEq)]
pub struct TrmPoly {
    pub normal: Vec3,
    pub w: f32,
    pub edges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceModel {
    pub kind: u32,
    pub verts: Vec<Vec3>,
    pub edges: Vec<[u16; 2]>,
    pub polys: Vec<TrmPoly>,
    pub max_poly_edges: u32,
    pub offset: Vec3,
    pub bounds: [Vec3; 2],
    pub radius: f32,
    pub convex: bool,
}

impl TraceModel {
    /// The vertex a polygon edge entry starts from (edges[e].v[side]).
    pub fn edge_vertex(&self, (e, side): (usize, usize)) -> usize {
        self.edges[e][side] as usize
    }

    /// idTraceModel::Shrink (0x1402d2c30), non-polygon models: every plane moves inward by `m`
    /// (w += m) and each vertex is rebuilt from its planes: the first incident plane, the incident plane
    /// least parallel to it (their intersection line), and the incident plane most aligned with that line.
    /// A vertex with fewer than two planes (or near-parallel planes) moves along the first plane's normal.
    pub fn shrink(&mut self, m: f32) {
        let mut incident: Vec<Vec<usize>> = vec![Vec::new(); self.verts.len()];
        for (pi, p) in self.polys.iter_mut().enumerate() {
            p.w += m;
            for &e in &p.edges {
                let v = self.edges[e.0][e.1] as usize;
                if v < incident.len() {
                    incident[v].push(pi);
                }
            }
        }
        for (vi, inc) in incident.iter().enumerate() {
            let Some(&p0) = inc.first() else { continue };
            let (n0, w0) = (self.polys[p0].normal, self.polys[p0].w);
            let along = |v: &mut Vec3| *v -= n0 * m;
            if inc.len() < 2 {
                along(&mut self.verts[vi]);
                continue;
            }
            // Least parallel plane to p0.
            let (mut best, mut n1, mut w1) = (1.0f32, Vec3::ZERO, 0.0f32);
            for &pi in &inc[1..] {
                let d = self.polys[pi].normal.dot(n0).abs();
                if d < best {
                    best = d;
                    n1 = self.polys[pi].normal;
                    w1 = self.polys[pi].w;
                }
            }
            if best > 0.9999 {
                along(&mut self.verts[vi]);
                continue;
            }
            // Line of the two planes: point on it and direction.
            let d = n1.dot(n0);
            let inv = 1.0 / (1.0 - d * d);
            let a = (-w0 - -(w1 * d)) * inv;
            let b = (-w1 - -(w0 * d)) * inv;
            let start = n0 * a + n1 * b;
            let dir = Vec3::new(n1.z * n0.y - n1.y * n0.z, n1.x * n0.z - n1.z * n0.x, n1.y * n0.x - n1.x * n0.y);
            if dir.abs_diff_eq(Vec3::ZERO, 1e-5) {
                along(&mut self.verts[vi]);
                continue;
            }
            let (mut best, mut n2, mut w2) = (0.0f32, Vec3::ZERO, 0.0f32);
            for &pi in &inc[1..] {
                let d = self.polys[pi].normal.dot(dir).abs();
                if best < d {
                    best = d;
                    n2 = self.polys[pi].normal;
                    w2 = self.polys[pi].w;
                }
            }
            let t = (n2.dot(start) - -w2) / n2.dot(dir);
            self.verts[vi] = start - dir * t;
        }
    }

    /// Volume, centre of mass and inertia tensor about the centre of mass at density 1 (rows), from the
    /// polygon loops split into tetrahedra around the vertex centroid (convex models).
    pub fn mass_properties(&self) -> (f32, Vec3, [Vec3; 3]) {
        let c0 = self.verts.iter().copied().sum::<Vec3>() / self.verts.len().max(1) as f32;
        let (mut vol, mut first) = (0.0f64, glam::DVec3::ZERO);
        // Second moments about c0: integral of x_i x_j.
        let mut second = [[0.0f64; 3]; 3];
        for p in &self.polys {
            let loop_: Vec<Vec3> = p.edges.iter().map(|&e| self.verts[self.edge_vertex(e)]).collect();
            for k in 1..loop_.len().saturating_sub(1) {
                let (a, b, c) = ((loop_[0] - c0).as_dvec3(), (loop_[k] - c0).as_dvec3(), (loop_[k + 1] - c0).as_dvec3());
                let mut v6 = a.dot(b.cross(c));
                // Loops wind either way; orient each tetrahedron away from the centroid.
                if v6 < 0.0 {
                    v6 = -v6;
                }
                let v = v6 / 6.0;
                vol += v;
                first += (a + b + c) * (v / 4.0);
                // Tetrahedron (0, a, b, c): integral x_i x_j = v/20 * (sum of products + sum of squares).
                let s = a + b + c;
                for i in 0..3 {
                    for j in 0..3 {
                        let q = a[i] * a[j] + b[i] * b[j] + c[i] * c[j];
                        second[i][j] += v / 20.0 * (q + s[i] * s[j]);
                    }
                }
            }
        }
        if vol <= 0.0 {
            return (0.0, c0, [Vec3::X, Vec3::Y, Vec3::Z]);
        }
        let com = first / vol;
        // Shift second moments to the centre of mass.
        let mut m = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = second[i][j] - vol * com[i] * com[j];
            }
        }
        let tr = m[0][0] + m[1][1] + m[2][2];
        let mut it = [Vec3::ZERO; 3];
        for i in 0..3 {
            for j in 0..3 {
                let v = if i == j { tr - m[i][i] } else { -m[i][j] };
                it[i][j] = v as f32;
            }
        }
        (vol as f32, c0 + com.as_vec3(), it)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawVert {
    pub xyz: Vec3,
    pub st: Vec2,
    pub normal: [u8; 4],
    pub tangent: [u8; 4],
    pub color: [u8; 4],
    pub vmtr_tc: Vec2,
    pub vmtr_sb: [u16; 4],
}

impl DrawVert {
    /// The piece (joint) this vertex follows.
    pub fn piece(&self) -> usize {
        self.color[0] as usize
    }
    pub fn normal_f32(&self) -> Vec3 {
        Vec3::new(self.normal[0] as f32 / 255.0 * 2.0 - 1.0, self.normal[1] as f32 / 255.0 * 2.0 - 1.0, self.normal[2] as f32 / 255.0 * 2.0 - 1.0)
    }
    pub fn tangent_f32(&self) -> Vec3 {
        Vec3::new(self.tangent[0] as f32 / 255.0 * 2.0 - 1.0, self.tangent[1] as f32 / 255.0 * 2.0 - 1.0, self.tangent[2] as f32 / 255.0 * 2.0 - 1.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    pub material: String,
    pub joint_offset: i32,
    pub num_joints: i32,
    pub verts: Vec<DrawVert>,
    pub indexes: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSurface {
    pub material: String,
    pub checksum: u32,
    pub render_surface: i32,
    pub first_vertex: i32,
    pub last_vertex: i32,
}

/// idJointMat: rotation rows and translation (p' = R p + t).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointMat {
    pub rows: [Vec3; 3],
    pub t: Vec3,
}

impl JointMat {
    pub fn point(&self, p: Vec3) -> Vec3 {
        Vec3::new(self.rows[0].dot(p), self.rows[1].dot(p), self.rows[2].dot(p)) + self.t
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DModel {
    pub timestamp: u32,
    pub max_radius: f32,
    pub bounds: [Vec3; 2],
    pub trace_models: Vec<TraceModel>,
    pub trace_materials: Vec<String>,
    pub transforms: Vec<JointMat>,
    pub surfaces: Vec<Surface>,
    pub source_surfaces: Vec<SourceSurface>,
    pub has_base_model: bool,
}

struct Rd<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.o + n <= self.b.len(), "dmodel truncated at {:#x} (+{n})", self.o);
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn vec3(&mut self) -> Result<Vec3> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
    fn str(&mut self) -> Result<String> {
        let n = u32::from_le_bytes(self.take(4)?.try_into()?) as usize;
        ensure!(n < 4096, "dmodel string length {n}");
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn count(&mut self, max: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        ensure!(n <= max, "dmodel count {n} at {:#x}", self.o - 4);
        Ok(n)
    }
}

fn be_f32(b: &[u8], o: usize) -> f32 {
    f32::from_bits(u32::from_be_bytes(b[o..o + 4].try_into().unwrap()))
}
fn be_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

fn trace_model(t: &[u8]) -> Result<TraceModel> {
    let kind = be_u32(t, 0x5c0);
    let (nv, ne, np) = (be_u32(t, 0x5c4) as usize, be_u32(t, 0x5c8) as usize, be_u32(t, 0x5cc) as usize);
    ensure!(nv <= 32 && ne <= 32 && np <= 16, "trace model counts {nv}/{ne}/{np}");
    let verts = (0..nv).map(|i| Vec3::new(be_f32(t, i * 4), be_f32(t, 0x80 + i * 4), be_f32(t, 0x100 + i * 4))).collect();
    // Edges may be 0-based with a trailing unused entry; keep every stored slot the polygons can index.
    let edges = (0..32).map(|i| [u16::from_be_bytes([t[0x540 + i * 4], t[0x541 + i * 4]]), u16::from_be_bytes([t[0x542 + i * 4], t[0x543 + i * 4]])]).collect();
    let polys = (0..np)
        .map(|p| {
            let n = (be_u32(t, 0x500 + p * 4) as usize).min(16);
            TrmPoly {
                normal: Vec3::new(be_f32(t, 0x300 + p * 4), be_f32(t, 0x340 + p * 4), be_f32(t, 0x380 + p * 4)),
                w: be_f32(t, 0x3c0 + p * 4),
                edges: (0..n).map(|k| t[0x400 + p * 16 + k]).map(|b| ((b & 0x7f) as usize, (b >> 7) as usize)).collect(),
            }
        })
        .collect();
    Ok(TraceModel {
        kind,
        verts,
        edges,
        polys,
        max_poly_edges: be_u32(t, 0x5d0),
        offset: Vec3::new(be_f32(t, 0x5d4), be_f32(t, 0x5d8), be_f32(t, 0x5dc)),
        bounds: [Vec3::new(be_f32(t, 0x5e0), be_f32(t, 0x5e4), be_f32(t, 0x5e8)), Vec3::new(be_f32(t, 0x5ec), be_f32(t, 0x5f0), be_f32(t, 0x5f4))],
        radius: be_f32(t, 0x5f8),
        convex: t[0x5fc] != 0,
    })
}

impl DModel {
    pub fn parse(b: &[u8]) -> Result<DModel> {
        let mut r = Rd { b, o: 0 };
        let magic = r.u32()?;
        if magic != MAGIC {
            bail!("not a dmodel (magic {magic:#x})");
        }
        let timestamp = r.u32()?;
        let max_radius = r.f32()?;
        let bounds = [r.vec3()?, r.vec3()?];
        let n = r.count(4096)?;
        let mut trace_models = Vec::with_capacity(n);
        for _ in 0..n {
            trace_models.push(trace_model(r.take(0x600)?)?);
        }
        let n = r.count(4096)?;
        let trace_materials = (0..n).map(|_| r.str()).collect::<Result<Vec<_>>>()?;
        let n = r.count(4096)?;
        let mut transforms = Vec::with_capacity(n);
        for _ in 0..n {
            let mut m = [0.0f32; 12];
            for v in &mut m {
                *v = r.f32()?;
            }
            transforms.push(JointMat { rows: [Vec3::new(m[0], m[1], m[2]), Vec3::new(m[4], m[5], m[6]), Vec3::new(m[8], m[9], m[10])], t: Vec3::new(m[3], m[7], m[11]) });
        }
        let n = r.count(1024)?;
        let mut surfaces = Vec::with_capacity(n);
        for _ in 0..n {
            let material = r.str()?;
            let (joint_offset, num_joints, nv, ni) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
            ensure!((0..=0x10000).contains(&nv) && (0..=0x30000).contains(&ni), "surface counts {nv}/{ni}");
            let mut verts = Vec::with_capacity(nv as usize);
            for _ in 0..nv {
                let xyz = r.vec3()?;
                let st = Vec2::new(r.f32()?, r.f32()?);
                let normal: [u8; 4] = r.take(4)?.try_into()?;
                let tangent: [u8; 4] = r.take(4)?.try_into()?;
                let color: [u8; 4] = r.take(4)?.try_into()?;
                let vmtr_tc = Vec2::new(r.f32()?, r.f32()?);
                let vmtr_sb = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
                verts.push(DrawVert { xyz, st, normal, tangent, color, vmtr_tc, vmtr_sb });
            }
            let indexes = (0..ni).map(|_| r.u16()).collect::<Result<Vec<_>>>()?;
            surfaces.push(Surface { material, joint_offset, num_joints, verts, indexes });
        }
        let n = r.count(0x10000)?;
        let mut source_surfaces = Vec::with_capacity(n);
        for _ in 0..n {
            let material = r.str()?;
            source_surfaces.push(SourceSurface { material, checksum: r.u32()?, render_surface: r.i32()?, first_vertex: r.i32()?, last_vertex: r.i32()? });
        }
        let has_base_model = r.take(1).map(|b| b[0] != 0).unwrap_or(false);
        Ok(DModel { timestamp, max_radius, bounds, trace_models, trace_materials, transforms, surfaces, source_surfaces, has_base_model })
    }

    /// `models/x/y.lwo` -> `generated/discreteanimation/models/x/y.dmodel`.
    pub fn resource(model: &str) -> String {
        let m = model.to_ascii_lowercase().replace('\\', "/");
        let stem = m.rsplit_once('.').map(|(s, _)| s).unwrap_or(&m);
        format!("generated/discreteanimation/{stem}.dmodel")
    }

    pub fn load(c: &idres::Container, model: &str) -> Result<DModel> {
        let path = Self::resource(model);
        let bytes = c.read_by_name(&path).with_context(|| path.clone())?;
        DModel::parse(&bytes).with_context(|| path)
    }

    /// The source material covering a render-surface vertex.
    pub fn source_material(&self, surface: usize, vertex: usize) -> Option<&str> {
        self.source_surfaces
            .iter()
            .find(|s| s.render_surface as usize == surface && (s.first_vertex as usize..=s.last_vertex as usize).contains(&vertex))
            .map(|s| s.material.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shotgun_shell_model() {
        let Some(doom) = idres::find_install() else { return };
        let Ok(c) = idres::Container::open(&doom.join("base"), "gameresources") else { return };
        let m = DModel::load(&c, "models/weapons/shotgun_pump/shotshell_breakable.lwo").unwrap();
        assert_eq!(m.trace_models.len(), 10);
        assert_eq!(m.transforms.len(), 10);
        assert_eq!(m.surfaces.len(), 1);
        let s = &m.surfaces[0];
        assert_eq!((s.verts.len(), s.indexes.len(), s.num_joints), (340, 840, 10));
        assert!(s.indexes.iter().all(|&i| (i as usize) < s.verts.len()));
        assert_eq!(s.verts[34].piece(), 1);
        assert_eq!(m.source_material(0, 0), Some("models/weapons/ammo/combatshotgun_ammo"));
        let t = &m.trace_models[0];
        assert_eq!((t.kind, t.verts.len(), t.polys.len()), (10, 8, 6));
        assert!((m.max_radius - 2.0024).abs() < 1e-3);
    }
}
