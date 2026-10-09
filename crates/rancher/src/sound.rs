//! DOOM (2016) sound in the testbed: idaudio's event engine and software mixer behind a Bevy plugin.
//!
//! Everything is read from the user's install (Wwise banks via `idaudio`, sound references via decls).
//! Positions are game units in DOOM / rancher_sim axes (x forward at yaw 0, z up); the listener follows
//! `crate::Sim`'s player eye and view angles automatically.
//!
//! Output: one custom Bevy audio source pulls the mix from the shared [`idaudio::Mixer`]. Env vars:
//! - `RANCHER_SOUND=0` — no audio output (the engine still runs).
//! - `RANCHER_SOUND_WAV=<path>` — no audio output; the mix is rendered with the frame clock and written
//!   to `<path>` (stereo 48 kHz WAV) when the app exits. Use this for automated checks.
//! - Background self-tests (`RANCHER_SHOT` / `RANCHER_SHOTS`) never open a sound output.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::audio::{AddAudioSource, AudioPlayer, ChannelCount, Decodable, PlaybackSettings, SampleRate, Source};
use bevy::prelude::*;
use idaudio::{AudioLibrary, Engine, Environment, Listener, Mixer};
use idres::decl::{Block, Value};
use idres::decldb::DeclDb;
use idres::md6def::AnimEvent;
use rancher_sim::weapons::WeaponDef;

use crate::animweb::Slot;
use crate::viewanim::HandsEvents;

/// Mixer output rate. Bevy/rodio resample to the device rate.
pub const RATE: u32 = 48000;
/// Frames rendered per mixer lock on the audio thread (10 ms).
const BLOCK: usize = 480;

pub struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        let mode = OutputMode::from_env();
        let sound = Sound::open(mode.clone());
        let mixer = sound.engine.as_ref().map(|e| e.mixer());
        app.insert_resource(sound);
        match (mode, mixer) {
            (OutputMode::Device, Some(m)) if app.is_plugin_added::<bevy::audio::AudioPlugin>() => {
                app.add_audio_source::<MixStream>().insert_resource(PendingStream(Some(m))).add_systems(bevy::app::Startup, start_stream);
            }
            (OutputMode::Capture(path), Some(m)) => {
                app.insert_resource(Capture { mixer: m, path, pcm: Vec::new(), owed: 0.0 }).add_systems(Last, capture);
            }
            _ => {}
        }
        // PostUpdate runs after viewanim::animate (Update), so HandsEvents holds this frame's events.
        app.add_systems(PostUpdate, (anim_events, bob_footsteps, update).chain().after(bevy::transform::TransformSystems::Propagate));
    }
}

#[derive(Clone, PartialEq)]
enum OutputMode {
    Device,
    Capture(std::path::PathBuf),
    Silent,
}

impl OutputMode {
    fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if let Some(p) = get("RANCHER_SOUND_WAV") {
            return OutputMode::Capture(p.into());
        }
        if get("RANCHER_SOUND").as_deref() == Some("0") || get("RANCHER_SHOT").is_some() || get("RANCHER_SHOTS").is_some() {
            return OutputMode::Silent;
        }
        OutputMode::Device
    }
}

/// The audio source Bevy plays: an endless stream rendered by the shared mixer.
#[derive(Asset, TypePath)]
pub struct MixStream {
    mixer: Arc<Mutex<Mixer>>,
}

pub struct MixDecoder {
    mixer: Arc<Mutex<Mixer>>,
    buf: Vec<f32>,
    pos: usize,
}

impl Iterator for MixDecoder {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() {
            if let Ok(mut m) = self.mixer.lock() {
                m.render(&mut self.buf);
            } else {
                self.buf.fill(0.0);
            }
            self.pos = 0;
        }
        let s = self.buf[self.pos];
        self.pos += 1;
        Some(s.clamp(-1.0, 1.0))
    }
}

