//! `.bmodel` static models (idStaticModel): map world geometry (`maps/<map>/_combo/_world.bmodel`,
//! `megamodel_*.bmodel`) and cooked props (`cooked/model/<path>.bmodel` for `models/<path>.lwo`).
//!
//! Decoded from idStaticModel::LoadResourceInternal (0x1417aec70), the per-surface geometry reader
//! 0x1417bd5e0 and the writer idStaticModel::WriteStaticBModel (0x14189ffa0); notes in
//! gamedata/re/MAPS.md. Strings are a little-endian u32 length plus bytes, everything else big-endian.
//!
//! File: u32 magic (`BMODEL_MAGIC` 0x1b4c4d42, or `BMODEL_MAGIC_PREVIOUS` 0x1a4c4d42 which lacks the
//! per-surface material check), u32 timestamp, u32 surface count, the surfaces (each followed by the
//! magic again), then u32 count + `(material, u32, u32, u32)` rows.
//! Surface: material, [u32 material check], u32, u32, u32 VT material count + names, geometry.
//! Geometry: u32 vertex count, u32 index count, u32 vertex format, 10 f32 dequantisation constants,
//! vertices, u16 indices, bounds (6 f32), u32.

use anyhow::{Context, Result, bail, ensure};

pub const BMODEL_MAGIC: u32 = 0x1b4c4d42;
pub const BMODEL_MAGIC_PREVIOUS: u32 = 0x1a4c4d42;

/// Vertex format with every attribute as a full idDrawVert (48 bytes). Bits (size function
/// 0x1417b15d0): 0 xyz f32x3, 1 st f32x2, 2 normal u8x4, 3 tangent u8x4, 4 color u8x4,
/// 15 vmtrTC f32x2, 16 vmtrSB u16x4; other bits select packed layouts (not used by shipped files).
pub const FORMAT_DRAWVERT: u32 = 0x1801f;
pub const DRAW_VERT_SIZE: usize = 48;

/// Size in bytes of one vertex of `format` (0x1417b15d0).
pub fn vertex_size(format: u32) -> usize {
    let bit = |b: u32| format & (1 << b) != 0;
    let mut n: i32 = 0;
    n += if bit(12) { 12 } else { 0 };
    n -= if bit(6) { 4 } else { 0 };
    n -= if bit(5) { 4 } else { 0 };
    n -= if bit(17) { 4 } else { 0 };
    n += if bit(0) { 12 } else { 0 };
    n += if bit(16) { 8 } else { 0 };
    n += if bit(15) { 8 } else { 0 };
    n += if bit(11) { 4 } else { 0 };
    n += if bit(4) { 4 } else { 0 };
    n += if bit(3) { 4 } else { 0 };
    n += if bit(1) { 8 } else { 0 };
    n += if bit(2) { 4 } else { 0 };
    n.max(0) as usize
}

/// idDrawVert as lit static geometry uses it (vertex.inc COMMON_VERTEX_OUTPUT, USE_LIGHTMAP).
#[derive(Debug, Clone, Copy, Default)]
pub struct StaticVert {
    pub xyz: [f32; 3],
    /// Material texcoords for props; unique-lightmap texcoords for `_world` surfaces whose material
    /// is the map's `mega` decl.
    pub st: [f32; 2],
    /// `b / 255 * 2 - 1`.
    pub normal: [u8; 4],
    /// xyz like the normal; `[3]` is the bitangent sign (`floor(w * 255.1 / 128) * 2 - 1`).
    pub tangent: [u8; 4],
    /// RGBA; static geometry scales rgb by `a * 16` for emissive/overbright (vertex.inc).
    pub color: [u8; 4],
    /// Texcoords inside the per-vertex VT material (`frac` wraps into its rect).
    pub vmtr_tc: [f32; 2],
    /// As stored: `[0]` indexes the surface's `vmtrs` list (0xffff = none; the loader replaces the
    /// four values with that material's VT rect in pages); the rest are zero on disk.
    pub vmtr_sb: [u16; 4],
}

