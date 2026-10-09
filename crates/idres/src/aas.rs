//! `.baas_<type>` AI navigation files (idAAS2File, "AAS2" version 3.29): the per-map area awareness
//! system the demons path on (`generated/maps/<map>.baas_monster48` for 48-unit-wide monsters such as
//! the Possessed: entityDef `aiConstants.movement.aasName "aas_monster48"`).
//!
//! Decoded from idAAS2File::LoadBinary (0x141606720), its settings reader (0x14160fff0) and the
//! byte-swap pass (0x1416022f0); field layouts from the exe's reflection (idAAS2File, aas2Area_t,
//! aas2Reachability_t, ... typeinfo; notes gamedata/re/AI.md section "AAS2"). Every number in the file
//! is big-endian.
//!
//! File: u32 magic (reads "2SAA", 0x32534141 big-endian), u8 major 3, u8 minor 29, u32 crc, u32 timestamp,
//! 4 x i32 (firstFakeVertex / Edge / EdgeIndex / Area), the idAAS2Settings block (u32 type; three
//! length-prefixed name strings; idBounds; 2 x i32 primitive modes; vec3 gravityDir; 28 x 4-byte values
//! starting with gravityValue), then 22 lists in this order, each a u32 count followed by count fixed-size
//! records: planes (16), vertices (12), edges (12), edgeIndex (4), reachabilities (40), areas (44),
//! nodes (16), portals (12), portalIndex (4), clusters (16), obstaclePVS (1), reachabilityNames (0x84),
//! animNames (0x80), dependencyNames (0x80), interactionEntityNames (0x80), cover (0x38),
//! areaCoverIndex (4), touchingCoverIndex (4), traversalPoints (0x3c), hintNodes (0x18), trees (0x18),
//! areaBounds (12). (The idAAS2File chokePoints list is not stored.) The intro map's monster48 file
//! parses to its last byte.

use anyhow::{Result, bail, ensure};
/// A position or direction (x, y, z).
pub type Vec3 = [f32; 3];

/// `MAGIC` as LoadBinary compares it after its big-endian read.
pub const MAGIC: u32 = 0x3253_4141;
pub const MAJOR: u8 = 3;
pub const MINOR: u8 = 29;

/// aas2Area_t (0x2c bytes).
#[derive(Clone, Copy, Debug, Default)]
pub struct Area {
    pub travel_flags: u32,
    pub flags: u16,
    /// Edges of the floor polygon (sign kept as stored).
    pub num_edges: i16,
    pub first_edge: i32,
    /// Negative: the area is a cluster portal.
    pub cluster: i16,
    pub cluster_area_num: u16,
    pub obstacle_pvs_offset: u32,
    /// First reachability leaving this area (-1: none); chained through [`Reach::next`].
    pub reach: i32,
    /// First reachability entering this area; chained through [`Reach::rev_next`].
    pub rev_reach: i32,
    pub first_choke_point: u16,
    pub num_choke_points: i16,
    pub first_cover: u16,
    pub num_cover: u16,
    pub first_traversal: i16,
    pub num_traversals: u16,
    pub first_hint_node: u16,
    pub num_hint_nodes: u16,
}

/// aas2Reachability_t (0x28 bytes). Points are whole world units (i16).
#[derive(Clone, Copy, Debug, Default)]
pub struct Reach {
    pub travel_flags: u32,
    pub travel_time: u16,
    pub from_area: u16,
    pub to_area: u16,
    pub reservation_id: u16,
    pub start: [i16; 3],
    pub end: [i16; 3],
    pub reservation_expire_time: i32,
    pub area_tt_ofs_and_number: u32,
    pub next: i32,
    pub rev_next: i32,
}

impl Reach {
    pub fn start(&self) -> Vec3 {
        self.start.map(|c| c as f32)
    }
    pub fn end(&self) -> Vec3 {
        self.end.map(|c| c as f32)
    }
}

/// aas2Node_t: a BSP node; `children` 0 = solid, negative = -(area number), else a node index.
#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    pub plane: u32,
    pub flags: u32,
    pub children: [i32; 2],
}

/// idPlane {a, b, c, d}: distance(p) = a*x + b*y + c*z + d.
#[derive(Clone, Copy, Debug, Default)]
pub struct Plane {
    pub normal: Vec3,
    pub d: f32,
}

