//! Game actions from the install's default key bindings (generated/binaryfile/default.bfile = default.cfg,
//! `bindset 0`, single-player first person). Each frame [`Actions`] holds which `_action`s are held and
//! which were pressed this frame; gameplay code reads actions, never keys.

use std::collections::{BTreeMap, HashMap, HashSet};

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use idres::Container;

/// A physical input as named by default.cfg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Input {
    Key(KeyCode),
    Mouse(MouseButton),
    WheelUp,
    WheelDown,
}

#[derive(Resource, Default)]
pub struct Actions {
    /// Input -> actions bound to it (a bind may name several, e.g. "_zoom _altfire").
    binds: HashMap<Input, Vec<String>>,
    /// default.cfg's binds (the menu's "reset to defaults").
    defaults: HashMap<Input, Vec<String>>,
    /// The player's binds that differ from default.cfg: key name -> actions ("" = unbound); saved by settings.rs.
    user: BTreeMap<String, String>,
    binds_changed: bool,
    held: HashSet<String>,
    pressed: HashSet<String>,
    /// Synthetic presses for the next frame (self-tests).
    queued: Vec<String>,
    /// Synthetic holds for the next frame (self-tests).
    queued_holds: Vec<String>,
}

impl Actions {
    pub fn held(&self, action: &str) -> bool {
        self.held.contains(action)
    }
    pub fn pressed(&self, action: &str) -> bool {
        self.pressed.contains(action)
    }
    /// Synthetic hold (no press edge) applied on the next frame (self-tests).
    pub fn queue_hold(&mut self, action: &str) {
        self.queued_holds.push(action.to_ascii_lowercase());
    }

    /// Synthetic press + hold applied on the next frame (self-tests).
    pub fn queue_press(&mut self, action: &str) {
        self.queued.push(action.to_ascii_lowercase());
    }

