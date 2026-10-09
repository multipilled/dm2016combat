//! The single-player HUD: drives DOOM's own HUD SWFs the way the game's C++ widgets do.
//!
//! Recovered from the exe (see gamedata/re/SWF.md):
//! - vitals (hud_bottom_left): 0x140c778d0 / 0x140c78300 — `valueText`/`ghostText` = ceil(health) / int(armor),
//!   100-frame `fill`/`overage` bars driven to int(frac * 100) (snap on decrease, play on increase, 0x140c75a80),
//!   armour widget hidden at 0 armour, low-health warning below `hud_health_lowWarning` (0.41).
//! - weapon info (ws_0): 0x140c73810 — `display`/`displayGhost` labels normal/infinite/empty/max, ammo text "%d",
//!   `ammoFill` = int(ammo / max * 100) (100 when the weapon uses no ammo), `weaponIcon.frame.material` = weapon decl
//!   `icon`, icon frame 3 when empty.
//! - reticle: weapon decl `reticle` -> weaponReticle decl `style` -> sprite name from
//!   `weaponreticleswfinfo/base` (`reticleSWFInfoList`); screens roll on with `rollOnBack`.
//! - weapon mods (ws_0 `weapon_mods`, idMenuWidget_Hud_WeaponMod): 0x140c4c3f0 from idHudInfo +0xe48/+0xe50/+0xe51,
//!   active / inactive `mod_image` = perk iconMaterial, `mod_name` text = perk displayName (0x140bd1e20 / 0x140bd2060).
//! - reticle charge (0x140c52f90): `charging` clip frame, the reticle sprite's chargeStart..chargeEnd (or
//!   extraChargeStart..End with the lock fraction), `chargeRelease`; `discharging` (0x140c53b00).

use std::collections::HashMap;

use anyhow::Result;

use crate::assets::Assets;
use crate::placement::{self, HudLayout, Tag};
use crate::player::{Easing, ObjId, Player, Value};

/// Everything the game feeds the HUD each frame.
#[derive(Debug, Clone, PartialEq)]
pub struct HudState {
    pub visible: bool,
    pub health: f32,
    pub max_health: f32,
    pub armor: f32,
    pub max_armor: f32,
    /// Ammo in the current weapon's pool.
    pub ammo: i32,
    /// Pool capacity; 0 or less means the weapon uses no ammo (pistol, fists): the panel shows "infinite".
    pub max_ammo: i32,
    /// Weapon decl `icon` material (e.g. "textures/guis/icons/weapons/simple/shotgun").
    pub weapon_icon: Option<String>,
    /// Weapon decl `reticle` (e.g. "weaponreticle/sp/shotgun_base"); `None` hides the reticle.
    pub reticle: Option<String>,
    /// BFG cells (0..=3) and chainsaw fuel pips (0..=3) under the ammo panel.
    pub bfg_ammo: u32,
    pub chainsaw_fuel: u32,
    /// idHudReticleInfo::spread: idPlayer::GetSpread of the fire mode / tan(view fov / 2) (0x140e04f3b).
    pub reticle_spread: f32,
    /// The reticle's zoomed spread state (aiming down sights).
    pub reticle_zoomed: bool,
    /// The current ammo decl's lowAmmoWarningCount (0 or less: never low).
    pub low_ammo_count: i32,
    /// Ammo one shot needs (fewer = insufficient ammo).
    pub ammo_per_shot: i32,
    /// The current weapon (idHudInfo +0xe0, the weapon handle): tells a mod switch from a weapon switch.
    pub weapon_id: Option<usize>,
    /// idHudInfo +0xe48 (0x140b8e1d0): the current weapon's active mod, i.e. an active perk whose `item` is the
    /// weapon and that is the base perk of one of its perk families. `None` hides `weapon_mods`.
    pub weapon_mod: Option<HudMod>,
    /// 0x140b8e440: the weapon's first perk family (first perk group, decl order) whose base perk is not active;
    /// shown as `inactive_mod` when owned.
    pub other_mod: Option<HudMod>,
    /// idHudInfo +0xe50 (0x140b8e930): the player owns `other_mod`.
    pub other_mod_owned: bool,
    /// idHudInfo +0xe51: the weapon can take a mod (shows "#str_hud_nomodavailable" while none is active).
    pub mod_slot: bool,
    /// idHudReticleInfo charging (+0x16c): CanCharge (vslot 0x488) ? charge percent (vslot 0x470) : 0.
    pub reticle_charge: f32,
    /// idHudReticleInfo discharging (+0x170): vslot 0x480, 0 for every weapon class.
    pub reticle_discharge: f32,
    /// idHudReticleInfo lock fraction (+0x158): vslot 0x358, the lock-on progress.
    pub reticle_lock: f32,
}

/// A weapon mod as the HUD shows it: its base idDeclPerk's iconMaterial (+0x80) and displayName (+0x70).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HudMod {
    /// Perk decl name (idHudInfo compares the perk pointers).
    pub perk: String,
    pub icon: Option<String>,
    /// `#str_...` key, empty for none.
    pub name: String,
    /// shouldHideOnHud (+0x100).
    pub hide_on_hud: bool,
}

impl Default for HudState {
    fn default() -> Self {
        HudState {
            visible: true,
            health: 100.0,
            max_health: 100.0,
            armor: 0.0,
            max_armor: 50.0,
            ammo: 0,
            max_ammo: 0,
            weapon_icon: Some("textures/guis/icons/weapons/pistol".into()),
            reticle: Some("weaponreticle/sp/pistol_base".into()),
            bfg_ammo: 0,
            chainsaw_fuel: 0,
            reticle_spread: 0.0,
            reticle_zoomed: false,
            low_ammo_count: 0,
            ammo_per_shot: 1,
            weapon_id: None,
            weapon_mod: None,
            other_mod: None,
            other_mod_owned: false,
            mod_slot: false,
            reticle_charge: 0.0,
            reticle_discharge: 0.0,
            reticle_lock: 0.0,
        }
    }
}

