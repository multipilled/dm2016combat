//! idPlayer zoom (iron sights), decoded from DOOMx64.exe (notes: gamedata/re/WEAPONS.md "Zoom").
//!
//! - Input (0x140e30310, before UpdateWeapon): BUTTON_ZOOM (0x10) is hold-to-zoom. Not zoomed: held -> zoom
//!   wanted (player+0xcec8, SetZoomWanted 0x140e45200). Zoomed: released -> not wanted.
//! - UpdateZoom 0x140e42960 (every frame) starts with UpdateZoomInput 0x140e45210: the wanted flag is dropped
//!   while airborne and moving without canZoomWhileJumping, for ZOOM_NONE weapons, empty weapons with
//!   forbidZoomWithInsufficientAmmo, and when the hands refuse (0x140d634c0, 0x140d69f90). A change of
//!   "wanted" against the weapon's zoom flag (weapon+0x294) records the zoom start time (player+0xcecc) and
//!   runs SetZoom. UpdateZoom then zooms in (wanted, hands agree, not airborne) or out, and pushes zoomPCT.
//! - SetZoom 0x140e444c0 drives two idInterpolate<float>: the view FOV (player+0xce74) and the zoom percent
//!   (player+0xce90), plays zoomIn/OutSound, and sets the hands' zoomHandsWeaponFovRatio (0x140d69a90).
//!
//! Times are game time (the weapon decl's zoomDelay / zoomTime / zoomBlendTime are used as they are).

use super::arsenal::Arsenal;
use super::decl::WeaponDef;

/// g_blendedZoom_disable.
pub const G_BLENDED_ZOOM_DISABLE: bool = false;
/// g_blendedZoom_fovDelay: blended zooms start the FOV lerp this much later.
pub const G_BLENDED_ZOOM_FOV_DELAY: i32 = 132;
/// g_blendedZoom_nonLinear: zoomPCT = smoothstep of the zoom percent.
pub const G_BLENDED_ZOOM_NON_LINEAR: bool = true;
/// g_fov's reset string ("90"): zoomedFOV scales with g_fov / 90 (0x140f14ff0 divides by atof of it).
pub const G_FOV_RESET: f32 = 90.0;

/// idDeclWeapon::zoomMode_t (decl +0xe30, ctor default ZOOM_NONE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZoomMode {
    #[default]
    None = 0,
    /// The hands web blends a zoom pose by zoomPCT.
    Weapon = 1,
    WeaponNoHandAnim = 2,
}

impl ZoomMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "ZOOM_WEAPON" => ZoomMode::Weapon,
            "ZOOM_WEAPON_NO_HANDANIM" => ZoomMode::WeaponNoHandAnim,
            _ => ZoomMode::None,
        }
    }
}

/// idDeclWeapon::zoomInfo_t ironSightZoom (decl +0xde8; ctor 0x1406f0780 defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct ZoomInfo {
    /// > 0: view FOV when zoomed (else g_fov).
    pub zoomed_fov: f32,
    /// > 0: hands FOV when zoomed (the hands ratio is zoomedHandsFOV / zoomedFOV).
    pub zoomed_hands_fov: f32,
    pub can_zoom_while_jumping: bool,
    pub sensitivity_scale_controller: f32,
    pub sensitivity_scale_mouse: f32,
    pub zoom_delay: i32,
    pub zoom_time: i32,
    pub has_blended_zoom: bool,
    pub zoom_blend_time: i32,
    pub hide_hands_on_zoom: bool,
    pub hide_hands_on_zoom_delay: i32,
}

impl Default for ZoomInfo {
    fn default() -> Self {
        Self {
            zoomed_fov: 0.0,
            zoomed_hands_fov: 0.0,
            can_zoom_while_jumping: false,
            sensitivity_scale_controller: 1.0,
            sensitivity_scale_mouse: 1.0,
            zoom_delay: 0,
            zoom_time: 0,
            has_blended_zoom: false,
            zoom_blend_time: 0,
            hide_hands_on_zoom: false,
            hide_hands_on_zoom_delay: 0,
        }
    }
}

/// idInterpolate<float> (GetCurrentValue 0x140362990).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interp {
    pub start: i32,
    pub duration: i32,
    pub from: f32,
    pub to: f32,
}

impl Interp {
    pub fn constant(v: f32) -> Self {
        Self { start: 0, duration: 0, from: v, to: v }
    }

    pub fn new(start: i32, duration: i32, from: f32, to: f32) -> Self {
        Self { start, duration, from, to }
    }

    pub fn value(&self, t: i32) -> f32 {
        let e = t.wrapping_sub(self.start);
        let d = self.duration;
        if d >= 0 {
            if e <= 0 {
                return self.from;
            }
            if e >= d {
                return self.to;
            }
        } else {
            if e >= 0 {
                return self.from;
            }
            if e <= d {
                return self.to;
            }
        }
        let inv = if d == 0 { 0.0 } else { 1.0 / d as f32 };
        let f = e as f32 * inv;
        let flush = |x: f32| if x.abs() <= 1e-18 { 0.0 } else { x };
        (1.0 - f) * flush(self.from) + f * flush(self.to)
    }
}

