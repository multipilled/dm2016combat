//! The in-game pause / settings menu: DOOM's own menu_shell_ingame.bswf (and dialog.bswf for the confirmations)
//! driven by `idswf::menu::Menu`, drawn on the HUD camera above the HUD with the same batch pipeline as `swf_hud`.
//!
//! - Esc opens the menu (and goes back / closes it): the plugin claims Esc ([`MenuOwnsEscape`]); [`MenuOpen`] pauses
//!   the game and frees the cursor. `MenuOpen` set by anything else (RANCHER_MENU) opens it too.
//! - Settings (cvars) go through [`Settings`] (applied live, saved to rancher.cfg); key binds through [`Actions`]
//!   (`bind` / `unbind` / `reset_binds`, saved by `settings::save`).
//! - The pause buttons become [`MenuAction`]s: Resume, RestartLevel (load checkpoint / restart mission) and Quit
//!   (exit to main menu / desktop; INTERIM: there is no main menu).
//! - Mouse: hover / click / slider drag / wheel; keys: arrows, Enter, Esc, R (DEFAULTS / REVERT TO DEFAULT; INTERIM,
//!   the game's shortcut key was not decoded). While the key bindings screen waits for a key every key, mouse button
//!   and wheel notch is the new binding (Esc cancels).
//!
//! The HUD hides while the menu is up (INTERIM) and a black layer dims the frozen game (see `DIM_ALPHA`).
//!
//! Self-tests: RANCHER_MENU=<t> opens the menu at t seconds (autotest); RANCHER_MENU_KEYS=<t:token,...> scripts its
//! input, tokens: up down left right accept back defaults press release wheel=<n> at=<x>/<y> key=<ENGINE KEY NAME>,
//! each optionally `*<n>` to repeat (e.g. `3:down,3.2:accept,4:key=UPARROW,5:right*50`). Times are seconds since start; an event waits until the menu is open.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};

use anyhow::Context;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use idswf::menu::{CvarStore, Menu, MenuEvent, MenuInput};
use idswf::texture::Texture;

use crate::input::{self, Actions};
use crate::settings::{MenuAction, MenuOpen, MenuOwnsEscape, Settings};
use crate::swf_hud::{BatchCx, Batcher, ImageKey, Mips, mips, player_images};

/// z of the menu's first batch slot (the HUD's start at 0).
const MENU_Z: f32 = 10.0;
/// shell_backdrop.bswf transparentScreen.bgFill alpha at the end of its rollOnBack is 0.387, blended in the engine's
/// gamma space (sRGB values scale by 0.613). This target blends in linear space, where the same darkening is
/// alpha 1 - 0.613^2.2 = 0.66. INTERIM: the fill's colour is assumed black.
const DIM_ALPHA: f32 = 0.66;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MenuOwnsEscape)
            .insert_resource(MenuScript::from_env())
            .add_systems(Startup, start)
            .add_systems(Update, menu_input)
            .add_systems(PostUpdate, (finish_loading, draw).chain());
    }
}

/// The engine's key names, in key-number order (input.rs's table).
const KEY_NAMES: &[&str] = &[
    "ESCAPE", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "MINUS", "EQUALS", "BACKSPACE", "TAB", "Q", "W", "E",
    "R", "T", "Y", "U", "I", "O", "P", "LBRACKET", "RBRACKET", "ENTER", "LCTRL", "A", "S", "D", "F", "G", "H", "J",
    "K", "L", "SEMICOLON", "APOSTROPHE", "GRAVE", "LSHIFT", "BACKSLASH", "Z", "X", "C", "V", "B", "N", "M", "COMMA",
    "PERIOD", "SLASH", "RSHIFT", "KP_STAR", "LALT", "SPACE", "CAPSLOCK", "F1", "F2", "F3", "F4", "F5", "F6", "F7",
    "F8", "F9", "F10", "NUMLOCK", "SCROLL", "KP_7", "KP_8", "KP_9", "KP_MINUS", "KP_4", "KP_5", "KP_6", "KP_PLUS",
    "KP_1", "KP_2", "KP_3", "KP_0", "KP_DOT", "F11", "F12", "KP_EQUALS", "KP_ENTER", "RCTRL", "KP_COMMA", "KP_SLASH",
    "PRINTSCREEN", "RALT", "PAUSE", "HOME", "UPARROW", "PGUP", "LEFTARROW", "RIGHTARROW", "END", "DOWNARROW", "PGDN",
    "INS", "DEL", "LWIN", "RWIN", "APPS", "MOUSE1", "MOUSE2", "MOUSE3", "MOUSE4", "MOUSE5", "MOUSE6", "MOUSE7",
    "MOUSE8", "MWHEELDOWN", "MWHEELUP",
];

