//! The Imp (`ai/demon/imp`): its combat FSM, fireball throws and melee (gamedata/re/DEMONS.md section 13).
//!
//! FSM: aiFSMManager `states/impcombat`, layers config "imp" (common + imp_combat; behaviors/zion/imp/default
//! aiFSMManagerDeclLayersConfig). Before the player is CONFIRMED the Imp is relaxed; then `impcombat`:
//!   imp_default: #0 IMPC_UseHangout -> hangout, #1 COMBAT_ShouldSightEnemy -> impc_sighted, #2 DefaultUse -> imp_idle
//!   imp_idle: #3 Shared_SocialRelationAvailable (DESTROY_AT_ALL_COSTS..ANGER, CONFIRMED) -> imp_terminate ->
//!     DefaultUse -> imp_destroy_primary = child FSM `imp_engage_primary`, whose idle state imp_primary_idle checks
//!     (orderIndex order):
//!   #0 Shared_ShouldAttack(default) -> imp_primary_melee        #1 Shared_ShouldCharge -> charge (role CHARGE only)
//!   #2 position checks (0.5 s timer) -> reposition / advance    #3 Shared_ChargeMelee(256) -> imp_charge_melee_primary
//!   #4 reposition check, #5 IMPC_ShouldRage, #6 fastball         #7 Shared_CanThrowProjectile -> imp_throw_primary
//!   #8 Shared_EnemyInRangeAndAngle(-1..9999, 45) NO -> imp_facetarget_primary, #9 reposition for visibility.
//! Ported: #0, #1 (role), #2's advance branch, #3, #7, #8. INTERIM / not ported: hangout and reposition points (the
//! position-awareness component and AAS position queries), rage, the fastball sub FSM, taunts.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decl::{Block, Value};
use idres::decldb::DeclDb;

use super::perception::Awareness;
use super::{wrap180, yaw_to, AiEvent, AiOutput, Body, Brain, Locomotion, Target, World, TICKS_PER_SEC};
use crate::demons::projectile::AiProjectileDef;

/// The Imp.
pub const IMP: &str = "ai/demon/imp";
/// Its combat FSM decl.
pub const IMP_FSM: &str = "states/impcombat";

/// impcombat / imp_engage_primary states (impcombat.decl), plus the relaxed behaviour before combat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpState {
    /// FSM_relaxed: idle until the player is CONFIRMED.
    Relaxed,
    /// imp_default [idImpCombat_Default].
    Default,
    /// impc_sighted [idCombat_SightedEnemy].
    Sighted,
    /// imp_idle [idOpenCombat_AttackIdle].
    Idle,
    /// imp_engage_primary: imp_primary_idle [idOpenCombat_AttackIdle].
    PrimaryIdle,
    /// imp_primary_melee [idShared_Attack].
    Melee,
    /// imp_charge_melee_primary [idShared_ChargeMelee].
    ChargeMelee,
    /// imp_throw_primary [idShared_ThrowProjectile].
    Throw,
    /// imp_facetarget_primary [idShared_FaceEnemy].
    FaceTarget,
    /// imp_advance_primary [idShared_MoveTowardEnemy, WALKSTATE_RUNNING].
    Advance,
}

impl ImpState {
    pub fn decl_name(self) -> &'static str {
        match self {
            ImpState::Relaxed => "behaviors_relaxed_shared",
            ImpState::Default => "imp_default",
            ImpState::Sighted => "impc_sighted",
            ImpState::Idle => "imp_idle",
            ImpState::PrimaryIdle => "imp_primary_idle",
            ImpState::Melee => "imp_primary_melee",
            ImpState::ChargeMelee => "imp_charge_melee_primary",
            ImpState::Throw => "imp_throw_primary",
            ImpState::FaceTarget => "imp_facetarget_primary",
            ImpState::Advance => "imp_advance_primary",
        }
    }
}

/// The FSM link `start` -> `end` of an aiFSMManager decl whose transition class is `class` (any when None): its
/// transitionType object (the class parameters).
pub fn fsm_link<'a>(b: &'a Block, start: &str, end: &str, class: Option<&str>) -> Option<&'a Block> {
    for (k, v) in &b.items {
        let Value::Block(c) = v else { continue };
        if k == "link" && c.str("startNode") == Some(start) && c.str("endNode") == Some(end) {
            let t = c.block("object.object.transitionType");
            if class.is_none() || t.and_then(|t| t.str("className")) == class {
                return t.and_then(|t| t.block("object"));
            }
            continue;
        }
        if let Some(f) = fsm_link(c, start, end, class) {
            return Some(f);
        }
    }
    None
}

/// The FSM state `name`'s stateType object (the state class parameters).
pub fn fsm_state<'a>(b: &'a Block, name: &str) -> Option<&'a Block> {
    for (k, v) in &b.items {
        let Value::Block(c) = v else { continue };
        if k == "node" && c.str("object.object.name") == Some(name) {
            return c.block("object.object.stateType.object");
        }
        if let Some(f) = fsm_state(c, name) {
            return Some(f);
        }
    }
    None
}

/// Shared_CanThrowProjectile's parameters (the link's transitionType object).
#[derive(Debug, Clone, PartialEq)]
pub struct ThrowItem {
    /// Ammo decl the throw launches ("ammo/zion/ai/imp/fireball").
    pub item: String,
    /// Web node played ("zion/characters/monsters/imp/hands_throw/throw").
    pub throw_anim: String,
    pub launch_delay: f32,
    pub primary_attack: bool,
}

impl ThrowItem {
    fn parse(t: &Block) -> ThrowItem {
        ThrowItem {
            item: t.str("item").unwrap_or("").to_string(),
            throw_anim: t.str("throwAnim").unwrap_or("").to_string(),
            launch_delay: t.f32("launchDelayInSeconds.value").unwrap_or(0.0),
            primary_attack: t.path("primaryAttack").and_then(|v| v.as_bool()).unwrap_or(false),
        }
    }
}

