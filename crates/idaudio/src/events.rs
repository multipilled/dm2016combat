//! `sound/soundbanks/pc/soundbanksinfo.events`: id Software's binary event table, read at startup by
//! exe 0x141689620 (record reader 0x141689d30) and turned into one sound object per event (0x141689b70).
//!
//! Layout: integers big-endian, strings u32 little-endian length + bytes.
//! - u32 event count, per event: u32 id, str name, str work-unit path, str bus, str attenuation,
//!   u8 x5 flags, f32 duration min, f32 duration max, f32 max attenuation, u32 (priority-like:
//!   0/70/90/100), u32 (0-5).
//! - u32 bus count, per bus: str name, str parent path.
//! - u32 state-group count, per group: str name, u32 n, n x str state.
//! - u32 switch-group count, same shape.

use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::hash::fnv1_lower;
use crate::read::Cursor;

#[derive(Debug, Clone)]
pub struct EventInfo {
    pub id: u32,
    pub name: String,
    /// Wwise work-unit folder, e.g. `doom_wep_sp_shotgun_combat/shotgun_combat/`.
    pub path: String,
    pub bus: String,
    pub attenuation: String,
    /// Raw flag bytes. Byte 0 sets 0x8 and byte 2 sets 0x20 on the engine's sound object.
    pub flags: [u8; 5],
    /// Seconds; both 0 for infinite (looping) events.
    pub duration_min: f32,
    pub duration_max: f32,
    pub max_attenuation: f32,
    /// Copied to the engine sound object (+0x74).
    pub unk_u32_a: u32,
    pub unk_u32_b: u32,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub id: u32,
    pub values: Vec<(String, u32)>,
}

#[derive(Debug, Clone, Default)]
pub struct EventTable {
    pub events: Vec<EventInfo>,
    /// (bus name, parent path)
    pub buses: Vec<(String, String)>,
    pub state_groups: Vec<Group>,
    pub switch_groups: Vec<Group>,
}

fn string(r: &mut Cursor) -> Result<String> {
    let n = r.u32()? as usize;
    Ok(String::from_utf8_lossy(r.bytes(n)?).into_owned())
}

fn groups(r: &mut Cursor) -> Result<Vec<Group>> {
    let n = r.be_u32()?;
    (0..n)
        .map(|_| {
            let name = string(r)?;
            let k = r.be_u32()?;
            let values = (0..k)
                .map(|_| {
                    let v = string(r)?;
                    let id = fnv1_lower(&v);
                    Ok((v, id))
                })
                .collect::<Result<_>>()?;
            Ok(Group { id: fnv1_lower(&name), name, values })
        })
        .collect()
}

impl EventTable {
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut r = Cursor::new(bytes);
        let n = r.be_u32()?;
        let mut events = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let id = r.be_u32()?;
            let name = string(&mut r)?;
            let path = string(&mut r)?;
            let bus = string(&mut r)?;
            let attenuation = string(&mut r)?;
            let flags: [u8; 5] = r.bytes(5)?.try_into()?;
            events.push(EventInfo {
                id,
                name,
                path,
                bus,
                attenuation,
                flags,
                duration_min: r.be_f32()?,
                duration_max: r.be_f32()?,
                max_attenuation: r.be_f32()?,
                unk_u32_a: r.be_u32()?,
                unk_u32_b: r.be_u32()?,
            });
        }
        let nb = r.be_u32()?;
        let buses = (0..nb).map(|_| Ok((string(&mut r)?, string(&mut r)?))).collect::<Result<_>>()?;
        let state_groups = groups(&mut r)?;
        let switch_groups = groups(&mut r)?;
        ensure!(r.remaining() == 0, "{} trailing bytes", r.remaining());
        Ok(Self { events, buses, state_groups, switch_groups })
    }

    /// Name of a switch/state group or value id, if it is listed.
    pub fn group_name(&self, id: u32) -> Option<String> {
        for g in self.switch_groups.iter().chain(&self.state_groups) {
            if g.id == id {
                return Some(g.name.clone());
            }
            if let Some((v, _)) = g.values.iter().find(|(_, vid)| *vid == id) {
                return Some(format!("{}/{}", g.name, v));
            }
        }
        None
    }
}
