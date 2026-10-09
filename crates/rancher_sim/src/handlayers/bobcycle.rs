//! idHandsBobCycle (idHands +0x3d78): the animated, additive bob cycle web `player/fp_hands_bob_cycle`, from the
//! user's own DOOMx64.exe (build 13954591). Notes: gamedata/re/HANDSLAYERS.md section 7 and ANIMWEB.md section 5.
//!
//! The web is merged onto the hands pose as BOP_ADD_RIGHT (ANIMWEB.md 3c) with the alpha this controller computes;
//! it only runs for weapons whose `weaponBob.enable` is off (in the campaign: the fists). Per frame, as the engine:
//! 1. [`HandsBobCycle::update`] (Update 0x140d83e80, before the anim stack): inputs (0x140d83120), bob type
//!    (0x140d82c70), state requests, the direction / alpha / attack springs and the web scalars;
//! 2. the caller steps the [`AnimWebRuntime`];
//! 3. [`HandsBobCycle::web_events`]: the web's node events fire the one-shot callbacks this controller registered
//!    (LeftCycleEnded / RightCycleEnded / TransitionFromSprintEnded), and the bob anims' ae_rightFoot /
//!    ae_leftFoot / ae_legsCrossing events become footsteps (0x140d83c90).

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decl::Block;
use idres::decldb::DeclDb;

use crate::animweb::{AnimWebRuntime, FiredEvent};
use crate::player::Spring;

/// bobType_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BobType {
    #[default]
    Idle = 0,
    Run = 1,
    Sprint = 2,
    AutoSprint = 3,
    Crouch = 4,
    Zoom = 5,
    Pda = 6,
}

/// idHandsBobCycleSingleCycleData_t (0x88); ctor 0x1406f6340 defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct CycleData {
    pub enable: bool,
    pub cycles_per_sec: f32,
    pub cycles_per_sec_min: f32,
    pub target_alpha: f32,
    pub alpha_rate: f32,
    pub attack_bias: f32,
    pub state_right: String,
    pub state_left: String,
    /// Set after load (0x140d839e0): the longest anim of state_right, numFrames / frameRate.
    pub state_secs: f32,
    /// Set after load: 0 idle, run speed (run, zoom), sprint speed, crouch speed.
    pub max_player_speed: f32,
    /// The rate the controller computes each frame (0x140d829b0).
    pub anim_rate: f32,
    pub bob_type: BobType,
}

impl CycleData {
    fn new(right: &str, left: &str) -> Self {
        Self {
            enable: true,
            cycles_per_sec: 1.0,
            cycles_per_sec_min: 0.1,
            target_alpha: 0.5,
            alpha_rate: 15.0,
            attack_bias: 0.0,
            state_right: right.to_string(),
            state_left: left.to_string(),
            state_secs: 0.0,
            max_player_speed: 0.0,
            anim_rate: 1.0,
            bob_type: BobType::Idle,
        }
    }

    fn read(&mut self, b: Option<&Block>) {
        let Some(b) = b else { return };
        if let Some(v) = b.path("enable").and_then(|v| v.as_bool()) {
            self.enable = v;
        }
        let f = |k: &str, d: f32| b.f32(k).unwrap_or(d);
        self.cycles_per_sec = f("cyclesPerSec", self.cycles_per_sec);
        self.cycles_per_sec_min = f("cyclesPerSecMin", self.cycles_per_sec_min);
        self.target_alpha = f("targetAlpha", self.target_alpha);
        self.alpha_rate = f("alphaRate", self.alpha_rate);
        self.attack_bias = f("attackBias", self.attack_bias);
        if let Some(s) = b.str("state_right") {
            self.state_right = s.to_string();
        }
        if let Some(s) = b.str("state_left") {
            self.state_left = s.to_string();
        }
        self.anim_rate = f("animRate", self.anim_rate);
    }
}

/// idDeclHandsBobCycle (`handsbobcycle/...`); ctor 0x1406f4250.
#[derive(Debug, Clone, PartialEq)]
pub struct HandsBobCycleDecl {
    pub subweb: String,
    /// idle, run, sprint, autoSprint, crouch, zoom, pda (bobType order).
    pub cycles: [CycleData; 7],
    pub scale_alpha_with_speed: bool,
    pub interrupt_sprint_to_change_weapons: bool,
}

impl Default for HandsBobCycleDecl {
    fn default() -> Self {
        Self {
            subweb: String::new(),
            cycles: [
                CycleData::new("idle", "idle"),
                CycleData::new("run_r", "run_l"),
                CycleData::new("sprint_r", "sprint_l"),
                CycleData::new("auto_sprint_r", "auto_sprint_l"),
                CycleData::new("crouch_r", "crouch_l"),
                CycleData::new("run_r", "run_l"),
                CycleData::new("", ""),
            ],
            scale_alpha_with_speed: true,
            interrupt_sprint_to_change_weapons: false,
        }
    }
}

/// Player move speeds the post-load step stores as each cycle's maxPlayerSpeed (idPlayer +0x55c74 / +0x55c7c /
/// +0x55c80).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerSpeeds {
    pub run: f32,
    pub sprint: f32,
    pub crouch: f32,
}

const CYCLE_KEYS: [&str; 7] = ["idleData", "runData", "sprintData", "autoSprintData", "crouchData", "zoomData", "pdaData"];