/// The Imp's decl parameters the brain reads.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpParms {
    /// aiBehavior minThrowDot (0.707).
    pub min_throw_dot: f32,
    /// aiBehavior maxThrowError.setting[difficulty] (96 / 64 / 32 / 24 / 24).
    pub max_throw_error: [f32; 5],
    /// aiBehavior throwLag.setting[difficulty] (0 x 5).
    pub throw_lag: [f32; 5],
    /// imp_primary_idle -> imp_throw_primary (#7).
    pub throw: ThrowItem,
    /// imp_primary_idle -> imp_charge_melee_primary (#3) Shared_ChargeMelee range (256).
    pub charge_melee_range: f32,
    /// imp_charge_melee_primary intervalInSeconds (4): the charge_melee timer.
    pub charge_melee_interval: f32,
    /// imp_charge_melee_primary -> imp_primary_idle #2 minSecondsInState (5).
    pub charge_melee_max_secs: f32,
    /// imp_primary_idle -> imp_facetarget_primary (#8) Shared_EnemyInRangeAndAngle range / maxAngle / use2D.
    pub face_range: (f32, f32),
    pub face_max_angle: f32,
    pub face_2d: bool,
    /// aiPositioningParms imp/default (positioningParms[0]): min / max distance from the enemy, optimal range.
    pub min_enemy_dist: f32,
    pub max_enemy_dist: f32,
    pub min_optimal_enemy_dist: f32,
    pub max_optimal_enemy_dist: f32,
    /// aiBehavior rolePreferenceOrder[0] (ROLE_THROW): the role a lone Imp takes (INTERIM: the encounter / group
    /// manager's role assignment is not decoded).
    pub role: String,
}

impl ImpParms {
    pub fn load(db: &DeclDb, entity: &str) -> Result<ImpParms> {
        let e = db.get("entitydef", entity).with_context(|| format!("entityDef {entity}"))?;
        let edit = e.block("edit").context("entityDef without edit")?;
        let bdecl = edit.str("aiEditable.behaviors.decl").context("no behaviors decl")?;
        let bh = db.get("aibehavior", bdecl).with_context(|| format!("aiBehavior {bdecl}"))?;
        let bh = bh.block("edit").context("aiBehavior without edit")?;
        let setting = |k: &str| {
            let mut v = [0.0; 5];
            for (i, x) in v.iter_mut().enumerate() {
                *x = bh.f32(&format!("{k}.setting.setting[{i}]")).unwrap_or(0.0);
            }
            v
        };
        let fsm = db.raw("aifsmmanager", IMP_FSM).with_context(|| format!("aiFSMManager {IMP_FSM}"))?;
        let throw = fsm_link(&fsm, "imp_primary_idle", "imp_throw_primary", None).context("no imp_primary_idle -> imp_throw_primary")?;
        let charge = fsm_link(&fsm, "imp_primary_idle", "imp_charge_melee_primary", None).context("no charge melee link")?;
        let charge_state = fsm_state(&fsm, "imp_charge_melee_primary").context("no imp_charge_melee_primary")?;
        let charge_out = fsm_link(&fsm, "imp_charge_melee_primary", "imp_primary_idle", Some("Shared_DefaultUse"));
        let face = fsm_link(&fsm, "imp_primary_idle", "imp_facetarget_primary", None).context("no face link")?;
        let pos = edit.str("aiConstants.positioningParms.item[0]").unwrap_or("imp/default");
        let pp = db.get("aipositioningparms", pos).with_context(|| format!("aiPositioningParms {pos}"))?;
        let pd = pp.block("edit.data");
        let pf = |k: &str| pd.and_then(|d| d.f32(k)).unwrap_or(0.0);
        Ok(ImpParms {
            min_throw_dot: bh.f32("minThrowDot").unwrap_or(0.0),
            max_throw_error: setting("maxThrowError"),
            throw_lag: setting("throwLag"),
            throw: ThrowItem::parse(throw),
            charge_melee_range: charge.f32("range").unwrap_or(0.0),
            charge_melee_interval: charge_state.f32("intervalInSeconds").unwrap_or(0.0),
            charge_melee_max_secs: charge_out.and_then(|b| b.f32("minSecondsInState.value")).unwrap_or(0.0),
            face_range: (face.f32("range.minRange").unwrap_or(0.0), face.f32("range.maxRange").unwrap_or(0.0)),
            face_max_angle: face.f32("maxAngle.value").unwrap_or(0.0),
            face_2d: face.path("use2Dchecks").and_then(|v| v.as_bool()).unwrap_or(false),
            min_enemy_dist: pf("minDistanceFromEnemy"),
            max_enemy_dist: pf("maxDistanceFromEnemy"),
            min_optimal_enemy_dist: pf("minOptimalDistanceFromEnemy"),
            max_optimal_enemy_dist: pf("maxOptimalDistanceFromEnemy"),
            role: bh.str("rolePreferenceOrder.item[0]").unwrap_or("ROLE_THROW").to_string(),
        })
    }
}

/// The AI's target prediction 0x140436640(ai, out, target, launchPoint, speed, throwLag[difficulty], timeToLaunch,
/// flags 2): with speed > 64, up to 4 passes of t = |p - launchPoint| / speed + timeToLaunch + AI +0x24e80 + lag and
/// p = lastKnown + targetVelocity * t, stopping once p moved <= 32 units.
/// INTERIM: AI +0x24e80 (an added time) is taken as 0; the target velocity is the player's physics velocity (the
/// game asks the AI manager, 0x14046c330); the accuracy adjustment 0x140430000 (pPred + unit(vel) *
/// clamp(|vel| / ai_prediction_normalizationSpeed, 0, 1) * (1 + AI +0x23db0)) and the clip of the predicted point to
/// the nav graph (0x14050c240) / world are not applied.
pub fn predict_aim(last_known: Vec3, target_vel: Vec3, launch: Vec3, speed: f32, time_to_launch: f32, lag: f32) -> Vec3 {
    if speed <= 64.0 {
        return last_known;
    }
    let inv = 1.0 / speed;
    let mut p = last_known;
    for _ in 0..4 {
        let t = (p - launch).length() * inv + time_to_launch + lag;
        let q = last_known + target_vel * t;
        let moved = (q - p).length();
        p = q;
        if 32.0 >= moved {
            break;
        }
    }
    p
}