/// `hud_health_lowWarning` default.
pub const LOW_HEALTH_WARNING: f32 = 0.41;
/// `hud_reticle_animSpeed`, `hud_reticle_scaler`, `hud_reticle_scalerZoomed` defaults.
pub const RETICLE_ANIM_SPEED: f32 = 160.0;
pub const RETICLE_SCALER: f32 = 0.6;
pub const RETICLE_SCALER_ZOOMED: f32 = 0.01;

/// `hud_disableLowAmmoIndicator` default.
pub const DISABLE_LOW_AMMO_INDICATOR: bool = false;

/// What the HUD uses of an idDeclWeaponReticle (class defaults from its ctor 0x1406f1840).
#[derive(Debug, Clone, PartialEq)]
pub struct ReticleDecl {
    /// Sprite in reticle.bswf (style -> weaponReticleSWFInfo).
    pub sprite: String,
    /// reticleModelScale (1).
    pub model_scale: f32,
    /// spreadFactor (10).
    pub spread_factor: f32,
    /// showSpread (true).
    pub show_spread: bool,
    /// shotsToShow (0).
    pub shots_to_show: i32,
    /// showCharge (+0xcf): bind the `charging` / `discharging` clips.
    pub show_charge: bool,
    /// showChargeRelease (+0xda): bind `chargeRelease` (a child clip, else the reticle sprite's own label).
    pub show_charge_release: bool,
    /// maxChargeFrame (+0xc0, 100).
    pub max_charge_frame: i32,
}

pub struct Hud {
    /// hud_bottom_left: health and armour.
    pub vitals: Player,
    /// ws_0: ammo, weapon icon, BFG/chainsaw pips.
    pub weapon: Player,
    /// hud_bottom: low health warning, power-ups.
    pub bottom: Player,
    /// reticle: crosshairs.
    pub reticle: Player,
    /// hud: the fullscreen HUD movie (2D over the whole view); only its ammo warning is driven here.
    pub screen: Option<Player>,
    prev: Option<HudState>,
    styles: HashMap<String, String>,
    reticle_cache: HashMap<String, Option<ReticleDecl>>,
    current_reticle: Option<String>,
    current_reticle_decl: Option<ReticleDecl>,
    /// Spread widget state (0x140c51c10): last integer spread (+0x208) and the snap flag (+0x25c).
    spread_last: i32,
    spread_force: bool,
    /// HUD tags of the helmet model (placement of every panel, see `placement`).
    pub tags: HashMap<String, Tag>,
    reticle_names: Vec<String>,
    container: std::sync::Arc<idres::Container>,
}

const VITALS: &str = "_root.defaultScreen.info.healthComponents";
const WEAPON: &str = "_root.quickMenu.infoMain.info.equippedWeapon";
/// idMenuScreen_Hud_WeaponInfo children bound in 0x140c47330: +0x260 weapon_mods, +0x220 noweaponmodtext,
/// +0x218 slotName.
const MODS: &str = "_root.quickMenu.infoMain.info.weapon_mods";
const NO_MOD_TEXT: &str = "_root.quickMenu.infoMain.info.noweaponmodtext";
const SLOT_NAME: &str = "_root.quickMenu.infoMain.info.slotName";

impl Hud {
    pub fn load(assets: &Assets) -> Result<Hud> {
        let vitals = assets.player("hud_bottom_left")?;
        let weapon = assets.player("ws_0")?;
        let bottom = assets.player("hud_bottom")?;
        let reticle = assets.player("reticle")?;
        let screen = assets.player("hud").map_err(|e| eprintln!("hud.bswf: {e:#}")).ok();
        let mut styles = HashMap::new();
        if let Ok(b) = assets.container.read_by_name("generated/decls/weaponreticleswfinfo/weaponreticleswfinfo/base.decl") {
            let text = String::from_utf8_lossy(&b);
            let mut style = None;
            for line in text.lines() {
                if let Some(v) = decl_value(line, "reticleStyle") {
                    style = Some(v);
                } else if let Some(v) = decl_value(line, "swfName") {
                    if let Some(s) = style.take() {
                        styles.insert(s, v);
                    }
                }
            }
        }
        let reticle_names = reticle
            .sprite(reticle.root)
            .map(|s| s.display.iter().filter(|d| d.inst.is_some()).map(|d| d.name.to_string()).collect())
            .unwrap_or_default();
        let mut hud = Hud {
            vitals,
            weapon,
            bottom,
            reticle,
            screen,
            prev: None,
            styles,
            reticle_cache: HashMap::new(),
            current_reticle: None,
            current_reticle_decl: None,
            spread_last: 0,
            spread_force: true,
            tags: placement::load_helmet_tags(&assets.container)?,
            reticle_names,
            container: assets.container.clone(),
        };
        hud.roll_on();
        Ok(hud)
    }