impl HandsBobCycleDecl {
    /// `name` as in the weapon decl's handsBobCycle, e.g. "player/fists".
    pub fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("handsbobcycle", name).with_context(|| format!("handsBobCycle decl {name}"))?;
        let e = b.block("edit").cloned().unwrap_or_default();
        Ok(Self::from_edit(&e))
    }

    pub fn from_edit(e: &Block) -> Self {
        let mut d = Self::default();
        if let Some(s) = e.str("subweb") {
            d.subweb = s.to_string();
        }
        for (c, k) in d.cycles.iter_mut().zip(CYCLE_KEYS) {
            c.read(e.block(k));
        }
        if let Some(v) = e.path("scaleAlphaWithSpeed").and_then(|v| v.as_bool()) {
            d.scale_alpha_with_speed = v;
        }
        if let Some(v) = e.path("interruptSprintToChangeWeapons").and_then(|v| v.as_bool()) {
            d.interrupt_sprint_to_change_weapons = v;
        }
        d
    }

    fn named(c: &CycleData) -> bool {
        c.enable && !c.state_right.is_empty() && !c.state_left.is_empty()
    }

    /// sprintData usable (0x140d0afc0).
    pub fn sprint_enabled(&self) -> bool {
        Self::named(&self.cycles[2])
    }
    /// autoSprintData usable (0x140d83520).
    pub fn auto_sprint_enabled(&self) -> bool {
        Self::named(&self.cycles[3])
    }
    /// crouchData usable (0x140d83590).
    pub fn crouch_enabled(&self) -> bool {
        Self::named(&self.cycles[4])
    }

    /// The cycle for a bob type (0x140d83020): autoSprint falls back to sprint, sprint to run.
    pub fn cycle_index(&self, t: BobType) -> usize {
        match t {
            BobType::Idle => 0,
            BobType::Run => 1,
            BobType::AutoSprint if self.auto_sprint_enabled() => 3,
            BobType::AutoSprint | BobType::Sprint if self.sprint_enabled() => 2,
            BobType::AutoSprint | BobType::Sprint => 1,
            BobType::Crouch => 4,
            BobType::Zoom => 5,
            BobType::Pda => 6,
        }
    }

    /// Post-load (0x140d83600 / 0x140d83800 -> 0x140d839e0): bob types, state durations and max speeds.
    pub fn init(&mut self, web: &AnimWebRuntime, speeds: &PlayerSpeeds) {
        let set = |d: &mut Self, i: usize, t: BobType, max: f32| {
            let secs = state_secs(web, &d.subweb, &d.cycles[i].state_right);
            let c = &mut d.cycles[i];
            c.enable = true;
            c.bob_type = t;
            c.max_player_speed = max;
            c.state_secs = secs;
        };
        set(self, 0, BobType::Idle, 0.0);
        if self.crouch_enabled() {
            set(self, 4, BobType::Crouch, speeds.crouch);
        }
        set(self, 1, BobType::Run, speeds.run);
        if self.sprint_enabled() {
            set(self, 2, BobType::Sprint, speeds.sprint);
        }
        if self.auto_sprint_enabled() {
            set(self, 3, BobType::AutoSprint, speeds.sprint);
        }
        let z = &self.cycles[5];
        if z.enable && !z.state_right.is_empty() && !z.state_left.is_empty() {
            set(self, 5, BobType::Zoom, speeds.run);
        }
    }
}

/// GetAnimDurationSecs (0x140ccb130 -> 0x141706ca0 -> 0x1417069c0): the node's longest model-0 alias,
/// numFrames / frameRate (chosen by ticks * numFrames / frameRate).
pub fn state_secs(web: &AnimWebRuntime, sub: &str, state: &str) -> f32 {
    let Some(sw) = web.web.sub_webs.iter().find(|s| s.name == sub) else { return 0.0 };
    let Some(node) = sw.nodes.iter().find(|n| n.state == state) else { return 0.0 };
    let mut best: Option<(i64, u32, u32)> = None;
    for t in node.trees.iter().filter(|t| t.model_index == 0) {
        for a in &t.anims {
            let Some(m) = web.data.meta.get(&a.name.to_ascii_lowercase()) else { continue };
            let fr = if m.frame_rate == 0 { 30 } else { m.frame_rate };
            let key = (crate::animweb::TICKS as i64 * m.num_frames as i64) / fr as i64;
            if best.is_none_or(|b| key > b.0) {
                best = Some((key, m.num_frames, fr));
            }
        }
    }
    best.map(|(_, n, fr)| n as f32 / fr as f32).unwrap_or(0.0)
}

/// idSpring<idVec2> (update 0x140d0b660): the same damped spring on a 2D offset (direction from the vector).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring2 {
    pub target: [f32; 2],
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub max_speed: f32,
    pub k: f32,
    pub damping: f32,
    pub mass: f32,
    pub rest: f32,
}

const TINY: f32 = 1.175_494_4e-38;

fn inv_sqrt(x: f32) -> f32 {
    let m = if x > TINY { x } else { TINY };
    let y0 = 1.0 / m.sqrt();
    let y1 = (m * y0 * y0 - 3.0) * y0 * -0.5;
    (m * y1 * y1 - 3.0) * y1 * -0.5
}