/// One parabola of the throw query (built by 0x14164c1b0 into a 0x1d0-byte trajectory record).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trajectory {
    /// Flight time (s) (+0x2c).
    pub time: f32,
    /// Horizontal unit direction.
    pub dir: Vec3,
    /// Horizontal speed (+0x40) = |end.xy - start.xy| / t.
    pub vh: f32,
    /// Vertical launch speed, up: (dz + g t^2 / 2) / t (stored negated at +0x44).
    pub vz: f32,
    /// Launch pitch, degrees, idTech sign (negative = up) (+0x38) = atan2(-vz, vh) * 57.2958.
    pub pitch: f32,
    /// Launch speed |v| (+0x30).
    pub speed: f32,
}

impl Trajectory {
    /// 0x14164c1b0(traj, start, end, |gravity|, time).
    pub fn build(start: Vec3, end: Vec3, gravity: f32, time: f32) -> Trajectory {
        let (dx, dy, dz) = (end.x - start.x, end.y - start.y, end.z - start.z);
        let h = (dx * dx + dy * dy).sqrt();
        let up = -((time * time) * (gravity * -0.5) - dz) / time;
        let vh = h / time;
        let pitch = (-up).atan2(vh) * 57.2958;
        let dir = Vec3::new(dx, dy, 0.0).normalize_or_zero();
        Trajectory { time, dir, vh, vz: up, pitch, speed: (vh * vh + up * up).sqrt() }
    }

    pub fn velocity(&self) -> Vec3 {
        self.dir * self.vh + Vec3::Z * self.vz
    }
}

/// aiItemSelect_t AIITEMSELECT_IMP (behaviors itemSelect[1] read at bdef +0x170).
pub const ITEMSELECT_IMP: i32 = 8;
/// ai_trajectory_closeTargetDist default (exe cvar table): "when targets are closer than this, try not to use
/// highest trajectories".
pub const CLOSE_TARGET_DIST_DEFAULT: f32 = 512.0;

/// The parabolic throw's candidates, 0x1404308f0(ai, query, trajectories, launchPoint, minT, maxT): flight times
/// (each x the query's speed scale, slot +0x794, 1) maxT, minT + 0.66 (maxT - minT), minT + 0.33 (maxT - minT) and
/// minT (0.001 when below FLT_MIN), each built by 0x14164c1b0 toward the aim point under |gravity|
/// (gameLocal +0x285c10).
pub fn throw_trajectories(start: Vec3, end: Vec3, gravity: f32, min_t: f32, max_t: f32, scale: f32) -> [Trajectory; 4] {
    let mut last = min_t * scale;
    if last < f32::MIN_POSITIVE {
        last = 0.001;
    }
    let times = [max_t * scale, ((max_t - min_t) * 0.66 + min_t) * scale, ((max_t - min_t) * 0.33 + min_t) * scale, last];
    times.map(|t| Trajectory::build(start, end, gravity.abs(), t))
}

/// The trajectory pick 0x1404b1450(trajectories, 4, &sub, &itemSelect, speed, &target): each candidate's clear
/// fraction (1 when its trace ends on the target, 0.93 when it runs to the end, else hit step / steps), capped at
/// 0.93, must exceed 0.9; the highest fraction wins, ties go to the smallest |vh - speed| (AIITEMSELECT_IMP) or
/// |pitch - (-45)| (other selects). Candidate 0 (the highest arc) is skipped when the target is closer than
/// ai_trajectory_closeTargetDist in xy, and only tried when nothing else qualifies.
/// INTERIM: the world traces of the trajectory job are not run; every candidate counts as clear (0.93).
pub fn pick_trajectory(c: &[Trajectory; 4], start: Vec3, end: Vec3, speed: f32, item_select: i32, close_target_dist: f32) -> Option<usize> {
    let metric = |t: &Trajectory| if item_select == ITEMSELECT_IMP { (speed - t.vh).abs() } else { (-45.0 - t.pitch).abs() };
    let d = end - start;
    let close = d.x * d.x + d.y * d.y < close_target_dist * close_target_dist;
    let pass = |order: &[usize]| {
        let (mut best, mut best_frac, mut best_metric) = (None, 0.0f32, 100000.0f32);
        for &i in order {
            let frac = 0.93f32;
            if frac <= 0.9 {
                continue;
            }
            let m = metric(&c[i]);
            if frac > best_frac || (frac == best_frac && best_metric > m) {
                (best, best_frac, best_metric) = (Some(i), frac, m);
            }
        }
        best
    };
    if close { pass(&[1, 2, 3]).or_else(|| pass(&[0])) } else { pass(&[0, 1, 2, 3]) }
}

/// Shared_PositionDistanceValid results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceCheck {
    /// TRANSCODE_YES.
    Valid,
    /// TRANSCODE_NO_LESS_THAN.
    Less,
    /// TRANSCODE_NO_GREATER_THAN.
    Greater,
}

/// A throw in progress (idShared_ThrowProjectile).
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveThrow {
    pub start: i64,
    /// Seconds the throw anim plays.
    pub duration: f32,
    /// The target point the throw aims at (the enemy's last known position when the throw was chosen).
    pub aim: Vec3,
    pub launched: bool,
}

pub struct ImpBrain {
    /// Shared brain pieces: decls, senses, the attack graph and its melee windows, path following.
    pub base: Brain,
    pub state: ImpState,
    pub parms: ImpParms,
    pub projectile: Arc<AiProjectileDef>,
    /// hands_combat/run (WALKSTATE_RUNNING) and hands_combat/sprint (WALKSTATE_SPRINTING).
    pub run: Locomotion,
    pub sprint: Locomotion,
    pub throw: Option<ActiveThrow>,
    /// Game tick the pending throw query was submitted (the "<link>_running_" blackboard key).
    throw_query: Option<i64>,
    /// The throw anim (duration s, frame rate).
    throw_anim: (f32, f32),
    /// Named AI timers (idAITimers): expiry game tick.
    timers: HashMap<&'static str, i64>,
    state_since: i64,
    pub difficulty: usize,
    /// ai_trajectory_closeTargetDist (the caller sets the install's cvar value).
    pub close_target_dist: f32,
}

