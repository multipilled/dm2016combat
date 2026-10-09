//! `.bcm` binary collision models (idCollisionModelLocal): the map's world collision
//! (`maps/<map>/_combo/world.bcm`), per-entity brush models (`maps/<map>/<entity>.bcm`) and prop
//! collision (`generated/cm/models/<path>.bcm`).
//!
//! Decoded from the loader (idCollisionModelLocal vtbl+0x40 = 0x1418dbe40), the submodel byte-swap
//! and pointer fixup (0x1418dd9f0, 0x1418dd330, 0x141955a20) and the query code that walks the data
//! (KD walk 0x141955f30, per polygon 0x141945f50, polygon plane 0x140dbe5e0, edge test 0x1419457c0);
//! notes in gamedata/re/MAPS.md. All numbers big-endian.
//!
//! File: u32 magic `BCM5`..`BCM8`, u32 timestamp, [version >= 7: collisionModelBuildParms_t
//! {u32 filterClipMask, vec3 scale, u8 simplify}], bounds (6 f32), u32, 4 flag bytes (byte 3: submodel
//! data lives in a separate `.tbcm` resource), u32 + submodel KD tree (16-byte nodes), u32 submodel
//! count, per submodel a 32-byte header {u32 size, u32, bounds} directly followed by `size` bytes of
//! submodel data (unless size is 32 = empty, or the data is streamed), then the magic again.
//! Submodel data: a 0x70-byte header {size, size, bounds, u32 kind, 9 x (u32 count, u32 offset),
//! u32 contents} then the nine arrays it points at.

use anyhow::{Context, Result, bail, ensure};

/// `CONTENTS_*` (idContents enum reflection in the exe).
pub mod contents {
    pub const SOLID: u32 = 1;
    pub const OPAQUE: u32 = 2;
    pub const WATER: u32 = 4;
    pub const PLAYERCLIP: u32 = 8;
    pub const MONSTERCLIP: u32 = 16;
    pub const HOLOGRAM: u32 = 32;
    pub const MOVEABLECLIP: u32 = 64;
    pub const SHOTCLIP: u32 = 128;
    pub const IKCLIP: u32 = 256;
    pub const AIAWARE: u32 = 512;
    pub const AI: u32 = 1024;
    pub const PROJECTILE: u32 = 2048;
    pub const CORPSE: u32 = 4096;
    pub const BREAKABLE: u32 = 8192;
    pub const TRIGGER: u32 = 16384;
    pub const PLAYER: u32 = 32768;
    pub const TEAM_ONE: u32 = 65536;
    pub const OBSTACLE: u32 = 131072;
    pub const TEAM_TWO: u32 = 262144;
    pub const PLAYERCOVERCLIP: u32 = 524288;
    pub const SOLIDPUSHABLE: u32 = 1048576;
    pub const PLAYERFOCUS: u32 = 2097152;
    pub const PUSHABLE: u32 = 4194304;
    pub const NO_TELEPORT: u32 = 8388608;
    pub const WALL_GRAB_CLIP: u32 = 16777216;
    pub const TEAM_THREE: u32 = 33554432;
    pub const AAS_SOLID: u32 = 67108864;
    pub const AAS_OBSTACLE: u32 = 134217728;
    pub const AAS_CLUSTER_PORTAL: u32 = 268435456;
    pub const TEAM_FOUR: u32 = 536870912;
    pub const NOCOVER: u32 = 1073741824;
}