impl Source for MixDecoder {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> ChannelCount {
        ChannelCount::new(2).unwrap()
    }
    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(RATE).unwrap()
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

impl Decodable for MixStream {
    type Decoder = MixDecoder;
    fn decoder(&self) -> MixDecoder {
        MixDecoder { mixer: self.mixer.clone(), buf: vec![0.0; BLOCK * 2], pos: BLOCK * 2 }
    }
}

#[derive(Resource)]
struct PendingStream(Option<Arc<Mutex<Mixer>>>);

fn start_stream(mut commands: Commands, mut pending: ResMut<PendingStream>, mut streams: ResMut<Assets<MixStream>>) {
    if let Some(mixer) = pending.0.take() {
        let handle = streams.add(MixStream { mixer });
        commands.spawn((AudioPlayer::<MixStream>(handle), PlaybackSettings::ONCE));
    }
}

/// Offline capture (`RANCHER_SOUND_WAV`): renders as many frames as real time advanced, written on exit.
#[derive(Resource)]
struct Capture {
    mixer: Arc<Mutex<Mixer>>,
    path: std::path::PathBuf,
    pcm: Vec<f32>,
    owed: f64,
}

fn capture(mut cap: ResMut<Capture>, time: Res<Time>) {
    cap.owed += f64::from(time.delta_secs()) * f64::from(RATE);
    let frames = cap.owed.floor() as usize;
    if frames == 0 {
        return;
    }
    cap.owed -= frames as f64;
    let mut buf = vec![0.0f32; frames * 2];
    cap.mixer.lock().unwrap().render(&mut buf);
    cap.pcm.extend_from_slice(&buf);
}

impl Drop for Capture {
    fn drop(&mut self) {
        let samples: Vec<i16> = self.pcm.iter().map(|x| (x.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
        match idaudio::wav::write(&self.path, RATE, 2, 3, &samples) {
            Ok(()) => eprintln!("sound: wrote {:.1}s of mix to {}", self.pcm.len() as f32 / 2.0 / RATE as f32, self.path.display()),
            Err(e) => eprintln!("sound: writing {}: {e:#}", self.path.display()),
        }
    }
}

/// How the player is moving when a footstep fires (selects the footstep effect table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gait {
    /// walkState RUNNING / SPRINTING: `footstepEffectTable_Sprint`.
    Sprint,
    /// walkState WALKING: `footstepEffectTable_SlowWalk`.
    SlowWalk,
    /// walkState WALKING while ducked: `footstepEffectTable_CrouchWalk`.
    Crouch,
}

/// The footstep effect tables tried in order (idActor 0x14074b0a0, the non-friendly branch: the player decl does not
/// set useFriendlyFootsteps): CrouchWalk -> SlowWalk -> footstepEffectTable, SlowWalk -> footstepEffectTable,
/// Sprint -> footstepEffectTable.
fn footstep_tables(gait: Gait) -> &'static [&'static str] {
    match gait {
        Gait::Crouch => &["footstepEffectTable_CrouchWalk", "footstepEffectTable_SlowWalk", "footstepEffectTable"],
        Gait::SlowWalk => &["footstepEffectTable_SlowWalk", "footstepEffectTable"],
        Gait::Sprint => &["footstepEffectTable_Sprint", "footstepEffectTable"],
    }
}

/// Which `actorSounds.sndPain*` to play; the damage thresholds are chosen by game code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PainSize {
    Small,
    Medium,
    Large,
}

/// Fire-sound keys of a projectile decl (idWeapon::PlayFireSound reads loopingFireSound, else fireSound).
#[derive(Debug, Clone, Default)]
struct FireSounds {
    fire: Option<String>,
    looping: Option<String>,
    stop: Option<String>,
    looping_unmanaged: Option<String>,
    looping_unmanaged_stop: Option<String>,
}

/// Game-facing sound API (a Bevy resource). Every call is a no-op if the sound data failed to load.
#[derive(Resource)]
pub struct Sound {
    engine: Option<Engine>,
    db: Option<DeclDb>,
    /// Loops started by `ae_soundBodyLoopUntilStopped`, per anim slot.
    anim_loops: HashMap<Slot, Vec<u32>>,
    fire: HashMap<String, FireSounds>,
    looping_fire: HashMap<String, Vec<u32>>,
    /// footstep / landing table decl → surface effect key (e.g. `concreteEffect`) → sound.
    tables: HashMap<String, HashMap<String, String>>,
    player_keys: HashMap<String, String>,
    entities: HashMap<Entity, u64>,
    next_emitter: u64,
    tracked: Vec<(Entity, u64)>,
    master_db: f32,
    muted: bool,
}