impl Spring2 {
    pub fn new(k: f32) -> Self {
        // ctor (0x140d83600): K 1, damping 2, mass 1, then SetK(k, -1) = critical damping.
        let k = k.min(10000.0);
        Self { target: [0.0; 2], pos: [0.0; 2], vel: [0.0; 2], max_speed: 0.0, k, damping: 2.0 * k.sqrt(), mass: 1.0, rest: 0.0 }
    }

    pub fn update(&mut self, mut dt: f32) {
        while 0.0 < dt {
            let h;
            if dt <= 0.0085 {
                h = dt;
                dt = 0.0;
            } else {
                h = 0.0085;
                dt -= 0.0085;
            }
            let dy = self.pos[1] - self.target[1];
            let dx = self.pos[0] - self.target[0];
            let l2 = dy * dy + dx * dx;
            let inv = inv_sqrt(l2);
            let f = -((inv * l2 - self.rest) * self.k);
            let inv_m = 1.0 / self.mass;
            self.vel[1] = (dy * inv * f - self.damping * self.vel[1]) * inv_m * h + self.vel[1];
            self.vel[0] = (dx * inv * f - self.damping * self.vel[0]) * inv_m * h + self.vel[0];
            let (vx, vy) = (self.vel[0], self.vel[1]);
            let s2 = vx * vx + vy * vy;
            let iv = inv_sqrt(s2);
            let speed = iv * s2;
            let s = if 0.0 < self.max_speed && self.max_speed < speed { self.max_speed } else { speed };
            self.vel = [s * vx * iv, s * vy * iv];
            for v in &mut self.vel {
                if v.abs() <= TINY {
                    *v = 0.0;
                }
            }
            self.pos = [h * self.vel[0] + self.pos[0], h * self.vel[1] + self.pos[1]];
            for p in &mut self.pos {
                if p.abs() <= TINY {
                    *p = 0.0;
                }
            }
        }
    }
}

fn spring1(k: f32) -> Spring {
    let mut s = Spring::default();
    s.set_k(k, -1.0);
    s
}

/// handsBobCycle_* cvars (exe defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BobCycleCvars {
    pub enable: bool,
    pub pm_no_bob: bool,
    pub pm_doom4_bob_cycle: bool,
    pub direction_spring_k: f32,
    pub blend_alpha_spring_k: f32,
    pub attack_spring_k: f32,
    pub relax_spring_k: f32,
    pub attack_hold_time_ms: i32,
    pub can_attack_while_sprinting: bool,
    pub idle_cmd_time_threshold: i32,
    pub jump_idle_delay_ms: i32,
    pub double_jump_idle_delay_ms: i32,
    pub fall_idle_delay_ms: i32,
    pub foot_step_event_timeout_ms: i32,
    pub pm_sprint_strafe_and_weapon_fire: bool,
}

impl Default for BobCycleCvars {
    fn default() -> Self {
        Self {
            enable: true,
            pm_no_bob: false,
            pm_doom4_bob_cycle: true,
            direction_spring_k: 100.0,
            blend_alpha_spring_k: 100.0,
            attack_spring_k: 2000.0,
            relax_spring_k: 300.0,
            attack_hold_time_ms: 300,
            can_attack_while_sprinting: false,
            idle_cmd_time_threshold: 150,
            jump_idle_delay_ms: 750,
            double_jump_idle_delay_ms: 900,
            fall_idle_delay_ms: 400,
            foot_step_event_timeout_ms: 5,
            pm_sprint_strafe_and_weapon_fire: false,
        }
    }
}

impl BobCycleCvars {
    pub fn from_cvars(cv: &crate::config::CvarValues) -> Self {
        let d = Self::default();
        let g = |n: &str, def: f32| cv.0.get(n).and_then(|v| v.trim_end_matches('f').parse().ok()).unwrap_or(def);
        let b = |n: &str, def: bool| g(n, def as i32 as f32) != 0.0;
        Self {
            enable: b("handsBobCycle_Enable", d.enable),
            pm_no_bob: b("pm_noBob", d.pm_no_bob),
            pm_doom4_bob_cycle: b("pm_doom4BobCycle", d.pm_doom4_bob_cycle),
            direction_spring_k: g("handsBobCycle_DirectionSpringK", d.direction_spring_k),
            blend_alpha_spring_k: g("handsBobCycle_BlendAlphaSpringK", d.blend_alpha_spring_k),
            attack_spring_k: g("handsBobCycle_AttackSpringK", d.attack_spring_k),
            relax_spring_k: g("handsBobCycle_RelaxSpringK", d.relax_spring_k),
            attack_hold_time_ms: g("handsBobCycle_AttackHoldTimeMS", d.attack_hold_time_ms as f32) as i32,
            can_attack_while_sprinting: b("handsBobCycle_CanAttackWhileSprinting", d.can_attack_while_sprinting),
            idle_cmd_time_threshold: g("handsBobCycle_IdleCmdTimeThreshold", d.idle_cmd_time_threshold as f32) as i32,
            jump_idle_delay_ms: g("handsBobCycle_JumpIdleDelayMS", d.jump_idle_delay_ms as f32) as i32,
            double_jump_idle_delay_ms: g("handsBobCycle_DoubleJumpIdleDelayMS", d.double_jump_idle_delay_ms as f32) as i32,
            fall_idle_delay_ms: g("handsBobCycle_FallIdleDelayMS", d.fall_idle_delay_ms as f32) as i32,
            foot_step_event_timeout_ms: g("handsBobCycle_FootStepEventTimeoutMS", d.foot_step_event_timeout_ms as f32) as i32,
            pm_sprint_strafe_and_weapon_fire: b("pm_sprintStrafeAndWeaponFire", d.pm_sprint_strafe_and_weapon_fire),
        }
    }
}