/// One frame of zoom input.
#[derive(Debug, Clone, Copy)]
pub struct ZoomInput {
    /// BUTTON_ZOOM held.
    pub button: bool,
    /// g_fov.
    pub base_fov: f32,
    /// Player physics: velocity != 0, movementFlags JUMPED (0x2) / DOUBLE_JUMPED (0x200) this frame, on the
    /// ground (physics vfunc +0x1e0).
    pub moving: bool,
    pub jumped: bool,
    pub double_jumped: bool,
    pub on_ground: bool,
    /// idHands gates: 0x140d634c0 (`Hands::zoom_state_ok`) and 0x140d69f90 (`Hands::zoom_flags_ok`).
    pub hands_state_ok: bool,
    pub hands_flags_ok: bool,
    /// 0x140d63390: the hands are hidden (`Hands::hidden`).
    pub hands_hidden: bool,
}

/// Zoom state of the player (and the zoom-related idHands members).
#[derive(Debug, Clone)]
pub struct Zoom {
    /// player+0xcec8: zoom wanted.
    pub wanted: bool,
    /// player+0xce46 & 2 (IsZoomed 0x140e42290).
    pub zoomed: bool,
    /// weapon+0x294 & 1 per weapon (vfunc +0x4f0).
    pub weapon_zoomed: Vec<bool>,
    /// player+0xcecc: zoom start time.
    pub start: i32,
    /// player+0xce74: view FOV.
    pub fov: Interp,
    /// player+0xce90: zoom percent 0..1.
    pub pct: Interp,
    /// hands+0x10358: zoomHandsWeaponFovRatio.
    pub hands_ratio: Interp,
    /// Hands hide flag 2 (hands+0x5ec8, 0x140d60c60 / 0x140d69ff0).
    pub hide_hands: bool,
    /// zoomPCT pushed to the hands this frame (UpdateZoom).
    pub zoom_pct: f32,
}

impl Zoom {
    /// Player spawn (0x140e432c0): FOV interp at g_fov, zoom percent 0.
    pub fn new(num_weapons: usize, base_fov: f32) -> Self {
        Self {
            wanted: false,
            zoomed: false,
            weapon_zoomed: vec![false; num_weapons],
            start: 0,
            fov: Interp::constant(base_fov),
            pct: Interp::constant(0.0),
            hands_ratio: Interp::constant(0.0),
            hide_hands: false,
            zoom_pct: 0.0,
        }
    }

    /// GetZoomMode 0x140f14ed0 (no per-mode / ammo overrides without mods).
    pub fn mode(def: &WeaponDef) -> ZoomMode {
        def.zoom_mode
    }

    /// 0x140f14ff0: the zoomed view FOV, scaled by g_fov / 90.
    pub fn zoomed_fov(def: &WeaponDef, base_fov: f32) -> f32 {
        let f = if def.zoom.zoomed_fov > 0.0 { def.zoom.zoomed_fov } else { base_fov };
        (base_fov / G_FOV_RESET) * f
    }

    /// 0x140f14df0: zoomDelay, plus g_blendedZoom_fovDelay for blended zooms.
    pub fn start_delay(def: &WeaponDef) -> i32 {
        let blend = if !G_BLENDED_ZOOM_DISABLE && def.zoom.has_blended_zoom { G_BLENDED_ZOOM_FOV_DELAY } else { 0 };
        def.zoom.zoom_delay + blend
    }

    /// GetZoomFraction 0x140e416c0: how far the view FOV is from g_fov towards the zoomed FOV.
    pub fn fraction(&self, def: &WeaponDef, now: i32, base_fov: f32) -> f32 {
        if Self::mode(def) == ZoomMode::None {
            return 0.0;
        }
        let zf = Self::zoomed_fov(def, base_fov);
        if zf == base_fov {
            return 0.0;
        }
        ((self.fov.value(now) - base_fov) / (zf - base_fov)).clamp(0.0, 1.0)
    }

    /// The view FOV of the zoom (0x140e3b580 without the other FOV offsets), clamped to [1, 179].
    pub fn view_fov(&self, now: i32) -> f32 {
        self.fov.value(now).clamp(1.0, 179.0)
    }

    /// Mouse sensitivity scale of the zoom (decl sensitivity_scale_mouse while zoomed). How the game applies
    /// it to the view input is not decoded.
    pub fn sensitivity_scale_mouse(&self, def: &WeaponDef) -> f32 {
        if self.zoomed { def.zoom.sensitivity_scale_mouse } else { 1.0 }
    }

