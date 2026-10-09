//! Game-thread side of playback: posts events like DOOM's idSound / Wwise game objects and keeps the
//! mixer's voices up to date (3D attenuation and panning, RTPCs, buses, environment sends).
//!
//! Engine behaviour reproduced (exe idSound::Update 0x1418558f0, idWeapon::PlayFireSound 0x140f1def0):
//! - `locality` switch: first_person when the emitter is the listener's own entity, else third_person.
//! - `sound_volume` RTPC = the sound's volume in dB (sound decl `parms.volume`; every sound decl inherits
//!   `sound/default` = -6), `sound_pitch` = pitch in cents (0).
//! - Environment (`idSoundEnvironment` entities, `soundEnvironment_t`): game-defined aux send to the
//!   environment's bus at `auxSendLevel`, output-bus (dry) volume `dryGainLevel`, and the `gun_env`
//!   switch value stamped on weapon fire sounds.
//! Approximations (documented in AUDIO.md): constant-power stereo panning with spread, one reverb for all
//! aux sends, LPF/HPF value→cutoff mapping, bus gains from static props only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::curve::db_to_gain;
use crate::hash::fnv1_lower;
use crate::hirc::{atten_slot, prop};
use crate::library::AudioLibrary;
use crate::mix::{MAX_CHANNELS, Mixer, Pcm, ReverbParams, VoiceParams, VoiceStart};
use crate::play::{Command, PlayState, Playable, Voice};
use crate::wem::{SPEAKER_FRONT_CENTER, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, SPEAKER_LFE};

pub type Vec3 = [f32; 3];

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Listener frame in game units. Any right-handed axes work as long as emitters use the same space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    pub pos: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
}

impl Default for Listener {
    fn default() -> Self {
        Self { pos: [0.0; 3], forward: [1.0, 0.0, 0.0], up: [0.0, 0.0, 1.0] }
    }
}

/// An `idSoundEnvironment` (map entity) the listener is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    /// Aux bus name (e.g. `sp_foundry_mainroom`), the game-defined aux send target.
    pub aux_bus: String,
    /// `gun_env` switch value (int_small, int_med, int_large, ext_large, ext_normal, int_pipe, hell).
    pub gun_env: String,
    /// Linear send level.
    pub aux_send_level: f32,
    /// Linear dry (output bus) gain.
    pub dry_gain: f32,
    pub reverb: ReverbParams,
}

impl Environment {
    /// An environment for an aux bus by name, with the reverb tuned from the bus's authored decay
    /// times (APPROXIMATION: mid-band RT60 → Freeverb feedback over its ~29 ms mean comb delay; damping
    /// from the high/mid decay ratio).
    pub fn for_bus(lib: &AudioLibrary, aux_bus: &str, gun_env: &str, aux_send_level: f32, dry_gain: f32) -> Self {
        let mut reverb = ReverbParams::default();
        if let Some(p) = lib.aux_reverb(fnv1_lower(aux_bus)) {
            let (mid, high) = (p[4].max(0.05), p[5].max(0.05));
            let g = 10f32.powf(-3.0 * 0.0295 / mid).clamp(0.7, 0.98);
            reverb.room_size = (g - 0.7) / 0.28;
            reverb.damping = (1.0 - high / mid * 0.5).clamp(0.0, 1.0);
        }
        Self { aux_bus: aux_bus.to_owned(), gun_env: gun_env.to_owned(), aux_send_level, dry_gain, reverb }
    }
}

/// LPF value (0..100) → one-pole cutoff. APPROXIMATION: exponential from 20 kHz (0) to 10 Hz (100).
pub fn lpf_cutoff(v: f32) -> f32 {
    if v <= 0.0 { 0.0 } else { 20000.0 * (10.0f32 / 20000.0).powf(v.min(100.0) / 100.0) }
}