impl ImpBrain {
    pub fn load(db: &DeclDb, entity: &str, difficulty: usize) -> Result<ImpBrain> {
        let base = Brain::load(db, entity)?;
        let parms = ImpParms::load(db, entity)?;
        let projectile = Arc::new(AiProjectileDef::load(db, &parms.throw.item)?);
        let c = db.container();
        let web_src = c.read_by_name(&format!("generated/decls/animweb/{}.decl", base.def.anim_web))?;
        let web = idres::animweb::AnimWeb::parse(&String::from_utf8_lossy(&web_src))?;
        let loco = |node: &str| -> Locomotion {
            let speed = web
                .sub_web("hands_combat")
                .and_then(|s| s.node(node))
                .and_then(|n| n.trees.iter().flat_map(|t| t.anims.iter()).find(|a| a.coordinate.first() == Some(&0.0)).or_else(|| n.trees.first()?.anims.first()))
                .and_then(|a| super::root_speed(c, &a.name))
                .map_or(0.0, |s| s.0);
            Locomotion { walk_node: format!("{}/hands_combat/{node}", base.def.anim_web), idle_node: base.loco.idle_node.clone(), walk_speed: speed, body_turn_rate: base.loco.body_turn_rate }
        };
        let (run, sprint) = (loco("run"), loco("sprint"));
        let throw_anim = super::first_anim(&web, &base.def.anim_web, &parms.throw.throw_anim)
            .and_then(|a| super::root_speed(c, &a))
            .map_or((0.0, 30.0), |s| (s.1, s.2));
        Ok(ImpBrain { base, state: ImpState::Relaxed, parms, projectile, run, sprint, throw: None, throw_query: None, throw_anim, timers: HashMap::new(), state_since: 0, difficulty: difficulty.min(4), close_target_dist: CLOSE_TARGET_DIST_DEFAULT })
    }

    fn timer_expired(&self, name: &str) -> bool {
        self.timers.get(name).is_none_or(|&t| self.base.now >= t)
    }

    fn secs_in_state(&self) -> f32 {
        (self.base.now - self.state_since) as f32 / TICKS_PER_SEC as f32
    }

    /// Shared_EnemyInRangeAndAngle 0x1405ec600: YES when the enemy is within [min, max] (3D unless use2Dchecks)
    /// and within maxAngle of the body's forward axis in xy.
    pub fn enemy_in_range_and_angle(&self, body: &Body, target: &Target) -> bool {
        let d = target.origin - body.origin;
        let mut dist2 = d.x * d.x + d.y * d.y;
        if !self.parms.face_2d {
            dist2 += d.z * d.z;
        }
        let (lo, hi) = self.parms.face_range;
        if dist2 < lo * lo || dist2 > hi * hi {
            return false;
        }
        let f = Vec3::new(body.yaw.to_radians().cos(), body.yaw.to_radians().sin(), 0.0);
        let dir = Vec3::new(d.x, d.y, 0.0).normalize_or_zero();
        f.dot(dir) >= (self.parms.face_max_angle * 0.017453292).cos()
    }

    /// Shared_ChargeMelee 0x1405ebeb0 (ignoreChargeTimer false, waitForPositionAwarenessChecks false): the
    /// charge_melee timer expired, no failed charge move within 2500 ticks, the enemy CONFIRMED and seen, not
    /// injured, and the enemy's last known position within `range`.
    pub fn should_charge_melee(&self, body: &Body, target: &Target) -> bool {
        self.timer_expired("charge_melee")
            && target.alive
            && self.base.sense.awareness == Awareness::Confirmed
            && self.base.sense.last_visible.is_some()
            && self.base.sense.last_known_pos.distance_squared(body.origin) <= self.parms.charge_melee_range * self.parms.charge_melee_range
    }

    /// Shared_ShouldCharge 0x1405f26d0: SHOULD_MOVE only in the charge group role (7).
    pub fn should_charge(&self) -> bool {
        self.parms.role == "ROLE_CHARGE"
    }

    /// The transition of the current state that fires this think, in orderIndex order (module docs).
    fn transition(&mut self, body: &Body, target: &Target) -> Option<ImpState> {
        let confirmed = self.base.sense.awareness == Awareness::Confirmed && target.alive;
        match self.state {
            ImpState::Relaxed => confirmed.then_some(ImpState::Default),
            ImpState::Default => {
                if self.base.sense.newly_aware {
                    Some(ImpState::Sighted)
                } else {
                    Some(ImpState::Idle)
                }
            }
            ImpState::Sighted => {
                // Shared_ChildFinished: INTERIM as the Possessed, once the body faces the enemy.
                let face = wrap180(yaw_to(target.origin - body.origin) - body.yaw).abs();
                (face <= self.base.def.movement.alignment_tolerance).then_some(ImpState::Default)
            }
            // #3 Shared_SocialRelationAvailable(CONFIRMED) -> imp_terminate -> imp_destroy_primary (engage FSM).
            ImpState::Idle => confirmed.then_some(ImpState::PrimaryIdle),
            ImpState::PrimaryIdle => {
                if !target.alive {
                    return None;
                }
                // #0 Shared_ShouldAttack(default).
                if self.base.choose_attack(body, target, "default").is_some() {
                    return Some(ImpState::Melee);
                }
                // #1 Shared_ShouldCharge -> imp_p_c_c -> TargetDistanceFilter -> charge: only in the charge role.
                // #2 DefaultUse (timer next_major_position_check: required expired 0.5 s, set on evaluate) -> the
                // position checks (imp_pdbp).
                let since = self.base.now - self.timers.get("next_major_position_check").copied().unwrap_or(i64::MIN / 2);
                if since >= TICKS_PER_SEC / 2 {
                    self.timers.insert("next_major_position_check", self.base.now);
                    if self.should_advance(body, target) {
                        return Some(ImpState::Advance);
                    }
                }
                // #3 Shared_ChargeMelee.
                if self.should_charge_melee(body, target) {
                    return Some(ImpState::ChargeMelee);
                }
                // #7 Shared_CanThrowProjectile: a passing precheck submits the throw query and defers (result 6);
                // a later think reads the query (0x1405e8700) -> TRANSCODE_OC_THROW_PROJECTILE.
                // INTERIM: the query reports on the next think (the job latency is not decoded).
                if self.can_throw(body, target) {
                    match self.throw_query {
                        Some(t) if t < self.base.now => {
                            self.throw_query = None;
                            return Some(ImpState::Throw);
                        }
                        Some(_) => {}
                        None => self.throw_query = Some(self.base.now),
                    }
                } else {
                    self.throw_query = None;
                }
                // #8 Shared_EnemyInRangeAndAngle NO.
                if !self.enemy_in_range_and_angle(body, target) {
                    return Some(ImpState::FaceTarget);
                }
                None
            }
            ImpState::Melee => self.base.attack_done().then_some(ImpState::PrimaryIdle),
            ImpState::ChargeMelee => {
                if self.base.choose_attack(body, target, "default").is_some() {
                    Some(ImpState::Melee)
                } else if self.base.reached(body, target) || self.secs_in_state() >= self.parms.charge_melee_max_secs {
                    // Shared_ChildFinished (INTERIM: the charge move arrived) / DefaultUse minSecondsInState 5.
                    Some(ImpState::PrimaryIdle)
                } else {
                    None
                }
            }
            ImpState::Throw => {
                let done = self.throw.as_ref().is_none_or(|t| (self.base.now - t.start) as f32 / TICKS_PER_SEC as f32 >= t.duration);
                done.then_some(ImpState::PrimaryIdle)
            }
            // Shared_WorkTransCode: the face state's work is done once the enemy is within the angle.
            ImpState::FaceTarget => self.enemy_in_range_and_angle(body, target).then_some(ImpState::PrimaryIdle),
            // #2 Shared_ReachedViablePosition MOVE_DONE (INTERIM: back inside the ideal distance; not decoded).
            ImpState::Advance => {
                (self.position_distance(body, target, true) != DistanceCheck::Greater || self.base.reached(body, target)).then_some(ImpState::PrimaryIdle)
            }
        }
    }

