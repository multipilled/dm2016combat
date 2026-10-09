//! idTech 6 resource containers: a `.index`/`.pindex` table plus `.resources`/`.patch` data files.
//!
//! Index layout (all integers big-endian unless noted):
//! - `05 'S' 'E' 'R'` magic, u32 table size (= file length - 0x20), 0x18 zero bytes, u32 entry count
//! - per entry: i32 id, then type / short name / full name as (LE u32 length + UTF-8),
//!   u64 offset, u32 size, u32 compressed size (differs from size => raw deflate), u32 flags, u8 patch
//!
//! Patch 0 is `<base>.resources`, 1 is `<base>.patch`, n is `<base>_00n.patch`.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail, ensure};
use memmap2::Mmap;

const MAGIC: [u8; 4] = [0x05, b'S', b'E', b'R'];

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i32,
    pub kind: String,
    pub short_name: String,
    pub full_name: String,
    pub offset: u64,
    pub size: u32,
    pub csize: u32,
    pub flags: u32,
    pub patch: u8,
}

impl Entry {
    pub fn is_compressed(&self) -> bool {
        self.size != self.csize
    }
}

pub struct Container {
    pub entries: Vec<Entry>,
    data_base: PathBuf,
    data: Vec<OnceLock<Option<Mmap>>>,
    by_full_name: HashMap<String, usize>,
}

impl Container {
    /// Opens `<dir>/<name>.pindex` (falling back to `.index`), e.g. `name = "gameresources"`.
    pub fn open(dir: &Path, name: &str) -> Result<Self> {
        let pindex = dir.join(format!("{name}.pindex"));
        let index = if pindex.is_file() { pindex } else { dir.join(format!("{name}.index")) };
        let bytes = std::fs::read(&index).with_context(|| format!("reading {}", index.display()))?;
        let entries = parse_index(&bytes).with_context(|| format!("parsing {}", index.display()))?;

        // Later entries are patches layered over earlier ones, so the last full name wins.
        let mut by_full_name = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            if !e.full_name.is_empty() {
                by_full_name.insert(e.full_name.clone(), i);
            }
        }

        Ok(Self {
            entries,
            data_base: dir.join(name),
            data: (0..256).map(|_| OnceLock::new()).collect(),
            by_full_name,
        })
    }

    pub fn get(&self, full_name: &str) -> Option<&Entry> {
        self.by_full_name.get(full_name).map(|&i| &self.entries[i])
    }

    /// Entries whose full name is the live (most recently patched) copy.
    pub fn live_entries(&self) -> impl Iterator<Item = &Entry> {
        self.by_full_name.values().map(|&i| &self.entries[i])
    }

    /// The stored (possibly compressed) bytes of an entry.
    pub fn read_raw(&self, entry: &Entry) -> Result<&[u8]> {
        let map = self.data_file(entry.patch)?;
        let start = entry.offset as usize;
        let end = start + entry.csize as usize;
        ensure!(end <= map.len(), "{} runs past end of patch {} data", entry.full_name, entry.patch);
        Ok(&map[start..end])
    }

    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        if entry.size == 0 {
            return Ok(Vec::new());
        }
        let raw = self.read_raw(entry)?;
        if !entry.is_compressed() {
            return Ok(raw.to_vec());
        }
        // idTech writes raw deflate without a final block, so decode until the declared size is reached
        // instead of waiting for end-of-stream.
        let mut out = Vec::with_capacity(entry.size as usize);
        let mut inflater = flate2::Decompress::new(false);
        inflater
            .decompress_vec(raw, &mut out, flate2::FlushDecompress::Sync)
            .with_context(|| format!("inflating {}", entry.full_name))?;
        ensure!(out.len() == entry.size as usize, "{}: inflated {} bytes, expected {}", entry.full_name, out.len(), entry.size);
        Ok(out)
    }

    pub fn read_by_name(&self, full_name: &str) -> Result<Vec<u8>> {
        let entry = self.get(full_name).with_context(|| format!("no resource named {full_name}"))?;
        self.read(entry)
    }

    fn data_file(&self, patch: u8) -> Result<&Mmap> {
        let slot = self.data[patch as usize].get_or_init(|| {
            let path = match patch {
                0 => self.data_base.with_extension("resources"),
                1 => self.data_base.with_extension("patch"),
                n => {
                    let stem = self.data_base.file_name()?.to_string_lossy();
                    self.data_base.with_file_name(format!("{stem}_{n:03}.patch"))
                }
            };
            let file = File::open(path).ok()?;
            // SAFETY: the game's data files are not modified while we read them.
            let map = unsafe { Mmap::map(&file) }.ok()?;
            (map.get(..4) == Some(&MAGIC)).then_some(map)
        });
        slot.as_ref().with_context(|| format!("patch {patch} data file missing or invalid"))
    }
}

fn parse_index(bytes: &[u8]) -> Result<Vec<Entry>> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(4)? != MAGIC {
        bail!("bad magic");
    }
    let table_size = r.be_u32()? as usize;
    ensure!(table_size + 0x20 == bytes.len(), "table size {table_size:#x} does not match file length");
    r.take(0x18)?;
    let count = r.be_u32()? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(Entry {
            id: r.be_u32()? as i32,
            kind: r.id_str()?,
            short_name: r.id_str()?,
            full_name: r.id_str()?,
            offset: r.be_u64()?,
            size: r.be_u32()?,
            csize: r.be_u32()?,
            flags: r.be_u32()?,
            patch: r.take(1)?[0],
        });
    }
    ensure!(r.pos == bytes.len(), "{} trailing bytes after {count} entries", bytes.len() - r.pos);
    Ok(entries)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.bytes.get(self.pos..self.pos + n).context("unexpected end of index")?;
        self.pos += n;
        Ok(s)
    }
    fn be_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn be_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn id_str(&mut self) -> Result<String> {
        let len = u32::from_le_bytes(self.take(4)?.try_into()?) as usize;
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
}
