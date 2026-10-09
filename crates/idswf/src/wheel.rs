//! The single-player weapon wheel (generated/swf/weapon_select4.bswf), driven like the game's weapon select HUD
//! element (Show 0x140c5b800, Update 0x140c5bcc0, mouse 0x140c5a200, sector pick 0x140c59ac0, Hide 0x140c5a630;
//! notes in gamedata/re/SWF.md, "Weapon wheel").
//!
//! Opening / closing is idPlayer's (UpdateWeapon 0x140e45e50): `_changeWeapon` held for
//! weapon_OpenWeaponWheelDelay opens it, releasing it closes it and selects the focused weapon. While it is open
//! the game timeline slows to swf_weaponSelect_slowTimeScale after swf_weaponSelect_slowTimeDelayMS, and the
//! mouse moves the GUI cursor instead of the view. The game side (Bevy) owns those; this module holds the exe's
//! numbers and the pure pieces: sector pick, dial angle, the time-scale ramp.

/// weapon_allowWeaponSwitchWheel (default 1): a `_changeWeapon` hold may open the wheel.
pub const ALLOW_WHEEL: bool = true;
/// weapon_OpenWeaponWheelDelay (180): `_changeWeapon` held this long (game ms, 0x140e46971) opens the wheel.
pub const OPEN_DELAY_MS: i32 = 180;
/// swf_weaponSelect_slowTime (1): slow the game while the wheel is open.
pub const SLOW_TIME: bool = true;
/// swf_weaponSelect_slowTimeScale (0.14): the game timeline's target scale.
pub const SLOW_TIME_SCALE: f32 = 0.14;
/// swf_weaponSelect_slowTimeDelayMS (192): the slow-down starts this long after the wheel opens (game time,
/// converted to timeline ticks with the timer's rate, 0x140c5bb3a).
pub const SLOW_TIME_DELAY_MS: i32 = 192;
/// Ramp durations of the time-scale blend {0.5, 0, 0.5, scale, 0, 0} passed to idGameTimeManagerLocal 0x140363430
/// (in, seconds) and restored by 0x140363600 (out, seconds).
pub const SLOW_RAMP_IN_S: f32 = 0.5;
pub const SLOW_RAMP_OUT_S: f32 = 0.5;
/// weapon_wheel_deadZone (50): GUI pixels the cursor must be away from the wheel centre (on x or y) to pick.
pub const DEAD_ZONE: f32 = 50.0;
/// The mouse path only picks when game time minus the element's +0x264 stamp exceeds 499 ticks (0x140c5a25a).
/// What sets the stamp is not decoded (never set on the mouse path, so it reads as 0); INTERIM.
pub const MOUSE_HOLDOFF_TICKS: i32 = 499;
/// The 360-frame dial sprite's frame is the pointer angle in whole degrees, clockwise from up (0x140c5a5a0).
pub const DIAL: &str = "_root.selector.info.options0.dial";
/// The movie's screen (idMenuScreen) and the list of eight item buttons.
pub const SCREEN: &str = "_root.selector";
pub const OPTIONS: &str = "_root.selector.info.options0";
/// Placement (0x140beef90): the idPlayerHud tag cached at +0x3c8 (swf_weaponSelect_displayCentered 0; 1 = +0x3e8
/// "weapon_select_front"), which is "weapon_select_vs" with swf_dossier_UseNewWeaponSelect 1, else "weapon_select"
/// (0x140dfdb5e..0x140dfdb8f); idAngles turn with UseNewWeaponSelect 1 (0x140beef90 -> 0x140f8ef50); component scale
/// swf_dossier_WeaponSelectScale (0.2, 0x140beed01). The gamepad stick also tilts it (swf_weaponSelect_maxYaw /
/// maxPitch, 0x140beeb10); the mouse does not. INTERIM: the tag goes through the helmet animator (0x1415ef950 ->
/// 0x1416fc7e0, joint "origin") here; assumed to be the static tag like the other HUD panels.
pub const TAG: &str = "weapon_select_vs";
pub const ANGLES: [f32; 3] = [0.5, 0.5, 0.0];
pub const SCALE: f32 = 0.2;
/// Focused slot art colours (0x142272eb0, 0x142272ea0).
pub const FOCUS: [f32; 4] = [1.0, 0.6, 0.0, 1.0];
pub const FOCUS_EMPTY: [f32; 4] = [0.75, 0.0, 0.0, 1.0];
/// Number of wheel slots (item0 at the top, then clockwise).
pub const SLOTS: usize = 8;

