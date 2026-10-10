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
//! - Auto switch on empty (idPlayer::UpdateWeapon 0x140e46560..0x140e46659, after the deferred switch and before
//!   `_changeWeapon`): g_weaponAutoSwitchOnEmpty (engineCVar_t +0x48 -> idCVar 0x1446523b0, default 1) on and the
//!   current weapon empty -> 0x140e433b0 -> auto-select 0x140e40390 -> SelectWeaponByDecl 0x140e43ac0, so the switch
//!   is issued like any other selection (g_weaponChangeMinIntervalMS).

use super::arsenal::Arsenal;

/// g_weaponChangeMinIntervalMS.
pub const WEAPON_CHANGE_MIN_INTERVAL_MS: i32 = 375;
/// weapon_SelectLastWeaponDelay: a `_changeWeapon` tap shorter than this selects the last weapon.
pub const SELECT_LAST_WEAPON_DELAY_MS: i32 = 180;
/// g_allowWeaponSwitchToEmpty.
pub const ALLOW_SWITCH_TO_EMPTY: bool = false;
/// g_weaponAutoSwitchOnEmpty default ("1"; idCVar 0x1446523b0 registered in 0x140210790, flags 1). The menu's
/// stored value overrides it ([`WeaponSelect::auto_switch_on_empty`]).
pub const AUTO_SWITCH_ON_EMPTY: bool = true;

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
    /// g_weaponAutoSwitchOnEmpty, read every frame by UpdateWeapon (0x140e465ce: engineCVar_t +0x48, value +0x30).
    pub auto_switch_on_empty: bool,
    /// idWeapon+0x1554 emptyWhenSelected per weapon: 0x140f1c320 sets it to IsEmpty (0x140f1ab30) with the
    /// selectedTime (+0x1550); adding ammo clears it (0x140f21700). A weapon picked while empty does not auto switch away.
    empty_when_selected: Vec<bool>,
    /// Each weapon's ammo count last frame (to see ammo being added).
    ammo_last: Vec<Option<i32>>,
    /// A switch issued but not yet equipped (the hands' pending weapon, 0x140e411f0).
    switching: Option<usize>,
}

