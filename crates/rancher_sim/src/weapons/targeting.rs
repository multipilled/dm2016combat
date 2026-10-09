//! idWeapon targeting: the lock-on slots of the RL lock-on mod (gamedata/re/MODS.md section 8b).
//!
//! UpdateTargeting (idWeapon vslot +0x370, 0x140f29850) runs at the top of idHands::UpdateWeapon_Default
//! every frame: slot 0 always, slot i > 0 while slot i-1 is LOCKED or slot i is not NONE, for i < maxTargets
//! (0x140f135c0). The per-slot update is 0x140f28080 (candidate validation, the LOS clip query, the NONE ->
//! ACQUIRING -> LOCKED machine, the lock sounds), the best-target query 0x140f0b890. The fire path reads the
//! slots in InitForFire (0x140f179a0: which slot's target each projectile seeks) and FinishFire (0x140f0d5c0:
//! clearAfterNumShots, loseLockOnFire, the recovery delay). CanCharge's canOnlyChargeWhenTargeting asks for
//! slot 0's candidate (0x140c26010).
//!
//! The game side supplies the world as [`LockTarget`]s (the AIs' target points, bounds and LOS results).

use glam::Vec3;

use super::arsenal::Arsenal;
use super::decl::TargetLockData;

/// weaponTargeting_t entries (weapon+0x1560, 3 x 0xa8).
pub const SLOTS: usize = 3;

/// The 960 Hz game clock the targeting code converts its *Sec values with (time manager +0x10).
const TICKS_PER_SEC: f32 = 960.0;

/// The 7 target points the best-target query tests, in its order (stack table at 0x140f0bf0b): AIMPOINT_CENTER,
/// HEAD, LEFT_SHOULDER, RIGHT_SHOULDER, TORSO, LEGS, FEET.
pub const TEST_POINTS: [usize; 7] = [3, 1, 6, 7, 2, 4, 5];

/// AIMPOINT_CENTER: the point the LOS clip query aims at (0x140f28780).
pub const AIMPOINT_CENTER: usize = 3;

/// weaponTargetState_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetState {
    #[default]
    None = 0,
    Acquiring = 1,
    Locked = 2,
}

/// idWeapon::weaponTargeting_t (0xa8, reflection 0x143207360; ctor 0x140f01290: all 0, lastFireMode -1).
#[derive(Debug, Clone, PartialEq)]
pub struct TargetSlot {
    /// +0 active.
    pub active: bool,
    /// +0x8 enemy (set to the candidate when it locks), +0x28 lockTargetCandidate (the target).
    pub enemy: Option<u32>,
    pub candidate: Option<u32>,
    /// +0x48 clipQuery: the LOS query issued last frame for the candidate (Some(blocked)), read next frame.
    pub los_query: Option<bool>,
    /// +0x68 canTargetTime, +0x6c targetTimeOut.
    pub can_target_time: i32,
    pub target_timeout: i32,
    /// +0x70 targetStartTime, +0x74 lockStartTime, +0x78 outOfFovTime, +0x7c LOSExpireTime.
    pub target_start_time: i32,
    pub lock_start_time: i32,
    pub out_of_fov_time: i32,
    pub los_expire_time: i32,
    /// +0x80 prevState, +0x84 lastFireMode (-1).
    pub prev_state: TargetState,
    pub last_fire_mode: i32,
    /// +0x88 lockPercent, +0x8c unlockPercent.
    pub lock_percent: f32,
    pub unlock_percent: f32,
    /// +0x90 lockedTargetLockData (a pointer into the decl; a copy here).
    pub locked_lock_data: Option<TargetLockData>,
    /// +0x98 waitForDeactivationRelease.
    pub wait_for_deactivation_release: bool,
    /// +0x9c clearAfterNumShots.
    pub clear_after_num_shots: i32,
    /// +0xa0 state.
    pub state: TargetState,
}

impl Default for TargetSlot {
    fn default() -> Self {
        Self {
            active: false,
            enemy: None,
            candidate: None,
            los_query: None,
            can_target_time: 0,
            target_timeout: 0,
            target_start_time: 0,
            lock_start_time: 0,
            out_of_fov_time: 0,
            los_expire_time: 0,
            prev_state: TargetState::None,
            last_fire_mode: -1,
            lock_percent: 0.0,
            unlock_percent: 0.0,
            locked_lock_data: None,
            wait_for_deactivation_release: false,
            clear_after_num_shots: 0,
            state: TargetState::None,
        }
    }
}

