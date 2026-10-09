//! Signed-distance-field fonts: `fonts/<name>/64_df.dat` plus the L8 atlas
//! `generated/image/fonts/<name>/64_df.tga$borderclamp$alpha.bimage`.
//!
//! `.dat` layout (big-endian): `"idf"` + version byte 0x2b, i16 point size (64), i16 ascender, i16 descender,
//! i16 padding (distance-field border in atlas pixels), i16 glyph count, glyph records of 10 bytes
//! (u8 width, u8 height, i8 top, i8 left, u8 x advance, u8 unused, LE u16 s, LE u16 t), then one LE u32 code
//! point per glyph, sorted ascending.

use anyhow::{Context, Result, ensure};
use idres::Container;

use crate::texture::{self, Texture};

#[derive(Debug, Clone, Copy, Default)]
pub struct Glyph {
    pub width: u8,
    pub height: u8,
    pub top: i8,
    pub left: i8,
    pub advance: u8,
    pub s: u16,
    pub t: u16,
}

#[derive(Debug, Clone)]
pub struct SdfFont {
    pub name: String,
    pub point_size: i16,
    pub ascender: i16,
    pub descender: i16,
    pub padding: i16,
    pub glyphs: Vec<Glyph>,
    pub codes: Vec<u32>,
    /// Atlas, single channel distance field (stored in `alpha`); width/height come from mip 0.
    pub atlas: Texture,
}

impl SdfFont {
    pub fn parse_dat(name: &str, b: &[u8], atlas: Texture) -> Result<SdfFont> {
        ensure!(b.len() >= 14 && &b[..3] == b"idf", "not an idf font");
        ensure!(b[3] == 0x2b, "unsupported idf version {:#x}", b[3]);
        let be16 = |o: usize| i16::from_be_bytes([b[o], b[o + 1]]);
        let n = be16(12) as usize;
        let glyph_end = 14 + n * 10;
        ensure!(b.len() == glyph_end + n * 4, "idf size mismatch ({} glyphs, {} bytes)", n, b.len());
        let glyphs = (0..n)
            .map(|i| {
                let g = &b[14 + i * 10..14 + i * 10 + 10];
                Glyph {
                    width: g[0],
                    height: g[1],
                    top: g[2] as i8,
                    left: g[3] as i8,
                    advance: g[4],
                    s: u16::from_le_bytes([g[6], g[7]]),
                    t: u16::from_le_bytes([g[8], g[9]]),
                }
            })
            .collect();
        let codes = (0..n).map(|i| u32::from_le_bytes(b[glyph_end + i * 4..glyph_end + i * 4 + 4].try_into().unwrap())).collect();
        Ok(SdfFont { name: name.to_string(), point_size: be16(4), ascender: be16(6), descender: be16(8), padding: be16(10), glyphs, codes, atlas })
    }

    /// Loads a font by its SWF face name ("TT Supermolot Thin" -> fonts/tt_supermolot_thin).
    pub fn load(container: &Container, face: &str) -> Result<SdfFont> {
        let dir = face_dir(face);
        let dat = container.read_by_name(&format!("fonts/{dir}/64_df.dat")).with_context(|| format!("font '{face}'"))?;
        let img = container.read_by_name(&format!("generated/image/fonts/{dir}/64_df.tga$borderclamp$alpha.bimage"))?;
        let atlas = texture::decode_bimage(&img)?;
        Self::parse_dat(face, &dat, atlas)
    }

    pub fn glyph(&self, code: u32) -> Option<&Glyph> {
        self.codes.binary_search(&code).ok().map(|i| &self.glyphs[i])
    }
}

/// Folder name under `fonts/` for a SWF font face name.
pub fn face_dir(face: &str) -> String {
    face.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}