    /// Parses `bind "<key>" "<actions>"` lines of one bindset.
    pub fn from_cfg(text: &str, bindset: u32) -> Self {
        let mut a = Actions::default();
        let mut set = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(n) = line.strip_prefix("bindset ") {
                set = n.trim().parse::<u32>().ok();
                continue;
            }
            if set != Some(bindset) {
                continue;
            }
            let Some(rest) = line.strip_prefix("bind ") else { continue };
            let parts = split_args(rest);
            if parts.len() < 2 {
                continue;
            }
            let Some(input) = key_name(&parts[0]) else { continue };
            let actions: Vec<String> = parts[1].split_whitespace().filter(|s| s.starts_with('_')).map(|s| s.to_ascii_lowercase()).collect();
            if !actions.is_empty() {
                a.binds.entry(input).or_default().extend(actions);
            }
        }
        a.defaults = a.binds.clone();
        a
    }

    /// Binds `input` to `actions` (space-separated `_action`s; "" unbinds it), replacing what the key did, as the
    /// game's `bind` does.
    pub fn bind(&mut self, input: Input, actions: &str) {
        let list: Vec<String> = actions.split_whitespace().filter(|s| s.starts_with('_')).map(|s| s.to_ascii_lowercase()).collect();
        if list.is_empty() {
            self.binds.remove(&input);
        } else {
            self.binds.insert(input, list.clone());
        }
        if let Some(name) = input_name(input) {
            if self.defaults.get(&input).cloned().unwrap_or_default() == list {
                self.user.remove(name);
            } else {
                self.user.insert(name.to_string(), list.join(" "));
            }
        }
        self.binds_changed = true;
    }

    pub fn unbind(&mut self, input: Input) {
        self.bind(input, "");
    }

    /// Every input bound to `action`, in key-table order.
    pub fn inputs_for(&self, action: &str) -> Vec<Input> {
        let action = action.to_ascii_lowercase();
        KEY_NAMES.iter().map(|(_, i)| *i).filter(|i| self.binds.get(i).is_some_and(|l| l.contains(&action))).collect()
    }

    /// Back to default.cfg's binds.
    pub fn reset_binds(&mut self) {
        self.binds = self.defaults.clone();
        self.user.clear();
        self.binds_changed = true;
    }

    /// The saved binds (settings.rs `bind` lines), applied at startup.
    pub fn apply_user_binds(&mut self, binds: &[(String, String)]) {
        for (key, actions) in binds {
            if let Some(input) = key_name(key) {
                self.bind(input, actions);
            }
        }
        self.binds_changed = false;
    }

    /// The player's binds that differ from default.cfg, when they changed since the last call.
    pub fn take_changed_binds(&mut self) -> Option<Vec<(String, String)>> {
        if !std::mem::take(&mut self.binds_changed) {
            return None;
        }
        Some(self.user.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    pub fn load(c: &Container) -> Self {
        match c.get("generated/binaryfile/default.bfile").map(|e| (c.read(e), e.short_name.clone())) {
            Some((Ok(bytes), short)) => match idres::crypt::decrypt(&bytes, &short) {
                Some(text) => Self::from_cfg(&String::from_utf8_lossy(&text), 0),
                None => {
                    eprintln!("default.cfg: decryption failed");
                    Self::default()
                }
            },
            _ => {
                eprintln!("default.cfg not found; no key bindings");
                Self::default()
            }
        }
    }
}

fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                if quoted {
                    out.push(std::mem::take(&mut cur));
                }
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The engine's key names (its key table; display strings `#str_key_<NAME>`) for every input we can read.
const KEY_NAMES: &[(&str, Input)] = &[
    ("ESCAPE", Input::Key(KeyCode::Escape)), ("1", Input::Key(KeyCode::Digit1)), ("2", Input::Key(KeyCode::Digit2)),
    ("3", Input::Key(KeyCode::Digit3)), ("4", Input::Key(KeyCode::Digit4)), ("5", Input::Key(KeyCode::Digit5)),
    ("6", Input::Key(KeyCode::Digit6)), ("7", Input::Key(KeyCode::Digit7)), ("8", Input::Key(KeyCode::Digit8)),
    ("9", Input::Key(KeyCode::Digit9)), ("0", Input::Key(KeyCode::Digit0)), ("MINUS", Input::Key(KeyCode::Minus)),
    ("EQUALS", Input::Key(KeyCode::Equal)), ("BACKSPACE", Input::Key(KeyCode::Backspace)),
    ("TAB", Input::Key(KeyCode::Tab)), ("Q", Input::Key(KeyCode::KeyQ)), ("W", Input::Key(KeyCode::KeyW)),
    ("E", Input::Key(KeyCode::KeyE)), ("R", Input::Key(KeyCode::KeyR)), ("T", Input::Key(KeyCode::KeyT)),
    ("Y", Input::Key(KeyCode::KeyY)), ("U", Input::Key(KeyCode::KeyU)), ("I", Input::Key(KeyCode::KeyI)),
    ("O", Input::Key(KeyCode::KeyO)), ("P", Input::Key(KeyCode::KeyP)),
    ("LBRACKET", Input::Key(KeyCode::BracketLeft)), ("RBRACKET", Input::Key(KeyCode::BracketRight)),
    ("ENTER", Input::Key(KeyCode::Enter)), ("LCTRL", Input::Key(KeyCode::ControlLeft)),
    ("A", Input::Key(KeyCode::KeyA)), ("S", Input::Key(KeyCode::KeyS)), ("D", Input::Key(KeyCode::KeyD)),
    ("F", Input::Key(KeyCode::KeyF)), ("G", Input::Key(KeyCode::KeyG)), ("H", Input::Key(KeyCode::KeyH)),
    ("J", Input::Key(KeyCode::KeyJ)), ("K", Input::Key(KeyCode::KeyK)), ("L", Input::Key(KeyCode::KeyL)),
    ("SEMICOLON", Input::Key(KeyCode::Semicolon)), ("APOSTROPHE", Input::Key(KeyCode::Quote)),
    ("GRAVE", Input::Key(KeyCode::Backquote)), ("LSHIFT", Input::Key(KeyCode::ShiftLeft)),
    ("BACKSLASH", Input::Key(KeyCode::Backslash)), ("Z", Input::Key(KeyCode::KeyZ)), ("X", Input::Key(KeyCode::KeyX)),
    ("C", Input::Key(KeyCode::KeyC)), ("V", Input::Key(KeyCode::KeyV)), ("B", Input::Key(KeyCode::KeyB)),
    ("N", Input::Key(KeyCode::KeyN)), ("M", Input::Key(KeyCode::KeyM)), ("COMMA", Input::Key(KeyCode::Comma)),
    ("PERIOD", Input::Key(KeyCode::Period)), ("SLASH", Input::Key(KeyCode::Slash)),
    ("RSHIFT", Input::Key(KeyCode::ShiftRight)), ("KP_STAR", Input::Key(KeyCode::NumpadMultiply)),
    ("LALT", Input::Key(KeyCode::AltLeft)), ("SPACE", Input::Key(KeyCode::Space)),
    ("CAPSLOCK", Input::Key(KeyCode::CapsLock)), ("F1", Input::Key(KeyCode::F1)), ("F2", Input::Key(KeyCode::F2)),
    ("F3", Input::Key(KeyCode::F3)), ("F4", Input::Key(KeyCode::F4)), ("F5", Input::Key(KeyCode::F5)),
    ("F6", Input::Key(KeyCode::F6)), ("F7", Input::Key(KeyCode::F7)), ("F8", Input::Key(KeyCode::F8)),
    ("F9", Input::Key(KeyCode::F9)), ("F10", Input::Key(KeyCode::F10)), ("NUMLOCK", Input::Key(KeyCode::NumLock)),
    ("SCROLL", Input::Key(KeyCode::ScrollLock)), ("KP_7", Input::Key(KeyCode::Numpad7)),
    ("KP_8", Input::Key(KeyCode::Numpad8)), ("KP_9", Input::Key(KeyCode::Numpad9)),
    ("KP_MINUS", Input::Key(KeyCode::NumpadSubtract)), ("KP_4", Input::Key(KeyCode::Numpad4)),
    ("KP_5", Input::Key(KeyCode::Numpad5)), ("KP_6", Input::Key(KeyCode::Numpad6)),
    ("KP_PLUS", Input::Key(KeyCode::NumpadAdd)), ("KP_1", Input::Key(KeyCode::Numpad1)),
    ("KP_2", Input::Key(KeyCode::Numpad2)), ("KP_3", Input::Key(KeyCode::Numpad3)),
    ("KP_0", Input::Key(KeyCode::Numpad0)), ("KP_DOT", Input::Key(KeyCode::NumpadDecimal)),
    ("F11", Input::Key(KeyCode::F11)), ("F12", Input::Key(KeyCode::F12)),
    ("KP_EQUALS", Input::Key(KeyCode::NumpadEqual)), ("KP_ENTER", Input::Key(KeyCode::NumpadEnter)),
    ("RCTRL", Input::Key(KeyCode::ControlRight)), ("KP_COMMA", Input::Key(KeyCode::NumpadComma)),
    ("KP_SLASH", Input::Key(KeyCode::NumpadDivide)), ("PRINTSCREEN", Input::Key(KeyCode::PrintScreen)),
    ("RALT", Input::Key(KeyCode::AltRight)), ("PAUSE", Input::Key(KeyCode::Pause)),
    ("HOME", Input::Key(KeyCode::Home)), ("UPARROW", Input::Key(KeyCode::ArrowUp)),
    ("PGUP", Input::Key(KeyCode::PageUp)), ("LEFTARROW", Input::Key(KeyCode::ArrowLeft)),
    ("RIGHTARROW", Input::Key(KeyCode::ArrowRight)), ("END", Input::Key(KeyCode::End)),
    ("DOWNARROW", Input::Key(KeyCode::ArrowDown)), ("PGDN", Input::Key(KeyCode::PageDown)),
    ("INS", Input::Key(KeyCode::Insert)), ("DEL", Input::Key(KeyCode::Delete)),
    ("LWIN", Input::Key(KeyCode::SuperLeft)), ("RWIN", Input::Key(KeyCode::SuperRight)),
    ("APPS", Input::Key(KeyCode::ContextMenu)), ("MOUSE1", Input::Mouse(MouseButton::Left)),
    ("MOUSE2", Input::Mouse(MouseButton::Right)), ("MOUSE3", Input::Mouse(MouseButton::Middle)),
    ("MOUSE4", Input::Mouse(MouseButton::Back)), ("MOUSE5", Input::Mouse(MouseButton::Forward)),
    ("MOUSE6", Input::Mouse(MouseButton::Other(6))), ("MOUSE7", Input::Mouse(MouseButton::Other(7))),
    ("MOUSE8", Input::Mouse(MouseButton::Other(8))), ("MWHEELDOWN", Input::WheelDown), ("MWHEELUP", Input::WheelUp),
];

/// A key name (case-insensitive) -> input. default.cfg also writes the side-less SHIFT / CTRL / ALT, which match
/// either side.
pub fn key_name(name: &str) -> Option<Input> {
    let n = name.to_ascii_uppercase();
    match n.as_str() {
        "SHIFT" => return Some(Input::Key(KeyCode::ShiftLeft)),
        "CTRL" => return Some(Input::Key(KeyCode::ControlLeft)),
        "ALT" => return Some(Input::Key(KeyCode::AltLeft)),
        _ => {}
    }
    KEY_NAMES.iter().find(|(k, _)| *k == n).map(|(_, i)| *i)
}

/// The engine's name of an input (display: the `#str_key_<NAME>` string).
pub fn input_name(input: Input) -> Option<&'static str> {
    KEY_NAMES.iter().find(|(_, i)| *i == input).map(|(k, _)| *k)
}

