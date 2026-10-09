//! Posting an event: what Wwise would start, given switch/state values and container history.
//!
//! Implemented: Play actions (delay, probability), Stop/Pause/Resume and SetState commands, sounds,
//! switch containers (switch and state groups, default value), layer containers, random containers
//! (standard and shuffle, weights, avoid-repeat) and sequence containers (wrap or ping-pong) in step
//! mode, and one pass of continuous containers. Per voice: volume / pitch / LPF / HPF / initial delay
//! summed with their random ranges over the sound and every `DirectParentID` ancestor, and the
//! sound's loop count. Not modelled: buses, RTPCs, attenuation, states' property offsets, music
//! objects, transitions between continuous items.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::hash::fnv1_lower;
use crate::hirc::{self, ActionParams, Object, prop};
use crate::library::AudioLibrary;

#[derive(Debug, Clone)]
pub struct Voice {
    /// The Sound object.
    pub sound: u32,
    /// Media (source) id; read it with [`AudioLibrary::decode_media`] unless `plugin_id` is a source
    /// plugin (`plugin_id & 0xf == 2`: 0x650002 Silence, 0x640002 Sine), which have no media.
    pub media: u32,
    pub plugin_id: u32,
    pub stream_type: u8,
    pub volume_db: f32,
    pub pitch_cents: f32,
    pub lpf: f32,
    pub hpf: f32,
    /// Initial delay in seconds (sum over the hierarchy), on top of the action delay.
    pub delay_s: f32,
    /// 0 = loop forever, 1 = play once, n = n times.
    pub loop_count: u32,
    /// First `OverrideBusId` found walking up from the sound (0 = parent bus not overridden).
    pub output_bus: u32,
    /// The sound followed by its ancestors; Stop/Pause/Resume target any of these.
    pub nodes: Vec<u32>,
    /// For a Silence source: its length in seconds (randomised within the plugin's range).
    pub silence_s: Option<f32>,
}

#[derive(Debug, Clone)]
pub enum Playable {
    Voice(Voice),
    /// Start together (layer containers, several children on one switch value).
    Together(Vec<Playable>),
    /// A continuous random/sequence container: play `items` back to back. `loops` is the container's
    /// loop count (0 = forever); call [`PlayState::next_pass`] for each further pass.
    Sequence { container: u32, loops: u16, transition_mode: u8, transition_s: f32, items: Vec<Playable> },
    /// Music objects and other unsupported targets.
    Unsupported { id: u32, kind: u8 },
    Nothing,
}

impl Playable {
    /// All voices, depth-first.
    pub fn voices(&self) -> Vec<&Voice> {
        let mut out = Vec::new();
        fn walk<'a>(p: &'a Playable, out: &mut Vec<&'a Voice>) {
            match p {
                Playable::Voice(v) => out.push(v),
                Playable::Together(v) => v.iter().for_each(|c| walk(c, out)),
                Playable::Sequence { items, .. } => items.iter().for_each(|c| walk(c, out)),
                _ => {}
            }
        }
        walk(self, &mut out);
        out
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    Play { action: u32, delay_s: f32, what: Playable },
    /// Stop (0x01xx), Pause (0x02xx) or Resume (0x03xx) voices whose `nodes` contain `target`.
    Stop { action: u32, target: u32, fade_ms: u32, delay_s: f32 },
    Pause { action: u32, target: u32, fade_ms: u32, delay_s: f32 },
    Resume { action: u32, target: u32, fade_ms: u32, delay_s: f32 },
    SetState { group: u32, state: u32 },
    Unsupported { action: u32, action_type: u16 },
}

#[derive(Debug, Clone, Default)]
struct History {
    /// Most recent last.
    recent: Vec<usize>,
    /// Shuffle: items still to play this cycle.
    pool: Vec<usize>,
    /// Sequence position and direction.
    next: usize,
    backward: bool,
}

/// Switch/state values and random/sequence history. Keep one per game object for non-global
/// containers if that matters; the containers DOOM's player sounds use are global.
pub struct PlayState {
    pub switches: HashMap<u32, u32>,
    pub states: HashMap<u32, u32>,
    histories: HashMap<u32, History>,
    rng: u64,
}

impl PlayState {
    pub fn new(seed: u64) -> Self {
        Self { switches: HashMap::new(), states: HashMap::new(), histories: HashMap::new(), rng: seed ^ 0x9e37_79b9_7f4a_7c15 }
    }

    /// e.g. `set_switch("locality", "first_person")`.
    pub fn set_switch(&mut self, group: &str, value: &str) -> &mut Self {
        self.switches.insert(fnv1_lower(group), fnv1_lower(value));
        self
    }