impl TargetSlot {
    /// 0x140d5b490: back to NONE; canTargetTime, targetTimeOut, lastFireMode and clearAfterNumShots stay.
    fn reset(&mut self) {
        self.prev_state = self.state;
        self.target_start_time = 0;
        self.lock_start_time = 0;
        self.out_of_fov_time = 0;
        self.state = TargetState::None;
        self.locked_lock_data = None;
        self.lock_percent = 0.0;
        self.unlock_percent = 0.0;
        self.active = false;
        self.candidate = None;
        self.enemy = None;
    }
}

/// The weapon's targeting runtime.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Targeting {
    pub slots: [TargetSlot; SLOTS],
    /// +0x1758 targetingShotIndex: the slot whose target the next projectile seeks (InitForFire round robin).
    pub shot_index: usize,
}

/// A lockable entity as the targeting code queries it (built each frame by the game side).
#[derive(Debug, Clone, PartialEq)]
pub struct LockTarget {
    pub id: u32,
    /// Entity vslot +0x370; for an idAI2 the physics origin (0x140393d10).
    pub origin: Vec3,
    /// GetTargetPoint (idAI2 vslot +0x710 -> 0x140749b10) per aimPoint_t 0..=7 (index 0 unused).
    pub aim_points: [Vec3; 8],
    /// Physics absolute bounds (vslot +0x58(-1)).
    pub bounds: (Vec3, Vec3),
    /// The team test 0x140f199e0 passes (hostile for a weapon without targetsFriendlies) and the entity is a
    /// live actor (0x140dd1fc0).
    pub eligible: bool,
    /// Validation trace (0x141638b60, contents SOLID, eye -> origin) is clear: fraction >= 1.
    pub origin_visible: bool,
    /// The LOS clip query (contents 0x103085, eye -> AIMPOINT_CENTER) hits nothing but this target.
    pub center_visible: bool,
}

/// What UpdateTargeting reads from the owner: its eye (vslot +0x370 = the view origin) and view forward
/// (0x140dd18c0 +0x128 angles -> 0x1402d90c0).
#[derive(Debug, Clone, Copy)]
pub struct TargetView<'a> {
    pub eye: Vec3,
    pub forward: Vec3,
    pub targets: &'a [LockTarget],
}

impl TargetView<'_> {
    fn get(&self, id: u32) -> Option<&LockTarget> {
        self.targets.iter().find(|t| t.id == id)
    }
}

/// The owner's view and the lockable entities, set by the game side every frame (Arsenal::target_world)
/// before the hands run UpdateWeapon_Default.
#[derive(Debug, Clone, Default)]
pub struct TargetWorld {
    pub eye: Vec3,
    pub forward: Vec3,
    pub targets: Vec<LockTarget>,
}

impl TargetWorld {
    pub fn view(&self) -> TargetView<'_> {
        TargetView { eye: self.eye, forward: self.forward, targets: &self.targets }
    }
}

/// The per-slot update's sound posts (0x140f2912d on): acquiring on NONE -> ACQUIRING, locked on ACQUIRING ->
/// LOCKED, broken when a lock is broken (out of FOV / LOS, unlock time), disengage otherwise.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetEvent {
    Sound(String),
    /// A slot changed state (trace only).
    State { slot: usize, from: TargetState, to: TargetState, target: Option<u32> },
}

/// acos as the exe calls it (0x141ed29f4) with the clamp in front: dot <= -1 -> pi, >= 1 -> 0.
fn acos_clamped(d: f32) -> f32 {
    if d <= -1.0 {
        std::f32::consts::PI
    } else if d < 1.0 {
        d.acos()
    } else {
        0.0
    }
}

/// idMath::InvSqrt with the FLT_MIN clamp (0x144144b50).
fn normalize(v: Vec3) -> Vec3 {
    let l2 = v.length_squared().max(f32::MIN_POSITIVE);
    v * (1.0 / l2.sqrt())
}

/// 0x140287c20: does the infinite line start + t * dir cross the box (slab test, t in (-1e30, 1e30))?
pub fn line_hits_bounds(lo: Vec3, hi: Vec3, start: Vec3, dir: Vec3) -> bool {
    let (mut t0, mut t1) = (-1e30f32, 1e30f32);
    for i in 0..3 {
        let (d, s, mn, mx) = (dir[i], start[i], lo[i], hi[i]);
        if d.abs() <= f32::MIN_POSITIVE {
            if s < mn || s > mx {
                return false;
            }
            continue;
        }
        let inv = 1.0 / d;
        let (mut a, mut b) = ((mn - s) * inv, (mx - s) * inv);
        if b < a {
            std::mem::swap(&mut a, &mut b);
        }
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return false;
        }
    }
    true
}

fn bounds_center(b: (Vec3, Vec3)) -> Vec3 {
    (b.0 + b.1) * 0.5
}

