//! Material textures from the virtual texture: `_vmtr*.vmtr` placement tables plus the sixteen
//! `_vmtr_sqN.mega2` quadrants (file `1 + qy*4 + qx` covers level-0 pages `[512*qx, 512*qx+512)` x
//! `[512*qy, ...)`; pages within a level are stored row-major).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Result, ensure};

use crate::vt::{HdpOptions, Mega2, PAGE, PAGE_BORDER, PAGE_PAYLOAD, Page, decode_page};

// channel-indexed loops mirror the ISPC kernel source
#[allow(clippy::needless_range_loop)]
pub mod bc3;
pub mod unique;

/// A material's area in the virtual texture, in level-0 texels (multiples of the page payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Parses a `.vmtr` table: a version line, a count line, then
/// `x y width height flags timeStamp mtrCheck "material"` rows.
pub fn parse_vmtr(text: &str) -> Vec<(String, VtRect)> {
    let mut out = Vec::new();
    for line in text.lines().skip(2) {
        let line = line.trim();
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        let Some(q) = line.find('"') else { continue };
        let name = line[q + 1..].trim_end_matches('"').to_string();
        let nums: Vec<i64> = line[..q].split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if nums.len() >= 4 {
            out.push((name, VtRect { x: nums[0] as u32, y: nums[1] as u32, w: nums[2] as u32, h: nums[3] as u32 }));
        }
    }
    out
}

/// One material at a mip level: the three page images (RGBA), the LZ plane and the cover mask.
pub struct MaterialImages {
    pub width: u32,
    pub height: u32,
    pub layers: [Vec<u8>; 3],
    pub lz: Vec<u8>,
    pub cover: Vec<u8>,
    pub missing_pages: usize,
}

pub struct VirtualTexture {
    files: Vec<Mega2>,
    /// `sqN` of each file (page cache directory names).
    tags: Vec<String>,
    rects: HashMap<String, VtRect>,
    /// Install build (Steam build id from the app manifest, else the exe size and time).
    build: String,
    /// Root of the transcoded-page cache, if enabled (`set_cache_dir`).
    cache: Option<std::path::PathBuf>,
    /// Threads used to transcode pages (0 = all cores).
    threads: usize,
}

/// Identifies the install the page files belong to: the Steam build id of DOOM (app 379720) when
/// the app manifest is next to the install, else the size and modification time of DOOMx64.exe.
fn install_build(doom: &Path) -> String {
    let manifest = doom.parent().and_then(Path::parent).map(|s| s.join("appmanifest_379720.acf"));
    if let Some(text) = manifest.and_then(|m| std::fs::read_to_string(m).ok()) {
        for line in text.lines() {
            let mut it = line.split('"').filter(|t| !t.trim().is_empty());
            if let (Some("buildid"), Some(id)) = (it.next(), it.next()) {
                return format!("b{id}");
            }
        }
    }
    let meta = std::fs::metadata(doom.join("DOOMx64.exe")).ok();
    let time = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    format!("x{}-{time}", meta.map_or(0, |m| m.len()))
}