const PLAYER_EMITTER: u64 = 1;

/// Volume cvar → the global RTPC the engine sets from it (registration 0x141a11d20).
pub const VOLUME_CVARS: &[(&str, &str)] = &[
    ("s_volume_dB", "master_volume"),
    ("s_volume_music", "music_volume"),
    ("s_volume_sfx", "sfx_volume"),
    ("s_volume_vo", "vo_volume"),
    ("s_volume_mpvo", "vo_volume_mp"),
];

impl Sound {
    /// The engine without any audio output (tests and offline checks).
    pub fn headless() -> Self {
        Self::open(OutputMode::Silent)
    }

    pub fn engine_mut(&mut self) -> Option<&mut Engine> {
        self.engine.as_mut()
    }

    fn open(mode: OutputMode) -> Self {
        let mut s = Sound {
            engine: None,
            db: None,
            anim_loops: HashMap::new(),
            fire: HashMap::new(),
            looping_fire: HashMap::new(),
            tables: HashMap::new(),
            player_keys: HashMap::new(),
            entities: HashMap::new(),
            next_emitter: 100,
            tracked: Vec::new(),
            master_db: 0.0,
            muted: false,
        };
        let Some(doom) = idres::find_install() else {
            eprintln!("sound: DOOM install not found; sound disabled");
            return s;
        };
        match AudioLibrary::open(&doom) {
            Ok(lib) => {
                let mixer = Arc::new(Mutex::new(Mixer::new(RATE)));
                let mut eng = Engine::new(Arc::new(lib), mixer, 0x5eed_d00d);
                eng.set_player_emitter(PLAYER_EMITTER);
                s.engine = Some(eng);
            }
            Err(e) => eprintln!("sound: {e:#}; sound disabled"),
        }
        match idres::Container::open(&doom.join("base"), "gameresources") {
            Ok(c) => s.db = Some(DeclDb::new(Arc::new(c))),
            Err(e) => eprintln!("sound: decls unavailable: {e:#}"),
        }
        s.load_player_keys();
        if mode == OutputMode::Silent {
            eprintln!("sound: output disabled (RANCHER_SOUND=0 or self-test)");
        }
        s
    }

    fn load_player_keys(&mut self) {
        let Some(db) = &self.db else { return };
        let mut keys = HashMap::new();
        // playerProps sounds (sndJump, sndCrouch, sndLegsCrossing, ...) and the player entityDef
        // (actorSounds.sndPain*, doubleJumpSound, footstep tables).
        if let Ok(b) = db.get("playerprops", "player/default") {
            collect_strings(&b, "", &mut keys);
        }
        if let Ok(b) = db.get("entitydef", "player") {
            collect_strings(&b, "", &mut keys);
        }
        for table in ["footstepEffectTable", "footstepEffectTable_Sprint", "footstepEffectTable_SlowWalk", "footstepEffectTable_CrouchWalk", "footstepEffectTable_Landing", "footstepEffectTable_HeavyLanding"] {
            // The tables sit inside the entityDef's actor block, so match on the last path component.
            let Some(name) = key_lookup(&keys, table) else { continue };
            if let Ok(t) = db.get("projectileimpacteffect", &name) {
                let mut m = HashMap::new();
                if let Some(edit) = t.block("edit") {
                    for (k, v) in &edit.items {
                        if let Some(snd) = v.as_block().and_then(|b| b.str("sndImpact")) {
                            m.insert(k.clone(), snd.to_owned());
                        }
                    }
                }
                self.tables.insert(table.to_owned(), m);
            }
        }
        self.player_keys = keys;
    }

    fn player_key(&self, key: &str) -> Option<String> {
        key_lookup(&self.player_keys, key)
    }