impl StaticVert {
    pub fn normal_f32(&self) -> [f32; 3] {
        unpack(self.normal)
    }
    pub fn tangent_f32(&self) -> [f32; 4] {
        let t = unpack(self.tangent);
        [t[0], t[1], t[2], (self.tangent[3] as f32 * 255.1 / 255.0 / 128.0).floor() * 2.0 - 1.0]
    }
    /// Index into the surface's `vmtrs`, if any.
    pub fn vmtr(&self) -> Option<usize> {
        (self.vmtr_sb[0] != 0xffff).then_some(self.vmtr_sb[0] as usize)
    }
}

fn unpack(b: [u8; 4]) -> [f32; 3] {
    [b[0] as f32 / 255.0 * 2.0 - 1.0, b[1] as f32 / 255.0 * 2.0 - 1.0, b[2] as f32 / 255.0 * 2.0 - 1.0]
}

#[derive(Debug, Clone)]
pub struct Surface {
    /// Material decl name (`textures/...`, or the map's unique `maps/<map>/mega` / `megatrans`).
    pub material: String,
    /// Checksum of the material when the model was built (BModel "generated with old material").
    pub material_check: u32,
    /// Stored with the surface entry (+0x08; -1 on props, 0 on world surfaces).
    pub unknown_b: u32,
    /// Stored with the surface entry (+0x24; 0 in the shipped files seen).
    pub unknown_c: u32,
    /// VT materials referenced per vertex (`StaticVert::vmtr`).
    pub vmtrs: Vec<String>,
    pub format: u32,
    /// xyz scale, xyz bias, st scale, st bias (identity for `FORMAT_DRAWVERT`).
    pub dequant: [f32; 10],
    pub verts: Vec<StaticVert>,
    pub indices: Vec<u16>,
    pub bounds: [[f32; 3]; 2],
    /// Last geometry word (+0x28 of the triangle struct; not interpreted).
    pub unknown_tail: u32,
}

#[derive(Debug, Clone)]
pub struct StaticModel {
    pub magic: u32,
    pub timestamp: u32,
    pub surfaces: Vec<Surface>,
    /// Trailing `(material, a, b, c)` rows (props: surface index, 0, last vertex index).
    pub material_table: Vec<(String, u32, u32, u32)>,
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
    fn s(&mut self) -> Result<String> {
        let n = u32::from_le_bytes(self.take(4)?.try_into()?) as usize;
        ensure!(n < 4096, "string length {n} at {:#x}", self.o - 4);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// Walks a `.bmodel` one surface at a time, so callers can convert and drop each surface (the map
/// world has ~2000 surfaces and ~5M vertices).
pub struct SurfaceReader<'a> {
    r: R<'a>,
    pub magic: u32,
    pub timestamp: u32,
    pub surface_count: usize,
    next: usize,
}

impl<'a> SurfaceReader<'a> {
    pub fn new(b: &'a [u8]) -> Result<Self> {
        let mut r = R { b, o: 0 };
        let magic = r.be32()?;
        ensure!(magic == BMODEL_MAGIC || magic == BMODEL_MAGIC_PREVIOUS, "bad bmodel magic {magic:#x}");
        let timestamp = r.be32()?;
        let surface_count = r.be32()? as usize;
        ensure!(surface_count < 1 << 20, "implausible surface count {surface_count}");
        Ok(Self { r, magic, timestamp, surface_count, next: 0 })
    }

