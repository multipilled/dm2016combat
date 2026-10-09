//! idHands (player+0x17078): the first-person hands state machine. It turns the trigger and the weapon
//! state into fp_hands anim-web state requests, writes the idAnimWebHands scalars, and fires the weapon
//! when the web plays an `ae_fireWeaponRight` frame event. Decoded from DOOMx64.exe; the decision
//! tables, flag bits and addresses are in gamedata/re/HANDS.md.
//!
//! Frame order inside idHands::Update 0x140d6b2c0 (called from idPlayer::UpdateWeapon 0x140e45e50):
//!  1. UpdateWeapon 0x140d7e640: weaponLoaded scalars, shootAnimSelect, then the weapon's update
//!     function (UpdateWeapon_Default 0x140d7fda0: trigger latch, ProcessTriggers, fire input and the
//!     pending FIRE / DRYFIRE / CEASEFIRE action; _Chaingun 0x140d7f390 adds barrel/settle blends).
//!  2. The switch on GetHandsState 0x140d60380: the pending action becomes web state requests.
//!  3. 0x140d7e230 -> 0x140d7bc00 (hands_updateAnim 1): the animator advances to the frame time and its
//!     md6 frame events (ae_*) and node notifications fire -> [`Hands::anim_event`] / [`Hands::node_event`].
//!  4. ResolveWeaponFire 0x140d66b10: hands_useDeferredFire 2 defers held-trigger hitscan shots to here,
//!     still inside the same frame, so it does not change any timing.
//!
//! Scope: the SP guns' primary fire, dry fire, cease fire, weapon switching, the jump/fall/land
//! requests, the directional melee (MELEE -> melee_into -> melee_miss, the joint trace in weapons::melee)
//! and the zoom gates / zoomPCT (weapons::zoom). Not ported: sync / glory-kill melees and the fists'
//! staged punches, throwables, reload (SP weapons have no clips), charge/alt fire and mods, zoom shoot
//! states, bursts, chainsaw, swimming, custom anims and generic hides.

use super::arsenal::{Arsenal, WeaponEvent, WeaponInput};
use super::melee::{MeleeHit, MeleeTrace, SweepHit};
use glam::Vec3;

/// hands_autoDryfire (default 0).
pub const HANDS_AUTO_DRYFIRE: bool = false;
/// hands_forceDryfire (default 0).
pub const HANDS_FORCE_DRYFIRE: bool = false;
/// hands_forceInterruptibleTransition (default 0).
pub const HANDS_FORCE_INTERRUPTIBLE: bool = false;
/// hands_weaponLoadedBlendSpringK (default 500).
pub const HANDS_WEAPON_LOADED_SPRING_K: f32 = 500.0;
/// hands_allowMeleeInterrupt: a MELEE may interrupt the transition back to idle.
pub const HANDS_ALLOW_MELEE_INTERRUPT: bool = true;
/// hands_weaponZoomPCTPower: weaponZoomPCT = zoomPCT^power (0x140d67840).
pub const HANDS_WEAPON_ZOOM_PCT_POWER: f32 = 0.3;
/// hands_weaponChangeTimingScheme (default 0: bringdown(cur) + bringup(next)).
pub const HANDS_WEAPON_CHANGE_TIMING_SCHEME: i32 = 0;
/// Chaingun settle blend: (time since last shot threshold ms, rise per frame, fall per second) for
/// UpdateWeapon_Default (0x142fa5324/531c/5320) and then UpdateWeapon_Chaingun (0x142fa5334/532c/5330).
pub const SETTLE_DEFAULT: (i32, f32, f32) = (32, 0.4, 3.0);
pub const SETTLE_CHAINGUN: (i32, f32, f32) = (32, 0.4, 1.0);
/// Blend frames used by the hands' ForceState calls (FUN_1416e6590(parms, 3)).
pub const FORCE_BLEND_FRAMES: i32 = 3;
/// FLT_MIN, the engine's "is zero" threshold in the spring and the weaponLoaded blend.
const FLT_MIN: f32 = 1.175_494_4e-38;
/// Game time is idTypesafeTime<int, gameTimeUnique_t, 960>: the game timer's ticks per second (ANIMWEB.md) and
/// its reciprocal (idGameTimeManagerLocal +0x140 / +0x170). Frame seconds (+0x108) are msec * 0.001 instead.
pub const TICKS_PER_SEC: f32 = 960.0;
pub const SEC_PER_TICK: f32 = 1.0 / 960.0;

/// idHands::handsState_t, derived from the web's current state (GetHandsState 0x140d60380).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandsState {
    #[default]
    Idle = 0,
    Reloading = 1,
    ReloadingDouble = 2,
    FallIdle = 3,
    ThrowBranch = 4,
    ThrowCook = 5,
    ThrowCookInto = 6,
    Hidden = 7,
    LoopingShoot = 8,
    LoopingDryfire = 9,
    ChargeIdle = 10,
    ChargeLoopingShoot = 11,
    ChargeLoopingDryfire = 12,
    ChainsawRev = 13,
    ChainsawStab = 14,
    SwimIdle = 15,
    VirtualGui = 16,
    PowerCoreIdle = 17,
    Transitioning = 18,
}

impl HandsState {
    /// 0x140d60210: the hands state a web state stands for (the swim test is not ported).
    pub fn of_state(state: &str) -> Self {
        use HandsState::*;
        match state {
            "throwbranch" => ThrowBranch,
            "throwcook" => ThrowCook,
            "throwcookinto" => ThrowCookInto,
            "idle" | "idle_alt" => Idle,
            "fall" | "fall_loop" => FallIdle,
            "reload" | "reloadfromempty" => Reloading,
            "reloaddouble" => ReloadingDouble,
            "generic_hide" => Hidden,
            "shootstate" => LoopingShoot,
            "charge_idle" => ChargeIdle,
            "charge_shootstate" => ChargeLoopingShoot,
            "charge_dryfirestate" => ChargeLoopingDryfire,
            "dryfirestate" => LoopingDryfire,
            "chainsaw_rev_into" | "chainsaw_rev_loop" | "chainsaw_rev_recovery" => ChainsawRev,
            "chainsaw_stab_fail" | "chainsaw_stab_finto" | "chainsaw_stab_loop" | "chainsaw_stab_recovery" => ChainsawStab,
            _ => Transitioning,
        }
    }
}

/// idHands::handsAction_t (pending action, hands+0x102e0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandsAction {
    #[default]
    None = 0,
    Idle = 1,
    ForceIdle = 2,
    Fire = 3,
    Ceasefire = 4,
    Dryfire = 5,
    Reload = 6,
    Melee = 7,
    MeleeRight = 8,
    MeleeLeft = 9,
    Bringdown = 10,
    Bringup = 11,
    CookItem = 12,
    ThrowItem = 13,
    Jump = 14,
    Fall = 15,
    CustomAnim = 16,
    LandNoAnim = 17,
    LandSm = 18,
    LandMed = 19,
    LandLg = 20,
    GenericHide = 21,
    GenericHideSlow = 22,
    GenericHideExtraSlow = 23,
    GenericUnhide = 24,
    /// HANDSACTION_CHARGE: into charge_idle (hasChargeState weapons).
    Charge = 39,
}

impl HandsAction {
    fn is_melee(self) -> bool {
        matches!(self, HandsAction::Melee | HandsAction::MeleeRight | HandsAction::MeleeLeft)
    }
}

/// What the player tells the hands each frame.
#[derive(Debug, Clone, Copy)]
pub struct HandsInput {
    /// Fire axes, spread input and `trigger` = the attack button held.
    pub weapon: WeaponInput,
    /// 0x140d530f0: the owner may use the weapon (hands+0xfafd).
    pub can_use_weapon: bool,
    /// idPlayer+0x14031 bit 0: weapon fire allowed by the player.
    pub weapon_enabled: bool,
    /// 0x140d63260: the owner is airborne (falling) with weapons usable.
    pub falling: bool,
    /// The player issued a jump this frame (JUMP pending action). INFERRED trigger.
    pub jumped: bool,
    /// The player landed this frame with the given anim size. INFERRED trigger.
    pub landed: Option<LandSize>,
    /// BUTTON_ATTACK2 (melee) pressed this frame.
    pub melee: bool,
    /// BUTTON_ALTFIRE held (the fists' trigger mode 0xd takes it as a second attack button; its press picks
    /// the left punch).
    pub altfire: bool,
}