    fn emitter_for(&mut self, entity: Option<Entity>, position: Option<rancher_sim::Vec3>) -> u64 {
        let id = match entity {
            Some(e) => *self.entities.entry(e).or_insert_with(|| {
                self.next_emitter += 1;
                self.tracked.push((e, self.next_emitter));
                self.next_emitter
            }),
            None if position.is_some() => {
                self.next_emitter += 1;
                self.next_emitter
            }
            None => PLAYER_EMITTER,
        };
        if let (Some(p), Some(eng)) = (position, self.engine.as_mut()) {
            eng.set_emitter(id, [p.x, p.y, p.z]);
        }
        id
    }

    /// Posts a sound by Wwise event name or any decl sound reference (`play_wpn_sp_shotgun_fire`,
    /// `player/pain/small`, ...). `emitter`/`position` None = the player (first person, 2D where the
    /// sound is authored so); `position` is in game units. Returns a playing id (0 if nothing played).
    pub fn post(&mut self, name: &str, emitter: Option<Entity>, position: Option<rancher_sim::Vec3>) -> u32 {
        self.post_id(idaudio::sound_event_id(name), emitter, position)
    }

    pub fn post_id(&mut self, event_id: u32, emitter: Option<Entity>, position: Option<rancher_sim::Vec3>) -> u32 {
        let em = self.emitter_for(emitter, position);
        let Some(eng) = self.engine.as_mut() else { return 0 };
        match eng.post(event_id, em) {
            Ok(id) => id,
            Err(e) => {
                warn!("sound: {e:#}");
                0
            }
        }
    }

    pub fn stop(&mut self, playing_id: u32, fade_ms: u32) {
        if let Some(eng) = self.engine.as_mut() {
            eng.stop_playing(playing_id, fade_ms);
        }
    }

    /// Stops everything an entity (or, with None, the player) is playing.
    pub fn stop_emitter(&mut self, emitter: Option<Entity>, fade_ms: u32) {
        let id = match emitter {
            Some(e) => match self.entities.get(&e) {
                Some(id) => *id,
                None => return,
            },
            None => PLAYER_EMITTER,
        };
        if let Some(eng) = self.engine.as_mut() {
            eng.stop_emitter(id, fade_ms);
        }
    }

    /// Global switch value, e.g. `set_switch("gun_env", "int_large")`. `locality` is set automatically.
    pub fn set_switch(&mut self, group: &str, value: &str) {
        if let Some(eng) = self.engine.as_mut() {
            eng.set_switch(None, group, value);
        }
    }

    /// Switch value for one entity's sounds (Wwise switches are per game object).
    pub fn set_switch_on(&mut self, emitter: Entity, group: &str, value: &str) {
        let id = self.emitter_for(Some(emitter), None);
        if let Some(eng) = self.engine.as_mut() {
            eng.set_switch(Some(id), group, value);
        }
    }

    /// Game parameter, e.g. `set_rtpc("player_health", 100.0)`, `set_rtpc("chainsaw_rpm", ..)`.
    pub fn set_rtpc(&mut self, name: &str, value: f32) {
        if let Some(eng) = self.engine.as_mut() {
            eng.set_rtpc(name, value);
        }
    }

    pub fn set_state(&mut self, group: &str, value: &str) {
        if let Some(eng) = self.engine.as_mut() {
            eng.set_state(group, value);
        }
    }

    /// Applies the game's volume cvars (all dB, exe default 0) the way Wwise_SoundHardware does
    /// (0x141a13890): each sets a global RTPC with a 500 ms linear ramp — `s_volume_dB` → master_volume,
    /// `s_volume_sfx` → sfx_volume, `s_volume_music` → music_volume, `s_volume_vo` → vo_volume,
    /// `s_volume_mpvo` → vo_volume_mp. Init.bnk binds those to BusVolume on Master Audio Bus /
    /// Environmental, UI, Arcade, SFX_Player_Glorykill / Music, music_stinger / VO, SFX_Story_VO /
    /// SFX_MP_VO with the curve -60 dB → silent (Exp3), 0 → unity, flat above 0. `s_volume_ambient` is
    /// registered and shown by the menu, but no sound code applies it, so it has no effect here either.
    /// Missing cvars are left unchanged.
    pub fn set_volume_cvars(&mut self, cvars: &rancher_sim::config::CvarValues) {
        for (cvar, _) in VOLUME_CVARS {
            if let Some(v) = cvars.0.get(*cvar).and_then(|v| v.trim().trim_end_matches('f').parse::<f32>().ok()) {
                self.set_volume_cvar(cvar, v);
            }
        }
    }