    /// Plays every screen's roll-on animation (the engine's idMenuScreen show transition).
    pub fn roll_on(&mut self) {
        show_screen(&mut self.vitals, "_root.defaultScreen");
        self.vitals.set(VITALS, "_visible", true);
        self.vitals.goto_and_play(VITALS, "rollOn");
        show_screen(&mut self.weapon, "_root.quickMenu");
        self.weapon.goto_and_play("_root.quickMenu.infoMain", "rollOn");
        // Challenge / mastery / rune notifications and the challenge progress meter (sub-widgets bound in
        // 0x140c47330) only appear on those events, which the testbed doesn't have.
        for n in ["ChallengeItems", "MasteryItems", "prog_meter"] {
            self.weapon.set(&format!("_root.quickMenu.{n}"), "_visible", false);
        }
        show_screen(&mut self.bottom, "_root.defaultScreen");
        if let Some(p) = self.screen.as_mut() {
            // Only the ammo warning of the fullscreen movie is modelled: hide everything else.
            hide_all_but(p, "_root", "_center");
            hide_all_but(p, "_root._center", "reloadIndicator");
        }
        // Hide every reticle until one is selected.
        for n in self.reticle_names.clone() {
            self.reticle.set(&format!("_root.{n}"), "_visible", false);
        }
        self.current_reticle = None;
    }

    /// Applies `state` and advances all HUD SWFs by `dt` seconds.
    pub fn update(&mut self, state: &HudState, dt: f64) {
        let prev = self.prev.clone();
        self.update_vitals(state, prev.as_ref());
        self.update_weapon(state, prev.as_ref());
        self.update_mods(state, prev.as_ref());
        self.update_bottom(state, prev.as_ref());
        self.update_reticle(state);
        self.update_ammo_warning(state, prev.as_ref());
        self.prev = Some(state.clone());
        for p in [&mut self.vitals, &mut self.weapon, &mut self.bottom, &mut self.reticle] {
            p.update(dt);
        }
        if let Some(p) = self.screen.as_mut() {
            p.update(dt);
        }
    }