/// `SURF_*` surface flags (enum reflection in the exe).
pub mod surface_flags {
    pub const WALL_GRAB: u32 = 1;
    pub const LEDGE_GRAB_ROUNDED: u32 = 2;
    pub const LEDGE_GRAB_ANGLED: u32 = 4;
    pub const NO_PROJECTILE_EXPLOSION_ENTITY: u32 = 8;
    pub const NODAMAGE: u32 = 16;
    pub const SLICK: u32 = 32;
    pub const COLLISION: u32 = 64;
    pub const LADDER: u32 = 128;
    pub const NOIMPACT: u32 = 256;
    pub const NOSTEPS: u32 = 512;
    pub const STAIRS: u32 = 1024;
    pub const OCCLUSION_TEST: u32 = 2048;
    pub const SHADOW_CASTER: u32 = 4096;
    pub const NULLNORMAL: u32 = 8192;
    pub const NOAREAS: u32 = 16384;
    pub const NON_PENETRABLE: u32 = 32768;
    pub const RAIL: u32 = 65536;
    pub const DETONATE_PROJECTILE: u32 = 131072;
    pub const LADDER_TOP: u32 = 2097152;
    pub const LADDER_DISMOUNT_TOP_RIGHT: u32 = 4194304;
    pub const LADDER_DISMOUNT_TOP_LEFT: u32 = 8388608;
}

/// Edge-reference bits in `SubModel::edge_refs`.
pub const EDGE_REVERSED: u16 = 0x8000;
/// Internal edge (between coplanar polygons): the edge test skips it (0x1419458cf).
pub const EDGE_INTERNAL: u16 = 0x4000;
pub const EDGE_INDEX_MASK: u16 = 0x3fff;

/// collisionModelBuildParms_t (version >= 7).
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildParms {
    pub filter_clip_mask: u32,
    pub scale: [f32; 3],
    pub simplify: bool,
}

/// Node of the KD tree over submodels. A child >= 0 is a node index, < 0 is submodel `-1 - child`.
#[derive(Debug, Clone, Copy)]
pub struct TreeNode {
    pub axis: u32,
    pub dist: f32,
    pub children: [i32; 2],
}

/// KD-tree node inside a submodel (16 bytes). Leaves have `axis == -1`; their polygons are
/// `refs[first .. first + num_polygons]`, brushes the `num_brushes` refs after those.
/// Children: `[0]` on the positive side of `dist`, `[1]` on the negative side (0x141955f30).
#[derive(Debug, Clone, Copy)]
pub struct KdNode {
    pub axis: i16,
    pub first: u16,
    pub dist: f32,
    pub children: [u16; 2],
    pub num_polygons: u16,
    pub num_brushes: u16,
}

/// collisionModelSurfaceInfo_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceInfo {
    pub contents: u32,
    pub surface_flags: u32,
    /// `SURFTYPE_*` (0 none, 1 metal, 2 stone, ...).
    pub surface_type: u32,
    pub color: [u8; 3],
}

#[derive(Debug, Clone, Copy)]
pub struct Polygon {
    /// Integer bounds enclosing the polygon (floor of min, ceil of max).
    pub bounds: [[i16; 3]; 2],
    /// Index into `surfaces`.
    pub surface: u8,
    pub num_edges: u8,
    /// Index into `edge_refs`.
    pub first_edge: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Brush {
    pub bounds: [[i16; 3]; 2],
    /// Index into `surfaces` (byte 12, like polygons).
    pub surface: u8,
    pub num_planes: u8,
    /// Index into `planes`.
    pub first_plane: u16,
}

#[derive(Debug, Clone, Default)]
pub struct SubModel {
    pub bounds: [[f32; 3]; 2],
    /// Header word +4 (32 in every shipped file).
    pub header_x4: u32,
    /// Data header +0x20 (copied first by the unpacker 0x141955a20; 0 in shipped files).
    pub kind: u32,
    /// OR of the surface contents; queries skip the submodel when `contents & mask == 0` (0x141955d10).
    pub contents: u32,
    pub nodes: Vec<KdNode>,
    pub refs: Vec<u16>,
    pub surfaces: Vec<SurfaceInfo>,
    pub polygons: Vec<Polygon>,
    /// `EDGE_REVERSED | EDGE_INTERNAL | edge index`. Polygon `p` uses
    /// `edge_refs[p.first_edge .. p.first_edge + p.num_edges]`, a closed vertex loop.
    pub edge_refs: Vec<u16>,
    pub edges: Vec<[u16; 2]>,
    pub verts: Vec<[f32; 3]>,
    /// The two u16 after each vertex (zero in every shipped file seen).
    pub vert_extra: Vec<[u16; 2]>,
    pub brushes: Vec<Brush>,
    /// idPlane (n.x, n.y, n.z, d) with n·p + d = 0; brush `b` uses `planes[b.first_plane ..][..b.num_planes]`.
    pub planes: Vec<[f32; 4]>,
}

impl SubModel {
    /// (edge index, reversed, internal) of a polygon edge reference.
    pub fn edge_ref(r: u16) -> (usize, bool, bool) {
        ((r & EDGE_INDEX_MASK) as usize, r & EDGE_REVERSED != 0, r & EDGE_INTERNAL != 0)
    }

