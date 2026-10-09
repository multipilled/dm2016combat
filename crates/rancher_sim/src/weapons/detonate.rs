//! The weapon's launched-projectile list and the alt-fire detonation (RL remote detonate; gamedata/re/MODS.md
//! section 8h).
//!
//! FinishFire (0x140f0e544) appends every launched projectile to launchedProjectiles (weapon +0x568: handle +
//! explodeTime = now + explodeProjectilesAutomaticallyDelay, or 0). idHands::UpdateWeapon_Default reports the
//! alt button to the weapon (0x140d80200..0x140d806b7): for secondary trigger modes SECONDARY_PRESS / TAP /
//! TOGGLE (3 / 4 / 0xc) a press calls AltFirePressed (vslot +0x318 -> +0x310 0x140f1e690), for
//! SECONDARY_HOLD_PRIMARY_PRESS / RELEASE (7 / 8) every held frame does; a release calls AltFireReleased (vslot
//! +0x328 -> +0x320 0x140f1f590). Both post the explode event when CanDetonate (vslot +0x338 0x140f19670) holds;
//! the event runs ExplodeLaunchedProjectiles(weapon, alt) 0x140f0b4a0.

use super::arsenal::Arsenal;
use super::targeting::TargetState;

/// projectileState_t (reflection enum table 0x143541960) values the list code tests.
pub const PROJECTILE_ACTIVE: i32 = 1;
pub const PROJECTILE_STUCK: i32 = 3;
pub const PROJECTILE_EXPLODED: i32 = 6;

/// idWeapon::launchedProjectile_t (0x28): the projectile handle (here the game side's projectile id) and
/// explodeTime; launchTime and canDetonateWithAltTrigger mirror the projectile's +0x484c / decl +0x325.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchedProjectile {
    pub id: u32,
    pub explode_time: i32,
    pub launch_time: i32,
    pub can_detonate_with_alt_trigger: bool,
    /// projectileState_t (+0x4860), kept current by the game side.
    pub state: i32,
}

/// The weapon members of the list (weapon +0x568 list, +0x584 the alt-released latch).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Launched {
    pub list: Vec<LaunchedProjectile>,
    /// +0x584: 1 after AltFireReleased ran, 0 after AltFirePressed (the first press after a release cancels a
    /// lock, the first release ends a non-maintained one).
    pub alt_released: bool,
    /// +0x588 playedDenyApprovedFireSound: DenyFire's sound plays once. INTERIM: only the ctor (0x140f00b11)
    /// was found clearing it.
    pub deny_sound_played: bool,
}

/// What the alt handlers ask the game side to do.
#[derive(Debug, Clone, PartialEq)]
pub enum DetonateEvent {
    /// ExplodeLaunchedProjectiles: explode these projectiles where they are (manuallyDetonatedByPlayer,
    /// useAltDamageFX / useAltExplodeSound), then post `sound` at the player (empty = none).
    Explode { weapon: usize, projectiles: Vec<u32>, sound: String },
    /// explodeProjectilesDenialSound (nothing to detonate).
    Sound { weapon: usize, sound: String },
}

impl Arsenal {
    /// FinishFire's append for one launched projectile of `w` (the shot's decl `mode_decl`).
    pub fn add_launched(&mut self, w: usize, mode_decl: &super::decl::WeaponDef, id: u32, now: i32) {
        let d = mode_decl.explode.automatically_delay;
        self.launched[w].list.push(LaunchedProjectile {
            id,
            explode_time: if 0 < d { now + d } else { 0 },
            launch_time: now,
            can_detonate_with_alt_trigger: mode_decl.projectile.can_detonate_with_alt_trigger,
            state: PROJECTILE_ACTIVE,
        });
    }

    /// The game side's projectile state change (EXPLODED entries are dropped, as the weapon think's prune
    /// 0x140f27560 does).
    pub fn launched_state(&mut self, id: u32, state: i32) {
        for l in self.launched.iter_mut() {
            l.list.retain_mut(|p| {
                if p.id == id {
                    p.state = state;
                }
                !(p.id == id && state == PROJECTILE_EXPLODED)
            });
        }
    }

    /// DetonatesProjectiles (vslot +0x330 0x140f04730): DETONATE_PROJECTILES on mode 0 or 1, or the current
    /// mode decl's explodeProjectilesOnAltFire / OnAltFireRelease.
    pub fn detonates_projectiles(&self, w: usize) -> bool {
        let a = &self.applied[w].modes;
        if a[0].detonate_projectiles || a[1].detonate_projectiles {
            return true;
        }
        self.mode_def(w, self.mstate[w].fire_mode).is_some_and(|d| d.explode.on_alt_fire || d.explode.on_alt_fire_release)
    }

    /// The current mode decl's explodeProjectilesAltFireDelay (0x140f12c10(weapon, mode) +0x660).
    fn alt_fire_delay(&self, w: usize) -> i32 {
        self.mode_def(w, self.mstate[w].fire_mode).map(|d| d.explode.alt_fire_delay).unwrap_or(0)
    }

    /// CanDetonate (vslot +0x338 0x140f19670): the weapon detonates projectiles and one in the list is ACTIVE
    /// with canDetonateWithAltTrigger, or STUCK, launched more than explodeProjectilesAltFireDelay ago. (The
    /// player-state gates in front are taken as passed.)
    pub fn can_detonate(&self, w: usize, now: i32) -> bool {
        if !self.detonates_projectiles(w) {
            return false;
        }
        let delay = self.alt_fire_delay(w);
        self.launched[w].list.iter().any(|p| ((p.can_detonate_with_alt_trigger && p.state == PROJECTILE_ACTIVE) || p.state == PROJECTILE_STUCK) && delay < now - p.launch_time)
    }

