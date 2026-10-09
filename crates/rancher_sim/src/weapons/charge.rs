//! Fire modes, the charge state machine and bursts of idWeapon (gamedata/re/MODS.md sections 2-3).
//!
//! Time units: the charge code adds and compares its *MS values with game time directly (960 ticks/s), so
//! chargeTimeMS 500 lasts 500 ticks; firing and burst intervals go through ms_to_ticks (FinishFire).

use std::sync::Arc;

use super::arsenal::Arsenal;
use super::decl::{ChargeInfo, ChargeProperty, WeaponDef};
use super::mods::ModLoadout;

/// CHARGE_STATE_* (reflection enum table 0x1435a0390).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChargeState {
    #[default]
    None = 0,
    Ready = 1,
    Charging = 2,
    FullyCharged = 3,
    Discharging = 4,
    Cooling = 5,
    Suspended = 6,
}

/// BURSTMODE_*.
pub const BURSTMODE_BURST: i32 = 1;
pub const BURSTMODE_FULLAUTO: i32 = 2;
pub const BURSTMODE_SELECTFIRE: i32 = 3;

/// The idWeapon charge members (names from the debug dump 0x140f078b0).
#[derive(Debug, Clone, PartialEq)]
pub struct Charge {
    /// +0x1760.
    pub state: ChargeState,
    /// +0x1768 chargePercent (also the hands' weaponChargeForTriggerEvent).
    pub percent: f32,
    /// +0x1764: the charge at the last charge event (DischargeCharge's "full").
    pub full: f32,
    /// +0x176c dischargePercent.
    pub discharge_percent: f32,
    /// +0x1770 dischargeTime, +0x1774 dischargeTimeoutStartTime, +0x1778 canChargeTime.
    pub discharge_time: i32,
    pub timeout_start: i32,
    pub can_charge_time: i32,
    /// +0x1790 chargeStartTime (-1 = not charging), +0x1794 chargeEndTime.
    pub start_time: i32,
    pub end_time: i32,
    /// +0x17a0 base percent, +0x17ac chargeDuration, +0x17b4 previous percent.
    pub base: f32,
    pub duration: f32,
    pub prev_percent: f32,
    /// chargePerShot: +0x17d0 value, +0x17c8 successes, +0x17cc misses.
    pub per_shot_val: f32,
    pub successes: i32,
    pub misses: i32,
    /// +0x180c chargedBurstShotsFired.
    pub burst_shots_fired: i32,
    /// +0x17a4: the charge percent before the last discharge.
    pub before_discharge: f32,
    /// +0x1824: chargeInfo +0xe9 copied at a timed discharge (taken as timeoutBlocksOtherFireMode): while
    /// cooling no mode may fire.
    pub blocks_other_mode: bool,
    /// +0x17bc "play charge anim": the hands may request HANDSACTION_CHARGE (UpdateWeapon_Default). +0x17b0
    /// the previous charge duration (-1 after a discharge); INTERIM: taken as stored at the end of each
    /// charging update (its writer is not traced; only chargeAnimIntervalMS weapons read it, none in SP).
    pub play_anim: bool,
    pub prev_duration: f32,
}

impl Default for Charge {
    fn default() -> Self {
        Self {
            state: ChargeState::None,
            percent: 0.0,
            full: 0.0,
            discharge_percent: 0.0,
            discharge_time: i32::MIN,
            timeout_start: 0,
            can_charge_time: i32::MIN,
            start_time: -1,
            end_time: i32::MIN,
            base: 0.0,
            duration: 0.0,
            prev_percent: 0.0,
            per_shot_val: 0.0,
            successes: 0,
            misses: 0,
            burst_shots_fired: 0,
            before_discharge: 0.0,
            blocks_other_mode: false,
            play_anim: false,
            prev_duration: -1.0,
        }
    }
}

/// Per-weapon mod / fire-mode runtime (idWeapon members beyond WeaponState).
#[derive(Debug, Clone, PartialEq)]
pub struct ModState {
    /// +0x8d4 current fire mode (0 primary, 1 secondary).
    pub fire_mode: usize,
    /// nextFireTime[1] (+0x918); mode 0's is WeaponState::next_fire (+0x914).
    pub next_fire_alt: i32,
    /// +0x910 burst mode (the mode decl's initialBurstMode).
    pub burst_mode: i32,
    /// +0x93c shots left in the burst.
    pub burst_count: i32,
    pub charge: Charge,
    /// Trigger state per mode (+0x8f8[mode] PULLED) and when it was pulled (+0xdd0[mode]).
    pub trigger_down: [bool; 2],
    pub trigger_time: [i32; 2],
    /// INTERIM press-type secondary trigger: one mode-1 pull is pending.
    pub press_shot: bool,
    /// +0x8e8 overrideFireMode (-1 = none; a lock-on lock pins its mode, weapons::targeting) and +0x8ec the
    /// mode requested meanwhile (restored when the override goes).
    pub override_fire_mode: i32,
    pub override_prev_mode: i32,
}