/// The eight corners that bound the sectors (0x140c59b8c: stack vec3s, z = 0), GUI space (x right, y down):
/// sector i lies between corner i and corner i + 1. Cardinal sectors span 53.13 degrees, diagonals 36.87.
pub const CORNERS: [[f32; 2]; SLOTS] = [[-0.5, -1.0], [0.5, -1.0], [1.0, -0.5], [1.0, 0.5], [0.5, 1.0], [-0.5, 1.0], [-1.0, 0.5], [-1.0, -0.5]];

/// 0x140fb21b0: is `d` inside the wedge from `a` to `b` (both edges included)? The exe normalises the three
/// vectors and tests dot(a x d, a x b) >= 0 and dot(d x b, a x b) >= 0 (z = 0, so only the z components count).
fn in_wedge(d: [f32; 2], a: [f32; 2], b: [f32; 2]) -> bool {
    let cross = |u: [f32; 2], v: [f32; 2]| u[0] * v[1] - u[1] * v[0];
    let n = cross(a, b);
    cross(a, d) * n >= 0.0 && cross(d, b) * n >= 0.0
}

/// 0x140c59ac0 with the mouse path's threshold 0: the slot (0..8) whose sector holds direction `d` (GUI space,
/// y down), first match in slot order; `None` for a zero direction.
pub fn pick_sector(d: [f32; 2]) -> Option<usize> {
    if d[0] == 0.0 && d[1] == 0.0 {
        return None;
    }
    (0..SLOTS).find(|&i| in_wedge(d, CORNERS[i], CORNERS[(i + 1) % SLOTS]))
}

/// The dial's frame for direction `d` (normalised, GUI space): acos(-y) in degrees, mirrored for x < 0
/// (0x140c5a540..0x140c5a5a0), truncated.
pub fn dial_degrees(d: [f32; 2]) -> i32 {
    let c = -d[1];
    let mut a = if c <= -1.0 {
        std::f32::consts::PI
    } else if c >= 1.0 {
        0.0
    } else {
        c.acos()
    };
    if d[0] < 0.0 {
        a = (3.1415927 - a) + 3.1415927;
    }
    (a * 57.295776) as i32
}

