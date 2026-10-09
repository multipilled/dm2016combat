//! Player settings: the cvar values the options menu changes, on top of the install's cvar table (exe defaults +
//! shipped configs). Changes apply live and are saved to `<Saved Games>/dm2016combat/rancher.cfg` as `seta` lines
//! (the game's own config syntax; the game's DOOMConfig.local is never written). `RANCHER_CFG=<path>` picks another
//! file; self-tests read and write no file unless it is set.
//!
//! The menu (swf_menu, hud-swf) reads and writes cvars through [`Settings`] and opens / closes with [`MenuOpen`].

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};
use rancher_sim::config::CvarValues;

#[derive(Resource)]
pub struct Settings {
    defaults: CvarValues,
    /// Lowercase name -> the table's spelling (cvar names are case-insensitive).
    names: HashMap<String, String>,
    overrides: BTreeMap<String, String>,
    /// Values for this run only (environment overrides such as RANCHER_HDR); never saved.
    session: BTreeMap<String, String>,
    path: Option<PathBuf>,
    /// The player's key binds that differ from default.cfg (`bind "<KEY>" "<actions>"` lines; "" = unbound).
    binds: Vec<(String, String)>,
    /// Cvars changed since the last apply.
    changed: Vec<String>,
    dirty: bool,
}

impl Settings {
    /// Loads the saved overrides; every saved cvar counts as changed, so the first apply honours it.
    pub fn load(defaults: &CvarValues, self_test: bool) -> Self {
        let path = match std::env::var("RANCHER_CFG") {
            Ok(p) if !p.is_empty() => Some(PathBuf::from(p)),
            _ if self_test => None,
            _ => std::env::var("USERPROFILE").ok().map(|h| PathBuf::from(h).join("Saved Games").join("dm2016combat").join("rancher.cfg")),
        };
        let names = defaults.0.keys().map(|k| (k.to_ascii_lowercase(), k.clone())).collect();
        let mut s = Settings { defaults: defaults.clone(), names, overrides: BTreeMap::new(), session: BTreeMap::new(), path, binds: Vec::new(), changed: Vec::new(), dirty: false };
        if let Some(text) = s.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("bind ") {
                    let args: Vec<&str> = rest.split('"').map(str::trim).filter(|a| !a.is_empty()).collect();
                    match args.as_slice() {
                        [key, actions] => s.binds.push((key.to_string(), actions.to_string())),
                        [key] => s.binds.push((key.to_string(), String::new())),
                        _ => {}
                    }
                    continue;
                }
                if let Some((name, value)) = parse_line(line) {
                    let name = s.canonical(name);
                    s.changed.push(name.clone());
                    s.overrides.insert(name, value);
                }
            }
        }
        s
    }

    fn canonical(&self, name: &str) -> String {
        self.names.get(&name.to_ascii_lowercase()).cloned().unwrap_or_else(|| name.to_string())
    }

    /// The current value: the player's setting, else the install's.
    pub fn get(&self, name: &str) -> Option<&str> {
        let name = self.canonical(name);
        self.session.get(&name).or_else(|| self.overrides.get(&name)).or_else(|| self.defaults.0.get(&name)).map(|v| v.as_str())
    }

    /// Sets a value for this run only (not saved); a later [`Settings::set`] of the same cvar replaces it.
    pub fn set_session(&mut self, name: &str, value: impl ToString) {
        let name = self.canonical(name);
        self.session.insert(name.clone(), value.to_string());
        self.changed.push(name);
    }

    pub fn f32(&self, name: &str) -> Option<f32> {
        self.get(name).and_then(|v| v.trim_end_matches('f').parse().ok())
    }

    pub fn bool(&self, name: &str) -> bool {
        self.f32(name).is_some_and(|v| v != 0.0)
    }

    pub fn set(&mut self, name: &str, value: impl ToString) {
        let name = self.canonical(name);
        let value = value.to_string();
        self.session.remove(&name);
        if self.get(&name) == Some(value.as_str()) {
            return;
        }
        if self.defaults.0.get(&name) == Some(&value) {
            self.overrides.remove(&name);
        } else {
            self.overrides.insert(name.clone(), value);
        }
        self.changed.push(name);
        self.dirty = true;
    }

    /// Back to the install's value.
    pub fn reset(&mut self, name: &str) {
        let name = self.canonical(name);
        if self.overrides.remove(&name).is_some() {
            self.changed.push(name);
            self.dirty = true;
        }
    }

    /// The install's table with the player's settings applied.
    pub fn merged(&self) -> CvarValues {
        let mut c = self.defaults.clone();
        for (k, v) in self.overrides.iter().chain(&self.session) {
            c.0.insert(k.clone(), v.clone());
        }
        c
    }

    pub fn binds(&self) -> &[(String, String)] {
        &self.binds
    }

    pub fn take_changed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.changed)
    }

    fn save(&mut self) {
        self.dirty = false;
        let Some(path) = &self.path else { return };
        let mut text = String::from("// dm2016combat settings (cvars changed from the install's defaults)\n");
        for (k, v) in &self.overrides {
            text.push_str(&format!("seta {k} \"{}\"\n", v.replace('"', "")));
        }
        for (k, v) in &self.binds {
            text.push_str(&format!("bind \"{k}\" \"{v}\"\n"));
        }
        let res = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(path, text));
        if let Err(e) = res {
            eprintln!("settings: can't save {}: {e}", path.display());
        }
    }
}