impl Default for ModState {
    fn default() -> Self {
        Self { fire_mode: 0, next_fire_alt: 0, burst_mode: 0, burst_count: 0, charge: Charge::default(), trigger_down: [false; 2], trigger_time: [0; 2], press_shot: false, override_fire_mode: -1, override_prev_mode: -1 }
    }
}

/// What the charge update reports (sounds to post; FX conditions are not ported).
#[derive(Debug, Clone, PartialEq)]
pub enum ChargeEvent {
    Sound(String),
    /// The charge sound channel stops (percent fell to 0).
    StopSound,
}

impl Arsenal {
    // ---- mods ----

    /// Set a weapon's owned / active mods and re-apply its upgrades (perk component activate /
    /// deactivate + ApplyUpgradeModifier for every modifier).
    pub fn set_loadout(&mut self, w: usize, l: ModLoadout) {
        self.loadouts[w] = l;
        self.rebuild_mods(w);
    }

    /// Testbed: every mod of every weapon owned at `level` (0 = base, 1..3 upgrades, 4 = mastery), with
    /// family `active` (index) active where it exists.
    pub fn unlock_all_mods(&mut self, level: u8, active: Option<usize>) {
        for w in 0..self.defs.len() {
            let Some(m) = self.defs[w].mods.clone() else { continue };
            let mut l = ModLoadout::unlock_all(&m, level);
            l.active = active.filter(|a| *a < m.families.len());
            self.set_loadout(w, l);
        }
    }

    /// Resolve the applied upgrades and each fire mode's decl: decl[mode] (DECL_WEAPON / DECL_AMMO
    /// overrides) else the base decl; mode 1 without an override is the base's secondaryFireDecl.
    pub(crate) fn rebuild_mods(&mut self, w: usize) {
        let base = self.defs[w].clone();
        let Some(m) = base.mods.clone() else {
            self.applied[w] = Default::default();
            self.mode_defs[w] = [Some(base), None];
            return;
        };
        let a = self.loadouts[w].applied(&m);
        let mut defs: [Option<Arc<WeaponDef>>; 2] = [None, None];
        for (mode, d) in defs.iter_mut().enumerate() {
            let s = &a.modes[mode];
            let name = match (&s.decl, mode) {
                (Some(n), _) => n.clone(),
                (None, 0) => base.decl.clone(),
                (None, _) if !base.secondary_fire_decl.is_empty() => base.secondary_fire_decl.clone(),
                _ => continue,
            };
            let ammo = s.ammo.clone().unwrap_or_default();
            *d = m.def(&name, &ammo).or_else(|| m.def(&name, ""));
            if mode == 0 && s.decl.is_none() && s.ammo.is_none() {
                *d = Some(base.clone());
            }
        }
        for d in defs.iter().flatten() {
            self.ensure_pool(d);
        }
        self.applied[w] = a;
        self.mode_defs[w] = defs;
        self.zoom_defs[w] = [self.build_zoom_def(w, 0), self.build_zoom_def(w, 1)];
        // DECL_WEAPON 0x140f22b90: the current mode's burst mode comes from its decl.
        let fm = self.mstate[w].fire_mode;
        self.mstate[w].burst_mode = self.mode_defs[w][fm].as_ref().map(|d| d.initial_burst_mode).unwrap_or(0);
    }

    /// The decl weapons::zoom reads while the weapon is in `mode`: GetZoomMode 0x140f14ed0 (ZOOM_MODE override of
    /// the mode, else the mode decl's zoomMode; the ammo decl's zoomModeOverride is not read: INTERIM),
    /// GetZoomedFOV 0x140f14ff0 / ZoomTime 0x140f14f70 / ZoomDelay 0x140f14df0 (mode 1's zoom info whenever a
    /// secondary decl exists, with the ZOOM_FOV / TIME / DELAY overrides > 0), forbidZoom* of the mode decl.
    fn build_zoom_def(&self, w: usize, mode: usize) -> Option<Arc<WeaponDef>> {
        let md = self.mode_def(w, mode)?;
        let zm = if self.mode_def(w, 1).is_some() { 1 } else { 0 };
        let zd = self.mode_def(w, zm).unwrap_or_else(|| md.clone());
        let mut d = (*md).clone();
        let s = &self.applied[w].modes[zm];
        d.zoom = zd.zoom.clone();
        if s.zoom_fov > 0.0 {
            d.zoom.zoomed_fov = s.zoom_fov;
        }
        if s.zoom_time > 0 {
            d.zoom.zoom_time = s.zoom_time;
        }
        if s.zoom_delay > 0 {
            d.zoom.zoom_delay = s.zoom_delay;
        }
        if let Some(z) = self.applied[w].modes[mode].zoom_mode.filter(|z| *z != super::zoom::ZoomMode::None) {
            d.zoom_mode = z;
        }
        d.zoom_in_sound = zd.zoom_in_sound.clone();
        d.zoom_out_sound = zd.zoom_out_sound.clone();
        d.hands_fov_scale = zd.hands_fov_scale;
        d.mods = None;
        Some(Arc::new(d))
    }