    /// The next surface, or None after the last one.
    pub fn next_surface(&mut self) -> Result<Option<Surface>> {
        if self.next >= self.surface_count {
            return Ok(None);
        }
        let i = self.next;
        self.next += 1;
        let r = &mut self.r;
        let material = r.s()?;
        let material_check = if self.magic != BMODEL_MAGIC_PREVIOUS { r.be32()? } else { 0 };
        let unknown_b = r.be32()?;
        let unknown_c = r.be32()?;
        let n = r.be32()? as usize;
        ensure!(n < 1 << 16, "surface {i}: {n} VT materials");
        let vmtrs = (0..n).map(|_| r.s()).collect::<Result<Vec<_>>>()?;
        // geometry (0x1417bd5e0)
        let nv = r.be32()? as usize;
        let ni = r.be32()? as usize;
        let format = r.be32()?;
        let mut dequant = [0f32; 10];
        for d in &mut dequant {
            *d = r.bef()?;
        }
        if format != FORMAT_DRAWVERT {
            bail!("surface {i} ({material}): vertex format {format:#x} ({} bytes) is not decoded", vertex_size(format));
        }
        let vb = r.take(nv * DRAW_VERT_SIZE)?;
        let mut verts = Vec::with_capacity(nv);
        for v in vb.chunks_exact(DRAW_VERT_SIZE) {
            let f = |o: usize| f32::from_be_bytes(v[o..o + 4].try_into().unwrap());
            let h = |o: usize| u16::from_be_bytes([v[o], v[o + 1]]);
            verts.push(StaticVert {
                xyz: [f(0), f(4), f(8)],
                st: [f(12), f(16)],
                normal: v[20..24].try_into().unwrap(),
                tangent: v[24..28].try_into().unwrap(),
                color: v[28..32].try_into().unwrap(),
                vmtr_tc: [f(32), f(36)],
                vmtr_sb: [h(40), h(42), h(44), h(46)],
            });
        }
        let ib = r.take(ni * 2)?;
        let indices: Vec<u16> = ib.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        ensure!(indices.iter().all(|&x| (x as usize) < nv), "surface {i}: index out of range");
        let bounds = [[r.bef()?, r.bef()?, r.bef()?], [r.bef()?, r.bef()?, r.bef()?]];
        let unknown_tail = r.be32()?;
        let m = r.be32()?;
        ensure!(m == BMODEL_MAGIC || m == BMODEL_MAGIC_PREVIOUS, "surface {i}: missing trailing magic ({m:#x})");
        Ok(Some(Surface { material, material_check, unknown_b, unknown_c, vmtrs, format, dequant, verts, indices, bounds, unknown_tail }))
    }

    /// The trailing material table; call after the last surface.
    pub fn material_table(&mut self) -> Result<Vec<(String, u32, u32, u32)>> {
        ensure!(self.next >= self.surface_count, "surfaces not fully read");
        let r = &mut self.r;
        let n = r.be32()? as usize;
        ensure!(n < 1 << 20, "implausible material table size {n}");
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push((r.s()?, r.be32()?, r.be32()?, r.be32()?));
        }
        Ok(out)
    }

    pub fn remaining(&self) -> usize {
        self.r.b.len() - self.r.o
    }
}

impl StaticModel {
    pub fn parse(b: &[u8]) -> Result<Self> {
        let mut rd = SurfaceReader::new(b)?;
        let mut surfaces = Vec::with_capacity(rd.surface_count);
        while let Some(s) = rd.next_surface()? {
            surfaces.push(s);
        }
        let material_table = rd.material_table()?;
        ensure!(rd.remaining() == 0, "{} trailing bytes", rd.remaining());
        Ok(Self { magic: rd.magic, timestamp: rd.timestamp, surfaces, material_table })
    }
}

/// Resource name of the static model an entity's `renderModelInfo.model` names: `.lwo`/`.ase`/
/// `.obj` editor sources are cooked to `cooked/model/<path>.bmodel`; names already pointing at a
/// `.bmodel` are used as they are. Returns None for skinned (`.md6`) and brush models.
pub fn resource_for_model(model: &str) -> Option<String> {
    let lower = model.to_ascii_lowercase();
    if lower.ends_with(".bmodel") {
        return Some(model.to_string());
    }
    for ext in [".lwo", ".ase", ".obj", ".ma", ".fbx"] {
        if lower.ends_with(ext) {
            return Some(format!("cooked/model/{}.bmodel", &model[..model.len() - ext.len()]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawvert_size() {
        assert_eq!(vertex_size(FORMAT_DRAWVERT), DRAW_VERT_SIZE);
    }

    #[test]
    fn model_resources() {
        assert_eq!(resource_for_model("models/a/b.lwo").as_deref(), Some("cooked/model/models/a/b.bmodel"));
        assert_eq!(resource_for_model("maps/x/_combo/m.bmodel").as_deref(), Some("maps/x/_combo/m.bmodel"));
        assert_eq!(resource_for_model("models/a/b.md6"), None);
    }
}