    /// One volume cvar by name (see [`Sound::set_volume_cvars`]); unknown names are ignored.
    pub fn set_volume_cvar(&mut self, cvar: &str, db: f32) {
        let Some((_, rtpc)) = VOLUME_CVARS.iter().find(|(c, _)| c.eq_ignore_ascii_case(cvar)) else { return };
        if *rtpc == "master_volume" {
            self.master_db = db;
            if self.muted {
                return;
            }
        }
        if let Some(eng) = self.engine.as_mut() {
            eng.set_rtpc_ramped(rtpc, db, 500);
        }
    }

    /// Mute: the engine drives master_volume to -60 instead of s_volume_dB while muted (0x141a13890).
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        let db = if muted { -60.0 } else { self.master_db };
        if let Some(eng) = self.engine.as_mut() {
            eng.set_rtpc_ramped("master_volume", db, 500);
        }
    }

    /// The listener's sound environment, as an `idSoundEnvironment` entity defines it: aux bus name
    /// (e.g. "sp_foundry_mainroom"), gunEnvironment switch value (e.g. "int_med"), auxSendLevel and
    /// dryGainLevel (linear). None = no environment (dry; gun_env falls back to int_med).
    pub fn set_environment(&mut self, aux_bus: Option<&str>, gun_env: &str, aux_send_level: f32, dry_gain: f32) {
        if let Some(eng) = self.engine.as_mut() {
            let env = aux_bus.map(|b| Environment::for_bus(&eng.lib, b, gun_env, aux_send_level, dry_gain));
            eng.set_environment(env);
        }
    }

    // ---- animation events -------------------------------------------------------------------------

    /// Plays one first-person anim frame event (`ae_sound`, `ae_soundWeapon`, `ae_soundBody`, ...) that
    /// carries a `sound` parameter. `ae_soundBodyLoopUntilStopped` keeps its playing id per slot until an
    /// `ae_soundBodyLoopStop` on the same slot. Events without a sound are ignored. Called by the plugin
    /// for every entry of `viewanim::HandsEvents`; public for other animators.
    pub fn anim_event(&mut self, slot: Slot, event: &AnimEvent) {
        if event.name.ends_with("LoopStop") {
            for id in self.anim_loops.remove(&slot).unwrap_or_default() {
                self.stop(id, 0);
            }
        }
        let Some(snd) = event.param("sound").and_then(|a| a.text()) else { return };
        if snd.is_empty() || snd == "NULL" {
            return;
        }
        let id = self.post(snd, None, None);
        if id != 0 && event.name.ends_with("LoopUntilStopped") {
            self.anim_loops.entry(slot).or_default().push(id);
        }
    }

    // ---- gameplay hooks ---------------------------------------------------------------------------

    fn fire_sounds(&mut self, def: &WeaponDef) -> FireSounds {
        let key = def.projectile.name.clone();
        if let Some(f) = self.fire.get(&key) {
            return f.clone();
        }
        let mut f = FireSounds { fire: Some(def.projectile.fire_sound.clone()).filter(|s| !s.is_empty() && s != "NULL"), ..Default::default() };
        if let Some(Ok(b)) = self.db.as_ref().map(|db| db.get("projectile", &key)) {
            let get = |k: &str| b.str(&format!("edit.{k}")).map(str::to_owned).filter(|s| !s.is_empty() && s != "NULL");
            f.looping = get("loopingFireSound");
            f.stop = get("stopFireSound");
            f.looping_unmanaged = get("loopingFireSoundUnmanaged");
            f.looping_unmanaged_stop = get("loopingFireSoundUnmanagedStopSound");
            if f.fire.is_none() {
                f.fire = get("fireSound");
            }
        }
        self.fire.insert(key, f.clone());
        f
    }

    /// `WeaponEvent::Fired`: idWeapon::PlayFireSound — the projectile's loopingFireSound (started once
    /// and kept while firing) or its fireSound, first person, with the current gun_env.
    pub fn weapon_fired(&mut self, def: &WeaponDef) {
        let f = self.fire_sounds(def);
        let key = def.projectile.name.clone();
        if f.looping.is_some() || f.looping_unmanaged.is_some() {
            if !self.looping_fire.contains_key(&key) {
                let mut ids = Vec::new();
                for s in [&f.looping, &f.looping_unmanaged].into_iter().flatten() {
                    ids.push(self.post(s, None, None));
                }
                self.looping_fire.insert(key, ids);
            }
        } else if let Some(s) = &f.fire {
            self.post(s, None, None);
        }
    }

    /// Trigger released / weapon stopped firing: posts stopFireSound (and the unmanaged loop's stop)
    /// for weapons with looping fire sounds; no-op otherwise.
    pub fn weapon_stopped_firing(&mut self, def: &WeaponDef) {
        let key = def.projectile.name.clone();
        let Some(ids) = self.looping_fire.remove(&key) else { return };
        let f = self.fire_sounds(def);
        for s in [&f.stop, &f.looping_unmanaged_stop].into_iter().flatten() {
            self.post(s, None, None);
        }
        // idWeapon::StopFireSound can force-stop instead of relying on the stop event.
        for id in ids {
            self.stop(id, 0);
        }
    }

    /// A player footstep on a surface (material surface type, e.g. "concrete", "metal", "wood", "dirt";
    /// unknown → the table's defaultEffect).
    pub fn footstep(&mut self, gait: Gait, surface: &str) {
        self.surface_sound(footstep_tables(gait), surface);
    }

    /// Landing after a fall: `footstepEffectTable_Landing`, or `_HeavyLanding` for hard landings (0x14074b0a0 types
    /// 4 / 8: HeavyLanding -> Landing -> footstepEffectTable).
    pub fn landing(&mut self, heavy: bool, surface: &str) {
        let chain: &[&str] = if heavy { &["footstepEffectTable_HeavyLanding", "footstepEffectTable_Landing", "footstepEffectTable"] } else { &["footstepEffectTable_Landing", "footstepEffectTable"] };
        self.surface_sound(chain, surface);
    }

    fn surface_sound(&mut self, tables: &[&str], surface: &str) {
        let Some(t) = tables.iter().find_map(|n| self.tables.get(*n)) else { return };
        let key = format!("{}Effect", surface.trim_start_matches("SURFTYPE_").to_lowercase());
        let snd = t
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&key))
            .or_else(|| t.iter().find(|(k, _)| k.as_str() == "defaultEffect"))
            .map(|(_, v)| v.clone());
        if let Some(s) = snd {
            self.post(&s, None, None);
        }
    }

    /// Jump take-off: playerProps `sounds.sndJump` (player/foley/foley_jump → Play_foley_jump).
    pub fn jump(&mut self) {
        if let Some(s) = self.player_key("sndJump") {
            self.post(&s, None, None);
        }
    }

    /// Double jump: player entityDef `doubleJumpSound` (player/special/player_suitjet_on).
    pub fn double_jump(&mut self) {
        if let Some(s) = self.player_key("doubleJumpSound") {
            self.post(&s, None, None);
        }
    }

    /// Player pain: entityDef `actorSounds.sndPainSmall/Medium/Large`.
    pub fn pain(&mut self, size: PainSize) {
        let key = match size {
            PainSize::Small => "sndPainSmall",
            PainSize::Medium => "sndPainMedium",
            PainSize::Large => "sndPainLarge",
        };
        if let Some(s) = self.player_key(key) {
            self.post(&s, None, None);
        }
    }

    /// Crouch / stand foley (playerProps sndCrouch / sndStandUp).
    pub fn crouch(&mut self, down: bool) {
        if let Some(s) = self.player_key(if down { "sndCrouch" } else { "sndStandUp" }) {
            self.post(&s, None, None);
        }
    }

    /// Any playerProps / player entityDef sound key by name (e.g. "sndLowHealth", "waterBoostSound").
    pub fn player_sound(&mut self, key: &str) {
        if let Some(s) = self.player_key(key) {
            self.post(&s, None, None);
        }
    }

    /// Voices currently mixing (for HUD/debug).
    pub fn voice_count(&self) -> usize {
        self.engine.as_ref().map_or(0, |e| e.voice_count())
    }
}