/// `seta name "value"`, `set name value` or `name "value"`; `//` comments.
fn parse_line(line: &str) -> Option<(&str, String)> {
    let line = line.split("//").next()?.trim();
    let mut rest = line;
    for kw in ["seta ", "set "] {
        if let Some(r) = rest.strip_prefix(kw) {
            rest = r.trim_start();
        }
    }
    let (name, value) = rest.split_once(char::is_whitespace)?;
    let value = value.trim().trim_matches('"');
    if name.is_empty() || name.contains('"') {
        return None;
    }
    Some((name, value.to_string()))
}

/// The pause / options menu is open: the game is paused, the cursor is free and game actions are not read. main.rs
/// toggles it on Esc unless a menu plugin claims Esc ([`MenuOwnsEscape`]).
#[derive(Resource, Default)]
pub struct MenuOpen(pub bool);

/// Inserted by the menu plugin when it handles Esc itself (open / back / close).
#[derive(Resource, Default)]
pub struct MenuOwnsEscape;

/// Menu commands main.rs carries out.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Resume,
    RestartLevel,
    Quit,
}

/// The post chain's cvars.
const POST_CVARS: &[&str] = &[
    "r_hdrAutoExposureBase", "r_hdrAutoExposureRatio", "r_hdrAutoExposureSpeed", "r_hdrBloom", "r_hdrBloomRatio", "r_hdrBloomThreshold",
    "r_lensDirtRatio", "r_lensFlaresRatio", "r_contrast", "r_saturation", "r_gamma", "r_chromaticAberration", "r_chromaticAberrationLimit",
    "r_vignette", "r_colorCorrection", "r_sharpening", "r_filmGrainRatio", "r_skipFlares",
];