    /// One frame: the button (0x140e30310), then UpdateZoom 0x140e42960. Returns sound events to post
    /// (zoomInSound / zoomOutSound).
    pub fn update(&mut self, arsenal: &Arsenal, now: i32, inp: &ZoomInput) -> Vec<String> {
        let mut sounds = Vec::new();
        let w = arsenal.current;
        let def = arsenal.zoom_def(w);
        if self.weapon_zoomed.len() < arsenal.defs.len() {
            self.weapon_zoomed.resize(arsenal.defs.len(), false);
        }
        // 0x140e30310 (hands+0x1032c zoom hold and the blocking states 0x27d69 / 0x140e2db10 not ported).
        if !self.zoomed {
            if inp.button {
                self.wanted = true;
            }
        } else if !inp.button {
            self.wanted = false;
        }

        // UpdateZoomInput 0x140e45210.
        let airborne = inp.moving && !def.zoom.can_zoom_while_jumping && (inp.jumped || inp.double_jumped || !inp.on_ground);
        if airborne {
            self.wanted = false;
        } else {
            if def.forbid_zoom_with_insufficient_ammo && arsenal.is_empty(w) {
                self.wanted = false;
            }
            // forbidZoomIfCannotCharge (+0xe35) && !CanCharge (MODS.md 3c): no scope during a charge timeout.
            if def.forbid_zoom_if_cannot_charge && !arsenal.can_charge(w, now) {
                self.wanted = false;
            }
            if Self::mode(&def) == ZoomMode::None || !inp.hands_state_ok || !inp.hands_flags_ok {
                self.wanted = false;
            }
            if self.wanted != self.weapon_zoomed[w] {
                self.start = now;
                self.weapon_zoomed[w] = self.wanted;
                let on = self.wanted;
                self.set_zoom(arsenal, now, on, inp.base_fov, &mut sounds);
            }
        }

        // UpdateZoom proper.
        let mut want = self.wanted && inp.hands_state_ok;
        if Self::mode(&def) == ZoomMode::None {
            want = false;
        }
        if airborne {
            want = false;
        }
        if want {
            if !self.zoomed {
                self.set_zoom(arsenal, now, true, inp.base_fov, &mut sounds);
            }
        } else if self.zoomed {
            self.set_zoom(arsenal, now, false, inp.base_fov, &mut sounds);
        }
        let f = self.pct.value(now);
        self.zoom_pct = 0.0;
        if Self::mode(&def) == ZoomMode::Weapon {
            self.zoom_pct = if G_BLENDED_ZOOM_NON_LINEAR { f * f * 3.0 - (f * f * f + f * f * f) } else { f };
        }
        let delay = def.zoom.hide_hands_on_zoom_delay;
        if self.zoomed && 0 < delay && self.start + delay < now && !inp.hands_hidden {
            self.hide_hands = true;
        }
        sounds
    }

    /// SetZoom 0x140e444c0.
    pub fn set_zoom(&mut self, arsenal: &Arsenal, now: i32, on: bool, base_fov: f32, sounds: &mut Vec<String>) {
        let w = arsenal.current;
        let zd = arsenal.zoom_def(w);
        let def = &*zd;
        if self.zoomed == on {
            return;
        }
        if Self::mode(def) == ZoomMode::None {
            // 0x140e431b0: reset (0x140e432c0) and unzoom at once, then fall through to the zoom-out below.
            self.fov = Interp::constant(base_fov);
            self.pct = Interp::constant(0.0);
            self.zoomed = false;
            self.wanted = false;
            self.start = now;
            self.weapon_zoomed[w] = false;
            self.hide_hands = false;
        }
        let zf = Self::zoomed_fov(def, base_fov);
        if !on {
            self.wanted = false;
            self.zoomed = false;
            self.weapon_zoomed[w] = false;
            self.hide_hands = false;
            if 0.0001 <= base_fov - zf {
                let cur = self.view_fov(now);
                let dur = (def.zoom.zoom_time as f32 * ((base_fov - cur) / (base_fov - zf))) as i32;
                self.fov = Interp::new(now, dur, cur, base_fov);
            } else {
                self.fov = Interp::new(now, def.zoom.zoom_time, zf, base_fov);
            }
            let f = self.pct.value(now);
            self.pct = Interp::new(now, (f * def.zoom.zoom_blend_time as f32) as i32, f, 0.0);
            if !def.zoom_out_sound.is_empty() {
                sounds.push(def.zoom_out_sound.clone());
            }
        } else {
            let cur = self.fov.value(now);
            self.fov = Interp::new(now + Self::start_delay(def), def.zoom.zoom_time, cur, zf);
            let f = self.pct.value(now);
            self.pct = Interp::new(now, (def.zoom.zoom_blend_time as f32 * (1.0 - f)) as i32, f, 1.0);
            self.zoomed = true;
            self.weapon_zoomed[w] = true;
            if def.zoom.hide_hands_on_zoom {
                self.hide_hands = true;
            }
            if !def.zoom_in_sound.is_empty() {
                sounds.push(def.zoom_in_sound.clone());
            }
        }
        // 0x140d69a90: zoomHandsWeaponFovRatio towards zoomedHandsFOV / zoomedFOV (else handsFovScale).
        let target = if 0.0 < def.zoom.zoomed_hands_fov { def.zoom.zoomed_hands_fov / def.zoom.zoomed_fov } else { def.hands_fov_scale };
        let cur = self.hands_ratio.value(now);
        self.hands_ratio = Interp::new(now + Self::start_delay(def), def.zoom.zoom_time, cur, target);
    }
}