impl VirtualTexture {
    pub fn open(doom: &Path) -> Result<Self> {
        let dir = doom.join("virtualtextures");
        let mut files = Vec::new();
        let mut tags = Vec::new();
        for n in 1..=16 {
            let p = dir.join(format!("_vmtr_sq{n}.mega2"));
            if p.exists() {
                files.push(Mega2::open(&p)?);
                tags.push(format!("sq{n}"));
            }
        }
        ensure!(!files.is_empty(), "no _vmtr_sq*.mega2 files in {}", dir.display());
        let mut tables: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "vmtr"))
            .collect();
        // The main table wins; DLC tables only add materials it lacks.
        tables.sort_by_key(|p| (p.file_name().is_some_and(|n| n != "_vmtr.vmtr"), p.clone()));
        let mut rects = HashMap::new();
        for t in tables {
            for (name, r) in parse_vmtr(&std::fs::read_to_string(&t)?) {
                rects.entry(name).or_insert(r);
            }
        }
        Ok(VirtualTexture { files, tags, rects, build: install_build(doom), cache: None, threads: 0 })
    }

    /// Caps the threads `levels` / `atlas` use to transcode pages (0 = all cores).
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads;
    }

    /// Enables the on-disk cache of transcoded pages under `dir` (created on demand): one LZ4
    /// file per page at `dir/vt/<build>-<bc3 variant>/<sqN>/<slot>.bc`. Only data derived from this
    /// install is written there.
    pub fn set_cache_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.cache = dir.map(|d| d.join("vt").join(format!("{}-{}", self.build, match bc3_variant() {
            bc3::Variant::Avx2 => "avx2",
            bc3::Variant::Sse => "sse",
        })));
    }

    /// (file index, slot) of the page at (level, page x, page y).
    fn page_slot(&self, level: usize, px: u32, py: u32) -> Option<(usize, usize)> {
        for (i, f) in self.files.iter().enumerate() {
            let l = f.levels.get(level)?;
            if px >= l.page_x && px < l.page_x + l.width && py >= l.page_y && py < l.page_y + l.height {
                let idx = l.first as usize + ((py - l.page_y) * l.width + (px - l.page_x)) as usize;
                return f.slot(idx).map(|s| (i, s));
            }
        }
        None
    }

    /// The page re-encoded like the engine, from the cache when present.
    pub fn page_blocks(&self, level: usize, px: u32, py: u32) -> Option<PageBlocks> {
        let (fi, slot) = self.page_slot(level, px, py)?;
        let path = self.cache.as_ref().map(|c| c.join(&self.tags[fi]).join(format!("{slot}.bc")));
        if let Some(b) = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|z| PageBlocks::unpack(&z)) {
            return Some(b);
        }
        let blocks = transcode_page(self.files[fi].page(slot)?).ok()?;
        if let Some(p) = path {
            // write-then-rename so concurrent readers never see a partial file
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            if p.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok()) && std::fs::write(&tmp, blocks.pack()).is_ok() && std::fs::rename(&tmp, &p).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Some(blocks)
    }

    pub fn rect(&self, material: &str) -> Option<VtRect> {
        self.rects.get(material).copied()
    }

    /// Compressed page at (level, page x, page y), from whichever quadrant holds it.
    pub fn page_data(&self, level: usize, px: u32, py: u32) -> Option<&[u8]> {
        for f in &self.files {
            let l = f.levels.get(level)?;
            if px >= l.page_x && px < l.page_x + l.width && py >= l.page_y && py < l.page_y + l.height {
                let idx = l.first as usize + ((py - l.page_y) * l.width + (px - l.page_x)) as usize;
                return f.slot(idx).and_then(|s| f.page(s));
            }
        }
        None
    }

    /// Decodes and stitches the pages covering `rect` at mip `level`, dropping page borders.
    pub fn assemble(&self, rect: VtRect, level: usize) -> Result<MaterialImages> {
        let p = PAGE_PAYLOAD as u32;
        let (x0, y0) = (rect.x >> level, rect.y >> level);
        let (w, h) = ((rect.w >> level).max(1), (rect.h >> level).max(1));
        let (px0, py0, px1, py1) = (x0 / p, y0 / p, (x0 + w).div_ceil(p), (y0 + h).div_ceil(p));
        let mut img = MaterialImages {
            width: w,
            height: h,
            layers: std::array::from_fn(|_| vec![0u8; (w * h * 4) as usize]),
            lz: vec![0u8; (w * h) as usize],
            cover: vec![255u8; (w * h) as usize],
            missing_pages: 0,
        };
        let coords: Vec<(u32, u32)> = (py0..py1).flat_map(|py| (px0..px1).map(move |px| (px, py))).collect();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
        let chunk = coords.len().div_ceil(threads).max(1);
        let decoded: Vec<((u32, u32), Option<Page>)> = std::thread::scope(|s| {
            let handles: Vec<_> = coords
                .chunks(chunk)
                .map(|c| {
                    s.spawn(move || {
                        c.iter()
                            .map(|&(px, py)| ((px, py), self.page_data(level, px, py).and_then(|d| decode_page(d, HdpOptions::default()).ok())))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        for ((px, py), page) in decoded {
            let Some(page) = page else {
                img.missing_pages += 1;
                continue;
            };
            for ty in 0..p {
                let gy = py * p + ty;
                if gy < y0 || gy >= y0 + h {
                    continue;
                }
                for tx in 0..p {
                    let gx = px * p + tx;
                    if gx < x0 || gx >= x0 + w {
                        continue;
                    }
                    let src = (ty as usize + PAGE_BORDER) * PAGE + tx as usize + PAGE_BORDER;
                    let dst = ((gy - y0) * w + (gx - x0)) as usize;
                    for (l, layer) in page.images.iter().enumerate() {
                        if let Some(d) = layer {
                            img.layers[l][dst * 4..dst * 4 + 4].copy_from_slice(&d[src * 4..src * 4 + 4]);
                        }
                    }
                    if let Some(lz) = &page.lz_plane {
                        img.lz[dst] = lz[src];
                    }
                    if let Some(c) = &page.cover {
                        img.cover[dst] = c[src];
                    }
                }
            }
        }
        Ok(img)
    }
}

/// One physical page as the engine uploads it (transcode job 0x141a656f0, compressed physical
/// images): layers 0..2 re-encoded to BC3 by the ISPC kernel (sampled as sRGB DXT5,
/// `_physicalvmtrpages0`, GL format 0xb + sRGB), layer 3 the page's BC7 colour mask as stored
/// (`_physicalvmtrpages3`, BPTC UNORM). Blocks are row-major, 32 x 32 per 128 x 128 page.
pub struct PageBlocks {
    pub layers: [Vec<u8>; 3],
    pub color_mask: Vec<u8>,
}

impl PageBlocks {
    fn pack(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(4 * PAGE_BLOCK_BYTES);
        for l in &self.layers {
            raw.extend_from_slice(l);
        }
        raw.extend_from_slice(&self.color_mask);
        lz4_flex::block::compress_prepend_size(&raw)
    }

    fn unpack(z: &[u8]) -> Option<PageBlocks> {
        let raw = lz4_flex::block::decompress_size_prepended(z).ok()?;
        (raw.len() == 4 * PAGE_BLOCK_BYTES).then(|| {
            let part = |i: usize| raw[i * PAGE_BLOCK_BYTES..(i + 1) * PAGE_BLOCK_BYTES].to_vec();
            PageBlocks { layers: [part(0), part(1), part(2)], color_mask: part(3) }
        })
    }
}

/// Bytes of one layer of a page in BC3 or BC7 (32 x 32 blocks of 16 bytes).
pub const PAGE_BLOCK_BYTES: usize = (PAGE / 4) * (PAGE / 4) * 16;

/// The BC3 kernel build this CPU would run (see `bc3::Variant`).
pub fn bc3_variant() -> bc3::Variant {
    static V: std::sync::OnceLock<bc3::Variant> = std::sync::OnceLock::new();
    *V.get_or_init(bc3::Variant::detect)
}

/// Decodes a page and re-encodes it exactly as the engine does before upload. Absent images are
/// encoded from zeros like in the engine; a page without colour mask gets zero blocks.
pub fn transcode_page(data: &[u8]) -> Result<PageBlocks> {
    let page = decode_page(data, HdpOptions::default())?;
    ensure!(page.raw.is_none(), "raw (pre-compressed) pages are not supported");
    let zeros = vec![0u8; PAGE * PAGE * 4];
    let v = bc3_variant();
    let layers = std::array::from_fn(|i| bc3::compress(page.images[i].as_deref().unwrap_or(&zeros), PAGE, PAGE, PAGE * 4, v));
    let color_mask = page.lz_plane.unwrap_or_else(|| vec![0u8; PAGE_BLOCK_BYTES]);
    ensure!(color_mask.len() == PAGE_BLOCK_BYTES, "colour mask of {} bytes", color_mask.len());
    Ok(PageBlocks { layers, color_mask })
}

/// A block-aligned texel region of one VT level in the engine's GPU formats.
pub struct VtLevel {
    pub level: usize,
    /// Region in texels of this level (multiples of 4; may extend past the virtual texture).
    pub x0: i64,
    pub y0: i64,
    pub width: u32,
    pub height: u32,
    /// BC3 blocks of layers 0..2, row-major (`width / 4` blocks per row).
    pub layers: [Vec<u8>; 3],
    /// BC7 blocks of the colour mask.
    pub color_mask: Vec<u8>,
    /// Pages in the region that are not in the page files (left as zero blocks).
    pub missing_pages: usize,
}

impl VtLevel {
    /// Region of `rect` (level-0 texels) at `level`, grown to whole 4x4 blocks plus `margin`
    /// texels (rounded up to blocks) on every side, like the 4-texel border of physical pages.
    pub fn region(rect: VtRect, level: usize, margin: u32) -> (i64, i64, u32, u32) {
        let s = 1i64 << level;
        let m = margin.div_ceil(4) as i64 * 4;
        let lo = |v: u32| (v as i64).div_euclid(s).div_euclid(4) * 4 - m;
        let hi = |v: u32| ((v as i64 + s - 1).div_euclid(s) + 3).div_euclid(4) * 4 + m;
        let (x0, y0) = (lo(rect.x), lo(rect.y));
        (x0, y0, (hi(rect.x + rect.w) - x0) as u32, (hi(rect.y + rect.h) - y0) as u32)
    }
}

impl VirtualTexture {
    /// Number of mip levels in the page files.
    pub fn level_count(&self) -> usize {
        self.files.iter().map(|f| f.levels.len()).min().unwrap_or(0)
    }

    /// The material's region at levels `first..=last`, decoded and re-encoded page by page on all
    /// cores, with `margin` texels of surrounding virtual texture on each side.
    pub fn levels(&self, rect: VtRect, first: usize, last: usize, margin: u32) -> Result<Vec<VtLevel>> {
        let p = PAGE_PAYLOAD as i64;
        struct Plan {
            level: usize,
            x0: i64,
            y0: i64,
            w: u32,
            h: u32,
            px0: i64,
            py0: i64,
            pw: usize,
            start: usize,
        }
        // every page of every level first, so one parallel pass covers them all
        let mut plans = Vec::new();
        let mut coords: Vec<(usize, i64, i64)> = Vec::new();
        for level in first..=last.min(self.level_count().saturating_sub(1)) {
            let (x0, y0, w, h) = VtLevel::region(rect, level, margin);
            let (px0, py0) = (x0.div_euclid(p), y0.div_euclid(p));
            let (px1, py1) = ((x0 + w as i64 - 1).div_euclid(p), (y0 + h as i64 - 1).div_euclid(p));
            plans.push(Plan { level, x0, y0, w, h, px0, py0, pw: (px1 - px0 + 1) as usize, start: coords.len() });
            coords.extend((py0..=py1).flat_map(|py| (px0..=px1).map(move |px| (level, px, py))));
        }
        let pages = self.transcode_pages(&coords);
        let mut out = Vec::new();
        for pl in plans {
            let (bw, bh) = ((pl.w / 4) as usize, (pl.h / 4) as usize);
            // pages missing inside the material itself (the margin may run into empty VT space)
            let s = 1i64 << pl.level;
            let inner = |px: i64, py: i64| {
                let (lx0, ly0) = ((rect.x as i64).div_euclid(s), (rect.y as i64).div_euclid(s));
                let (lx1, ly1) = ((rect.x + rect.w) as i64 + s - 1, (rect.y + rect.h) as i64 + s - 1);
                px * p < lx1.div_euclid(s) && (px + 1) * p > lx0 && py * p < ly1.div_euclid(s) && (py + 1) * p > ly0
            };
            let missing = (0..pages.len() - pl.start)
                .take_while(|&i| coords.get(pl.start + i).is_some_and(|c| c.0 == pl.level))
                .filter(|&i| pages[pl.start + i].is_none() && inner(coords[pl.start + i].1, coords[pl.start + i].2))
                .count();
            let mut lv = VtLevel {
                level: pl.level,
                x0: pl.x0,
                y0: pl.y0,
                width: pl.w,
                height: pl.h,
                layers: std::array::from_fn(|_| vec![0u8; bw * bh * 16]),
                color_mask: vec![0u8; bw * bh * 16],
                missing_pages: missing,
            };
            for by in 0..bh {
                let ty = pl.y0 + by as i64 * 4;
                let (py, oy) = (ty.div_euclid(p), ty.rem_euclid(p));
                for bx in 0..bw {
                    let tx = pl.x0 + bx as i64 * 4;
                    let (px, ox) = (tx.div_euclid(p), tx.rem_euclid(p));
                    let Some(page) = &pages[pl.start + (py - pl.py0) as usize * pl.pw + (px - pl.px0) as usize] else { continue };
                    // payload texel (ox, oy) is physical texel (4 + ox, 4 + oy): block (1 + ox/4, 1 + oy/4)
                    let src = ((1 + oy as usize / 4) * (PAGE / 4) + 1 + ox as usize / 4) * 16;
                    let dst = (by * bw + bx) * 16;
                    for l in 0..3 {
                        lv.layers[l][dst..dst + 16].copy_from_slice(&page.layers[l][src..src + 16]);
                    }
                    lv.color_mask[dst..dst + 16].copy_from_slice(&page.color_mask[src..src + 16]);
                }
            }
            out.push(lv);
        }
        Ok(out)
    }

    /// Transcodes the pages at `coords` (level, page x, page y) in parallel; None where a page is
    /// absent or outside the virtual texture.
    fn transcode_pages(&self, coords: &[(usize, i64, i64)]) -> Vec<Option<PageBlocks>> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let next = AtomicUsize::new(0);
        let all = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let threads = if self.threads == 0 { all } else { self.threads.min(all) }.min(coords.len().max(1));
        let mut results: Vec<Option<PageBlocks>> = (0..coords.len()).map(|_| None).collect();
        let done: Vec<Vec<(usize, PageBlocks)>> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let mut mine = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&(level, px, py)) = coords.get(i) else { break };
                            if px < 0 || py < 0 {
                                continue;
                            }
                            if let Some(b) = self.page_blocks(level, px as u32, py as u32) {
                                mine.push((i, b));
                            }
                        }
                        mine
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap_or_default()).collect()
        });
        for (i, b) in done.into_iter().flatten() {
            results[i] = Some(b);
        }
        results
    }
}