    /// Shared_PositionDistanceValid 0x1405f0c50 with the positioning parms (aiPositioningParms imp/default): the
    /// 3D distance to the enemy against min / maxDistanceFromEnemy (TRANSCODE_NO_LESS_THAN 9 /
    /// NO_GREATER_THAN 10), with checkIdeal also against min / maxOptimalDistanceFromEnemy; else YES (7).
    /// INTERIM: the enemy position is the target origin (the game reads 0x140433340).
    pub fn position_distance(&self, body: &Body, target: &Target, check_ideal: bool) -> DistanceCheck {
        let d2 = body.origin.distance_squared(target.origin);
        let p = &self.parms;
        if d2 < p.min_enemy_dist * p.min_enemy_dist {
            DistanceCheck::Less
        } else if d2 > p.max_enemy_dist * p.max_enemy_dist {
            DistanceCheck::Greater
        } else if check_ideal && d2 < p.min_optimal_enemy_dist * p.min_optimal_enemy_dist {
            DistanceCheck::Less
        } else if check_ideal && p.max_optimal_enemy_dist * p.max_optimal_enemy_dist < d2 {
            DistanceCheck::Greater
        } else {
            DistanceCheck::Valid
        }
    }

    /// imp_pdbp: #0 Shared_PositionDistanceValid NO -> imp_bad_distance_primary, #1 Shared_HasValidMinorPositions
    /// NO -> imp_bad_visibility_primary; from both, Shared_SuperiorPositionAvailable NO -> imp_adv_check_dist_primary:
    /// Shared_PositionDistanceValid(checkIdeal) NO_GREATER_THAN -> imp_adv_check_role_primary: Shared_EncounterRole
    /// 0x1405ec500 (role in the exclude list ROLE_DEFEND -> NO; no include list -> YES) -> imp_advance_primary.
    /// Links match their transcode exactly (0x14064e570: a link code of 0 takes any nonzero result), so #0 is dead
    /// in this build (PositionDistanceValid returns YES / NO_LESS_THAN / NO_GREATER_THAN, never NO); the advance
    /// comes through #1. INTERIM: without position-awareness data (the range) HasValidMinorPositions and
    /// SuperiorPositionAvailable are taken as NO, so the Imp advances whenever the enemy is beyond
    /// maxOptimalDistanceFromEnemy.
    fn should_advance(&self, body: &Body, target: &Target) -> bool {
        self.position_distance(body, target, true) == DistanceCheck::Greater && self.parms.role != "ROLE_DEFEND"
    }

    /// Shared_CanThrowProjectile 0x1405eb120 for the fireball (#7): the throw precheck 0x1405f79b0 ("all throw
    /// checks pass", result 0x3b) -
    /// - Entity_HasBeenSeen(target, now, 1 s): `now - lastVisibleTime < 960` ticks;
    /// - unless hangout: dot(unit(last known position - origin), facing axis 0x1408213b0) >= bdef minThrowDot
    ///   (+0x17c), the direction in 3D (INTERIM: the facing axis is the body's horizontal forward);
    /// - the item's projectile decl minRange^2 <= |d.xy|^2 <= maxRange^2 (+0x39c / +0x398);
    /// then the trajectory query (AddTrajectoryTestForQuery 0x140483050) runs and 0x1405e8700 returns
    /// TRANSCODE_OC_THROW_PROJECTILE once a trajectory checks out. INTERIM: the query's parabola tests against the
    /// world are not run (the range is open); the result is taken on the same think instead of a later one.
    pub fn can_throw(&self, body: &Body, target: &Target) -> bool {
        if !target.alive || self.base.sense.awareness != Awareness::Confirmed {
            return false;
        }
        let Some(seen) = self.base.sense.last_visible else { return false };
        if self.base.now - seen >= TICKS_PER_SEC {
            return false;
        }
        let to = self.base.sense.last_known_pos - body.origin;
        let f = Vec3::new(body.yaw.to_radians().cos(), body.yaw.to_radians().sin(), 0.0);
        if f.dot(to.normalize_or_zero()) < self.parms.min_throw_dot {
            return false;
        }
        let d2 = to.x * to.x + to.y * to.y;
        let (lo, hi) = (self.projectile.min_range, self.projectile.max_range);
        lo * lo <= d2 && d2 <= hi * hi
    }