struct Loaded {
    menu: Menu,
    assets: idswf::Assets,
    images: Vec<(ImageKey, Mips)>,
    known: Vec<String>,
}

#[derive(Resource)]
struct Loading(Mutex<Receiver<anyhow::Result<Loaded>>>);

#[derive(Resource)]
struct MenuRuntime {
    menu: Menu,
    assets: idswf::Assets,
    batcher: Batcher,
    /// Every `_action` a key can be bound to (default.cfg's, the key bindings screen's, the saved binds'): the
    /// engine keeps a key's action string, [`Actions`] only answers per action.
    known: Vec<String>,
    /// Arrow key repeat: the key, seconds held, seconds to the next repeat.
    repeat: Option<(KeyCode, f32)>,
    last_pointer: Option<Vec2>,
}

fn start(mut commands: Commands) {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(load());
    });
    commands.insert_resource(Loading(Mutex::new(rx)));
}

fn load() -> anyhow::Result<Loaded> {
    let doom = idres::find_install().context("DOOM (2016) install not found")?;
    let assets = idswf::Assets::open(&doom)?;
    let menu = Menu::load(&assets)?;
    let mut images = vec![(ImageKey::White, mips(&Texture::white()))];
    player_images(&mut images, 0, &menu.player);
    if let Some(d) = &menu.dialog_player {
        player_images(&mut images, 1, d);
    }
    let mut known: Vec<String> = idswf::menu::BIND_ROWS.iter().flat_map(|(_, a)| a.split_whitespace()).map(str::to_ascii_lowercase).collect();
    known.extend(default_cfg_actions(&assets.container));
    known.sort();
    known.dedup();
    Ok(Loaded { menu, assets, images, known })
}

/// The `_action`s default.cfg binds (any bindset).
fn default_cfg_actions(c: &idres::Container) -> Vec<String> {
    let Some(e) = c.get("generated/binaryfile/default.bfile") else { return Vec::new() };
    let Some(text) = c.read(e).ok().and_then(|raw| idres::crypt::decrypt(&raw, &e.short_name)) else { return Vec::new() };
    String::from_utf8_lossy(&text)
        .lines()
        .filter(|l| l.trim_start().starts_with("bind "))
        .flat_map(|l| l.split('"').skip(3).flat_map(|a| a.split_whitespace().map(str::to_string).collect::<Vec<_>>()).collect::<Vec<_>>())
        .filter(|a| a.starts_with('_'))
        .map(|a| a.to_ascii_lowercase())
        .collect()
}

fn finish_loading(mut commands: Commands, loading: Option<Res<Loading>>, mut images: ResMut<Assets<Image>>, settings: Option<Res<Settings>>) {
    let Some(loading) = loading else { return };
    let result = match loading.0.lock().unwrap().try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("loader thread died")),
    };
    commands.remove_resource::<Loading>();
    match result {
        Ok(mut l) => {
            // Actions saved in rancher.cfg that default.cfg does not bind.
            if let Some(s) = settings {
                for (_, a) in s.binds() {
                    l.known.extend(a.split_whitespace().map(str::to_ascii_lowercase));
                }
                l.known.sort();
                l.known.dedup();
            }
            let handles: HashMap<ImageKey, Option<Handle<Image>>> = l.images.into_iter().map(|(k, m)| (k, Some(images.add(crate::swf_hud::image(m))))).collect();
            commands.insert_resource(MenuRuntime { menu: l.menu, assets: l.assets, batcher: Batcher::new(handles, MENU_Z), known: l.known, repeat: None, last_pointer: None });
        }
        Err(e) => eprintln!("swf menu: {e:#}"),
    }
}

/// The menu's view of the player's settings and key binds.
struct Store<'a> {
    settings: &'a mut Settings,
    actions: &'a mut Actions,
    known: &'a [String],
}