/// HPF value (0..100) → one-pole cutoff. APPROXIMATION: exponential from 10 Hz (0) to 20 kHz (100).
pub fn hpf_cutoff(v: f32) -> f32 {
    if v <= 0.0 { 0.0 } else { 10.0 * 2000.0f32.powf(v.min(100.0) / 100.0) }
}

/// Stereo gains for each source channel without positioning (Wwise 2D): L→L, R→R, C → both at -3 dB,
/// LFE dropped, surrounds folded at -3 dB to their side.
pub fn downmix_gains(channels: u16, mask: u32) -> [[f32; 2]; MAX_CHANNELS] {
    let h = std::f32::consts::FRAC_1_SQRT_2;
    let mut g = [[0.0; 2]; MAX_CHANNELS];
    let mut bit = 0;
    for slot in g.iter_mut().take((channels as usize).min(MAX_CHANNELS)) {
        while bit < 32 && mask & (1 << bit) == 0 {
            bit += 1;
        }
        let sp = if bit < 32 { 1u32 << bit } else { 0 };
        bit += 1;
        *slot = match sp {
            SPEAKER_FRONT_LEFT => [1.0, 0.0],
            SPEAKER_FRONT_RIGHT => [0.0, 1.0],
            SPEAKER_FRONT_CENTER => [h, h],
            SPEAKER_LFE => [0.0, 0.0],
            0x10 | 0x200 => [h, 0.0],
            0x20 | 0x400 => [0.0, h],
            _ => [h * 0.5, h * 0.5],
        };
    }
    g
}

struct Instance {
    playing_id: u32,
    emitter: u64,
    voice: Voice,
    pcm: Arc<Pcm>,
    is_3d: bool,
    attenuation: u32,
    game_aux: bool,
    user_send_db: Option<f32>,
    /// Output bus and its ancestors: (static dB, RTPC curves) each.
    buses: Arc<Vec<(f32, Vec<crate::hirc::RtpcCurve>)>>,
    sound_volume: f32,
    sound_pitch: f32,
}

struct PendingPass {
    container: u32,
    emitter: u64,
    playing_id: u32,
    loops_left: u16,
    next_frame: u64,
}

#[derive(Default)]
struct Emitter {
    pos: Vec3,
    switches: HashMap<u32, u32>,
}

pub struct Engine {
    pub lib: Arc<AudioLibrary>,
    mixer: Arc<Mutex<Mixer>>,
    play: PlayState,
    listener: Listener,
    player: u64,
    emitters: HashMap<u64, Emitter>,
    global_switches: HashMap<u32, u32>,
    params: HashMap<u32, f32>,
    env: Option<Environment>,
    instances: HashMap<u64, Instance>,
    pending: Vec<PendingPass>,
    pcm: HashMap<u32, Arc<Pcm>>,
    bus_cache: HashMap<u32, Arc<Vec<(f32, Vec<crate::hirc::RtpcCurve>)>>>,
    /// Global game-parameter ramps: id -> (from, to, start frame, frames).
    ramps: HashMap<u32, (f32, f32, u64, u64)>,
    /// Mixer clock at the last update.
    now: u64,
    next_tag: u64,
    next_playing: u32,
    /// Default `sound_volume` for posted sounds (sound/default.decl parms.volume).
    pub default_sound_volume: f32,
}

impl Engine {
    pub fn new(lib: Arc<AudioLibrary>, mixer: Arc<Mutex<Mixer>>, seed: u64) -> Self {
        Self {
            lib,
            mixer,
            play: PlayState::new(seed),
            listener: Listener::default(),
            player: 1,
            emitters: HashMap::new(),
            global_switches: HashMap::new(),
            params: HashMap::new(),
            env: None,
            instances: HashMap::new(),
            pending: Vec::new(),
            pcm: HashMap::new(),
            bus_cache: HashMap::new(),
            ramps: HashMap::new(),
            now: 0,
            next_tag: 1,
            next_playing: 1,
            default_sound_volume: -6.0,
        }
    }