/// Per-frame inputs (0x140d83120 / 0x140d82c70). Flags the campaign rarely sets default to false.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BobCycleInput {
    /// usercmd forwardmove / rightmove (cmd +0xc / +0xd).
    pub forward_move: i8,
    pub right_move: i8,
    /// Physics linear velocity minus the push velocities (game units/s): `physics.velocity`.
    pub velocity: Vec3,
    /// View yaw, degrees (idPlayerController vslot 37).
    pub view_yaw: f32,
    /// current.groundPlane (player +0x16990).
    pub on_ground: bool,
    /// The physics crouch test (0x1416b04c0).
    pub crouched: bool,
    /// movementFlags JUMPED / DOUBLE_JUMPED (0x1416b0470 / 0x1416b0430).
    pub jumped: bool,
    pub double_jumped: bool,
    /// idPlayer isSprinting (+0x27ca0) and the auto-sprint option.
    pub sprinting: bool,
    pub auto_sprint: bool,
    /// The player is zoomed (0x140e42290) with the weapon's zoomMode (+0xe30).
    pub zoomed: bool,
    pub zoom_mode: i32,
    /// Player flags 0xce46 bit 2 (forces idle unless the debug move cvars are set).
    pub force_idle: bool,
    /// Player flags 0x273f0 bit 0 (idle).
    pub idle_flag: bool,
    /// Player flags 0x273ee bit 2 (run; also "sprint melee" for the alpha suppression).
    pub run_flag: bool,
    /// idHands 0x140d634b0 (run).
    pub hands_force_run: bool,
    /// Player flags 0x273ef bit 2 && !(0x273ec bit 6): a weapon switch is under way (run with a weapon bob decl).
    pub weapon_switching: bool,
    /// Player +0x16f64 or the mechanic +0x3e010 vslot 4 (e.g. a ledge grab): idle.
    pub mechanic_active: bool,
    /// Sprinting with a weapon switch whose bob decls ask to interrupt sprint (interruptSprintToChangeWeapons).
    pub sprint_switch_interrupt: bool,
    /// The weapon's last fire time for its current mode (weapon +0x91c + mode*4), game ms.
    pub weapon_last_fire_time: Option<i32>,
    /// idPlayer sprintStats +4 (+0x27c90): cap on rate_sprint.
    pub sprint_rate_cap: f32,
    /// Game time (ms) and frame length.
    pub now: i32,
    pub msec: i32,
}

/// A footstep the bob cycle asks the player to play (idPlayer vslot 0xc08 via 0x14074cc40; `right` = foot arg 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BobCycleFootstep {
    Foot { right: bool },
    /// FOOTSTEP_LEGS_CROSSING: idPlayer vslot 0x2e8 with the player's (or move-state) decl (cloth sound).
    LegsCrossing,
}

/// Web event ids the controller registers (static event defs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum WebCallback {
    /// animWeb_LeftCycleEnded (0x1445987c0) -> leftCycleEnded = 1, sprintTransitionPlaying = 0 (0x140d82a50).
    LeftCycleEnded,
    /// animWeb_RightCycleEnded (0x144598820) -> rightCycleEnded = 1, sprintTransitionPlaying = 0 (0x140d82a70).
    RightCycleEnded,
    /// animWeb_TransitionFromSprintEnded (0x144598880) -> canPlayTracers = 1 (0x140d82a90).
    TransitionFromSprintEnded,
}

/// One idAnimWebEventHandler slot (one per web event type; one-shot, 0x1417297a0 / 0x141729c20).
#[derive(Debug, Clone, PartialEq)]
struct Slot {
    sub: String,
    state: String,
    event: WebCallback,
}

/// The controller state (idHandsBobCycle fields).
#[derive(Debug, Clone, PartialEq)]
pub struct HandsBobCycle {
    pub direction_spring: Spring2,
    pub blend_alpha_spring: Spring,
    pub attack_spring: Spring,
    pub relax_spring: Spring,
    pub idle_delay_time: i32,
    pub idle_force_time: i32,
    pub was_falling: bool,
    pub attacking: bool,
    pub can_play_tracers: bool,
    pub no_input_time: i32,
    /// 0 player default decl, 2 weapon decl (handsBobCycle_t).
    pub hands_bob_cycle: u8,
    pub bob_type: BobType,
    /// footStep_t: 0 none, 1 right, 2 left.
    pub foot_step_state: u8,
    pub foot_step_event: u8,
    /// footStepEventTime (+0x93c..): [unused, foot, foot-or-crossing, legs crossing].
    pub foot_step_event_time: [i32; 4],
    pub left_cycle_ended: bool,
    pub right_cycle_ended: bool,
    pub suppress_during_melee: bool,
    pub sprint_transition_playing: bool,
    pub sprint2idle_secs: f32,
    pub sprint2run_secs: f32,
    /// The merge branch alpha for the ADD_RIGHT merge (idAnimatorParms alpha 0.5 until the first update).
    pub alpha: f32,
    slots: [Option<Slot>; 11],
}