    /// The zoom view of weapon `w` in its current fire mode (weapons::zoom).
    pub fn zoom_def(&self, w: usize) -> Arc<WeaponDef> {
        self.zoom_defs[w][self.mstate[w].fire_mode].clone().unwrap_or_else(|| self.defs[w].clone())
    }

    /// Decl of `mode` (None: the weapon has no such mode, GetDecl(weapon, 1) == 0).
    pub fn mode_def(&self, w: usize, mode: usize) -> Option<Arc<WeaponDef>> {
        self.mode_defs[w][mode.min(1)].clone()
    }

    /// The current weapon's decl for its current fire mode.
    pub fn cur_def(&self) -> Arc<WeaponDef> {
        self.wdef(self.current)
    }

    /// Weapon `w`'s decl for its current fire mode.
    pub fn wdef(&self, w: usize) -> Arc<WeaponDef> {
        self.mode_def(w, self.mstate[w].fire_mode).unwrap_or_else(|| self.defs[w].clone())
    }

    /// GetTriggerMode(weapon, 1) 0x140f14870: the SECONDARY_TRIGGER_MODE override (mode 1's slot +0x1ad4) when
    /// set, even without a mode-1 decl (RL remote detonate), else the mode-1 decl's triggerMode, else 0.
    pub fn secondary_trigger_mode(&self, w: usize) -> i32 {
        if let Some(o) = self.applied[w].weapon.secondary_trigger_mode.filter(|o| *o != 0) {
            return o;
        }
        self.mode_def(w, 1).map(|d| d.hands.trigger_mode).unwrap_or(0)
    }

    /// SetFireMode 0x140d698b0 -> the weapon's fire mode; true when it changed. While a fire-mode override is
    /// set (0x140f21a88) another request is remembered (+0x8ec) and the override mode is kept.
    pub fn set_fire_mode(&mut self, w: usize, mode: usize) -> bool {
        let mut mode = if mode == 1 && self.mode_def(w, 1).is_none() { 0 } else { mode };
        let o = self.mstate[w].override_fire_mode;
        if o != -1 && mode as i32 != o {
            self.mstate[w].override_prev_mode = mode as i32;
            mode = o as usize;
        }
        if self.mstate[w].fire_mode == mode {
            return false;
        }
        self.mstate[w].fire_mode = mode;
        self.mstate[w].burst_mode = self.mode_def(w, mode).map(|d| d.initial_burst_mode).unwrap_or(0);
        // 0x140f21fd4: FIRE_DELAY (+0x1974 + 400 * mode) > 0 -> canAttackTime[mode] = max(it, now + delay);
        // MOVEMENT_DELAY (+0x1978) > 0 -> canMoveTime[mode] likewise (chaingun turret deploy). INTERIM: the
        // gate "entity +0x54 < now" in front of both is taken as passed.
        let now = self.time_ms;
        let (fd, mv) = (self.applied[w].modes[mode].fire_delay, self.applied[w].modes[mode].movement_delay);
        let st = &mut self.states[w];
        if 0 < fd {
            st.fire_delay_until[mode] = st.fire_delay_until[mode].max(now + fd);
        }
        if 0 < mv {
            st.can_move_time[mode] = st.can_move_time[mode].max(now + mv);
        }
        true
    }

    /// The secondary trigger's fire-mode choice (UpdateWeapon_Default's trigger-mode switch on the secondary
    /// decl's trigger mode, then ProcessTriggers 0x140d64a00): returns the mode when it changed.
    /// 7 SECONDARY_HOLD_PRIMARY_PRESS (decoded): altfire held -> mode 1 (fired with the attack button),
    /// released -> mode 0. INTERIM (not decoded): 8 / 9 behave as 7; 3..6 (SECONDARY_PRESS variants with a
    /// mode-1 decl: plasma) take an altfire press as one mode-1 trigger pull (`press_shot`). The RL remote
    /// detonate (SECONDARY_PRESS, no mode-1 decl) goes through AltFirePressed instead (weapons::detonate).
    pub fn alt_trigger(&mut self, w: usize, held: bool, pressed: bool) -> Option<usize> {
        let tm = self.secondary_trigger_mode(w);
        let want = match tm {
            7..=9 => held,
            3..=6 => {
                if pressed {
                    self.mstate[w].press_shot = true;
                }
                self.mstate[w].press_shot
            }
            _ => false,
        };
        let mode = want as usize;
        self.set_fire_mode(w, mode).then_some(self.mstate[w].fire_mode)
    }