    pub fn mixer(&self) -> Arc<Mutex<Mixer>> {
        self.mixer.clone()
    }

    pub fn set_listener(&mut self, l: Listener) {
        self.listener = l;
    }

    /// The emitter that is the listener's own entity (gets `locality = first_person`).
    pub fn set_player_emitter(&mut self, id: u64) {
        self.player = id;
    }

    pub fn player_emitter(&self) -> u64 {
        self.player
    }

    pub fn set_emitter(&mut self, id: u64, pos: Vec3) {
        self.emitters.entry(id).or_default().pos = pos;
    }

    /// Stops everything the emitter plays and forgets it.
    pub fn remove_emitter(&mut self, id: u64) {
        self.stop_emitter(id, 0);
        self.emitters.remove(&id);
    }

    /// Switch value for one emitter, or for all emitters when `emitter` is None.
    pub fn set_switch(&mut self, emitter: Option<u64>, group: &str, value: &str) {
        let (g, v) = (fnv1_lower(group), fnv1_lower(value));
        match emitter {
            Some(e) => {
                self.emitters.entry(e).or_default().switches.insert(g, v);
            }
            None => {
                self.global_switches.insert(g, v);
            }
        }
    }

    pub fn set_state(&mut self, group: &str, value: &str) {
        self.play.set_state(group, value);
    }

    /// Global game-parameter value (e.g. `player_health`, `chainsaw_rpm`).
    pub fn set_rtpc(&mut self, name: &str, value: f32) {
        let id = fnv1_lower(name);
        self.ramps.remove(&id);
        self.params.insert(id, value);
    }

    /// Global game parameter moved linearly over `ramp_ms` from its current value, as
    /// AK::SoundEngine::SetRTPCValue(id, value, global, ramp, AkCurveInterpolation_Linear) does.
    pub fn set_rtpc_ramped(&mut self, name: &str, value: f32, ramp_ms: u32) {
        let id = fnv1_lower(name);
        let from = self.global_param(id);
        let (now, rate) = {
            let m = self.mixer.lock().unwrap();
            (m.clock(), u64::from(m.rate))
        };
        self.now = now;
        self.ramps.insert(id, (from, value, now, u64::from(ramp_ms) * rate / 1000));
        self.params.insert(id, value);
    }

    pub fn set_environment(&mut self, env: Option<Environment>) {
        if let Some(e) = &env {
            self.mixer.lock().unwrap().set_reverb(e.reverb);
        }
        self.env = env;
    }

    fn pcm(&mut self, media: u32) -> Option<Arc<Pcm>> {
        if let Some(p) = self.pcm.get(&media) {
            return Some(p.clone());
        }
        let d = self.lib.decode_media(media).ok()?;
        let p = Arc::new(Pcm::from(d));
        self.pcm.insert(media, p.clone());
        Some(p)
    }

    /// Posts an event from an emitter. Returns a playing id for [`Engine::stop_playing`].
    pub fn post(&mut self, event_id: u32, emitter: u64) -> Result<u32> {
        let v = self.default_sound_volume;
        self.post_with(event_id, emitter, v, 0.0)
    }

    /// Posts by event name or decl sound reference (normalised like the engine).
    pub fn post_name(&mut self, name: &str, emitter: u64) -> Result<u32> {
        self.post(crate::hash::sound_event_id(name), emitter)
    }