impl WeaponSelect {
    /// Every weapon of the arsenal owned, in `defs` order (SP_WEAPONS = campaign pickup order).
    pub fn new(arsenal: &Arsenal) -> Self {
        let n = arsenal.defs.len();
        let mut marker = vec![-1; n];
        marker[arsenal.current] = 0;
        Self {
            inventory: (0..n).collect(),
            marker,
            pending_at: 0,
            next_allowed: 0,
            change_pressed_at: 0,
            change_last: false,
            current: arsenal.current,
            auto_switch_on_empty: AUTO_SWITCH_ON_EMPTY,
            empty_when_selected: (0..n).map(|w| w == arsenal.current && arsenal.is_empty(w)).collect(),
            ammo_last: (0..n).map(|w| arsenal.ammo_for(w)).collect(),
            switching: None,
        }
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
                self.switching = Some(w);
                // 0x140f1c320 (idWeapon vfunc +0x270) on the selected weapon. INTERIM: its caller is not traced;
                // it is taken to run when the switch is issued.
                self.empty_when_selected[w] = arsenal.is_empty(w);
                // arsenal.current changes when the weapon is actually equipped (sync() follows it).
            }
            self.pending_at = 0;
            self.next_allowed = now + WEAPON_CHANGE_MIN_INTERVAL_MS;
        }
        if issued.is_none() {
            self.auto_switch_on_empty(arsenal, now);
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
        // 0x140f21700: ammo added to a weapon clears its emptyWhenSelected.
        for w in 0..self.ammo_last.len() {
            let now = arsenal.ammo_for(w);
            if let (Some(a), Some(b)) = (self.ammo_last[w], now) {
                if b > a {
                    self.empty_when_selected[w] = false;
                }
            }
            self.ammo_last[w] = now;
        }
        let cur = arsenal.current;
        if self.switching == Some(cur) {
            self.switching = None;
        }
        if cur == self.current {
            return;
        }
        let old = self.current;
        self.current = cur;
        self.switching = None;
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
    /// The auto switch on empty in idPlayer::UpdateWeapon (0x140e46560..0x140e46659). Returns whether a weapon was
    /// selected (it is issued on a later frame, like any selection).
    ///
    /// INTERIM: the "busy" gate (r12b, 0x140e45fea..0x140e46013: hands action 0xb, hands flags +0x10377 / +0x10374,
    /// player+0x26b28), idWeapon vfunc +0x5b8 and the game's vfunc +0xb70 are not decoded and are taken as passing.
    /// INTERIM: the decl's specialWeapon (+0x640, which forces the switch with the cvar off) and noAmmoCache (+0x4b9)
    /// are not parsed (specialWeapon is a doommp flag per its reflection comment); both are taken as 0.
    pub fn auto_switch_on_empty(&mut self, arsenal: &Arsenal, now: i32) -> bool {
        let cur = arsenal.current;
        if cur >= self.empty_when_selected.len() {
            return false;
        }
        // 0x140f1ad30(weapon, 0): out of ammo for fire mode 0.
        if !arsenal.is_empty(cur) {
            return false;
        }
        // 0x140e465ce: g_weaponAutoSwitchOnEmpty (or the decl's specialWeapon).
        if !self.auto_switch_on_empty {
            return false;
        }
        // 0x140e465e1: idWeapon+0x1554 emptyWhenSelected.
        if self.empty_when_selected[cur] {
            return false;
        }
        // 0x140e465f4..0x140e4661e (noAmmoCache 0): the inventory's ammo item for the decl (0x140eecd10) with a
        // nonzero count (vfunc +0x90) keeps the weapon. INTERIM: the ammo item is taken to be the weapon's pool
        // (the decl match 0x1406f2130 is not decoded).
        if arsenal.ammo_for(cur).is_some_and(|c| c != 0) {
            return false;
        }
        // 0x140e46623: no pending weapon (0x140e411f0).
        if self.switching.is_some() || self.pending_at != 0 {
            return false;
        }
        // 0x140e46659: 0x140e433b0 -> 0x140e40390 -> SelectWeaponByDecl(decl, 0) 0x140e433f5.
        // INTERIM: with nothing to auto-select, 0x140e434bf selects the player's +0xb518 decl (not decoded).
        match self.auto_select(arsenal, None) {
            Some(w) => self.select_by_decl(arsenal, now, w),
            None => false,
        }
    }

    /// The single-player path of the auto-select 0x140e40390 (called from 0x140e433b0 with prefer-other 1).
    /// 1. The quick-swap reserve (the marker in player+0xcd48, flipped 0 <-> 1 when it is the current weapon's,
    ///    0x140e403f7..0x140e4041c; 0x140eed260 finds it) when usable with ammo (0x140e3fb90(.., 1)) and
    ///    autoSelectable (decl +0x63e).
    /// 2. Markers 0 and 1, skipping the current weapon, highest weaponSelectionGroup (decl +0x518, strictly
    ///    greater) (0x140e40510..0x140e405ae).
    /// 3. Every inventory weapon (0x140e405f0..0x140e406f4): canBeSlotted (+0x63c), not the current one, usable
    ///    with ammo and autoSelectable (0x140e3fb20), highest weaponSelectionGroup, the first in inventory order on
    ///    a tie (strict compare at 0x140e4069a).
    ///
    /// The group numbers and flags are the install's decl values ([`super::decl::WeaponDef`]), read at run time.
    /// INTERIM: the campaign order this gives is unchecked: weaponSelectionGroup (+0x518, read at 0x140e40560 /
    /// 0x140e4069a), autoSelectable (+0x63e) and canBeSlotted (+0x63c) live in the decls in gameresources, which
    /// are not on the dev machine; an install-backed check of the order is queued for a machine with the game.
    /// INTERIM: player+0xcd48 is taken as 1 whenever a reserve exists (its spawn value is not decoded).
    /// INTERIM: the player+0xcd68 decl (0x140e4042a, written at 0x140db4d03) is taken as unset.
    /// INTERIM: the last pass (0x140e40717.., game type != 0 via 0x140398320) is taken as off in the campaign.
    pub fn auto_select(&self, arsenal: &Arsenal, exclude: Option<usize>) -> Option<usize> {
        let cur = arsenal.current;
        let defs = &arsenal.defs;
        // 0x140e3fb90(player, weapon, 1): empty weapons fail, else the decl's selectable (+0x63d).
        let usable = |w: usize| arsenal.has_ammo(w) && defs[w].selectable;
        let ok = |w: usize| Some(w) != exclude && usable(w) && defs[w].auto_selectable;
        if let Some(r) = self.marked(1).filter(|&r| r != cur && ok(r)) {
            return Some(r);
        }
        let mut best: Option<usize> = None;
        let mut best_group = -1;
        for m in 0..2 {
            if let Some(w) = self.marked(m).filter(|&w| w != cur && ok(w)) {
                if defs[w].selection_group_index > best_group {
                    best_group = defs[w].selection_group_index;
                    best = Some(w);
                }
            }
        }
        if best.is_some() {
            return best;
        }
        for &w in &self.inventory {
            if w == cur || !defs[w].can_be_slotted || !ok(w) {
                continue;
            }
            if defs[w].selection_group_index > best_group {
                best_group = defs[w].selection_group_index;
                best = Some(w);
            }
        }
        best
    }
}