/// Value of a collected decl key, matched on its last path component (`sndJump` finds `sounds.sndJump`).
fn key_lookup(keys: &HashMap<String, String>, key: &str) -> Option<String> {
    let dotted = format!(".{key}");
    keys.iter()
        .find(|(k, _)| k.as_str() == key || k.ends_with(&dotted))
        .map(|(_, v)| v.clone())
        .filter(|v| v != "NULL" && !v.is_empty())
}

/// Walking footsteps timed by the weapon bob: when `hands_layers.step` is set this frame (the bob's
/// idPlayer vslot 0xc08 call, 0x140e30080(foot, 0, 1); skipped by the bob while crouched), play the
/// player footstep on the default surface (no material surface types in the range yet).
/// Table: 0x140e30080 -> 0x14074cc50 -> 0x14074b0a0 with type 0 picks by walkState (actorVolatile +0x3da8, vslot
/// 0xbd8; 0 while player flag 0xce46 bit 2): WALKING (1) -> CrouchWalk when ducked (vslot 0xbc8 = physics ducked)
/// else SlowWalk, RUNNING (2) / SPRINTING (3) -> Sprint, NOCLIP (0) -> none. The player's speed function
/// 0x140e2bd70 sets WALKING when the move magnitude <= pm_walkthreshold or walk is held (SpeedState::Walk), else
/// RUNNING (SP has no sprint, pm_sprintEnabled 0).
fn bob_footsteps(mut sound: ResMut<Sound>, layers: Option<Res<crate::hands_layers::HandsLayersState>>, sim: Option<Res<crate::Sim>>) {
    let (Some(layers), Some(sim)) = (layers, sim) else { return };
    if layers.step.is_none() {
        return;
    }
    let p = &sim.player;
    let gait = if p.speed_state != rancher_sim::player::SpeedState::Walk {
        Gait::Sprint
    } else if p.physics.flags & rancher_sim::physics::flags::DUCKED != 0 {
        Gait::Crouch
    } else {
        Gait::SlowWalk
    };
    sound.footstep(gait, "default");
}