    /// `sound_volume` (dB) and `sound_pitch` (cents) as idSound sets them for this sound.
    pub fn post_with(&mut self, event_id: u32, emitter: u64, sound_volume: f32, sound_pitch: f32) -> Result<u32> {
        let playing_id = self.next_playing;
        self.next_playing += 1;
        self.prepare_switches(emitter);
        let cmds = self.play.post_event(&self.lib, event_id)?;
        let now = self.mixer.lock().unwrap().clock();
        let rate = self.mixer.lock().unwrap().rate;
        for c in cmds {
            match c {
                Command::Play { delay_s, what, .. } => {
                    let start = now + (f64::from(delay_s) * f64::from(rate)) as u64;
                    self.schedule(&what, emitter, playing_id, start, sound_volume, sound_pitch);
                }
                Command::Stop { target, fade_ms, .. } => self.stop_target(target, Some(emitter), fade_ms),
                Command::Pause { target, .. } => self.pause_target(target, Some(emitter), true),
                Command::Resume { target, .. } => self.pause_target(target, Some(emitter), false),
                Command::SetState { .. } | Command::Unsupported { .. } => {}
            }
        }
        Ok(playing_id)
    }

    fn prepare_switches(&mut self, emitter: u64) {
        let mut s = self.global_switches.clone();
        s.insert(fnv1_lower("locality"), fnv1_lower(if emitter == self.player { "first_person" } else { "third_person" }));
        if let Some(env) = &self.env {
            s.insert(fnv1_lower("gun_env"), fnv1_lower(&env.gun_env));
        }
        if let Some(e) = self.emitters.get(&emitter) {
            s.extend(e.switches.iter().map(|(k, v)| (*k, *v)));
        }
        self.play.switches = s;
    }

    /// Starts a playable at `start` (mixer frame); returns when it ends (u64::MAX if it never does).
    fn schedule(&mut self, p: &Playable, emitter: u64, playing_id: u32, start: u64, sv: f32, sp: f32) -> u64 {
        let rate = f64::from(self.mixer.lock().unwrap().rate);
        match p {
            Playable::Voice(v) => {
                let delay = (f64::from(v.delay_s.max(0.0)) * rate) as u64;
                if let Some(s) = v.silence_s {
                    return start + delay + (f64::from(s) * rate) as u64;
                }
                let Some(pcm) = self.pcm(v.media) else { return start + delay };
                let tag = self.next_tag;
                self.next_tag += 1;
                let (is_3d, attenuation, _) = self.lib.positioning(v.sound);
                let (game_aux, user) = self.lib.aux_sends(v.sound);
                let user_send_db = if user.is_empty() {
                    None
                } else {
                    Some(10.0 * user.iter().map(|(_, db)| db_to_gain(*db).powi(2)).sum::<f32>().max(1e-20).log10())
                };
                let buses = self.bus_levels(v.output_bus);
                let inst = Instance {
                    playing_id,
                    emitter,
                    voice: v.clone(),
                    pcm: pcm.clone(),
                    is_3d,
                    attenuation,
                    game_aux,
                    user_send_db,
                    buses,
                    sound_volume: sv,
                    sound_pitch: sp,
                };
                let params = self.params_for(&inst);
                let pitch_ratio = 2f32.powf(self.pitch_cents(&inst) / 1200.0);
                self.instances.insert(tag, inst);
                self.mixer.lock().unwrap().start(VoiceStart { tag, pcm: pcm.clone(), start_frame: start + delay, loops: v.loop_count, params });
                if v.loop_count == 0 {
                    u64::MAX
                } else {
                    let frames = pcm.frames() as f64 * f64::from(v.loop_count) / f64::from(pcm.rate) * rate / f64::from(pitch_ratio);
                    start + delay + frames as u64
                }
            }
            Playable::Together(items) => items.iter().map(|i| self.schedule(i, emitter, playing_id, start, sv, sp)).max().unwrap_or(start),
            Playable::Sequence { container, loops, transition_mode, transition_s, items } => {
                let mut t = start;
                for i in items {
                    let end = self.schedule(i, emitter, playing_id, t, sv, sp);
                    if end == u64::MAX {
                        return u64::MAX;
                    }
                    // Transition mode 3 = delay between items.
                    t = end + if *transition_mode == 3 { (f64::from(*transition_s) * rate) as u64 } else { 0 };
                }
                if *loops != 1 {
                    self.pending.push(PendingPass {
                        container: *container,
                        emitter,
                        playing_id,
                        loops_left: if *loops == 0 { 0 } else { loops - 1 },
                        next_frame: t,
                    });
                    u64::MAX
                } else {
                    t
                }
            }
            Playable::Unsupported { .. } | Playable::Nothing => start,
        }
    }