/// The mouse path's cursor step (0x140c5a200): with the cursor at `cursor` and the wheel's screen rectangle
/// centred on `centre` with half extents `half`, returns the clamped cursor (the game warps the OS cursor there)
/// and the unit direction from the centre, or `None` inside the dead zone.
pub fn mouse_direction(cursor: [f32; 2], centre: [f32; 2], half: [f32; 2]) -> Option<([f32; 2], [f32; 2])> {
    if (cursor[0] - centre[0]).abs() <= DEAD_ZONE && (cursor[1] - centre[1]).abs() <= DEAD_ZONE {
        return None;
    }
    let x = cursor[0].min(centre[0] + half[0]).max(centre[0] - half[0]);
    let y = cursor[1].min(centre[1] + half[1]).max(centre[1] - half[1]);
    let (dx, dy) = (x - centre[0], y - centre[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 0.0 {
        return None;
    }
    Some(([x, y], [dx / len, dy / len]))
}

/// One timeline's linear blend (0x140362990): from `from` at `start` to `to` after `duration` ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ramp {
    pub start: i32,
    pub duration: i32,
    pub from: f32,
    pub to: f32,
}

impl Ramp {
    pub const fn constant(v: f32) -> Ramp {
        Ramp { start: 0, duration: 0, from: v, to: v }
    }

    /// Starts a blend from the current value to `to` over `seconds` (ticks = int(rate * seconds), 0x140362810).
    pub fn blend_to(&mut self, now: i32, to: f32, seconds: f32, ticks_per_second: i32) {
        let from = self.value(now);
        *self = Ramp { start: now, duration: (ticks_per_second as f32 * seconds) as i32, from, to };
    }

    pub fn value(&self, now: i32) -> f32 {
        let t = now - self.start;
        if t <= 0 {
            return self.from;
        }
        if t >= self.duration {
            return self.to;
        }
        let f = t as f32 * (1.0 / self.duration as f32);
        self.from * (1.0 - f) + self.to * f
    }
}

/// What one wheel slot shows (the button data 0x140c5bef0 fills from the player's wheel slot list).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    /// Weapon decl (the selection Hide makes, data +0x298); `None` = an empty slot ("Empty", art off).
    pub weapon: Option<String>,
    /// HUD icon material.
    pub icon: Option<String>,
    /// Localised display name.
    pub name: String,
    /// Ammo count shown under the icon.
    pub count: i32,
    /// The player's current weapon (data +0x2a0).
    pub current: bool,
    /// The player owns it (slot flag +0x1b = player+0x13188 slot +0x2eb, data +0x2a2): the mouse path only focuses such slots.
    pub selectable: bool,
    /// Out of ammo (INTERIM: the item widget's use of the slot flags is not decoded).
    pub empty: bool,
}

/// The weapon_select4 movie driven like the weapon select HUD element.
pub struct Movie {
    pub player: crate::Player,
    pub items: Vec<Item>,
    /// Focused slot (the list's focus index - 1), as the game's list holds it.
    pub focus: Option<usize>,
    /// Focus per item as last shown (None before the first update), to play selecting / unselecting once.
    shown_focus: Option<Option<usize>>,
    pub open: bool,
    /// Dial frame (degrees) and whether it is shown.
    pub dial: Option<i32>,
    /// Centre and half extents of the shown screen (stage units), measured once at load.
    layout: ([f32; 2], [f32; 2]),
}

impl Movie {
    pub fn load(assets: &crate::Assets) -> anyhow::Result<Movie> {
        let player = assets.player("weapon_select4")?;
        let mut m = Movie { player, items: vec![Item::default(); SLOTS], focus: None, shown_focus: None, open: false, dial: None, layout: ([0.0; 2], [0.0; 2]) };
        // The geometry never moves: measure the settled, shown screen once (the game measures it every frame).
        m.show(vec![Item { weapon: Some(String::new()), ..Item::default() }; SLOTS]);
        m.update(1.0);
        m.layout = m.measure();
        m.hide();
        m.player.set(SCREEN, "_visible", false);
        Ok(m)
    }

    /// Show 0x140c5b800: fill the slots, show the screen (idMenuScreen show: "rollOnBack", as the HUD's other
    /// screens), the list at its idle frame with no focus, the dial hidden. INTERIM: the transition argument comes from
    /// the HUD state switch (not traced; on this screen every roll-on is the same fade and every roll-off is instant);
    /// the snap_button_list page widget (0x140c7fee0 / 0x140c80080, MP pages) is left as loaded.
    pub fn show(&mut self, items: Vec<Item>) {
        self.items = items;
        self.items.resize(SLOTS, Item::default());
        self.focus = None;
        self.shown_focus = None;
        self.dial = None;
        self.open = true;
        let p = &mut self.player;
        p.set(SCREEN, "_visible", true);
        p.goto_and_play(SCREEN, "rollOnBack");
        p.goto_and_stop(OPTIONS, "idle");
        self.apply_items();
    }