impl HandsBobCycle {
    /// Init (0x140d83600). The caller also puts the web in "generic"/"idle" (idAnimatorParms).
    pub fn new(cv: &BobCycleCvars) -> Self {
        let mut direction_spring = Spring2::new(cv.direction_spring_k);
        direction_spring.target = [0.0, 1.0];
        direction_spring.pos = [0.0, 1.0];
        Self {
            direction_spring,
            blend_alpha_spring: spring1(cv.blend_alpha_spring_k),
            attack_spring: spring1(cv.attack_spring_k),
            relax_spring: spring1(cv.relax_spring_k),
            idle_delay_time: 0,
            idle_force_time: 0,
            was_falling: false,
            attacking: false,
            can_play_tracers: true,
            no_input_time: 0,
            hands_bob_cycle: 0,
            bob_type: BobType::Idle,
            foot_step_state: 0,
            foot_step_event: 0,
            foot_step_event_time: [0; 4],
            left_cycle_ended: false,
            right_cycle_ended: false,
            suppress_during_melee: false,
            sprint_transition_playing: false,
            sprint2idle_secs: 0.0,
            sprint2run_secs: 0.0,
            alpha: 0.5,
            slots: Default::default(),
        }
    }

    /// sprint_to_idle / sprint_to_run durations (0x140d83a70): frames / 30 of those transition anims.
    pub fn set_sprint_transition_secs(&mut self, to_idle: f32, to_run: f32) {
        self.sprint2idle_secs = to_idle;
        self.sprint2run_secs = to_run;
    }

    /// FUN_14170b370 (via 0x140d83e00): registers a one-shot callback for web event `kind` (1 edge out of the node,
    /// 2 blend into the node done) on (sub, state). A slot of the same kind that is still pending first fires if
    /// the web currently is in its node (0x1417297a0), then is replaced.
    fn register(&mut self, web: &AnimWebRuntime, kind: usize, sub: &str, state: &str, event: WebCallback) {
        let old = self.slots[kind].take();
        if let Some(old) = old.filter(|o| web.current() == Some((o.sub.as_str(), o.state.as_str()))) {
            self.fire(old.event);
        }
        self.slots[kind] = Some(Slot { sub: sub.to_string(), state: state.to_string(), event });
    }

    fn fire(&mut self, e: WebCallback) {
        match e {
            WebCallback::LeftCycleEnded => {
                self.left_cycle_ended = true;
                self.sprint_transition_playing = false;
            }
            WebCallback::RightCycleEnded => {
                self.right_cycle_ended = true;
                self.sprint_transition_playing = false;
            }
            WebCallback::TransitionFromSprintEnded => self.can_play_tracers = true,
        }
    }

    /// Input bookkeeping of 0x140d83120: velocity in the view's yaw frame, no-input time, idle delays.
    fn inputs(&mut self, cv: &BobCycleCvars, inp: &BobCycleInput) -> Vec3 {
        let (sy, cy) = (inp.view_yaw.to_radians().sin(), inp.view_yaw.to_radians().cos());
        let v = inp.velocity;
        // idRotation(gravity normal, -yaw).RotatePoint: the velocity in the yaw frame (x forward, y left).
        let local = Vec3::new(v.x * cy + v.y * sy, v.y * cy - v.x * sy, 0.0);
        if inp.forward_move == 0 && inp.right_move == 0 {
            self.no_input_time += inp.msec;
        } else {
            self.no_input_time = 0;
        }
        if inp.jumped {
            self.idle_delay_time += cv.jump_idle_delay_ms;
            self.was_falling = true;
        } else if inp.double_jumped {
            self.idle_delay_time += cv.double_jump_idle_delay_ms;
            self.was_falling = true;
        } else if !self.was_falling && !inp.on_ground {
            self.idle_delay_time += cv.fall_idle_delay_ms;
            self.was_falling = true;
        } else if !(self.was_falling && inp.on_ground) {
            if self.idle_delay_time != 0 {
                self.idle_delay_time = (self.idle_delay_time - inp.msec).max(0);
            }
        } else {
            self.idle_delay_time = 0;
            self.was_falling = false;
        }
        if self.idle_force_time != 0 {
            self.idle_force_time = (self.idle_force_time - inp.msec).max(0);
        }
        local
    }

    /// GetDesiredHandsBobCycleAndBobType (0x140d82c70).
    fn desired_type(&self, cv: &BobCycleCvars, d: &HandsBobCycleDecl, inp: &BobCycleInput, weapon_decl: bool) -> (u8, BobType) {
        let cycle = if weapon_decl { 2 } else { 0 };
        let has_input = inp.forward_move != 0 || inp.right_move != 0;
        let keep = self.bob_type;
        let t = if inp.force_idle || self.idle_force_time != 0 {
            BobType::Idle
        } else if inp.zoomed && inp.zoom_mode == 1 && d.cycles[5].enable {
            BobType::Zoom
        } else if has_input || self.no_input_time < cv.idle_cmd_time_threshold || 0 < self.idle_delay_time {
            if (!inp.on_ground && self.idle_delay_time < 1) || inp.idle_flag {
                BobType::Idle
            } else if inp.run_flag || inp.hands_force_run || (inp.weapon_switching && cycle != 0) {
                BobType::Run
            } else if inp.mechanic_active {
                BobType::Idle
            } else if inp.crouched {
                BobType::Crouch
            } else if !has_input {
                keep
            } else if !inp.sprinting || inp.sprint_switch_interrupt || cv.pm_sprint_strafe_and_weapon_fire {
                BobType::Run
            } else if inp.auto_sprint {
                BobType::AutoSprint
            } else {
                BobType::Sprint
            }
        } else {
            BobType::Idle
        };
        (cycle, t)
    }

