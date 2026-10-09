//! Virtual textures: `virtualtextures/_vmtr_sqN.mega2` page files and their pages, read from the
//! user's own install (loader 0x14185ac50, page decode 0x141a64c70 in DOOMx64.exe).
//!
//! `.mega2` (little-endian): 0x170-byte header — magic 0xa63fbb21, version 2, ..., virtual size
//! (245760²), pages (2048²), quadrant size, quadrant pages, at 0x38 u64 slot-table offset, 0x40 u64
//! page-index offset, 0x48 u32 slot count, 0x4c u32 page count, then 12 level descriptors of six
//! u32 (first page x, first page y, pages wide, pages high, first page index, page count).
//! Page index entries are u32 slots (0xffffffff = absent); a slot is (u64 offset, u64 size).
//!
//! Page (header big-endian, 16 bytes): quality q0 q1 q2, flags (4 = LZ4 instead of LZW, 8 = has a
//! cover mask), u16 sizes of HDP images 0..2, u16 LZ plane size, u16 size of image 3, flags2
//! (1/2/4/0x10 = image 0/1/2/3 absent, 0x20 = constant cover, 0x40 = no LZ plane, 0x80 = raw page,
//! 8 = raw page is all zero),
//! cover fill byte. Data: images 0..3, the LZ plane (128x128 bytes), then the 128x128-bit cover.
//! Images are HD Photo codestreams (original operators, sub-version 0) whose image header the
//! engine strips: a YUV444 colour plane header and a Y-only alpha plane header follow directly,
//! then a header-size VLW and the single spatial tile. The header is rebuilt for jxrlib.
//! `decode_page` reproduces the engine's page buffer byte for byte (see `Page`), checked against
//! the engine's own decoder run under emulation by `tools/hdp_verify.py`.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use memmap2::Mmap;

pub const PAGE: usize = 128;
pub const PAGE_BORDER: usize = 4;
pub const PAGE_PAYLOAD: usize = PAGE - 2 * PAGE_BORDER;

#[derive(Debug, Clone, Copy)]
pub struct Level {
    pub page_x: u32,
    pub page_y: u32,
    pub width: u32,
    pub height: u32,
    pub first: u32,
    pub count: u32,
}