    /// First and second vertex of an edge reference in the polygon's winding.
    pub fn edge_verts(&self, r: u16) -> (usize, usize) {
        let (e, rev, _) = Self::edge_ref(r);
        let [a, b] = self.edges[e];
        if rev { (b as usize, a as usize) } else { (a as usize, b as usize) }
    }

    pub fn polygon_edge_refs(&self, p: &Polygon) -> &[u16] {
        &self.edge_refs[p.first_edge as usize..p.first_edge as usize + p.num_edges as usize]
    }

    /// The polygon's vertex loop (start vertex of each edge in winding order).
    pub fn polygon_verts(&self, p: &Polygon) -> impl Iterator<Item = usize> + '_ {
        self.polygon_edge_refs(p).iter().map(|&r| self.edge_verts(r).0)
    }

    /// The polygon's plane as the game computes it (0x140dbe5e0): from the start vertex `a` of the
    /// first edge and the second edge `b → c`: n = normalize((c − b) × (a − b)), d = −n·b.
    /// Returns idPlane (n, d) with n·p + d = 0. The game normalises with rsqrtss and two Newton steps
    /// (within an ulp of `1/sqrt`).
    pub fn polygon_plane(&self, p: &Polygon) -> [f32; 4] {
        let refs = self.polygon_edge_refs(p);
        let a = self.verts[self.edge_verts(refs[0]).0];
        let (bi, ci) = self.edge_verts(refs[1]);
        let (b, c) = (self.verts[bi], self.verts[ci]);
        let (ax, ay, az) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
        let (cx, cy, cz) = (c[0] - b[0], c[1] - b[1], c[2] - b[2]);
        let nx = cy * az - cz * ay;
        let ny = cz * ax - cx * az;
        let nz = cx * ay - cy * ax;
        let len2 = (ny * ny + nx * nx + nz * nz).max(f32::MIN_POSITIVE);
        let inv = 1.0 / len2.sqrt();
        let (nx, ny, nz) = (nx * inv, ny * inv, nz * inv);
        [nx, ny, nz, -(nx * b[0] + ny * b[1] + nz * b[2])]
    }
}

#[derive(Debug, Clone)]
pub struct CollisionModel {
    /// ASCII version digit (`b'5'` ..= `b'8'`).
    pub version: u8,
    pub timestamp: u32,
    pub build: BuildParms,
    pub bounds: [[f32; 3]; 2],
    /// Header word after the bounds (+0x70).
    pub unknown_70: u32,
    /// Bytes +0x74..+0x77; `[3]` = submodel data streamed from a `.tbcm` resource.
    pub flags: [u8; 4],
    pub tree: Vec<TreeNode>,
    pub submodels: Vec<SubModel>,
}

struct R<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.o..self.o + n).with_context(|| format!("unexpected end at {:#x} (+{n})", self.o))?;
        self.o += n;
        Ok(s)
    }
    fn be32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn bef(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn bounds(&mut self) -> Result<[[f32; 3]; 2]> {
        Ok([[self.bef()?, self.bef()?, self.bef()?], [self.bef()?, self.bef()?, self.bef()?]])
    }
}