    pub fn set_state(&mut self, group: &str, value: &str) -> &mut Self {
        self.states.insert(fnv1_lower(group), fnv1_lower(value));
        self
    }

    fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, (lo, hi): (f32, f32)) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// Runs an event's actions in order.
    pub fn post_event(&mut self, lib: &AudioLibrary, event_id: u32) -> Result<Vec<Command>> {
        let ev = lib.event(event_id).with_context(|| format!("no event {event_id}"))?;
        let mut out = Vec::new();
        for &aid in &ev.actions {
            let Some(a) = lib.action(aid) else { continue };
            let mut delay_ms = a.prop_u32(prop::DELAY_TIME).unwrap_or(0) as f32;
            if let Some(&(_, lo, hi)) = a.ranged.iter().find(|r| r.0 == prop::DELAY_TIME) {
                // Integer prop: the range is stored as raw integers.
                delay_ms += self.range((lo.to_bits() as i32 as f32, hi.to_bits() as i32 as f32));
            }
            let delay_s = delay_ms.max(0.0) / 1000.0;
            let fade_ms = a.prop_u32(prop::TRANSITION_TIME).unwrap_or(0);
            match a.params {
                ActionParams::Play { .. } => {
                    if let Some(p) = a.prop_u32(prop::PROBABILITY).map(f32::from_bits)
                        && self.unit() * 100.0 >= p
                    {
                        continue;
                    }
                    let what = self.play_node(lib, a.target, 0);
                    out.push(Command::Play { action: aid, delay_s, what });
                }
                ActionParams::Active { .. } => {
                    let (action, target) = (aid, a.target);
                    out.push(match a.action_type >> 8 {
                        1 => Command::Stop { action, target, fade_ms, delay_s },
                        2 => Command::Pause { action, target, fade_ms, delay_s },
                        _ => Command::Resume { action, target, fade_ms, delay_s },
                    });
                }
                ActionParams::SetState { group, state } => {
                    self.states.insert(group, state);
                    out.push(Command::SetState { group, state });
                }
                ActionParams::Other(_) => out.push(Command::Unsupported { action: aid, action_type: a.action_type }),
            }
        }
        Ok(out)
    }

    /// The next pass of a continuous container returned as [`Playable::Sequence`].
    pub fn next_pass(&mut self, lib: &AudioLibrary, container: u32) -> Playable {
        match lib.node(container) {
            Some(Object::RanSeq(c)) => {
                let picks = (0..c.playlist.len()).filter_map(|_| self.pick(container, c)).collect::<Vec<_>>();
                Playable::Sequence {
                    container,
                    loops: c.loop_count,
                    transition_mode: c.transition_mode,
                    transition_s: c.transition_time / 1000.0,
                    items: picks.into_iter().map(|id| self.play_node(lib, id, 1)).collect(),
                }
            }
            _ => Playable::Nothing,
        }
    }

    fn play_node(&mut self, lib: &AudioLibrary, id: u32, depth: u32) -> Playable {
        if depth > 64 {
            return Playable::Nothing;
        }
        let Some(obj) = lib.node(id) else {
            return match lib.other_object(id) {
                Some((kind, _)) => Playable::Unsupported { id, kind },
                None => Playable::Nothing,
            };
        };
        match obj {
            Object::Sound(s) => Playable::Voice(self.voice(lib, id, s)),
            Object::Switch(sw) => {
                let value = if sw.group_type == 1 { self.states.get(&sw.group) } else { self.switches.get(&sw.group) }
                    .copied()
                    .unwrap_or(sw.default_switch);
                let kids = sw.switches.iter().find(|(v, _)| *v == value).map(|(_, k)| k.clone()).unwrap_or_default();
                together(kids.into_iter().map(|k| self.play_node(lib, k, depth + 1)).collect())
            }
            Object::Layer(l) => together(l.children.iter().map(|k| self.play_node(lib, *k, depth + 1)).collect()),
            Object::RanSeq(c) => {
                if c.bits & hirc::RANSEQ_CONTINUOUS != 0 {
                    if c.bits & hirc::RANSEQ_RESET_PLAYLIST != 0 {
                        self.histories.remove(&id);
                    }
                    let picks = (0..c.playlist.len()).filter_map(|_| self.pick(id, c)).collect::<Vec<_>>();
                    Playable::Sequence {
                        container: id,
                        loops: c.loop_count,
                        transition_mode: c.transition_mode,
                        transition_s: c.transition_time / 1000.0,
                        items: picks.into_iter().map(|k| self.play_node(lib, k, depth + 1)).collect(),
                    }
                } else {
                    match self.pick(id, c) {
                        Some(k) => self.play_node(lib, k, depth + 1),
                        None => Playable::Nothing,
                    }
                }
            }
            Object::ActorMixer(_) | Object::Event(_) | Object::Action(_) | Object::Other(_) => Playable::Nothing,
        }
    }

    /// Chooses the next playlist item of a random or sequence container.
    fn pick(&mut self, id: u32, c: &hirc::RanSeq) -> Option<u32> {
        let n = c.playlist.len();
        if n == 0 {
            return None;
        }
        let mut h = self.histories.remove(&id).unwrap_or_default();
        let chosen = if c.mode == 1 {
            // Sequence: wrap around, or reverse at the ends with "restart backward".
            let i = h.next.min(n - 1);
            if c.bits & hirc::RANSEQ_RESTART_BACKWARD != 0 && n > 1 {
                if h.backward {
                    if i == 0 {
                        h.backward = false;
                        h.next = 1;
                    } else {
                        h.next = i - 1;
                    }
                } else if i + 1 == n {
                    h.backward = true;
                    h.next = n - 2;
                } else {
                    h.next = i + 1;
                }
            } else {
                h.next = (i + 1) % n;
            }
            i
        } else {
            let avoid = (c.avoid_repeat as usize).min(n - 1);
            let weight = |i: usize| if c.bits & hirc::RANSEQ_USING_WEIGHT != 0 { c.playlist[i].1.max(0) as f32 } else { 1.0 };
            let candidates: Vec<usize> = if c.random_mode == 1 {
                // Shuffle: every item once per cycle; refill without the last `avoid` played.
                h.pool.retain(|i| *i < n);
                if h.pool.is_empty() {
                    h.pool = (0..n).filter(|i| !h.recent.iter().rev().take(avoid).any(|r| r == i)).collect();
                }
                h.pool.clone()
            } else {
                (0..n).filter(|i| !h.recent.iter().rev().take(avoid).any(|r| r == i)).collect()
            };
            let total: f32 = candidates.iter().map(|&i| weight(i)).sum();
            let mut x = self.unit() * total;
            let mut pick = *candidates.last()?;
            for &i in &candidates {
                x -= weight(i);
                if x < 0.0 {
                    pick = i;
                    break;
                }
            }
            h.pool.retain(|&i| i != pick);
            h.recent.push(pick);
            if h.recent.len() > n {
                h.recent.remove(0);
            }
            pick
        };
        self.histories.insert(id, h);
        Some(c.playlist[chosen].0)
    }

    fn voice(&mut self, lib: &AudioLibrary, id: u32, s: &hirc::Sound) -> Voice {
        let mut nodes = vec![id];
        nodes.extend(lib.ancestors(id));
        let mut v = Voice {
            sound: id,
            media: s.source.source_id,
            plugin_id: s.source.plugin_id,
            stream_type: s.source.stream_type,
            volume_db: 0.0,
            pitch_cents: 0.0,
            lpf: 0.0,
            hpf: 0.0,
            delay_s: 0.0,
            loop_count: s.node.prop_u32(prop::LOOP).unwrap_or(1),
            output_bus: 0,
            nodes: Vec::new(),
            silence_s: None,
        };
        if let Some((d, minus, plus)) = lib.silence_duration(s.source.source_id) {
            v.silence_s = Some((d + self.range((-minus, plus))).max(0.0));
        }
        for n in &nodes {
            let Some(base) = lib.node(*n).and_then(|o| o.node()) else { continue };
            let mut add = |p: u8| base.prop_f32(p).unwrap_or(0.0) + base.range(p).map_or(0.0, |r| self.range(r));
            v.volume_db += add(prop::VOLUME);
            v.pitch_cents += add(prop::PITCH);
            v.lpf += add(prop::LPF);
            v.hpf += add(prop::HPF);
            v.delay_s += add(prop::INITIAL_DELAY);
            if v.output_bus == 0 {
                v.output_bus = base.override_bus;
            }
        }
        v.lpf = v.lpf.clamp(0.0, 100.0);
        v.hpf = v.hpf.clamp(0.0, 100.0);
        v.nodes = nodes;
        v
    }
}

fn together(mut v: Vec<Playable>) -> Playable {
    v.retain(|p| !matches!(p, Playable::Nothing));
    match v.len() {
        0 => Playable::Nothing,
        1 => v.pop().unwrap(),
        _ => Playable::Together(v),
    }
}