/// idAAS2File::bspTree_t.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tree {
    pub floor_normal: Vec3,
    pub head_node: i32,
    pub first_area: i32,
    pub last_area: i32,
}

/// aas2Edge_t.
#[derive(Clone, Copy, Debug, Default)]
pub struct Edge {
    pub v: [i32; 2],
    pub flags: i32,
}

/// The parts of idAAS2Settings the AI uses (the rest is kept raw in `words`).
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// idAAS2Settings::type_t (1 = AAS_MONSTER).
    pub kind: u32,
    pub file_extension: String,
    pub bounds: [Vec3; 2],
    pub gravity_dir: Vec3,
    /// gravityValue, maxStepHeight, maxBarrierHeight, maxWaterJumpHeight, maxFallHeight, minFloorCos,
    /// minHighCeiling, groundSpeed, waterSpeed, ladderSpeed, ... in file order (LoadBinary skips
    /// maxLedgeGrabHeight): 28 big-endian 4-byte values.
    pub words: Vec<u32>,
}

impl Settings {
    fn f(&self, i: usize) -> f32 {
        f32::from_bits(self.words[i])
    }
    pub fn gravity(&self) -> f32 {
        self.f(0)
    }
    pub fn max_step_height(&self) -> f32 {
        self.f(1)
    }
    pub fn min_floor_cos(&self) -> f32 {
        self.f(5)
    }
    /// Units per second.
    pub fn ground_speed(&self) -> f32 {
        self.f(7)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Aas {
    pub crc: u32,
    pub timestamp: u32,
    pub settings: Settings,
    pub planes: Vec<Plane>,
    pub vertices: Vec<Vec3>,
    pub edges: Vec<Edge>,
    pub edge_index: Vec<i32>,
    pub reach: Vec<Reach>,
    pub areas: Vec<Area>,
    pub nodes: Vec<Node>,
    pub trees: Vec<Tree>,
    /// aas2AreaBounds_t: whole world units.
    pub area_bounds: Vec<[[i16; 3]; 2]>,
    pub num_clusters: usize,
    pub num_portals: usize,
}

struct Rd<'a> {
    d: &'a [u8],
    o: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(self.o + n <= self.d.len(), "AAS: truncated at {:#x} (+{n})", self.o);
        let s = &self.d[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec3(&mut self) -> Result<Vec3> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    fn i16x3(&mut self) -> Result<[i16; 3]> {
        Ok([self.i16()?, self.i16()?, self.i16()?])
    }
    /// A list: u32 count, then `count` records of `size` bytes parsed by `f` (which must consume them).
    fn list<T>(&mut self, size: usize, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let n = self.u32()? as usize;
        ensure!(n.saturating_mul(size) <= self.d.len() - self.o, "AAS: list of {n} x {size} overruns the file");
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let at = self.o;
            out.push(f(self)?);
            debug_assert_eq!(self.o - at, size);
        }
        Ok(out)
    }
    fn skip_list(&mut self, size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        self.take(n * size)?;
        Ok(n)
    }
}

impl Aas {
    pub fn parse(data: &[u8]) -> Result<Aas> {
        let mut r = Rd { d: data, o: 0 };
        let magic = r.u32()?;
        if magic != MAGIC {
            bail!("not an AAS2 file (magic {magic:#x})");
        }
        let (major, minor) = (r.u8()?, r.u8()?);
        ensure!(major == MAJOR && minor == MINOR, "AAS version {major}.{minor}, expected {MAJOR}.{MINOR}");
        let crc = r.u32()?;
        let timestamp = r.u32()?;
        for _ in 0..4 {
            r.i32()?; // firstFakeVertex / Edge / EdgeIndex / Area
        }
        // idAAS2Settings (0x14160fff0)
        let kind = r.u32()?;
        let mut names = Vec::new();
        for _ in 0..3 {
            let n = r.u32()? as usize;
            let s = r.take(n)?;
            let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            names.push(String::from_utf8_lossy(&s[..end]).into_owned());
        }
        let bounds = [r.vec3()?, r.vec3()?];
        r.i32()?; // primitiveModeBrush
        r.i32()?; // primitiveModeModel
        let gravity_dir = r.vec3()?;
        let mut words = Vec::with_capacity(28);
        for _ in 0..28 {
            words.push(r.u32()?);
        }
        let settings = Settings { kind, file_extension: names.swap_remove(0), bounds, gravity_dir, words };

        let planes = r.list(16, |r| Ok(Plane { normal: r.vec3()?, d: r.f32()? }))?;
        let vertices = r.list(12, |r| r.vec3())?;
        let edges = r.list(12, |r| Ok(Edge { v: [r.i32()?, r.i32()?], flags: r.i32()? }))?;
        let edge_index = r.list(4, |r| r.i32())?;
        let reach = r.list(0x28, |r| {
            Ok(Reach {
                travel_flags: r.u32()?,
                travel_time: r.u16()?,
                from_area: r.u16()?,
                to_area: r.u16()?,
                reservation_id: r.u16()?,
                start: r.i16x3()?,
                end: r.i16x3()?,
                reservation_expire_time: r.i32()?,
                area_tt_ofs_and_number: r.u32()?,
                next: r.i32()?,
                rev_next: r.i32()?,
            })
        })?;
        let areas = r.list(0x2c, |r| {
            Ok(Area {
                travel_flags: r.u32()?,
                flags: r.u16()?,
                num_edges: r.i16()?,
                first_edge: r.i32()?,
                cluster: r.i16()?,
                cluster_area_num: r.u16()?,
                obstacle_pvs_offset: r.u32()?,
                reach: r.i32()?,
                rev_reach: r.i32()?,
                first_choke_point: r.u16()?,
                num_choke_points: r.i16()?,
                first_cover: r.u16()?,
                num_cover: r.u16()?,
                first_traversal: r.i16()?,
                num_traversals: r.u16()?,
                first_hint_node: r.u16()?,
                num_hint_nodes: r.u16()?,
            })
        })?;
        let nodes = r.list(16, |r| Ok(Node { plane: r.u32()?, flags: r.u32()?, children: [r.i32()?, r.i32()?] }))?;
        let num_portals = r.skip_list(12)?;
        r.skip_list(4)?; // portalIndex
        let num_clusters = r.skip_list(16)?;
        r.skip_list(1)?; // obstaclePVS
        r.skip_list(0x84)?; // reachabilityNames
        r.skip_list(0x80)?; // animNames
        r.skip_list(0x80)?; // dependencyNames
        r.skip_list(0x80)?; // interactionEntityNames
        r.skip_list(0x38)?; // cover
        r.skip_list(4)?; // areaCoverIndex
        r.skip_list(4)?; // touchingCoverIndex
        r.skip_list(0x3c)?; // traversalPoints
        r.skip_list(0x18)?; // hintNodes
        let trees = r.list(0x18, |r| {
            Ok(Tree { floor_normal: r.vec3()?, head_node: r.i32()?, first_area: r.i32()?, last_area: r.i32()? })
        })?;
        let area_bounds = r.list(12, |r| Ok([r.i16x3()?, r.i16x3()?]))?;
        ensure!(r.o == data.len(), "AAS: {} trailing bytes", data.len() - r.o);
        Ok(Aas {
            crc,
            timestamp,
            settings,
            planes,
            vertices,
            edges,
            edge_index,
            reach,
            areas,
            nodes,
            trees,
            area_bounds,
            num_clusters,
            num_portals,
        })
    }