fn is_magic(m: u32) -> bool {
    m.wrapping_sub(0x42434d35) < 4
}

impl CollisionModel {
    pub fn parse(b: &[u8]) -> Result<Self> {
        let mut r = R { b, o: 0 };
        let magic = r.be32()?;
        ensure!(is_magic(magic), "bad bcm magic {magic:#x}");
        let version = magic as u8;
        let timestamp = r.be32()?;
        let build = if version >= b'7' {
            let filter_clip_mask = r.be32()?;
            let scale = [r.bef()?, r.bef()?, r.bef()?];
            let simplify = r.take(1)?[0] != 0;
            BuildParms { filter_clip_mask, scale, simplify }
        } else {
            BuildParms { filter_clip_mask: 0, scale: [1.0; 3], simplify: false }
        };
        let bounds = r.bounds()?;
        let unknown_70 = r.be32()?;
        let mut flags: [u8; 4] = r.take(4)?.try_into()?;
        if version == b'5' {
            flags[3] = 0;
        }
        let nt = r.be32()? as usize;
        ensure!(nt < 1 << 24, "implausible tree size {nt}");
        let mut tree = Vec::with_capacity(nt);
        for _ in 0..nt {
            let axis = r.be32()?;
            let dist = r.bef()?;
            let children = [r.be32()? as i32, r.be32()? as i32];
            tree.push(TreeNode { axis, dist, children });
        }
        let ns = r.be32()? as usize;
        ensure!(ns < 1 << 24, "implausible submodel count {ns}");
        let streamed = flags[3] != 0;
        let mut submodels = Vec::with_capacity(ns);
        for i in 0..ns {
            let size = r.be32()? as usize;
            let header_x4 = r.be32()?;
            let sb = r.bounds()?;
            let mut sm = if size == 0x20 {
                SubModel::default()
            } else if streamed {
                bail!("submodel {i}: data is streamed from a .tbcm resource (not supported)");
            } else {
                parse_submodel(r.take(size)?, version).with_context(|| format!("submodel {i}"))?
            };
            sm.bounds = sb;
            sm.header_x4 = header_x4;
            submodels.push(sm);
        }
        let m = r.be32()?;
        ensure!(is_magic(m), "missing trailing magic ({m:#x}) at {:#x}", r.o - 4);
        ensure!(r.o == b.len(), "{} trailing bytes", b.len() - r.o);
        Ok(Self { version, timestamp, build, bounds, unknown_70, flags, tree, submodels })
    }

    pub fn polygon_count(&self) -> usize {
        self.submodels.iter().map(|s| s.polygons.len()).sum()
    }
}