impl CvarStore for Store<'_> {
    fn cvar(&self, name: &str) -> Option<String> {
        self.settings.get(name).map(str::to_string)
    }

    fn set_cvar(&mut self, name: &str, value: &str) {
        self.settings.set(name, value);
    }

    fn reset_cvar(&mut self, name: &str) {
        self.settings.reset(name);
    }

    fn binds(&self) -> Vec<(String, Vec<String>)> {
        let mut by_input: HashMap<input::Input, Vec<String>> = HashMap::new();
        for a in self.known {
            for i in self.actions.inputs_for(a) {
                by_input.entry(i).or_default().push(a.clone());
            }
        }
        KEY_NAMES
            .iter()
            .filter_map(|n| {
                let i = input::key_name(n)?;
                Some((n.to_string(), by_input.remove(&i)?))
            })
            .collect()
    }

    fn set_bind(&mut self, key: &str, actions: &str) {
        if let Some(i) = input::key_name(key) {
            self.actions.bind(i, actions);
        }
    }

    fn reset_binds(&mut self) {
        self.actions.reset_binds();
    }
}

/// RANCHER_MENU_KEYS: scripted menu input.
#[derive(Resource, Default)]
struct MenuScript {
    events: Vec<(f32, MenuInput)>,
}

impl MenuScript {
    fn from_env() -> Self {
        let mut events = Vec::new();
        for item in std::env::var("RANCHER_MENU_KEYS").unwrap_or_default().split(',') {
            let Some((t, tok)) = item.trim().split_once(':') else { continue };
            let Ok(t) = t.trim().parse::<f32>() else { continue };
            // `<token>*<n>` repeats the token n times, 0.03 s apart.
            let (tok, n) = match tok.trim().rsplit_once('*') {
                Some((t, n)) if n.parse::<usize>().is_ok() => (t, n.parse::<usize>().unwrap()),
                _ => (tok.trim(), 1),
            };
            let ev = match tok {
                "up" => Some(MenuInput::Up),
                "down" => Some(MenuInput::Down),
                "left" => Some(MenuInput::Left),
                "right" => Some(MenuInput::Right),
                "accept" => Some(MenuInput::Accept),
                "back" => Some(MenuInput::Back),
                "defaults" => Some(MenuInput::Defaults),
                "press" => Some(MenuInput::Press),
                "release" => Some(MenuInput::Release),
                _ => tok.split_once('=').and_then(|(k, v)| match k {
                    "wheel" => v.parse().ok().map(MenuInput::Wheel),
                    "key" => Some(MenuInput::Key(v.to_ascii_uppercase())),
                    "at" => {
                        let (x, y) = v.split_once('/')?;
                        Some(MenuInput::Pointer(x.parse().ok()?, y.parse().ok()?))
                    }
                    _ => None,
                }),
            };
            match ev {
                Some(ev) => events.extend((0..n).map(|i| (t + i as f32 * 0.03, ev.clone()))),
                None => eprintln!("RANCHER_MENU_KEYS: unknown token '{tok}'"),
            }
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        MenuScript { events }
    }
}

/// Esc, keys, mouse and the script go to the menu; its events become [`MenuAction`]s.
#[allow(clippy::too_many_arguments)]
fn menu_input(
    rt: Option<ResMut<MenuRuntime>>,
    mut open: ResMut<MenuOpen>,
    mut settings: ResMut<Settings>,
    mut actions: ResMut<Actions>,
    mut script: ResMut<MenuScript>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    wheel: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut out: MessageWriter<MenuAction>,
) {
    let esc = keys.just_pressed(KeyCode::Escape);
    let mut opened_now = false;
    if esc && !open.0 {
        open.0 = true;
        opened_now = true;
    }
    let Some(mut rt) = rt else { return };
    let rt = &mut *rt;
    let mut store = Store { settings: &mut settings, actions: &mut actions, known: &rt.known };
    if open.0 && !rt.menu.open {
        rt.menu.open(&store);
    }
    if !open.0 {
        if rt.menu.open {
            rt.menu.close();
        }
        return;
    }
    if opened_now {
        // The Esc that opened the menu is not also "back".
        return;
    }

    let mut events: Vec<MenuInput> = Vec::new();
    let now = time.elapsed_secs();
    while script.events.first().is_some_and(|(t, _)| *t <= now) {
        events.push(script.events.remove(0).1);
    }
    if rt.menu.capturing() {
        // Every input is the new key; Esc cancels (the menu decides).
        if let Some(i) = input::just_pressed_input(&keys, &mouse, &wheel) {
            if let Some(name) = input::input_name(i) {
                events.push(MenuInput::Key(name.to_string()));
            }
        }
    } else {
        if esc {
            events.push(MenuInput::Back);
        }
        for (key, ev) in [
            (KeyCode::ArrowUp, MenuInput::Up),
            (KeyCode::ArrowDown, MenuInput::Down),
            (KeyCode::ArrowLeft, MenuInput::Left),
            (KeyCode::ArrowRight, MenuInput::Right),
        ] {
            if keys.just_pressed(key) {
                events.push(ev);
                rt.repeat = Some((key, -0.35));
            } else if keys.pressed(key) && rt.repeat.is_some_and(|(k, _)| k == key) {
                let (_, held) = rt.repeat.as_mut().unwrap();
                *held += time.delta_secs();
                if *held >= 0.07 {
                    *held = 0.0;
                    events.push(ev);
                }
            }
        }
        if rt.repeat.is_some_and(|(k, _)| !keys.pressed(k)) {
            rt.repeat = None;
        }
        if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]) {
            events.push(MenuInput::Accept);
        }
        if keys.just_pressed(KeyCode::KeyR) {
            events.push(MenuInput::Defaults);
        }
        if wheel.delta.y != 0.0 {
            events.push(MenuInput::Wheel(wheel.delta.y.signum() as i32 * (wheel.delta.y.abs().ceil() as i32).min(3)));
        }
        if let Ok(w) = window.single() {
            if let Some(p) = w.cursor_position() {
                if rt.last_pointer != Some(p) {
                    rt.last_pointer = Some(p);
                    events.push(MenuInput::Pointer(p.x, p.y));
                }
            }
        }
        if mouse.just_pressed(MouseButton::Left) {
            events.push(MenuInput::Press);
        }
    }
    if mouse.just_released(MouseButton::Left) {
        events.push(MenuInput::Release);
    }

    for ev in events {
        match rt.menu.input(ev, &mut store) {
            Some(MenuEvent::Resume) => {
                out.write(MenuAction::Resume);
            }
            Some(MenuEvent::LoadCheckpoint | MenuEvent::RestartMission) => {
                out.write(MenuAction::RestartLevel);
            }
            Some(MenuEvent::ExitToMainMenu | MenuEvent::ExitToDesktop) => {
                out.write(MenuAction::Quit);
            }
            None => {}
        }
    }
}

