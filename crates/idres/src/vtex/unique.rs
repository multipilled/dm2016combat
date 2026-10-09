//! Per-map unique virtual texture `virtualtextures/<map>.pages` (the material's `landPageFile`):
//! the baked HDR lightmap sampled by FETCH_UNIQUE_HDR_LIGHTMAP (unique.inc) through
//! `_physical%sPages4` (unique page type 4: GL format 0x16 = BPTC unsigned float, i.e. BC6H UF16).
//!
//! File (LE): 0x00 magic 0x77339906, 0x04 version 3, 0x0c pages wide at level 0, 0x14 level count,
//! 0x18 page slots (full pyramid), 0x1c offset unit (5), 0x20 u64 file size, 0x28 u32 tag,
//! 0x30 root record offset / unit, 0x40 root record length / unit. Records form a quadtree from the
//! single coarsest page down: u32 tag, u32 4, u32 x4 child offsets / unit (0 = absent; children in
//! the order (0,0) (1,0) (0,1) (1,1)), u16 x4 child lengths / unit, u16 x, u16 y, u32 level, then a
//! virtual-texture page (16-byte big-endian header as in `.mega2`, see idres::vt) padded to the
//! unit. Pages are raw (`flags2` 0x80) LZ4 payloads of 16 KiB: 32 x 32 BC6H blocks of a 128 x 128
//! physical page with a 4-texel border, as uploaded by the engine's page decoder (0x141a64c70).

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use memmap2::Mmap;

use crate::vt::{HdpOptions, PAGE, PAGE_PAYLOAD, decode_page};

/// A map's unique virtual texture.
pub struct UniqueVt {
    map: Mmap,
    /// Pages per side at level 0.
    pub pages_wide: u32,
    /// Number of levels (level 0 finest; `levels - 1` is the single root page).
    pub levels: usize,
    index: HashMap<(usize, u32, u32), (usize, usize)>,
}

/// One level of the unique virtual texture as a single BC6H texture of page payloads (borders
/// dropped), `width` x `height` texels, blocks row-major.
pub struct UniqueLevel {
    pub level: usize,
    pub width: u32,
    pub height: u32,
    pub blocks: Vec<u8>,
    /// Pages of the level that are not in the file (zero blocks).
    pub missing_pages: usize,
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}

impl UniqueVt {
    /// Opens `virtualtextures/<map>.pages`, e.g. `maps/game/sp/intro/intro` (a material's
    /// `landPageFile`, with or without the leading `maps/`).
    pub fn open(doom: &Path, map: &str) -> Result<Self> {
        let rel = if map.starts_with("maps/") { map.to_string() } else { format!("maps/{map}") };
        let path = doom.join("virtualtextures").join(format!("{rel}.pages"));
        let f = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: read-only mapping of a game file that is not modified while we run.
        let map = unsafe { Mmap::map(&f)? };
        ensure!(map.len() >= 0x50 && u32le(&map, 0) == 0x7733_9906, "bad .pages magic");
        ensure!(u32le(&map, 4) == 3, "unsupported .pages version {}", u32le(&map, 4));
        let (pages_wide, levels, unit) = (u32le(&map, 0x0c), u32le(&map, 0x14) as usize, u32le(&map, 0x1c) as usize);
        ensure!(unit > 0 && levels > 0 && levels <= 16, "bad .pages header");
        let tag = u32le(&map, 0x28);
        let mut index = HashMap::new();
        let mut stack = vec![(u32le(&map, 0x30) as usize * unit, u32le(&map, 0x40) as usize * unit, levels - 1, 0u32, 0u32)];
        while let Some((o, n, level, x, y)) = stack.pop() {
            ensure!(o + 56 <= map.len() && o + n <= map.len() && n >= 56, "page record out of range");
            ensure!(u32le(&map, o) == tag, "bad page record at {o:#x}");
            ensure!((u16le(&map, o + 32) as u32, u16le(&map, o + 34) as u32, u32le(&map, o + 36) as usize) == (x, y, level), "page record at {o:#x} is not at ({level}, {x}, {y})");
            index.insert((level, x, y), (o + 40, n - 40));
            if level == 0 {
                continue;
            }
            for i in 0..4 {
                let co = u32le(&map, o + 8 + 4 * i) as usize * unit;
                if co != 0 {
                    stack.push((co, u16le(&map, o + 24 + 2 * i) as usize * unit, level - 1, 2 * x + (i as u32 & 1), 2 * y + (i as u32 >> 1)));
                }
            }
        }
        Ok(UniqueVt { map, pages_wide, levels, index })
    }