    fn param(&self, inst: &Instance, id: u32) -> f32 {
        if id == fnv1_lower("sound_volume") {
            return inst.sound_volume;
        }
        if id == fnv1_lower("sound_pitch") {
            return inst.sound_pitch;
        }
        self.global_param(id)
    }

    /// Current value of a global game parameter: an in-progress ramp, the last value set, or the
    /// Init.bnk default.
    pub fn global_param(&self, id: u32) -> f32 {
        if let Some(&(from, to, start, dur)) = self.ramps.get(&id) {
            if self.now < start + dur {
                return from + (to - from) * (self.now.saturating_sub(start) as f32 / dur.max(1) as f32);
            }
        }
        self.params.get(&id).copied().unwrap_or_else(|| self.lib.game_param_default(id))
    }

    fn bus_levels(&mut self, bus: u32) -> Arc<Vec<(f32, Vec<crate::hirc::RtpcCurve>)>> {
        if let Some(b) = self.bus_cache.get(&bus) {
            return b.clone();
        }
        let levels = Arc::new(self.lib.bus_chain(bus).into_iter().map(|(id, db)| (db, self.lib.bus_rtpcs(id))).collect::<Vec<_>>());
        self.bus_cache.insert(bus, levels.clone());
        levels
    }

    /// Gain in dB of a bus and its ancestors right now (static props plus the bus RTPCs, e.g. the
    /// volume cvars' master_volume / sfx_volume / music_volume / vo_volume), as voices on it get it.
    pub fn bus_volume_db(&mut self, bus_name: &str) -> f32 {
        let levels = self.bus_levels(fnv1_lower(bus_name));
        self.levels_db(&levels)
    }

    /// Bus gain in dB: static bus props plus bus RTPCs on Volume (0) / BusVolume (5).
    fn bus_db(&self, inst: &Instance) -> f32 {
        self.levels_db(&inst.buses)
    }

    fn levels_db(&self, levels: &[(f32, Vec<crate::hirc::RtpcCurve>)]) -> f32 {
        levels
            .iter()
            .map(|(db, rtpcs)| {
                db + rtpcs
                    .iter()
                    .filter(|r| r.param == prop::VOLUME || r.param == prop::BUS_VOLUME)
                    .map(|r| r.curve().eval(self.global_param(r.rtpc)))
                    .sum::<f32>()
            })
            .sum()
    }

    /// Sum of RTPC curve outputs for one parameter (0 volume dB, 2 pitch cents, 3 LPF, 4 HPF) over the
    /// sound and its ancestors.
    fn rtpc_sum(&self, inst: &Instance, param: u8) -> f32 {
        let mut sum = 0.0;
        for n in &inst.voice.nodes {
            let Some(base) = self.lib.node(*n).and_then(|o| o.node()) else { continue };
            for r in base.rtpcs.iter().filter(|r| r.param == param && r.rtpc_type == 0) {
                sum += r.curve().eval(self.param(inst, r.rtpc));
            }
        }
        sum
    }

    fn pitch_cents(&self, inst: &Instance) -> f32 {
        inst.voice.pitch_cents + self.rtpc_sum(inst, prop::PITCH)
    }