/// Advances the menu and rebuilds its batch meshes; the dim layer follows the menu.
#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    rt: Option<ResMut<MenuRuntime>>,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<crate::swf_hud::HudMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut vis: Query<&mut Visibility>,
) {
    let Ok(window) = window.single() else { return };
    let (w, h) = (window.width(), window.height());
    let Some(mut rt) = rt else { return };
    let rt = &mut *rt;
    rt.menu.update(w, h, time.delta_secs_f64());
    let mut cx = BatchCx { commands: &mut commands, meshes: &mut meshes, materials: &mut materials, images: &mut images };
    rt.batcher.begin();
    if rt.menu.open {
        // The frozen game dims behind the menu.
        let dim = dim_list(w, h);
        rt.batcher.push(&mut cx, &rt.assets, &dim, &ImageKey::White, None, (w, h), (w, h), 1.0);
        for (i, p) in rt.menu.players().into_iter().enumerate() {
            let list = idswf::render::draw_clipped(p, w, h);
            rt.batcher.push(&mut cx, &rt.assets, &list, &ImageKey::Atlas(i), None, (p.swf.frame_width, p.swf.frame_height), (w, h), 1.0);
        }
    }
    rt.batcher.finish(&mut vis);
}

/// A full-window black quad at [`DIM_ALPHA`] (the draw list's white texture is the tint's carrier).
fn dim_list(w: f32, h: f32) -> idswf::render::DrawList {
    use idswf::render::{Batch, Blend, DrawList, Shader, StencilOp, TexRef, Vertex};
    let v = |x: f32, y: f32| Vertex { pos: [x, y], uv: [0.5, 0.5], color: [0.0, 0.0, 0.0, DIM_ALPHA], add: [0.0; 4] };
    DrawList {
        width: w,
        height: h,
        batches: vec![Batch {
            texture: TexRef::White,
            shader: Shader::Gui,
            blend: Blend::Normal,
            stencil: 0,
            op: StencilOp::Draw,
            verts: vec![v(0.0, 0.0), v(w, 0.0), v(w, h), v(0.0, h)],
            indices: vec![0, 1, 2, 0, 2, 3],
        }],
    }
}