    /// [`predict_aim`] with this Imp's throwLag.
    pub fn predict_aim(&self, last_known: Vec3, target_vel: Vec3, launch: Vec3, speed: f32, time_to_launch: f32) -> Vec3 {
        predict_aim(last_known, target_vel, launch, speed, time_to_launch, self.parms.throw_lag[self.difficulty])
    }

    /// ae_launchItem: the launch velocity from `from` (the event joint) toward `aim` (the player's aim point,
    /// moving at `aim_vel`) under `gravity` (|g|, units/s^2).
    /// The aim is led by [`ImpBrain::predict_aim`]; a straight (non-parabolic or gravity-free) projectile flies at
    /// its decl speed; a parabolic one takes the [`throw_trajectories`] candidate [`pick_trajectory`] picks.
    /// INTERIM: the game builds and picks the trajectory when the throw query runs (AddTrajectoryTestForQuery
    /// 0x140483050, refreshed at ae_updateTrajectories) from the anim's launch point and LaunchItem (0x140435910)
    /// follows the stored one; here it is built at the launch from the joint's actual position (time to launch 0),
    /// the world traces of the candidates are not run (every candidate counts as clear), and no throw error
    /// (maxThrowError) is applied (where the game applies it was not found).
    pub fn launch_velocity(&mut self, from: Vec3, aim: Vec3, aim_vel: Vec3, gravity: f32) -> Option<Vec3> {
        let aim = self.predict_aim(aim, aim_vel, from, self.projectile.speed, 0.0);
        let p = &self.projectile;
        if let Some(t) = self.throw.as_mut() {
            t.launched = true;
        }
        let d = aim - from;
        let dist = d.length();
        if dist < 1e-3 || p.speed <= 0.0 {
            return None;
        }
        if !p.parabolic || !p.gravity {
            return Some(d / dist * p.speed);
        }
        let cands = throw_trajectories(from, aim, gravity, p.min_trajectory_time, p.max_trajectory_time, 1.0);
        let i = pick_trajectory(&cands, from, aim, p.speed, ITEMSELECT_IMP, self.close_target_dist)?;
        Some(cands[i].velocity())
    }

    fn enter(&mut self, s: ImpState, body: &Body, target: &Target, out: &mut AiOutput) {
        let from = self.state;
        self.state = s;
        self.state_since = self.base.now;
        out.events.push(AiEvent::DeclStateChanged { from: from.decl_name(), to: s.decl_name() });
        match s {
            ImpState::Sighted => self.base.sense.newly_aware = false,
            ImpState::Melee => {
                self.base.start_attack(body, target, "default", out);
            }
            ImpState::ChargeMelee => {
                // idShared_ChargeMelee intervalInSeconds: the charge_melee timer.
                let t = self.base.now + (self.parms.charge_melee_interval * TICKS_PER_SEC as f32) as i64;
                self.timers.insert("charge_melee", t);
            }
            ImpState::Throw => {
                self.throw = Some(ActiveThrow { start: self.base.now, duration: self.throw_anim.0, aim: self.base.sense.last_known_pos, launched: false });
                out.events.push(AiEvent::ThrowStart { node: self.parms.throw.throw_anim.clone(), dest_node: self.base.loco.idle_node.clone(), item: self.parms.throw.item.clone() });
            }
            _ => {}
        }
    }

