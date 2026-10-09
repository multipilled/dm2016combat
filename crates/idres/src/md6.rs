//! `.bmd6model` skinned meshes (idTech 6 MD6).
//!
//! Strings are a little-endian u32 length plus bytes; numeric fields are big-endian.
//! File: u32 magic (`2b 02 'M' 'M'`), 8 bytes (timestamps), skeleton path, placeholder bounds,
//! u8, u32, u16 joint-remap count + remap bytes, model bounds, u32 blend-shape count + names,
//! 9 floats, u32 mesh count.
//! Mesh: name, material, u32le texcoord sets, u8 flags, u32 vertex count, u32 triangle count,
//! bounds, vertices (48-byte idDrawVert), u16 indices, then joint offset, joint count, checksum,
//! skin-remap count (+ remaps) and a flag byte.

use anyhow::{Context, Result, bail, ensure};

pub const DRAW_VERT_SIZE: usize = 48;

#[derive(Debug, Clone, Copy, Default)]
pub struct DrawVert {
    pub xyz: [f32; 3],
    pub st: [f32; 2],
    /// Unit normal from bytes (b/255*2-1); `[3]` is a skin weight byte.
    pub normal: [u8; 4],
    /// `[3]` is the texture polarity sign (0 or 128).
    pub tangent: [u8; 4],
    /// Joint palette indices for skinned meshes.
    pub color: [u8; 4],
    /// Remaining 16 bytes (virtual-texture coordinates / scale-bias in idDrawVert); kept raw.
    pub extra: [u8; 16],
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub name: String,
    pub material: String,
    pub texcoord_sets: u32,
    pub flags: u8,
    pub bounds: [[f32; 3]; 2],
    pub verts: Vec<DrawVert>,
    pub indices: Vec<u16>,
    /// joint offset, joint count, checksum, skin-remap count
    pub trailer: [u32; 4],
    pub trailer_flag: u8,
}

