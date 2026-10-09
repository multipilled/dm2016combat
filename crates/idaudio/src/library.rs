//! Everything in `base/sound/soundbanks/pc`, indexed: banks, packages, HIRC objects, media and the
//! event table.
//!
//! Load order follows exe 0x141a123f0: `Init.bnk`, `doom_initial.bnk`, `initial.pck`, then every
//! `.bnk` and `.pck` in the folder, then the language subfolder. When two banks define the same
//! object, the first one loaded wins (Wwise keeps the resident copy).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::bnk::{Bank, DidxEntry};
use crate::events::{EventInfo, EventTable};
use crate::hirc::{self, Action, Event, Object, kind};
use crate::pck::{Package, PckEntry};
use crate::wem::{self, Decoded, WemInfo};

pub const DEFAULT_LANGUAGE: &str = "English(US)";

#[derive(Debug, Clone, Copy)]
pub enum MediaLocation {
    Bank { bank: usize, entry: DidxEntry },
    Package { package: usize, entry: PckEntry },
}

/// A media file reached from an event: the Sound object that plays it and its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaRef {
    pub media_id: u32,
    pub sound_id: u32,
    pub plugin_id: u32,
    pub stream_type: u8,
}

/// Switch-group and state-group values used when walking switch containers.
#[derive(Debug, Clone, Default)]
pub struct Switches {
    pub values: HashMap<u32, u32>,
}

impl Switches {
    pub fn set(&mut self, group: &str, value: &str) -> &mut Self {
        self.values.insert(crate::hash::fnv1_lower(group), crate::hash::fnv1_lower(value));
        self
    }
    pub fn get(&self, group: u32) -> Option<u32> {
        self.values.get(&group).copied()
    }
}

/// One node of an event's play tree, for display and analysis.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub id: u32,
    pub kind: u8,
    /// Switch value this subtree is assigned to (for switch-container children).
    pub switch_value: Option<u32>,
    pub label: String,
    pub children: Vec<TreeNode>,
}

pub struct AudioLibrary {
    pub dir: PathBuf,
    pub banks: Vec<Bank>,
    pub packages: Vec<Package>,
    pub events_table: EventTable,
    event_info: HashMap<u32, usize>,
    events: HashMap<u32, Event>,
    actions: HashMap<u32, Action>,
    nodes: HashMap<u32, Object>,
    other: HashMap<u32, (u8, usize)>,
    media: HashMap<u32, Vec<MediaLocation>>,
    attenuations: HashMap<u32, hirc::Attenuation>,
    /// Init.bnk STMG.
    pub globals: crate::bnk::GlobalSettings,
    game_param_defaults: HashMap<u32, f32>,
    /// (object class, id) -> (bank, HIRC entry index) of the copy in use.
    first_def: HashMap<(u8, u32), (usize, usize)>,
    /// Objects defined in more than one bank with different bytes: (id, first bank, other bank).
    pub conflicts: Vec<(u32, usize, usize)>,
    /// Objects that failed to parse: (bank, id, error).
    pub parse_errors: Vec<(usize, u32, String)>,
}

/// `<doom>/base/sound/soundbanks/pc`.
pub fn soundbank_dir(doom_dir: &Path) -> PathBuf {
    doom_dir.join("base").join("sound").join("soundbanks").join("pc")
}

fn sorted_files(dir: &Path, ext: &str) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
        .collect();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    Ok(files)
}

impl AudioLibrary {
    /// Opens the install's sound data with English (US) voice-over.
    pub fn open(doom_dir: &Path) -> Result<Self> {
        Self::open_with(doom_dir, Some(DEFAULT_LANGUAGE))
    }

