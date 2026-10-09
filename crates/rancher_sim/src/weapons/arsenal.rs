//! The player's weapons and the per-frame fire state machine (idHands/idWeapon/idPlayer behaviour
//! decoded from DOOMx64.exe; addresses in gamedata/re/WEAPONS.md).
//!
//! Each frame, in the game's order: idPlayer::UpdateSpread, bring-up/down, weapon think (heat, chaingun
//! barrel), trigger latch + ProcessTriggers, fire (InitForFire/launch/FinishFire, idPlayer::WeaponFired),
//! then idPlayer::UpdateWeaponKick.
//! INFERRED (anim web not decoded): a shot happens on the first frame the trigger is pulled and
//! idWeapon::CanFire passes, i.e. the shoot state's fire event is taken to sit at its first frame;
//! weapons become ready exactly when the bring-up duration has elapsed.

use std::sync::Arc;

use glam::Vec3;

use super::decl::{ChaingunData, WeaponDef};
use super::kick::{KickComponent, OldKick, ViewKick};
use super::spread::{shot_direction, PlayerSpread, SpreadInput};
use super::GameRng;

/// g_weaponChangeMinIntervalMS.
pub const WEAPON_CHANGE_MIN_INTERVAL_MS: i32 = 375;
/// g_weaponKickBackRatio (FinishFire knockback split).
pub const WEAPON_KICK_BACK_RATIO: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WeaponPhase {
    Raising { left: f32 },
    Ready,
    Lowering { left: f32, next: usize },
}

/// Chaingun barrel states (idChaingun::idChaingunBarrelState).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarrelState {
    Resetting = 0,
    #[default]
    Stopped = 1,
    Accel = 2,
    Decel = 3,
    Aligning = 4,
}

/// idChaingun barrel (weapon+0x1c04 angle, +0x1c0c deg/s, +0x1c10 spin fraction, +0x1c58 state).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Barrel {
    pub angle: f32,
    pub vel: f32,
    pub spin: f32,
    pub state: BarrelState,
    decel_rate: f32,
}

impl Barrel {
    /// idChaingun barrel update 0x140ecba40. `spin_request`: trigger pulled while the hands allow it.
    pub fn update(&mut self, sec: f32, spin_request: bool, data: &ChaingunData, def: &WeaponDef) {
        let state_ok = (self.state as i32) & !4 != 0;
        match self.state {
            BarrelState::Resetting => {
                // Barrel angle reset (resetBarrelAngleOnFireModeChange); unused by the SP chaingun.
                self.angle = 0.0;
                self.vel = 0.0;
                self.spin = 0.0;
                self.state = BarrelState::Stopped;
            }
            _ if spin_request && state_ok => {
                self.state = BarrelState::Accel;
                let up = data.spin_up_ms;
                if self.spin >= 1.0 {
                } else if up as f32 == 0.0 {
                    self.spin = 1.0;
                } else {
                    self.spin += sec * (1.0 / (up as f32 * 0.001));
                    if 1.0 <= self.spin {
                        self.spin = 1.0;
                    }
                }
                let interval = def.firing_interval_at(self.spin);
                self.vel = ((1.0 / (interval as f32 * 0.001)) / data.num_barrels as f32) * 360.0;
                self.angle += sec * self.vel;
            }
            BarrelState::Stopped => {}
            _ => {
                // Runs every frame while decelerating or aligning (the exe re-enters DECEL each time).
                let rate = 1.0 / (data.spin_down_ms as f32 * 0.001);
                if self.state != BarrelState::Decel {
                    self.decel_rate = rate * self.vel;
                }
                self.state = BarrelState::Decel;
                let align = data.barrel_align_degs_per_sec.max(0.0);
                self.spin = (self.spin - sec * rate).max(0.0);
                self.vel = (self.vel - sec * self.decel_rate).max(0.0);
                if self.vel <= align {
                    self.state = BarrelState::Aligning;
                }
                if self.state == BarrelState::Aligning {
                    let a = data.barrel_align_degs_per_sec.abs();
                    if a != 0.0 {
                        let sector = 360.0 / data.barrel_align_sectors;
                        let (target, reached) = if data.barrel_align_degs_per_sec > 0.0 {
                            let t = (self.angle / sector).ceil() * sector;
                            self.angle += sec * a;
                            (t, self.angle >= t)
                        } else {
                            let t = (self.angle / sector).floor() * sector;
                            self.angle -= sec * a;
                            (t, self.angle <= t)
                        };
                        if !reached {
                            return self.wrap();
                        }
                        self.angle = target;
                    }
                    self.vel = 0.0;
                    self.spin = 0.0;
                    self.state = BarrelState::Stopped;
                } else {
                    self.angle += sec * self.vel;
                }
            }
        }
        self.wrap();
    }

    fn wrap(&mut self) {
        if 360.0 <= self.angle || self.angle < 0.0 {
            self.angle -= (self.angle * 0.002_777_777_8).floor() * 360.0;
        }
    }
}

