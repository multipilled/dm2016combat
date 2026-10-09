//! `.bimage` textures (idTech 6, "BIM" version 7).
//!
//! Layout: u32 timestamp, magic `07 'M' 'I' 'B'` (big-endian 'BIM\x07'), then big-endian
//! u32 texture type, width, height, depth, mip count, u32 (unknown), then a byte-packed tail
//! (format byte at 0x20) up to 0x2d. Each mip follows as little-endian
//! `{level, dest_z, width, height, data_size}` plus `data_size` bytes.

use anyhow::{Result, bail, ensure};

#[derive(Debug, Clone)]
pub struct Mip {
    pub level: u32,
    pub dest_z: u32,
    pub width: u32,
    pub height: u32,
    pub data: std::ops::Range<usize>,
}

#[derive(Debug, Clone)]
pub struct BImage {
    pub texture_type: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub num_levels: u32,
    /// Raw header bytes 0x1c..0x2a, kept until every field is identified.
    pub tail: [u8; 14],
    pub format: u8,
    pub mips: Vec<Mip>,
}

pub const HEADER_SIZE: usize = 0x2a;

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

impl BImage {
    pub fn parse(b: &[u8]) -> Result<Self> {
        ensure!(b.len() >= HEADER_SIZE, "bimage too short");
        if &b[4..8] != b"\x07MIB" {
            bail!("bad bimage magic {:02x?}", &b[4..8]);
        }
        let mut img = BImage {
            texture_type: be32(b, 0x08),
            width: be32(b, 0x0c),
            height: be32(b, 0x10),
            depth: be32(b, 0x14),
            num_levels: be32(b, 0x18),
            tail: b[0x1c..0x2a].try_into().unwrap(),
            format: b[0x20],
            mips: Vec::new(),
        };
        let mut o = HEADER_SIZE;
        while o + 20 <= b.len() {
            let size = be32(b, o + 16) as usize;
            let start = o + 20;
            ensure!(start + size <= b.len(), "mip runs past end ({} + {size} > {})", start, b.len());
            img.mips.push(Mip { level: be32(b, o), dest_z: be32(b, o + 4), width: be32(b, o + 8), height: be32(b, o + 12), data: start..start + size });
            o = start + size;
        }
        ensure!(o == b.len(), "{} trailing bytes", b.len() - o);
        Ok(img)
    }
}
