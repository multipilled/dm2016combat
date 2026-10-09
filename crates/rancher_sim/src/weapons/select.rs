//! idPlayer weapon selection: the `_weap0`..`_weap9`, `_weapnext`, `_weapprev` and `_changeWeapon` buttons,
//! decoded from DOOMx64.exe (notes: gamedata/re/WEAPONS.md "Weapon selection").
//!
//! - `_weapN` (BUTTON_WEAP_N = 0x400 << N, handled in 0x140e30310 before UpdateWeapon): select weapon
//!   selection group N (SelectWeaponForSelectionGroup 0x140e43d50, SP branch): the group's weapons in
//!   inventory order, rotated so the current one comes first when it is in the group (repeat presses
//!   cycle), the first other selectable one is chosen. No ammo check.
//! - `_weapnext` / `_weapprev` (BUTTON_WEAP_NEXT 0x100 / PREV 0x200, in UpdateWeapon 0x140e45e50):
//!   NextOrPrevWeapon 0x140e422f0 walks the inventory from the current weapon, skipping non-selectable
//!   weapons and the chainsaw, and takes the first one with ammo (0x140f166b0;
//!   g_allowWeaponSwitchToEmpty 0 drops the empty fallback).
//! - `_changeWeapon` (BUTTON_CHANGEWEAPON 0x40): weapon_allowWeaponSwitchWheel 1: released within
//!   weapon_SelectLastWeaponDelay (180) of the press -> last weapon (0x140e40140, quick-swap reserve);
//!   held for weapon_OpenWeaponWheelDelay (180) -> weapon wheel (not ported).
//! - A selection marks the weapon (inventory item +0x40: 0 selected, 1 quick-swap reserve, -1 none;
//!   0x140e42eb0 / 0x140ef4280) and sets the switch time player+0xcd74 = max(player+0x19af, now); the
//!   switch is issued by UpdateWeapon once now >= that time (idPlayer::SelectWeapon 0x140e43810 ->
//!   idHands::SelectWeapon), and player+0x19af = now + g_weaponChangeMinIntervalMS (375).

use super::arsenal::Arsenal;

/// g_weaponChangeMinIntervalMS.
pub const WEAPON_CHANGE_MIN_INTERVAL_MS: i32 = 375;
/// weapon_SelectLastWeaponDelay: a `_changeWeapon` tap shorter than this selects the last weapon.
pub const SELECT_LAST_WEAPON_DELAY_MS: i32 = 180;
/// g_allowWeaponSwitchToEmpty.
pub const ALLOW_SWITCH_TO_EMPTY: bool = false;

/// The selection buttons for one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct SelectInput {
    /// `_weap0`..`_weap9` pressed this frame.
    pub weap: [bool; 10],
    pub next: bool,
    pub prev: bool,
    /// `_changeWeapon` held.
    pub change: bool,
}

/// idPlayer's selection state.
#[derive(Debug, Clone)]
pub struct WeaponSelect {
    /// Inventory order (pickup order) as indices into `Arsenal::defs`.
    pub inventory: Vec<usize>,
    /// Per weapon (def index): the inventory item's selection marker (+0x40).
    pub marker: Vec<i32>,
    /// player+0xcd74: when the marked switch may be issued (0 = none pending).
    pub pending_at: i32,
    /// player+0x19af: last issued switch + g_weaponChangeMinIntervalMS.
    pub next_allowed: i32,
    /// player+0xcdfc: `_changeWeapon` press time.
    change_pressed_at: i32,
    change_last: bool,
    /// The weapon the markers were last synced to.
    current: usize,
}

impl WeaponSelect {
    /// Every weapon of the arsenal owned, in `defs` order (SP_WEAPONS = campaign pickup order).
    pub fn new(arsenal: &Arsenal) -> Self {
        let n = arsenal.defs.len();
        let mut marker = vec![-1; n];
        marker[arsenal.current] = 0;
        Self { inventory: (0..n).collect(), marker, pending_at: 0, next_allowed: 0, change_pressed_at: 0, change_last: false, current: arsenal.current }
    }

    /// One frame: group buttons, the deferred switch, then `_changeWeapon` / `_weapprev` / `_weapnext`
    /// (whose selections are issued on a later frame). Returns the weapon to switch to now
    /// (idHands::SelectWeapon), if any.
    pub fn update(&mut self, arsenal: &Arsenal, now: i32, inp: &SelectInput) -> Option<usize> {
        self.sync(arsenal);
        // 0x140e30310: the first pressed group button.
        if let Some(g) = inp.weap.iter().position(|&p| p) {
            self.select_group(arsenal, now, g as i32);
        }
        // UpdateWeapon: issue the marked switch once its time has come.
        let mut issued = None;
        if 0 < self.pending_at && self.pending_at <= now {
            let w = self.marked(0).filter(|&w| w != arsenal.current);
            if let Some(w) = w.filter(|_| !arsenal.def().cant_switch_from) {
                issued = Some(w);
                // arsenal.current changes when the weapon is actually equipped (sync() follows it).
            }
            self.pending_at = 0;
            self.next_allowed = now + WEAPON_CHANGE_MIN_INTERVAL_MS;
        }
        // BUTTON_CHANGEWEAPON pressed: remember the time.
        let pressed = inp.change && !self.change_last;
        let released = !inp.change && self.change_last;
        self.change_last = inp.change;
        if pressed {
            self.change_pressed_at = now;
        }
        if inp.prev {
            self.next_or_prev(arsenal, now, false);
        }
        if inp.next {
            self.next_or_prev(arsenal, now, true);
        }
        // weapon_allowWeaponSwitchWheel 1, weapon_QuickFlipEnable 0: a tap selects the last weapon; a hold
        // of weapon_OpenWeaponWheelDelay opens the wheel (not ported).
        if released && now - self.change_pressed_at < SELECT_LAST_WEAPON_DELAY_MS {
            self.last_weapon(arsenal, now);
        }
        issued
    }

