//! The mod switch ("perk switcher", idPlayer 0x140e456a0, every player think; gamedata/re/MODS.md 3d).
//!
//! WEAP_RELOAD (key R, "_reload") just pressed picks the next owned mod family; 50 ticks later the hands hide
//! (GENERIC_HIDE_SLOW), 300 ticks after that the mod swaps and the hands come back through
//! generic_unhide_mod_select with the family's unhideWeaponModSelect slot. All times are game ticks.

use super::arsenal::Arsenal;
use super::hands::{Hands, HandsAction};

/// perkSwitcher_selectLagTime, perkSwitcher_showHandsTime, perkSwitcher_nextSwitchPerkTime (cvar defaults).
pub const PERK_SWITCHER_SELECT_LAG_TIME: i32 = 50;
pub const PERK_SWITCHER_SHOW_HANDS_TIME: i32 = 300;
pub const PERK_SWITCHER_NEXT_SWITCH_PERK_TIME: i32 = 100;

/// Player members of the perk switcher.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModSwitch {
    /// player+0xc410 select deadline (0 = none), +0xc414 show deadline, +0xc418 next switch time.
    pub select_deadline: i32,
    pub show_deadline: i32,
    pub next_switch: i32,
    /// player+0xc420 target family.
    pub target: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModSwitchEvent {
    /// The hands start hiding for the switch.
    Hide { weapon: usize },
    /// The mod changed to `family` (the hands unhide through the mod-select anim).
    Switched { weapon: usize, family: usize },
}

impl ModSwitch {
    /// One player think. `reload_pressed`: _reload just pressed; `busy`: the player may not switch
    /// (0x1407bdf50 and friends; the caller decides).
    pub fn update(&mut self, arsenal: &mut Arsenal, hands: &mut Hands, now: i32, reload_pressed: bool, busy: bool) -> Option<ModSwitchEvent> {
        let w = arsenal.current;
        let d = arsenal.defs[w].clone();
        let Some(m) = d.mods.clone() else { return None };
        if d.no_perk_switcher || m.families.is_empty() {
            return None;
        }
        let n = m.families.len();
        let l = arsenal.loadouts[w].clone();
        let active = l.active.map(|a| a.min(n - 1));
        if reload_pressed && now > self.next_switch && !busy {
            self.target = match active {
                Some(a) => Some((a + 1) % n),
                None => l.owned.iter().position(|o| *o),
            };
            self.select_deadline = now + PERK_SWITCHER_SELECT_LAG_TIME;
        }
        let t = self.target?;
        if t >= n || !l.owned.get(t).copied().unwrap_or(false) || active == Some(t) {
            return None;
        }
        if self.select_deadline != 0 && now > self.select_deadline {
            hands.request_generic(HandsAction::GenericHideSlow, now);
            self.select_deadline = 0;
            self.show_deadline = now + PERK_SWITCHER_SHOW_HANDS_TIME;
            self.next_switch = now + PERK_SWITCHER_NEXT_SWITCH_PERK_TIME;
            return Some(ModSwitchEvent::Hide { weapon: w });
        }
        if self.show_deadline != 0 && now > self.show_deadline {
            hands.request_generic(HandsAction::GenericUnhide, now);
            self.show_deadline = 0;
            // Old mod removed, the new family's base perk activated, the weapon flags the mod-select anim
            // (0x140f1b6a0, weapon+0x1bf8).
            let mut l = l;
            l.active = Some(t);
            arsenal.set_loadout(w, l);
            arsenal.mod_select_pending[w] = true;
            return Some(ModSwitchEvent::Switched { weapon: w, family: t });
        }
        None
    }
}

impl Arsenal {
    /// 0x140f148e0: the active family's unhideWeaponModSelect while the weapon's mod-select flag is set.
    pub fn mod_select_slot(&self, w: usize) -> i32 {
        if !self.mod_select_pending[w] {
            return 0;
        }
        let (Some(m), Some(a)) = (self.defs[w].mods.as_ref(), self.loadouts[w].active) else { return 0 };
        m.families.get(a).map(|f| f.unhide_slot).unwrap_or(0)
    }
}