    /// `language` is a subfolder of `soundbanks/pc` (e.g. `"French(France)"`); `None` skips voice-over.
    pub fn open_with(doom_dir: &Path, language: Option<&str>) -> Result<Self> {
        let dir = soundbank_dir(doom_dir);
        let events_table = EventTable::open(&dir.join("soundbanksinfo.events"))?;
        let mut lib = Self {
            dir: dir.clone(),
            banks: Vec::new(),
            packages: Vec::new(),
            event_info: events_table.events.iter().enumerate().map(|(i, e)| (e.id, i)).collect(),
            events_table,
            events: HashMap::new(),
            actions: HashMap::new(),
            nodes: HashMap::new(),
            other: HashMap::new(),
            media: HashMap::new(),
            first_def: HashMap::new(),
            attenuations: HashMap::new(),
            globals: Default::default(),
            game_param_defaults: HashMap::new(),
            conflicts: Vec::new(),
            parse_errors: Vec::new(),
        };

        let mut bank_files = vec![dir.join("Init.bnk"), dir.join("doom_initial.bnk")];
        let mut pck_files = vec![dir.join("initial.pck")];
        for p in sorted_files(&dir, "bnk")? {
            if !bank_files.contains(&p) {
                bank_files.push(p);
            }
        }
        for p in sorted_files(&dir, "pck")? {
            if !pck_files.contains(&p) {
                pck_files.push(p);
            }
        }
        if let Some(lang) = language {
            let ldir = dir.join(lang);
            bank_files.extend(sorted_files(&ldir, "bnk")?);
            pck_files.extend(sorted_files(&ldir, "pck")?);
        }

        for p in &bank_files {
            let bank = Bank::open(p)?;
            lib.add_bank(bank);
        }
        for p in &pck_files {
            let pck = Package::open(p)?;
            let pi = lib.packages.len();
            for e in &pck.streams {
                lib.media.entry(e.id).or_default().push(MediaLocation::Package { package: pi, entry: *e });
            }
            let embedded: Vec<Bank> = pck
                .banks
                .iter()
                .filter(|e| !lib.banks.iter().any(|b| b.id == e.id))
                .map(|e| {
                    let name = format!("{}#{}", p.file_name().unwrap_or_default().to_string_lossy(), e.id);
                    Bank::from_map(name, pck.map.clone(), e.offset() as usize, e.size as usize)
                })
                .collect::<Result<_>>()?;
            lib.packages.push(pck);
            for bank in embedded {
                lib.add_bank(bank);
            }
        }
        Ok(lib)
    }

    fn add_bank(&mut self, bank: Bank) {
        let bi = self.banks.len();
        match bank.global_settings() {
            Ok(Some(g)) if self.game_param_defaults.is_empty() => {
                self.game_param_defaults = g.game_params.iter().map(|p| (p.0, p.1)).collect();
                self.globals = g;
            }
            Ok(_) => {}
            Err(err) => self.parse_errors.push((bi, 0, format!("STMG: {err:#}"))),
        }
        for e in &bank.media {
            self.media.entry(e.id).or_default().push(MediaLocation::Bank { bank: bi, entry: *e });
        }
        for (ei, e) in bank.hirc.iter().enumerate() {
            let body = bank.object_bytes(e);
            let class = match e.kind {
                kind::EVENT => 0,
                kind::ACTION => 1,
                kind::SOUND | kind::RAN_SEQ | kind::SWITCH | kind::LAYER | kind::ACTOR_MIXER => 2,
                _ => 3,
            };
            if let Some(&(fb, fe)) = self.first_def.get(&(class, e.id)) {
                let first = if fb == bi { &bank } else { &self.banks[fb] };
                if first.object_bytes(&first.hirc[fe]) != body {
                    self.conflicts.push((e.id, fb, bi));
                }
                continue;
            }
            self.first_def.insert((class, e.id), (bi, ei));
            if class == 3 {
                self.other.insert(e.id, (e.kind, bi));
                if e.kind == kind::ATTENUATION {
                    match hirc::parse_attenuation(body) {
                        Ok(a) => {
                            self.attenuations.insert(e.id, a);
                        }
                        Err(err) => self.parse_errors.push((bi, e.id, format!("{err:#}"))),
                    }
                }
                continue;
            }
            match hirc::parse(e.kind, body) {
                Ok(Object::Event(ev)) => {
                    self.events.insert(e.id, ev);
                }
                Ok(Object::Action(a)) => {
                    self.actions.insert(e.id, a);
                }
                Ok(obj) => {
                    self.nodes.insert(e.id, obj);
                }
                Err(err) => self.parse_errors.push((bi, e.id, format!("{err:#}"))),
            }
        }
        self.banks.push(bank);
    }