    /// Follow a weapon change made outside this selector (start-up, tests): the new weapon is the
    /// selected one and the old one becomes the reserve as 0x140e42eb0 would make it.
    fn sync(&mut self, arsenal: &Arsenal) {
        let cur = arsenal.current;
        if cur == self.current {
            return;
        }
        let old = self.current;
        self.current = cur;
        if self.marker[cur] != 0 {
            self.mark_selected(arsenal, Some(old), cur);
        }
    }

    fn marked(&self, m: i32) -> Option<usize> {
        self.inventory.iter().copied().find(|&w| self.marker[w] == m)
    }

    /// 0x140e42eb0: mark `w` selected, the current weapon becomes the reserve (weapon_changeCurrentToReserve
    /// 1) when it can be slotted and is remembered for quick swaps.
    fn mark_selected(&mut self, arsenal: &Arsenal, cur: Option<usize>, w: usize) {
        for &v in &self.inventory {
            if v != w && self.marker[v] == 0 && arsenal.defs[v].can_be_slotted {
                self.marker[v] = -1;
            }
        }
        let last = self.marked(1);
        if let Some(c) = cur.filter(|&c| c != w) {
            let d = &arsenal.defs[c];
            let mut m = -1;
            if d.can_be_slotted {
                m = 1;
                if let Some(l) = last {
                    self.marker[l] = -1;
                }
            }
            if !d.can_be_slotted || !d.quick_swap_remember {
                m = -1;
            }
            self.marker[c] = m;
        }
        self.marker[w] = 0;
    }

    /// 0x140e42eb0 tail: the switch may be issued at max(player+0x19af, now).
    fn select(&mut self, arsenal: &Arsenal, now: i32, w: usize) -> bool {
        if w == arsenal.current {
            return false;
        }
        self.mark_selected(arsenal, Some(arsenal.current), w);
        self.pending_at = self.next_allowed.max(now);
        true
    }

    /// SelectWeaponForSelectionGroup 0x140e43d50 (single player).
    pub fn select_group(&mut self, arsenal: &Arsenal, now: i32, group: i32) -> bool {
        let cur = arsenal.current;
        let mut list: Vec<usize> = self.inventory.iter().copied().filter(|&w| arsenal.defs[w].selection_group_index == group).collect();
        if arsenal.defs[cur].selection_group_index == group {
            if let Some(i) = list.iter().position(|&w| w == cur) {
                list.rotate_left(i);
            }
        }
        match list.into_iter().find(|&w| w != cur && arsenal.defs[w].selectable) {
            Some(w) => self.select(arsenal, now, w),
            None => false,
        }
    }

    /// NextOrPrevWeapon 0x140e422f0 (not forcing empty weapons).
    pub fn next_or_prev(&mut self, arsenal: &Arsenal, now: i32, next: bool) -> bool {
        let cur = arsenal.current;
        let n = self.inventory.len() as i32;
        if n == 0 {
            return false;
        }
        let dir = if next { 1 } else { -1 };
        let mut i = self.inventory.iter().position(|&w| w == cur).map(|i| i as i32).unwrap_or(-1);
        let mut fallback = None;
        for _ in 0..n {
            i = (i + dir) % n;
            if i < 0 {
                i += n;
            }
            let w = self.inventory[i as usize];
            let d = &arsenal.defs[w];
            if !d.selectable || d.is_chainsaw() || w == cur {
                continue;
            }
            if arsenal.has_ammo(w) {
                return self.select_by_decl(arsenal, now, w);
            }
            fallback.get_or_insert(w);
        }
        match fallback {
            Some(w) if ALLOW_SWITCH_TO_EMPTY => self.select_by_decl(arsenal, now, w),
            _ => false,
        }
    }

    /// SelectWeaponByDecl 0x140e43ac0: selectable weapons that cannot be slotted switch at once.
    pub fn select_by_decl(&mut self, arsenal: &Arsenal, now: i32, w: usize) -> bool {
        if !arsenal.defs[w].selectable {
            return false;
        }
        if !arsenal.defs[w].can_be_slotted {
            self.mark_selected(arsenal, Some(arsenal.current), w);
            self.pending_at = now;
            return true;
        }
        self.select(arsenal, now, w)
    }

    /// 0x140e40140: swap with the quick-swap reserve (marker 1). Without one the game auto-selects
    /// (0x140e40390, not ported).
    pub fn last_weapon(&mut self, arsenal: &Arsenal, now: i32) -> bool {
        let Some(l) = self.marked(1) else { return false };
        if self.marker[l] == 0 || l == arsenal.current {
            return false;
        }
        if let Some(c) = self.marked(0) {
            let d = &arsenal.defs[c];
            let mut m = -1;
            if d.quick_swap_remember {
                self.marker[l] = -1;
                if d.can_be_slotted {
                    m = 1;
                }
            }
            self.marker[c] = m;
        }
        self.marker[l] = 0;
        self.pending_at = self.next_allowed.max(now);
        true
    }
}