pub struct Mega2 {
    map: Mmap,
    pub levels: Vec<Level>,
    index_ofs: usize,
    index_count: usize,
    slot_ofs: usize,
    slot_count: usize,
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Mega2 {
    pub fn open(path: &Path) -> Result<Self> {
        let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: read-only mapping of a game file that is not modified while we run.
        let map = unsafe { Mmap::map(&f)? };
        ensure!(map.len() >= 0x170, "mega2 too small");
        ensure!(le32(&map, 0) == 0xa63f_bb21, "bad mega2 magic");
        ensure!(le32(&map, 4) == 2, "unsupported mega2 version");
        let nlevels = le32(&map, 0x0c) as usize;
        let levels = (0..nlevels.min(12))
            .map(|i| {
                let o = 0x50 + i * 24;
                Level { page_x: le32(&map, o), page_y: le32(&map, o + 4), width: le32(&map, o + 8), height: le32(&map, o + 12), first: le32(&map, o + 16), count: le32(&map, o + 20) }
            })
            .collect();
        let m = Mega2 {
            levels,
            slot_ofs: le64(&map, 0x38) as usize,
            index_ofs: le64(&map, 0x40) as usize,
            slot_count: le32(&map, 0x48) as usize,
            index_count: le32(&map, 0x4c) as usize,
            map,
        };
        ensure!(m.index_ofs + m.index_count * 4 <= m.map.len() && m.slot_ofs + m.slot_count * 16 <= m.map.len(), "mega2 tables out of range");
        Ok(m)
    }

    /// Slot of the page with this index (`Level::first` + position within the level).
    pub fn slot(&self, page_index: usize) -> Option<usize> {
        if page_index >= self.index_count {
            return None;
        }
        let s = le32(&self.map, self.index_ofs + page_index * 4);
        (s != u32::MAX && (s as usize) < self.slot_count).then_some(s as usize)
    }

    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    pub fn page(&self, slot: usize) -> Option<&[u8]> {
        let o = self.slot_ofs + slot * 16;
        let (ofs, size) = (le64(&self.map, o) as usize, le64(&self.map, o + 8) as usize);
        self.map.get(ofs..ofs + size)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PageHeader {
    pub quality: [u8; 3],
    pub flags: u8,
    pub sizes: [u16; 4],
    pub lz_size: u16,
    pub flags2: u8,
    pub cover_fill: u8,
}

impl PageHeader {
    pub fn parse(b: &[u8]) -> Result<Self> {
        ensure!(b.len() >= 16, "page header truncated");
        let be = |o: usize| u16::from_be_bytes([b[o], b[o + 1]]);
        Ok(PageHeader { quality: [b[0], b[1], b[2]], flags: b[3], sizes: [be(4), be(6), be(8), be(12)], lz_size: be(10), flags2: b[14], cover_fill: b[15] })
    }

    fn present(&self, i: usize) -> bool {
        self.flags2 & [1, 2, 4, 0x10][i] == 0
    }
}

/// A decoded page, laid out as the engine's page decoder (0x141a64c70) leaves it in its 0x50000-byte
/// page buffer: images 0..2 at 0x00000/0x10000/0x20000 (128x128 RGBA8), the LZ plane at 0x30000
/// (128x128 bytes) and, for raw pages, 16 KiB at 0x40000.
///
/// Engine semantics mirrored here:
/// - an absent image (`flags2` 1/2/4) is zero-filled; images 0 and 1 are `None` then (all zero);
/// - image 2 is always produced for non-raw pages: its third byte (B) is overwritten with the cover
///   mask (0xff where the cover bit is set, 0 elsewhere), or with 0xff everywhere when the page has
///   no cover (`flags` 8 clear), also when image 2 itself is absent (zeros plus that byte);
/// - the LZ plane (`flags2` 0x40 clear) is LZ4 (`flags` 4) or LZW (idLZWCompressor, `flags` 4
///   clear) compressed; with `flags2` 0x40 the engine zero-fills it (`None` here);
/// - raw pages (`flags2` 0x80) skip all of the above (the engine leaves layers 0..3 untouched) and
///   decompress the bytes after the present images into 16 KiB (`raw`), or zero-fill it when
///   `flags2` 8 is set. No page in the shipped `.mega2` files uses LZW or raw pages.
#[derive(Debug, Clone)]
pub struct Page {
    pub header: PageHeader,
    pub images: [Option<Vec<u8>>; 3],
    pub lz_plane: Option<Vec<u8>>,
    /// Cover mask per texel (0 or 255) when the page has one (`flags` 8).
    pub cover: Option<Vec<u8>>,
    /// 16 KiB raw (pre-compressed) page payload (`flags2` 0x80).
    pub raw: Option<Vec<u8>>,
}

/// Decoder options for the stripped HD Photo image header.
#[derive(Debug, Clone, Copy)]
pub struct HdpOptions {
    pub overlap: u8,
}

impl Default for HdpOptions {
    fn default() -> Self {
        HdpOptions { overlap: 1 }
    }
}

/// Decodes one page image (colour plane + interleaved alpha plane) to 128x128 RGBA8.
pub fn decode_hdp(stream: &[u8], opt: HdpOptions) -> Result<Vec<u8>> {
    jxr_sys::decode_headerless(stream, PAGE as u32, PAGE as u32, opt.overlap as u32, true).context("HD Photo decode failed")
}

/// Size of the LZ plane and of a raw page payload.
const PLANE: usize = PAGE * PAGE;

/// `LZ4_decompress_fast` as linked into the engine (0x141afff60): decodes sequences until exactly
/// `out_len` bytes are produced; the compressed size is not used.
pub fn lz4_decompress_fast(src: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len);
    let mut i = 0usize;
    let byte = |i: usize| src.get(i).copied().context("LZ4 input truncated");
    let length = |i: &mut usize, mut n: usize| -> Result<usize> {
        if n == 15 {
            loop {
                let b = byte(*i)?;
                *i += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Ok(n)
    };
    loop {
        let token = byte(i)?;
        i += 1;
        let lit = length(&mut i, (token >> 4) as usize)?;
        ensure!(out.len() + lit <= out_len, "LZ4 literals overrun the output");
        out.extend_from_slice(src.get(i..i + lit).context("LZ4 input truncated")?);
        i += lit;
        if out.len() == out_len {
            return Ok(out);
        }
        let ofs = u16::from_le_bytes([byte(i)?, byte(i + 1)?]) as usize;
        i += 2;
        let len = length(&mut i, (token & 15) as usize)? + 4;
        ensure!(ofs != 0 && ofs <= out.len(), "LZ4 match offset out of range");
        ensure!(out.len() + len <= out_len, "LZ4 match overruns the output");
        let from = out.len() - ofs;
        for k in 0..len {
            out.push(out[from + k]);
        }
    }
}

/// The engine's LZW decoder (idLZWCompressor: Start 0x141915130, DecompressBlock 0x141914db0,
/// ReadBits 0x141915090, WriteChain 0x141915470, Read 0x141914fe0): LSB-first codes of 9..12 bits,
/// codes 0..255 literal, dictionary reset when the code width would reach 13 bits, blocks of up to
/// 0x7000 (+ one chain) bytes. Decodes `src` to `out_len` bytes; on short or bad input the engine
/// leaves the rest of its buffer as it was, here it stays zero.
pub fn lzw_decompress(src: &[u8], out_len: usize) -> Vec<u8> {
    const FIRST: i32 = 0x100;
    const START_BITS: u32 = 9;
    let mut dict_k = [0u8; 4096];
    let mut dict_w = [0xffffu16; 4096];
    for (i, k) in dict_k.iter_mut().enumerate().take(256) {
        *k = i as u8;
    }
    let (mut next, mut bits) = (FIRST, START_BITS);
    let (mut temp, mut temp_bits, mut read) = (0u64, 0u32, 0usize);
    let mut old: i32 = -1;
    let mut out = vec![0u8; out_len];
    let mut written = 0usize;
    let mut chain: Vec<u8> = Vec::with_capacity(4096);
    let mut block: Vec<u8> = Vec::with_capacity(0x7000 + 4097);
    while written < out_len {
        // DecompressBlock: returns false on an invalid code; Read() then stops after this block.
        block.clear();
        let mut ok = true;
        while block.len() < 0x7000 {
            let mut need = bits as i32 - temp_bits as i32;
            let mut eof = false;
            while need > 0 {
                let Some(&b) = src.get(read) else {
                    eof = true;
                    break;
                };
                temp |= (b as u64) << temp_bits;
                read += 1;
                temp_bits += 8;
                need -= 8;
            }
            if eof {
                break;
            }
            let code = (temp as u32 & ((1u32 << bits) - 1)) as i32;
            temp >>= bits;
            temp_bits -= bits;
            if old == -1 {
                if code > 0xff {
                    ok = false;
                    break;
                }
                block.push(code as u8);
                old = code;
                continue;
            }
            // WriteChain: emit the string of `c`, return its first byte.
            let mut write_chain = |mut c: i32, block: &mut Vec<u8>| -> u8 {
                chain.clear();
                loop {
                    chain.push(dict_k[c as usize]);
                    let w = dict_w[c as usize];
                    if w == 0xffff {
                        break;
                    }
                    c = w as i32;
                }
                block.extend(chain.iter().rev());
                chain[chain.len() - 1]
            };
            let first = if code < next {
                write_chain(code, &mut block)
            } else if code == next {
                let f = write_chain(old, &mut block);
                block.push(f);
                f
            } else {
                ok = false;
                break;
            };
            dict_k[next as usize] = first;
            dict_w[next as usize] = old as u16;
            next += 1;
            if next == 1 << bits {
                bits += 1;
                if bits > 12 {
                    next = FIRST;
                    bits = START_BITS;
                    old = -1;
                    continue;
                }
            }
            old = code;
        }
        if !ok || block.is_empty() {
            break;
        }
        let n = block.len().min(out_len - written);
        out[written..written + n].copy_from_slice(&block[..n]);
        written += n;
    }
    out
}

/// LZ plane / raw payload decompression (0x141a6cc90): LZ4 when `flags` bit 2 is set (the
/// compressed size is ignored), else LZW reading at most `lz_size` bytes.
fn decompress_plane(h: &PageHeader, src: &[u8]) -> Result<Vec<u8>> {
    if h.flags & 4 != 0 {
        lz4_decompress_fast(src, PLANE)
    } else {
        Ok(lzw_decompress(src.get(..h.lz_size as usize).context("LZW data out of range")?, PLANE))
    }
}

pub fn decode_page(data: &[u8], opt: HdpOptions) -> Result<Page> {
    let h = PageHeader::parse(data)?;
    let body = &data[16..];
    let size = |i: usize| if h.present(i) { h.sizes[i] as usize } else { 0 };
    let images_len: usize = (0..4).map(size).sum();
    if h.flags2 & 0x80 != 0 {
        let raw = if h.flags2 & 8 == 0 { decompress_plane(&h, body.get(images_len..).context("raw page out of range")?)? } else { vec![0u8; PLANE] };
        return Ok(Page { header: h, images: [None, None, None], lz_plane: None, cover: None, raw: Some(raw) });
    }
    let mut images: [Option<Vec<u8>>; 3] = [None, None, None];
    let mut ofs = 0usize;
    for (i, img) in images.iter_mut().enumerate() {
        if h.present(i) {
            let s = body.get(ofs..ofs + size(i)).context("page image out of range")?;
            *img = Some(decode_hdp(s, opt).with_context(|| format!("page image {i}"))?);
        }
        ofs += size(i);
    }
    let lz_plane = if h.flags2 & 0x40 == 0 { Some(decompress_plane(&h, body.get(images_len..).context("LZ plane out of range")?).context("LZ plane")?) } else { None };
    // Cover bits follow the LZ plane (offset 0x141a64bc0), LSB first, row-major.
    let cover = if h.flags & 8 != 0 {
        let at = images_len + if h.flags2 & 0x40 == 0 { h.lz_size as usize } else { 0 };
        let bits: Vec<u8> = if h.flags2 & 0x20 != 0 { vec![h.cover_fill; PLANE / 8] } else { body.get(at..at + PLANE / 8).context("cover out of range")?.to_vec() };
        Some((0..PLANE).map(|i| if bits[i >> 3] >> (i & 7) & 1 != 0 { 255 } else { 0 }).collect::<Vec<u8>>())
    } else {
        None
    };
    let img2 = images[2].get_or_insert_with(|| vec![0u8; PLANE * 4]);
    for i in 0..PLANE {
        img2[i * 4 + 2] = cover.as_ref().map_or(0xff, |c| c[i]);
    }
    Ok(Page { header: h, images, lz_plane, cover, raw: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// idLZWCompressor::Write/End: the encoder the engine's LZW decoder inverts.
    fn lzw_compress(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let (mut temp, mut temp_bits) = (0u64, 0u32);
        let mut put = |code: i32, bits: u32, out: &mut Vec<u8>| {
            temp |= (code as u64) << temp_bits;
            temp_bits += bits;
            while temp_bits >= 8 {
                out.push(temp as u8);
                temp >>= 8;
                temp_bits -= 8;
            }
        };
        let mut dict = std::collections::HashMap::new();
        let (mut next, mut bits, mut w) = (0x100i32, 9u32, -1i32);
        for &k in data {
            if w == -1 {
                w = k as i32;
                continue;
            }
            if let Some(&c) = dict.get(&(w, k)) {
                w = c;
                continue;
            }
            put(w, bits, &mut out);
            let mut reset = false;
            if next == 1 << bits {
                bits += 1;
                if bits > 12 {
                    dict.clear();
                    next = 0x100;
                    bits = 9;
                    reset = true;
                }
            }
            if !reset {
                dict.insert((w, k), next);
                next += 1;
            }
            w = k as i32;
        }
        if w != -1 {
            put(w, bits, &mut out);
        }
        if temp_bits > 0 {
            out.push(temp as u8);
        }
        out
    }

    fn test_plane(seed: u32) -> Vec<u8> {
        // Runs of slowly varying bytes plus noise: exercises long chains, KwKwK and resets.
        let mut s = seed;
        (0..PLANE)
            .map(|i| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                if s >> 28 == 0 { (s >> 8) as u8 } else { (i / 97 % 7) as u8 * 30 }
            })
            .collect()
    }

    #[test]
    fn lzw_roundtrip() {
        for seed in 0..4 {
            let plane = test_plane(seed);
            assert_eq!(lzw_decompress(&lzw_compress(&plane), PLANE), plane);
        }
        let noise: Vec<u8> = (0..PLANE as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert_eq!(lzw_decompress(&lzw_compress(&noise), PLANE), noise);
    }

    #[test]
    fn lz4_fast() {
        // literals "abc", match offset 3 length 7, then 3 literals "xyz" ending the block.
        let src = [0x33, b'a', b'b', b'c', 3, 0, 0x30, b'x', b'y', b'z'];
        assert_eq!(lz4_decompress_fast(&src, 13).unwrap(), b"abcabcabcaxyz");
    }

    fn page(flags: u8, flags2: u8, fill: u8, lz: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut p = vec![30, 30, 30, flags, 0, 0, 0, 0, 0, 0, (lz.len() >> 8) as u8, lz.len() as u8, 0, 0, flags2, fill];
        p.extend_from_slice(lz);
        p.extend_from_slice(tail);
        p
    }

    #[test]
    fn lzw_plane_and_cover() {
        let plane = test_plane(7);
        let cover: Vec<u8> = (0..PLANE / 8).map(|i| (i * 37) as u8).collect();
        // images 0..2 absent, LZW plane, explicit cover bits.
        let p = decode_page(&page(8, 7, 0, &lzw_compress(&plane), &cover), HdpOptions::default()).unwrap();
        assert_eq!(p.lz_plane.as_deref(), Some(&plane[..]));
        assert!(p.images[0].is_none() && p.images[1].is_none());
        let img2 = p.images[2].as_ref().unwrap();
        for i in 0..PLANE {
            let bit = cover[i >> 3] >> (i & 7) & 1 != 0;
            assert_eq!(img2[i * 4..i * 4 + 4], [0, 0, if bit { 255 } else { 0 }, 0]);
            assert_eq!(p.cover.as_ref().unwrap()[i], if bit { 255 } else { 0 });
        }
        // constant cover (flags2 0x20) and no cover at all: B = fill bits / 0xff.
        let p = decode_page(&page(8, 0x67, 0x0f, &[], &[]), HdpOptions::default()).unwrap();
        assert!(p.lz_plane.is_none());
        assert_eq!(p.images[2].as_ref().unwrap()[2 + 4 * 3], 255);
        assert_eq!(p.images[2].as_ref().unwrap()[2 + 4 * 4], 0);
        let p = decode_page(&page(4, 0x67, 0, &[], &[]), HdpOptions::default()).unwrap();
        assert!(p.cover.is_none() && p.images[2].as_ref().unwrap().chunks(4).all(|c| c == [0, 0, 255, 0]));
    }

    #[test]
    fn raw_pages() {
        let plane = test_plane(3);
        let p = decode_page(&page(0, 0x80 | 0x17, 0, &lzw_compress(&plane), &[]), HdpOptions::default()).unwrap();
        assert_eq!(p.raw.as_deref(), Some(&plane[..]));
        assert!(p.images.iter().all(Option::is_none) && p.lz_plane.is_none() && p.cover.is_none());
        let p = decode_page(&page(4, 0x88 | 0x17, 0, &[], &[]), HdpOptions::default()).unwrap();
        assert_eq!(p.raw.as_deref(), Some(&[0u8; PLANE][..]));
    }
}
