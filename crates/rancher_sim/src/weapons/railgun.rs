//! idRailGun (the gauss cannon class, vtable 0x1422f26d8): the ADS charge of a mode whose weaponData
//! (idDeclWeapon_RailGunData) does not set useBaseChargeBehavior, i.e. the precision bolt
//! (gamedata/re/MODS.md "gauss precision bolt"). Siege mode's data sets it, so siege uses the base charge
//! (weapons::charge). Times are game ticks; chargeTimeMS is compared raw, like the base charge.
//!
//! Overrides decoded: charge update (vslot +0x490) 0x140efcd30, GetChargePercent (+0x470) 0x140efbcd0, the
//! ADS-charge check 0x140efc750, the shot hook (+0x390) 0x140efc0a0 and FinishFire (+0x398) 0x140efbbe0's
//! reset. Not ported: the railgun CanCharge override 0x140efba60 (no SP railgun mode has charge-state hands or
//! forbidZoomIfCannotCharge), the second charge clock (+0x1c00, gameLocal vfunc +0x270), the laser and the
//! debug charge bar.

use std::sync::Arc;

use super::arsenal::{Arsenal, WeaponEvent};
use super::decl::{RailGunData, WeaponDef};

/// idRailGun members (reflection 0x14320dbd0).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RailGunState {
    /// +0x1c20 adsChargeStartTime (0 = not charging).
    pub ads_charge_start: i32,
    /// +0x1c24 lastChargePercent.
    pub last_charge_percent: f32,
    /// +0x1c28 railGunLastChargeDurationMS.
    pub last_charge_duration: i32,
    /// +0x1c48 fullyCharged (the fully charged sound played).
    pub fully_charged: bool,
    /// +0x1c10: the charge the last shot hook read.
    pub shot_percent: f32,
    /// The charge sound loop runs.
    pub sound_on: bool,
}

impl Arsenal {
    /// Weapon `w`'s current mode decl when it runs idRailGun's charge (railgun weaponData without
    /// useBaseChargeBehavior).
    pub fn railgun_def(&self, w: usize) -> Option<Arc<WeaponDef>> {
        let d = self.wdef(w);
        d.railgun.as_ref().is_some_and(|r| !r.use_base_charge_behavior).then_some(d)
    }

    fn rail(d: &WeaponDef) -> &RailGunData {
        d.railgun.as_ref().expect("railgun data")
    }

    /// 0x140efc750: the ADS charge may run: not empty, ammo left (or infinite), zoomed (weapon+0x14a0 bit 0)
    /// unless the data lifts chargeRequiresZoom, and fireModeForFireEvent (+0x8d8, written by SetFireMode) is
    /// the secondary mode. (The owner state bit +0x273ef & 1 is not ported.)
    pub fn railgun_can_charge(&self, w: usize) -> bool {
        let Some(d) = self.railgun_def(w) else { return false };
        if self.is_empty(w) {
            return false;
        }
        if !self.zoomed && Self::rail(&d).charge_requires_zoom {
            return false;
        }
        self.mstate[w].fire_mode == 1
    }

    /// GetChargePercent 0x140efbcd0 (owner = the player): T = CHARGE_TIME override (>= 0) else
    /// secondaryFire.chargeTimeMS; min((now - adsChargeStartTime) / T, 1) once charging started, while the ADS
    /// check passes and the weapon is not overheated; else 0.
    pub fn railgun_charge_percent(&self, w: usize, now: i32) -> f32 {
        let Some(d) = self.railgun_def(w) else { return self.mstate[w].charge.percent };
        let mode = self.mstate[w].fire_mode;
        let o = self.applied[w].modes[mode].charge_time;
        let t = if 0.0 <= o as f32 { o } else { Self::rail(&d).charge_time_ms };
        let start = self.railgun[w].ads_charge_start;
        if !(self.railgun_can_charge(w) && self.states[w].overheat < 1) || t <= 0 || start <= 0 || now < start {
            return 0.0;
        }
        ((now - start) as f32 / t as f32).min(1.0)
    }