/// aimPoint_t names (reflection enum table 0x14351c160) -> value.
pub fn aim_point(name: &str) -> Option<usize> {
    const T: [&str; 13] = ["ORIGIN", "HEAD", "TORSO", "CENTER", "LEGS", "FEET", "LEFT_SHOULDER", "RIGHT_SHOULDER", "LEFT_FOOT", "RIGHT_FOOT", "BEST", "EYELEVEL", "LOOKAHEAD"];
    let n = name.strip_prefix("AIMPOINT_").unwrap_or(name);
    T.iter().position(|t| *t == n)
}

/// GetTargetPoint (idActor 0x140749b10, idAI2 vslot +0x710) for aimPoint_t 0..=7: the first
/// actorConstants.aimPointJoints entry of that type (the joint's world position, given in `joints`) wins; else
/// HEAD = the eye position (vslot +0x718 0x14074a6d0: the `head` joint, given as `eye`), TORSO = 0.35 * the
/// bounds centre + 0.65 * the eye, CENTER = the bounds centre, LEGS = the origin raised by a quarter of the bounds
/// height, everything else the physics origin. `bounds` are the physics absolute bounds.
pub fn aim_points(joints: &[(usize, Vec3)], eye: Vec3, origin: Vec3, bounds: (Vec3, Vec3)) -> [Vec3; 8] {
    let mut out = [origin; 8];
    for (p, o) in out.iter_mut().enumerate() {
        if let Some((_, j)) = joints.iter().find(|(t, _)| *t == p) {
            *o = *j;
            continue;
        }
        let (lo, hi) = bounds;
        *o = match p {
            1 => eye,
            2 => (lo + hi) * 0.5 * 0.35000002 + eye * 0.65,
            3 => (lo + hi) * 0.5,
            4 => Vec3::new(origin.x, origin.y, (hi.z - lo.z) * 0.25 + origin.z),
            _ => origin,
        };
    }
    out
}

impl Arsenal {
    /// The fire mode's decl lock data (GetTargetLockData 0x140f14770): targetLockZoomed when zoomed and the mode
    /// decl has a zoom mode, else targetLockNormal.
    pub fn target_lock_data(&self, w: usize, mode: usize) -> Option<TargetLockData> {
        let d = self.mode_def(w, mode)?;
        Some(if self.zoomed && d.zoom_mode != super::zoom::ZoomMode::None { d.target_lock_zoomed.clone() } else { d.target_lock_normal.clone() })
    }

    /// 0x140f135c0: TARGET_MAX_TARGETS of the mode when >= 0, else maxTargets of the first slot's locked lock
    /// data, else of the current mode's decl (1 without one).
    pub fn max_targets(&self, w: usize) -> i32 {
        let mode = self.mstate[w].fire_mode;
        let o = self.applied[w].modes[mode].target_max_targets;
        if o >= 0 {
            return o;
        }
        let t = &self.targeting[w];
        match t.slots.iter().find_map(|s| s.locked_lock_data.as_ref()).cloned().or_else(|| self.target_lock_data(w, mode)) {
            Some(l) => l.max_targets,
            None => 1,
        }
    }

    /// GetLockPercent (idWeapon vslot +0x358, 0x140f12bb0): the last ACQUIRING slot's lockPercent, else the last
    /// LOCKED slot's (slot 0 when none).
    pub fn lock_percent(&self, w: usize) -> f32 {
        let s = &self.targeting[w].slots;
        let acq = s.iter().rposition(|x| x.state == TargetState::Acquiring);
        let locked = s.iter().rposition(|x| x.state == TargetState::Locked).unwrap_or(0);
        s[acq.unwrap_or(locked)].lock_percent
    }

    /// GetReticleDecl 0x140f12c70: the mode decl's lockedReticle once GetLockPercent >= 1, its reticleWhenZoomed
    /// while zoomed, the mode's RETICLE_DECL override, else its reticle. Empty = none.
    pub fn reticle_decl(&self, w: usize) -> String {
        let mode = self.mstate[w].fire_mode;
        let Some(d) = self.mode_def(w, mode) else { return String::new() };
        if self.lock_percent(w) >= 1.0 && !d.locked_reticle.is_empty() {
            return d.locked_reticle.clone();
        }
        if self.zoomed && !d.reticle_when_zoomed.is_empty() {
            return d.reticle_when_zoomed.clone();
        }
        let o = &self.applied[w].modes[mode].reticle;
        if !o.is_empty() {
            return o.clone();
        }
        d.reticle.clone()
    }