/// Posts this frame's first-person anim sound events (viewanim::animate refills HandsEvents each frame).
fn anim_events(mut sound: ResMut<Sound>, events: Option<Res<HandsEvents>>) {
    let Some(events) = events else { return };
    for f in &events.0 {
        sound.anim_event(f.slot, &f.event);
    }
}

/// key path (dotted, without `edit.`) → string value, for every string in a decl.
fn collect_strings(b: &Block, prefix: &str, out: &mut HashMap<String, String>) {
    for (k, v) in &b.items {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            Value::Block(inner) => collect_strings(inner, &path, out),
            Value::Str(s) | Value::Atom(s) => {
                out.insert(path.trim_start_matches("edit.").to_owned(), s.clone());
            }
        }
    }
}

/// Bevy world → game axes (inverse of range::to_bevy).
fn to_game(v: Vec3) -> [f32; 3] {
    [-v.z, -v.x, v.y]
}

fn update(mut sound: ResMut<Sound>, sim: Option<Res<crate::Sim>>, transforms: Query<&GlobalTransform>) {
    let sound = &mut *sound;
    let Some(eng) = sound.engine.as_mut() else { return };
    if let Some(sim) = sim {
        let p = &sim.player;
        let o = p.physics.origin;
        let (pitch, yaw) = (p.view_angles[0].to_radians(), p.view_angles[1].to_radians());
        eng.set_listener(Listener {
            pos: [o.x, o.y, o.z + p.eye_height()],
            forward: [yaw.cos() * pitch.cos(), yaw.sin() * pitch.cos(), -pitch.sin()],
            up: [0.0, 0.0, 1.0],
        });
        eng.set_emitter(PLAYER_EMITTER, [o.x, o.y, o.z + p.eye_height()]);
    }
    sound.tracked.retain(|(e, id)| match transforms.get(*e) {
        Ok(t) => {
            eng.set_emitter(*id, to_game(t.translation()));
            true
        }
        Err(_) => {
            eng.remove_emitter(*id);
            sound.entities.remove(e);
            false
        }
    });
    eng.update();
}