    // ---- next fire time per mode ----

    pub fn next_fire(&self, w: usize, mode: usize) -> i32 {
        if mode == 0 { self.states[w].next_fire } else { self.mstate[w].next_fire_alt }
    }

    pub fn set_next_fire(&mut self, w: usize, mode: usize, t: i32) {
        if mode == 0 {
            self.states[w].next_fire = t;
        } else {
            self.mstate[w].next_fire_alt = t;
        }
    }

    // ---- bursts ----

    /// Burst mode: BURST_MODE override (+0x96c) when != -1, else +0x910.
    pub fn burst_mode(&self, w: usize) -> i32 {
        let o = self.applied[w].weapon.burst_mode;
        if o != -1 { o } else { self.mstate[w].burst_mode }
    }

    /// IsBurstMode 0x140c4f6c0: BURST with the mode decl's burstInfo[1].burstCount (+0x590) >= 0.
    pub fn is_burst_mode(&self, w: usize) -> bool {
        self.burst_mode(w) == BURSTMODE_BURST && self.wdef(w).bursts[1].burst_count >= 0
    }

    /// 0x140f16850: SELECTFIRE with burstInfo[3].burstCount (+0x5f0) >= 0 (HAR micro missiles).
    pub fn is_select_fire(&self, w: usize) -> bool {
        self.burst_mode(w) == BURSTMODE_SELECTFIRE && self.wdef(w).bursts[3].burst_count >= 0
    }

    /// InBurst 0x140c4f8d0.
    pub fn in_burst(&self, w: usize) -> bool {
        self.is_burst_mode(w) && self.mstate[w].burst_count > 0
    }

    /// Shots the weapon's ammo allows (0x140f03930: ammo / ammoPerShot of the current mode).
    fn ammo_shots(&self, w: usize) -> i32 {
        let d = self.wdef(w);
        match self.pool_for(&d) {
            None => i32::MAX,
            Some(p) => {
                let per = d.ammo_per_shot.max(1);
                self.pools[p].count / per
            }
        }
    }

    /// GetBurstSize 0x140f132b0.
    pub fn burst_size(&self, w: usize, now: i32) -> i32 {
        let d = self.wdef(w);
        let bm = self.burst_mode(w).clamp(0, 3) as usize;
        let item = d.charge.item(&ChargeProperty::BurstCount);
        let size = if let (Some(_), true, false) = (item, self.is_burst_mode(w), d.charge.no_discharge) {
            let v = self.charge_value(w, &ChargeProperty::BurstCount, self.mstate[w].charge.percent) as i32;
            if !self.can_charge(w, now) && v == 0 {
                return 0;
            }
            v
        } else {
            let mut n = self.applied[w].weapon.burst_size;
            if n == 0 {
                n = d.bursts[bm].burst_count;
            }
            if n < 0 || d.bursts[bm].fake_burst_count > 0 || bm != BURSTMODE_BURST as usize {
                return 0;
            }
            // 0 -> clip size (no clips on SP weapons) or 0x7fffffff.
            if n == 0 { i32::MAX } else { n }
        };
        if size != i32::MAX && size != 0 && !d.infinite_ammo && d.bursts[bm].ammo_per_burst == -1 && self.applied[w].weapon.burst_ammo_cost == -1 {
            return size.min(self.ammo_shots(w)).max(1);
        }
        size
    }

    /// StartBurst 0x140f15710: burstCount = GetBurstSize, capped by the ammo (no ammoPerBurst / cost).
    pub fn start_burst(&mut self, w: usize, now: i32) {
        let n = self.burst_size(w, now);
        self.mstate[w].burst_count = n;
    }

    // ---- charge helpers ----

    /// The decl whose chargeInfo the charge functions read (the selection at their top, e.g. FinishFire
    /// ~0x140f0e9e2): mode 1 its decl; mode 0 the secondary decl when that one's chargeInfo has
    /// overridePrimaryChargeInfo (+0x414, plasma), else mode 0's decl.
    pub fn charge_decl(&self, w: usize) -> Arc<WeaponDef> {
        if self.mstate[w].fire_mode == 0 {
            if let Some(d1) = self.mode_def(w, 1).filter(|d| d.charge.override_primary_charge_info) {
                return d1;
            }
        }
        self.wdef(w)
    }

    fn charge_info(&self, w: usize) -> ChargeInfo {
        self.charge_decl(w).charge.clone()
    }

    /// chargeItemIndex 0x140f11e00 (current mode).
    pub fn charge_item(&self, w: usize, p: &ChargeProperty) -> Option<usize> {
        self.charge_decl(w).charge.item(p)
    }