/// The first input pressed this frame, for "press a key" rebinding (Esc included; the menu decides what it means).
pub fn just_pressed_input(keys: &ButtonInput<KeyCode>, mouse: &ButtonInput<MouseButton>, wheel: &AccumulatedMouseScroll) -> Option<Input> {
    if let Some(k) = keys.get_just_pressed().find(|k| input_name(Input::Key(**k)).is_some()) {
        return Some(Input::Key(*k));
    }
    if let Some(b) = mouse.get_just_pressed().find(|b| input_name(Input::Mouse(**b)).is_some()) {
        return Some(Input::Mouse(*b));
    }
    if wheel.delta.y > 0.0 {
        return Some(Input::WheelUp);
    }
    if wheel.delta.y < 0.0 {
        return Some(Input::WheelDown);
    }
    None
}

/// Rebuilds held / pressed actions from this frame's input (runs first in Update).
pub fn update_actions(
    mut actions: ResMut<Actions>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    wheel: Res<AccumulatedMouseScroll>,
    menu: Res<crate::settings::MenuOpen>,
) {
    let a = &mut *actions;
    a.held.clear();
    a.pressed.clear();
    if menu.0 {
        // The pause menu takes the input: no game actions while it is open.
        a.queued_holds.clear();
        a.queued.clear();
        return;
    }
    for act in std::mem::take(&mut a.queued_holds) {
        a.held.insert(act);
    }
    for act in std::mem::take(&mut a.queued) {
        a.held.insert(act.clone());
        a.pressed.insert(act);
    }
    for (input, acts) in &a.binds {
        let (held, pressed) = match *input {
            // Either side of a modifier counts, as in the game.
            Input::Key(KeyCode::ShiftLeft) => (keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]), keys.any_just_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight])),
            Input::Key(KeyCode::ControlLeft) => (keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]), keys.any_just_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])),
            Input::Key(KeyCode::AltLeft) => (keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]), keys.any_just_pressed([KeyCode::AltLeft, KeyCode::AltRight])),
            Input::Key(k) => (keys.pressed(k), keys.just_pressed(k)),
            Input::Mouse(b) => (mouse.pressed(b), mouse.just_pressed(b)),
            Input::WheelUp => (wheel.delta.y > 0.0, wheel.delta.y > 0.0),
            Input::WheelDown => (wheel.delta.y < 0.0, wheel.delta.y < 0.0),
        };
        for act in acts {
            if held {
                a.held.insert(act.clone());
            }
            if pressed {
                a.pressed.insert(act.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bindset() {
        let cfg = "bindset 0\nunbindall\nbind \"w\" \"_moveforward\"\nbind \"MOUSE2\" \"_zoom _altfire\"\nbind F12 screenshot\nbindset 1\nbind \"w\" \"_other\"\n";
        let a = Actions::from_cfg(cfg, 0);
        assert_eq!(a.binds.get(&Input::Key(KeyCode::KeyW)), Some(&vec!["_moveforward".to_string()]));
        assert_eq!(a.binds.get(&Input::Mouse(MouseButton::Right)).map(Vec::len), Some(2));
        assert!(!a.binds.contains_key(&Input::Key(KeyCode::F12)));
    }

    #[test]
    fn rebinds_round_trip() {
        let cfg = "bindset 0\nbind \"w\" \"_moveforward\"\nbind \"SHIFT\" \"_walk\"\n";
        let mut a = Actions::from_cfg(cfg, 0);
        a.bind(Input::Key(KeyCode::ArrowUp), "_moveforward");
        a.unbind(Input::Key(KeyCode::KeyW));
        assert_eq!(a.inputs_for("_moveforward"), vec![Input::Key(KeyCode::ArrowUp)]);
        let saved = a.take_changed_binds().unwrap();
        assert_eq!(saved, vec![("UPARROW".to_string(), "_moveforward".to_string()), ("W".to_string(), String::new())]);
        assert!(a.take_changed_binds().is_none());
        // A fresh start with the saved binds gives the same result; binding a key back to its default drops it.
        let mut b = Actions::from_cfg(cfg, 0);
        b.apply_user_binds(&saved);
        assert_eq!(b.inputs_for("_moveforward"), vec![Input::Key(KeyCode::ArrowUp)]);
        b.bind(Input::Key(KeyCode::KeyW), "_moveforward");
        assert_eq!(b.take_changed_binds().unwrap(), vec![("UPARROW".to_string(), "_moveforward".to_string())]);
        b.reset_binds();
        assert_eq!(b.inputs_for("_moveforward"), vec![Input::Key(KeyCode::KeyW)]);
        assert_eq!(key_name("shift"), Some(Input::Key(KeyCode::ShiftLeft)));
        assert_eq!(input_name(Input::WheelUp), Some("MWHEELUP"));
    }
}