    /// One frame of idHandsBobCycle::Update (0x140d83e80). `weapon_bob_enabled` = the held weapon's
    /// weaponBob.enable (then the web's alpha is zeroed and nothing else runs); `weapon_decl` = the weapon names a
    /// handsBobCycle decl (`d`), else `d` is the player's default. Returns the merge alpha for the ADD_RIGHT merge.
    pub fn update(
        &mut self,
        web: &mut AnimWebRuntime,
        d: &mut HandsBobCycleDecl,
        cv: &BobCycleCvars,
        inp: &BobCycleInput,
        weapon_bob_enabled: bool,
        weapon_decl: bool,
    ) -> f32 {
        if !cv.enable || cv.pm_no_bob || !cv.pm_doom4_bob_cycle {
            return self.alpha;
        }
        if weapon_bob_enabled {
            self.alpha = 0.0;
            return 0.0;
        }
        let vel = self.inputs(cv, inp);
        let (cycle, t) = self.desired_type(cv, d, inp, weapon_decl);
        let cur_i = d.cycle_index(t);
        let prev_i = d.cycle_index(self.bob_type);
        let changed = self.hands_bob_cycle != cycle || d.cycles[prev_i].bob_type != d.cycles[cur_i].bob_type;
        self.foot_step_event = 0;
        let sub = d.subweb.clone();
        let (right, left) = (d.cycles[cur_i].state_right.clone(), d.cycles[cur_i].state_left.clone());
        let prev_type = self.bob_type;
        // 0x140d0ab90: 0 when the web sits in the idle state with no edge pending.
        let settled_idle = web.current().is_some_and(|(_, s)| s == d.cycles[0].state_right) && !web.has_pending();
        if !settled_idle {
            if t == BobType::Idle {
                if changed {
                    self.foot_step_state = 0;
                    web.request(Some(&sub), &right, None, 1, 1);
                    if prev_type == BobType::Sprint {
                        self.can_play_tracers = false;
                        self.register(web, 2, &sub, &right, WebCallback::TransitionFromSprintEnded);
                    }
                }
            } else if changed {
                self.left_cycle_ended = false;
                self.right_cycle_ended = false;
                if self.foot_step_state == 1 {
                    self.foot_step_state = 2;
                    web.request(Some(&sub), &right, Some((Some(&sub), &left)), 1, 0);
                    self.register(web, 1, &sub, &left, WebCallback::LeftCycleEnded);
                    if prev_type == BobType::Sprint {
                        self.can_play_tracers = false;
                        self.register(web, 2, &sub, &right, WebCallback::TransitionFromSprintEnded);
                    }
                } else {
                    self.foot_step_state = 1;
                    web.request(Some(&sub), &left, Some((Some(&sub), &right)), 1, 1);
                    self.register(web, 1, &sub, &right, WebCallback::RightCycleEnded);
                    if prev_type == BobType::Sprint {
                        self.can_play_tracers = false;
                        self.register(web, 2, &sub, &left, WebCallback::TransitionFromSprintEnded);
                    }
                }
            } else if self.right_cycle_ended {
                self.left_cycle_ended = false;
                self.right_cycle_ended = false;
                self.foot_step_state = 2;
                web.request(Some(&sub), &right, Some((Some(&sub), &left)), 1, 1);
                self.register(web, 1, &sub, &left, WebCallback::LeftCycleEnded);
            } else if self.left_cycle_ended {
                self.left_cycle_ended = false;
                self.right_cycle_ended = false;
                self.foot_step_state = 1;
                web.request(Some(&sub), &left, Some((Some(&sub), &right)), 1, 1);
                self.register(web, 1, &sub, &right, WebCallback::RightCycleEnded);
            }
            self.hands_bob_cycle = cycle;
            self.bob_type = t;
            if !changed && t == BobType::Sprint {
                self.can_play_tracers = false;
            }
        } else if changed {
            if t == BobType::Idle {
                self.foot_step_state = 0;
                web.request(Some(&sub), &right, None, 0, 0);
            } else {
                self.left_cycle_ended = false;
                self.right_cycle_ended = false;
                self.foot_step_state = 2;
                web.request(Some(&sub), &right, Some((Some(&sub), &left)), 0, 0);
                self.register(web, 1, &sub, &left, WebCallback::LeftCycleEnded);
                if t == BobType::Sprint {
                    self.can_play_tracers = false;
                }
            }
            self.hands_bob_cycle = cycle;
            self.bob_type = t;
        }
        if t == BobType::Zoom {
            self.bob_type = BobType::Zoom;
        }
        let dt = inp.msec as f32 * 0.001;
        self.attack_weights(web, d, cv, inp, dt);

        // Direction weights (forward/right of the yaw-frame velocity) through the direction spring.
        let (vx, vy, vz) = (vel.x, vel.y, vel.z);
        let sq = vy * vy + vx * vx + vz * vz;
        let speed = inv_sqrt(sq) * sq;
        let ry = -vy;
        let inv = inv_sqrt(ry * ry + vx * vx + vz * vz);
        self.direction_spring.target = [(inv * vx + 1.0) * 0.5, (ry * inv + 1.0) * 0.5];
        self.direction_spring.update(dt);
        let (wf, wr) = (self.direction_spring.pos[0].clamp(0.0, 1.0), self.direction_spring.pos[1].clamp(0.0, 1.0));
        web.set_scalar("weight_forward", wf);
        web.set_scalar("weight_right", wr);
        let m2 = wr * wr + wf * wf;
        web.set_scalar("weight_move", (inv_sqrt(m2) * m2).clamp(0.0, 1.0));

        // Alpha: speed over the cycle's max speed, through the blend alpha spring.
        let c = &d.cycles[cur_i];
        let mut rate = c.alpha_rate;
        let target = if c.max_player_speed == 0.0 { 0.0 } else { ((speed * c.target_alpha) / c.max_player_speed).clamp(0.0, 1.0) };
        self.blend_alpha_spring.target = target;
        self.blend_alpha_spring.update(dt);
        let mut a = self.blend_alpha_spring.pos.clamp(0.0, 1.0);
        if inp.run_flag && (self.bob_type == BobType::Sprint || self.sprint_transition_playing) {
            self.suppress_during_melee = true;
        }
        if self.suppress_during_melee {
            if !inp.run_flag {
                self.suppress_during_melee = false;
            } else {
                a = 0.0001;
                rate = 0.0;
            }
        }
        let _ = rate; // alphaRate is set with currentAlpha = targetAlpha, so the alpha advance is a no-op.
        self.alpha = a;

        // Cycle rates (0x140d829b0) and the rate scalars.
        for t in [BobType::Idle, BobType::Run, BobType::Sprint, BobType::Crouch, BobType::Zoom] {
            let i = d.cycle_index(t);
            let c = &mut d.cycles[i];
            c.anim_rate = if c.max_player_speed != 0.0 {
                let mut r = ((speed * c.cycles_per_sec) / c.max_player_speed) / (0.5 / c.state_secs);
                if r <= c.cycles_per_sec_min {
                    r = c.cycles_per_sec_min;
                }
                if !inp.on_ground && 0 < self.idle_delay_time {
                    r *= 0.25;
                }
                r
            } else {
                1.0
            };
        }
        match self.bob_type {
            BobType::Idle => web.set_scalar("rate_run", d.cycles[0].anim_rate),
            BobType::Crouch => web.set_scalar("rate_crouch", d.cycles[4].anim_rate),
            BobType::Zoom => web.set_scalar("rate_run", d.cycles[5].anim_rate),
            _ => web.set_scalar("rate_run", d.cycles[1].anim_rate),
        }
        let s = if d.sprint_enabled() { 2 } else { 1 };
        web.set_scalar("rate_sprint", inp.sprint_rate_cap.min(d.cycles[s].anim_rate));
        a
    }