    /// ExplodeLaunchedProjectiles 0x140f0b4a0(weapon, alt, false, timed): from the last entry back, an alt
    /// detonation takes the canDetonateWithAltTrigger ones, a timed pass those whose explodeTime is due; a
    /// projectile older than explodeProjectilesAltFireDelay explodes and leaves the list (younger ones stay).
    /// The alt-fire sound follows when anything exploded.
    pub fn explode_launched(&mut self, w: usize, alt: bool, timed: bool, now: i32) -> Option<DetonateEvent> {
        let delay = self.alt_fire_delay(w);
        let mut out = Vec::new();
        let list = &mut self.launched[w].list;
        let mut i = list.len();
        while i > 0 {
            i -= 1;
            let p = &list[i];
            let due = timed && 0 < p.explode_time && p.explode_time <= now;
            let pick = due || (alt && p.can_detonate_with_alt_trigger);
            if !pick || p.state == PROJECTILE_EXPLODED {
                continue;
            }
            if delay < now - p.launch_time {
                out.push(p.id);
                // The removal copies the last entry into the hole (idList::RemoveIndexFast).
                list.swap_remove(i);
            }
        }
        if out.is_empty() {
            return None;
        }
        let sound = if alt || timed { self.mode_def(w, self.mstate[w].fire_mode).map(|d| d.explode.alt_fire_sound.clone()).unwrap_or_default() } else { String::new() };
        Some(DetonateEvent::Explode { weapon: w, projectiles: out, sound })
    }

    /// DenyFire (idWeapon vslot +0x468 0x140f061e0, asked before CanFire on the hands' fire paths): the mode decl
    /// explodes projectiles on alt fire and 0 < max(DETONATE_PROJECTILES_MAX_NUM, explodeProjectilesMaxNum) <= the
    /// launched count (burst count 0, explodeProjectilesExceededMaxSound), or the mode's lock data
    /// requireLockToFire with targeting slot 0 not LOCKED (noTargetDenySound). Returns (deny, the sound to post).
    pub fn deny_fire(&mut self, w: usize) -> (bool, Option<String>) {
        let mode = self.mstate[w].fire_mode;
        let Some(md) = self.mode_def(w, mode) else { return (false, None) };
        let mut deny = false;
        let mut snd = String::new();
        let a = &self.applied[w].modes;
        let cap = match self.mode_def(w, 0).filter(|d| d.explode.max_num != 0) {
            Some(d0) => a[0].detonate_projectiles_max_num.max(d0.explode.max_num),
            None => match self.mode_def(w, 1) {
                Some(d1) => a[1].detonate_projectiles_max_num.max(d1.explode.max_num),
                None => 0,
            },
        };
        if (md.explode.on_alt_fire || md.explode.on_alt_fire_release) && 0 < cap && cap as usize <= self.launched[w].list.len() {
            self.mstate[w].burst_count = 0;
            deny = true;
            snd = md.explode.exceeded_max_sound.clone();
        }
        if let Some(l) = self.target_lock_data(w, mode) {
            if l.require_lock_to_fire && self.targeting[w].slots[0].state != TargetState::Locked {
                deny = true;
                snd = l.no_target_deny_sound.clone();
            }
        }
        if !deny {
            return (false, None);
        }
        let mut out = None;
        if !std::mem::replace(&mut self.launched[w].deny_sound_played, true) && !snd.is_empty() {
            out = Some(snd);
        }
        (true, out)
    }

    /// AltFirePressed (0x140f1e690): detonate when CanDetonate, else the denial sound; the first press after a
    /// release drops a LOCKED target of a locking mode.
    pub fn alt_fire_pressed(&mut self, w: usize) -> Vec<DetonateEvent> {
        let now = self.time_ms;
        let mut ev = Vec::new();
        let was_released = std::mem::replace(&mut self.launched[w].alt_released, false);
        if self.can_detonate(w, now) {
            ev.extend(self.explode_launched(w, true, false, now));
        } else if let Some(s) = self.mode_def(w, self.mstate[w].fire_mode).map(|d| d.explode.denial_sound.clone()).filter(|s| !s.is_empty()) {
            ev.push(DetonateEvent::Sound { weapon: w, sound: s });
        }
        // INTERIM: the charge-timeout sound (weapon +0x1788 while ChargeTimeoutPercent < 1) is not posted.
        let mode = self.mstate[w].fire_mode;
        if was_released && self.target_lock_data(w, mode).is_some_and(|l| l.can_lock) && self.targeting[w].slots.iter().any(|s| s.state == TargetState::Locked) {
            self.clear_target_slot(w, None);
        }
        ev
    }

    /// AltFireReleased (0x140f1f590), once per release: detonate when CanDetonate; a locking mode whose lock
    /// data does not automaticallyMaintainLock drops its targets.
    pub fn alt_fire_released(&mut self, w: usize) -> Vec<DetonateEvent> {
        let now = self.time_ms;
        let mut ev = Vec::new();
        if std::mem::replace(&mut self.launched[w].alt_released, true) {
            return ev;
        }
        if self.can_detonate(w, now) {
            ev.extend(self.explode_launched(w, true, false, now));
        }
        let mode = self.mstate[w].fire_mode;
        if let Some(l) = self.target_lock_data(w, mode) {
            if l.can_lock && !l.automatically_maintain_lock {
                self.clear_target_slot(w, None);
            }
        }
        ev
    }
}