/// Per-weapon runtime state (each idWeapon instance in the inventory).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WeaponState {
    /// weapon+0x914: game time the weapon may fire again.
    pub next_fire: i32,
    /// weapon+0x92c canAttackTime[mode]: no shot of that mode before it (FIRE_DELAY on SetFireMode).
    pub fire_delay_until: [i32; 2],
    /// weapon+0x934 canMoveTime[mode] (MOVEMENT_DELAY on SetFireMode; read by the player movement).
    pub can_move_time: [i32; 2],
    /// weapon+0x1540 heat 0..1, +0x153c glow, +0x1548 cooling delay ms, +0x1544 overheat timer ms.
    pub heat: f32,
    pub glow: f32,
    pub cool_delay: i32,
    pub overheat: i32,
    pub barrel: Barrel,
    /// weapon+0x91c: game time of the last shot (chaingun settle blend).
    pub last_fire: i32,
    /// weapon+0xdc8 lastFinishFireTime[mode].
    pub last_finish_fire: [i32; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct AmmoPool {
    pub key: String,
    pub count: i32,
    pub max: i32,
}

/// What the player feeds the weapon code each frame.
#[derive(Debug, Clone, Copy)]
pub struct WeaponInput {
    pub trigger: bool,
    pub spread: SpreadInput,
    /// Fire axis rows: forward is `spread.view_forward`; left and up complete idAngles::ToMat3.
    pub left: Vec3,
    pub up: Vec3,
    /// g_useGaussianAimSpread (default 1).
    pub gaussian_spread: bool,
    /// BUTTON_ALTFIRE held (the secondary fire mode's trigger, weapons::charge).
    pub altfire: bool,
}

impl Default for WeaponInput {
    fn default() -> Self {
        Self { trigger: false, spread: SpreadInput::default(), left: Vec3::Y, up: Vec3::Z, gaussian_spread: true, altfire: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub weapon: usize,
    pub time_ms: i32,
    /// idPlayer::GetSpread at the shot (before this shot's addition).
    pub spread_deg: f32,
    /// One direction per damage trace / projectile (spawnCount).
    pub dirs: Vec<Vec3>,
    pub ammo_used: i32,
    pub next_fire_ms: i32,
    /// weaponFeedBack.weaponKnockback (game units; FinishFire splits it with g_weaponKickBackRatio).
    pub knockback: f32,
    /// Fire mode of the shot and its decl (projectile, damage, fire sound come from it).
    pub mode: usize,
    pub def: ShotDef,
    /// Use-time damage scale (WMT DAMAGE_SCALE and the charge's DAMAGE_SCALE item); the damage event's scale.
    pub damage_scale: f32,
    /// Burst shots still to come after this one.
    pub burst_left: i32,
    /// chargePercent when the shot was fired.
    pub charge: f32,
    /// The entity its projectiles seek (InitForFire: the targeting slot's lock target, weapons::targeting).
    pub target: Option<u32>,
}

/// The decl a shot was fired with (compared by identity).
#[derive(Debug, Clone)]
pub struct ShotDef(pub Arc<WeaponDef>);

impl PartialEq for ShotDef {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

impl std::ops::Deref for ShotDef {
    type Target = WeaponDef;
    fn deref(&self) -> &WeaponDef {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WeaponEvent {
    Fired(Shot),
    DryFire { weapon: usize },
    Overheated { weapon: usize },
    Raised { weapon: usize },
    Lowered { weapon: usize, next: usize },
    /// idWeapon::StopFireSound 0x140f23a80 (hands driver only): stop the weapon's looping fire sound and
    /// post its projectile's stopFireSound. Sent on ReleaseTrigger (PULLED -> RELEASED), after a shot fired
    /// with the trigger already released (FinishFire), and when a looping dry-fire state starts.
    StopFireSound { weapon: usize },
    /// Charge feedback (weapons::charge): a sound to post (start / interval / fully charged).
    ChargeSound { weapon: usize, sound: String },
    /// The charge sound channel stops.
    ChargeStop { weapon: usize },
    /// The fire mode changed (SetFireMode 0x140d698b0).
    FireMode { weapon: usize, mode: usize },
    /// ExplodeLaunchedProjectiles (weapons::detonate): explode these projectiles (game side ids) where they are,
    /// then post `sound` (empty = none).
    Detonate { weapon: usize, projectiles: Vec<u32>, sound: String },
    /// A weapon sound to post at the player (explodeProjectilesDenialSound).
    Sound { weapon: usize, sound: String },
}

/// FinishFire's interval conversion: the game time manager's vslot +0x168 (0x140362810) turns milliseconds
/// into game ticks as (int)(ticksPerSecond 960 * (ms * 0.001f)), e.g. 144 ms -> 138 ticks, 850 -> 816.
pub fn ms_to_ticks(ms: i32) -> i32 {
    (960.0f32 * (ms as f32 * 0.001f32)) as i32
}

/// Compatibility view of the kick for renderers: [0] pitch up (+), [1] yaw, [2] FOV narrowing (+).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KickState {
    pub value: f32,
}

pub struct Arsenal {
    pub defs: Vec<Arc<WeaponDef>>,
    /// Ammo available to each weapon (its pool's count), refreshed every tick.
    pub ammo: Vec<i32>,
    pub pools: Vec<AmmoPool>,
    pool_of: Vec<Option<usize>>,
    pub current: usize,
    pub phase: WeaponPhase,
    /// Milliseconds until the current weapon may fire again (compatibility; from next_fire).
    pub cooldown_ms: f32,
    pub trigger_was_down: bool,
    pub rng: GameRng,
    pub kick: [KickState; 3],
    pub view_kick: ViewKick,
    pub last_shot_dirs: Vec<Vec3>,
    pub time_ms: i32,
    pub states: Vec<WeaponState>,
    pub spread: PlayerSpread,
    pub kick_new: KickComponent,
    pub kick_old: OldKick,
    /// hands+0xfaf0: trigger must be released before the next pull (single tap, dry fire).
    pub latch: bool,
    /// A pull that arrived early on an allowShotQueueing weapon.
    pub queued: bool,
    /// weapon+0x8f8 for the current weapon: true = TRIGGERSTATE_PULLED.
    pub trigger_pulled: bool,
    pub next_change_ms: i32,
    pub last_fire_ms: Option<i32>,
    carry_ms: f32,
    /// Mods per weapon (weapons::mods / weapons::charge): owned / active perks, the applied upgrade slots,
    /// each fire mode's decl and the fire-mode / charge / burst runtime.
    pub loadouts: Vec<super::mods::ModLoadout>,
    pub applied: Vec<super::mods::Applied>,
    pub mode_defs: Vec<[Option<Arc<WeaponDef>>; 2]>,
    pub mstate: Vec<super::charge::ModState>,
    /// weapon+0x1bf8: the next unhide plays the mod-select anim (weapons::modswitch).
    pub mod_select_pending: Vec<bool>,
    /// idRailGun members per weapon (weapons::railgun).
    pub railgun: Vec<super::railgun::RailGunState>,
    /// weapon+0x14a0 bit 0 for the current weapon: zoomed (from the frame's spread input).
    pub zoomed: bool,
    /// Per weapon and fire mode: the decl view weapons::zoom reads (Arsenal::zoom_def).
    pub zoom_defs: Vec<[Option<Arc<WeaponDef>>; 2]>,
    altfire_was_down: bool,
    /// The lock-on slots per weapon (weapons::targeting), the world the game side offers them each frame and
    /// what they report (lock sounds / state changes; the game side drains it).
    pub targeting: Vec<super::targeting::Targeting>,
    pub target_world: super::targeting::TargetWorld,
    pub target_events: Vec<super::targeting::TargetEvent>,
    /// The launched-projectile list per weapon (weapons::detonate).
    pub launched: Vec<super::detonate::Launched>,
}

impl Arsenal {
    pub fn new(defs: Vec<Arc<WeaponDef>>) -> Self {
        let mut pools: Vec<AmmoPool> = Vec::new();
        let mut pool_of = Vec::new();
        for d in &defs {
            if d.ammo_pool.is_empty() || d.infinite_ammo {
                pool_of.push(None);
                continue;
            }
            let idx = match pools.iter().position(|p| p.key == d.ammo_pool) {
                Some(i) => i,
                None => {
                    // Testbed: start every pool full.
                    pools.push(AmmoPool { key: d.ammo_pool.clone(), count: d.ammo_max.max(d.ammo_start), max: d.ammo_max });
                    pools.len() - 1
                }
            };
            pool_of.push(Some(idx));
        }
        let start = defs.iter().position(|d| d.decl.ends_with("/shotgun")).unwrap_or(0);
        let up = defs.get(start).map(|d| d.bringup_s).unwrap_or(0.0);
        let n = defs.len();
        let mut a = Self {
            defs,
            ammo: vec![0; n],
            pools,
            pool_of,
            current: start,
            phase: WeaponPhase::Raising { left: up },
            cooldown_ms: 0.0,
            trigger_was_down: false,
            rng: GameRng(0x1234_5678),
            kick: Default::default(),
            view_kick: ViewKick::default(),
            last_shot_dirs: Vec::new(),
            time_ms: 0,
            // weapon+0x91c starts long before the first frame (the game clock is well past 0 at spawn).
            states: vec![WeaponState { last_fire: -1_000_000, ..WeaponState::default() }; n],
            spread: PlayerSpread::default(),
            kick_new: KickComponent::default(),
            kick_old: OldKick::default(),
            latch: false,
            queued: false,
            trigger_pulled: false,
            next_change_ms: 0,
            last_fire_ms: None,
            carry_ms: 0.0,
            loadouts: Vec::new(),
            applied: vec![Default::default(); n],
            mode_defs: vec![[None, None]; n],
            mstate: vec![Default::default(); n],
            mod_select_pending: vec![false; n],
            railgun: vec![Default::default(); n],
            zoomed: false,
            zoom_defs: vec![[None, None]; n],
            altfire_was_down: false,
            targeting: vec![Default::default(); n],
            target_world: Default::default(),
            target_events: Vec::new(),
            launched: vec![Default::default(); n],
        };
        a.loadouts = a.defs.iter().map(|d| d.mods.as_deref().map(super::mods::ModLoadout::new).unwrap_or_default()).collect();
        for w in 0..n {
            a.rebuild_mods(w);
        }
        a.sync_compat();
        a
    }

    pub fn def(&self) -> &Arc<WeaponDef> {
        &self.defs[self.current]
    }

    /// The pool a decl draws from (None: infinite ammo or no ammo decl).
    pub fn pool_for(&self, d: &WeaponDef) -> Option<usize> {
        if d.ammo_pool.is_empty() || d.infinite_ammo {
            return None;
        }
        self.pools.iter().position(|p| p.key == d.ammo_pool)
    }

    /// A mod decl's ammo pool that no base weapon shares joins the pools, full.
    pub(crate) fn ensure_pool(&mut self, d: &WeaponDef) {
        if !d.ammo_pool.is_empty() && !d.infinite_ammo && self.pool_for(d).is_none() {
            self.pools.push(AmmoPool { key: d.ammo_pool.clone(), count: d.ammo_max.max(d.ammo_start), max: d.ammo_max });
        }
    }

    pub fn ammo_for(&self, weapon: usize) -> Option<i32> {
        self.pool_of[weapon].map(|p| self.pools[p].count)
    }

    /// Enough ammo for a shot of the weapon's current fire mode.
    pub fn has_ammo(&self, weapon: usize) -> bool {
        let d = self.wdef(weapon);
        match self.pool_for(&d) {
            None => true,
            Some(p) => d.ammo_per_shot <= 0 || self.pools[p].count >= d.ammo_per_shot,
        }
    }

    /// Request a weapon change (bring-down of the current weapon, then bring-up of `idx`).
    pub fn select(&mut self, idx: usize) {
        if idx >= self.defs.len() || idx == self.current {
            return;
        }
        if let WeaponPhase::Lowering { .. } = self.phase {
            return;
        }
        if self.time_ms < self.next_change_ms {
            return;
        }
        self.next_change_ms = self.time_ms + WEAPON_CHANGE_MIN_INTERVAL_MS;
        self.phase = WeaponPhase::Lowering { left: self.def().bringdown_s, next: idx };
    }

    /// A switch issued by weapons::select::WeaponSelect (which applies g_weaponChangeMinIntervalMS itself):
    /// bring the current weapon down and `idx` up; a new target during a bring-down replaces the old one
    /// (SetPendingActionWeapon_ 0x140d689f0 overrides the pending BRINGDOWN item).
    pub fn switch_to(&mut self, idx: usize) {
        if idx >= self.defs.len() {
            return;
        }
        match self.phase {
            WeaponPhase::Lowering { left, .. } => self.phase = WeaponPhase::Lowering { left, next: idx },
            _ if idx == self.current => {}
            _ => self.phase = WeaponPhase::Lowering { left: self.def().bringdown_s, next: idx },
        }
    }

    /// idWeapon::CanFire 0x140f19810 for the current fire mode (ammo checked separately), plus the charge
    /// fire gate (0x140f19b90 / 0x140f1afe0, weapons::charge).
    pub fn can_fire(&self, weapon: usize, now: i32) -> bool {
        let d = self.wdef(weapon);
        let st = &self.states[weapon];
        let mode = self.mstate[weapon].fire_mode;
        if !d.ignore_firing_interval && now < self.next_fire(weapon, mode) {
            return false;
        }
        if 0 < st.overheat {
            return false;
        }
        now >= st.fire_delay_until[mode] && self.charge_gate(weapon, now, true)
    }

    /// Compatibility wrapper: advance `ms` with only the trigger as input; true when a shot was fired.
    pub fn update(&mut self, ms: f32, trigger: bool) -> bool {
        self.carry_ms += ms;
        let whole = self.carry_ms.floor();
        self.carry_ms -= whole;
        let input = WeaponInput { trigger, ..Default::default() };
        self.tick(whole as i32, &input).iter().any(|e| matches!(e, WeaponEvent::Fired(_)))
    }

    /// Advance one game frame of `ms` milliseconds.
    pub fn tick(&mut self, ms: i32, inp: &WeaponInput) -> Vec<WeaponEvent> {
        let mut events = Vec::new();
        self.time_ms += ms;
        let now = self.time_ms;
        let sec = ms as f32 * 0.001;
        // Bring-up/down clips play desiredBring*DurationSecs of anim time; anim time runs 960 game ticks per
        // second (idTypesafeTime<int, 960>), frame seconds (heat, barrel) are msec * 0.001.
        let anim_sec = ms as f32 / 960.0;
        let cur = self.current;
        let def = self.defs[cur].clone();
        self.zoomed = inp.spread.zoomed;

        self.spread.update(now, &def.spread_params, &inp.spread);

        match self.phase {
            WeaponPhase::Raising { left } => {
                let left = left - anim_sec;
                if left <= 0.0 {
                    self.phase = WeaponPhase::Ready;
                    events.push(WeaponEvent::Raised { weapon: cur });
                } else {
                    self.phase = WeaponPhase::Raising { left };
                }
            }
            WeaponPhase::Lowering { left, next } => {
                let left = left - anim_sec;
                if left <= 0.0 {
                    events.push(WeaponEvent::Lowered { weapon: cur, next });
                    self.set_weapon(next);
                } else {
                    self.phase = WeaponPhase::Lowering { left, next };
                }
            }
            WeaponPhase::Ready => {}
        }
        let cur = self.current;
        let ready = self.phase == WeaponPhase::Ready;

        if self.update_heat(cur, ms, sec) {
            events.push(WeaponEvent::Overheated { weapon: cur });
        }
        // The think's timed ExplodeLaunchedProjectiles (see begin_frame).
        if let Some(super::detonate::DetonateEvent::Explode { weapon, projectiles, sound }) = self.explode_launched(cur, false, true, now) {
            events.push(WeaponEvent::Detonate { weapon, projectiles, sound });
        }
        // The secondary trigger picks the fire mode (weapons::charge::alt_trigger).
        let alt_pressed = inp.altfire && !self.altfire_was_down;
        self.altfire_was_down = inp.altfire;
        if ready {
            if let Some(m) = self.alt_trigger(cur, inp.altfire, alt_pressed) {
                events.push(WeaponEvent::FireMode { weapon: cur, mode: m });
            }
        }
        let def = self.wdef(cur);

        // idHands::UpdateTriggerLatch 0x140d7c580 / ProcessTriggers 0x140d64a00.
        let has_ammo = self.has_ammo(cur);
        let idle = now >= self.states[cur].next_fire;
        if self.latch && !inp.trigger && (def.allow_shot_queueing || idle || !has_ammo) {
            self.latch = false;
        }
        let press = self.mstate[cur].press_shot;
        let pull = ready && (inp.trigger || press) && !self.latch;
        let fm = self.mstate[cur].fire_mode;
        if (inp.trigger || press) && !self.mstate[cur].trigger_down[fm] {
            self.mstate[cur].trigger_time[fm] = now;
        }
        self.mstate[cur].trigger_down = [false; 2];
        self.mstate[cur].trigger_down[fm] = inp.trigger || press;
        if pull {
            self.trigger_pulled = true;
        } else if !inp.trigger {
            self.trigger_pulled = false;
        }

        if let Some((cg, md)) = self.chaingun_data(cur) {
            let spin = ready && self.trigger_pulled && !(cg.prevent_spin_up_while_dryfiring && !has_ammo);
            self.states[cur].barrel.update(sec, spin, &cg, &md);
        }

        // UpdateWeapon_Default: fire && IsBurstMode && !InBurst && CanFire -> StartBurst; in a burst the
        // fire input stays on.
        if ready && (pull || self.queued) && self.is_burst_mode(cur) && !self.in_burst(cur) && self.can_fire(cur, now) {
            self.start_burst(cur, now);
        }
        let in_burst = self.in_burst(cur);
        if ready && (pull || self.queued || in_burst) {
            if self.can_fire(cur, now) {
                self.queued = false;
                if has_ammo {
                    let shot = self.fire(cur, now, inp);
                    events.push(WeaponEvent::Fired(shot));
                } else {
                    events.push(WeaponEvent::DryFire { weapon: cur });
                    // hands_autoDryfire 0: an empty weapon needs a fresh pull.
                    self.latch = true;
                }
            } else if pull && def.allow_shot_queueing && inp.trigger && !self.trigger_was_down {
                self.queued = true;
            }
        }

        if press {
            self.mstate[cur].press_shot = false;
            if self.set_fire_mode(cur, 0) {
                events.push(WeaponEvent::FireMode { weapon: cur, mode: 0 });
            }
        }
        self.charge_events(cur, now, &mut events);
        // idPlayer::UpdateWeaponKick 0x140e472c0: the current weapon's decl picks the kick system.
        self.view_kick = if def.feedback.use_new_kick_system { self.kick_new.offsets(now) } else { self.kick_old.update(now, ms) };
        self.trigger_was_down = inp.trigger;
        self.sync_compat();
        events
    }

    /// The weapon's charge update (weapons::charge) with its feedback as weapon events.
    pub fn charge_events(&mut self, w: usize, now: i32, events: &mut Vec<WeaponEvent>) {
        // idRailGun overrides the update (vslot +0x490) unless its weaponData uses the base behaviour.
        if self.railgun_def(w).is_some() {
            self.railgun_update(w, now, events);
            return;
        }
        for e in self.charge_update(w, now) {
            events.push(match e {
                super::charge::ChargeEvent::Sound(sound) => WeaponEvent::ChargeSound { weapon: w, sound },
                super::charge::ChargeEvent::StopSound => WeaponEvent::ChargeStop { weapon: w },
            });
        }
    }

    // ---- Hands-driven frames (weapons::hands::Hands owns the fire decisions) ----

    /// First half of a hands-driven frame: clock, idPlayer::UpdateSpread, idWeapon::UpdateHeat and the
    /// chaingun barrel (`spin_request` comes from idHands::UpdateWeapon_Chaingun). No firing here.
    pub fn begin_frame(&mut self, ms: i32, inp: &WeaponInput, spin_request: bool) -> Vec<WeaponEvent> {
        let mut events = Vec::new();
        self.time_ms += ms;
        let now = self.time_ms;
        let sec = ms as f32 * 0.001;
        let cur = self.current;
        let def = self.defs[cur].clone();
        self.zoomed = inp.spread.zoomed;
        self.spread.update(now, &def.spread_params, &inp.spread);
        if self.update_heat(cur, ms, sec) {
            events.push(WeaponEvent::Overheated { weapon: cur });
        }
        if let Some((cg, md)) = self.chaingun_data(cur) {
            let spin = spin_request && !(cg.prevent_spin_up_while_dryfiring && !self.has_ammo(cur));
            self.states[cur].barrel.update(sec, spin, &cg, &md);
        }
        // The think (vslot +0x2a0 0x140f02890 = vslot +0x638, then the launched-list prune 0x140f27560) ends with a
        // timed ExplodeLaunchedProjectiles(weapon, 0, 0, 1): entries past explodeProjectilesAutomaticallyDelay (the
        // micro missiles' 750 ticks) explode.
        if let Some(super::detonate::DetonateEvent::Explode { weapon, projectiles, sound }) = self.explode_launched(cur, false, true, now) {
            events.push(WeaponEvent::Detonate { weapon, projectiles, sound });
        }
        events
    }

    /// idChaingun's weaponData for the current fire mode (GetWeaponData: the mode decl's weaponData, so the
    /// gatling / turret decls bring their own) with CHAINGUN_SPIN_UP_TIME_MS (+0x19d0 + 400 * mode, read and
    /// written for the CURRENT fire mode; upgrades are applied in mode 0) over spinUpTimeMS (0x140ecbff6).
    pub fn chaingun_data(&self, w: usize) -> Option<(super::decl::ChaingunData, Arc<WeaponDef>)> {
        let md = self.wdef(w);
        let mut cg = md.chaingun?;
        let o = self.applied[w].weapon.chaingun_spin_up_ms;
        if self.mstate[w].fire_mode == 0 && 0 <= o {
            cg.spin_up_ms = o;
        }
        Some((cg, md))
    }

    /// Second half: idPlayer::UpdateWeaponKick and the compatibility mirrors.
    pub fn end_frame(&mut self, ms: i32, trigger: bool) {
        let now = self.time_ms;
        let def = self.def().clone();
        self.view_kick = if def.feedback.use_new_kick_system { self.kick_new.offsets(now) } else { self.kick_old.update(now, ms) };
        self.trigger_was_down = trigger;
        self.sync_compat();
    }

    /// The weapon side of idHands::FireWeapon 0x140d5e1d0 for an ae_fireWeaponRight that passed the hands'
    /// checks: idWeapon::Fire + idPlayer::WeaponFired. CanFire / nextFireTime are not rechecked here.
    pub fn fire_weapon(&mut self, inp: &WeaponInput) -> Shot {
        let now = self.time_ms;
        self.fire(self.current, now, inp)
    }

    /// idHands::SetWeapon 0x140d5d7f0 when the hands equip a weapon (ae_equipNextWeaponRight).
    pub fn equip(&mut self, idx: usize) {
        self.current = idx;
        self.latch = false;
        self.queued = false;
        self.trigger_pulled = false;
        self.phase = WeaponPhase::Ready;
        self.sync_compat();
    }

    /// 0x140f1ab30 for the SP weapons (no clips): not enough ammo for a shot.
    pub fn is_empty(&self, weapon: usize) -> bool {
        !self.has_ammo(weapon)
    }

    /// idWeapon::GetFiringInterval (vfunc +0x430/+0x438) for the current fire mode.
    pub fn firing_interval(&self, weapon: usize) -> i32 {
        self.firing_interval_mode(weapon, self.mstate[weapon].fire_mode)
    }

    /// GetFiringInterval 0x140f12ef0(weapon, mode) (MODS.md section 2): in-burst burstFiringInterval, the
    /// WMT FIRING_INTERVAL slot, the charge-scaled shoot anim, else the decl's firingInterval (the chaingun's
    /// lerped by barrel spin). Not ported: the per-mode override (+0x183c), the locked-target slot and the
    /// override shoot state (SSG mastery shoot_single) steps.
    pub fn firing_interval_mode(&self, weapon: usize, mode: usize) -> i32 {
        let Some(d) = self.mode_def(weapon, mode) else { return self.defs[weapon].firing_interval_at(self.states[weapon].barrel.spin) };
        if self.in_burst(weapon) {
            let bm = self.burst_mode(weapon).clamp(0, 3) as usize;
            let v = d.bursts[bm].burst_firing_interval;
            if v != 0 {
                return v;
            }
        }
        let o = self.applied[weapon].modes[mode.min(1)].firing_interval;
        if o != 0 {
            return o;
        }
        // scaleShootAnimToMatchChargeTime: chargeTime / shotsPerShootAnim (decl +0x7b4).
        if d.charge.scale_shoot_anim_to_match_charge_time && d.hands.shots_per_shoot_anim != 0 {
            return self.charge_time(weapon, mode) / d.hands.shots_per_shoot_anim;
        }
        d.firing_interval_at(self.states[weapon].barrel.spin)
    }

    /// idHands::SetWeapon 0x140d5d7f0: latches cleared, bring-up starts.
    fn set_weapon(&mut self, idx: usize) {
        self.current = idx;
        self.latch = false;
        self.queued = false;
        self.trigger_pulled = false;
        self.phase = WeaponPhase::Raising { left: self.defs[idx].bringup_s };
    }

    /// The heatInfo UpdateHeat 0x140f26b00, StartOverheat 0x140f1ca30 and FinishFire read: the mode-1 decl's
    /// (DECL_WEAPON override +0x1a28, else secondaryFireDecl) when its heatIncrement > 0, else mode 0's (override
    /// +0x1898, else the base decl), whatever the current fire mode.
    pub fn heat_info(&self, w: usize) -> super::decl::HeatInfo {
        if let Some(d1) = self.mode_def(w, 1).filter(|d| 0.0 < d.heat.heat_increment) {
            return d1.heat;
        }
        self.mode_def(w, 0).map(|d| d.heat).unwrap_or(self.defs[w].heat)
    }

    /// idWeapon::UpdateHeat 0x140f26b00; returns true when the weapon overheated this frame.
    fn update_heat(&mut self, w: usize, ms: i32, sec: f32) -> bool {
        let h = self.heat_info(w);
        // StartOverheat: OVERHEAT_DELAY override (+0x18dc + 400 * mode) when >= 0, else overheatDelayMS.
        let od = self.applied[w].modes[self.mstate[w].fire_mode].overheat_delay;
        let overheat_delay = if 0 <= od { od } else { h.overheat_delay_ms };
        let st = &mut self.states[w];
        if st.cool_delay != 0 {
            st.cool_delay = (st.cool_delay - ms).max(0);
        }
        if st.cool_delay == 0 && 0.0 < st.glow {
            let mut g = st.glow - sec * h.glow_decrement;
            if h.max_glow <= g {
                g = h.max_glow;
            }
            if g <= 0.0 {
                g = 0.0;
            }
            st.glow = g;
        }
        let mut overheated = false;
        if st.overheat < 1 {
            // vfunc +0x4b0: can overheat only when maxHeatPercent == 1.
            if 1.0 <= st.heat && h.max_heat_percent == 1.0 {
                // idWeapon::StartOverheat 0x140f1ca30.
                st.heat = 1.0;
                st.cool_delay = 0;
                st.overheat = overheat_delay;
                overheated = true;
            }
        } else {
            st.overheat -= ms;
            if st.overheat < 1 {
                st.overheat = 0;
                if 0.0 <= h.overheat_recovery_percent {
                    st.heat = h.overheat_recovery_percent;
                }
            }
        }
        if 0.0 < st.heat {
            let dec = h.heat_decrement;
            if !overheated && (st.overheat < 1 || h.cool_during_overheat_delay) && st.cool_delay == 0 && 0.0 < dec {
                st.heat -= sec * dec;
                if st.heat <= 0.0 {
                    st.heat = 0.0;
                }
            }
        }
        overheated
    }

    /// idHands::FireWeapon -> idWeapon::Fire (InitForFire, launch, FinishFire) -> idPlayer::WeaponFired.
    fn fire(&mut self, w: usize, now: i32, inp: &WeaponInput) -> Shot {
        use super::decl::ChargeProperty as P;
        let mode = self.mstate[w].fire_mode;
        let def = self.wdef(w);
        // Spread is read before the shot's own addition (FireWeapon 0x140d5e1d0).
        let spread_deg = self.spread.value(now);
        let pat = def.projectile.pattern();
        let n = pat.count.max(1);
        let fwd = inp.spread.view_forward;
        let dirs: Vec<Vec3> = (0..n).map(|i| shot_direction(&mut self.rng, fwd, inp.left, inp.up, spread_deg, &def.spread_params, &pat, i, inp.gaussian_spread)).collect();
        // Use-time values (weapons::mods UseTime) and the charge's DAMAGE_SCALE item at the shot's charge.
        let charge_pct = self.mstate[w].charge.percent;
        let mut damage_scale = self.applied[w].use_time[mode].damage_scale;
        if self.charge_item(w, &P::DamageScale).is_some() {
            damage_scale *= self.charge_value(w, &P::DamageScale, charge_pct);
        }
        // Shot hook 0x140f179a0 ~0x140f17f12: a secondary-mode (or zoomed) shot multiplies its damage scale
        // (+0x1e4) by PRIMARY_CHARGE_SECONDARY_DAMAGE_SCALE (12) from the item cache (0x140f125e0; INTERIM: the
        // cache is taken as the value at the current charge) - the plasma heat blast's built-up charge.
        if (mode == 1 || self.zoomed) && self.charge_item(w, &P::PrimaryChargeSecondaryDamageScale).is_some() {
            damage_scale *= self.charge_value(w, &P::PrimaryChargeSecondaryDamageScale, charge_pct);
        }
        damage_scale *= self.railgun_shot_scale(w, now);
        // InitForFire's targeting round robin (weapons::targeting).
        let target = self.shot_target(w);

        // FinishFire 0x140f0d5c0: next fire time. The last shot of a burst waits the burst interval
        // (BURST_INTERVAL override, else burstInfo[bm].burstInterval, 0 -> GetFiringInterval(0)) and
        // discharges a BURST_COUNT charge; other shots wait GetFiringInterval + rand15 % addedFiringInterval.
        let burst_left = self.mstate[w].burst_count;
        let bm = self.burst_mode(w).clamp(0, 3) as usize;
        let last_burst_shot = burst_left == 1;
        if last_burst_shot {
            let o = self.applied[w].weapon.burst_interval;
            let mut bi = if o >= 0 { o } else { def.bursts[bm].burst_interval };
            if bi == 0 {
                bi = self.firing_interval_mode(w, 0);
            }
            let nf = self.next_fire(w, mode).max(now + ms_to_ticks(bi));
            self.set_next_fire(w, mode, nf);
            if self.charge_item(w, &P::BurstCount).is_some() && self.is_burst_mode(w) {
                self.discharge(w, true, now);
            }
        } else {
            let mut interval = self.firing_interval(w);
            if 0 < def.added_firing_interval {
                interval += (self.rng.next15() as i32) % def.added_firing_interval;
            }
            let nf = self.next_fire(w, mode).max(now + ms_to_ticks(interval));
            self.set_next_fire(w, mode, nf);
        }
        // Every shot blocks the other mode for otherFireModeFiringInterval raw ticks (0x140f13c20).
        let o = self.applied[w].modes[mode].other_fire_mode_firing_interval;
        let v = if o >= 0 { o } else { self.mode_def(w, mode).map(|d| d.other_fire_mode_firing_interval).unwrap_or(0) };
        if mode == 1 {
            let nf = self.next_fire(w, 0).max(now + v);
            self.set_next_fire(w, 0, nf);
        } else if self.mode_def(w, 1).is_some() {
            let nf = self.next_fire(w, 1).max(now + v);
            self.set_next_fire(w, 1, nf);
        }
        let next_fire = self.next_fire(w, mode);
        // Ammo (0x140f29cf0): in a burst the BURST_AMMO_COST / ammoPerBurst on its last shot; an AMMO_TO_USE
        // charge item's value; else ammoPerShot.
        let in_burst = self.in_burst(w);
        let cost = self.applied[w].weapon.burst_ammo_cost;
        let ammo_n = if in_burst && cost >= 0 {
            if last_burst_shot { cost } else { 0 }
        } else if in_burst && def.bursts[bm].ammo_per_burst >= 0 {
            if last_burst_shot { def.bursts[bm].ammo_per_burst } else { 0 }
        } else if self.charge_item(w, &P::AmmoToUse).is_some() {
            self.charge_value(w, &P::AmmoToUse, self.mstate[w].charge.full) as i32
        } else {
            def.ammo_per_shot
        };
        let mut ammo_used = 0;
        if let Some(p) = self.pool_for(&def) {
            ammo_used = ammo_n;
            let pool = &mut self.pools[p];
            pool.count = (pool.count - ammo_used).max(0);
        }
        let ms = &mut self.mstate[w];
        ms.burst_count = (ms.burst_count - 1).max(0);
        if self.charge_item(w, &P::BurstCount).is_some() {
            self.mstate[w].charge.burst_shots_fired += 1;
        }
        // chargePerShot (FinishFire ~0x140f0eb00): a PRIMARY shot while CanCharge, maxCharge > 0 and not zoomed
        // adds chargeIncrement (or the CHARGE_PER_SHOT_INCREMENT override, 0x140f11a10) per result entry
        // (one per trace; requireHitToCharge off in every SP decl), every numShotsPerCharge successes, up to
        // maxCharge. INTERIM: requireHitToCharge / misses are not evaluated (no hit results here).
        let cd = self.charge_decl(w);
        let ps = cd.charge.per_shot;
        if mode == 0 && ps.max_charge > 0.0 && !inp.spread.zoomed && self.can_charge(w, now) {
            let om = if cd.charge.override_primary_charge_info { 1 } else { 0 };
            let o = self.applied[w].modes[om].charge_per_shot_increment;
            let inc = if o >= 0.0 { o } else { ps.charge_increment };
            let ch = &mut self.mstate[w].charge;
            for _ in 0..n {
                ch.successes += 1;
                ch.misses = 0;
                if ps.num_shots_per_charge < 1 || ch.successes == ps.num_shots_per_charge {
                    ch.successes = 0;
                    ch.per_shot_val = (ch.per_shot_val + inc).min(ps.max_charge);
                }
            }
        }
        // A shot of the discharge fire mode (or dischargeFireMode 2) discharges the charge.
        let dm = def.charge.discharge_fire_mode;
        if !def.charge.items.is_empty() && (mode as i32 == dm || dm == 2) {
            self.discharge(w, true, now);
        }
        let h = self.heat_info(w);
        let st = &mut self.states[w];
        let mut g = h.glow_increment + st.glow;
        if h.max_glow <= g {
            g = h.max_glow;
        }
        st.glow = g;
        if 0.0 < h.heat_increment && st.overheat < 1 {
            let max = h.max_heat_percent;
            let mut inc = h.heat_increment;
            if max != 1.0 {
                inc = inc * (max - st.heat) * (max - st.heat);
            }
            if 0.0 < inc {
                let mut v = inc + st.heat;
                if max <= v {
                    v = max;
                }
                st.heat = v;
                let fi = def.firing_interval_at(st.barrel.spin);
                let mut d = fi * 2;
                if d < st.cool_delay {
                    d = st.cool_delay;
                }
                st.cool_delay = if h.cooling_delay_ms < d { d } else { h.cooling_delay_ms };
            }
        }

        // idPlayer::WeaponFired 0x140e47b80: spread addition, then kick.
        self.spread.on_fire(now, &def.spread_params, &inp.spread);
        if def.feedback.use_new_kick_system {
            self.kick_new.kick(now, &def.feedback, &mut self.rng);
        } else {
            self.kick_old.kick(now, &def.feedback, def.firing_interval, &mut self.rng);
        }
        if def.single_tap || (inp.spread.zoomed && def.single_tap_ads) {
            self.latch = true;
        }
        self.last_fire_ms = Some(now);
        self.states[w].last_fire = now;
        self.states[w].last_finish_fire[mode] = now;
        self.railgun_finish_fire(w);
        // FinishFire after the launch: clearAfterNumShots / loseLockOnFire (weapons::targeting).
        self.targeting_finish_fire(w, mode);
        Shot {
            weapon: w,
            time_ms: now,
            spread_deg,
            dirs,
            ammo_used,
            next_fire_ms: next_fire,
            knockback: def.feedback.weapon_knockback,
            mode,
            def: ShotDef(def.clone()),
            damage_scale,
            burst_left: self.mstate[w].burst_count,
            charge: charge_pct,
            target,
        }
    }

    fn sync_compat(&mut self) {
        let now = self.time_ms;
        self.cooldown_ms = (self.states[self.current].next_fire - now).max(0) as f32;
        self.kick[0].value = -self.view_kick.pitch;
        self.kick[1].value = self.view_kick.yaw;
        self.kick[2].value = -self.view_kick.fov;
        for i in 0..self.defs.len() {
            // Infinite-ammo weapons (pistol, fists) keep showing their decl count.
            let d = &self.defs[i];
            self.ammo[i] = self.ammo_for(i).unwrap_or(d.ammo_max.max(d.ammo_start));
        }
    }
}