    /// 0x140c26010: slot 0's target (CanCharge with canOnlyChargeWhenTargeting needs one).
    pub fn lock_candidate(&self, w: usize) -> Option<u32> {
        self.targeting[w].slots[0].candidate
    }

    /// 0x140f13af0: slots with a target in LOCKED.
    pub fn locked_count(&self, w: usize) -> usize {
        self.targeting[w].slots.iter().filter(|s| s.candidate.is_some() && s.state == TargetState::Locked).count()
    }

    /// 0x140f05870: drop the fire-mode override (the lock's forced mode), back to the remembered request.
    pub fn restore_fire_mode(&mut self, w: usize) {
        let ms = &mut self.mstate[w];
        if ms.override_fire_mode != -1 {
            ms.override_fire_mode = -1;
            let prev = ms.override_prev_mode;
            self.set_fire_mode(w, prev.max(0) as usize);
            self.mstate[w].override_prev_mode = -1;
        }
    }

    /// forceFireModeWhenLocked 0x140f1cb70: remember the current mode (+0x8ec) and pin `mode` (+0x8e8).
    pub fn force_fire_mode(&mut self, w: usize, mode: usize) {
        if self.mstate[w].override_fire_mode == mode as i32 {
            return;
        }
        let cur = self.mstate[w].fire_mode as i32;
        self.mstate[w].override_prev_mode = cur;
        self.mstate[w].override_fire_mode = -1;
        self.set_fire_mode(w, mode);
        self.mstate[w].override_fire_mode = mode as i32;
    }

    /// 0x140f058b0(weapon, slot): reset the slot; once every slot is NONE the fire-mode override goes.
    /// `None` = all slots (0x140f05670).
    pub fn clear_target_slot(&mut self, w: usize, slot: Option<usize>) {
        match slot {
            Some(i) => {
                self.targeting[w].slots[i].reset();
                if self.targeting[w].slots.iter().any(|s| s.state != TargetState::None) {
                    return;
                }
            }
            None => {
                for s in self.targeting[w].slots.iter_mut() {
                    s.reset();
                }
            }
        }
        self.restore_fire_mode(w);
    }

    /// UpdateTargeting against `target_world`; the events collect in `target_events` for the game side.
    pub fn think_targeting(&mut self, w: usize) {
        let world = std::mem::take(&mut self.target_world);
        let ev = self.update_targeting(w, &world.view());
        self.target_world = world;
        self.target_events.extend(ev);
    }

    /// UpdateTargeting 0x140f29850.
    pub fn update_targeting(&mut self, w: usize, view: &TargetView) -> Vec<TargetEvent> {
        let mut ev = Vec::new();
        let mut i = 0;
        while (i as i32) < self.max_targets(w) && i < SLOTS {
            let t = &self.targeting[w];
            if i == 0 || t.slots[i - 1].state == TargetState::Locked || t.slots[i].state != TargetState::None {
                self.update_target_slot(w, i, view, &mut ev);
            }
            i += 1;
        }
        ev
    }