/// Where one VT level of a material sits in a `VtAtlas`.
#[derive(Debug, Clone, Copy)]
pub struct AtlasLevel {
    pub level: usize,
    /// Region origin in texels of this level (see `VtLevel`).
    pub x0: i64,
    pub y0: i64,
    /// Region position and size in the atlas, in texels (multiples of 4).
    pub ax: u32,
    pub ay: u32,
    pub width: u32,
    pub height: u32,
}

/// A material's VT levels packed side by side in single-mip textures, the way the engine's
/// physical page cache holds pages: `pages` is a 3-layer BC3 array (layer-major), `color_mask`
/// BC7, both `width` x `height` texels. Level `first` sits at (0, 0), the coarser ones in a column
/// to its right.
pub struct VtAtlas {
    pub width: u32,
    pub height: u32,
    pub pages: Vec<u8>,
    pub color_mask: Vec<u8>,
    pub levels: Vec<AtlasLevel>,
    pub missing_pages: usize,
}

impl VtAtlas {
    pub fn pack(levels: Vec<VtLevel>) -> VtAtlas {
        let mut placed = Vec::new();
        let (mut width, mut height, mut col_y, mut col_w) = (0u32, 0u32, 0u32, 0u32);
        for (i, l) in levels.iter().enumerate() {
            let (ax, ay) = if i == 0 { (0, 0) } else { (levels[0].width, col_y) };
            if i > 0 {
                col_y += l.height;
                col_w = col_w.max(l.width);
            }
            width = width.max(if i == 0 { l.width } else { levels[0].width + col_w });
            height = height.max(ay + l.height);
            placed.push(AtlasLevel { level: l.level, x0: l.x0, y0: l.y0, ax, ay, width: l.width, height: l.height });
        }
        let bw = (width / 4) as usize;
        let layer = bw * (height / 4) as usize * 16;
        let mut pages = vec![0u8; layer * 3];
        let mut color_mask = vec![0u8; layer];
        for (l, a) in levels.iter().zip(&placed) {
            let lbw = (l.width / 4) as usize;
            for by in 0..(l.height / 4) as usize {
                let src = by * lbw * 16;
                let dst = ((a.ay / 4) as usize + by) * bw * 16 + (a.ax / 4) as usize * 16;
                for k in 0..3 {
                    pages[k * layer + dst..k * layer + dst + lbw * 16].copy_from_slice(&l.layers[k][src..src + lbw * 16]);
                }
                color_mask[dst..dst + lbw * 16].copy_from_slice(&l.color_mask[src..src + lbw * 16]);
            }
        }
        VtAtlas { width, height, pages, color_mask, missing_pages: levels.iter().map(|l| l.missing_pages).sum(), levels: placed }
    }
}