    pub fn event_info(&self, id: u32) -> Option<&EventInfo> {
        self.event_info.get(&id).map(|&i| &self.events_table.events[i])
    }

    /// Event table entry by event name (any case).
    pub fn event_info_by_name(&self, name: &str) -> Option<&EventInfo> {
        self.event_info(crate::hash::event_id(name))
    }

    pub fn event(&self, id: u32) -> Option<&Event> {
        self.events.get(&id)
    }

    pub fn action(&self, id: u32) -> Option<&Action> {
        self.actions.get(&id)
    }

    /// Sound / container / actor-mixer object.
    pub fn node(&self, id: u32) -> Option<&Object> {
        self.nodes.get(&id)
    }

    /// Kind and defining bank of an object that is not decoded (bus, music, attenuation, ...).
    pub fn other_object(&self, id: u32) -> Option<(u8, usize)> {
        self.other.get(&id).copied()
    }

    /// Raw body of an object that is not decoded (bus, music, effect, ...).
    pub fn other_object_bytes(&self, id: u32) -> Option<(u8, &[u8])> {
        let (k, _) = *self.other.get(&id)?;
        let &(bank, entry) = self.first_def.get(&(3, id))?;
        let b = &self.banks[bank];
        Some((k, b.object_bytes(&b.hirc[entry])))
    }

    /// Parameter block of a source plugin (Silence, Sine, ...). Wwise stores it as an FxCustom
    /// object whose id is the sound's source id: u32 plugin id, u32 size, parameters.
    pub fn source_plugin_params(&self, source_id: u32) -> Option<(u32, &[u8])> {
        let (k, body) = self.other_object_bytes(source_id)?;
        if k != kind::FX_CUSTOM || body.len() < 8 {
            return None;
        }
        let plugin = u32::from_le_bytes(body[0..4].try_into().ok()?);
        let size = u32::from_le_bytes(body[4..8].try_into().ok()?) as usize;
        Some((plugin, body.get(8..8 + size)?))
    }

    /// A bus or aux bus (Init.bnk): (parent bus, props). Only the leading prop bundle is decoded.
    pub fn bus(&self, id: u32) -> Option<(u32, Vec<(u8, u32)>)> {
        let (k, body) = self.other_object_bytes(id)?;
        if k != kind::BUS && k != kind::AUX_BUS {
            return None;
        }
        let mut r = crate::read::Cursor::new(body);
        let parent = r.u32().ok()?;
        let n = r.u8().ok()? as usize;
        let ids = r.bytes(n).ok()?.to_vec();
        let props = ids.into_iter().map(|p| r.u32().map(|v| (p, v))).collect::<Result<Vec<_>>>().ok()?;
        Some((parent, props))
    }

    /// Bus name from the event table's bus list (bus ids are FNV hashes of the names).
    pub fn bus_name(&self, id: u32) -> Option<&str> {
        self.events_table.buses.iter().find(|(n, _)| crate::hash::fnv1_lower(n) == id).map(|(n, _)| n.as_str())
    }

    /// The bus and its ancestors with their static gain (props Volume + BusVolume, dB). Ducking, HDR,
    /// RTPCs and effects are not included.
    pub fn bus_chain(&self, bus: u32) -> Vec<(u32, f32)> {
        let mut out = Vec::new();
        let mut cur = bus;
        while cur != 0 && !out.iter().any(|(b, _)| *b == cur) {
            let Some((parent, props)) = self.bus(cur) else { break };
            let db = hirc::prop_raw(&props, hirc::prop::VOLUME).map_or(0.0, f32::from_bits)
                + hirc::prop_raw(&props, hirc::prop::BUS_VOLUME).map_or(0.0, f32::from_bits);
            out.push((cur, db));
            cur = parent;
        }
        out
    }