    /// The per-slot update 0x140f28080.
    fn update_target_slot(&mut self, w: usize, i: usize, view: &TargetView, ev: &mut Vec<TargetEvent>) {
        let now = self.time_ms;
        let mode = self.mstate[w].fire_mode;
        let Some(mode_decl) = self.mode_def(w, mode) else { return };
        // The slot's lock data: its locked copy, else the mode decl's (normal / zoomed).
        let lock = match self.targeting[w].slots[i].locked_lock_data.clone() {
            Some(l) => l,
            None => match self.target_lock_data(w, mode) {
                Some(l) => l,
                None => return,
            },
        };
        let ms = &self.mstate[w];
        let was_active = self.targeting[w].slots[i].active;
        if !was_active && ms.override_fire_mode != ms.override_prev_mode {
            return;
        }
        let ms_slots = &self.applied[w].modes;
        let mut keep = ms_slots[mode].target_unbreakable_lock;
        let start_state = self.targeting[w].slots[i].state;
        let mut broken = false;

        let locking_mode = |a: &Arsenal, m: usize| a.mode_def(w, m).is_some_and(|d| d.target_lock_normal.can_lock) || a.applied[w].modes[m].projectile_lock;
        let mut go_main = true;
        if !locking_mode(self, mode) {
            // A mode that cannot lock: leaving a locking mode drops the lock unless the lock data keeps it
            // (automaticallyMaintain / InitiateLock with any slot LOCKED).
            let last = self.targeting[w].slots[i].last_fire_mode;
            if mode as i32 != last && last != -1 && locking_mode(self, last as usize) {
                let keep_any = (lock.automatically_maintain_lock || lock.automatically_initiate_lock) && self.targeting[w].slots.iter().any(|s| s.state == TargetState::Locked);
                if !keep_any {
                    self.clear_target_slot(w, Some(i));
                }
            }
            self.targeting[w].slots[i].wait_for_deactivation_release = false;
            if !self.targeting[w].slots[i].active {
                self.after_inactive(w, &lock, i);
            }
        } else if mode as i32 == self.targeting[w].slots[i].last_fire_mode {
            let ms = &self.mstate[w];
            if ms.override_fire_mode == ms.override_prev_mode {
                if !self.targeting[w].slots[i].active {
                    self.after_inactive(w, &lock, i);
                }
            } else if was_active {
                if self.targeting[w].slots[i].state != TargetState::Locked {
                    self.clear_target_slot(w, Some(i));
                }
                if !self.targeting[w].slots[i].active {
                    self.after_inactive(w, &lock, i);
                }
            } else {
                self.after_inactive(w, &lock, i);
            }
        } else {
            // Into a locking mode: the slot toggles on (needs ammo for the mode, 0x140f166b0), or an active
            // slot restarts.
            let s = &mut self.targeting[w].slots[i];
            s.active = !was_active;
            if !was_active {
                if !self.has_ammo_for_mode(w, mode) {
                    self.targeting[w].slots[i].active = false;
                    self.after_inactive(w, &lock, i);
                }
            } else {
                s.target_start_time = 0;
                s.lock_start_time = 0;
                s.out_of_fov_time = 0;
                s.prev_state = s.state;
                s.state = TargetState::None;
                s.locked_lock_data = None;
                s.lock_percent = 0.0;
                s.candidate = None;
                s.los_query = None;
                self.mstate[w].override_fire_mode = -1;
                self.targeting[w].slots[i].active = true;
            }
        }
        self.targeting[w].slots[i].last_fire_mode = mode as i32;

        if !self.targeting[w].slots[i].active {
            // An inactive slot still holding a target is cleared; a LOCKED one counts as broken.
            if self.targeting[w].slots[i].state == TargetState::Locked {
                broken = true;
            }
            if self.targeting[w].slots[i].state != TargetState::None {
                self.clear_target_slot(w, Some(i));
            }
            go_main = false;
        } else if !mode_decl.target_lock_normal.can_lock && !self.mode_def(w, 1).is_some_and(|d| d.target_lock_normal.can_lock) {
            // 0x140f2849c: the active slot needs a decl that can lock: the mode's, else mode 1's (DECL_WEAPON
            // +0x1a28, else the base's secondaryFireDecl).
            self.clear_target_slot(w, Some(i));
            go_main = false;
        }

        if go_main {
            // Candidate validation: the current candidate stays while its origin is inside lockFOV / 2 of the view
            // and the solid trace to it is clear (TARGET_UNBREAKABLE_LOCK keeps it without the test).
            let cand = self.targeting[w].slots[i].candidate;
            let mut valid: Option<u32> = None;
            if !keep {
                if let Some(c) = cand.and_then(|c| view.get(c)) {
                    if !lock.players_block_line_of_sight {
                        let dir = normalize(c.origin - view.eye);
                        let ang = acos_clamped(view.forward.dot(dir)) * 57.295776;
                        if ang <= lock.lock_fov * 0.5 && c.origin_visible {
                            valid = Some(c.id);
                            keep = !lock.auto_break_lock;
                        }
                    }
                }
            } else {
                valid = cand;
            }
            if valid.is_none() {
                valid = self.best_lock_target(w, cand, view);
            }

            // The LOS clip query of last frame (eye -> the candidate's AIMPOINT_CENTER): blocked by something
            // else starts the out-of-LOS timer.
            let s = &mut self.targeting[w].slots[i];
            match s.candidate.and_then(|c| view.get(c)) {
                Some(c) => {
                    if let Some(blocked) = s.los_query.take() {
                        if !blocked {
                            s.los_expire_time = 0;
                        } else if s.los_expire_time == 0 {
                            s.los_expire_time = now + (TICKS_PER_SEC * lock.out_of_los_time_sec) as i32;
                        }
                    }
                    s.los_query = Some(!c.center_visible);
                }
                None => s.los_expire_time = 0,
            }

            if self.targeting[w].slots[i].state == TargetState::None {
                let s = &self.targeting[w].slots[i];
                if now < s.can_target_time {
                    self.clear_target_slot(w, Some(i));
                    if mode as i32 == self.targeting[w].slots[i].last_fire_mode {
                        self.targeting[w].slots[i].active = true;
                    }
                } else if let (Some(v), false) = (valid, s.wait_for_deactivation_release) {
                    let s = &mut self.targeting[w].slots[i];
                    s.prev_state = s.state;
                    s.state = TargetState::Acquiring;
                    s.target_start_time = now;
                    s.candidate = Some(v);
                    if !self.fire_mode_forced(w) && self.mstate[w].override_fire_mode != -1 {
                        self.restore_fire_mode(w);
                    }
                }
            } else {
                self.update_held_slot(w, i, &lock, &mode_decl, valid, keep, view, &mut broken);
                // 0x140f29103: a target that is gone or dead (not offered any more) ends the slot.
                let s = &self.targeting[w].slots[i];
                if s.state != TargetState::None && s.candidate.and_then(|c| view.get(c)).is_none_or(|t| !t.eligible) {
                    self.clear_target_slot(w, Some(i));
                }
            }
        }

        // Sounds on the state change (0x140f2912d..).
        let end_state = self.targeting[w].slots[i].state;
        let snd = match (start_state, end_state) {
            (TargetState::None, TargetState::Acquiring) => Some(&lock.sound_acquiring),
            (TargetState::Acquiring, TargetState::Locked) => Some(&lock.sound_locked),
            (TargetState::Locked, TargetState::None) if broken => Some(&lock.sound_lock_broken),
            (TargetState::Locked, TargetState::None) | (TargetState::Acquiring, TargetState::None) => Some(&lock.sound_lock_disengage),
            _ => None,
        };
        if start_state != end_state {
            ev.push(TargetEvent::State { slot: i, from: start_state, to: end_state, target: self.targeting[w].slots[i].candidate });
        }
        if let Some(s) = snd.filter(|s| !s.is_empty()) {
            ev.push(TargetEvent::Sound(s.clone()));
        }
    }