    /// One game tick (as [`Brain::tick`]).
    pub fn tick(&mut self, world: &dyn World, body: &Body, target: &Target, now: i64, dt: f32) -> AiOutput {
        self.base.now = now;
        let mut out = AiOutput { yaw: body.yaw, ..Default::default() };
        if !body.alive || body.in_pain {
            return out;
        }
        self.base.sense_tick(world, body, target, now, dt, &mut out);
        self.base.moving = if matches!(self.state, ImpState::ChargeMelee | ImpState::Advance) { super::Moving::Running } else { super::Moving::Stopped };
        for _ in 0..5 {
            match self.transition(body, target) {
                Some(s) => self.enter(s, body, target, &mut out),
                None => break,
            }
        }
        let to_target = target.origin - body.origin;
        let web = self.base.def.anim_web.clone();
        match self.state {
            ImpState::Relaxed => out.web_node = Some(format!("{web}/hands_relaxed/idle")),
            ImpState::Default | ImpState::Idle | ImpState::PrimaryIdle => out.web_node = Some(self.base.loco.idle_node.clone()),
            ImpState::Sighted | ImpState::FaceTarget => {
                out.web_node = Some(self.base.loco.idle_node.clone());
                // INTERIM: in-place turn at the body turn rate (the game plays the turn transitions).
                out.yaw = self.base.turn_toward(body.yaw, yaw_to(to_target), dt);
            }
            ImpState::Melee => self.base.attack_tick(&mut out),
            ImpState::Throw => out.web_node = Some(self.parms.throw.throw_anim.clone()),
            ImpState::ChargeMelee | ImpState::Advance => {
                // idShared_ChargeMelee / idShared_MoveTowardEnemy: WALKSTATE_SPRINTING for the charge (INTERIM: the
                // charge state's walk state is not decoded; imp_charge_target_primary sprints), RUNNING to advance.
                let loco = if self.state == ImpState::ChargeMelee { self.sprint.clone() } else { self.run.clone() };
                let keep = std::mem::replace(&mut self.base.loco, loco);
                self.base.walk(world, body, target, dt, &mut out);
                self.base.loco = keep;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    //! Install-backed tests of the Imp's decoded numbers and brain (skipped without an install).
    use super::*;
    use crate::ai::OpenWorld;
    use crate::demons::damage::AiDamageParms;
    use crate::demons::live::{player_damage, PlayerDamageScales};

    const FRAME_TICKS: i64 = 16;
    const DT: f32 = 1.0 / 60.0;

    fn install() -> Option<crate::install::Install> {
        let doom = idres::find_install()?;
        Some(crate::install::load(&doom).expect("loading install"))
    }

    fn target_at(p: Vec3) -> Target {
        Target { origin: p, sight_point: p + Vec3::Z * 45.0, alive: true, half_width: 16.0 }
    }

    /// Runs frames until `stop` holds for an output (or `n` frames); returns every output.
    fn run(b: &mut ImpBrain, body: &mut Body, t: &Target, start: i64, n: usize, stop: impl Fn(&AiOutput) -> bool) -> Vec<AiOutput> {
        let w = OpenWorld { nav: None };
        let mut outs = Vec::new();
        for i in 0..n {
            let o = b.tick(&w, body, t, start + i as i64 * FRAME_TICKS, DT);
            body.step(&o, DT);
            let done = stop(&o);
            outs.push(o);
            if done {
                break;
            }
        }
        outs
    }

    #[test]
    fn imp_decls() {
        let Some(inst) = install() else { return };
        let p = ImpParms::load(&inst.decls, IMP).unwrap();
        assert_eq!(p.min_throw_dot, 0.707);
        assert_eq!(p.max_throw_error, [96.0, 64.0, 32.0, 24.0, 24.0]);
        assert_eq!(p.throw_lag, [0.0; 5]);
        assert_eq!(p.throw.item, "ammo/zion/ai/imp/fireball");
        assert_eq!(p.throw.throw_anim, "zion/characters/monsters/imp/hands_throw/throw");
        assert!(p.throw.primary_attack);
        assert_eq!((p.charge_melee_range, p.charge_melee_interval, p.charge_melee_max_secs), (256.0, 4.0, 5.0));
        assert_eq!((p.face_range, p.face_max_angle, p.face_2d), ((-1.0, 9999.0), 45.0, false));
        assert_eq!((p.min_enemy_dist, p.max_enemy_dist, p.min_optimal_enemy_dist, p.max_optimal_enemy_dist), (384.0, 2048.0, 512.0, 1024.0));
        assert_eq!(p.role, "ROLE_THROW");

        // ammo -> projectile -> projectile entity.
        let f = AiProjectileDef::load(&inst.decls, &p.throw.item).unwrap();
        assert_eq!(f.name, "projectile/zion/ai/imp/fireball");
        assert_eq!((f.speed, f.parabolic, f.gravity, f.max_trajectory_time), (1250.0, true, true, 1.0));
        assert_eq!((f.min_range, f.max_range), (0.0, 9999.0));
        assert_eq!(f.damage_decl, "damage/zion/ai/imp/fireball");
        assert_eq!(f.entity_def, "projectile_ent/zion/ai/imp/fireball");
        assert_eq!((f.clip_size, f.clip_offset), (Vec3::splat(15.0), Vec3::new(0.0, 0.0, 2.0)));
        assert!(f.explode_on_impact);
        let fast = AiProjectileDef::load(&inst.decls, "ammo/zion/ai/imp/fireball_fastball").unwrap();
        assert_eq!((fast.speed, fast.gravity, fast.max_trajectory_time), (2400.0, false, 0.3));

        // idPlayer::Damage: fireball 60 x playerDamageScale 0.25 x difficulty; melee 30 x 0.25.
        let cvars = crate::config::CvarValues::default();
        let fb = AiDamageParms::from_decl(&inst.decls, &f.damage_decl).unwrap();
        let by_diff: Vec<f32> = (0..5).map(|d| player_damage(&fb, 512.0 * 512.0, 1.0, &PlayerDamageScales::load(&inst.decls, &cvars, d))).collect();
        assert_eq!(by_diff, [7.5, 15.0, 26.25, 45.0, 45.0]);
        let fastd = AiDamageParms::from_decl(&inst.decls, &fast.damage_decl).unwrap();
        assert_eq!(player_damage(&fastd, 0.0, 1.0, &PlayerDamageScales::load(&inst.decls, &cvars, 1)), 25.0);
        let melee = AiDamageParms::from_decl(&inst.decls, "damage/zion/ai/imp/demon_imp_melee").unwrap();
        assert_eq!(player_damage(&melee, 0.0, 1.0, &PlayerDamageScales::load(&inst.decls, &cvars, 1)), 7.5);
        // The damage / pain side of the range demon loads for the Imp as for the Possessed.
        let def = crate::demons::DemonDef::load(&inst.decls, IMP).unwrap();
        assert_eq!((def.health, def.anim_web.as_str()), (150.0, "zion/characters/monsters/imp"));
        crate::demons::WebTags::load(inst.decls.container(), &def.anim_web).unwrap();
    }

    #[test]
    fn throw_trajectory_pick() {
        // Fireball: minTrajectoryTime 0, maxTrajectoryTime 1 -> flight times 1, 0.66, 0.33, 0.001; g_gravity 1066.
        let c = throw_trajectories(Vec3::ZERO, Vec3::new(600.0, 0.0, 0.0), 1066.0, 0.0, 1.0, 1.0);
        let times: Vec<f32> = c.iter().map(|t| t.time).collect();
        assert_eq!(times, [1.0, 0.66, 0.33, 0.001]);
        assert!((c[1].vh - 909.0909).abs() < 1e-2 && (c[1].vz - 351.78).abs() < 1e-2, "{:?}", c[1]);
        assert!(c[1].pitch < 0.0);
        // 600 units: |vh - 1250| is smallest for t = 0.66.
        assert_eq!(pick_trajectory(&c, Vec3::ZERO, Vec3::new(600.0, 0.0, 0.0), 1250.0, ITEMSELECT_IMP, 512.0), Some(1));
        // 1500 units: t = 1 (vh 1500) beats t = 0.66 (2272).
        let far = throw_trajectories(Vec3::ZERO, Vec3::new(1500.0, 0.0, 0.0), 1066.0, 0.0, 1.0, 1.0);
        assert_eq!(pick_trajectory(&far, Vec3::ZERO, Vec3::new(1500.0, 0.0, 0.0), 1250.0, ITEMSELECT_IMP, 512.0), Some(0));
        // 300 units (closer than ai_trajectory_closeTargetDist): the highest arc is skipped; t = 0.33 (vh 909).
        let near = throw_trajectories(Vec3::ZERO, Vec3::new(300.0, 0.0, 0.0), 1066.0, 0.0, 1.0, 1.0);
        assert_eq!(pick_trajectory(&near, Vec3::ZERO, Vec3::new(300.0, 0.0, 0.0), 1250.0, ITEMSELECT_IMP, 512.0), Some(2));
    }

    #[test]
    fn fireball_flies_and_hits_the_player() {
        let Some(inst) = install() else { return };
        let mut b = ImpBrain::load(&inst.decls, IMP, 1).unwrap();
        let mut w = crate::collision::World::default();
        w.add(crate::collision::Hull::cuboid(Vec3::new(-1000.0, -1000.0, -64.0), Vec3::new(1000.0, 1000.0, 0.0)));
        // Player standing at the origin (pm_bboxwidth 32, pm_normalheight 90 boxes); the Imp's hand 600 units away,
        // 60 up; aim point type 8: origin + pm_normalViewHeight (87) / 2.
        let player = (Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 90.0));
        let hand = Vec3::new(600.0, 0.0, 60.0);
        let vel = b.launch_velocity(hand, Vec3::new(0.0, 0.0, 43.5), Vec3::ZERO, 1066.0).unwrap();
        // t = 0.66 s candidate: 909 u/s across.
        assert!((Vec3::new(vel.x, vel.y, 0.0).length() - 600.0 / 0.66).abs() < 0.5, "{vel}");
        let mut p = crate::demons::projectile::AiProjectile { def: b.projectile.clone(), pos: hand, vel, age: 0.0, owner: 1, launch: hand };
        let mut hit = None;
        for _ in 0..120 {
            if let Some(h) = p.step(&w, 1066.0, player, 1.0 / 60.0) {
                hit = Some(h);
                break;
            }
        }
        assert!(matches!(hit, Some(crate::demons::projectile::Impact::Player { .. })), "{hit:?}");
        assert!((p.age - 0.66).abs() < 0.1, "{}", p.age);
        let parms = AiDamageParms::from_decl(&inst.decls, &p.def.damage_decl).unwrap();
        let cvars = crate::config::CvarValues::default();
        assert_eq!(player_damage(&parms, p.launch.distance_squared(Vec3::ZERO), 1.0, &PlayerDamageScales::load(&inst.decls, &cvars, 1)), 15.0);
    }

    #[test]
    fn imp_notices_and_throws() {
        let Some(inst) = install() else { return };
        let mut b = ImpBrain::load(&inst.decls, IMP, 1).unwrap();
        // The Imp 600 units away facing the player.
        let mut body = Body { origin: Vec3::ZERO, yaw: 0.0, alive: true, in_pain: false };
        let t = target_at(Vec3::new(600.0, 0.0, 0.0));
        let outs = run(&mut b, &mut body, &t, 0, 120, |o| o.events.iter().any(|e| matches!(e, AiEvent::ThrowStart { .. })));
        assert!(outs.iter().any(|o| o.events.contains(&AiEvent::Noticed)));
        let thrown = outs.iter().flat_map(|o| o.events.iter()).find_map(|e| match e {
            AiEvent::ThrowStart { node, item, .. } => Some((node.clone(), item.clone())),
            _ => None,
        });
        assert_eq!(thrown, Some(("zion/characters/monsters/imp/hands_throw/throw".into(), "ammo/zion/ai/imp/fireball".into())));
        assert_eq!(b.state, ImpState::Throw);
        // The throw ends with the throw anim; then the Imp is back in imp_primary_idle and throws again.
        let dur = b.throw.as_ref().unwrap().duration;
        assert!(dur > 0.5 && dur < 3.0, "{dur}");
        let start = outs.len() as i64 * FRAME_TICKS;
        let more = run(&mut b, &mut body, &t, start, (dur * 60.0) as usize + 30, |o| o.events.iter().any(|e| matches!(e, AiEvent::ThrowStart { .. })));
        assert!(more.iter().any(|o| o.events.contains(&AiEvent::DeclStateChanged { from: "imp_throw_primary", to: "imp_primary_idle" })));
    }

    #[test]
    fn imp_melees_up_close_and_charges() {
        let Some(inst) = install() else { return };
        // 100 units: melee_forward (0..128 within +-45).
        let mut b = ImpBrain::load(&inst.decls, IMP, 1).unwrap();
        let mut body = Body { origin: Vec3::ZERO, yaw: 0.0, alive: true, in_pain: false };
        let t = target_at(Vec3::new(100.0, 0.0, 0.0));
        let outs = run(&mut b, &mut body, &t, 0, 120, |o| o.events.iter().any(|e| matches!(e, AiEvent::AttackStart { .. })));
        let attack = outs.iter().flat_map(|o| o.events.iter()).find_map(|e| match e {
            AiEvent::AttackStart { attack, .. } => Some(attack.clone()),
            _ => None,
        });
        assert_eq!(attack.as_deref(), Some("melee_forward"));
        // 200 units (inside Shared_ChargeMelee's 256, beyond every melee range): the Imp charges, sprinting.
        let mut b = ImpBrain::load(&inst.decls, IMP, 1).unwrap();
        let mut body = Body { origin: Vec3::ZERO, yaw: 0.0, alive: true, in_pain: false };
        let t = target_at(Vec3::new(200.0, 0.0, 0.0));
        let outs = run(&mut b, &mut body, &t, 0, 60, |o| o.events.contains(&AiEvent::DeclStateChanged { from: "imp_primary_idle", to: "imp_charge_melee_primary" }));
        assert_eq!(b.state, ImpState::ChargeMelee);
        assert_eq!(outs.last().unwrap().web_node.as_deref(), Some("zion/characters/monsters/imp/hands_combat/sprint"));
        // Once running, the attack graph's "moving" attacks (0..300, not usable while stopped) are valid:
        // Shared_ShouldAttack (#0 of the charge state) starts one.
        let o = run(&mut b, &mut body, &t, outs.len() as i64 * FRAME_TICKS, 1, |_| false).pop().unwrap();
        let attack = o.events.iter().find_map(|e| match e {
            AiEvent::AttackStart { attack, .. } => Some(attack.clone()),
            _ => None,
        });
        assert!(attack.as_deref().is_some_and(|a| a.starts_with("moving_")), "{attack:?}");
        assert_eq!(b.state, ImpState::Melee);
    }
}