    /// RTPC curves on a bus (e.g. Master Audio Bus ← master_volume, param 5 BusVolume). The bus layout
    /// after its prop bundle is not decoded, so entries are located by game-parameter id (Init.bnk STMG)
    /// and kept only when they parse as a complete RTPC entry inside the body: u32 id, u8 type 0, u8
    /// accum, u8 param, u32 curve id, u8 scaling, u16 n, n × (f32 x ascending, f32 y, u32 interp ≤ 9).
    pub fn bus_rtpcs(&self, bus: u32) -> Vec<hirc::RtpcCurve> {
        let Some((_, body)) = self.other_object_bytes(bus) else { return Vec::new() };
        let entry = |b: &[u8]| -> Option<hirc::RtpcCurve> {
            let mut r = crate::read::Cursor::new(b);
            let rtpc = r.u32().ok()?;
            let (rtpc_type, accum, param) = (r.u8().ok()?, r.u8().ok()?, r.u8().ok()?);
            let curve_id = r.u32().ok()?;
            let scaling = r.u8().ok()?;
            let n = r.u16().ok()? as usize;
            if rtpc_type != 0 || param >= 64 || scaling > 5 || n == 0 || n > 64 {
                return None;
            }
            let points = (0..n).map(|_| Some((r.f32().ok()?, r.f32().ok()?, r.u32().ok()?))).collect::<Option<Vec<_>>>()?;
            let sane = points.windows(2).all(|w| w[0].0 <= w[1].0) && points.iter().all(|p| p.2 <= 9 && p.0.is_finite() && p.1.is_finite());
            sane.then_some(hirc::RtpcCurve { rtpc, rtpc_type, accum, param, curve_id, scaling, points })
        };
        let mut out = Vec::new();
        for &(id, ..) in &self.globals.game_params {
            let pat = id.to_le_bytes();
            for k in 4..body.len().saturating_sub(4) {
                if body[k..k + 4] == pat {
                    if let Some(c) = entry(&body[k..]) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// Reverb of an aux bus: its first FX of plugin 0x00021033 (DOOM's environment reverb; a
    /// third-party effect) → params (wet?, crossover low Hz, crossover high Hz, decay low/mid/high s).
    /// The FX reference is found by scanning the bus body for a known FX id (bus layout after the prop
    /// bundle is not decoded).
    pub fn aux_reverb(&self, bus_id: u32) -> Option<[f32; 6]> {
        let (k, body) = self.other_object_bytes(bus_id)?;
        if k != kind::AUX_BUS && k != kind::BUS {
            return None;
        }
        for off in 4..body.len().saturating_sub(4) {
            let id = u32::from_le_bytes(body[off..off + 4].try_into().ok()?);
            let Some((fk, fx)) = self.other_object_bytes(id) else { continue };
            if (fk == kind::FX_SHARE_SET || fk == kind::FX_CUSTOM) && fx.len() >= 8 + 24 {
                let plugin = u32::from_le_bytes(fx[0..4].try_into().ok()?);
                if plugin == 0x0002_1033 {
                    let f = |i: usize| f32::from_le_bytes(fx[8 + 4 * i..12 + 4 * i].try_into().unwrap());
                    return Some([f(0), f(1), f(2), f(3), f(4), f(5)]);
                }
            }
        }
        None
    }

    /// Silence source (plugin 0x00650002) duration: (seconds, random minus, random plus).
    pub fn silence_duration(&self, source_id: u32) -> Option<(f32, f32, f32)> {
        match self.source_plugin_params(source_id)? {
            (0x0065_0002, p) if p.len() >= 12 => {
                let f = |o: usize| f32::from_le_bytes(p[o..o + 4].try_into().unwrap());
                Some((f(0), f(4), f(8)))
            }
            _ => None,
        }
    }

    pub fn event_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.events.keys().copied()
    }

    pub fn nodes(&self) -> impl Iterator<Item = (u32, &Object)> {
        self.nodes.iter().map(|(k, v)| (*k, v))
    }

    pub fn media_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.media.keys().copied()
    }

    pub fn media_locations(&self, media_id: u32) -> &[MediaLocation] {
        self.media.get(&media_id).map_or(&[], |v| v.as_slice())
    }

    pub fn location_bytes(&self, loc: &MediaLocation) -> &[u8] {
        match loc {
            MediaLocation::Bank { bank, entry } => self.banks[*bank].media_bytes(entry),
            MediaLocation::Package { package, entry } => self.packages[*package].bytes(entry),
        }
    }

    pub fn location_name(&self, loc: &MediaLocation) -> String {
        match loc {
            MediaLocation::Bank { bank, .. } => self.banks[*bank].name.clone(),
            MediaLocation::Package { package, .. } => {
                self.packages[*package].path.file_name().unwrap_or_default().to_string_lossy().into_owned()
            }
        }
    }

    /// The complete `.wem` for a media id: a stream-package copy if there is one (bank copies of
    /// streamed sounds are only the prefetch prefix), else the first complete bank copy.
    pub fn media_bytes(&self, media_id: u32) -> Result<&[u8]> {
        let locs = self.media_locations(media_id);
        if locs.is_empty() {
            bail!("media {media_id} is in no loaded bank or package");
        }
        if let Some(l) = locs.iter().find(|l| matches!(l, MediaLocation::Package { .. })) {
            return Ok(self.location_bytes(l));
        }
        let complete = locs.iter().find(|l| wem::parse(self.location_bytes(l)).is_ok_and(|i| !i.is_truncated()));
        Ok(self.location_bytes(complete.unwrap_or(&locs[0])))
    }

    pub fn media_info(&self, media_id: u32) -> Result<WemInfo> {
        wem::parse(self.media_bytes(media_id)?).with_context(|| format!("media {media_id}"))
    }

    pub fn decode_media(&self, media_id: u32) -> Result<Decoded> {
        wem::decode(self.media_bytes(media_id)?).with_context(|| format!("decoding media {media_id}"))
    }

    /// Every media file an event can play, across all switch values and random choices.
    pub fn resolve_event(&self, event_id: u32) -> Result<Vec<MediaRef>> {
        self.resolve(event_id, None)
    }

    /// Media an event can play with the given switch/state values (unset groups use each switch
    /// container's default), still including every random/sequence choice.
    pub fn resolve_event_with(&self, event_id: u32, switches: &Switches) -> Result<Vec<MediaRef>> {
        self.resolve(event_id, Some(switches))
    }

    fn resolve(&self, event_id: u32, switches: Option<&Switches>) -> Result<Vec<MediaRef>> {
        let ev = self.event(event_id).with_context(|| format!("no event {event_id}"))?;
        let mut out = Vec::new();
        for a in &ev.actions {
            let Some(action) = self.action(*a) else { continue };
            if matches!(action.action_type, hirc::action::PLAY | hirc::action::PLAY_AND_CONTINUE) {
                self.collect(action.target, switches, &mut out, 0);
            }
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|m| seen.insert(*m));
        Ok(out)
    }

    fn collect(&self, id: u32, switches: Option<&Switches>, out: &mut Vec<MediaRef>, depth: u32) {
        if depth > 64 {
            return;
        }
        match self.node(id) {
            Some(Object::Sound(s)) => out.push(MediaRef {
                media_id: s.source.source_id,
                sound_id: id,
                plugin_id: s.source.plugin_id,
                stream_type: s.source.stream_type,
            }),
            Some(Object::Switch(sw)) => match switches {
                None => sw.children.iter().for_each(|c| self.collect(*c, switches, out, depth + 1)),
                Some(s) => {
                    let value = s.get(sw.group).unwrap_or(sw.default_switch);
                    if let Some((_, kids)) = sw.switches.iter().find(|(v, _)| *v == value) {
                        kids.iter().for_each(|c| self.collect(*c, switches, out, depth + 1));
                    }
                }
            },
            Some(obj) => obj.children().iter().for_each(|c| self.collect(*c, switches, out, depth + 1)),
            None => {}
        }
    }

    /// The full play tree of an event, for inspection.
    pub fn event_tree(&self, event_id: u32) -> Result<TreeNode> {
        let ev = self.event(event_id).with_context(|| format!("no event {event_id}"))?;
        let name = self.event_info(event_id).map_or_else(String::new, |e| e.name.clone());
        let children = ev
            .actions
            .iter()
            .map(|a| match self.action(*a) {
                Some(action) => {
                    let target = if matches!(action.action_type, hirc::action::PLAY | hirc::action::PLAY_AND_CONTINUE) {
                        vec![self.node_tree(action.target, None, 0)]
                    } else {
                        Vec::new()
                    };
                    TreeNode {
                        id: *a,
                        kind: kind::ACTION,
                        switch_value: None,
                        label: format!("action {:#06x} -> {} props {:?}", action.action_type, action.target, action.props),
                        children: target,
                    }
                }
                None => TreeNode { id: *a, kind: kind::ACTION, switch_value: None, label: "missing action".into(), children: vec![] },
            })
            .collect();
        Ok(TreeNode { id: event_id, kind: kind::EVENT, switch_value: None, label: name, children })
    }

    fn node_tree(&self, id: u32, switch_value: Option<u32>, depth: u32) -> TreeNode {
        let mut node = TreeNode { id, kind: 0, switch_value, label: String::new(), children: Vec::new() };
        let Some(obj) = self.node(id) else {
            node.label = match self.other_object(id) {
                Some((k, b)) => format!("{} (not decoded) in {}", kind::name(k), self.banks[b].name),
                None => "missing".into(),
            };
            return node;
        };
        node.kind = obj.kind();
        if depth > 64 {
            return node;
        }
        let props = |n: &hirc::NodeBase| {
            let mut s = String::new();
            for (p, v) in &n.props {
                s += &format!(" p{p}={}", fmt_prop(*p, *v));
            }
            for (p, a, b) in &n.ranged {
                s += &format!(" r{p}=[{a},{b}]");
            }
            for r in &n.rtpcs {
                let name = self.events_table.group_name(r.rtpc).unwrap_or_else(|| format!("{:#x}", r.rtpc));
                s += &format!(" rtpc[{name}->p{} {}pts s{}]", r.param, r.points.len(), r.scaling);
            }
            if n.positioning.bits & 0x09 == 0x09 {
                s += &format!(" 3d(att {})", n.positioning.attenuation.unwrap_or(0));
            }
            s
        };
        match obj {
            Object::Sound(s) => {
                node.label = format!(
                    "media {} stream {} plugin {:#x}{}",
                    s.source.source_id,
                    s.source.stream_type,
                    s.source.plugin_id,
                    props(&s.node)
                );
            }
            Object::RanSeq(c) => {
                node.label = format!(
                    "{} {} avoid {} loop {} bits {:#x}{}",
                    if c.mode == 0 { "random" } else { "sequence" },
                    if c.bits & hirc::RANSEQ_CONTINUOUS != 0 { "continuous" } else { "step" },
                    c.avoid_repeat,
                    c.loop_count,
                    c.bits,
                    props(&c.node)
                );
                node.children = c.children.iter().map(|k| self.node_tree(*k, None, depth + 1)).collect();
            }
            Object::Switch(c) => {
                let gname = self.events_table.group_name(c.group).unwrap_or_else(|| c.group.to_string());
                let dname = self.events_table.group_name(c.default_switch).unwrap_or_else(|| c.default_switch.to_string());
                node.label = format!("switch on {gname} (default {dname}){}", props(&c.node));
                for (value, kids) in &c.switches {
                    for k in kids {
                        node.children.push(self.node_tree(*k, Some(*value), depth + 1));
                    }
                }
            }
            Object::Layer(c) => {
                node.label = format!("layer x{}{}", c.layers.len(), props(&c.node));
                node.children = c.children.iter().map(|k| self.node_tree(*k, None, depth + 1)).collect();
            }
            Object::ActorMixer(c) => node.label = format!("actor-mixer{}", props(&c.node)),
            _ => {}
        }
        node
    }

    pub fn attenuation(&self, id: u32) -> Option<&hirc::Attenuation> {
        self.attenuations.get(&id)
    }

    /// Default value of a game parameter (RTPC) from Init.bnk; 0 if unknown.
    pub fn game_param_default(&self, id: u32) -> f32 {
        self.game_param_defaults.get(&id).copied().unwrap_or(0.0)
    }

    /// Effective positioning of a node: the nearest node (itself or an ancestor) that overrides its
    /// parent's positioning, else the top of the hierarchy. Returns (is_3d, attenuation id, bits).
    pub fn positioning(&self, id: u32) -> (bool, u32, u8) {
        let mut chain = vec![id];
        chain.extend(self.ancestors(id));
        let mut pick = None;
        for n in &chain {
            if let Some(b) = self.node(*n).and_then(|o| o.node()) {
                pick = Some(b);
                if b.positioning.bits & 0x01 != 0 {
                    break;
                }
            }
        }
        match pick {
            Some(b) if b.positioning.bits & 0x01 != 0 && b.positioning.bits & 0x08 != 0 => {
                (true, b.positioning.attenuation.unwrap_or(0), b.positioning.bits)
            }
            Some(b) => (false, 0, b.positioning.bits),
            None => (false, 0, 0),
        }
    }

    /// Aux sends of a node: (uses game-defined aux sends, user aux sends as (aux bus, send dB)).
    /// AuxParams bits: 0x1 override game aux, 0x2 use game aux, 0x4 override user aux, 0x8 has user aux.
    /// User send volumes are props 18..21 summed over the hierarchy.
    pub fn aux_sends(&self, id: u32) -> (bool, Vec<(u32, f32)>) {
        let mut chain = vec![id];
        chain.extend(self.ancestors(id));
        let bases: Vec<&hirc::NodeBase> = chain.iter().filter_map(|n| self.node(*n).and_then(|o| o.node())).collect();
        let game = bases.iter().find(|b| b.aux_bits & 0x01 != 0).or(bases.last()).is_some_and(|b| b.aux_bits & 0x02 != 0);
        let user = bases.iter().find(|b| b.aux_bits & 0x04 != 0).or(bases.last());
        let mut sends = Vec::new();
        if let Some(u) = user.filter(|u| u.aux_bits & 0x08 != 0) {
            for (k, bus) in u.aux_buses.iter().enumerate() {
                if *bus != 0 {
                    let db: f32 = bases.iter().map(|b| b.prop_f32(hirc::prop::USER_AUX_SEND_VOLUME0 + k as u8).unwrap_or(0.0)).sum();
                    sends.push((*bus, db));
                }
            }
        }
        (game, sends)
    }

    /// Ancestors of a node through `DirectParentID`, nearest first.
    pub fn ancestors(&self, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = self.node(id).and_then(|o| o.node()).map_or(0, |n| n.parent);
        while cur != 0 && !out.contains(&cur) {
            out.push(cur);
            cur = self.node(cur).and_then(|o| o.node()).map_or(0, |n| n.parent);
        }
        out
    }
}

/// Human-readable property value (floats except the integer props).
pub fn fmt_prop(id: u8, raw: u32) -> String {
    match id {
        hirc::prop::LOOP | hirc::prop::DELAY_TIME | hirc::prop::TRANSITION_TIME | 60 => raw.to_string(),
        _ => format!("{}", f32::from_bits(raw)),
    }
}