#[cfg(test)]
mod tests {
    use idaudio::curve::{db_to_gain, Curve, SCALING_DB};
    use idswf::menu::{slider_value, SliderMap};

    /// Init.bnk's BusVolume curve for every volume RTPC (master/sfx/music/vo_volume, AUDIO.md "Volume cvars"):
    /// (-60, -1, Exp3) (0, 0, Constant) (60, 0), scaling 2 (linear amplitude - 1).
    fn volume_curve() -> Curve {
        Curve { scaling: SCALING_DB, points: vec![(-60.0, -1.0, 8), (0.0, 0.0, 9), (60.0, 0.0, 4)] }
    }

    /// s_volume_* dB -> bus gain: silent at -60, unity at 0, no boost above; between, the Exp3 segment gives
    /// gain = ((dB + 60) / 60)^3. With the menu's dB = 60 * (sqrt(slider / 100) - 1) (0x141000ed0) that is
    /// gain = (slider / 100)^1.5.
    #[test]
    fn volume_cvar_gain_math() {
        let c = volume_curve();
        assert_eq!(db_to_gain(c.eval(-60.0)), 0.0);
        assert_eq!(db_to_gain(c.eval(-90.0)), 0.0);
        assert!((c.eval(0.0)).abs() < 1e-6);
        assert!((c.eval(12.0)).abs() < 1e-6, "no boost above 0 dB");
        assert!((c.eval(-30.0) - 20.0 * 0.125f32.log10()).abs() < 1e-3, "-30 dB cvar -> -18.06 dB");
        for slider in [1.0, 10.0, 25.0, 50.0, 81.0, 100.0] {
            let db: f32 = slider_value(&SliderMap::VolumeDb, slider).parse().unwrap();
            let gain = db_to_gain(c.eval(db));
            assert!((gain - (slider / 100.0f32).powf(1.5)).abs() < 1e-3, "slider {slider}: {db} dB -> {gain}");
        }
    }

    /// The bank's own curves match the one above (needs the install; skipped without it).
    #[test]
    fn init_bnk_volume_curves() {
        let sound = super::Sound::headless();
        let Some(eng) = sound.engine.as_ref() else {
            eprintln!("no DOOM install: skipped");
            return;
        };
        for (bus, rtpc) in [("Master Audio Bus", "master_volume"), ("UI", "sfx_volume"), ("Music", "music_volume"), ("VO", "vo_volume")] {
            let id = idaudio::hash::fnv1_lower(rtpc);
            let curves = eng.lib.bus_rtpcs(idaudio::hash::fnv1_lower(bus));
            let r = curves.iter().find(|r| r.rtpc == id).unwrap_or_else(|| panic!("{bus}: no {rtpc} curve"));
            assert_eq!(r.param, idaudio::hirc::prop::BUS_VOLUME, "{bus}");
            let (c, want) = (r.curve(), volume_curve());
            for db in [-60.0, -45.0, -30.0, -6.0, 0.0, 6.0] {
                assert!((c.eval(db) - want.eval(db)).abs() < 1e-3, "{bus} at {db}: {} vs {}", c.eval(db), want.eval(db));
            }
        }
    }
}

#[cfg(test)]
mod footstep_tests {
    use super::*;

    #[test]
    fn footstep_table_chains() {
        // 0x14074b0a0 non-friendly fallbacks; entitydef/player: Sprint footsteps/player_sprint, SlowWalk and
        // CrouchWalk footsteps/player_slowwalk, footstepEffectTable footsteps/player.
        assert_eq!(footstep_tables(Gait::Sprint), ["footstepEffectTable_Sprint", "footstepEffectTable"]);
        assert_eq!(footstep_tables(Gait::SlowWalk), ["footstepEffectTable_SlowWalk", "footstepEffectTable"]);
        assert_eq!(footstep_tables(Gait::Crouch), ["footstepEffectTable_CrouchWalk", "footstepEffectTable_SlowWalk", "footstepEffectTable"]);
    }
}