    /// 0x140f28406 / 0x140f2843c: an inactive slot drops the override once no slot is active, and
    /// automaticallyInitiateLock re-activates it.
    fn after_inactive(&mut self, w: usize, lock: &TargetLockData, i: usize) {
        if self.mstate[w].override_fire_mode != -1 && !self.targeting[w].slots.iter().any(|s| s.active) {
            self.restore_fire_mode(w);
        }
        if lock.automatically_initiate_lock {
            self.clear_target_slot(w, Some(i));
            self.targeting[w].slots[i].active = true;
        }
    }

    /// 0x140f16b30: any slot is LOCKED.
    fn fire_mode_forced(&self, w: usize) -> bool {
        self.targeting[w].slots.iter().any(|s| s.state == TargetState::Locked)
    }

    /// 0x140f166b0: the mode's ammo covers a shot (ammoPerShot / the AMMO_TO_USE item).
    fn has_ammo_for_mode(&self, w: usize, mode: usize) -> bool {
        let Some(d) = self.mode_def(w, mode) else { return false };
        match self.pool_for(&d) {
            None => true,
            Some(p) => self.pools[p].count >= d.ammo_per_shot.max(1),
        }
    }

    /// ACQUIRING / LOCKED (0x140f28a4c..0x140f2912d).
    #[allow(clippy::too_many_arguments)]
    fn update_held_slot(&mut self, w: usize, i: usize, lock: &TargetLockData, mode_decl: &super::decl::WeaponDef, valid: Option<u32>, keep: bool, view: &TargetView, broken: &mut bool) {
        let now = self.time_ms;
        let mode = self.mstate[w].fire_mode;
        let slots = self.applied[w].modes[mode].clone();
        let s = &mut self.targeting[w].slots[i];
        s.unlock_percent = 0.0;
        if valid.is_some() && valid == s.candidate {
            s.out_of_fov_time = now;
        } else {
            // Lost: an ACQUIRING slot at once; a LOCKED one once out of FOV for outOfFovTimeSec
            // (TARGET_OUT_OF_FOV_TIME_SEC first) or past the LOS expiry, unless the lock is kept.
            let mut lose = s.state == TargetState::Acquiring;
            if !keep && 0 < s.los_expire_time && s.los_expire_time <= now {
                lose = true;
            }
            let fov_time = if slots.target_out_of_fov_time_sec < 0.0 { lock.out_of_fov_time_sec } else { slots.target_out_of_fov_time_sec };
            let limit = (TICKS_PER_SEC * fov_time) as i32;
            if s.out_of_fov_time == 0 {
                s.out_of_fov_time = now;
            } else if !keep && now - s.out_of_fov_time >= limit {
                lose = true;
            }
            if lose {
                *broken = true;
                self.clear_target_slot(w, Some(i));
            }
            // Read after the clear, as the exe does (the reset slot's outOfFovTime is 0).
            let s = &mut self.targeting[w].slots[i];
            s.unlock_percent = (now - s.out_of_fov_time) as f32 / limit as f32;
        }
        let s = &mut self.targeting[w].slots[i];
        if s.state == TargetState::Acquiring {
            // lockPercent over TARGET_LOCK_TIME (else lockTimeSec) times the owner's stat scale (identity here).
            let t = if slots.target_lock_time_sec < 0.0 { lock.lock_time_sec } else { slots.target_lock_time_sec };
            let pct = if t == 0.0 {
                1.0
            } else {
                let p = (now - s.target_start_time) as f32 / ((TICKS_PER_SEC * t) as i32) as f32;
                if 1.0 <= p {
                    1.0
                } else if p <= 0.0 {
                    0.0
                } else {
                    p
                }
            };
            s.lock_percent = pct;
            if pct == 1.0 {
                s.prev_state = s.state;
                s.state = TargetState::Locked;
                s.lock_start_time = now;
                s.out_of_fov_time = 0;
                s.locked_lock_data = Some(lock.clone());
                s.enemy = s.candidate;
                // clearAfterNumShots = min(ammo / ammoPerShot, the mode's lock data clearAfterNumShots).
                let per = mode_decl.ammo_per_shot.max(1);
                let ammo = self.pool_for(mode_decl).map(|p| self.pools[p].count).unwrap_or(i32::MAX);
                let clear = self.target_lock_data(w, mode).map(|l| l.clear_after_num_shots).unwrap_or(0);
                self.targeting[w].slots[i].clear_after_num_shots = (ammo / per).min(clear);
                if mode_decl.force_fire_mode_when_locked {
                    self.force_fire_mode(w, mode);
                }
                // The next slot may start after nextTargetTimeoutSec.
                let n = self.max_targets(w).min(3);
                if ((i + 1) as i32) < n && self.targeting[w].slots[i + 1].state == TargetState::None {
                    let to = (TICKS_PER_SEC * lock.next_target_timeout_sec) as i32;
                    let next = &mut self.targeting[w].slots[i + 1];
                    next.target_timeout = to;
                    next.can_target_time = now + to;
                }
            }
        }
        // unlockTimeSec (0x140f28eb0): a lock held that long lets go of its target; with lockTimeSec 0 a fresh
        // best-target query that picks the same target relocks it at once, anything else breaks the lock.
        let s = &self.targeting[w].slots[i];
        if !keep && 0 < s.lock_start_time && ((TICKS_PER_SEC * lock.unlock_time_sec) as i32) <= now - s.lock_start_time {
            let cur = s.candidate;
            self.targeting[w].slots[i].candidate = None;
            if lock.lock_time_sec == 0.0 && cur.is_some() && self.best_lock_target(w, None, view) == cur {
                let s = &mut self.targeting[w].slots[i];
                s.lock_start_time = now;
                s.candidate = cur;
            } else {
                *broken = true;
                self.clear_target_slot(w, Some(i));
            }
        }
    }