    /// 0x140d84c40: attack / relax springs -> weight_shoot_*, and the sprint transition rates.
    fn attack_weights(&mut self, web: &mut AnimWebRuntime, d: &HandsBobCycleDecl, cv: &BobCycleCvars, inp: &BobCycleInput, dt: f32) {
        let bias = match self.bob_type {
            BobType::Run => d.cycles[1].attack_bias,
            BobType::AutoSprint if d.auto_sprint_enabled() => d.cycles[3].attack_bias,
            BobType::AutoSprint | BobType::Sprint => d.cycles[if d.sprint_enabled() { 2 } else { 1 }].attack_bias,
            BobType::Crouch => d.cycles[4].attack_bias,
            BobType::Zoom => d.cycles[5].attack_bias,
            _ => 0.0,
        };
        self.attacking = inp.weapon_last_fire_time.is_some_and(|t| inp.now < t + cv.attack_hold_time_ms);
        let (v, div) = if !self.attacking {
            self.relax_spring.target = (bias + 0.0).clamp(0.0, 1.0);
            self.relax_spring.update(dt);
            let v = self.relax_spring.pos;
            self.attack_spring.pos = v;
            self.attack_spring.target = v;
            (v, 0.5)
        } else {
            self.attack_spring.target = (bias + 1.0).clamp(0.0, 1.0);
            self.attack_spring.update(dt);
            let v = self.attack_spring.pos;
            self.relax_spring.pos = v;
            self.relax_spring.target = v;
            (v, 0.1)
        };
        web.set_scalar("rate_sprint2idle", self.sprint2idle_secs / div);
        web.set_scalar("rate_sprint2run", self.sprint2run_secs / div);
        let c = v.clamp(0.0, 1.0);
        web.set_scalar("weight_shoot_idle", c);
        web.set_scalar("weight_shoot_run", c);
        web.set_scalar("weight_shoot_sprint", if cv.can_attack_while_sprinting { c } else { 0.0 });
        web.set_scalar("weight_shoot_crouch", c);
    }