impl VirtualTexture {
    /// `levels` packed into one atlas.
    pub fn atlas(&self, rect: VtRect, first: usize, last: usize, margin: u32) -> Result<VtAtlas> {
        Ok(VtAtlas::pack(self.levels(rect, first, last, margin)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test -p idres --release -- --ignored vtex_levels --nocapture` (needs the install).
    #[test]
    #[ignore]
    fn vtex_levels() {
        let doom = std::env::var("DOOM_DIR").unwrap_or_else(|_| r"C:/Program Files (x86)/Steam/steamapps/common\DOOM".into());
        let mut vt = VirtualTexture::open(Path::new(&doom)).unwrap();
        vt.set_cache_dir(Some(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../gamedata/cache")).to_path_buf()));
        for (name, first) in [("models/weapons/shotgun/shotgun_base", 1), ("models/weapons/shotgun/shotgun_base", 1), ("models/characters/doommarine_playerhands/doommarine_playerhands", 0)] {
            let rect = vt.rect(name).unwrap();
            let t = std::time::Instant::now();
            let lv = vt.levels(rect, first, 11, 8).unwrap();
            let bytes: usize = lv.iter().map(|l| l.layers.iter().map(Vec::len).sum::<usize>() + l.color_mask.len()).sum();
            eprintln!("{name} {rect:?}: {} levels from {first}, {:.1} MB, {:.2}s", lv.len(), bytes as f64 / 1e6, t.elapsed().as_secs_f32());
            for l in &lv {
                eprintln!("  level {} region ({}, {}) {}x{} missing {}", l.level, l.x0, l.y0, l.width, l.height, l.missing_pages);
            }
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore]
    fn vtex_page_cost() {
        let doom = std::env::var("DOOM_DIR").unwrap_or_else(|_| r"C:/Program Files (x86)/Steam/steamapps/common\DOOM".into());
        let vt = VirtualTexture::open(Path::new(&doom)).unwrap();
        let data = vt.page_data(1, 560, 512).or_else(|| vt.page_data(1, 561, 513)).expect("page").to_vec();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            decode_page(&data, HdpOptions::default()).unwrap();
        }
        let dec = t.elapsed().as_secs_f64() / 20.0;
        let page = decode_page(&data, HdpOptions::default()).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            for i in 0..3 {
                bc3::compress(page.images[i].as_deref().unwrap(), PAGE, PAGE, PAGE * 4, bc3_variant());
            }
        }
        let enc = t.elapsed().as_secs_f64() / 20.0;
        eprintln!("decode {:.2} ms, bc3 x3 {:.2} ms per page ({:?})", dec * 1e3, enc * 1e3, bc3_variant());
    }
}