    /// The best-target query 0x140f0b890(weapon, current): eligible actors with one of the 7 target points
    /// inside lockFOV / 2 (TARGET_LOCK_FOV first) of the view; the current target wins while it qualifies, else
    /// (weapon_useTargetingVer 2) the first qualifier, replaced by a later one that is no farther (bounds
    /// centre distance, within lockMaxDist) and either crossed by the horizontal view line (0x140287c20) and
    /// closer, or on the view's side of the XY bisector between the two.
    pub fn best_lock_target(&self, w: usize, current: Option<u32>, view: &TargetView) -> Option<u32> {
        let ms = &self.mstate[w];
        let mode = if ms.override_fire_mode != -1 { ms.override_fire_mode as usize } else { ms.fire_mode };
        let lock = self.targeting[w].slots[0].locked_lock_data.clone().or_else(|| self.target_lock_data(w, ms.fire_mode))?;
        let o = self.applied[w].modes[mode.min(1)].target_lock_fov;
        let fov = if o < 0.0 { lock.lock_fov } else { o };
        let mut best_dist = if lock.lock_max_dist <= 0.0 { 1e30 } else { lock.lock_max_dist };
        // The horizontal view direction (z dropped, renormalised).
        let fwd2 = normalize(Vec3::new(view.forward.x, view.forward.y, 0.0));
        let fwd2 = Vec3::new(fwd2.x, fwd2.y, 0.0);
        let mut best: Option<&LockTarget> = None;
        for t in view.targets {
            if !t.eligible {
                continue;
            }
            if Some(t.id) != current && self.targeting[w].slots.iter().any(|s| s.candidate == Some(t.id)) {
                continue;
            }
            let inside = TEST_POINTS.iter().any(|&p| {
                let d = normalize(view.eye - t.aim_points[p]);
                let a = acos_clamped(d.dot(view.forward));
                fov * 0.5 >= 180.0 - a * 57.295776
            });
            if !inside {
                continue;
            }
            if Some(t.id) == current {
                return current;
            }
            let dist = (bounds_center(t.bounds) - view.eye).length();
            let hit = line_hits_bounds(t.bounds.0, t.bounds.1, view.eye, fwd2);
            match best {
                None => {
                    best = Some(t);
                    if hit {
                        best_dist = dist;
                    }
                }
                Some(b) => {
                    let closer = hit && dist < best_dist;
                    if closer {
                        best_dist = dist;
                    }
                    let db = normalize(bounds_center(b.bounds) - view.eye);
                    let dc = normalize(bounds_center(t.bounds) - view.eye);
                    let bis = normalize(Vec3::new(dc.x + db.x, dc.y + db.y, 0.0));
                    let bis = Vec3::new(bis.x, bis.y, 0.0);
                    let n = bis.cross(Vec3::Z);
                    let sv = n.dot(fwd2);
                    let sc = n.dot(dc);
                    if ((0.0 < sv && 0.0 < sc) || (sv < 0.0 && sc < 0.0) || closer) && dist <= best_dist {
                        best = Some(t);
                    }
                }
            }
        }
        best.map(|b| b.id)
    }

