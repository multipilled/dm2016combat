//! Wwise soundbanks (`.bnk`, bank version 113 = Wwise 2016.1).
//!
//! A bank is a list of sections (`[4]tag, u32 size, data`):
//! - `BKHD` u32 version (113), u32 bank id (FNV of the bank name), u32 language id, u32 feedback flag,
//!   u32 project id (0x2c1), padding.
//! - `DIDX` {u32 media id, u32 offset into DATA, u32 size} per embedded `.wem` (for prefetched streams
//!   only the first `PrefetchSize` bytes are embedded).
//! - `DATA` the embedded media.
//! - `HIRC` u32 count, then {u8 type, u32 size, u32 id, size-4 bytes} objects (see [`crate::hirc`]).
//! - `STID` (doom_music.bnk) u32 type, u32 count, {u32 bank id, u8 len, name}.
//! - `STMG`/`ENVS`/`PLAT` (Init.bnk only) global settings; not parsed.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use memmap2::Mmap;

use crate::read::Cursor;

#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub tag: [u8; 4],
    /// Absolute offset of the section payload within the bank.
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DidxEntry {
    pub id: u32,
    /// Absolute offset within the bank.
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct HircEntry {
    pub kind: u8,
    pub id: u32,
    /// Absolute offset of the object body (after the id) within the bank.
    pub offset: usize,
    pub len: usize,
}

pub struct Bank {
    pub name: String,
    map: Arc<Mmap>,
    base: usize,
    len: usize,
    pub version: u32,
    pub id: u32,
    pub language: u32,
    pub sections: Vec<Section>,
    pub media: Vec<DidxEntry>,
    pub hirc: Vec<HircEntry>,
    pub bank_names: Vec<(u32, String)>,
}

impl Bank {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: the game's data files are not modified while we read them.
        let map = unsafe { Mmap::map(&file) }.with_context(|| format!("mapping {}", path.display()))?;
        let len = map.len();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self::from_map(name, Arc::new(map), 0, len)
    }

    /// A bank stored at `base..base + len` of a mapped file (e.g. inside a `.pck`).
    pub fn from_map(name: String, map: Arc<Mmap>, base: usize, len: usize) -> Result<Self> {
        ensure!(base + len <= map.len(), "bank range out of file");
        let mut bank = Self {
            name,
            map,
            base,
            len,
            version: 0,
            id: 0,
            language: 0,
            sections: Vec::new(),
            media: Vec::new(),
            hirc: Vec::new(),
            bank_names: Vec::new(),
        };
        bank.parse().with_context(|| format!("parsing bank {}", bank.name))?;
        Ok(bank)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.map[self.base..self.base + self.len]
    }

    pub fn section(&self, tag: &[u8; 4]) -> Option<&Section> {
        self.sections.iter().find(|s| &s.tag == tag)
    }

    pub fn media_bytes(&self, e: &DidxEntry) -> &[u8] {
        &self.bytes()[e.offset..e.offset + e.size]
    }

    pub fn object_bytes(&self, e: &HircEntry) -> &[u8] {
        &self.bytes()[e.offset..e.offset + e.len]
    }

    fn parse(&mut self) -> Result<()> {
        let bytes = &self.map[self.base..self.base + self.len];
        let mut r = Cursor::new(bytes);
        while r.remaining() >= 8 {
            let tag: [u8; 4] = r.bytes(4)?.try_into()?;
            let len = r.u32()? as usize;
            let offset = r.pos;
            r.skip(len).with_context(|| format!("section {} truncated", String::from_utf8_lossy(&tag)))?;
            self.sections.push(Section { tag, offset, len });
        }
        ensure!(r.remaining() == 0, "{} trailing bytes", r.remaining());

        let bkhd = *self.section(b"BKHD").context("no BKHD")?;
        let mut h = Cursor::new(&bytes[bkhd.offset..bkhd.offset + bkhd.len]);
        self.version = h.u32()?;
        self.id = h.u32()?;
        self.language = h.u32()?;
        ensure!(self.version == 113, "bank version {} (only 113 / Wwise 2016.1 is supported)", self.version);

        if let (Some(didx), Some(data)) = (self.section(b"DIDX").copied(), self.section(b"DATA").copied()) {
            ensure!(didx.len % 12 == 0, "DIDX size {} not a multiple of 12", didx.len);
            let mut d = Cursor::new(&bytes[didx.offset..didx.offset + didx.len]);
            for _ in 0..didx.len / 12 {
                let (id, off, size) = (d.u32()?, d.u32()? as usize, d.u32()? as usize);
                ensure!(off + size <= data.len, "media {id} runs past DATA");
                self.media.push(DidxEntry { id, offset: data.offset + off, size });
            }
        }

        if let Some(hirc) = self.section(b"HIRC").copied() {
            let mut o = Cursor::new(&bytes[hirc.offset..hirc.offset + hirc.len]);
            let count = o.u32()?;
            for _ in 0..count {
                let kind = o.u8()?;
                let size = o.u32()? as usize;
                ensure!(size >= 4, "HIRC object smaller than its id");
                let id = o.u32()?;
                let offset = hirc.offset + o.pos;
                o.skip(size - 4)?;
                self.hirc.push(HircEntry { kind, id, offset, len: size - 4 });
            }
            ensure!(o.remaining() == 0, "HIRC has {} trailing bytes", o.remaining());
        }

        if let Some(stid) = self.section(b"STID").copied() {
            let mut s = Cursor::new(&bytes[stid.offset..stid.offset + stid.len]);
            let _kind = s.u32()?;
            for _ in 0..s.u32()? {
                let id = s.u32()?;
                let n = s.u8()? as usize;
                self.bank_names.push((id, String::from_utf8_lossy(s.bytes(n)?).into_owned()));
            }
        }
        Ok(())
    }
}