    /// After the web's update: fire the registered one-shot callbacks from its node events (kinds 1 / 2) and turn the
    /// bob anims' foot events into footsteps (0x140d83c90).
    pub fn web_events(&mut self, web: &AnimWebRuntime, fired: &[FiredEvent], cv: &BobCycleCvars, now: i32) -> Vec<BobCycleFootstep> {
        for (kind, sub, state) in web.node_events() {
            let k = kind as usize;
            if k >= self.slots.len() {
                continue;
            }
            if self.slots[k].as_ref().is_some_and(|s| s.sub == sub && s.state == state) {
                let s = self.slots[k].take().unwrap();
                self.fire(s.event);
            }
        }
        let mut out = Vec::new();
        for e in fired {
            let kind = match e.event.name.as_str() {
                "ae_rightFoot" => 1,
                "ae_leftFoot" => 2,
                "ae_legsCrossing" => 3,
                _ => continue,
            };
            let to = cv.foot_step_event_timeout_ms;
            let ev = if kind < 3 {
                if to < now - self.foot_step_event_time[2] && to < now - self.foot_step_event_time[1] {
                    self.foot_step_event_time[2] = now;
                    self.foot_step_event = kind;
                    self.foot_step_event_time[1] = now;
                    kind
                } else {
                    0
                }
            } else if to < now - self.foot_step_event_time[3] {
                self.foot_step_event_time[3] = now;
                self.foot_step_event = 3;
                3
            } else {
                0
            };
            match ev {
                1 => out.push(BobCycleFootstep::Foot { right: true }),
                2 => out.push(BobCycleFootstep::Foot { right: false }),
                3 => out.push(BobCycleFootstep::LegsCrossing),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring2_settles_on_target() {
        let mut s = Spring2::new(100.0);
        s.target = [1.0, 0.25];
        for _ in 0..200 {
            s.update(0.016);
        }
        assert!((s.pos[0] - 1.0).abs() < 1e-3 && (s.pos[1] - 0.25).abs() < 1e-3, "{:?}", s.pos);
    }

    /// The fists' bob cycle on the install's fp_hands_bob_cycle web: running forward alternates the half-cycles,
    /// raises the alpha toward run speed / max speed and emits alternating footsteps; standing still returns to idle.
    #[test]
    fn fists_run_cycle_on_install() {
        use std::sync::Arc;
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("install");
        let c = idres::Container::open(&doom.join("base"), "gameresources").unwrap();
        let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands_bob_cycle.decl").unwrap()).into_owned();
        let webdecl = Arc::new(idres::animweb::AnimWeb::parse(&text).unwrap());
        let data = Arc::new(crate::animweb::AnimData::load(&c, &webdecl, &["generic"]));
        let mut web = AnimWebRuntime::new(webdecl, data);
        web.hands_weights = false;
        assert!(web.set_state("generic", "idle"));
        let mut d = HandsBobCycleDecl::from_decl(&inst.decls, "player/fists").unwrap();
        let speeds = PlayerSpeeds { run: inst.movement.run_speed, sprint: inst.movement.run_speed, crouch: inst.movement.crouch_speed };
        d.init(&web, &speeds);
        assert_eq!(d.subweb, "generic");
        assert!(d.cycles[1].state_secs > 0.0, "run_r duration");
        let cv = BobCycleCvars::from_cvars(&inst.cvars);
        let mut bob = HandsBobCycle::new(&cv);
        let (mut now, mut states, mut feet, mut max_alpha) = (0, Vec::new(), Vec::new(), 0.0f32);
        let speed = inst.movement.run_speed;
        for i in 0..300 {
            let running = i < 200;
            let inp = BobCycleInput {
                forward_move: if running { 127 } else { 0 },
                velocity: if running { Vec3::new(speed, 0.0, 0.0) } else { Vec3::ZERO },
                on_ground: true,
                sprint_rate_cap: f32::MAX,
                now,
                msec: 16,
                ..Default::default()
            };
            let a = bob.update(&mut web, &mut d, &cv, &inp, false, true);
            max_alpha = max_alpha.max(a);
            now += 16;
            let fired = web.update(now);
            feet.extend(bob.web_events(&web, &fired, &cv, now));
            let s = web.current().map(|(_, s)| s.to_string()).unwrap_or_default();
            if states.last() != Some(&s) {
                states.push(s);
            }
        }
        assert!(states.iter().filter(|s| *s == "run_l").count() >= 2 && states.iter().filter(|s| *s == "run_r").count() >= 2, "{states:?}");
        assert_eq!(states.last().map(String::as_str), Some("idle"), "{states:?}");
        assert!((max_alpha - 1.0).abs() < 0.05, "alpha {max_alpha}");
        assert!(bob.alpha < 0.05, "alpha after stopping {}", bob.alpha);
        let rights: Vec<bool> = feet.iter().filter_map(|f| match f { BobCycleFootstep::Foot { right } => Some(*right), _ => None }).collect();
        assert!(rights.len() >= 4 && rights.windows(2).all(|w| w[0] != w[1]), "{feet:?}");
    }

    #[test]
    fn decl_defaults() {
        let src = r#"{ edit = { subweb = "generic"; runData = { cyclesPerSec = 1.2; targetAlpha = 1; } sprintData = { enable = false; } } }"#;
        let b = idres::decl::parse(src).unwrap();
        let d = HandsBobCycleDecl::from_edit(b.block("edit").unwrap());
        assert_eq!(d.subweb, "generic");
        assert_eq!(d.cycles[1].cycles_per_sec, 1.2);
        assert_eq!(d.cycles[1].target_alpha, 1.0);
        assert_eq!(d.cycles[1].state_right, "run_r");
        assert_eq!(d.cycles[4].target_alpha, 0.5);
        assert!(!d.sprint_enabled());
        assert_eq!(d.cycle_index(BobType::Sprint), 1);
        assert_eq!(d.cycles[5].state_left, "run_l");
    }
}