    // ---- fire path ----

    /// InitForFire 0x140f179a0 ~0x140f18068: the projectile seeks the target of slot targetingShotIndex when a
    /// slot is active and something is locked; the index then advances to the next slot with a target
    /// (wrapping past maxTargets).
    pub fn shot_target(&mut self, w: usize) -> Option<u32> {
        if !self.targeting[w].slots.iter().any(|s| s.active) || self.locked_count(w) == 0 {
            return None;
        }
        let max = self.max_targets(w);
        let t = &mut self.targeting[w];
        let start = t.shot_index;
        let target = t.slots[start.min(SLOTS - 1)].candidate;
        let mut idx = start;
        loop {
            idx += 1;
            if max < idx as i32 || idx >= SLOTS {
                idx = 0;
            }
            if t.slots[idx].candidate.is_some() || idx == start {
                break;
            }
        }
        t.shot_index = idx;
        target
    }

    /// FinishFire 0x140f0f53f..0x140f0f72d: with any slot LOCKED, slot 0's clearAfterNumShots counts the shot;
    /// at 0 a loseLockOnFire lock clears every slot (and the fire-mode override), and targeting may restart
    /// after TARGET_RECOVERY_SEC (else the lock data's lockTimeoutSec).
    pub fn targeting_finish_fire(&mut self, w: usize, mode: usize) {
        let now = self.time_ms;
        if !self.targeting[w].slots.iter().any(|s| s.state == TargetState::Locked) {
            return;
        }
        let s0 = &mut self.targeting[w].slots[0];
        if 0 < s0.clear_after_num_shots {
            s0.clear_after_num_shots -= 1;
        }
        if s0.clear_after_num_shots != 0 {
            return;
        }
        let lock = self.target_lock_data(w, mode).unwrap_or_default();
        if lock.lose_lock_on_fire {
            self.clear_target_slot(w, None);
        }
        let o = self.applied[w].modes[mode].target_recovery_sec;
        // INTERIM: the non-loseLockOnFire branch's millisecond source ([rsp+0x70]) is not traced; 0 here.
        let secs = if 0.0 <= o {
            o
        } else if lock.lose_lock_on_fire {
            lock.lock_timeout_sec
        } else {
            self.targeting[w].slots[0].can_target_time = 0;
            0.0
        };
        let s0 = &mut self.targeting[w].slots[0];
        s0.target_timeout = (TICKS_PER_SEC * secs) as i32;
        s0.can_target_time = now + s0.target_timeout;
        self.targeting[w].shot_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_hits_bounds_is_a_two_sided_slab_test() {
        let (lo, hi) = (Vec3::new(10.0, -1.0, 0.0), Vec3::new(12.0, 1.0, 2.0));
        assert!(line_hits_bounds(lo, hi, Vec3::new(0.0, 0.0, 1.0), Vec3::X));
        // Behind the start counts too (t is not clamped at 0).
        assert!(line_hits_bounds(lo, hi, Vec3::new(20.0, 0.0, 1.0), Vec3::X));
        // A horizontal line above the box misses (z component 0, start outside the z slab).
        assert!(!line_hits_bounds(lo, hi, Vec3::new(0.0, 0.0, 3.0), Vec3::X));
        assert!(!line_hits_bounds(lo, hi, Vec3::new(0.0, 5.0, 1.0), Vec3::X));
    }
}
