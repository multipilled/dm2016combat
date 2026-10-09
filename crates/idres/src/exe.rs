//! Offline scan of the user's own DOOM (2016) executable for console-variable registrations.
//!
//! Static initialisers construct every cvar with the same call shape:
//! `lea r8,[default]; lea rdx,[name]; lea rcx,[this]; call idCVar::idCVar` (the min/max variant
//! interleaves a few SSE moves between those instructions). Reading those three RIP-relative
//! addresses yields each cvar's name, default value string and object address. Nothing read here
//! is stored in this repository; the table is rebuilt from the user's install at startup.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

#[derive(Debug, Clone)]
pub struct CvarInfo {
    pub default: String,
    pub object_va: u64,
}

#[derive(Debug, Default, Clone)]
pub struct CvarTable {
    pub cvars: HashMap<String, CvarInfo>,
}

impl CvarTable {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.cvars.get(name).map(|c| c.default.as_str())
    }
}

struct Section {
    va: u32,
    raw_ptr: u32,
    raw_size: u32,
    name: [u8; 8],
}

struct Pe<'a> {
    data: &'a [u8],
    sections: Vec<Section>,
    image_base: u64,
}

impl<'a> Pe<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let rd16 = |o: usize| u16::from_le_bytes(data[o..o + 2].try_into().unwrap());
        let rd32 = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        if data.get(..2) != Some(b"MZ") {
            bail!("not a PE file");
        }
        let pe = rd32(0x3c) as usize;
        if data.get(pe..pe + 4) != Some(b"PE\0\0") {
            bail!("missing PE signature");
        }
        let nsec = rd16(pe + 6) as usize;
        let opt_size = rd16(pe + 20) as usize;
        let image_base = u64::from_le_bytes(data[pe + 24 + 24..pe + 24 + 32].try_into()?);
        let mut sections = Vec::with_capacity(nsec);
        for i in 0..nsec {
            let o = pe + 24 + opt_size + i * 40;
            sections.push(Section {
                name: data[o..o + 8].try_into()?,
                va: rd32(o + 12),
                raw_size: rd32(o + 16),
                raw_ptr: rd32(o + 20),
            });
        }
        Ok(Self { data, sections, image_base })
    }

    fn rva_to_offset(&self, rva: u32) -> Option<usize> {
        self.sections
            .iter()
            .find(|s| s.va <= rva && rva < s.va + s.raw_size)
            .map(|s| (s.raw_ptr + rva - s.va) as usize)
    }

    fn c_str(&self, rva: u32) -> Option<&'a str> {
        let o = self.rva_to_offset(rva)?;
        let end = self.data[o..].iter().take(4096).position(|&b| b == 0)? + o;
        std::str::from_utf8(&self.data[o..end]).ok()
    }
}

const LEA_RDX: [u8; 3] = [0x48, 0x8d, 0x15];
const LEA_RCX: [u8; 3] = [0x48, 0x8d, 0x0d];
const LEA_R8: [u8; 3] = [0x4c, 0x8d, 0x05];

/// Any C identifier: a candidate cvar name before the constructor check.
fn is_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 96 && b[0].is_ascii_alphabetic() && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// The common `prefix_name` form (lowercase prefix), accepted without the constructor check.
fn is_cvar_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_lowercase()
        && s.contains('_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && s.split('_').next().is_some_and(|p| p.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
}

pub fn scan_cvars(exe: &Path) -> Result<CvarTable> {
    let file = std::fs::File::open(exe).with_context(|| format!("opening {}", exe.display()))?;
    // SAFETY: the executable is not modified while mapped.
    let map = unsafe { Mmap::map(&file)? };
    let pe = Pe::parse(&map)?;
    let text = pe.sections.iter().find(|s| s.name.starts_with(b".text")).context("no .text section")?;
    let code = &map[text.raw_ptr as usize..(text.raw_ptr + text.raw_size) as usize];
    let lea_target = |i: usize| -> u32 {
        let disp = i32::from_le_bytes(code[i + 3..i + 7].try_into().unwrap());
        (text.va as i64 + i as i64 + 7 + disp as i64) as u32
    };

    // Every `lea rdx,[string]` with a `lea r8,[string]` before it and `lea rcx,[object]` after it,
    // plus the first direct call that follows (the idCVar constructor for real registrations).
    struct Site<'s> {
        name: &'s str,
        default: &'s str,
        object_rva: u32,
        call: Option<u32>,
    }
    let mut sites = Vec::new();
    let mut i = 40;
    while i + 64 < code.len() {
        if code[i..i + 3] != LEA_RDX {
            i += 1;
            continue;
        }
        let at = i;
        i += 1;
        let Some(name) = pe.c_str(lea_target(at)).filter(|n| is_identifier(n)) else { continue };
        let r8 = (at.saturating_sub(40)..=at - 7).rev().find(|&j| code[j..j + 3] == LEA_R8);
        let rcx = (at + 7..at + 24).find(|&j| code[j..j + 3] == LEA_RCX);
        let (Some(r8), Some(rcx)) = (r8, rcx) else { continue };
        let Some(default) = pe.c_str(lea_target(r8)).filter(|v| v.len() <= 64) else { continue };
        let call = (rcx + 7..rcx + 48).find(|&j| code[j] == 0xe8).map(|j| {
            let rel = i32::from_le_bytes(code[j + 1..j + 5].try_into().unwrap());
            (text.va as i64 + j as i64 + 5 + rel as i64) as u32
        });
        sites.push(Site { name, default, object_rva: lea_target(rcx), call });
    }

    // Learn the constructors from the sites whose names follow the usual `prefix_name` convention,
    // then also accept names outside that convention (`timescale`, `handsBobCycle_Enable`) when they
    // are passed to one of those constructors.
    let mut calls: HashMap<u32, usize> = HashMap::new();
    for s in sites.iter().filter(|s| is_cvar_name(s.name)) {
        if let Some(c) = s.call {
            *calls.entry(c).or_default() += 1;
        }
    }
    let constructors: Vec<u32> = calls.iter().filter(|(_, n)| **n >= 100).map(|(c, _)| *c).collect();
    let mut table = CvarTable::default();
    for s in &sites {
        let ctor = s.call.is_some_and(|c| constructors.contains(&c));
        if is_cvar_name(s.name) || ctor {
            table.cvars.entry(s.name.to_string()).or_insert(CvarInfo {
                default: s.default.to_string(),
                object_va: pe.image_base + s.object_rva as u64,
            });
        }
    }
    if table.cvars.len() < 1000 {
        bail!("only {} cvars found in {}; unexpected executable", table.cvars.len(), exe.display());
    }
    Ok(table)
}

/// Applies a console config (`name value`, `set[a] name value`, `reset name`) on top of a table.
/// `reset` restores the exe default, which is what the shipped default_sp.cfg relies on.
pub fn apply_cfg(values: &mut HashMap<String, String>, defaults: &CvarTable, cfg: &str) {
    for line in cfg.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue };
        match first.to_ascii_lowercase().as_str() {
            "reset" => {
                if let Some(name) = words.next() {
                    if let Some(d) = defaults.get(name) {
                        values.insert(name.to_string(), d.to_string());
                    }
                }
            }
            "set" | "seta" | "sets" | "setu" => {
                if let (Some(name), Some(v)) = (words.next(), words.next()) {
                    values.insert(name.to_string(), v.trim_matches('"').to_string());
                }
            }
            name if defaults.cvars.contains_key(name) => {
                if let Some(v) = words.next() {
                    values.insert(name.to_string(), v.trim_matches('"').to_string());
                }
            }
            _ => {}
        }
    }
}