    fn params_for(&self, inst: &Instance) -> VoiceParams {
        let v = &inst.voice;
        // Voice volume shared by the dry and aux paths: props, buses, RTPCs.
        let base_db = v.volume_db + self.bus_db(inst) + self.rtpc_sum(inst, prop::VOLUME);
        let mut dry_db = base_db;
        let mut lpf = v.lpf + self.rtpc_sum(inst, prop::LPF);
        let mut hpf = v.hpf + self.rtpc_sum(inst, prop::HPF);
        let home = downmix_gains(inst.pcm.channels, inst.pcm.channel_mask);
        let mut gains = home;
        let (mut game_aux_db, mut user_aux_db) = (0.0f32, 0.0f32);
        if inst.is_3d {
            let epos = self.emitters.get(&inst.emitter).map_or(self.listener.pos, |e| e.pos);
            let rel = sub(epos, self.listener.pos);
            let dist = dot(rel, rel).sqrt();
            let mut spread = 0.0;
            if let Some(a) = self.lib.attenuation(inst.attenuation) {
                let at = |slot| a.curve(slot).map(|c| c.eval(dist));
                dry_db += at(atten_slot::VOLUME_DRY).unwrap_or(0.0);
                game_aux_db = at(atten_slot::VOLUME_GAME_AUX).unwrap_or(0.0);
                user_aux_db = at(atten_slot::VOLUME_USER_AUX).unwrap_or(0.0);
                lpf += at(atten_slot::LPF).unwrap_or(0.0);
                hpf += at(atten_slot::HPF).unwrap_or(0.0);
                spread = (at(atten_slot::SPREAD).unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
            }
            // Constant-power pan from the azimuth in the listener's horizontal plane (rear folds onto
            // the sides); spread blends toward the unpositioned stereo image.
            let right = cross(self.listener.forward, self.listener.up);
            let (x, z) = (dot(rel, right), dot(rel, self.listener.forward));
            let pan = if dist > 1e-3 { (x / (x * x + z * z).sqrt().max(1e-6)).clamp(-1.0, 1.0) } else { 0.0 };
            let ang = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            let (pl, pr) = (ang.cos(), ang.sin());
            let h = std::f32::consts::FRAC_1_SQRT_2;
            for (c, g) in gains.iter_mut().enumerate().take(inst.pcm.channels as usize) {
                let weight = if home[c] == [0.0, 0.0] { 0.0 } else if home[c][0] > 0.0 && home[c][1] > 0.0 { 1.0 } else { h };
                *g = [(1.0 - spread) * weight * pl + spread * home[c][0], (1.0 - spread) * weight * pr + spread * home[c][1]];
            }
        }
        // Aux sends: game-defined (environment) and user sends, power-summed into one reverb.
        let mut send_pow = 0.0f32;
        if let Some(e) = &self.env {
            dry_db += 20.0 * e.dry_gain.max(1e-10).log10();
            if inst.game_aux && !e.aux_bus.is_empty() {
                send_pow += db_to_gain(base_db + game_aux_db).powi(2) * e.aux_send_level * e.aux_send_level;
            }
        }
        if let Some(u) = inst.user_send_db {
            send_pow += db_to_gain(base_db + u + user_aux_db).powi(2);
        }
        let gain = db_to_gain(dry_db.min(24.0));
        for g in gains.iter_mut() {
            g[0] *= gain;
            g[1] *= gain;
        }
        VoiceParams {
            gains,
            send: send_pow.sqrt(),
            pitch: 2f32.powf(self.pitch_cents(inst) / 1200.0),
            lpf_hz: lpf_cutoff(lpf.clamp(0.0, 100.0)),
            hpf_hz: hpf_cutoff(hpf.clamp(0.0, 100.0)),
        }
    }

    fn stop_target(&mut self, target: u32, emitter: Option<u64>, fade_ms: u32) {
        let rate = self.mixer.lock().unwrap().rate;
        let fade = (u64::from(fade_ms) * u64::from(rate) / 1000) as u32;
        let tags: Vec<u64> = self
            .instances
            .iter()
            .filter(|(_, i)| i.voice.nodes.contains(&target) && emitter.is_none_or(|e| e == i.emitter))
            .map(|(t, _)| *t)
            .collect();
        let mut m = self.mixer.lock().unwrap();
        for t in tags {
            m.stop(t, fade);
        }
        drop(m);
        self.pending.retain(|p| !(p.container == target && emitter.is_none_or(|e| e == p.emitter)));
    }

    fn pause_target(&mut self, target: u32, emitter: Option<u64>, paused: bool) {
        let mut m = self.mixer.lock().unwrap();
        for (t, i) in &self.instances {
            if i.voice.nodes.contains(&target) && emitter.is_none_or(|e| e == i.emitter) {
                m.pause(*t, paused);
            }
        }
    }

    pub fn stop_playing(&mut self, playing_id: u32, fade_ms: u32) {
        let rate = self.mixer.lock().unwrap().rate;
        let fade = (u64::from(fade_ms) * u64::from(rate) / 1000) as u32;
        let mut m = self.mixer.lock().unwrap();
        for (t, i) in &self.instances {
            if i.playing_id == playing_id {
                m.stop(*t, fade);
            }
        }
        drop(m);
        self.pending.retain(|p| p.playing_id != playing_id);
    }

    pub fn stop_emitter(&mut self, emitter: u64, fade_ms: u32) {
        let rate = self.mixer.lock().unwrap().rate;
        let fade = (u64::from(fade_ms) * u64::from(rate) / 1000) as u32;
        let mut m = self.mixer.lock().unwrap();
        for (t, i) in &self.instances {
            if i.emitter == emitter {
                m.stop(*t, fade);
            }
        }
        drop(m);
        self.pending.retain(|p| p.emitter != emitter);
    }

    pub fn stop_all(&mut self, fade_ms: u32) {
        let emitters: Vec<u64> = self.instances.values().map(|i| i.emitter).collect();
        for e in emitters {
            self.stop_emitter(e, fade_ms);
        }
    }

    pub fn is_playing(&self, playing_id: u32) -> bool {
        self.instances.values().any(|i| i.playing_id == playing_id) || self.pending.iter().any(|p| p.playing_id == playing_id)
    }

    pub fn voice_count(&self) -> usize {
        self.instances.len()
    }

    /// Per frame: refresh 3D / RTPC parameters, forget finished voices, continue looping sequences.
    pub fn update(&mut self) {
        let finished = {
            let mut m = self.mixer.lock().unwrap();
            self.now = m.clock();
            m.drain_finished()
        };
        for t in finished {
            self.instances.remove(&t);
        }
        let params: Vec<(u64, VoiceParams)> = self.instances.iter().map(|(t, i)| (*t, self.params_for(i))).collect();
        {
            let mut m = self.mixer.lock().unwrap();
            for (t, p) in params {
                m.set_params(t, p);
            }
        }
        let (now, rate) = {
            let m = self.mixer.lock().unwrap();
            (m.clock(), u64::from(m.rate))
        };
        let due: Vec<PendingPass> = {
            let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|p| p.next_frame <= now + rate / 2);
            self.pending = keep;
            due
        };
        for p in due {
            self.prepare_switches(p.emitter);
            let pass = self.play.next_pass(&self.lib, p.container);
            if let Playable::Sequence { items, transition_mode, transition_s, .. } = &pass {
                let mut t = p.next_frame;
                let mut endless = false;
                for i in items {
                    let end = self.schedule(i, p.emitter, p.playing_id, t, self.default_sound_volume, 0.0);
                    if end == u64::MAX {
                        endless = true;
                        break;
                    }
                    t = end + if *transition_mode == 3 { (f64::from(*transition_s) * rate as f64) as u64 } else { 0 };
                }
                if !endless && p.loops_left != 1 {
                    self.pending.push(PendingPass { loops_left: if p.loops_left == 0 { 0 } else { p.loops_left - 1 }, next_frame: t, ..p });
                }
            }
        }
    }
}