    /// Hide 0x140c5a630 (idMenuScreen hide: "rollOffFront"). Returns the focused slot's weapon (the selection).
    pub fn hide(&mut self) -> Option<String> {
        self.open = false;
        self.player.goto_and_play(SCREEN, "rollOffFront");
        self.focus.and_then(|i| self.items.get(i)).and_then(|it| it.weapon.clone())
    }

    /// The mouse path's result: focus a slot when it holds a selectable weapon (0x140c5a4a0), and turn the dial.
    pub fn point(&mut self, dir: [f32; 2]) {
        if let Some(i) = pick_sector(dir) {
            if self.items.get(i).is_some_and(|it| it.weapon.is_some() && it.selectable) {
                self.focus = Some(i);
            }
        }
        self.dial = Some(dial_degrees(dir));
    }

    fn apply_items(&mut self) {
        let p = &mut self.player;
        for (i, it) in self.items.iter().enumerate() {
            // art%d: frame 1 ("on") with a weapon, 2 ("off") without (0x140c5c0bb).
            p.goto_and_stop(&format!("{OPTIONS}.art{i}"), if it.weapon.is_some() { 1 } else { 2 });
            let item = format!("{OPTIONS}.item{i}");
            p.set(&format!("{item}.weaponImg"), "_visible", it.weapon.is_some());
            p.set(&format!("{item}.ammoInfo"), "_visible", it.weapon.is_some());
            if let Some(icon) = &it.icon {
                p.set(&format!("{item}.weaponImg.state.frameImg"), "material", crate::Value::str(icon));
            }
            // Slot count -1 = no ammo item (an infinite-ammo weapon); INTERIM: shown as no text.
            let count = if it.count < 0 { String::new() } else { it.count.to_string() };
            p.set_text(&format!("{item}.ammoInfo.count.txtCount"), &count);
        }
    }

    /// Plays focus transitions, the details text and the dial, then advances the movie by `dt` seconds.
    pub fn update(&mut self, dt: f64) {
        if self.open && self.shown_focus != Some(self.focus) {
            let prev = self.shown_focus;
            for i in 0..SLOTS {
                let path = format!("{OPTIONS}.item{i}");
                let has = self.focus == Some(i);
                let label = if self.items[i].weapon.is_none() {
                    "disabled"
                } else {
                    match prev {
                        None => if has { "sel_up" } else { "up" },
                        Some(f) if (f == Some(i)) == has => continue,
                        Some(_) => if has { "selecting" } else { "unselecting" },
                    }
                };
                self.player.goto_and_play(&path, label);
            }
            // Focus event (0x140c5b3c0): the focused slot's art is tinted (idSWFSpriteInstance::SetColor = the display
            // entry's colour multiply) orange, dark red when it is owned, usable and out of ammo; losing focus sets
            // white. INTERIM: the art widget's own focus calls (vslots 0x28 / 0x30) are not decoded.
            for i in 0..SLOTS {
                let it = &self.items[i];
                let tint = match self.focus {
                    Some(f) if f == i && it.empty => FOCUS_EMPTY,
                    Some(f) if f == i => FOCUS,
                    _ => [1.0; 4],
                };
                if let Some(id) = self.player.find(&format!("{OPTIONS}.art{i}")) {
                    if let Some(d) = self.player.display_of_mut(id) {
                        d.cxform.mul = tint;
                    }
                }
            }
            let name = self.focus.map(|i| self.items[i].name.clone()).unwrap_or_default();
            self.player.set_text(&format!("{OPTIONS}.details.nameInfo.selectedName.txtVal"), &name);
            self.shown_focus = Some(self.focus);
        }
        let dial = self.dial;
        self.player.set(DIAL, "_visible", dial.is_some());
        if let Some(d) = dial {
            // gotoAndStop(frame) with the angle in degrees (0x14174fc50 takes the 1-based frame number).
            self.player.goto_and_stop(DIAL, d.max(1));
        }
        self.player.update(dt);
        // After the roll-off the screen is gone.
        if !self.open && self.visible() {
            let id = self.player.find(SCREEN);
            let end = id.and_then(|id| self.player.find_label(id, "rollOnFront"));
            let frame = id.and_then(|id| self.player.sprite(id)).map(|s| s.frame);
            if let (Some(end), Some(frame)) = (end, frame) {
                if frame + 2 >= end {
                    self.player.set(SCREEN, "_visible", false);
                }
            }
        }
    }