    /// Skips all running roll-on animations to their end state (for still renders).
    pub fn settle(&mut self, seconds: f64) {
        let s = self.prev.clone().unwrap_or_default();
        let mut t = 0.0;
        while t < seconds {
            self.update(&s, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
    }

    /// The movies in draw order; "hud" (if loaded) is the fullscreen one.
    pub fn players(&self) -> Vec<(&'static str, &Player)> {
        let mut v = vec![("hud_bottom_left", &self.vitals), ("ws_0", &self.weapon), ("hud_bottom", &self.bottom), ("reticle", &self.reticle)];
        if let Some(p) = &self.screen {
            v.push((SCREEN, p));
        }
        v
    }

    /// The low / no ammo warning (`_center.reloadIndicator` of hud.bswf; 0x140c30320). Flags per frame, as
    /// the game's weaponAmmoInfo_t: lowAmmo (INTERIM: ammo <= the ammo decl's lowAmmoWarningCount; the code
    /// that sets the flag was not found), insufficientAmmo (ammo < ammoPerShot), usesAmmo (pool capacity > 0).
    fn update_ammo_warning(&mut self, s: &HudState, prev: Option<&HudState>) {
        let Some(p) = self.screen.as_mut() else { return };
        let flags = |s: &HudState| {
            let uses = s.max_ammo > 0;
            let low = uses && s.low_ammo_count > 0 && s.ammo <= s.low_ammo_count;
            let insufficient = uses && s.ammo < s.ammo_per_shot.max(1);
            let on = s.visible && uses;
            // reload (no reload or reserve in SP), low, out, insufficient
            let reload = on && low && s.ammo >= 1;
            let low_ammo = on && low && !DISABLE_LOW_AMMO_INDICATOR;
            let out = on && insufficient && s.max_ammo != 0 && !DISABLE_LOW_AMMO_INDICATOR;
            let insuff = on && insufficient;
            [reload, low_ammo, out, insuff]
        };
        let cur = flags(s);
        let was = prev.map(flags).unwrap_or_default();
        let path = "_root._center.reloadIndicator";
        if cur.iter().any(|&b| b) {
            if !was.iter().any(|&b| b) {
                p.goto_and_play(path, "fadeIn");
            }
            let [_, low_ammo, out, insuff] = cur;
            p.goto_and_stop(&format!("{path}.info"), if out || insuff { 3 } else if low_ammo { 2 } else { 1 });
            let text = if out {
                "#str_out_of_ammo"
            } else if low_ammo {
                "#str_low_ammo"
            } else if insuff {
                "#str_insuff_ammo"
            } else {
                "#str_reload_indicator"
            };
            p.set_text(&format!("{path}.info.indicator.txtVal.text"), text);
            p.set_text(&format!("{path}.info.indicator.messageGhost.txtVal"), text);
        } else if was.iter().any(|&b| b) {
            p.goto_and_play(path, "fadeOut");
        }
    }

    fn update_vitals(&mut self, s: &HudState, prev: Option<&HudState>) {
        let p = &mut self.vitals;
        let health_frac = if s.max_health > 0.0 { (s.health / s.max_health).clamp(0.0, 1.0) } else { 0.0 };
        let over_frac = if s.max_health > 0.0 { ((s.health - s.max_health) / s.max_health).clamp(0.0, 1.0) } else { 0.0 };
        let armor_frac = if s.max_armor > 0.0 { (s.armor / s.max_armor).clamp(0.0, 1.0) } else { 0.0 };
        let (ph, po, pa) = match prev {
            Some(q) => (
                if q.max_health > 0.0 { (q.health / q.max_health).clamp(0.0, 1.0) } else { 0.0 },
                if q.max_health > 0.0 { ((q.health - q.max_health) / q.max_health).clamp(0.0, 1.0) } else { 0.0 },
                if q.max_armor > 0.0 { (q.armor / q.max_armor).clamp(0.0, 1.0) } else { 0.0 },
            ),
            None => (health_frac, over_frac, armor_frac),
        };
        let first = prev.is_none();
        let health_text = format!("{}", s.health.ceil() as i32);
        let armor_text = format!("{}", s.armor as i32);
        for t in ["valueText", "ghostText"] {
            p.set_text(&format!("{VITALS}.healthComp.{t}.txtVal"), &health_text);
            p.set_text(&format!("{VITALS}.shieldComp.{t}.txtVal"), &armor_text);
        }
        bar(p, &format!("{VITALS}.healthComp.fill"), health_frac, ph, first);
        bar(p, &format!("{VITALS}.healthComp.overage"), over_frac, po, first);
        bar(p, &format!("{VITALS}.shieldComp.fill"), armor_frac, pa, first);
        bar(p, &format!("{VITALS}.shieldComp.overage"), armor_frac, pa, first);
        p.set(&format!("{VITALS}.shieldComp"), "_visible", s.armor > 0.0);
        let low = health_frac <= LOW_HEALTH_WARNING;
        p.set(&format!("{VITALS}.healthComp.fill.warning"), "_visible", low);
        p.set(&format!("{VITALS}.healthComp.warning"), "_visible", low);
    }

    fn update_weapon(&mut self, s: &HudState, prev: Option<&HudState>) {
        let p = &mut self.weapon;
        let infinite = s.max_ammo <= 0;
        let label = if infinite {
            "infinite"
        } else if s.ammo <= 0 {
            "empty"
        } else if s.ammo >= s.max_ammo {
            "max"
        } else {
            "normal"
        };
        p.goto_and_stop(&format!("{WEAPON}.display"), label);
        p.goto_and_stop(&format!("{WEAPON}.displayGhost"), label);
        let text = format!("{}", s.ammo);
        p.set_text(&format!("{WEAPON}.display.ammo.info.txtVal"), &text);
        p.set_text(&format!("{WEAPON}.displayGhost.ammo.info.txtVal"), &text);
        let fill = if infinite { 100 } else { ((s.ammo as f32 / s.max_ammo as f32) * 100.0) as i32 };
        p.goto_and_stop(&format!("{WEAPON}.ammoFill"), fill.max(1));
        let icon_frame = if !infinite && s.ammo <= 0 { 3 } else { 1 };
        let icon_changed = prev.is_none_or(|q| q.weapon_icon != s.weapon_icon);
        for icon in ["weaponIcon", "weaponIconGhost"] {
            p.goto_and_stop(&format!("{WEAPON}.{icon}"), icon_frame);
            let path = format!("{WEAPON}.{icon}.frame");
            if icon_changed {
                let v = s.weapon_icon.as_deref().map(Value::str).unwrap_or_default();
                p.set(&path, "material", v);
                p.set(&path, "_visible", s.weapon_icon.is_some());
            }
        }
        for i in 0..3u32 {
            let bfg = if i < s.bfg_ammo { "on" } else { "off" };
            p.goto_and_stop(&format!("_root.quickMenu.infoMain.info.bfg_ammo.ammo{i}"), bfg);
            let saw = if i < s.chainsaw_fuel { "on" } else { "off" };
            p.goto_and_stop(&format!("_root.quickMenu.infoMain.info.chainsaw_ammo.ammo{i}"), saw);
        }
    }

    fn update_bottom(&mut self, s: &HudState, prev: Option<&HudState>) {
        let frac = if s.max_health > 0.0 { s.health / s.max_health } else { 1.0 };
        let low = frac <= LOW_HEALTH_WARNING && s.health > 0.0;
        let was_low = prev.is_some_and(|q| q.max_health > 0.0 && q.health / q.max_health <= LOW_HEALTH_WARNING && q.health > 0.0);
        let path = "_root.defaultScreen.info.warning";
        if low && !was_low {
            self.bottom.goto_and_play(path, "rollOn");
        } else if !low && (was_low || prev.is_none()) {
            self.bottom.goto_and_stop(path, 1);
        }
    }

    fn update_reticle(&mut self, s: &HudState) {
        let info = s.reticle.as_ref().and_then(|r| self.reticle_info(r));
        let want = info.as_ref().map(|i| i.sprite.clone());
        self.current_reticle_decl = info;
        if want != self.current_reticle {
            if let Some(old) = self.current_reticle.take() {
                self.reticle.set(&format!("_root.{old}"), "_visible", false);
            }
            if let Some(n) = &want {
                show_screen(&mut self.reticle, &format!("_root.{n}"));
            }
            self.current_reticle = want;
            self.spread_force = true;
        }
        self.update_spread(s.reticle_spread, s.reticle_zoomed);
        let prev = self.prev.as_ref().map_or((0.0, 0.0), |q| (q.reticle_charge, q.reticle_discharge));
        self.update_charge(s.reticle_charge, prev.0, s.reticle_lock);
        self.update_discharge(s.reticle_discharge, prev.1);
    }

    /// The reticle charge widget (0x140c52f90, called from 0x140c307d0 with the idHudReticleInfo charging value,
    /// the previous frame's, the decl's maxChargeFrame and the lock fraction). The `leftCharge` / `rightCharge`
    /// "extra_damage_on" block for one weapon upgrade type (0x140e2ae80 +0x38 == 1) is not modelled.
    fn update_charge(&mut self, charge: f32, prev: f32, lock: f32) {
        let (Some(name), Some(decl)) = (self.current_reticle.clone(), self.current_reticle_decl.clone()) else { return };
        let p = &mut self.reticle;
        let Some(ret) = p.find(&format!("_root.{name}.reticle")) else { return };
        let max = if decl.max_charge_frame > 0 { decl.max_charge_frame } else { 100 };
        let clamp_frame = |v: f32| ((max as f32 * v) as i32).clamp(1, max) as u32;
        if decl.show_charge {
            if let Some(c) = p.child(ret, "charging") {
                goto(p, c, clamp_frame(charge));
            }
        }
        let label = |p: &Player, l: &str| p.find_label(ret, l).map_or(-1, |f| f as i32);
        let (xs, xe) = (label(p, "extraChargeStart"), label(p, "extraChargeEnd"));
        let frame = if xs >= 1 && xe >= 1 && xs < xe && charge == 1.0 && prev == 1.0 && lock > 0.0 && lock < 1.0 {
            Some(((xe - xs) as f32 * lock) as i32 + xs)
        } else {
            let (cs, ce) = (label(p, "chargeStart"), label(p, "chargeEnd"));
            (cs >= 1 && ce >= 1 && cs < ce).then(|| ((ce - cs) as f32 * charge) as i32 + cs)
        };
        if let Some(f) = frame {
            goto(p, ret, f.max(1) as u32);
        }
        if decl.show_charge_release && charge < prev {
            // The `chargeRelease` child clip, else the reticle sprite itself when it has that label.
            let release = p.child(ret, "chargeRelease").or_else(|| p.find_label(ret, "chargeRelease").map(|_| ret));
            if let Some(r) = release {
                if let Some(c) = p.child(r, "charging") {
                    goto(p, c, clamp_frame(prev));
                }
                if let Some(f) = p.find_label(r, "chargeRelease") {
                    p.goto(r, Value::Num(f as f64), true);
                    p.run_actions();
                }
            }
        }
    }

    /// The reticle discharge widget (0x140c53b00) on the `discharging` clip.
    fn update_discharge(&mut self, d: f32, prev: f32) {
        let (Some(name), Some(decl)) = (self.current_reticle.clone(), self.current_reticle_decl.clone()) else { return };
        if !decl.show_charge {
            return;
        }
        let p = &mut self.reticle;
        let Some(ret) = p.find(&format!("_root.{name}.reticle")) else { return };
        let Some(c) = p.child(ret, "discharging") else { return };
        if d <= 0.5 {
            if d <= 0.0 {
                goto(p, c, 1);
            } else if prev <= 0.0 {
                play_label(p, c, "discharging");
            }
        } else if prev < 0.5 {
            play_label(p, c, "dischargingCritical");
            let path = format!("{}.warningText.txtVal", p.target_path(c));
            p.set(&path, "_visible", true);
        }
    }

    /// idMenuScreen_Hud_WeaponInfo's weapon mod update (0x140c4c3f0) and, on a weapon change, the slot block of
    /// 0x140c4a440 that replays the mod name (0x140bd2180).
    fn update_mods(&mut self, s: &HudState, prev: Option<&HudState>) {
        let none = HudState { visible: false, weapon_mod: None, weapon_id: None, other_mod_owned: false, mod_slot: false, ..s.clone() };
        let q = prev.unwrap_or(&none);
        let p = &mut self.weapon;
        let active = format!("{MODS}.active_mod");
        if s.weapon_mod != q.weapon_mod {
            match &s.weapon_mod {
                Some(m) if m.icon.is_some() && !m.hide_on_hud => {
                    if q.weapon_mod.is_none() {
                        widget_show(p, MODS);
                    }
                    if s.weapon_id == q.weapon_id && q.weapon_mod.is_some() {
                        // Mod switch on the same weapon (0x140bd2120): the name rolls on, the panel plays "switch".
                        widget_show(p, &active);
                        play_label_path(p, &format!("{MODS}.mod_name"), "rollOn");
                        set_mod_image(p, &active, m, true);
                        play_label_path(p, MODS, "switch");
                    } else {
                        mods_switch_roll_on(p);
                        set_mod_image(p, &active, m, true);
                    }
                    update_inactive_mod(p, s);
                }
                _ => {
                    set_mod_name(p, "");
                    widget_hide(p, MODS, false);
                }
            }
        } else if !q.visible && s.weapon_mod.is_none() {
            widget_hide(p, MODS, true);
        }
        if s.weapon_mod.is_some() && s.other_mod_owned != q.other_mod_owned {
            update_inactive_mod(p, s);
        }
        // The weapon-change block of 0x140c4a440 (after this update in the exe) replays the mod name.
        if prev.is_some_and(|q| q.weapon_id != s.weapon_id) {
            mods_switch_roll_on(p);
        }
        // noweaponmodtext (+0x220).
        if s.mod_slot {
            if !q.mod_slot && s.weapon_mod.is_none() {
                if let Some(id) = p.find(SLOT_NAME) {
                    let off = p.find_label(id, "rollOff").map_or(-1, |f| f as i32);
                    let cur = p.sprite(id).map_or(0, |x| x.frame) as i32;
                    if cur < off && cur != 1 {
                        p.goto(id, Value::Num(off as f64), true);
                        p.run_actions();
                    }
                }
                p.set_text(&format!("{NO_MOD_TEXT}.txtVal.text"), "#str_hud_nomodavailable");
                p.goto_and_stop(NO_MOD_TEXT, 2);
            }
            return;
        }
        if let Some(id) = p.find(NO_MOD_TEXT) {
            if p.sprite(id).is_some_and(|x| x.frame < 3) {
                play_label(p, id, "rollOff");
            }
        }
    }

    /// The reticle spread widget (0x140c51c10): `<reticle>.spread` (else `zoomedReticleSpread` when zoomed,
    /// else the reticle sprite itself) shows int(spread * spreadFactor); a change tweens the quadrant arms `spread.q<n>.inner/outer` to
    /// (-d, -d), d = value * hud_reticle_scaler, at hud_reticle_animSpeed units per second; an unchanged value
    /// sets the sprite's frame within its spreadStart..spreadEnd labels.
    fn update_spread(&mut self, spread: f32, zoomed: bool) {
        let (Some(name), Some(decl)) = (self.current_reticle.clone(), self.current_reticle_decl.clone()) else { return };
        let p = &mut self.reticle;
        let Some(ret) = p.find(&format!("_root.{name}.reticle")) else { return };
        let spread_sprite = p.child(ret, "spread");
        let target = match spread_sprite {
            Some(sp) => sp,
            None if zoomed => match p.child(ret, "zoomedReticleSpread") {
                Some(z) => z,
                None => return,
            },
            // No spread child: the reticle sprite's own spreadStart..spreadEnd frames show the spread.
            None => ret,
        };
        if decl.shots_to_show >= 1 {
            return;
        }
        let factor = if decl.show_spread { decl.spread_factor } else { 0.0 };
        let value = (spread * factor) as i32;
        if (value == self.spread_last && !self.spread_force) || spread_sprite.is_none() {
            let (a, b) = if zoomed { ("zoomedSpreadStart", "zoomedSpreadEnd") } else { ("spreadStart", "spreadEnd") };
            let start = p.find_label(target, a).map_or(0, |f| f as i32);
            let end = p.find_label(target, b).map_or(0, |f| f as i32);
            let frame = if start < 1 || end < 1 { 1 } else { value.max(0).min(end - start) + start };
            p.goto(target, Value::Num(frame as f64), false);
            p.run_actions();
        } else {
            let delta = (value - self.spread_last).abs() as f32;
            self.spread_last = value;
            let d = (value as f32 * if zoomed { RETICLE_SCALER_ZOOMED } else { RETICLE_SCALER }) as f64;
            // Integer milliseconds, linear easing (0x1417eca60). The arms' `_z` (0, or 3 zoomed) has no effect
            // on the 2D draw and is not modelled.
            let secs = ((1.0 / RETICLE_ANIM_SPEED) * delta * 1000.0) as i32 as f64 / 1000.0;
            for q in 0..4 {
                let Some(quad) = p.child(target, &format!("q{q}")) else { continue };
                let arms: Vec<ObjId> = [p.child(quad, "inner"), p.child(quad, "outer")].into_iter().flatten().collect();
                for arm in arms {
                    if self.spread_force {
                        p.set(&p.target_path(arm), "_x", -d);
                        p.set(&p.target_path(arm), "_y", -d);
                    } else {
                        let path = p.target_path(arm);
                        let x = p.get(&path, "_x");
                        let x = p.to_number(&x);
                        let y = p.get(&path, "_y");
                        let y = p.to_number(&y);
                        p.add_tween(arm, "_x", x, -d, secs, Easing::Linear, 0.0);
                        p.add_tween(arm, "_y", y, -d, secs, Easing::Linear, 0.0);
                    }
                }
            }
        }
        self.spread_force = false;
    }

    /// Sprite name in reticle.bswf for a weaponReticle decl.
    pub fn reticle_sprite(&mut self, decl: &str) -> Option<String> {
        self.reticle_info(decl).map(|i| i.sprite)
    }

    /// The HUD-relevant fields of a weaponReticle decl.
    pub fn reticle_info(&mut self, decl: &str) -> Option<ReticleDecl> {
        if let Some(r) = self.reticle_cache.get(decl) {
            return r.clone();
        }
        let path = format!("generated/decls/weaponreticle/{decl}.decl");
        let text = self.container.read_by_name(&path).ok().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let key = |k: &str| text.lines().find_map(|l| decl_value(l, k));
        let num = |k: &str, d: f32| key(k).and_then(|v| v.parse().ok()).unwrap_or(d);
        let r = key("style").and_then(|st| self.styles.get(&st).cloned()).map(|sprite| ReticleDecl {
            sprite,
            model_scale: num("reticleModelScale", placement::DEFAULT_RETICLE_SCALE),
            spread_factor: num("spreadFactor", 10.0),
            show_spread: key("showSpread").map_or(true, |v| v == "true"),
            shots_to_show: num("shotsToShow", 0.0) as i32,
            show_charge: key("showCharge").is_some_and(|v| v == "true"),
            show_charge_release: key("showChargeRelease").is_some_and(|v| v == "true"),
            max_charge_frame: num("maxChargeFrame", 100.0) as i32,
        });
        self.reticle_cache.insert(decl.to_string(), r.clone());
        r
    }

    /// Where every movie sits on a `width` x `height` window; `view_fov` is the view's field of view
    /// (g_fov style, horizontal at 16:9) used by the reticle.
    pub fn layout(&self, width: f32, height: f32, view_fov: f32) -> HudLayout {
        let scale = self.current_reticle_decl.as_ref().map_or(placement::DEFAULT_RETICLE_SCALE, |d| d.model_scale);
        HudLayout::new(&self.tags, width, height, view_fov, scale)
    }

    /// The helmet's HUD tags (marine_helmet.md6 prop "_info").
    pub fn tags(&self) -> &HashMap<String, placement::Tag> {
        &self.tags
    }

    pub fn current_reticle(&self) -> Option<&str> {
        self.current_reticle.as_deref()
    }
}

/// `key = "value";` inside a decl `edit` block.
fn decl_value(line: &str, key: &str) -> Option<String> {
    let l = line.trim();
    let rest = l.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    Some(rest.trim_end_matches(';').trim().trim_matches('"').to_string())
}

/// Hides every named child of `parent` except `keep`.
fn hide_all_but(p: &mut Player, parent: &str, keep: &str) {
    let Some(id) = p.find(parent) else { return };
    let names: Vec<String> = p
        .sprite(id)
        .map(|s| s.display.iter().filter(|d| d.inst.is_some()).map(|d| d.name.to_string()).collect())
        .unwrap_or_default();
    for n in names {
        if n != keep {
            p.set(&format!("{parent}.{n}"), "_visible", false);
        }
    }
}

/// Name of the fullscreen HUD movie in `Hud::players` (drawn 2D over the whole view, not on a panel).
pub const SCREEN: &str = "hud";

/// idMenuScreen show: make visible and play the "rollOnBack" transition.
fn show_screen(p: &mut Player, path: &str) {
    p.set(path, "_visible", true);
    p.goto_and_play(path, "rollOnBack");
}

/// A 100-frame bar: snap down on decrease, play up on increase (0x140c75a80).
fn bar(p: &mut Player, path: &str, frac: f32, prev: f32, first: bool) {
    let Some(id) = p.find(path) else { return };
    let to = ((frac * 100.0) as i32).max(1) as u32;
    let from = ((prev * 100.0) as i32).max(1) as u32;
    if first || to <= from {
        let cur = p.sprite(id).map(|s| s.frame).unwrap_or(0);
        if first || cur != to {
            goto(p, id, to);
        }
    } else if to != from {
        p.play_range(id, from, to);
    }
}

/// gotoAndPlay(max(label, 1)) (the inlined form in 0x140bd2180 / 0x140bd2120).
fn play_label(p: &mut Player, id: ObjId, label: &str) {
    let f = p.find_label(id, label).unwrap_or(1).max(1);
    p.goto(id, Value::Num(f as f64), true);
    p.run_actions();
}

fn play_label_path(p: &mut Player, path: &str, label: &str) {
    if let Some(id) = p.find(path) {
        play_label(p, id, label);
    }
}

/// idMenuWidget::Show (0x140fb0b90): visible; replay `rollOn` unless already rolling on or idle.
fn widget_show(p: &mut Player, path: &str) {
    let Some(id) = p.find(path) else { return };
    p.set(path, "_visible", true);
    let on = p.find_label(id, "rollOn").map_or(-1, |f| f as i32);
    let idle = p.find_label(id, "idle").map_or(-1, |f| f as i32);
    if on > 0 && (idle > 0 || idle == -1) {
        let cur = p.sprite(id).map_or(0, |x| x.frame) as i32;
        if cur != on && (cur < 2 || idle < cur) {
            p.goto(id, Value::Num(on as f64), true);
            p.run_actions();
        }
    }
}

/// idMenuWidget::Hide (0x140faf440): `immediate` stops on frame 1 and hides; otherwise play `rollOff` (unless
/// already past it or on frame 1), hiding outright when there is no such label.
fn widget_hide(p: &mut Player, path: &str, immediate: bool) {
    let Some(id) = p.find(path) else { return };
    let vis = p.get(path, "_visible");
    if !p.to_bool(&vis) {
        return;
    }
    if immediate {
        goto(p, id, 1);
    } else if let Some(off) = p.find_label(id, "rollOff").filter(|f| *f > 0) {
        let cur = p.sprite(id).map_or(0, |x| x.frame);
        if cur < off && cur != 1 {
            p.goto(id, Value::Num(off as f64), true);
            p.run_actions();
        }
        return;
    }
    p.set(path, "_visible", false);
}

/// 0x140bd2180: `active_mod` shows and the mod name plays "switchRollOn".
fn mods_switch_roll_on(p: &mut Player) {
    widget_show(p, &format!("{MODS}.active_mod"));
    play_label_path(p, &format!("{MODS}.mod_name"), "switchRollOn");
}

/// 0x140bd1e20: a mod widget's `mod_image` gets the perk's iconMaterial (none hides the widget at once); the
/// active one also sets the mod name.
fn set_mod_image(p: &mut Player, widget: &str, m: &HudMod, active: bool) {
    let path = format!("{widget}.mod_image");
    if p.find(&path).is_none() {
        return;
    }
    match &m.icon {
        None => widget_hide(p, widget, true),
        Some(icon) => {
            p.set(&path, "material", Value::str(icon));
            if active {
                set_mod_name(p, &m.name);
            }
        }
    }
}

/// 0x140bd2060: `mod_name.weaponmod_text.mod_name.text` = the displayName (blank for none or the double barrel
/// upgrade placeholder string).
fn set_mod_name(p: &mut Player, name: &str) {
    let text = if name.is_empty() || name == "#str_zion_weapon_double_barrel_upgrade_improvements" { "" } else { name };
    p.set_text(&format!("{MODS}.mod_name.weaponmod_text.mod_name.text"), text);
}

/// 0x140c4a3b0: `inactive_mod` shows the other mod's icon while it is owned.
fn update_inactive_mod(p: &mut Player, s: &HudState) {
    let path = format!("{MODS}.inactive_mod");
    match (&s.other_mod, s.other_mod_owned) {
        (Some(m), true) => {
            widget_show(p, &path);
            set_mod_image(p, &path, m, false);
        }
        _ => widget_hide(p, &path, true),
    }
}

fn goto(p: &mut Player, id: ObjId, frame: u32) {
    if let Some(s) = p.sprite_mut(id) {
        s.playing = false;
        s.stop_at = None;
    }
    p.run_to(id, frame);
    p.run_actions();
}

/// HUD icon material and weaponReticle decl of a weapon decl (`edit.icon`, `edit.reticle`, inheritance resolved).
pub fn weapon_visuals(db: &idres::decldb::DeclDb, weapon_decl: &str) -> (Option<String>, Option<String>) {
    match db.get("weapon", weapon_decl) {
        Ok(b) => (b.str("edit.icon").map(str::to_string), b.str("edit.reticle").map(str::to_string)),
        Err(_) => (None, None),
    }
}

/// The HUD view of a weapon mod's base perk decl (`edit.iconMaterial`, `edit.displayName`, `edit.shouldHideOnHud`).
pub fn perk_visuals(db: &idres::decldb::DeclDb, perk: &str) -> Option<HudMod> {
    let b = db.get("perks", perk).ok()?;
    Some(HudMod {
        perk: perk.to_string(),
        icon: b.str("edit.iconMaterial").filter(|v| *v != "NULL").map(str::to_string),
        name: b.str("edit.displayName").unwrap_or_default().to_string(),
        hide_on_hud: b.str("edit.shouldHideOnHud").is_some_and(|v| v == "true"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The HUD and decls from the user's install; `None` (test skipped) without one.
    fn load() -> Option<(Hud, idres::decldb::DeclDb)> {
        let dir = idres::find_install()?;
        let assets = Assets::open(&dir).ok()?;
        let db = idres::decldb::DeclDb::new(assets.container.clone());
        Some((Hud::load(&assets).ok()?, db))
    }

    fn frame(p: &Player, path: &str) -> u32 {
        p.find(path).and_then(|id| p.sprite(id)).map_or(0, |s| s.frame)
    }

    fn text(p: &Player, path: &str) -> String {
        p.find(path).and_then(|id| p.text(id)).map(|t| t.text.clone()).unwrap_or_default()
    }

    #[test]
    fn mod_widget_shows_switches_and_reports_no_mod() {
        let Some((mut hud, db)) = load() else { return };
        let pop = perk_visuals(&db, "perk/zion/player/sp/weapons/shotgun/pop_rocket").unwrap();
        let burst = perk_visuals(&db, "perk/zion/player/sp/weapons/shotgun/secondary_charge_burst").unwrap();
        assert_eq!(pop.icon.as_deref(), Some("textures/guis/icons/perks/weapons/shotgun/pop_rocket"));
        let mut s = HudState { weapon_id: Some(2), weapon_mod: Some(pop.clone()), other_mod: Some(burst.clone()), other_mod_owned: true, mod_slot: true, ..Default::default() };
        hud.update(&s, 1.0 / 60.0);
        let p = &hud.weapon;
        let image = p.find(&format!("{MODS}.active_mod.mod_image")).and_then(|id| p.sprite(id)).and_then(|x| x.material.clone());
        assert_eq!(image.as_deref(), Some("textures/guis/icons/perks/weapons/shotgun/pop_rocket"));
        assert_eq!(text(p, &format!("{MODS}.mod_name.weaponmod_text.mod_name.text")), "#str_zion_weapon_shotgun_upgrade_pop_rocket");
        // A mod switch on the same weapon plays weapon_mods' "switch" (frame 40 on).
        s.weapon_mod = Some(burst);
        s.other_mod = Some(pop);
        hud.update(&s, 1.0 / 60.0);
        assert!(frame(&hud.weapon, MODS) >= 40, "weapon_mods frame {}", frame(&hud.weapon, MODS));
        assert_eq!(text(&hud.weapon, &format!("{MODS}.mod_name.weaponmod_text.mod_name.text")), "#str_zion_weapon_shotgun_upgrade_secondary_charge_burst");
        // A weapon without mods: weapon_mods rolls off.
        s = HudState { weapon_id: Some(0), ..Default::default() };
        hud.update(&s, 1.0 / 60.0);
        assert!(frame(&hud.weapon, MODS) >= 25);
        // Then a moddable weapon with none active: +0xe51 turns on, "NO MOD EQUIPPED" stops on frame 2.
        s = HudState { weapon_id: Some(7), mod_slot: true, ..Default::default() };
        hud.update(&s, 1.0 / 60.0);
        assert_eq!(frame(&hud.weapon, NO_MOD_TEXT), 2);
        assert_eq!(text(&hud.weapon, &format!("{NO_MOD_TEXT}.txtVal.text")), "#str_hud_nomodavailable");
    }

    #[test]
    fn reticle_charge_frames_follow_charge_then_lock() {
        let Some((mut hud, _)) = load() else { return };
        // The RL lock-on reticle (reticle5): chargeStart 101..chargeEnd 145, extraChargeStart 185..extraChargeEnd 229.
        let mut s = HudState { reticle: Some("weaponreticle/sp/rocket_launcher_lockon_mod".into()), reticle_charge: 0.5, ..Default::default() };
        hud.update(&s, 1.0 / 60.0);
        let name = hud.current_reticle().unwrap().to_string();
        let path = format!("_root.{name}.reticle");
        assert_eq!(frame(&hud.reticle, &path), 101 + (44.0 * 0.5f32) as u32);
        s.reticle_charge = 1.0;
        hud.update(&s, 1.0 / 60.0);
        assert_eq!(frame(&hud.reticle, &path), 145);
        // Fully charged two frames running with a partial lock: the extra-charge frames show the lock fraction.
        s.reticle_lock = 0.5;
        hud.update(&s, 1.0 / 60.0);
        assert_eq!(frame(&hud.reticle, &path), 185 + 22);
    }
}