    /// 0x140f12840: (1 - p) * valueMin + p * valueMax with the CHARGE_VALUE_MIN / MAX overrides (>= 0);
    /// |x| <= 1e-18 snaps to 0. A table is looked up at p instead (idLookupTable 0x140284580, interpolated, no
    /// left/right remap): the mode's CHARGE_VALUE_TABLE override first, then for mode 0 of an
    /// overridePrimaryChargeInfo weapon mode 1's override entry (0x140f11e90: its table, else its max), then the
    /// item's valueTable.
    pub fn charge_value(&self, w: usize, p: &ChargeProperty, pct: f32) -> f32 {
        let d = self.charge_decl(w);
        let Some(i) = d.charge.item(p) else { return 0.0 };
        let it = &d.charge.items[i];
        let (mut lo, mut hi) = (it.value_min, it.value_max);
        let fm = self.mstate[w].fire_mode;
        if let Some(o) = self.applied[w].modes[fm].charge_values.iter().find(|c| &c.0 == p) {
            if let Some(t) = self.value_table(w, &o.3) {
                return lookup_raw(&t, pct);
            }
            if o.1 >= 0.0 {
                lo = o.1;
            }
            if o.2 >= 0.0 {
                hi = o.2;
            }
        }
        if fm == 0 && d.charge.override_primary_charge_info {
            if let Some(o) = self.applied[w].modes[1].charge_values.iter().find(|c| &c.0 == p) {
                if let Some(t) = self.value_table(w, &o.3) {
                    return lookup_raw(&t, pct);
                }
                if o.2 >= 0.0 {
                    hi = o.2;
                }
            }
        }
        if let Some(t) = self.value_table(w, &it.value_table) {
            return lookup_raw(&t, pct);
        }
        // BURST_COUNT in burst mode: the max is at least the BURST_SIZE override (+0x968).
        if *p == ChargeProperty::BurstCount && self.is_burst_mode(w) && hi <= self.applied[w].weapon.burst_size as f32 {
            hi = self.applied[w].weapon.burst_size as f32;
        }
        let hi = if hi.abs() <= 1e-18 { 0.0 } else { hi };
        let lo = if lo.abs() <= 1e-18 { 0.0 } else { lo };
        (1.0 - pct) * lo + pct * hi
    }

    fn value_table(&self, w: usize, name: &str) -> Option<Arc<crate::handlayers::reactions::DeclTable>> {
        if name.is_empty() {
            return None;
        }
        self.defs[w].mods.as_ref()?.tables.get(name).cloned()
    }

    /// ChargeTime 0x140f121a0 (owner stat scale = 1 without powerups).
    pub fn charge_time(&self, w: usize, mode: usize) -> i32 {
        let o = self.applied[w].modes[mode.min(1)].charge_time;
        if o >= 0 { o } else { self.mode_def(w, mode).map(|d| d.charge.charge_time_ms).unwrap_or(0) }
    }

    /// DischargeTimeout 0x140f117f0: CHARGE_TIMEOUT override >= 0, else decl.dischargeTimeoutMS.
    pub fn discharge_timeout(&self, w: usize, mode: usize) -> i32 {
        let o = self.applied[w].modes[mode.min(1)].charge_timeout;
        if o >= 0 { o } else { self.mode_def(w, mode).map(|d| d.charge.discharge_timeout_ms).unwrap_or(0) }
    }

    /// 0x140f11b10: the charge fraction the remaining ammo allows.
    pub fn max_frac(&self, w: usize) -> f32 {
        let d = self.charge_decl(w);
        let c = &d.charge;
        let mut r = 1.0;
        let bm = self.burst_mode(w);
        if c.item(&ChargeProperty::BurstCount).is_some() && (self.is_burst_mode(w) || self.is_select_fire(w) || bm == BURSTMODE_FULLAUTO) {
            let max_v = self.charge_value(w, &ChargeProperty::BurstCount, 1.0);
            r = if max_v <= 0.0 {
                0.0
            } else {
                let a = d.ammo_per_shot;
                let ammo = match self.pool_for(&d) {
                    None => max_v as i32 * a.max(1),
                    Some(p) => self.pools[p].count,
                };
                if c.item(&ChargeProperty::AmmoToUse).is_some() {
                    let mut k = max_v as i32;
                    loop {
                        if k < 0 {
                            break 0.0;
                        }
                        let need = k as f32 * self.charge_value(w, &ChargeProperty::AmmoToUse, k as f32 / max_v);
                        if ammo as f32 >= need {
                            break k as f32 / max_v;
                        }
                        k -= 1;
                    }
                } else if a > 0 {
                    ((ammo as f32 / a as f32).floor().min(max_v) as i32) as f32 / max_v
                } else {
                    1.0
                }
            };
        }
        if r < 1.0 && c.min_charge_required_to_discharge == 1.0 && !c.allow_partial_charge_with_insufficient_ammo {
            r = 0.0;
        }
        r
    }