#[derive(Debug, Clone)]
pub struct Md6Model {
    pub skeleton: String,
    pub joint_remap: Vec<u8>,
    pub bounds: [[f32; 3]; 2],
    pub blend_shapes: Vec<String>,
    pub blend_params: [f32; 9],
    pub meshes: Vec<Mesh>,
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
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn be16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn be32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn le32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn bef(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn s(&mut self) -> Result<String> {
        let n = self.le32()? as usize;
        ensure!(n < 4096, "string length {n} at {:#x}", self.o - 4);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn bounds(&mut self) -> Result<[[f32; 3]; 2]> {
        Ok([[self.bef()?, self.bef()?, self.bef()?], [self.bef()?, self.bef()?, self.bef()?]])
    }
}

impl Md6Model {
    pub fn parse(b: &[u8]) -> Result<Self> {
        let mut r = R { b, o: 0 };
        if r.take(4)? != [0x2b, 0x02, b'M', b'M'] {
            bail!("bad bmd6model magic");
        }
        r.take(8)?;
        let skeleton = r.s()?;
        r.bounds()?;
        r.u8()?;
        r.be32()?;
        let n = r.be16()? as usize;
        let joint_remap = r.take(n)?.to_vec();
        let bounds = r.bounds()?;
        let nshapes = r.be32()? as usize;
        let mut blend_shapes = Vec::with_capacity(nshapes);
        for _ in 0..nshapes {
            blend_shapes.push(r.s()?);
        }
        let mut blend_params = [0f32; 9];
        for p in &mut blend_params {
            *p = r.bef()?;
        }
        let num_meshes = r.be32()? as usize;
        let mut meshes = Vec::with_capacity(num_meshes);
        for _ in 0..num_meshes {
            let name = r.s()?;
            let material = r.s()?;
            let texcoord_sets = r.le32()?;
            let flags = r.u8()?;
            let nv = r.be32()? as usize;
            let nt = r.be32()? as usize;
            let mb = r.bounds()?;
            let vb = r.take(nv * DRAW_VERT_SIZE)?;
            let mut verts = Vec::with_capacity(nv);
            for v in vb.chunks_exact(DRAW_VERT_SIZE) {
                let f = |o: usize| f32::from_be_bytes(v[o..o + 4].try_into().unwrap());
                verts.push(DrawVert {
                    xyz: [f(0), f(4), f(8)],
                    st: [f(12), f(16)],
                    normal: v[20..24].try_into().unwrap(),
                    tangent: v[24..28].try_into().unwrap(),
                    color: v[28..32].try_into().unwrap(),
                    extra: v[32..48].try_into().unwrap(),
                });
            }
            let ib = r.take(nt * 3 * 2)?;
            let indices: Vec<u16> = ib.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            ensure!(indices.iter().all(|&i| (i as usize) < nv), "mesh {name}: index out of range");
            // jointOffset, numJoints, checksum, skin-remap count
            let trailer = [r.be32()?, r.be32()?, r.be32()?, r.be32()?];
            for _ in 0..trailer[3] {
                r.s()?;
                r.take(8)?;
            }
            let trailer_flag = r.u8()?;
            meshes.push(Mesh { name, material, texcoord_sets, flags, bounds: mb, verts, indices, trailer, trailer_flag });
        }
        Ok(Self { skeleton, joint_remap, bounds, blend_shapes, blend_params, meshes })
    }
}

pub fn unpack_normal(b: [u8; 4]) -> [f32; 3] {
    [b[0] as f32 / 255.0 * 2.0 - 1.0, b[1] as f32 / 255.0 * 2.0 - 1.0, b[2] as f32 / 255.0 * 2.0 - 1.0]
}

/// `.bmd6skl` skeleton: bind pose in parent space.
#[derive(Debug, Clone)]
pub struct Md6Skel {
    pub names: Vec<String>,
    pub parents: Vec<i16>,
    /// Quaternion x, y, z, w.
    pub rotations: Vec<[f32; 4]>,
    pub scales: Vec<[f32; 3]>,
    pub translations: Vec<[f32; 3]>,
}

impl Md6Skel {
    /// Layout (big-endian, offsets relative to 0x0c): u16 names offset, u16 joint count, u16, u16,
    /// u16 rotations offset, u16 inverse-bind offset, u16 parents offset, ...; the transform arrays are
    /// padded to a multiple of 8 joints: rotations (xyzw f32), then scales (vec3), then translations (vec3)
    /// (checked on all 400 skeletons: parents offset - rotations offset >= padded * 40).
    pub fn parse(b: &[u8]) -> Result<Self> {
        ensure!(b.len() > 0x20 && b[0..4] == [0x0e, 0x02, b'S', b'M'], "bad bmd6skl magic");
        let base = 0x0c;
        let u16be = |o: usize| u16::from_be_bytes([b[o], b[o + 1]]) as usize;
        let f = |o: usize| f32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let names_ofs = u16be(base);
        let n = u16be(base + 2);
        let rot_ofs = u16be(base + 8);
        let parents_ofs = u16be(base + 12);
        let padded = n.div_ceil(8) * 8;
        let rot = base + rot_ofs;
        let scl = rot + padded * 16;
        let trn = scl + padded * 12;
        ensure!(trn + padded * 12 <= b.len(), "skeleton arrays out of range");
        let mut s = Md6Skel { names: Vec::new(), parents: Vec::new(), rotations: Vec::new(), scales: Vec::new(), translations: Vec::new() };
        for j in 0..n {
            s.rotations.push([f(rot + j * 16), f(rot + j * 16 + 4), f(rot + j * 16 + 8), f(rot + j * 16 + 12)]);
            s.scales.push([f(scl + j * 12), f(scl + j * 12 + 4), f(scl + j * 12 + 8)]);
            s.translations.push([f(trn + j * 12), f(trn + j * 12 + 4), f(trn + j * 12 + 8)]);
            let p = base + parents_ofs + j * 2;
            s.parents.push(i16::from_be_bytes([b[p], b[p + 1]]));
        }
        // Names: little-endian length-prefixed strings; scan from the names offset.
        let mut o = base + names_ofs;
        while s.names.len() < n && o + 4 <= b.len() {
            let len = u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
            if len == 0 || len > 256 || o + 4 + len > b.len() {
                o += 1;
                continue;
            }
            let bytes = &b[o + 4..o + 4 + len];
            if bytes.iter().all(|c| c.is_ascii_graphic()) {
                s.names.push(String::from_utf8_lossy(bytes).into_owned());
                o += 4 + len;
            } else {
                o += 1;
            }
        }
        if s.names.len() != n {
            bail!("found {} of {n} joint names", s.names.len());
        }
        Ok(s)
    }
}