    /// Reachabilities leaving `area` (its `reach` chain).
    pub fn reach_from(&self, area: usize) -> impl Iterator<Item = (usize, &Reach)> + '_ {
        let mut i = self.areas.get(area).map_or(-1, |a| a.reach);
        std::iter::from_fn(move || {
            if i < 0 || i as usize >= self.reach.len() {
                return None;
            }
            let at = i as usize;
            i = self.reach[at].next;
            Some((at, &self.reach[at]))
        })
    }

    /// The floor polygon of an area (vertices in edge order).
    pub fn area_polygon(&self, area: usize) -> Vec<Vec3> {
        let a = &self.areas[area];
        (0..a.num_edges.unsigned_abs() as i32)
            .filter_map(|k| {
                let e = *self.edge_index.get((a.first_edge + k) as usize)?;
                let edge = self.edges.get(e.unsigned_abs() as usize)?;
                let v = if e < 0 { edge.v[1] } else { edge.v[0] };
                self.vertices.get(v as usize).copied()
            })
            .collect()
    }

    pub fn area_center(&self, area: usize) -> Vec3 {
        let b = self.area_bounds[area];
        [(b[0][0] as f32 + b[1][0] as f32) * 0.5, (b[0][1] as f32 + b[1][1] as f32) * 0.5, b[0][2] as f32]
    }
}