    /// GetMinChargeRequiredToDischarge 0x140f11970.
    pub fn min_charge_to_discharge(&self, w: usize) -> f32 {
        let c = self.charge_info(w);
        if c.items.is_empty() {
            return 0.0;
        }
        if c.allow_partial_charge_with_insufficient_ammo { self.max_frac(w).min(c.min_charge_required_to_discharge) } else { c.min_charge_required_to_discharge }
    }

    /// CanCharge 0x140f04420 (owner / player-state gates: always allowed here); canOnlyChargeWhenTargeting
    /// (decl +0x416) needs a target in targeting slot 0 (0x140c26010, weapons::targeting).
    pub fn can_charge(&self, w: usize, now: i32) -> bool {
        let d = self.charge_decl(w);
        let c = &d.charge;
        let ms = &self.mstate[w];
        let ch = &ms.charge;
        if c.items.is_empty() || now == ch.end_time || ch.state == ChargeState::Discharging {
            return false;
        }
        let m = ms.fire_mode;
        if c.hold_time_before_charging_ms >= 1 && !(ms.trigger_down[m] && now - ms.trigger_time[m] >= c.hold_time_before_charging_ms) {
            return false;
        }
        if c.wait_for_next_fire_time_before_charging && self.next_fire(w, m) > now {
            return false;
        }
        if self.is_empty(w) || (c.can_only_charge_when_targeting && self.lock_candidate(w).is_none()) {
            return false;
        }
        let mf = self.max_frac(w);
        if mf == 0.0 || (c.min_charge_required_to_discharge == 1.0 && 0.0 < mf && mf < 1.0) {
            return false;
        }
        !self.in_burst(w) && now >= ch.can_charge_time
    }

    /// 0x140f167c0(weapon, mode): the mode uses a charge (its decl has charge items, or mode 0 with a
    /// secondary decl whose chargeInfo has overridePrimaryChargeInfo).
    pub fn mode_uses_charge(&self, w: usize, mode: usize) -> bool {
        if mode == 0 && self.mode_def(w, 1).is_some_and(|d| !d.charge.items.is_empty() && d.charge.override_primary_charge_info) {
            return true;
        }
        self.mode_def(w, mode).is_some_and(|d| !d.charge.items.is_empty())
    }

    /// The charge fire gate 0x140f19b90 (and the zoom / zero-charge refusals of 0x140f1afe0): false = the
    /// current mode may not fire yet.
    pub fn charge_gate(&self, w: usize, now: i32, zoomed: bool) -> bool {
        use ChargeProperty as P;
        let d = self.charge_decl(w);
        let c = &d.charge;
        let mode = self.mstate[w].fire_mode;
        let m = mode as i32;
        let ch = &self.mstate[w].charge;
        if d.hands.can_only_fire_when_zoomed && !zoomed {
            return false;
        }
        let discharge_mode = m == c.discharge_fire_mode || c.discharge_fire_mode == 2;
        if c.dryfire_at_zero_charge && discharge_mode && ch.percent == 0.0 {
            return false;
        }
        // The charge just discharged this tick reads the value from before the discharge (+0x17a4).
        let pct = if ch.end_time == now { ch.before_discharge } else { ch.percent };
        let burst_item = c.item(&P::BurstCount).is_some();
        let burst_mode = self.is_burst_mode(w);
        let in_burst = burst_mode && self.mstate[w].burst_count > 0;
        if pct > 0.0 && burst_item && burst_mode && self.charge_value(w, &P::BurstCount, pct).floor() as i32 == 0 {
            return false;
        }
        if burst_item && burst_mode && !in_burst && self.max_frac(w) == 0.0 {
            return false;
        }
        if ch.end_time < now && now < ch.can_charge_time {
            if self.mode_uses_charge(w, mode) && !(in_burst && burst_item) && m == c.discharge_fire_mode {
                return false;
            }
            if ch.blocks_other_mode {
                return false;
            }
        }
        if !(burst_item && in_burst) && self.can_charge(w, now) && discharge_mode && pct < self.min_charge_to_discharge(w) {
            return false;
        }
        if c.item(&P::MaxProjectiles).is_some() && self.charge_value(w, &P::MaxProjectiles, pct).floor() as i32 == 0 {
            return false;
        }
        true
    }