fn parse_submodel(d: &[u8], version: u8) -> Result<SubModel> {
    ensure!(d.len() >= 0x70, "submodel data too short ({})", d.len());
    let u32at = |o: usize| u32::from_be_bytes(d[o..o + 4].try_into().unwrap());
    let u16at = |o: usize| u16::from_be_bytes([d[o], d[o + 1]]);
    let i16at = |o: usize| i16::from_be_bytes([d[o], d[o + 1]]);
    let f32at = |o: usize| f32::from_be_bytes(d[o..o + 4].try_into().unwrap());
    ensure!(u32at(0) as usize == d.len(), "size word {} != data size {}", u32at(0), d.len());
    let kind = u32at(0x20);
    let contents = u32at(0x6c);
    // (count, offset) pairs at 0x24.. (0x141955a20); element sizes from the swap 0x1418dd330.
    const STRIDE: [usize; 9] = [16, 2, 16, 16, 2, 4, 16, 16, 16];
    let mut arr = [(0usize, 0usize); 9];
    for (k, a) in arr.iter_mut().enumerate() {
        let (count, off) = (u32at(0x24 + 8 * k) as usize, u32at(0x28 + 8 * k) as usize);
        ensure!(off + count * STRIDE[k] <= d.len(), "array {k} ({count} at {off:#x}) out of range");
        *a = (count, off);
    }
    let recs = |k: usize| (0..arr[k].0).map(move |i| arr[k].1 + i * STRIDE[k]);
    ensure!(version >= b'8', "KD nodes of version {} are not decoded", version as char);
    let nodes = recs(0)
        .map(|o| KdNode {
            axis: i16at(o),
            first: u16at(o + 2),
            dist: f32at(o + 4),
            children: [u16at(o + 8), u16at(o + 10)],
            num_polygons: u16at(o + 12),
            num_brushes: u16at(o + 14),
        })
        .collect();
    let refs = recs(1).map(u16at).collect();
    let surfaces = recs(2)
        .map(|o| SurfaceInfo { contents: u32at(o), surface_flags: u32at(o + 4), surface_type: u32at(o + 8), color: [d[o + 12], d[o + 13], d[o + 14]] })
        .collect();
    let bounds16 = |o: usize| [[i16at(o), i16at(o + 2), i16at(o + 4)], [i16at(o + 6), i16at(o + 8), i16at(o + 10)]];
    let polygons: Vec<Polygon> = recs(3).map(|o| Polygon { bounds: bounds16(o), surface: d[o + 12], num_edges: d[o + 13], first_edge: u16at(o + 14) }).collect();
    let edge_refs: Vec<u16> = recs(4).map(u16at).collect();
    let edges: Vec<[u16; 2]> = recs(5).map(|o| [u16at(o), u16at(o + 2)]).collect();
    let verts: Vec<[f32; 3]> = recs(6).map(|o| [f32at(o), f32at(o + 4), f32at(o + 8)]).collect();
    let vert_extra = recs(6).map(|o| [u16at(o + 12), u16at(o + 14)]).collect();
    let brushes: Vec<Brush> = recs(7).map(|o| Brush { bounds: bounds16(o), surface: d[o + 12], num_planes: d[o + 13], first_plane: u16at(o + 14) }).collect();
    let planes: Vec<[f32; 4]> = recs(8).map(|o| [f32at(o), f32at(o + 4), f32at(o + 8), f32at(o + 12)]).collect();
    let sm = SubModel {
        bounds: [[0.0; 3]; 2],
        header_x4: 0,
        kind,
        contents,
        nodes,
        refs,
        surfaces,
        polygons,
        edge_refs,
        edges,
        verts,
        vert_extra,
        brushes,
        planes,
    };
    // Cross-check every index so consumers can index without bounds worries.
    for (i, p) in sm.polygons.iter().enumerate() {
        ensure!((p.surface as usize) < sm.surfaces.len().max(1), "polygon {i}: surface {} out of range", p.surface);
        ensure!(p.num_edges >= 2 && p.first_edge as usize + p.num_edges as usize <= sm.edge_refs.len(), "polygon {i}: edges out of range");
    }
    for &e in &sm.edge_refs {
        ensure!(((e & EDGE_INDEX_MASK) as usize) < sm.edges.len(), "edge ref {e:#x} out of range");
    }
    for e in &sm.edges {
        ensure!((e[0] as usize) < sm.verts.len() && (e[1] as usize) < sm.verts.len(), "edge vertex out of range");
    }
    for b in &sm.brushes {
        ensure!(b.first_plane as usize + b.num_planes as usize <= sm.planes.len(), "brush planes out of range");
    }
    for n in &sm.nodes {
        if n.axis < 0 {
            ensure!(n.first as usize + n.num_polygons as usize + n.num_brushes as usize <= sm.refs.len(), "leaf refs out of range");
        } else {
            ensure!(n.axis < 3 && (n.children[0] as usize) < sm.nodes.len() && (n.children[1] as usize) < sm.nodes.len(), "bad KD node");
        }
    }
    Ok(sm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_range() {
        assert!(is_magic(u32::from_be_bytes(*b"BCM8")));
        assert!(is_magic(u32::from_be_bytes(*b"BCM5")));
        assert!(!is_magic(u32::from_be_bytes(*b"BCM9")));
    }
}