impl Default for HandsInput {
    fn default() -> Self {
        Self { weapon: WeaponInput::default(), can_use_weapon: true, weapon_enabled: true, falling: false, jumped: false, landed: None, melee: false, altfire: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandSize {
    NoAnim,
    Small,
    Medium,
    Large,
}

/// The anim-web runtime the hands drive (idAnimator_GameAnimWeb at hands+0x3248).
pub trait HandsWeb {
    /// Current node as (subWeb, state); None before the web has a node.
    fn current(&self) -> Option<(&str, &str)>;
    /// A transition blend is still running (hands+0x36cc/0x36ce both >= 0).
    fn is_blending(&self) -> bool;
    fn set_scalar(&mut self, name: &str, value: f32);
    /// WebAnimatorChangeState_ 0x140d81b70 -> 0x141700000.
    fn change_state(&mut self, sub: &str, to: &str);
    /// WebAnimatorChangeStateVia_ 0x140d81970 -> 0x141700ee0: go to `to`, routed through `via`.
    fn change_state_via(&mut self, sub: &str, to: &str, via: &str);
    /// WebAnimatorForceState_ 0x140d820e0 -> 0x141705510 with a blend of `blend_frames` 30 Hz frames.
    fn force_state(&mut self, sub: &str, to: &str, blend_frames: i32);
    /// WebAnimatorForceStateVia_ 0x140d81ef0 -> 0x1417046b0, arguments as the exe passes them (its debug
    /// line reads "changing to state <sub:to> via <sub:via>"); the hands use it only for landings, as
    /// (to = land_sm/med/lg, via = idle). How the runtime routes it is the runtime's (ANIMWEB.md) business.
    fn force_state_via(&mut self, sub: &str, to: &str, via: &str, blend_frames: i32);
    /// numFrames and frameRate of the anim an md6Def alias resolves to on `model` (a handsModelMD6).
    fn alias_anim(&self, model: &str, alias: &str) -> Option<(i32, i32)>;
    /// numFrames of the hands-model anim a state plays (0x141706640 out+4), for the bring rates.
    fn state_anim_frames(&self, sub: &str, state: &str) -> Option<i32>;
    /// Advance to `now_ms` and report what fired, in order.
    fn update(&mut self, now_ms: i32) -> Vec<WebEvent>;
}

/// Something the web reports while advancing.
#[derive(Debug, Clone, PartialEq)]
pub enum WebEvent {
    /// An md6 frame event with its string arguments in decl order (e.g. ae_handsStartJointMeleeTrace
    /// "right_hand" "melee_impact"); handled like `Anim`.
    AnimArgs { name: String, int: Option<i32>, strings: Vec<String> },
    /// An md6 frame event of any model in the current tree (`ae_fireWeaponRight`, `ae_setInterruptible` ...);
    /// `int` is the event's `int` parameter.
    Anim { name: String, int: Option<i32> },
    /// idAnimWebEventHandler web event `kind` for node (sub, state): 0 an edge into the node was taken,
    /// 1 an edge out of it was taken, 2 its blend-in completed, 3 its blend-out completed
    /// (0x14170c300, ANIMWEB.md). Kinds 4..10 are reported too but the hands only register 0, 1, 3.
    Node { kind: u8, sub: String, state: String },
}

/// A state request the hands made this frame (also applied to the web). `line` is the exe's debug line id.
#[derive(Debug, Clone, PartialEq)]
pub enum WebRequest {
    ChangeState { line: u16, sub: String, to: &'static str },
    ChangeStateVia { line: u16, sub: String, to: &'static str, via: &'static str },
    ForceState { line: u16, sub: String, to: &'static str, blend_frames: i32 },
    ForceStateVia { line: u16, sub: String, to: &'static str, via: &'static str, blend_frames: i32 },
}

/// The idEventDef the hands attach to a web node (FUN_140d69950 -> 0x14170b320, handled by
/// idHands' event dispatcher 0x1414a7f10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEvent {
    /// animWeb_ShootStarted -> 0x140d5ddd0: flags 0x10375 |= 3, 0x10377 &= ~1.
    ShootStarted,
    /// animWeb_ShootEnded -> 0x140d5dcd0: 0x10375 &= 0x8c, 0x10374 |= 0x40 (interruptible), switch-on-empty.
    ShootEnded,
    /// animWeb_ShootEndedReloadStarted -> 0x140d5dd40: 0x10375 &= 0xec, 0x10374 |= 0x12.
    ShootEndedReloadStarted,
    /// animWeb_DryfireEnded -> 0x140d5db90: switch-on-empty.
    DryfireEnded,
    /// animWeb_ChangeWeaponBringdownEnded -> 0x140d5da50: 0x10377 |= 8.
    ChangeWeaponBringdownEnded,
    /// animWeb_ChangeWeaponEnded -> 0x140d5da60: if 0x10377 & 8: 0x10377 &= ~4, 0x10379 &= ~1.
    ChangeWeaponEnded,
    /// animWeb_MeleeEnded / animWeb_DirectionalMeleeEnded (eventNum 0x80 / 0x81) -> 0x140d5dbc0 -> 0x140d63b50.
    MeleeEnded,
    /// animWeb_MeleeToShoot (0x82) -> 0x140d5dbe0.
    MeleeToShoot,
    /// animWeb_BlendToChargeEnded (0x8e, idEventDef 0x14459ba60) -> 0x140d5d9d0: 0x10378 &= ~0x10.
    BlendToChargeEnded,
    /// animWeb_BlendFromChargeEnded (0x8f, 0x14459bac0) -> 0x140d5d9c0: 0x10378 &= ~0x20.
    BlendFromChargeEnded,
}

#[derive(Debug, Clone, PartialEq)]
struct Slot {
    sub: String,
    state: &'static str,
    event: StateEvent,
}

/// The idAnimWebHands members idHands writes (names as bound by RegisterScalars 0x140cc9280).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandsScalars {
    pub zoom_pct: f32,
    pub weapon_zoom_pct: f32,
    pub weapon_loaded_select: f32,
    pub weapon_loaded_blend: f32,
    pub weapon_lock_percent: f32,
    pub weapon_has_locked_target: f32,
    pub weapon_can_reload_after_firing_select: f32,
    pub shoot_anim_rate: f32,
    pub shoot_charge_anim_rate: f32,
    pub shoot_anim_select: f32,
    pub shoot_anim_barrel_select: f32,
    pub shoot_again1_anim_select: f32,
    pub shoot_again2_anim_select: f32,
    pub reload_anim_rate: f32,
    pub bring_up_anim_rate: f32,
    pub bring_up_intro_anim_select: f32,
    pub bring_down_anim_rate: f32,
    pub blend_chaingun_barrel: f32,
    pub blend_chaingun_settle: f32,
    pub shoot_delay_scale: f32,
    pub weapon_charge_for_trigger_event: f32,
    pub weapon_flourish_anim_select: f32,
    /// hands+0x3ad4 (0x140d66e40): 0 default, 1 sprint melee, 2 multiplayer.
    pub melee_select: f32,
    /// Mod switch (weapons::modswitch): genericHideAnimSelect (0x140cca050: hide speed 0 / slow 1 /
    /// extra slow 2), unhideWeaponModSelect (the family's slot), modSelectAnimRate (1 / modSpeed, 0x140d69990).
    pub generic_hide_anim_select: f32,
    pub unhide_weapon_mod_select: f32,
    pub mod_select_anim_rate: f32,
    /// chargeIntoRateScale / chargeOutRateScale (0x140d67270, set before charge_into / charge_out play).
    pub charge_into_rate_scale: f32,
    pub charge_out_rate_scale: f32,
}

impl Default for HandsScalars {
    /// INFERRED: idAnimWebHands ctor values (not decoded); rates 1, everything else 0.
    fn default() -> Self {
        Self {
            zoom_pct: 0.0,
            weapon_zoom_pct: 0.0,
            weapon_loaded_select: 0.0,
            weapon_loaded_blend: 0.0,
            weapon_lock_percent: 0.0,
            weapon_has_locked_target: 0.0,
            weapon_can_reload_after_firing_select: 0.0,
            shoot_anim_rate: 1.0,
            shoot_charge_anim_rate: 1.0,
            shoot_anim_select: 0.0,
            shoot_anim_barrel_select: 0.0,
            shoot_again1_anim_select: 0.0,
            shoot_again2_anim_select: 0.0,
            reload_anim_rate: 1.0,
            bring_up_anim_rate: 1.0,
            bring_up_intro_anim_select: 0.0,
            bring_down_anim_rate: 1.0,
            blend_chaingun_barrel: 0.0,
            blend_chaingun_settle: 0.0,
            shoot_delay_scale: 1.0,
            weapon_charge_for_trigger_event: 0.0,
            weapon_flourish_anim_select: 0.0,
            melee_select: 0.0,
            generic_hide_anim_select: 0.0,
            unhide_weapon_mod_select: 0.0,
            mod_select_anim_rate: 1.0,
            charge_into_rate_scale: 1.0,
            charge_out_rate_scale: 1.0,
        }
    }
}

impl HandsScalars {
    /// (anim-web scalar name, value) for every member the hands write.
    pub fn pairs(&self) -> [(&'static str, f32); 28] {
        [
            ("zoomPCT", self.zoom_pct),
            ("weaponZoomPCT", self.weapon_zoom_pct),
            ("weaponLoadedSelect", self.weapon_loaded_select),
            ("weaponLoadedBlend", self.weapon_loaded_blend),
            ("weaponLockPercent", self.weapon_lock_percent),
            ("weaponHasLockedTarget", self.weapon_has_locked_target),
            ("weaponCanReloadAfterFiringSelect", self.weapon_can_reload_after_firing_select),
            ("shootAnimRate", self.shoot_anim_rate),
            ("shootChargeAnimRate", self.shoot_charge_anim_rate),
            ("shootAnimSelect", self.shoot_anim_select),
            ("shootAnimBarrelSelect", self.shoot_anim_barrel_select),
            ("shootAgain1AnimSelect", self.shoot_again1_anim_select),
            ("shootAgain2AnimSelect", self.shoot_again2_anim_select),
            ("reloadAnimRate", self.reload_anim_rate),
            ("bringUpAnimRate", self.bring_up_anim_rate),
            ("bringUpIntroAnimSelect", self.bring_up_intro_anim_select),
            ("bringDownAnimRate", self.bring_down_anim_rate),
            ("blendChaingunBarrel", self.blend_chaingun_barrel),
            ("blendChaingunSettle", self.blend_chaingun_settle),
            ("shootDelayScale", self.shoot_delay_scale),
            ("weaponChargeForTriggerEvent", self.weapon_charge_for_trigger_event),
            ("weaponFlourishAnimSelect", self.weapon_flourish_anim_select),
            ("meleeSelect", self.melee_select),
            ("genericHideAnimSelect", self.generic_hide_anim_select),
            ("unhideWeaponModSelect", self.unhide_weapon_mod_select),
            ("modSelectAnimRate", self.mod_select_anim_rate),
            ("chargeIntoRateScale", self.charge_into_rate_scale),
            ("chargeOutRateScale", self.charge_out_rate_scale),
        ]
    }
}

/// The engine's damped spring (update 0x1409442f0, constants 0x1409430e0), used for weaponLoadedBlend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub target: f32,
    pub value: f32,
    pub vel: f32,
    pub max_vel: f32,
    pub k: f32,
    pub damping: f32,
    pub mass: f32,
    pub rest: f32,
}

impl Spring {
    /// hands ctor 0x140d52480: k 1, damping 2, mass 1, rest 0; then 0x1409430e0(k = cvar, damping -1).
    fn weapon_loaded() -> Self {
        let mut s = Self { target: 0.0, value: 0.0, vel: 0.0, max_vel: 0.0, k: 1.0, damping: 2.0, mass: 1.0, rest: 0.0 };
        s.set_k(HANDS_WEAPON_LOADED_SPRING_K, -1.0);
        s
    }

    /// 0x1409430e0: k = min(k, 10000); damping < 0 -> critical 2*sqrt(k*mass) (rsqrtss + two Newton steps
    /// in the exe; sqrt here).
    fn set_k(&mut self, k: f32, damping: f32) {
        self.k = k.min(10000.0);
        self.damping = if damping >= 0.0 { damping } else { 2.0 * (self.k * self.mass).max(1e-30).sqrt() };
    }

    /// 0x1409442f0: semi-implicit Euler in steps of at most 8.5 ms.
    fn update(&mut self, mut dt: f32) {
        while 0.0 < dt {
            let step;
            if dt <= 0.0085 {
                step = dt;
                dt = 0.0;
            } else {
                step = 0.0085;
                dt -= 0.0085;
            }
            let sign = if self.value - self.target < 0.0 { -1.0 } else { 1.0 };
            let mut v = ((-((self.value - self.target).abs() - self.rest) * self.k * sign) - self.damping * self.vel) / self.mass * step + self.vel;
            let mag = v.abs();
            let s = if v < 0.0 { -1.0 } else { 1.0 };
            let lim = if 0.0 < self.max_vel && self.max_vel < mag { self.max_vel } else { mag };
            v = lim * s;
            if v.abs() <= FLT_MIN {
                v = 0.0;
            }
            self.vel = v;
            self.value += step * v;
            if self.value.abs() <= FLT_MIN {
                self.value = 0.0;
            }
        }
    }
}

/// hands+0x10374..0x1037a.
pub const F74: usize = 0;
pub const F75: usize = 1;
pub const F76: usize = 2;
pub const F77: usize = 3;
pub const F78: usize = 4;
pub const F79: usize = 5;
pub const F7A: usize = 6;

/// The hands' flag bytes, raw as in the exe (meanings in HANDS.md; the ones used here:
/// 0x10374: 0x02 shoot-to-reload/reloading, 0x08 weapon set, 0x40 interruptible;
/// 0x10375: 0x02 in a shoot state, 0x04 shoot/shoot_again alternation, 0x10 a shot fired this shoot,
///          0x20 FIRE accepted but not fired yet, 0x40 dry fired, 0x80 last FireWeapon failed;
/// 0x10377: 0x02 hide inhibit, 0x04 weapon change running, 0x08 its bringdown ended;
/// 0x10378: 0x01 in water, 0x08 charging, 0x40/0x80 forced intro / accent bring-up;
/// 0x10379: 0x01 intro bring-up playing, 0x80 custom anim; 0x1037a: 0x01 refuse all actions).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HandsFlags(pub [u8; 7]);

impl HandsFlags {
    pub fn has(&self, b: usize, m: u8) -> bool {
        self.0[b] & m != 0
    }
    fn or(&mut self, b: usize, m: u8) {
        self.0[b] |= m;
    }
    fn and(&mut self, b: usize, m: u8) {
        self.0[b] &= m;
    }
}

/// idHands.
pub struct Hands {
    /// hands+0x102e0.
    pub pending: HandsAction,
    /// hands+0x10320: when the pending action was (re)set.
    pub pending_since: i32,
    pub flags: HandsFlags,
    /// hands+0xfaf0: trigger latch per fire mode.
    pub latch: [bool; 2],
    /// hands+0xfafc: this frame's fire input (after latch, single tap and can-use).
    pub fire: bool,
    /// hands+0xfafe / 0xfb00: attack held / pressed this frame (ProcessTriggers).
    pub attack_held: bool,
    pub attack_pressed: bool,
    held_last: bool,
    /// hands+0x3b28: hands state of the last requested destination.
    pub target: HandsState,
    /// hands+0xfab0: weapon being brought up (cleared by ae_equipNextWeaponRight).
    pub new_weapon: Option<usize>,
    /// hands+0x102e8: weapon of the pending BRINGDOWN.
    pub pending_item: Option<usize>,
    /// hands+0xfac0: a BRINGDOWN deferred out of a non-idle state.
    pub queued_item: Option<usize>,
    /// Current weapon's trigger state (weapon+0x8f8): true = TRIGGERSTATE_PULLED.
    pub trigger_pulled: bool,
    pub scalars: HandsScalars,
    /// Requests made by the last update (also applied to the web).
    pub requests: Vec<WebRequest>,
    /// weapon+0x1555 per weapon: the intro bring-up has played.
    pub intro_played: Vec<bool>,
    /// idPlayer+0xcdf6: weapons play their intro bring-up the first time they come up.
    pub intro_bringups: bool,
    /// Additive looping shoot anim on (FUN_140d64410(1): (0x10375 & 0x12) == 0x12 outside a shootstate).
    pub additive_shoot: bool,
    loaded: Spring,
    /// Bring rates to compute when the BRINGDOWN is processed: (weapon going down, weapon coming up).
    bring_rates_pending: Option<(usize, Option<usize>)>,
    slots: [Option<Slot>; 11],
    pub now: i32,
    /// Weapon events raised outside FireWeapon (StopFireSound from ReleaseTrigger), returned by the next
    /// `update` / `anim_event`.
    out: Vec<WeaponEvent>,
    /// idMeleeTrace (hands+0x101c8): the caller runs `melee_update` every frame.
    pub melee: MeleeTrace,
    /// hands+0x102d4: last melee start.
    pub melee_time: i32,
    /// hands+0x10390: the fists' next punch side.
    pub melee_right: bool,
    /// String arguments of the anim event being handled (WebEvent::AnimArgs).
    event_strings: Vec<String>,
    /// Last frame's BUTTON_ATTACK1 / BUTTON_ALTFIRE (usercmd pressed = down now, up before).
    attack1_last: bool,
    altfire_last: bool,
    /// (weapon, fire mode) seen last frame: a mode the weapon changed itself (a lock's fire-mode override
    /// restored, weapons::targeting) is handled like a trigger-mode switch.
    seen_fire_mode: Option<(usize, usize)>,
    /// The fire mode changed this frame: the hands re-read shootAnimRate / shootDelayScale (0x140d66230 /
    /// 0x140d66080).
    mode_changed: bool,
}

impl Hands {
    pub fn new(num_weapons: usize) -> Self {
        Self {
            pending: HandsAction::None,
            pending_since: 0,
            flags: HandsFlags::default(),
            latch: [false; 2],
            fire: false,
            attack_held: false,
            attack_pressed: false,
            held_last: false,
            target: HandsState::Idle,
            new_weapon: None,
            pending_item: None,
            queued_item: None,
            trigger_pulled: false,
            scalars: HandsScalars::default(),
            requests: Vec::new(),
            intro_played: vec![false; num_weapons],
            intro_bringups: false,
            additive_shoot: false,
            loaded: Spring::weapon_loaded(),
            bring_rates_pending: None,
            slots: Default::default(),
            now: 0,
            out: Vec::new(),
            melee: MeleeTrace::default(),
            melee_time: 0,
            melee_right: false,
            event_strings: Vec::new(),
            attack1_last: false,
            altfire_last: false,
            seen_fire_mode: None,
            mode_changed: false,
        }
    }

    /// GetHandsState 0x140d60380(hands, 0): TRANSITIONING while a blend runs or without a node.
    pub fn hands_state(web: &dyn HandsWeb) -> HandsState {
        if web.is_blending() {
            return HandsState::Transitioning;
        }
        match web.current() {
            Some((_, s)) if !s.is_empty() => HandsState::of_state(s),
            _ => HandsState::Transitioning,
        }
    }

    // ---- zoom (weapons::zoom) ----

    /// 0x140d67840: zoomPCT = z, weaponZoomPCT = z^hands_weaponZoomPCTPower (0.3), when z changes.
    pub fn set_zoom_pct(&mut self, z: f32) {
        if z != self.scalars.zoom_pct {
            self.scalars.zoom_pct = z;
            self.scalars.weapon_zoom_pct = z.powf(HANDS_WEAPON_ZOOM_PCT_POWER);
        }
    }

    /// 0x140d634c0: the hands' web state lets the player zoom. Web state handles are the hands' name table
    /// (hands+0x30 + 8*i, HANDS.md "State names").
    pub fn zoom_state_ok(&self, arsenal: &Arsenal, web: &dyn HandsWeb) -> bool {
        let Some((_, cur)) = web.current() else { return false };
        if cur.is_empty() || cur == "generic_hide" || self.target == HandsState::Hidden {
            return false;
        }
        let f = &self.flags;
        if !f.has(F77, 0x10) || f.has(F77, 1) || (f.has(F77, 4) && !f.has(F77, 8)) || (f.has(F79, 8) && !f.has(F79, 0x10)) {
            return false;
        }
        if !matches!(cur, "idle" | "dryfire" | "dryfirestate" | "bringup") && !f.has(F75, 4) && !f.has(F76, 4) {
            if f.has(F74, 2) || f.has(F76, 1) {
                return arsenal.def().zoom.has_blended_zoom;
            }
            if !matches!(cur, "throw" | "throwcookthrow" | "fall" | "land_sm" | "land_med" | "land_lg") && self.target != HandsState::Idle && self.target != HandsState::FallIdle {
                return self.target == HandsState::ChargeIdle;
            }
        }
        true
    }

    /// 0x140d69f90: no intro / hide flags and not HIDDEN / swimming / virtual GUI / power core.
    pub fn zoom_flags_ok(&self, web: &dyn HandsWeb) -> bool {
        let b = self.flags.0[F79];
        if b & 1 != 0 || (b & 8 != 0 && b & 0x10 == 0) || 0x80 <= b {
            return false;
        }
        let mut s = Self::hands_state(web);
        if s == HandsState::Transitioning {
            s = self.target;
        }
        s != HandsState::Hidden && !(15..=17).contains(&(s as i32))
    }

    /// 0x140d63390 (without the hide flags, which weapons::zoom keeps, and the player's weapon test).
    pub fn hidden(&self, web: &dyn HandsWeb) -> bool {
        Self::hands_state(web) == HandsState::Hidden || self.target == HandsState::Hidden
    }

    // ---- set-up ----

    /// idHands::SetWeapon 0x140d5d7f0 + ResetAnimWeb 0x140d65d30 for the weapon the player spawns with:
    /// the web starts in the bring-up (or intro) and is sent to idle.
    pub fn start(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb) {
        let w = arsenal.current;
        self.now = arsenal.time_ms;
        self.set_weapon(arsenal, web, w);
        let sub = arsenal.defs[w].hands.subweb.clone();
        let from = if self.intro_bringup(arsenal, w) {
            self.scalars.bring_up_intro_anim_select = 0.0;
            "bringup_intro"
        } else if self.intro_accent_bringup(arsenal, w) {
            self.scalars.bring_up_intro_anim_select = 1.0;
            "bringup_intro"
        } else {
            "bringup"
        };
        self.bring_rates(arsenal, web, None, Some(w));
        self.flush_scalars(web);
        self.force(web, 0x25ef, &sub, from, 0);
        self.change(web, 0x25ef, &sub, "idle");
        self.intro_played[w] = true;
        self.flags.and(F78, 0x3f);
    }

    /// idHands::SelectWeapon 0x140d66eb0 (slot 2), called by the player once its selection is due
    /// (weapons::select::WeaponSelect): SetPendingActionWeapon_ 0x140d689f0 FORCES the BRINGDOWN (no
    /// approval, it replaces e.g. a pending FIRE), the incoming weapon is set and the bring rates are
    /// computed for (current, new). Refused when `idx` is already the incoming/current weapon or already
    /// pending; while HIDDEN the game parks the weapon (hands+0xfad0, not ported) and refuses.
    pub fn select(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb, idx: usize) -> bool {
        if idx >= arsenal.defs.len() || idx == self.new_weapon.unwrap_or(arsenal.current) {
            return false;
        }
        if Self::hands_state(web) == HandsState::Hidden || self.target == HandsState::Hidden {
            return false;
        }
        if self.pending == HandsAction::Bringdown && self.pending_item == Some(idx) {
            self.pending_since = arsenal.time_ms;
            return false;
        }
        self.pending = HandsAction::Bringdown;
        self.pending_item = Some(idx);
        self.pending_since = arsenal.time_ms;
        self.new_weapon = Some(idx);
        self.bring_rates(arsenal, web, Some(arsenal.current), Some(idx));
        true
    }

    // ---- per frame ----

    /// One game frame: weapon think, idHands::Update, the web advancing (events -> FireWeapon) and the
    /// kick. `arsenal.time_ms` advances by `ms`.
    pub fn tick(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, ms: i32, inp: &HandsInput) -> Vec<WeaponEvent> {
        let hs = Self::hands_state(web);
        // UpdateWeapon_Chaingun 0x140d7f390: spin request = !HIDDEN && (ALTFIRE || attack) -> SetSpinRequest
        // 0x140ec9ea0, which latches the idle spin (+0x1c14) only for weaponData allowIdleBarrelSpin (gatling);
        // the barrel spins for that or a pulled trigger (taken as the attack button held).
        let idle_spin = arsenal.chaingun_data(arsenal.current).is_some_and(|(cg, _)| cg.allow_idle_barrel_spin) && (inp.altfire || inp.weapon.trigger);
        let spin = arsenal.def().chaingun.is_some() && hs != HandsState::Hidden && (inp.weapon.trigger || idle_spin);
        let mut ev = arsenal.begin_frame(ms, &inp.weapon, spin);
        ev.extend(self.update(arsenal, web, ms, inp));
        let now = arsenal.time_ms;
        for e in web.update(now) {
            ev.extend(self.dispatch(arsenal, web, &e, inp));
        }
        let w = arsenal.current;
        arsenal.charge_events(w, now, &mut ev);
        arsenal.end_frame(ms, inp.weapon.trigger);
        ev
    }

    /// Route one web event.
    pub fn dispatch(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, e: &WebEvent, inp: &HandsInput) -> Vec<WeaponEvent> {
        match e {
            WebEvent::Anim { name, int } => self.anim_event(arsenal, web, name, *int, inp),
            WebEvent::AnimArgs { name, int, strings } => {
                self.event_strings = strings.clone();
                let ev = self.anim_event(arsenal, web, name, *int, inp);
                self.event_strings.clear();
                ev
            }
            WebEvent::Node { kind, sub, state } => {
                self.node_event(arsenal, *kind, sub, state);
                Vec::new()
            }
        }
    }

    /// idHands::Update 0x140d6b2c0 up to the animator service: weapon update, then the pending action
    /// becomes state requests. Weapon events emitted here are dry-fire clicks (0x140f1b8b0).
    pub fn update(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, ms: i32, inp: &HandsInput) -> Vec<WeaponEvent> {
        self.requests.clear();
        self.now = arsenal.time_ms;
        let mut ev = Vec::new();
        let Some((sub, state)) = web.current().map(|(a, b)| (a.to_string(), b.to_string())) else { return ev };
        let hs = Self::hands_state(web);
        // idHands::Update: the flags are snapshotted and 0x10376 bit 0x20 (melee started / lunging this
        // frame) is cleared before UpdateWeapon.
        self.flags.and(F76, 0xdf);

        // Player-side requests (the weapon switch arrives through `select`).
        if inp.jumped {
            self.set_pending(arsenal, hs, HandsAction::Jump, None);
        }
        if let Some(l) = inp.landed {
            let a = match l {
                LandSize::NoAnim => HandsAction::LandNoAnim,
                LandSize::Small => HandsAction::LandSm,
                LandSize::Medium => HandsAction::LandMed,
                LandSize::Large => HandsAction::LandLg,
            };
            self.set_pending(arsenal, hs, a, None);
        }

        self.update_weapon(arsenal, web, ms, inp, hs, &mut ev);

        // The mod switch's generic hide / unhide (weapons::modswitch). INTERIM: handled here for every
        // non-hidden (hide) / the hidden (unhide) state instead of per state as the exe's switch does.
        match self.pending {
            HandsAction::GenericHide | HandsAction::GenericHideSlow | HandsAction::GenericHideExtraSlow if hs != HandsState::Hidden && hs != HandsState::Transitioning => {
                self.generic_hide(web, &sub);
                self.flush_scalars(web);
                ev.append(&mut self.out);
                return ev;
            }
            HandsAction::GenericUnhide if hs == HandsState::Hidden => {
                self.generic_unhide(arsenal, web, &sub);
                self.flush_scalars(web);
                ev.append(&mut self.out);
                return ev;
            }
            _ => {}
        }

        match hs {
            HandsState::Idle => self.state_idle(arsenal, web, &sub, inp, false),
            HandsState::FallIdle => self.state_idle(arsenal, web, &sub, inp, true),
            HandsState::LoopingShoot => self.state_looping_shoot(arsenal, web, &sub),
            HandsState::LoopingDryfire => self.state_looping_dryfire(arsenal, web, &sub),
            HandsState::Transitioning => self.state_transitioning(arsenal, web, &sub, &state, inp),
            HandsState::ChargeIdle => self.state_charge_idle(arsenal, web, &sub),
            HandsState::ChargeLoopingShoot => self.state_charge_looping_shoot(arsenal, web, &sub),
            HandsState::ChargeLoopingDryfire => self.state_charge_looping_dryfire(arsenal, web, &sub),
            _ => {}
        }
        self.flush_scalars(web);
        ev.append(&mut self.out);
        ev
    }

    // ---- UpdateWeapon 0x140d7e640 ----

    fn update_weapon(&mut self, arsenal: &mut Arsenal, web: &dyn HandsWeb, ms: i32, inp: &HandsInput, hs: HandsState, ev: &mut Vec<WeaponEvent>) {
        let w = arsenal.current;
        let def = arsenal.cur_def();
        // 0x140d7ed00(hands, 0x140f29e30 [empty appearance], instant 0).
        self.update_weapon_loaded(arsenal.is_empty(w), false, ms);
        // 0x140d7e7ac..0x140d7e91b: targeting slot 0's lockPercent (0x140f147e0(weapon, 0) +0x88):
        // weaponHasLockedTarget = (percent == 1), weaponLockPercent = percent. INTERIM: the weapon model's lock
        // display anims played on the 0 / 1 edges (0x1417005b0) are not driven.
        let lp = arsenal.targeting[w].slots[0].lock_percent;
        self.scalars.weapon_has_locked_target = if lp == 1.0 { 1.0 } else { 0.0 };
        self.scalars.weapon_lock_percent = lp;
        // shootAnimSelect follows "in a shoot state" (0x10375 & 2).
        if !self.flags.has(F75, 2) {
            if self.scalars.shoot_anim_select as i32 != 1 {
                self.scalars.shoot_anim_select = 1.0;
            }
        } else if self.scalars.shoot_anim_select as i32 != 0 {
            self.scalars.shoot_anim_select = 0.0;
        }
        if self.target != HandsState::LoopingShoot && self.target != HandsState::ChargeLoopingShoot {
            if self.flags.0[F75] & 0x12 == 0x12 {
                self.additive_shoot = true;
            } else {
                self.additive_shoot = false;
                if def.hands.has_looping_shoot_state {
                    self.release_trigger(arsenal.current);
                }
            }
        }
        // hands+0x10598: InitWeapon 0x140d62c80 picks the weapon's update function.
        self.update_weapon_default(arsenal, ms, inp, hs, ev);
        if self.mode_changed {
            self.mode_changed = false;
            self.shoot_anim_rate(arsenal, web);
            self.shoot_delay_scale(arsenal, web);
        }
        if def.chaingun.is_some() {
            self.update_weapon_chaingun(arsenal, web, ms);
        }
    }

    /// 0x140d7ed00: weaponLoadedSelect = empty (hands_forceWeaponLoadedAppearance 0); the blend springs to
    /// it (or snaps when `instant`). Skipped while a shot is in flight (0x10375 & 0x22).
    fn update_weapon_loaded(&mut self, empty: bool, instant: bool, ms: i32) {
        if self.flags.has(F75, 2) || self.flags.has(F75, 0x20) {
            return;
        }
        let sel = empty as i32;
        if sel != self.scalars.weapon_loaded_select as i32 {
            self.scalars.weapon_loaded_select = sel as f32;
        }
        self.loaded.target = sel as f32;
        if instant {
            self.loaded.value = sel as f32;
        } else {
            self.loaded.update(ms as f32 * 0.001);
        }
        if self.loaded.value < FLT_MIN {
            self.loaded.value = 0.0;
        }
        self.scalars.weapon_loaded_blend = self.loaded.value;
    }

    /// UpdateWeapon_Default 0x140d7fda0 for a primary-trigger weapon without a secondary fire mode.
    fn update_weapon_default(&mut self, arsenal: &mut Arsenal, ms: i32, inp: &HandsInput, hs: HandsState, ev: &mut Vec<WeaponEvent>) {
        if hs == HandsState::Hidden {
            return;
        }
        let w = arsenal.current;
        // UpdateTargeting (idWeapon vslot +0x370) with the owner's eye / view (weapons::targeting).
        arsenal.think_targeting(w);
        let now = arsenal.time_ms;
        self.fire = false;
        self.attack_held = false;
        self.attack_pressed = false;
        let can_use = inp.can_use_weapon;
        let attack1_pressed = inp.weapon.trigger && !self.attack1_last;
        let altfire_pressed = inp.altfire && !self.altfire_last;
        let altfire_released = !inp.altfire && self.altfire_last;
        self.attack1_last = inp.weapon.trigger;
        self.altfire_last = inp.altfire;
        // A fire mode the weapon changed itself since last frame (a lock's override restored by FinishFire or
        // the targeting update): handled as a mode switch.
        if let Some((sw, sm)) = self.seen_fire_mode {
            let fm = arsenal.mstate[w].fire_mode;
            if sw == w && sm != fm {
                if sm == 0 {
                    self.release_trigger(w);
                }
                self.mode_changed = true;
                ev.push(WeaponEvent::FireMode { weapon: w, mode: fm });
            }
        }
        // With a secondary decl the secondary trigger mode picks the fire mode (0xfb02 -> SetFireMode
        // 0x140d698b0 in ProcessTriggers; weapons::charge::alt_trigger). Leaving mode 0 releases its trigger.
        if arsenal.mode_def(w, 1).is_some() {
            let before = arsenal.mstate[w].fire_mode;
            if let Some(m) = arsenal.alt_trigger(w, inp.altfire, altfire_pressed) {
                if before == 0 {
                    self.release_trigger(w);
                }
                self.mode_changed = true;
                ev.push(WeaponEvent::FireMode { weapon: w, mode: m });
            }
        }
        self.seen_fire_mode = Some((w, arsenal.mstate[w].fire_mode));
        // 0x140d80200..0x140d806b7: the alt button reaches the weapon (weapons::detonate). SECONDARY_PRESS / TAP /
        // TOGGLE (3 / 4 / 0xc) call AltFirePressed on a press, SECONDARY_HOLD_PRIMARY_PRESS / RELEASE (7 / 8) on
        // every held frame; a release calls AltFireReleased. INTERIM: the other trigger modes' flags are not ported.
        let stm = arsenal.secondary_trigger_mode(w);
        let det = match stm {
            3 | 4 | 0xc if altfire_pressed => arsenal.alt_fire_pressed(w),
            7 | 8 if inp.altfire => arsenal.alt_fire_pressed(w),
            3 | 4 | 0xc | 7 | 8 if altfire_released => arsenal.alt_fire_released(w),
            _ => Vec::new(),
        };
        for d in det {
            ev.push(match d {
                super::detonate::DetonateEvent::Explode { weapon, projectiles, sound } => WeaponEvent::Detonate { weapon, projectiles, sound },
                super::detonate::DetonateEvent::Sound { weapon, sound } => WeaponEvent::Sound { weapon, sound },
            });
        }
        let def = arsenal.cur_def();
        let h = &def.hands;
        // ProcessTriggers 0x140d64a00: trigger mode 1 = BUTTON_ATTACK1 (mode 1 of trigger mode 7 fires on it
        // too, usePrimaryFireButton); 0xd (PRIMARY_PRESS_OR_SECONDARY_PRESS, fists) = ATTACK1 or ALTFIRE, pressed
        // when either was pressed. INTERIM press-type secondary triggers pull mode 1 once (press_shot).
        let either = h.trigger_mode == 0xd;
        let press = arsenal.mstate[w].press_shot;
        let held = inp.weapon.trigger || (either && inp.altfire) || press;
        let pressed = attack1_pressed || (either && altfire_pressed) || (press && altfire_pressed);
        let released = !held && self.held_last;
        self.held_last = held;

        // UpdateTriggerLatch 0x140d7c580.
        if (def.allow_shot_queueing || arsenal.can_fire(w, now) || arsenal.is_empty(w)) && self.latch[arsenal.mstate[w].fire_mode] && !held {
            self.latch[arsenal.mstate[w].fire_mode] = false;
        }
        // ProcessTriggers 0x140d64a00 (trigger mode 1).
        if held {
            if !self.latch[arsenal.mstate[w].fire_mode] {
                self.scalars.weapon_charge_for_trigger_event = 0.0;
                self.trigger_pulled = true;
            }
            self.attack_held = true;
            self.attack_pressed = pressed;
        } else if released {
            self.release_trigger(arsenal.current);
        }

        // Melee input (UpdateWeapon_Default after ProcessTriggers): meleeFromMeleeInput weapons melee on
        // BUTTON_ATTACK2, meleeFromFireInput ones (fists) on the attack button, which then does not fire.
        let can_melee = self.can_melee(arsenal, hs);
        let md = &arsenal.defs[self.new_weapon.unwrap_or(w)].melee;
        let mut melee_input = false;
        if !self.attack_held || !md.melee_from_fire_input {
            if md.melee_from_melee_input {
                melee_input = can_melee && inp.melee;
                if melee_input {
                    self.flags.and(F76, 0xbf);
                }
            }
        } else if !md.melee_to_shoot_state || self.attack_pressed {
            self.attack_held = false;
            if can_melee && self.attack_pressed {
                self.flags.or(F76, 0x40);
                melee_input = true;
            }
        }

        // Fire input.
        let zoomed = inp.weapon.spread.zoomed;
        let single_tap = def.single_tap || (zoomed && def.single_tap_ads);
        let empty = arsenal.is_empty(w);
        if single_tap || (empty && !HANDS_AUTO_DRYFIRE) {
            self.fire = self.attack_held && (h.has_looping_dryfire_state || !self.latch[arsenal.mstate[w].fire_mode]);
        } else if !self.latch[arsenal.mstate[w].fire_mode] {
            self.fire = self.attack_held;
        }
        // In a burst the fire input stays on; fire && IsBurstMode && !InBurst && CanFire -> StartBurst
        // 0x140f15710.
        if arsenal.in_burst(w) {
            self.fire = true;
        } else if self.fire && arsenal.is_burst_mode(w) && arsenal.can_fire(w, now) {
            arsenal.start_burst(w, now);
        }
        let ceasefire = self.flags.has(F75, 0x32) && !self.fire && !arsenal.in_burst(w);
        self.fire = can_use && self.fire;

        let target = self.target;
        // DenyFire (idWeapon vslot +0x468, weapons::detonate) comes before CanFire on the fire paths
        // (0x140d80c94..): requireLockToFire without a LOCKED slot 0 (RL lock-on), the explode-on-alt cap.
        let deny = self.fire && !arsenal.is_empty(w) && {
            let (d, snd) = arsenal.deny_fire(w);
            if let Some(sound) = snd {
                ev.push(WeaponEvent::Sound { weapon: w, sound });
            }
            d
        };
        let can_fire = !deny && arsenal.can_fire(w, now);
        let looping = h.has_looping_shoot_state;
        // The CHARGE requests (0x8a4 idle, 0x8b6 looping shoot, 0x901 looping dry fire, 0x947 transitioning): the
        // secondary trigger holds the weapon in mode 1 (0xfb02), it can charge (vslot +0x488) or 0xfaff is set,
        // the charge update set +0x17bc, and not 0x10378 & 8; requested unless already pending or heading to a
        // charge state (0x3b28 - 10 < 3). 0xfaff (0x140d53090) is only written on frames the owner may not use the
        // weapon (player vfunc +0x4c8 not decoded): INTERIM false. 0xfb08 (a refused queued SetFireMode) is not
        // ported: false.
        let fb02 = inp.altfire && matches!(arsenal.secondary_trigger_mode(w), 7..=9);
        let faff = false;
        let charge_req = fb02 && (arsenal.can_charge(w, now) || faff) && arsenal.mstate[w].charge.play_anim && !self.flags.has(F78, 8);
        let charge_ok = self.pending != HandsAction::Charge && !matches!(target, HandsState::ChargeIdle | HandsState::ChargeLoopingShoot | HandsState::ChargeLoopingDryfire);
        match hs {
            HandsState::Idle | HandsState::FallIdle | HandsState::ChargeIdle => {
                if ceasefire && looping {
                    if target != HandsState::Idle && target != HandsState::ChargeIdle {
                        self.set_pending(arsenal, hs, HandsAction::Ceasefire, None);
                    }
                } else if melee_input {
                    // (the target-based sync melee 0x140d6b110 comes first; no AI targets here)
                    self.melee_request(arsenal, hs, altfire_pressed);
                } else if self.fire && inp.weapon_enabled {
                    if !empty && !HANDS_FORCE_DRYFIRE {
                        if can_fire {
                            if !looping {
                                if !self.flags.has(F75, 0x40) {
                                    self.set_pending(arsenal, hs, HandsAction::Fire, None);
                                }
                            } else if target != HandsState::LoopingShoot && target != HandsState::ChargeLoopingShoot {
                                self.set_pending(arsenal, hs, HandsAction::Fire, None);
                            }
                        }
                    } else if self.pending != HandsAction::Reload && !self.flags.has(F75, 0x40) {
                        self.request_dryfire(arsenal, hs, ev);
                    }
                } else if charge_req && charge_ok {
                    self.set_pending(arsenal, hs, HandsAction::Charge, None);
                }
            }
            HandsState::LoopingShoot | HandsState::ChargeLoopingShoot => {
                if !self.pending.is_melee() && self.pending != HandsAction::CookItem {
                    if charge_req {
                        if charge_ok {
                            self.set_pending(arsenal, hs, HandsAction::Charge, None);
                        }
                    } else if ceasefire || !looping {
                        if target != HandsState::Idle && target != HandsState::ChargeIdle {
                            self.set_pending(arsenal, hs, HandsAction::Ceasefire, None);
                        }
                    } else if melee_input {
                        self.melee_request(arsenal, hs, altfire_pressed);
                    } else if !self.fire || !inp.weapon_enabled || empty {
                        let melee_ok = (!self.flags.has(F76, 4) && self.pending != HandsAction::Melee) || (can_use && self.attack_pressed);
                        if self.fire && inp.weapon_enabled && empty && melee_ok && target != HandsState::LoopingDryfire && target != HandsState::ChargeLoopingDryfire {
                            self.request_dryfire(arsenal, hs, ev);
                        }
                    } else {
                        let melee_ok = (!self.flags.has(F76, 4) && !self.pending.is_melee()) || (can_use && self.attack_pressed);
                        if can_fire && melee_ok && target != HandsState::LoopingShoot && target != HandsState::ChargeLoopingShoot {
                            self.set_pending(arsenal, hs, HandsAction::Fire, None);
                        }
                    }
                }
            }
            HandsState::LoopingDryfire | HandsState::ChargeLoopingDryfire => {
                if !self.pending.is_melee() && self.pending != HandsAction::CookItem {
                    if charge_req {
                        if charge_ok {
                            self.set_pending(arsenal, hs, HandsAction::Charge, None);
                        }
                    } else if ceasefire || !self.fire {
                        if target != HandsState::Idle && target != HandsState::ChargeIdle {
                            self.set_pending(arsenal, hs, HandsAction::Ceasefire, None);
                        }
                    } else if melee_input {
                        self.melee_request(arsenal, hs, altfire_pressed);
                    } else {
                        let melee_ok = (!self.flags.has(F76, 4) && !self.pending.is_melee()) || (can_use && self.fire);
                        if !inp.weapon_enabled || !empty {
                            if inp.weapon_enabled && !empty && can_fire && melee_ok && target != HandsState::LoopingShoot && target != HandsState::ChargeLoopingShoot {
                                self.set_pending(arsenal, hs, HandsAction::Fire, None);
                            }
                        } else if melee_ok && target != HandsState::LoopingDryfire && target != HandsState::ChargeLoopingDryfire {
                            self.request_dryfire(arsenal, hs, ev);
                        }
                    }
                }
            }
            HandsState::Transitioning => {
                if charge_req {
                    if charge_ok {
                        self.set_pending(arsenal, hs, HandsAction::Charge, None);
                    }
                } else if (target == HandsState::LoopingShoot || target == HandsState::ChargeLoopingShoot) && !looping {
                    self.set_pending(arsenal, hs, HandsAction::Ceasefire, None);
                } else if ceasefire && looping && !self.flags.has(F76, 2) && target != HandsState::Idle && target != HandsState::ChargeIdle {
                    self.set_pending(arsenal, hs, HandsAction::Ceasefire, None);
                } else if melee_input {
                    self.melee_request(arsenal, hs, altfire_pressed);
                } else if self.fire && !self.flags.has(F75, 0x20) && inp.weapon_enabled && self.pending != HandsAction::CookItem {
                    if !self.flags.has(F74, 2) {
                        let changing = self.flags.has(F77, 4);
                        let open = self.flags.has(F74, 0x40) || HANDS_FORCE_INTERRUPTIBLE || ((self.pending == HandsAction::None || self.pending.is_melee()) && !changing);
                        if (!changing || self.new_weapon.is_none()) && open {
                            if can_fire {
                                if !empty && !HANDS_FORCE_DRYFIRE {
                                    let melee_ok = (!self.flags.has(F76, 4) && !self.pending.is_melee()) || (can_use && self.fire);
                                    if melee_ok {
                                        if !looping || (target != HandsState::LoopingShoot && target != HandsState::ChargeLoopingShoot) {
                                            self.set_pending(arsenal, hs, HandsAction::Fire, None);
                                        }
                                    }
                                } else if !self.flags.has(F75, 0x40) {
                                    self.request_dryfire(arsenal, hs, ev);
                                }
                            }
                        } else if self.flags.has(F79, 1) {
                            self.set_pending(arsenal, hs, HandsAction::Idle, None);
                        }
                    }
                }
            }
            _ => {}
        }
        // Chaingun settle blend, run for every weapon (UpdateWeapon_Default tail).
        self.settle(arsenal, ms, SETTLE_DEFAULT);
    }

    /// The tail of UpdateWeapon_Default / UpdateWeapon_Chaingun: blendChaingunSettle rises by a constant
    /// every frame while the last shot is recent, else falls per second.
    fn settle(&mut self, arsenal: &Arsenal, ms: i32, (thresh, rise, fall): (i32, f32, f32)) {
        let w = arsenal.current;
        let mut s = self.scalars.blend_chaingun_settle;
        if thresh < arsenal.time_ms - arsenal.states[w].last_fire {
            s -= ms as f32 * 0.001 * fall;
            if s <= 0.0 {
                s = 0.0;
            }
        } else {
            s += rise;
            if 1.0 <= s {
                s = 1.0;
            }
        }
        self.scalars.blend_chaingun_settle = s;
    }

    /// UpdateWeapon_Chaingun 0x140d7f390 after the default update: barrel blend (chaingun_QuickSpinupBlend 0),
    /// shootAnimRate follows the spin-dependent interval (0x140ecc720: progressive firing interval), and
    /// the settle blend again.
    fn update_weapon_chaingun(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb, ms: i32) {
        let w = arsenal.current;
        let v = arsenal.states[w].barrel.spin.clamp(0.0, 1.0);
        if v != self.scalars.blend_chaingun_barrel {
            self.scalars.blend_chaingun_barrel = v;
        }
        if arsenal.defs[w].chaingun.is_some_and(|c| c.progressive_firing_interval) {
            self.shoot_anim_rate(arsenal, web);
        }
        self.settle(arsenal, ms, SETTLE_CHAINGUN);
    }
}

/// Result of 0x140d59a30 (idHands pending-action approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Par {
    Approve,
    Deny,
    DenyAndClearTrue,
}

impl Hands {
    // ---- melee (weapons::melee) ----

    /// FUN_140d53170 -> FUN_140d52cc0(hands, 0): not HIDDEN and the weapon has a melee projectile (the
    /// player-side interaction checks are not ported).
    fn can_melee(&self, arsenal: &Arsenal, hs: HandsState) -> bool {
        let d = arsenal.def();
        hs != HandsState::Hidden && (d.melee.projectile.is_some() || !d.projectile.name.is_empty())
    }

    /// The fists' punches (MELEE_RIGHT / MELEE_LEFT, meleeLTRT): idle via melee_r1 / melee_r2 (handles 78 /
    /// 79) or melee_l1 / melee_l2 (80 / 81), alternating by hands+0x10390.
    fn punch(&mut self, web: &mut dyn HandsWeb, sub: &str, left: bool, line: u16) {
        self.flags.or(F75, 1);
        self.clear_pending();
        self.flags.and(F74, 0x3f);
        self.flags.or(F76, 0x24);
        self.melee_time = self.now;
        let via = match (left, self.melee_right) {
            (false, false) => "melee_r1",
            (false, true) => "melee_r2",
            (true, false) => "melee_l1",
            (true, true) => "melee_l2",
        };
        self.via(web, line, sub, "idle", via);
    }

    /// FUN_140d63ee0 (FUN_140d5ae10's player-state gate taken as passing): MELEE, or MELEE_RIGHT /
    /// MELEE_LEFT for meleeLTRT weapons (fists). The melee cooldown hands+0x102d8 is 0.
    fn melee_request(&mut self, arsenal: &Arsenal, hs: HandsState, altfire_pressed: bool) {
        use HandsAction as A;
        if self.pending == A::Bringdown {
            return;
        }
        if !arsenal.def().melee.melee_ltrt {
            self.set_pending(arsenal, hs, A::Melee, None);
        } else if !matches!(self.pending, A::MeleeRight | A::MeleeLeft) {
            let a = if altfire_pressed { A::MeleeLeft } else { A::MeleeRight };
            self.set_pending(arsenal, hs, a, None);
            self.melee_right = !self.melee_right;
        }
        // Sprint melee (0x10378 & 2) needs the sprint state, not ported.
        self.flags.and(F78, 0xfd);
    }

    /// The MELEE branch of HS_IDLE / HS_FALL_IDLE (0x1000 / 0x11c9), HS_LOOPING_SHOOT (0x16b4) and
    /// HS_LOOPING_DRYFIRE (0x183a) for directional-melee weapons (every SP gun): melee_into is forced at
    /// once and the hit / miss is decided on the next HS_TRANSITIONING update (0x10376 & 8).
    fn melee_start(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str, line: u16, looping: bool) {
        let w = arsenal.current;
        if looping {
            self.flags.and(F74, 0x3f);
            self.flags.0[F75] = (self.flags.0[F75] & 0x8d) | 1;
        } else {
            self.flags.or(F75, 1);
        }
        self.clear_pending();
        self.release_trigger(w);
        if !looping {
            self.flags.and(F74, 0x3f);
        }
        self.flags.or(F76, 0x2c);
        self.melee_time = self.now;
        self.melee_select(arsenal);
        self.force(web, line, sub, "melee_into", 0);
    }

    /// 0x140d66e40: meleeSelect = 1 for a sprint melee of a hasSprintMelee weapon, else 0 (2 in MP).
    fn melee_select(&mut self, arsenal: &Arsenal) {
        let sprint = arsenal.def().melee.has_sprint_melee && self.flags.has(F78, 2);
        self.scalars.melee_select = if sprint { 1.0 } else { 0.0 };
    }

    /// HS_TRANSITIONING with 0x10376 & 8 (the frame after melee_into was forced): with no melee target
    /// (0x140d639f0, player+0x38720 - only AI) the melee misses: idle via melee_miss (0x209f).
    fn melee_decide(&mut self, arsenal: &Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        self.via(web, 0x209f, sub, "idle", "melee_miss");
        self.flags.and(F76, 0xf7);
        if arsenal.def().melee.melee_to_shoot_state {
            self.register(1, sub, "melee_miss", StateEvent::MeleeToShoot);
        } else {
            self.register(1, sub, "melee_out", StateEvent::MeleeEnded);
        }
    }

    /// animWeb_MeleeEnded 0x140d63b50.
    fn melee_ended(&mut self) {
        if self.flags.has(F76, 0x20) || (self.flags.0[F76] & 0xc == 0 && !self.melee.active()) {
            return;
        }
        self.melee.stop();
        self.flags.and(F76, 0xd3);
        if self.pending == HandsAction::Melee {
            self.clear_pending();
        }
    }

    /// One frame of the melee trace (0x140d7c980, run by the caller after `tick` with the followed joint's
    /// world position from last frame's pose, see weapons::melee). Returns the hits.
    pub fn melee_update(&mut self, now: i32, joint: Vec3, eye: Vec3, forward: Vec3, sweep: &mut dyn FnMut(Vec3, Vec3, f32) -> Option<SweepHit>) -> Vec<MeleeHit> {
        let hits = self.melee.update(now, joint, eye, forward, sweep);
        // hands+0x102d0 (something was hit): meleeImmediateTransition -> 0x10374 |= 1.
        if !hits.is_empty() && self.melee.immediate {
            self.flags.or(F74, 1);
        }
        if !self.melee.active() {
            self.melee.stop();
        }
        hits
    }

    // ---- pending actions ----

    /// SetPendingAction_ 0x140d68d60 (SetPendingActionWeapon_ 0x140d689f0 for BRINGDOWN): true when the
    /// action is pending afterwards (already pending counts; its time is refreshed).
    fn set_pending(&mut self, arsenal: &Arsenal, hs: HandsState, a: HandsAction, item: Option<usize>) -> bool {
        if a == self.pending && (a != HandsAction::Bringdown || item == self.pending_item) {
            self.pending_since = self.now;
            return true;
        }
        match self.approve(arsenal, hs, a, item) {
            Par::Approve => {
                self.pending = a;
                self.pending_item = if a == HandsAction::Bringdown { item } else { None };
                self.pending_since = self.now;
                if a == HandsAction::Bringdown {
                    // idHands::SelectWeapon 0x140d66eb0: the incoming weapon and the bring rates are set
                    // as soon as the request is accepted.
                    self.new_weapon = item;
                    self.bring_rates_pending = Some((arsenal.current, item));
                }
                true
            }
            Par::Deny => false,
            Par::DenyAndClearTrue => {
                self.clear_pending();
                false
            }
        }
    }

    /// ClearPendingAction_ 0x140d5b660.
    fn clear_pending(&mut self) {
        self.pending = HandsAction::None;
        self.pending_item = None;
    }

    /// The player's hide / unhide request (0x140dc9b00 with hide reason 2, the mod switch): sets the pending
    /// GENERIC_HIDE_SLOW (0x16) / GENERIC_UNHIDE (0x18) action.
    pub fn request_generic(&mut self, a: HandsAction, now: i32) {
        self.pending = a;
        self.pending_item = None;
        self.pending_since = now;
    }

    /// GENERIC_HIDE(_SLOW / _EXTRA_SLOW) (hands update ~0x1960): genericHideAnimSelect = 0 / 1 / 2 (0x140cca050),
    /// ForceState generic_hide. INTERIM: the blend comes from an unread cvar (DAT_1444b0440): FORCE_BLEND_FRAMES.
    fn generic_hide(&mut self, web: &mut dyn HandsWeb, sub: &str) {
        self.scalars.generic_hide_anim_select = match self.pending {
            HandsAction::GenericHideExtraSlow => 2.0,
            HandsAction::GenericHideSlow => 1.0,
            _ => 0.0,
        };
        self.clear_pending();
        self.force(web, 0x1960, sub, "generic_hide", FORCE_BLEND_FRAMES);
        self.flags.and(F77, 0xef);
        self.target = HandsState::Hidden;
    }

    /// GENERIC_UNHIDE in the hidden state (hands update ~0x15d8): with the weapon's mod-select slot
    /// (0x140f148e0) unhideWeaponModSelect = slot and modSelectAnimRate = 1 / modSpeed (1 without the player
    /// ability, 0x140d69990), then ForceStateVia generic_unhide_mod_select (blend 0); without a slot the normal
    /// unhide through the bring-up (0x15e9).
    fn generic_unhide(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        self.clear_pending();
        let w = arsenal.current;
        let slot = arsenal.mod_select_slot(w);
        self.flags.and(F74, 0x3f);
        self.flags.or(F77, 0xc);
        if slot != 0 {
            self.scalars.unhide_weapon_mod_select = slot as f32;
            self.scalars.mod_select_anim_rate = 1.0;
            // "changing to state idle via generic_unhide_mod_select": the switch anim plays, then idle. The
            // runtime's force_via forces its first state and then paths to the second (as for landings).
            self.force_via(web, 0x15d8, sub, "generic_unhide_mod_select", "idle", 0);
            // animWeb_ChangeWeaponModEnded: taken as the change-weapon end (clears 0x10377 bit 4) when the
            // web leaves the state. INTERIM: the event's handler is not decoded.
            self.register(1, sub, "generic_unhide_mod_select", StateEvent::ChangeWeaponEnded);
            arsenal.mod_select_pending[w] = false;
        } else {
            self.via(web, 0x15e9, sub, "idle", "bringup");
            self.register(1, sub, "bringup", StateEvent::ChangeWeaponEnded);
        }
        self.target = HandsState::Idle;
    }

    /// 0x140d59a30 for the actions the SP guns use. `hs` is GetHandsState (TRANSITIONING while blending).
    fn approve(&self, arsenal: &Arsenal, hs: HandsState, a: HandsAction, item: Option<usize>) -> Par {
        use HandsAction as A;
        use HandsState as S;
        let cur = self.pending;
        if a == A::ForceIdle {
            return Par::Approve;
        }
        if cur == A::ForceIdle || self.flags.has(F7A, 1) {
            return Par::Deny;
        }
        if hs == S::Hidden || self.target == S::Hidden || matches!(cur, A::GenericHide | A::GenericHideSlow) {
            let ai = a as i32;
            if 0x14 < ai {
                if ai < 0x18 {
                    return Par::DenyAndClearTrue;
                }
                if ai == 0x18 {
                    return Par::Approve;
                }
            }
            return Par::Deny;
        }
        if self.flags.has(F77, 4) && !self.flags.has(F77, 8) && !(0x15..=0x17).contains(&(a as i32)) {
            return Par::Deny;
        }
        let hd = arsenal.cur_def();
        let h = &hd.hands;
        match a {
            A::Idle => {
                if cur != A::CustomAnim && self.flags.0[F79] < 0x80 {
                    Par::Approve
                } else {
                    Par::Deny
                }
            }
            A::Fire => {
                if cur == A::Bringdown || cur == A::Reload || (self.flags.0[F75] & 0x22 != 0 && !h.has_shoot_again_state) || self.flags.has(F78, 1) {
                    Par::Deny
                } else {
                    self.approve_default(hs, a)
                }
            }
            A::Dryfire => {
                if cur == A::Bringdown || cur == A::Reload || self.flags.has(F75, 0x40) || self.flags.has(F78, 1) {
                    Par::Deny
                } else {
                    self.approve_default(hs, a)
                }
            }
            A::Bringdown => {
                if item.is_some_and(|i| arsenal.defs[i].hands.subweb.is_empty()) {
                    return Par::Deny;
                }
                if cur == A::CookItem {
                    return Par::Deny;
                }
                if cur == A::Bringdown || cur == A::Bringup || self.new_weapon.is_some() {
                    return Par::Approve;
                }
                self.approve_default(hs, a)
            }
            A::Jump | A::Fall | A::LandNoAnim | A::LandSm | A::LandMed => {
                // hands_landAnimsCannotInterupt 0.
                let t = self.target;
                if t == S::LoopingShoot || t == S::ChargeLoopingShoot || t == S::ChainsawRev || cur == A::CookItem {
                    Par::Deny
                } else if cur == A::Bringdown || cur == A::Bringup || self.new_weapon.is_some() {
                    Par::Deny
                } else if !self.flags.has(F76, 4) && !self.flags.has(F74, 2) && self.flags.0[F75] & 3 == 0 && !self.flags.has(F76, 2) {
                    if t == S::ChargeIdle || self.flags.has(F78, 8) || self.flags.0[F79] >= 0x80 {
                        Par::Deny
                    } else {
                        self.approve_default(hs, a)
                    }
                } else {
                    Par::Deny
                }
            }
            _ => self.approve_default(hs, a),
        }
    }

    /// The tail switch of 0x140d59a30 on the action already pending.
    fn approve_default(&self, hs: HandsState, a: HandsAction) -> Par {
        use HandsAction as A;
        let ok = |b: bool| if b { Par::Approve } else { Par::Deny };
        match self.pending {
            A::None | A::Ceasefire | A::Jump | A::Fall | A::LandNoAnim | A::LandSm | A::LandMed => Par::Approve,
            A::Fire | A::CookItem => ok(a == A::ThrowItem),
            A::Melee | A::MeleeRight | A::MeleeLeft => ok(a == A::Fire),
            A::Bringdown | A::ThrowItem | A::LandLg | A::GenericHide | A::GenericHideSlow | A::GenericHideExtraSlow | A::GenericUnhide => Par::Deny,
            _ => {
                let ai = a as i32;
                ok(matches!(ai, 3 | 7 | 8 | 9 | 0xc | 0xd | 0x10 | 0x12 | 0x13 | 0x14 | 0x1a | 0x1b | 0x1f | 0x26 | 0x27) || hs != HandsState::Transitioning)
            }
        }
    }

    /// The DRYFIRE request in UpdateWeapon_Default: SetPendingAction(DRYFIRE), weapon+0x93c = 0 and the
    /// dry-fire click 0x140f1b8b0. The exe restarts the click on every request frame; one DryFire event
    /// is reported per request that was not already pending.
    fn request_dryfire(&mut self, arsenal: &Arsenal, hs: HandsState, ev: &mut Vec<WeaponEvent>) {
        let was = self.pending == HandsAction::Dryfire;
        self.set_pending(arsenal, hs, HandsAction::Dryfire, None);
        if !was {
            ev.push(WeaponEvent::DryFire { weapon: arsenal.current });
        }
    }

    /// Weapon vfunc +0x300 ReleaseTrigger 0x140f1f920 (the hands call it through 0x140d65150 with the
    /// current fire mode): only when the trigger is PULLED it becomes RELEASED (weapon+0x8f8) and vfunc
    /// +0x650 0x140f1faf0 runs, which calls StopFireSound 0x140f23a80 (trigger mode 1).
    fn release_trigger(&mut self, w: usize) {
        if self.trigger_pulled {
            self.out.push(WeaponEvent::StopFireSound { weapon: w });
        }
        self.trigger_pulled = false;
    }

    /// idHands::Update calls StopFireSound 0x140f23a80 directly before entering a looping dry-fire state.
    fn stop_fire_sound(&mut self, w: usize) {
        self.out.push(WeaponEvent::StopFireSound { weapon: w });
    }

    // ---- web requests (0x3b28 = hands state of the destination, 0x140d60210) ----

    fn change(&mut self, web: &mut dyn HandsWeb, line: u16, sub: &str, to: &'static str) {
        self.target = HandsState::of_state(to);
        web.change_state(sub, to);
        self.requests.push(WebRequest::ChangeState { line, sub: sub.to_string(), to });
    }

    fn via(&mut self, web: &mut dyn HandsWeb, line: u16, sub: &str, to: &'static str, via: &'static str) {
        self.target = HandsState::of_state(to);
        web.change_state_via(sub, to, via);
        self.requests.push(WebRequest::ChangeStateVia { line, sub: sub.to_string(), to, via });
    }

    fn force(&mut self, web: &mut dyn HandsWeb, line: u16, sub: &str, to: &'static str, blend_frames: i32) {
        self.target = HandsState::of_state(to);
        web.force_state(sub, to, blend_frames);
        self.requests.push(WebRequest::ForceState { line, sub: sub.to_string(), to, blend_frames });
    }

    fn force_via(&mut self, web: &mut dyn HandsWeb, line: u16, sub: &str, to: &'static str, via: &'static str, blend_frames: i32) {
        self.target = HandsState::of_state(to);
        web.force_state_via(sub, to, via, blend_frames);
        self.requests.push(WebRequest::ForceStateVia { line, sub: sub.to_string(), to, via, blend_frames });
    }

    /// FUN_140d69950 -> 0x14170b320: post `event` once when web event `kind` happens on (sub, state).
    /// One registration per kind; a new one replaces the old.
    fn register(&mut self, kind: usize, sub: &str, state: &'static str, event: StateEvent) {
        self.slots[kind] = Some(Slot { sub: sub.to_string(), state, event });
    }

    /// FUN_140d5b790 -> 0x1417296d0: drop the `kind` registration for (sub, state).
    fn unregister(&mut self, kind: usize, sub: &str, state: &str) {
        if self.slots[kind].as_ref().is_some_and(|s| s.sub == sub && s.state == state) {
            self.slots[kind] = None;
        }
    }

    /// The pending node registrations, as (kind, sub, state, event).
    pub fn registrations(&self) -> Vec<(usize, &str, &str, StateEvent)> {
        self.slots.iter().enumerate().filter_map(|(k, s)| s.as_ref().map(|s| (k, s.sub.as_str(), s.state, s.event))).collect()
    }

    fn flush_scalars(&self, web: &mut dyn HandsWeb) {
        for (n, v) in self.scalars.pairs() {
            web.set_scalar(n, v);
        }
    }

    // ---- the switch in idHands::Update ----

    /// HS_IDLE (case 0) and HS_FALL_IDLE (case 3).
    fn state_idle(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str, inp: &HandsInput, fall: bool) {
        use HandsAction as A;
        if fall {
            if let Some(n) = self.new_weapon.take() {
                self.queued_item = Some(n);
            }
            let f = &mut self.flags;
            f.and(F74, 0x39);
            f.and(F75, 0xfc);
            f.and(F76, 0x7b);
            f.and(F75, 0x8f);
            f.and(F76, 0xfd);
            f.and(F78, 0xc7);
            f.and(F79, 0x3f);
            f.or(F77, 0x10);
        } else {
            if self.queued_item.is_some() && self.pending == A::None {
                self.pending = A::Bringdown;
                self.pending_item = self.queued_item.take();
            }
            self.new_weapon = None;
            let f = &mut self.flags;
            f.and(F74, 0x39);
            f.and(F75, 0xfc);
            f.and(F76, 0x7b);
            f.and(F75, 0x83);
            f.and(F76, 0xfd);
            f.and(F77, 0xda);
            f.and(F78, 0xc7);
            f.and(F79, 0x3f);
            f.and(F7A, 0xfe);
            f.or(F77, 0x10);
        }
        let h = arsenal.cur_def().hands.clone();
        match self.pending {
            A::None => {
                if !fall {
                    if inp.falling && self.approve(arsenal, HandsState::Idle, A::Fall, None) == Par::Approve {
                        self.flags.or(F74, 0x40);
                        self.clear_pending();
                        self.via(web, 0x10dd, sub, "fall_loop", "fall");
                    }
                } else if !inp.falling {
                    self.flags.or(F74, 0x40);
                    self.clear_pending();
                    self.change(web, 0x127d, sub, "idle");
                }
            }
            A::Idle => {
                if fall {
                    self.force(web, 0x10fe, sub, "idle", FORCE_BLEND_FRAMES);
                }
                self.clear_pending();
            }
            A::LandNoAnim => {
                if fall {
                    self.flags.or(F74, 0x40);
                    self.clear_pending();
                    self.change(web, 0x1105, sub, "idle");
                } else {
                    self.clear_pending();
                }
            }
            A::LandSm | A::LandMed | A::LandLg => {
                let (line, to) = match (self.pending, fall) {
                    (A::LandSm, false) => (0xf41, "land_sm"),
                    (A::LandMed, false) => (0xf47, "land_med"),
                    (A::LandLg, false) => (0xf4d, "land_lg"),
                    (A::LandSm, true) => (0x110b, "land_sm"),
                    (A::LandMed, true) => (0x1111, "land_med"),
                    _ => (0x1117, "land_lg"),
                };
                self.flags.or(F74, 0x40);
                self.clear_pending();
                self.force_via(web, line, sub, to, "idle", FORCE_BLEND_FRAMES);
            }
            A::Dryfire => {
                self.flags.0[F75] = (self.flags.0[F75] & 0xed) | 0x40;
                self.clear_pending();
                if !h.has_looping_dryfire_state {
                    if fall && !inp.falling {
                        return;
                    }
                    self.latch = [true, true];
                    if fall {
                        self.via(web, 0x1139, sub, "fall_loop", "dryfire");
                    } else {
                        self.via(web, 0xf6f, sub, "idle", "dryfire");
                    }
                    self.register(1, sub, "dryfire", StateEvent::DryfireEnded);
                } else {
                    self.stop_fire_sound(arsenal.current);
                    self.flags.or(F74, 0x40);
                    self.change(web, if fall { 0x1134 } else { 0xf6a }, sub, "dryfirestate");
                    self.register(1, sub, "dryfirestate", StateEvent::DryfireEnded);
                }
            }
            A::Fire => self.fire_request(arsenal, web, sub, None),
            A::Ceasefire => self.clear_pending(),
            A::Charge => self.charge_enter(arsenal, web, sub, if fall { 0x126c } else { 0x10c6 }),
            A::Bringdown => self.bringdown(arsenal, web, sub, true),
            A::Melee if arsenal.def().melee.has_directional_melee => {
                self.melee_start(arsenal, web, sub, if fall { 0x11c9 } else { 0x1000 }, false);
            }
            A::MeleeRight => self.punch(web, sub, false, if fall { 0x11a9 } else { 0xfdf }),
            A::MeleeLeft => self.punch(web, sub, true, if fall { 0x11b3 } else { 0xfe9 }),
            A::Jump => {
                if !fall {
                    self.flags.or(F74, 0x40);
                    self.clear_pending();
                    self.via(web, 0x10bd, sub, "fall_loop", "jump");
                } else {
                    self.clear_pending();
                }
            }
            _ => self.clear_pending(),
        }
    }

    /// HS_LOOPING_SHOOT_STATE (case 8).
    fn state_looping_shoot(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        use HandsAction as A;
        if let Some(n) = self.new_weapon.take() {
            self.queued_item = Some(n);
        }
        let f = &mut self.flags;
        f.and(F74, 0xf9);
        f.or(F75, 3);
        f.and(F76, 0x7f);
        f.or(F75, 0x10);
        f.and(F76, 0xfd);
        f.and(F77, 0xfe);
        f.and(F78, 0xc7);
        f.and(F79, 0x3f);
        f.or(F77, 0x10);
        let h = arsenal.cur_def().hands.clone();
        match self.pending {
            A::None => {}
            A::Idle => {
                self.scalars.shoot_anim_select = 1.0;
                self.release_trigger(arsenal.current);
                self.force_via(web, 0x1632, sub, "idle", "shootstate_recovery", FORCE_BLEND_FRAMES);
                self.clear_pending();
            }
            A::LandLg => {
                self.flags.and(F75, 0x8c);
                self.clear_pending();
                self.force_via(web, 0x1658, sub, "land_lg", "idle", FORCE_BLEND_FRAMES);
            }
            A::Ceasefire => {
                self.scalars.shoot_anim_select = 1.0;
                self.release_trigger(arsenal.current);
                self.flags.and(F75, 0x8c);
                self.flags.or(F74, 0x40);
                self.via(web, 0x1716, sub, "idle", "shootstate_recovery");
                self.clear_pending();
            }
            A::Dryfire => {
                self.flags.0[F75] = (self.flags.0[F75] & 0xed) | 0x40;
                self.scalars.shoot_anim_select = 1.0;
                self.release_trigger(arsenal.current);
                self.clear_pending();
                if !h.has_looping_dryfire_state {
                    self.latch = [true, true];
                    self.via(web, 0x1738, sub, "idle", "dryfire");
                    self.register(1, sub, "dryfire", StateEvent::DryfireEnded);
                } else {
                    self.stop_fire_sound(arsenal.current);
                    self.flags.or(F74, 0x40);
                    self.change(web, 0x1733, sub, "dryfirestate");
                    self.register(1, sub, "dryfirestate", StateEvent::DryfireEnded);
                }
            }
            A::Bringdown => {
                self.scalars.shoot_anim_select = 1.0;
                self.release_trigger(arsenal.current);
                self.flags.and(F75, 0x8c);
                self.bringdown(arsenal, web, sub, false);
            }
            A::Charge => self.charge_enter(arsenal, web, sub, 0x1720),
            A::Melee if arsenal.def().melee.has_directional_melee => self.melee_start(arsenal, web, sub, 0x16b4, true),
            A::MeleeRight | A::MeleeLeft => {
                self.release_trigger(arsenal.current);
                let left = self.pending == A::MeleeLeft;
                self.punch(web, sub, left, if left { 0x1695 } else { 0x1684 });
            }
            _ => self.clear_pending(),
        }
    }

    /// HS_LOOPING_DRYFIRE_STATE (case 9).
    fn state_looping_dryfire(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        use HandsAction as A;
        if let Some(n) = self.new_weapon.take() {
            self.queued_item = Some(n);
        }
        let f = &mut self.flags;
        f.and(F74, 0xf9);
        f.0[F75] = (f.0[F75] & 0xfd) | 1;
        f.and(F76, 0x7f);
        f.and(F75, 0x8f);
        f.and(F76, 0xfd);
        f.and(F77, 0xfe);
        f.and(F78, 0xc7);
        f.and(F79, 0x3f);
        f.or(F77, 0x10);
        match self.pending {
            A::None => {}
            A::Idle => {
                self.force(web, 0x17c3, sub, "idle", FORCE_BLEND_FRAMES);
                self.clear_pending();
            }
            A::Fire => {
                self.scalars.shoot_anim_select = 0.0;
                self.scalars.weapon_can_reload_after_firing_select = (!can_reload_after_firing(arsenal)) as i32 as f32;
                self.flags.0[F75] = (self.flags.0[F75] & 0x6c) | 0x20;
                self.clear_pending();
                self.flags.or(F74, 0x40);
                self.change(web, 0x17ee, sub, "shootstate");
                self.register(0, sub, "shootstate_into", StateEvent::ShootStarted);
                self.register(1, sub, "shootstate", StateEvent::ShootEnded);
            }
            A::Ceasefire => {
                self.flags.and(F75, 0x8c);
                self.flags.or(F74, 0x40);
                self.scalars.shoot_anim_select = 1.0;
                self.change(web, 0x18a1, sub, "idle");
                self.clear_pending();
            }
            A::Jump => {
                self.flags.or(F74, 0x40);
                self.clear_pending();
                self.via(web, 0x180a, sub, "fall_loop", "jump");
            }
            A::Bringdown => {
                self.release_trigger(arsenal.current);
                self.flags.and(F75, 0x8c);
                self.bringdown(arsenal, web, sub, false);
            }
            A::Charge => self.charge_enter(arsenal, web, sub, 0x18f3),
            A::Melee if arsenal.def().melee.has_directional_melee => self.melee_start(arsenal, web, sub, 0x183a, true),
            A::MeleeRight => self.punch(web, sub, false, 0x1814),
            A::MeleeLeft => self.punch(web, sub, true, 0x181e),
            _ => self.clear_pending(),
        }
    }

    /// The HANDSACTION_CHARGE branch of HS_IDLE (0x10c6) / FALL_IDLE (0x126c) / LOOPING_SHOOT (0x1720) /
    /// LOOPING_DRYFIRE (0x18f3) / TRANSITIONING (0x1f89): shootAnimSelect 1 (0x140cca7d0), and for a
    /// hasChargeState decl: interruptible, 0x10378 |= 0x18 (in charge, blending into it), the charge rates
    /// (0x140d67270), charge_idle (through the web's charge_into) with BlendToChargeEnded on leaving charge_into.
    fn charge_enter(&mut self, arsenal: &Arsenal, web: &mut dyn HandsWeb, sub: &str, line: u16) {
        self.scalars.shoot_anim_select = 1.0;
        self.clear_pending();
        if !arsenal.cur_def().has_charge_state {
            return;
        }
        self.flags.or(F74, 0x40);
        self.flags.or(F78, 0x18);
        self.charge_rates(arsenal, web);
        self.change(web, line, sub, "charge_idle");
        self.register(1, sub, "charge_into", StateEvent::BlendToChargeEnded);
    }

    /// 0x140d67270: chargeIntoRateScale = len(charge_into) / (ChargeTime / 960 s), chargeOutRateScale =
    /// len(charge_out) / (DischargeTimeout / 960 s); lengths are the hands-web states' frames / 30; 1 unless
    /// both are positive.
    fn charge_rates(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb) {
        let w = arsenal.current;
        let d = arsenal.cur_def();
        let mode = arsenal.mstate[w].fire_mode;
        let len = |state: &str| web.state_anim_frames(&d.hands.subweb, state).map(|f| f as f32 / 30.0).unwrap_or(0.0);
        let (into, out) = (len("charge_into"), len("charge_out"));
        let t = arsenal.charge_time(w, mode) as f32 / TICKS_PER_SEC;
        let o = arsenal.discharge_timeout(w, mode) as f32 / TICKS_PER_SEC;
        self.scalars.charge_into_rate_scale = if 0.0 < t && 0.0 < into { into / t } else { 1.0 };
        self.scalars.charge_out_rate_scale = if 0.0 < o && 0.0 < out { out / o } else { 1.0 };
    }

    /// The exits of the charge states to idle via charge_out: 0x10378 = (& ~8) | 0x20, interruptible,
    /// BlendFromChargeEnded when the web leaves charge_out.
    fn charge_out(&mut self, web: &mut dyn HandsWeb, sub: &str, line: u16) {
        self.flags.0[F78] = (self.flags.0[F78] & 0xf7) | 0x20;
        self.flags.or(F74, 0x40);
        self.clear_pending();
        self.change(web, line, sub, "idle");
        self.register(1, sub, "charge_out", StateEvent::BlendFromChargeEnded);
    }

    /// The FIRE set-up of the charge states (0x1c23 / 0x1e77): shootAnimSelect 0,
    /// weaponCanReloadAfterFiringSelect, 0x10375 = (& 0x6c) | 0x20.
    fn charge_fire_setup(&mut self, arsenal: &Arsenal) {
        self.scalars.shoot_anim_select = 0.0;
        self.scalars.weapon_can_reload_after_firing_select = (!can_reload_after_firing(arsenal)) as i32 as f32;
        self.flags.0[F75] = (self.flags.0[F75] & 0x6c) | 0x20;
        self.clear_pending();
    }

    /// HS_CHARGE_IDLE (case 10). Bring-down / melee requests take the same paths as the other states (their
    /// debug line ids differ: 0x1c6d.. bring-down).
    fn state_charge_idle(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        use HandsAction as A;
        self.new_weapon = None;
        let f = &mut self.flags;
        f.and(F74, 0xb9);
        f.and(F75, 0xfc);
        f.and(F76, 0x7b);
        f.and(F75, 0x83);
        f.and(F76, 0xfd);
        f.and(F77, 0xda);
        f.0[F78] = (f.0[F78] & 0xcf) | 8;
        f.and(F79, 0x3f);
        f.or(F77, 0x10);
        let w = arsenal.current;
        let d = arsenal.cur_def();
        if self.pending == A::Idle {
            self.clear_pending();
        }
        match self.pending {
            // Stays while the weapon charges, the decl keeps charge_idle without charging, or 0xfaff.
            A::None => {
                if !(d.has_charge_state && (arsenal.can_charge(w, arsenal.time_ms) || d.can_use_charge_state_when_not_charging)) {
                    self.charge_out(web, sub, 0x1d17);
                }
            }
            A::Charge | A::Ceasefire => self.clear_pending(),
            A::Fire => {
                self.charge_fire_setup(arsenal);
                if !d.hands.has_looping_shoot_state {
                    self.via(web, 0x1c29, sub, "charge_idle", "charge_shoot");
                    self.register(0, sub, "charge_shoot", StateEvent::ShootStarted);
                    self.register(1, sub, "charge_shoot", StateEvent::ShootEnded);
                } else {
                    self.flags.or(F74, 0x40);
                    self.change(web, 0x1c26, sub, "charge_shootstate");
                    self.register(0, sub, "charge_shootstate_into", StateEvent::ShootStarted);
                }
            }
            A::Dryfire => {
                self.flags.0[F75] = (self.flags.0[F75] & 0xed) | 0x40;
                self.flags.and(F74, 0x39);
                self.clear_pending();
                if !d.hands.has_looping_dryfire_state {
                    self.latch = [true, true];
                    self.via(web, 0x1c41, sub, "charge_idle", "charge_dryfire");
                    self.register(1, sub, "charge_dryfire", StateEvent::DryfireEnded);
                } else {
                    self.stop_fire_sound(w);
                    self.flags.or(F74, 0x40);
                    self.change(web, 0x1c3c, sub, "charge_dryfirestate");
                    self.register(1, sub, "charge_dryfirestate", StateEvent::DryfireEnded);
                }
            }
            A::Bringdown => self.bringdown(arsenal, web, sub, false),
            A::Melee if arsenal.def().melee.has_directional_melee => {
                self.flags.and(F78, 0xf7);
                self.melee_start(arsenal, web, sub, 0x1ccd, false);
            }
            A::MeleeRight | A::MeleeLeft => {
                self.flags.and(F78, 0xf7);
                let left = self.pending == A::MeleeLeft;
                self.punch(web, sub, left, if left { 0x1cb5 } else { 0x1caa });
            }
            _ => self.clear_pending(),
        }
    }

    /// HS_CHARGE_LOOPING_SHOOT_STATE (case 0xb).
    fn state_charge_looping_shoot(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        use HandsAction as A;
        if let Some(n) = self.new_weapon.take() {
            self.queued_item = Some(n);
        }
        let f = &mut self.flags;
        f.and(F74, 0xf9);
        f.or(F75, 3);
        f.and(F76, 0x7f);
        f.or(F75, 0x10);
        f.and(F76, 0xfd);
        f.and(F77, 0xfe);
        f.0[F78] = (f.0[F78] & 0xcf) | 8;
        f.and(F79, 0x3f);
        f.or(F77, 0x10);
        let w = arsenal.current;
        let d = arsenal.cur_def();
        match self.pending {
            A::Charge => self.clear_pending(),
            A::Idle => {
                self.force(web, 0x1d39, sub, "charge_idle", FORCE_BLEND_FRAMES);
                self.clear_pending();
            }
            A::Ceasefire => {
                self.flags.and(F75, 0x8c);
                self.flags.or(F74, 0x40);
                self.scalars.shoot_anim_select = 1.0;
                self.via(web, 0x1d57, sub, "charge_idle", "charge_shootstate_recovery");
                self.clear_pending();
            }
            A::Bringdown => {
                self.scalars.shoot_anim_select = 1.0;
                self.release_trigger(w);
                self.flags.and(F75, 0x8c);
                self.bringdown(arsenal, web, sub, false);
            }
            A::Dryfire => {
                self.clear_pending();
                if !d.hands.has_looping_dryfire_state {
                    self.latch = [true, true];
                    self.via(web, 0x1d8f, sub, "charge_idle", "charge_dryfire");
                    // (the exe registers this one on "dryfire", not "charge_dryfire")
                    self.register(1, sub, "dryfire", StateEvent::DryfireEnded);
                } else {
                    self.stop_fire_sound(w);
                    self.flags.or(F74, 0x40);
                    self.change(web, 0x1d8a, sub, "charge_dryfirestate");
                    self.register(1, sub, "charge_dryfirestate", StateEvent::DryfireEnded);
                }
            }
            A::Melee if arsenal.def().melee.has_directional_melee => {
                self.flags.and(F78, 0xf7);
                self.melee_start(arsenal, web, sub, 0x1ddd, true);
            }
            A::MeleeRight | A::MeleeLeft => {
                self.flags.and(F78, 0xf7);
                self.release_trigger(w);
                let left = self.pending == A::MeleeLeft;
                self.punch(web, sub, left, if left { 0x1dc5 } else { 0x1dba });
            }
            _ => {
                // Every other action, NONE included: out via charge_out once the weapon cannot charge (0xfaff
                // alone would go to charge_dryfirestate, 0x1e20).
                if !d.has_charge_state || !arsenal.can_charge(w, arsenal.time_ms) {
                    self.charge_out(web, sub, 0x1e1b);
                } else if self.pending != A::None {
                    self.clear_pending();
                }
            }
        }
    }

    /// HS_CHARGE_LOOPING_DRYFIRE_STATE (case 0xc).
    fn state_charge_looping_dryfire(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str) {
        use HandsAction as A;
        if let Some(n) = self.new_weapon.take() {
            self.queued_item = Some(n);
        }
        let f = &mut self.flags;
        f.and(F74, 0xf9);
        f.0[F75] = (f.0[F75] & 0xfd) | 1;
        f.and(F76, 0x7f);
        f.and(F75, 0x8f);
        f.and(F76, 0xfd);
        f.and(F77, 0xfe);
        f.0[F78] = (f.0[F78] & 0xcf) | 8;
        f.and(F79, 0x3f);
        f.or(F77, 0x10);
        let w = arsenal.current;
        let d = arsenal.cur_def();
        match self.pending {
            A::Charge | A::Dryfire => self.clear_pending(),
            A::Idle => {
                self.force(web, 0x1e59, sub, "charge_idle", FORCE_BLEND_FRAMES);
                self.clear_pending();
            }
            A::Fire => {
                self.charge_fire_setup(arsenal);
                self.flags.or(F74, 0x40);
                self.change(web, 0x1e79, sub, "charge_shootstate");
                // (kind 1 goes on the plain "shootstate" node, as the exe registers it)
                self.register(0, sub, "charge_shootstate_into", StateEvent::ShootStarted);
                self.register(1, sub, "shootstate", StateEvent::ShootEnded);
            }
            A::Ceasefire => {
                self.flags.and(F75, 0x8c);
                self.flags.or(F74, 0x40);
                self.scalars.shoot_anim_select = 1.0;
                self.change(web, 0x1e84, sub, "charge_idle");
                self.clear_pending();
            }
            A::Bringdown => {
                self.release_trigger(w);
                self.flags.and(F75, 0x8c);
                self.bringdown(arsenal, web, sub, false);
            }
            A::Melee if arsenal.def().melee.has_directional_melee => {
                self.flags.and(F78, 0xf7);
                self.melee_start(arsenal, web, sub, 0x1eb3, true);
            }
            A::MeleeRight | A::MeleeLeft => {
                self.flags.and(F78, 0xf7);
                let left = self.pending == A::MeleeLeft;
                self.punch(web, sub, left, if left { 0x1e9b } else { 0x1e90 });
            }
            _ => {
                if !d.has_charge_state || (!arsenal.can_charge(w, arsenal.time_ms) && !d.can_use_charge_state_when_not_charging) {
                    self.charge_out(web, sub, 0x1f2c);
                } else if self.pending != A::None {
                    self.clear_pending();
                }
            }
        }
    }

    /// HS_TRANSITIONING (case 0x12).
    fn state_transitioning(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str, state: &str, inp: &HandsInput) {
        use HandsAction as A;
        use HandsState as S;
        self.flags.and(F78, 0xf7);
        let h = arsenal.cur_def().hands.clone();
        // 0x2074: a MELEE interrupts the transition once ae_interruptTransitionForMelee opened it
        // (0x10374 & 0x80) and hands_allowMeleeInterrupt (1) allows it.
        let md = arsenal.def().melee.clone();
        if (self.flags.has(F74, 0x80) || HANDS_FORCE_INTERRUPTIBLE) && self.pending == A::Melee && !self.flags.has(F76, 2) && md.has_directional_melee && HANDS_ALLOW_MELEE_INTERRUPT {
            if self.flags.0[F76] < 0x80 {
                self.melee_interrupt(arsenal, web, sub, 0x2074);
            } else {
                self.clear_pending();
            }
            return;
        }
        if self.pending != A::Ceasefire {
            // (0x1f79 charge_idle when 0x10378 & 8: never, the case clears that bit first.)
            if self.pending == A::Idle && (self.flags.has(F74, 0x40) || HANDS_FORCE_INTERRUPTIBLE) && self.target != S::Idle && self.target != S::ChargeIdle {
                self.change(web, 0x1f7c, sub, "idle");
                self.clear_pending();
            } else if self.pending == A::Charge && (self.flags.has(F74, 0x40) || HANDS_FORCE_INTERRUPTIBLE) {
                self.charge_enter(arsenal, web, sub, 0x1f89);
            }
        } else {
            if self.target != S::Idle {
                self.flags.and(F75, 0xfc);
                self.flags.and(F76, 0x7f);
                self.flags.and(F75, 0x8f);
                self.flags.or(F74, 0x40);
                self.scalars.shoot_anim_select = 1.0;
                if self.target == S::LoopingShoot && (state == "shootstate" || state == "shootstate_into") {
                    self.via(web, 0x1f53, sub, "idle", "shootstate_recovery");
                } else {
                    self.change(web, 0x1f55, sub, "idle");
                }
            }
            self.clear_pending();
        }
        // LAB_140d78f42: a looping shoot whose trigger was let go goes back to idle.
        if self.target == S::LoopingShoot && !self.trigger_pulled {
            self.flags.and(F75, 0x8c);
            if inp.falling {
                self.change(web, 0x1fe9, sub, "fall_loop");
            } else {
                self.flags.or(F74, 0x40);
                self.change(web, 0x1fec, sub, "idle");
            }
        }
        // Heading to charge_idle / charge_dryfirestate while the weapon no longer charges: out via charge_out
        // (0x200c idle, 0x2009 fall_loop when falling and FALL would be approved).
        let w = arsenal.current;
        let d = arsenal.cur_def();
        if self.pending != A::Fire && !self.flags.has(F75, 2) && (!d.has_charge_state || (!arsenal.can_charge(w, arsenal.time_ms) && !d.can_use_charge_state_when_not_charging)) && matches!(self.target, S::ChargeIdle | S::ChargeLoopingDryfire) {
            self.flags.and(F75, 0x9f);
            self.flags.0[F78] = (self.flags.0[F78] & 0xf7) | 0x20;
            if !inp.falling || self.approve(arsenal, S::Transitioning, A::Fall, None) != Par::Approve {
                self.flags.or(F74, 0x40);
                self.change(web, 0x200c, sub, "idle");
            } else {
                self.change(web, 0x2009, sub, "fall_loop");
            }
            self.register(1, sub, "charge_out", StateEvent::BlendFromChargeEnded);
        }
        // A charge looping shoot whose trigger was let go goes back to charge_idle (0x201a).
        if self.target == S::ChargeLoopingShoot && !self.trigger_pulled && !arsenal.in_burst(w) {
            self.flags.and(F75, 0x8c);
            self.flags.or(F74, 0x40);
            self.change(web, 0x201a, sub, "charge_idle");
        }
        if self.flags.has(F76, 8) {
            self.melee_decide(arsenal, web, sub);
            return;
        }
        if !self.flags.has(F74, 0x40) && !HANDS_FORCE_INTERRUPTIBLE {
            return;
        }
        if self.pending == A::LandLg {
            self.flags.or(F74, 0x40);
            self.clear_pending();
            self.force_via(web, 0x20cc, sub, "land_lg", "idle", FORCE_BLEND_FRAMES);
            return;
        }
        // 0x20f1: MELEE while interruptible.
        if self.pending == A::Melee && !self.flags.has(F76, 2) && md.has_directional_melee {
            if self.flags.0[F76] < 0x80 {
                self.melee_interrupt(arsenal, web, sub, 0x20f1);
            } else {
                self.clear_pending();
            }
            return;
        }
        // 0x20fb / 0x2105: the fists' punches while interruptible.
        if self.pending == A::MeleeRight || self.pending == A::MeleeLeft {
            let left = self.pending == A::MeleeLeft;
            self.punch(web, sub, left, if left { 0x2105 } else { 0x20fb });
            return;
        }
        let firing = self.pending == A::Fire || self.pending == A::Dryfire;
        let reloading_block = self.flags.has(F74, 2) && !h.fire_breaks_reload && !h.has_shoot_again_state;
        if !firing || reloading_block {
            match self.pending {
                A::Bringdown => {
                    self.flags.and(F75, 0x8c);
                    self.bringdown(arsenal, web, sub, false);
                }
                A::Jump => {
                    self.flags.or(F74, 0x40);
                    self.flags.and(F75, 0x8c);
                    self.clear_pending();
                    self.via(web, 0x2266, sub, "fall_loop", "jump");
                }
                A::LandNoAnim => {
                    self.clear_pending();
                    self.flags.or(F74, 0x40);
                    self.force(web, 0x2274, sub, "idle", FORCE_BLEND_FRAMES);
                }
                A::LandSm | A::LandMed => {
                    let (line, to) = if self.pending == A::LandSm { (0x2283, "land_sm") } else { (0x228e, "land_med") };
                    self.clear_pending();
                    self.flags.or(F74, 0x40);
                    self.force_via(web, line, sub, to, "idle", FORCE_BLEND_FRAMES);
                }
                _ => {}
            }
            return;
        }
        // FIRE / DRYFIRE while interruptible.
        if self.pending != A::Fire || (self.flags.has(F75, 2) && !arsenal.cur_def().single_tap && !h.has_shoot_again_state) {
            if self.pending != A::Dryfire {
                return;
            }
            self.flags.0[F75] = (self.flags.0[F75] & 0xed) | 0x40;
            self.flags.and(F74, 0x39);
            self.clear_pending();
            let wsub = h.subweb.clone();
            if !h.has_looping_dryfire_state {
                self.latch = [true, true];
                self.via(web, 0x2222, &wsub, "idle", "dryfire");
                self.register(1, &wsub, "dryfire", StateEvent::DryfireEnded);
            } else {
                self.stop_fire_sound(arsenal.current);
                self.change(web, 0x221d, sub, "dryfirestate");
                self.register(1, &wsub, "dryfirestate", StateEvent::DryfireEnded);
            }
            return;
        }
        self.fire_request(arsenal, web, sub, Some(state));
    }

    /// The HS_TRANSITIONING melee starts (0x2074 / 0x20f1).
    fn melee_interrupt(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str, line: u16) {
        let w = arsenal.current;
        self.clear_pending();
        self.release_trigger(w);
        self.flags.and(F74, 0x39);
        self.flags.or(F75, 1);
        self.flags.or(F76, 0xac);
        self.flags.and(F75, 0x8d);
        if line == 0x20f1 {
            self.flags.and(F76, 0xfd);
        }
        self.melee_time = self.now;
        self.melee_select(arsenal);
        self.force(web, line, sub, "melee_into", 0);
    }

    /// The FIRE branch of HS_IDLE / HS_FALL_IDLE (`from` None) or HS_TRANSITIONING (`from` = current
    /// state): picks the shoot state for the weapon's decl flags.
    fn fire_request(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, sub: &str, from: Option<&str>) {
        let w = arsenal.current;
        let h = arsenal.cur_def().hands.clone();
        let idle = from.is_none();
        self.scalars.shoot_anim_select = 0.0;
        self.scalars.weapon_can_reload_after_firing_select = (!can_reload_after_firing(arsenal)) as i32 as f32;
        let sub: String = if idle {
            self.flags.0[F75] = (self.flags.0[F75] & 0x6c) | 0x20;
            sub.to_string()
        } else {
            self.flags.and(F74, 0x39);
            self.flags.and(F77, 0xfb);
            self.flags.0[F75] = (self.flags.0[F75] & 0x3c) | 0x20;
            self.flags.and(F76, 0x7f);
            h.subweb.clone()
        };
        let sub = sub.as_str();
        self.clear_pending();
        self.scalars.shoot_anim_barrel_select = if h.has_shoot_alternating_barrels { ((self.scalars.shoot_anim_barrel_select as i32 == 1) as i32 + 1) as f32 } else { 0.0 };
        self.scalars.weapon_flourish_anim_select = 0.0;
        // Burst shoot states (0x140f232a0): burstInfo[bm].burstShootState set and (InBurst, or a BURST_COUNT
        // charge item with weapon vslot +0x480 < 1): the shot goes to the burst shoot state via idle (0xf9b),
        // a burst's last shot to its lastShotState (0xf97) when set. vslot +0x480 returns 0.0 for idWeapon and
        // every subclass (the HUD's discharging value), so a BURST_COUNT item always qualifies. INTERIM: the
        // transitioning path reuses the idle line ids.
        let d = arsenal.cur_def();
        let bm = arsenal.burst_mode(w).clamp(0, 3) as usize;
        let bi = &d.bursts[bm];
        let burst_item = arsenal.charge_item(w, &super::decl::ChargeProperty::BurstCount).is_some();
        if !bi.burst_shoot_state.is_empty() && (arsenal.in_burst(w) || burst_item) {
            let last = arsenal.mstate[w].burst_count == 1 && !bi.last_shot_state.is_empty();
            let st = static_state(if last { &bi.last_shot_state } else { &bi.burst_shoot_state });
            self.via(web, if last { 0xf97 } else { 0xf9b }, sub, "idle", st);
            self.register(0, sub, st, StateEvent::ShootStarted);
            self.register(1, sub, st, StateEvent::ShootEnded);
            return;
        }
        let (started, ended): (&'static str, &'static str);
        let mut end_event = StateEvent::ShootEnded;
        if h.has_looping_shoot_state {
            self.flags.or(F74, 0x40);
            if h.has_pre_shoot_charge {
                self.via(web, if idle { 0xfa3 } else { 0x21c2 }, sub, "shootstate", "shoot_charge");
            } else {
                self.change(web, if idle { 0xfa5 } else { 0x21c4 }, sub, "shootstate");
            }
            started = "shootstate_into";
            ended = "shootstate";
        } else if h.has_last_shot_anims && arsenal.ammo_for(w) == Some(1) {
            self.via(web, if idle { 0xfaa } else { 0x21ca }, sub, "idle", "shoot_last_round");
            started = "shoot_last_round";
            ended = "shoot_last_round";
        } else if h.has_shoot_to_reload_anims {
            if can_reload_after_firing(arsenal) {
                if idle {
                    self.flags.and(F74, 0xb1);
                    self.via(web, 0xfb6, sub, "idle", "shoot_delay");
                } else {
                    self.flags.0[F74] = (self.flags.0[F74] & 0xb7) | 2;
                    if from == Some("shoot_delay") {
                        self.via(web, 0x21d7, sub, "idle", "shoot_delay_2");
                    } else {
                        self.via(web, 0x21d9, sub, "idle", "shoot_delay");
                    }
                }
                end_event = StateEvent::ShootEndedReloadStarted;
            } else {
                self.via(web, if idle { 0xfba } else { 0x21de }, sub, "idle", "shoot_end_no_reload");
            }
            started = "shoot";
            ended = "shoot";
        } else if h.has_shoot_again_state {
            let (a1, a2) = (self.scalars.shoot_again1_anim_select as i32, self.scalars.shoot_again2_anim_select as i32);
            if idle {
                self.via(web, 0xfbf, sub, "idle", "shoot");
                self.scalars.shoot_again1_anim_select = shoot_again_select(arsenal, a2, a1) as f32;
                started = "shoot";
                ended = "shoot";
            } else if !self.flags.has(F75, 4) {
                self.flags.or(F75, 4);
                self.via(web, 0x21ef, sub, "idle", "shoot_again");
                self.scalars.shoot_again2_anim_select = shoot_again_select(arsenal, a1, a2) as f32;
                self.register(0, sub, "shoot_again", StateEvent::ShootStarted);
                self.unregister(1, sub, "shoot");
                self.register(1, sub, "shoot_again", StateEvent::ShootEnded);
                return;
            } else {
                self.flags.and(F75, 0xfb);
                self.via(web, 0x21e5, sub, "idle", "shoot");
                self.scalars.shoot_again1_anim_select = shoot_again_select(arsenal, a2, a1) as f32;
                self.register(0, sub, "shoot", StateEvent::ShootStarted);
                self.unregister(1, sub, "shoot_again");
                self.register(1, sub, "shoot", StateEvent::ShootEnded);
                return;
            }
        } else if h.shoot_to_idle_alt {
            self.via(web, if idle { 0xfce } else { 0x220b }, sub, "idle_alt", "shoot");
            started = "shoot";
            ended = "shoot";
        } else if h.has_pre_shoot_charge {
            self.via(web, if idle { 0xfc9 } else { 0x21fc }, sub, "idle", "shoot_charge");
            started = "shoot";
            ended = "shoot";
        } else {
            if !idle && (self.flags.has(F78, 8) || self.target == HandsState::ChargeIdle) {
                self.via(web, 0x2201, sub, "charge_idle", "charge_shoot");
                self.register(0, sub, "charge_shoot", StateEvent::ShootStarted);
                self.register(1, sub, "charge_shoot", StateEvent::ShootEnded);
                return;
            }
            self.via(web, if idle { 0xfcb } else { 0x2205 }, sub, "idle", "shoot");
            started = "shoot";
            ended = "shoot";
        }
        self.register(0, sub, started, StateEvent::ShootStarted);
        self.register(1, sub, ended, end_event);
    }

    /// The BRINGDOWN branch (HS_IDLE 0x1099..0x109f; the looping and transitioning states make the same
    /// requests): the web goes to the new weapon's subWeb idle via its bring-up.
    fn bringdown(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, cur_sub: &str, from_idle: bool) {
        self.flags.and(F74, 0x3f);
        self.flags.0[F77] = (self.flags.0[F77] & 0xf7) | 4;
        self.release_trigger(arsenal.current);
        let item = self.pending_item;
        self.new_weapon = item;
        let new_sub = item.map(|i| arsenal.defs[i].hands.subweb.clone()).unwrap_or_else(|| cur_sub.to_string());
        let up: &'static str;
        if new_sub == cur_sub {
            self.via(web, 0x1099, &new_sub, "idle", "bringdown");
            up = "bringdown";
        } else if let Some(n) = self.new_weapon.filter(|&n| self.intro_bringup(arsenal, n) || self.intro_accent_bringup(arsenal, n)) {
            let accent = !self.intro_bringup(arsenal, n);
            self.scalars.bring_up_intro_anim_select = accent as i32 as f32;
            self.flags.or(F79, 1);
            self.via(web, if accent { 0x1092 } else { 0x108d }, &new_sub, "idle", "bringup_intro");
            up = "bringup_intro";
        } else {
            self.via(web, 0x1095, &new_sub, "idle", "bringup");
            up = "bringup";
        }
        self.register(1, &new_sub, up, StateEvent::ChangeWeaponEnded);
        if from_idle {
            self.register(3, cur_sub, "bringdown", StateEvent::ChangeWeaponBringdownEnded);
        }
        if let Some(n) = self.new_weapon {
            // 0x140d691e0: the weapon's intro is used up; forced-intro flag cleared.
            self.intro_played[n] = true;
            self.flags.and(F78, 0xbf);
        }
        self.flags.and(F78, 0x7f);
        if let Some((down, up_w)) = self.bring_rates_pending.take() {
            self.bring_rates(arsenal, web, Some(down), up_w);
        }
        self.clear_pending();
    }

    /// 0x140d69d20: intro bring-up (hands_NoIntroBringup 0, hands_forceIntroBringup 0).
    fn intro_bringup(&self, arsenal: &Arsenal, w: usize) -> bool {
        arsenal.defs[w].hands.has_intro_bringup && (self.flags.has(F78, 0x40) || (self.intro_bringups && !self.intro_played[w]))
    }

    /// 0x140d69ca0: intro "accent" bring-up (hands_forceIntroAccentBringup 0).
    fn intro_accent_bringup(&self, arsenal: &Arsenal, w: usize) -> bool {
        arsenal.defs[w].hands.has_intro_accent_bringup && self.flags.0[F78] > 0x7f
    }

    // ---- web events ----

    /// A node notification from the web: posts the registered idEventDef (one shot).
    pub fn node_event(&mut self, arsenal: &mut Arsenal, kind: u8, sub: &str, state: &str) {
        let k = kind as usize;
        if k >= self.slots.len() {
            return;
        }
        if !self.slots[k].as_ref().is_some_and(|s| s.sub == sub && s.state == state) {
            return;
        }
        match self.slots[k].take().map(|s| s.event) {
            Some(StateEvent::ShootStarted) => {
                self.flags.or(F75, 3);
                self.flags.and(F77, 0xfe);
            }
            Some(StateEvent::ShootEnded) => {
                self.flags.and(F75, 0x8c);
                self.flags.or(F74, 0x40);
                self.switch_on_empty(arsenal);
            }
            Some(StateEvent::ShootEndedReloadStarted) => {
                self.flags.and(F75, 0xec);
                self.flags.or(F74, 0x12);
            }
            Some(StateEvent::DryfireEnded) => self.switch_on_empty(arsenal),
            Some(StateEvent::ChangeWeaponBringdownEnded) => self.flags.or(F77, 8),
            Some(StateEvent::BlendToChargeEnded) => self.flags.and(F78, 0xef),
            Some(StateEvent::BlendFromChargeEnded) => self.flags.and(F78, 0xdf),
            Some(StateEvent::MeleeEnded) => self.melee_ended(),
            Some(StateEvent::MeleeToShoot) => {
                // 0x140d5dbe0: MeleeEnded on leaving the shoot state, interruptible, FIRE requested.
                let sub = sub.to_string();
                self.register(1, &sub, "shootstate", StateEvent::MeleeEnded);
                self.flags.or(F74, 0x40);
                self.pending = HandsAction::Fire;
                self.pending_since = self.now;
            }
            Some(StateEvent::ChangeWeaponEnded) => {
                if self.flags.has(F77, 8) {
                    self.flags.and(F77, 0xfb);
                    self.flags.and(F79, 0xfe);
                }
            }
            None => {}
        }
    }

    /// idHands::AnimEvent_* (dispatcher 0x1414a7f10) for the events that matter to firing and switching.
    pub fn anim_event(&mut self, arsenal: &mut Arsenal, web: &mut dyn HandsWeb, name: &str, int: Option<i32>, inp: &HandsInput) -> Vec<WeaponEvent> {
        match name {
            // 0x140d54f90 -> FireWeapon(slot 2, deferred 0); 0x140d551f0 / 0x140d55310 only while the owner's
            // weapon is in fire mode 0 / 1 (weapon+0x8d4; the chaingun turret's shoot anims use SecondaryOnly).
            "ae_fireWeaponRight" => return self.fire_weapon(arsenal, false, inp),
            "ae_fireWeaponRightPrimaryOnly" | "ae_fireWeaponRightSecondaryOnly" => {
                let mode = if name == "ae_fireWeaponRightPrimaryOnly" { 0 } else { 1 };
                if arsenal.mstate[arsenal.current].fire_mode == mode {
                    return self.fire_weapon(arsenal, false, inp);
                }
                return Vec::new();
            }
            // 0x140d53e90 -> FireWeapon(2, 0, dry 1).
            "ae_dryfireWeaponRight" => return self.fire_weapon(arsenal, true, inp),
            // 0x140d55ed0.
            "ae_handsWeaponFireFinished" => {
                self.flags.and(F75, 0x8c);
                self.flags.or(F74, 0x40);
            }
            // 0x140d57040.
            "ae_setInterruptible" => {
                self.flags.and(F74, 0xbf);
                if int.unwrap_or(0) != 0 {
                    self.flags.or(F74, 0x40);
                }
            }
            // 0x140d56760.
            "ae_releaseWeaponTrigger" => self.release_trigger(arsenal.current),
            // 0x140d54d00: equip the weapon being brought up (its subWeb must be the current one).
            "ae_equipNextWeaponRight" => {
                if let Some(n) = self.new_weapon {
                    if web.current().is_some_and(|(s, _)| s == arsenal.defs[n].hands.subweb) {
                        self.set_weapon(arsenal, web, n);
                        self.queued_item = None;
                    }
                    self.new_weapon = None;
                }
            }
            // 0x140d59560 -> 0x140d5a6b0.
            "ae_weaponSwitchOnEmpty" => self.switch_on_empty(arsenal),
            // 0x140d56140 / 0x140d560b0.
            "ae_hideInhibitOn" => self.flags.or(F77, 2),
            "ae_hideInhibitOff" => self.flags.and(F77, 0xfd),
            // 0x140d59810 / 0x140d59780: zoom inhibit (0x10377 bit 1, checked by 0x140d634c0).
            "ae_zoomInhibitOn" => self.flags.or(F77, 1),
            "ae_zoomInhibitOff" => self.flags.and(F77, 0xfe),
            // 0x140d55770 (slot, joint): every SP gun sends "right_hand" "melee_impact"; that pair is used
            // when the runtime gives no strings.
            "ae_handsStartJointMeleeTrace" => {
                let joint = self.event_strings.get(1).cloned().unwrap_or_else(|| "melee_impact".to_string());
                self.melee_trace_start(arsenal, &joint);
            }
            // 0x140d559f0 (anim, slot, tag): the fists' slash trace follows a tag of the slot item's model;
            // INFERRED: the tag is followed as a hands joint of the same name.
            "ae_handsStartMeleeTraceSlash" => {
                if let Some(tag) = self.event_strings.get(1).cloned() {
                    self.melee_trace_start(arsenal, &tag);
                }
            }
            // 0x140d55610 -> 0x140d5d710.
            "ae_handsEndMeleeTrace" => self.melee.stop(),
            // 0x140d57f30: 0x10376 |= 0x20 (the player lunge is not ported).
            "ae_startMeleeLunge" => self.flags.or(F76, 0x20),
            // 0x140d562e0: hands+0x102cc (0) <= last melee start -> 0x10374 |= 0x80.
            "ae_interruptTransitionForMelee" => {
                if 0 <= self.melee_time {
                    self.flags.or(F74, 0x80);
                }
            }
            _ => {}
        }
        std::mem::take(&mut self.out)
    }

    /// 0x140d55770 -> 0x140d82260: the projectile is meleeSlashProjectile, else the sprint variant for a
    /// sprint melee (0x10378 & 2), else meleeProjectile (directional / one-hit-kill variants need their
    /// player-side triggers, not ported).
    fn melee_trace_start(&mut self, arsenal: &Arsenal, joint: &str) {
        let w = arsenal.current;
        let md = &arsenal.defs[w].melee;
        let proj = md
            .slash_projectile
            .as_ref()
            .or(if self.flags.has(F78, 2) && md.has_sprint_melee { md.sprint_projectile.as_ref() } else { None })
            .or(md.projectile.as_ref());
        self.flags.and(F78, 0xfd);
        // Without melee projectiles: the weapon's own (its ammo's projectileDecl, e.g. the fists).
        let own;
        let proj = match proj {
            Some(p) => Some(p),
            None if !arsenal.defs[w].projectile.name.is_empty() => {
                own = super::melee::MeleeProjectile {
                    def: arsenal.defs[w].projectile.clone(),
                    bounds: super::melee::MeleeBounds::None,
                    damage_type: super::melee::MeleeDamageType::None,
                    damage_cap: -1.0,
                };
                Some(&own)
            }
            None => None,
        };
        if let Some(p) = proj {
            self.melee.start(w, md, p, joint);
        }
        self.flags.and(F78, 0xfb);
    }

    /// 0x140d5a6b0: with g_forceWeaponSwitchOnDryFire 0 (default) it never switches; the auto switch on
    /// empty (g_weaponAutoSwitchOnEmpty) lives on the player side and is not ported.
    fn switch_on_empty(&mut self, _arsenal: &Arsenal) {}

    /// idHands::FireWeapon 0x140d5e1d0 (slot 2, not deferred). No CanFire / nextFireTime check: the gate
    /// was idWeapon::CanFire when the FIRE action was requested.
    fn fire_weapon(&mut self, arsenal: &mut Arsenal, dry: bool, inp: &HandsInput) -> Vec<WeaponEvent> {
        let w = arsenal.current;
        let def = arsenal.cur_def();
        let h = &def.hands;
        let mut ev = Vec::new();
        if h.has_looping_shoot_state && (self.pending.is_melee() || self.flags.0[F76] & 0x28 != 0) {
            self.flags.or(F75, 0x80);
            return ev;
        }
        let zoomed = inp.weapon.spread.zoomed;
        let failed = dry || (h.can_only_fire_when_zoomed && !zoomed) || HANDS_FORCE_DRYFIRE || arsenal.is_empty(w);
        if failed {
            self.flags.or(F75, 0x80);
        } else if h.has_looping_shoot_state && !self.trigger_pulled && self.flags.has(F75, 0x10) {
            self.flags.or(F75, 0x80);
        } else if h.has_looping_shoot_state && self.flags.has(F74, 2) && !self.flags.has(F74, 8) {
            self.flags.or(F75, 0x80);
        } else {
            let shot = arsenal.fire_weapon(&inp.weapon);
            // INTERIM press-type secondary trigger: back to mode 0 after its shot.
            if shot.mode == 1 && arsenal.mstate[w].press_shot {
                arsenal.mstate[w].press_shot = false;
                arsenal.set_fire_mode(w, 0);
                self.mode_changed = true;
            }
            ev.push(WeaponEvent::Fired(shot));
            // FinishFire 0x140f0d5c0: triggerState[mode] == RELEASED -> StopFireSound 0x140f23a80.
            if !self.trigger_pulled {
                ev.push(WeaponEvent::StopFireSound { weapon: w });
            }
            self.flags.0[F75] = (self.flags.0[F75] & 0xdf) | 0x10;
        }
        if def.single_tap || (zoomed && def.single_tap_ads) || (!HANDS_AUTO_DRYFIRE && arsenal.is_empty(w) && !h.has_looping_dryfire_state) {
            self.latch[arsenal.mstate[w].fire_mode] = true;
        }
        ev
    }

    // ---- weapon set-up and the rate scalars ----

    /// idHands::SetWeapon 0x140d5d7f0.
    fn set_weapon(&mut self, arsenal: &mut Arsenal, web: &dyn HandsWeb, w: usize) {
        self.latch = [false, false];
        arsenal.equip(w);
        self.trigger_pulled = false;
        self.shoot_anim_rate(arsenal, web);
        self.update_weapon_loaded(arsenal.is_empty(w), true, 0);
        self.shoot_delay_scale(arsenal, web);
        self.new_weapon = None;
        self.flags.and(F79, 0xdf);
        self.flags.0[F74] = (self.flags.0[F74] & 0xe9) | 8;
    }

    /// 0x140d62670: shootAnimRate so that the shoot anim spans the firing interval (looping:
    /// shotsPerLoopingShootAnim shots per loop; scaleShootAnimToFiringInterval: shotsPerShootAnim shots).
    /// `len = (numFrames - 1) / frameRate` of the weapon model's shoot alias; game ticks per second 960
    /// (timer vfunc +0x140), so a loop of N shots lasts N intervals of game time.
    fn shoot_anim_rate(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb) {
        let w = arsenal.current;
        let cd = arsenal.cur_def();
        let def = &*cd;
        let h = &def.hands;
        let interval = arsenal.firing_interval(w).max(1) as f32;
        let mode = arsenal.mstate[w].fire_mode;
        let charge_span = (arsenal.charge_time(w, mode) + arsenal.discharge_timeout(w, mode)) as f32;
        // chargeInfo.scaleShootAnimToMatchChargeTime (+0x29, chargeInfo of 0x140f11910) with ChargeTime +
        // DischargeTimeout != 0: the shoot alias spans that many ticks (numFrames / frameRate, no -1 here).
        if arsenal.charge_decl(w).charge.scale_shoot_anim_to_match_charge_time && charge_span != 0.0 {
            if let Some((frames, rate)) = web.alias_anim(&def.hands_md6, &h.shoot_anim_alias) {
                self.scalars.shoot_anim_rate = (frames as f32 / rate as f32 * TICKS_PER_SEC) / charge_span;
            }
            self.shoot_charge_anim_rate(arsenal, web, charge_span);
            return;
        }
        let shots = if h.scale_shoot_anim_to_firing_interval {
            (0 < h.shots_per_shoot_anim).then_some(h.shots_per_shoot_anim)
        } else if h.has_looping_shoot_state {
            Some(h.shots_per_looping_shoot_anim)
        } else {
            self.scalars.shoot_anim_rate = 1.0;
            None
        };
        if let (Some(n), Some((frames, rate))) = (shots, web.alias_anim(&def.hands_md6, &h.shoot_anim_alias)) {
            let len = (frames - 1) as f32 / rate as f32;
            self.scalars.shoot_anim_rate = (len * TICKS_PER_SEC) / (n as f32 * interval);
        }
        self.shoot_charge_anim_rate(arsenal, web, charge_span);
    }

    /// The shootChargeAnimRate tail of 0x140d62670: a pre-shoot charge (hasPreShootCharge with
    /// preShootChargeDurationMS > 0) is not on the SP guns; with hasChargeState the hands-web states charge_shoot
    /// (hands+0xb8) plus chargeShootToChargeIdleState's charge_shoot_to_charge_idle (hands+0xc8) span ChargeTime +
    /// DischargeTimeout ticks: rate = (frames / frameRate) * 960 / span; else 1. INTERIM: the hands-web state
    /// length (0x141706de0 out+4 / out+8) is taken at 30 frames/s (HandsWeb has no state frame rate).
    fn shoot_charge_anim_rate(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb, span: f32) {
        let d = arsenal.cur_def();
        self.scalars.shoot_charge_anim_rate = 1.0;
        if !d.has_charge_state {
            return;
        }
        let Some(mut frames) = web.state_anim_frames(&d.hands.subweb, "charge_shoot") else {
            return;
        };
        if !d.charge_shoot_to_charge_idle_state.is_empty() {
            frames += web.state_anim_frames(&d.hands.subweb, "charge_shoot_to_charge_idle").unwrap_or(0);
        }
        self.scalars.shoot_charge_anim_rate = (frames as f32 / 30.0 * TICKS_PER_SEC) / span;
    }

    /// 0x140d66230: shootDelayScale so that shoot + shoot_delay spans shotsPerShootAnim firing intervals.
    /// Frame counts are 30 Hz frames: the weapon model's alias if it has one, else the hands state's anim.
    fn shoot_delay_scale(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb) {
        let w = arsenal.current;
        let cd = arsenal.cur_def();
        let def = &*cd;
        let h = &def.hands;
        if h.has_looping_shoot_state {
            self.scalars.shoot_delay_scale = 1.0;
            return;
        }
        let frames = |state: &str| web.state_anim_frames(&h.subweb, state).map(|f| web.alias_anim(&def.hands_md6, state).map(|a| a.0).unwrap_or(f));
        let (Some(shoot), Some(delay)) = (frames(&h.shoot_anim_alias), frames("shoot_delay")) else {
            self.scalars.shoot_delay_scale = 1.0;
            return;
        };
        // Timer vfunc +0x170 0x1403627f0: ms * (1 / ticks per second).
        let interval_s = arsenal.firing_interval(w) as f32 * SEC_PER_TICK;
        let shoot_s = shoot as f32 / 30.0;
        let delay_s = delay as f32 / 30.0;
        let mut rem = h.shots_per_shoot_anim as f32 * interval_s - shoot_s;
        if rem < FLT_MIN {
            rem = 0.0;
        }
        let s = delay_s / rem.max(0.0001);
        self.scalars.shoot_delay_scale = if 0.0 < s { s } else { 1.0 };
    }

    /// 0x140d69550 (hands_weaponChangeTimingScheme 0), called by SelectWeapon with (current, new):
    /// rate = (numFrames / 30) / desiredBring{down,up}DurationSecs, only written when positive.
    fn bring_rates(&mut self, arsenal: &Arsenal, web: &dyn HandsWeb, down: Option<usize>, up: Option<usize>) {
        let rate = |w: usize, state: &str, secs: f32| {
            let frames = web.state_anim_frames(&arsenal.defs[w].hands.subweb, state).unwrap_or(0);
            if 0.0 < secs {
                (frames as f32 / 30.0) / secs
            } else {
                1.0
            }
        };
        if let Some(d) = down {
            let r = rate(d, "bringdown", arsenal.defs[d].bringdown_s);
            if r != self.scalars.bring_down_anim_rate && 0.0 < r {
                self.scalars.bring_down_anim_rate = r;
            }
        }
        if let Some(u) = up {
            let r = rate(u, "bringup", arsenal.defs[u].bringup_s);
            if r != self.scalars.bring_up_anim_rate && 0.0 < r {
                self.scalars.bring_up_anim_rate = r;
            }
        }
    }
}

/// 0x140d5b0c0 -> 0x140f049d0 for the SP guns: another shot's ammo remains after this one (the super
/// shotgun's shoot-to-reload choice; weaponCanReloadAfterFiringSelect = !this).
/// A decl-named hands state as the &'static str the request log keeps (known mod states, else leaked once).
fn static_state(s: &str) -> &'static str {
    match s {
        "shoot_burst" => "shoot_burst",
        "shoot_burst_again" => "shoot_burst_again",
        "shoot_grenade" => "shoot_grenade",
        "shoot_single" => "shoot_single",
        o => Box::leak(o.to_string().into_boxed_str()),
    }
}

fn can_reload_after_firing(arsenal: &Arsenal) -> bool {
    let w = arsenal.current;
    let per = arsenal.cur_def().ammo_per_shot;
    if per == 0 {
        return false;
    }
    match arsenal.ammo_for(w) {
        Some(n) => per * 2 <= n,
        None => true,
    }
}

/// Weapon vfunc +0x570 0x140f20f60: a random anim index in 0..4 other than `a` and `b`
/// (one game-RNG draw: list[rand15 % len]).
fn shoot_again_select(arsenal: &mut Arsenal, a: i32, b: i32) -> i32 {
    let list: Vec<i32> = (0..4).filter(|&i| i != a && i != b).collect();
    if list.is_empty() {
        return 0;
    }
    let r = arsenal.rng.next15() as i32;
    list[(r % list.len() as i32) as usize]
}
