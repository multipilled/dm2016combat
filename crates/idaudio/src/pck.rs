//! Wwise file packages (`.pck`, magic `AKPK`): the streamed `.wem` files and a few banks.
//!
//! Layout (little-endian): `AKPK`, u32 header size (from offset 8), u32 version (1), u32 language-map
//! size, u32 bank-LUT size, u32 stream-LUT size, u32 externals-LUT size; then the language map
//! (u32 count, {u32 string offset, u32 id}, UTF-16LE names), the bank and stream LUTs (u32 count,
//! {u32 id, u32 block size, u32 byte size, u32 start block, u32 language id}) and the externals LUT
//! (always empty here). A file's data starts at `start block * block size`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use memmap2::Mmap;

use crate::read::Cursor;

#[derive(Debug, Clone, Copy)]
pub struct PckEntry {
    pub id: u32,
    pub block_size: u32,
    pub size: u32,
    pub start_block: u32,
    pub language: u32,
}

impl PckEntry {
    pub fn offset(&self) -> u64 {
        u64::from(self.start_block) * u64::from(self.block_size)
    }
}

pub struct Package {
    pub path: PathBuf,
    pub map: Arc<Mmap>,
    pub languages: Vec<(u32, String)>,
    pub banks: Vec<PckEntry>,
    pub streams: Vec<PckEntry>,
}

impl Package {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: the game's data files are not modified while we read them.
        let map = unsafe { Mmap::map(&file) }.with_context(|| format!("mapping {}", path.display()))?;
        let (languages, banks, streams) = parse_header(&map).with_context(|| format!("parsing {}", path.display()))?;
        for e in banks.iter().chain(&streams) {
            ensure!(e.offset() + u64::from(e.size) <= map.len() as u64, "{}: file {} runs past the end", path.display(), e.id);
        }
        Ok(Self { path: path.to_owned(), map: Arc::new(map), languages, banks, streams })
    }

    pub fn bytes(&self, e: &PckEntry) -> &[u8] {
        let start = e.offset() as usize;
        &self.map[start..start + e.size as usize]
    }

    pub fn language_name(&self, id: u32) -> Option<&str> {
        self.languages.iter().find(|(l, _)| *l == id).map(|(_, n)| n.as_str())
    }
}

type Header = (Vec<(u32, String)>, Vec<PckEntry>, Vec<PckEntry>);

fn parse_header(b: &[u8]) -> Result<Header> {
    let mut r = Cursor::new(b);
    ensure!(r.bytes(4)? == b"AKPK", "bad magic");
    let header_size = r.u32()? as usize;
    let version = r.u32()?;
    ensure!(version == 1, "unsupported AKPK version {version}");
    let lang_size = r.u32()? as usize;
    let banks_size = r.u32()? as usize;
    let streams_size = r.u32()? as usize;
    let externals_size = r.u32()? as usize;
    ensure!(0x1c + lang_size + banks_size + streams_size + externals_size == header_size + 8, "LUT sizes do not add up");

    let lang = r.sub(lang_size)?;
    let mut languages = Vec::new();
    let mut lr = Cursor::new(lang);
    for _ in 0..lr.u32()? {
        let (offset, id) = (lr.u32()? as usize, lr.u32()?);
        let units: Vec<u16> = lang
            .get(offset..)
            .context("language name offset out of range")?
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        languages.push((id, String::from_utf16_lossy(&units)));
    }
    let banks = parse_lut(r.sub(banks_size)?)?;
    let streams = parse_lut(r.sub(streams_size)?)?;
    let mut er = Cursor::new(r.sub(externals_size)?);
    if er.u32()? != 0 {
        bail!("external-source LUTs are not supported");
    }
    Ok((languages, banks, streams))
}

fn parse_lut(b: &[u8]) -> Result<Vec<PckEntry>> {
    let mut r = Cursor::new(b);
    let n = r.u32()? as usize;
    ensure!(b.len() == 4 + n * 20, "LUT of {n} entries is {} bytes", b.len());
    (0..n)
        .map(|_| Ok(PckEntry { id: r.u32()?, block_size: r.u32()?, size: r.u32()?, start_block: r.u32()?, language: r.u32()? }))
        .collect()
}