    /// The charge update override 0x140efcd30 (weapon_reloadTogglesFireMode 0): charging waits
    /// afterFireChargeDelay ticks after the last secondary FinishFire, starts when the ADS check passes, restarts
    /// after an overheat (start = now + overheat timer) and stops outside the secondary mode. Sounds: the
    /// charge loop + start sound when a charge begins, the fully charged sound once at 1.
    pub fn railgun_update(&mut self, w: usize, now: i32, ev: &mut Vec<WeaponEvent>) {
        let Some(d) = self.railgun_def(w) else { return };
        let r = Self::rail(&d);
        let mode = self.mstate[w].fire_mode;
        if (now as f32) < self.states[w].last_finish_fire[1] as f32 + r.after_fire_charge_delay {
            if self.railgun[w].sound_on {
                self.railgun[w].sound_on = false;
                ev.push(WeaponEvent::ChargeStop { weapon: w });
            }
            return;
        }
        if self.railgun[w].ads_charge_start == 0 && self.railgun_can_charge(w) {
            self.railgun[w].ads_charge_start = now;
        }
        let overheat = self.states[w].overheat;
        let s = &mut self.railgun[w];
        let mut elapsed = 0.0f32;
        if 0 < s.ads_charge_start {
            if s.ads_charge_start < now && mode == 1 {
                if 1 <= overheat {
                    s.ads_charge_start = now + overheat;
                } else {
                    elapsed = (now - s.ads_charge_start) as f32;
                }
            }
            if 0 < s.ads_charge_start && mode != 1 {
                s.ads_charge_start = 0;
            }
        }
        if s.last_charge_duration < 1 {
            if 0.0 < elapsed {
                s.sound_on = true;
                if !r.charge_sound.is_empty() {
                    ev.push(WeaponEvent::ChargeSound { weapon: w, sound: r.charge_sound.clone() });
                }
                if !r.start_sound.is_empty() {
                    ev.push(WeaponEvent::ChargeSound { weapon: w, sound: r.start_sound.clone() });
                }
            }
        } else if elapsed <= 0.0 && s.sound_on {
            s.sound_on = false;
            ev.push(WeaponEvent::ChargeStop { weapon: w });
        }
        if self.railgun[w].ads_charge_start != 0 {
            let pct = self.railgun_charge_percent(w, now);
            let s = &mut self.railgun[w];
            // 1 - FLT_MIN (0x142f8f000) <= pct.
            if 1.0 - f32::MIN_POSITIVE <= pct && !s.fully_charged && !r.fully_charged_sound.is_empty() {
                s.fully_charged = true;
                ev.push(WeaponEvent::ChargeSound { weapon: w, sound: r.fully_charged_sound.clone() });
            }
            s.last_charge_duration = elapsed as i32;
            s.last_charge_percent = pct;
        }
    }

    /// The shot hook 0x140efc0a0 for a secondary-mode shot: the charge (capped at 1, kept in +0x1c10) is looked
    /// up in chargeDamageScaleTable (idLookupTable 0x140284580, interpolated, no left/right remap) as the shot's
    /// damage scale (fire parms +0x1e4: INTERIM, taken as the shot's damage multiplier). Above 0.97 the shot
    /// uses fullyChargedProjectileDecl (for the SP precision bolt the ammo's own projectile, so the projectile is
    /// unchanged here). ammoUsedAtMaxCharge >= 0 would blend the ammo cost; no SP railgun data sets it.
    pub fn railgun_shot_scale(&mut self, w: usize, now: i32) -> f32 {
        let Some(d) = self.railgun_def(w) else { return 1.0 };
        if self.mstate[w].fire_mode != 1 {
            return 1.0;
        }
        let pct = self.railgun_charge_percent(w, now).min(1.0);
        self.railgun[w].shot_percent = pct;
        match &Self::rail(&d).charge_damage_scale_table {
            Some(t) => super::charge::lookup_raw(t, pct),
            None => 1.0,
        }
    }

    /// FinishFire 0x140efbbe0: adsChargeStartTime, the second clock and fullyCharged are cleared.
    pub fn railgun_finish_fire(&mut self, w: usize) {
        if self.railgun_def(w).is_none() {
            return;
        }
        let s = &mut self.railgun[w];
        s.ads_charge_start = 0;
        s.fully_charged = false;
    }
}