    /// The screen is up or rolling off.
    pub fn visible(&self) -> bool {
        self.player.find(SCREEN).and_then(|id| self.player.display_of(id)).is_some_and(|d| d.visible)
    }

    /// The element's centre and half extents in stage units (0x140c5a2a0: the selector sprite's position plus
    /// half its bounds' size).
    pub fn centre_and_half(&self) -> ([f32; 2], [f32; 2]) {
        self.layout
    }

    /// INTERIM: the selector's bounds come from its draw list (the game's 0x141749590 walks the sprite's children).
    fn measure(&self) -> ([f32; 2], [f32; 2]) {
        let (fw, fh) = (self.player.swf.frame_width, self.player.swf.frame_height);
        let Some(id) = self.player.find(SCREEN) else { return ([fw * 0.5, fh * 0.5], [fw * 0.5, fh * 0.5]) };
        let pos = self.player.display_of(id).map(|d| [d.matrix.tx, d.matrix.ty]).unwrap_or([0.0, 0.0]);
        let b = crate::render::sprite_bounds(&self.player, fw, fh, id).unwrap_or([0.0, 0.0, fw, fh]);
        let half = [(b[2] - b[0]) * 0.5, (b[3] - b[1]) * 0.5];
        ([pos[0] + half[0], pos[1] + half[1]], half)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sectors_run_clockwise_from_the_top() {
        assert_eq!(pick_sector([0.0, -1.0]), Some(0));
        assert_eq!(pick_sector([1.0, -1.0]), Some(1));
        assert_eq!(pick_sector([1.0, 0.0]), Some(2));
        assert_eq!(pick_sector([1.0, 1.0]), Some(3));
        assert_eq!(pick_sector([0.0, 1.0]), Some(4));
        assert_eq!(pick_sector([-1.0, 1.0]), Some(5));
        assert_eq!(pick_sector([-1.0, 0.0]), Some(6));
        assert_eq!(pick_sector([-1.0, -1.0]), Some(7));
        // The cardinal sectors are wider: 25 degrees right of up is still the top slot, 30 is not.
        let d = |deg: f32| [deg.to_radians().sin(), -deg.to_radians().cos()];
        assert_eq!(pick_sector(d(25.0)), Some(0));
        assert_eq!(pick_sector(d(30.0)), Some(1));
        assert_eq!(pick_sector([0.0, 0.0]), None);
    }

    #[test]
    fn dial_is_degrees_clockwise_from_up() {
        assert_eq!(dial_degrees([0.0, -1.0]), 0);
        assert_eq!(dial_degrees([1.0, 0.0]), 90);
        assert_eq!(dial_degrees([0.0, 1.0]), 180);
        assert_eq!(dial_degrees([-1.0, 0.0]), 270);
    }

    #[test]
    fn dead_zone_and_clamp() {
        assert_eq!(mouse_direction([110.0, 140.0], [100.0, 100.0], [200.0, 200.0]), None);
        let (c, d) = mouse_direction([500.0, 100.0], [100.0, 100.0], [200.0, 200.0]).unwrap();
        assert_eq!(c, [300.0, 100.0]);
        assert_eq!(d, [1.0, 0.0]);
    }

    #[test]
    fn ramp_is_linear() {
        let mut r = Ramp::constant(1.0);
        r.blend_to(100, 0.14, 0.5, 960);
        assert_eq!(r.duration, 480);
        assert_eq!(r.value(100), 1.0);
        assert!((r.value(340) - 0.57).abs() < 1e-6);
        assert_eq!(r.value(580), 0.14);
    }
}