    /// Number of pages stored in the file.
    pub fn page_count(&self) -> usize {
        self.index.len()
    }

    /// Pages per side at `level`.
    pub fn level_pages(&self, level: usize) -> u32 {
        (self.pages_wide >> level).max(1)
    }

    /// The stored page (16-byte header + data) at (level, page x, page y).
    pub fn page(&self, level: usize, x: u32, y: u32) -> Option<&[u8]> {
        self.index.get(&(level, x, y)).map(|&(o, n)| &self.map[o..o + n])
    }

    /// The page's 16 KiB of BC6H blocks (32 x 32, row-major, including the 4-texel border).
    pub fn page_blocks(&self, level: usize, x: u32, y: u32) -> Option<Vec<u8>> {
        decode_page(self.page(level, x, y)?, HdpOptions::default()).ok()?.raw
    }

    /// All of `level` as one BC6H texture (payloads only; level 1 of a 512-page map is 30720 texels
    /// square, so use coarse levels for whole-map textures).
    pub fn level_texture(&self, level: usize) -> UniqueLevel {
        let n = self.level_pages(level) as usize;
        let pb = PAGE_PAYLOAD / 4; // payload blocks per page side (30)
        let bw = n * pb;
        let mut blocks = vec![0u8; bw * bw * 16];
        let mut missing = 0;
        for py in 0..n {
            for px in 0..n {
                let Some(b) = self.page_blocks(level, px as u32, py as u32) else {
                    missing += 1;
                    continue;
                };
                for by in 0..pb {
                    let src = ((1 + by) * (PAGE / 4) + 1) * 16;
                    let dst = ((py * pb + by) * bw + px * pb) * 16;
                    blocks[dst..dst + pb * 16].copy_from_slice(&b[src..src + pb * 16]);
                }
            }
        }
        let side = (n * PAGE_PAYLOAD) as u32;
        UniqueLevel { level, width: side, height: side, blocks, missing_pages: missing }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test -p idres --release -- --ignored unique_pages --nocapture` (needs the install).
    #[test]
    #[ignore]
    fn unique_pages() {
        let doom = std::env::var("DOOM_DIR").unwrap_or_else(|_| r"C:/Program Files (x86)/Steam/steamapps/common\DOOM".into());
        let u = UniqueVt::open(Path::new(&doom), "maps/game/sp/intro/intro").unwrap();
        eprintln!("{} pages, {} wide, {} levels", u.page_count(), u.pages_wide, u.levels);
        // every stored page decodes to 16 KiB of BC6H blocks with valid mode bits
        let (mut pages, mut reserved) = (0, 0);
        for (&(l, x, y), _) in u.index.iter().take(2000) {
            let b = u.page_blocks(l, x, y).expect("page decodes");
            assert_eq!(b.len(), 16384);
            for blk in b.chunks(16) {
                let m = blk[0] & 3;
                if m >= 2 && matches!(blk[0] & 0x1f, 0x13 | 0x17 | 0x1b | 0x1f) {
                    reserved += 1;
                }
            }
            pages += 1;
        }
        eprintln!("{pages} pages checked, {reserved} blocks with reserved BC6H modes");
        let t = u.level_texture(4);
        eprintln!("level 4: {}x{}, {} missing", t.width, t.height, t.missing_pages);
    }
}