/// Applies changed cvars to what reads them. Window cvars only apply outside self-tests.
pub fn apply(
    mut settings: ResMut<Settings>,
    mut sim: ResMut<crate::Sim>,
    mut hdr: ResMut<crate::HdrMode>,
    mut layers: ResMut<crate::hands_layers::HandsLayersState>,
    post: Option<ResMut<crate::post::PostSettings>>,
    auto: Res<crate::autotest::AutoTest>,
    sound: Option<ResMut<crate::sound::Sound>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let changed = settings.take_changed();
    if changed.is_empty() {
        return;
    }
    if let Some(mut sound) = sound {
        apply_volume(&settings, &changed, &mut sound);
    }
    let is = |n: &str| changed.iter().any(|c| c.eq_ignore_ascii_case(n));
    let s = &*settings;
    let cfg = &mut sim.player.cfg;
    if is("g_fov") {
        cfg.fov = s.f32("g_fov").unwrap_or(cfg.fov);
        // The hands FOV scale divides by the live g_fov (default / g_fov), so the hands keep their size.
        layers.cvars.g_fov = cfg.fov;
    }
    if is("m_sensitivity") {
        cfg.m_sensitivity = s.f32("m_sensitivity").unwrap_or(cfg.m_sensitivity);
    }
    if is("m_yaw") {
        cfg.m_yaw = s.f32("m_yaw").unwrap_or(cfg.m_yaw);
    }
    if is("m_pitch") {
        cfg.m_pitch = s.f32("m_pitch").unwrap_or(cfg.m_pitch);
    }
    if is("r_hdrPostProcess") {
        hdr.on = s.bool("r_hdrPostProcess");
    }
    if POST_CVARS.iter().any(|n| is(n)) {
        if let Some(mut p) = post {
            p.cvars = crate::post::PostCvars::from_cvars(&s.merged());
            // The menu's lens flare toggle is !r_skipFlares; the post chain's flares are its only flares.
            if s.bool("r_skipFlares") {
                p.cvars.lens_flares_ratio = 0.0;
            }
        }
    }
    if auto.shot.is_none() && ["r_fullscreen", "r_windowWidth", "r_windowHeight", "r_swapInterval"].iter().any(|n| is(n)) {
        if let Ok(mut w) = windows.single_mut() {
            // r_fullscreen: 0 windowed, 1 fullscreen, 2 borderless.
            w.mode = match s.f32("r_fullscreen").unwrap_or(0.0) as i32 {
                1 => WindowMode::Fullscreen(MonitorSelection::Current, bevy::window::VideoModeSelection::Current),
                2 => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            let (ww, wh) = (s.f32("r_windowWidth").unwrap_or(0.0) as u32, s.f32("r_windowHeight").unwrap_or(0.0) as u32);
            if ww > 0 && wh > 0 && w.mode == WindowMode::Windowed {
                w.resolution.set(ww as f32, wh as f32);
            }
            // r_swapInterval: 0 off, 1 on, -1 adaptive (tears when late).
            w.present_mode = match s.f32("r_swapInterval").unwrap_or(0.0) as i32 {
                0 => PresentMode::AutoNoVsync,
                1 => PresentMode::AutoVsync,
                _ => PresentMode::FifoRelaxed,
            };
        }
    }
}

/// The AUDIO tab's volume cvars (dB; the menu writes dB = 60 * (sqrt(slider / 100) - 1), idswf::menu) to the
/// sound engine, which sets the global RTPCs the exe's Wwise_SoundHardware sets from them (sound::VOLUME_CVARS).
/// s_volume_ambient is shown by the menu but no sound code applies it (AUDIO.md), so it is not here either.
pub fn apply_volume(settings: &Settings, changed: &[String], sound: &mut crate::sound::Sound) {
    for (cvar, _) in crate::sound::VOLUME_CVARS {
        if changed.iter().any(|c| c.eq_ignore_ascii_case(cvar)) {
            if let Some(db) = settings.f32(cvar) {
                sound.set_volume_cvar(cvar, db);
            }
        }
    }
}

/// Writes the settings file after a change (cvars or key binds).
pub fn save(mut settings: ResMut<Settings>, mut actions: ResMut<crate::input::Actions>) {
    if let Some(binds) = actions.take_changed_binds() {
        settings.binds = binds;
        settings.dirty = true;
    }
    if settings.dirty {
        settings.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use idswf::menu::{slider_value, SliderMap};

    /// The menu's volume slider -> s_volume_* (dB) -> settings::apply's volume step -> the engine's bus gain.
    /// Needs the install (Init.bnk); skipped without it.
    #[test]
    fn volume_slider_reaches_the_mixer() {
        let mut sound = crate::sound::Sound::headless();
        let Some(eng) = sound.engine_mut() else {
            eprintln!("no DOOM install: skipped");
            return;
        };
        // "UI" is bound to sfx_volume, "Music" to music_volume, both under Master Audio Bus (master_volume).
        let (ui0, music0) = (eng.bus_volume_db("UI"), eng.bus_volume_db("Music"));
        let defaults = CvarValues(crate::sound::VOLUME_CVARS.iter().map(|(c, _)| (c.to_string(), "0".to_string())).collect());
        let mut settings = Settings::load(&defaults, true);
        settings.set("s_volume_sfx", slider_value(&SliderMap::VolumeDb, 50.0));
        settings.set("s_volume_dB", slider_value(&SliderMap::VolumeDb, 81.0));
        let changed = settings.take_changed();
        apply_volume(&settings, &changed, &mut sound);
        let eng = sound.engine_mut().unwrap();
        // The RTPCs ramp over 500 ms of mixer time.
        let mixer = eng.mixer();
        mixer.lock().unwrap().render(&mut vec![0.0; 2 * 48000 * 6 / 10]);
        eng.update();
        let gain_db = |slider: f32| 20.0 * (slider / 100.0f32).powf(1.5).log10();
        let (ui, music) = (eng.bus_volume_db("UI"), eng.bus_volume_db("Music"));
        assert!((ui - ui0 - (gain_db(50.0) + gain_db(81.0))).abs() < 0.05, "UI {ui0} -> {ui}");
        assert!((music - music0 - gain_db(81.0)).abs() < 0.05, "Music {music0} -> {music}");
    }
}