/// Init.bnk `STMG` (bank 113): f32 volumeThreshold, u16 maxVoices, u32 nStateGroups × {u32 id,
/// u32 defaultTransitionMs, u32 n × (u32 from, u32 to, u32 ms)}, u32 nSwitchGroups × {u32 id,
/// u32 rtpcId, u32 n × (f32 x, u32 switch, u32 interp)} (none in DOOM), u32 nGameParams × {u32 id,
/// f32 default, u32 rampType, f32 rampUp, f32 rampDown, u8 builtIn}. Parses exactly.
#[derive(Debug, Clone, Default)]
pub struct GlobalSettings {
    pub volume_threshold_db: f32,
    pub max_voices: u16,
    /// (state group, default transition ms)
    pub state_groups: Vec<(u32, u32)>,
    /// game parameter id -> (default value, ramp type, ramp up, ramp down, built-in binding)
    pub game_params: Vec<(u32, f32, u32, f32, f32, u8)>,
}

impl Bank {
    pub fn global_settings(&self) -> Result<Option<GlobalSettings>> {
        let Some(s) = self.section(b"STMG").copied() else { return Ok(None) };
        let mut r = Cursor::new(&self.bytes()[s.offset..s.offset + s.len]);
        let mut g = GlobalSettings { volume_threshold_db: r.f32()?, max_voices: r.u16()?, ..Default::default() };
        for _ in 0..r.u32()? {
            let (id, ms) = (r.u32()?, r.u32()?);
            let n = r.u32()? as usize;
            r.skip(12 * n)?;
            g.state_groups.push((id, ms));
        }
        for _ in 0..r.u32()? {
            r.skip(8)?;
            let n = r.u32()? as usize;
            r.skip(12 * n)?;
        }
        for _ in 0..r.u32()? {
            g.game_params.push((r.u32()?, r.f32()?, r.u32()?, r.f32()?, r.f32()?, r.u8()?));
        }
        ensure!(r.remaining() == 0, "STMG has {} unparsed bytes", r.remaining());
        Ok(Some(g))
    }
}