    /// ChargeTimeoutPercent 0x140f12280.
    pub fn charge_timeout_percent(&self, w: usize, now: i32) -> f32 {
        let ch = &self.mstate[w].charge;
        if self.charge_item(w, &ChargeProperty::BurstCount).is_some() && self.in_burst(w) {
            return 0.0;
        }
        if now < ch.can_charge_time {
            if now < ch.timeout_start {
                return 0.0;
            }
            let span = (ch.can_charge_time - ch.timeout_start) as f32;
            return if span > 0.0 { 1.0 - (ch.can_charge_time - now) as f32 / span } else { 1.0 };
        }
        1.0
    }

    /// The per-frame charge update (idWeapon vslot +0x490 0x140f248c0) for weapon `w`'s current mode.
    pub fn charge_update(&mut self, w: usize, now: i32) -> Vec<ChargeEvent> {
        let mut ev = Vec::new();
        let d = self.charge_decl(w);
        let c = d.charge.clone();
        // A mode without charge items still drops a charge left from the other mode (CanCharge is false).
        if c.items.is_empty() && c.per_shot.max_charge <= 0.0 && self.mstate[w].charge.percent == 0.0 && self.mstate[w].charge.state != ChargeState::Cooling {
            self.mstate[w].charge.play_anim = c.charge_anim_state_name.is_empty() && self.wdef(w).has_charge_state;
            return ev;
        }
        let mode = self.mstate[w].fire_mode;
        if self.mstate[w].charge.state == ChargeState::Cooling && self.charge_timeout_percent(w, now) >= 1.0 {
            self.mstate[w].charge.state = ChargeState::None;
        }
        if self.mstate[w].charge.state == ChargeState::Discharging {
            return ev;
        }
        // 0x140f248c0 ~0x140f24d00 (charge2.c l.180): +0x17bc = chargeAnimStateName empty or chargeAnimIntervalMS
        // < 1 ? the mode decl's hasChargeState : (int)(prevDuration / interval) < (int)(duration / interval).
        {
            let md = self.wdef(w);
            let ch = &mut self.mstate[w].charge;
            ch.play_anim = if c.charge_anim_state_name.is_empty() || c.charge_anim_interval_ms < 1 {
                md.has_charge_state
            } else {
                let f = 1.0 / c.charge_anim_interval_ms as f32;
                ((f * ch.prev_duration) as i32) < ((f * ch.duration) as i32)
            };
        }
        let prev = self.mstate[w].charge.percent;
        if !self.can_charge(w, now) {
            // INTERIM: not while a burst runs (the update's place in the frame relative to the burst's first
            // FireWeapon is not decoded; discharging untimed first would drop the burst's timeout).
            if prev > 0.0 && !c.keep_charge_when_cant_charge && !self.in_burst(w) {
                self.discharge(w, false, now);
            }
            self.charge_feedback(w, &c, prev, &mut ev);
            return ev;
        }
        let mf = self.max_frac(w);
        let t = self.charge_time(w, mode) as f32 * mf;
        let overheated = self.states[w].overheat >= 1;
        {
            let ch = &mut self.mstate[w].charge;
            if t > 0.0 {
                ch.duration = 0.0;
                if overheated {
                    ch.can_charge_time = ch.can_charge_time.max(now + self.states[w].overheat);
                } else if ch.start_time == -1 {
                    ch.start_time = now;
                    ch.discharge_percent = 0.0;
                    ch.burst_shots_fired = 0;
                } else if !c.oscillate {
                    ch.duration = ((now - ch.start_time) as f32).min(t).ceil();
                } else {
                    ch.duration = (now - ch.start_time) as f32;
                }
            }
            let mut pct = ch.base;
            if t == 0.0 && c.per_shot.max_charge <= 0.0 {
                pct = 1.0;
            } else if t > 0.0 && !c.oscillate {
                pct = (ch.duration / t + ch.base).min(1.0);
            } else if t > 0.0 {
                // OSCILLATE ping-pong (0x141ed13c8): x in [0, 2) folded back after 1.
                let x = (ch.duration / t + ch.base) % 2.0;
                pct = if x <= 1.0 { x } else { 2.0 - x };
            }
            if c.per_shot.max_charge > 0.0 {
                pct += ch.per_shot_val / c.per_shot.max_charge;
            }
            ch.percent = pct.min(mf);
            if ch.percent > 0.0 {
                ch.full = ch.percent;
            }
            ch.state = if ch.percent >= 1.0 { ChargeState::FullyCharged } else if ch.start_time != -1 { ChargeState::Charging } else { ChargeState::Ready };
            ch.prev_duration = ch.duration;
        }
        self.charge_feedback(w, &c, prev, &mut ev);
        ev
    }

    /// Charge feedback at the end of the update: interval sounds per item step, start / looping sound,
    /// fully charged sound, sound stop at 0.
    fn charge_feedback(&mut self, w: usize, c: &ChargeInfo, prev: f32, ev: &mut Vec<ChargeEvent>) {
        let new = self.mstate[w].charge.percent;
        self.mstate[w].charge.prev_percent = new;
        for it in &c.items {
            if (self.charge_value(w, &it.property, prev) as i32) < (self.charge_value(w, &it.property, new) as i32) && !it.interval_sound.is_empty() {
                ev.push(ChargeEvent::Sound(it.interval_sound.clone()));
            }
        }
        if prev == 0.0 && new > 0.0 {
            if !c.looping_sound.is_empty() {
                ev.push(ChargeEvent::Sound(c.looping_sound.clone()));
            }
            let mode = self.mstate[w].fire_mode;
            let s = &self.applied[w].modes[mode].charge_start_sound;
            let start = if s.is_empty() { &c.start_sound } else { s };
            if !start.is_empty() {
                ev.push(ChargeEvent::Sound(start.clone()));
            }
        }
        if new >= 1.0 && prev < 1.0 && !c.fully_charged_sound.is_empty() {
            ev.push(ChargeEvent::Sound(c.fully_charged_sound.clone()));
        }
        if new == 0.0 && prev > 0.0 {
            ev.push(ChargeEvent::StopSound);
        }
    }

    /// DischargeCharge 0x140f06400 (vslot +0x4a0); `fired`: a shot of the discharge mode was fired.
    pub fn discharge(&mut self, w: usize, fired: bool, now: i32) {
        let c = self.charge_info(w);
        let mode = self.mstate[w].fire_mode;
        let mf = self.max_frac(w);
        let ch = self.mstate[w].charge.clone();
        let full = ch.full;
        let d = ch.discharge_percent;
        let mut per = c.discharge_percent_per_shot;
        if c.discharge_only_for_insufficient_ammo && mf == 1.0 {
            per = 0.0;
        }
        let new_d = if fired { (d + per).min(full) } else { full };
        let remaining = if full <= new_d { 0.0 } else { full };
        let timed = fired || (0.0 < d && new_d == full && per < 1.0);
        if (d == new_d && ch.percent == remaining) || ch.discharge_time == now || now < ch.can_charge_time {
            return;
        }
        let burst_item = self.charge_item(w, &ChargeProperty::BurstCount).is_some();
        let burst_mode = self.is_burst_mode(w);
        let interval = self.firing_interval(w);
        // The item cache (+0x17d8, 0x140f125e0) holds the BURST_COUNT value of the charge being discharged.
        let burst_value = self.charge_value(w, &ChargeProperty::BurstCount, full) as i32;
        let ch = &mut self.mstate[w].charge;
        ch.discharge_time = now;
        if ch.end_time == now {
            return;
        }
        if remaining <= 0.0 {
            ch.end_time = now;
            // INTERIM: the chargePerShot value (+0x17d0) goes with a full discharge (that part of
            // DischargeCharge is not read).
            ch.per_shot_val = 0.0;
        }
        ch.before_discharge = ch.percent;
        ch.base = remaining;
        ch.discharge_percent = new_d;
        ch.percent = remaining;
        ch.prev_percent = remaining;
        ch.start_time = -1;
        // 0x140f072df: prevDuration -1, chargeDuration 0, +0x17bc cleared.
        ch.prev_duration = -1.0;
        ch.duration = 0.0;
        ch.play_anim = false;
        if timed {
            // DischargeTimeout scaled by the charge discharged from (scaleDischargeTimeoutByDischargePct uses
            // "full", the charge at the last charge event).
            let mut t = self.discharge_timeout(w, mode);
            if c.scale_discharge_timeout_by_discharge_pct {
                t = (t as f32 * full) as i32;
            }
            let ch = &mut self.mstate[w].charge;
            if ch.can_charge_time < now + t {
                ch.can_charge_time = now + t;
            }
            ch.timeout_start = now;
            ch.blocks_other_mode = c.timeout_blocks_other_fire_mode;
            // A BURST_COUNT weapon in burst mode pushes both back by burstValue * GetFiringInterval (raw).
            if burst_item && burst_mode {
                let push = interval * burst_value;
                ch.can_charge_time += push;
                ch.timeout_start += push;
            }
        } else {
            let ch = &mut self.mstate[w].charge;
            ch.can_charge_time = ch.can_charge_time.max(now);
        }
        let ch = &mut self.mstate[w].charge;
        ch.state = if now < ch.can_charge_time { ChargeState::Cooling } else { ChargeState::None };
    }
}

/// idLookupTable lookup 0x140284580 (interpolated): min + (max - min) * curve(x), min / max snapped at 1e-18; no
/// idDeclTable left/right remap.
pub fn lookup_raw(t: &crate::handlayers::reactions::DeclTable, x: f32) -> f32 {
    let c = t.curve(x);
    let tiny = |v: f32| if v.abs() <= 1e-18 { 0.0 } else { v };
    (1.0 - c) * tiny(t.min) + c * tiny(t.max)
}
