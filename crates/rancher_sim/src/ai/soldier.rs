//! The Possessed Soldier (`ai/hellified/marine_rifle`): its open-combat FSM, the anim-driven plasma rifle and melee
//! (gamedata/re/DEMONS.md section 15).
//!
//! FSM: aiFSMManager `states/plasmacombat`, layers config "hellsoldier" (common + hellsoldier; behaviors
//! hellified_marine_rifle aiFSMManagerDeclLayersConfig). Once the player is CONFIRMED: plasmacombat oc_bookkeeping ->
//! oc_default -> oc_idle -> (Shared_SocialRelationAvailable, CONFIRMED) oc_destroy_type -> oc_destroy_primary = child
//! FSM `oc_engage_primary`, default state oc_idle_primary:
//!   oc_idle_primary: #0 COMBAT_ShouldSightEnemy -> oc_sighted_primary, #1 DefaultUse -> p_idle_do_something:
//!     #0 RandomChance 0.5 -> reposition_sequence, #1 -> idle_shoot_sequence.
//!   idle_shoot_sequence (child FSM plasma_idle: plasma_idle_default -> #0 target visible -> plasma_shoot_idle, an
//!     AttackIdle with idleFireMode WEAPON_EXPLICIT_RELEASE and override idle anim rifle_combat/shoot, back after
//!     >= 1 s once the target is not visible): #0 DefaultUse -> p_immediate, #1 (4 s) -> p_r1, #2 (5 s, RandomChance
//!     0.5, debounce timer iss_to_rs_debounce 1 s) -> reposition_sequence.
//!   reposition_sequence (child plasma_reposition_sequence: Shared_Reposition -> p_repo_short_slow, WALKING):
//!     #0 -> p_immediate, #1 -> p_r1, #2 (10 s) -> oc_idle_primary.
//!   p_immediate (orderIndex): #0 Shared_ShouldAttack -> plasma_melee, #1 Shared_ShouldCharge -> charge (role),
//!     #2 PositionDistanceValid NO (dead link: exact transcode match, see the Imp), #3 slug test (tokens), #4
//!     HasValidMinorPositions NO -> ... -> PositionDistanceValid(ideal) NO_GREATER_THAN -> EncounterRole (exclude
//!     ROLE_DEFEND) -> oc_advance_to_enemy, #5 FiringIntentCollisions, #6 reposition for visibility, #7
//!     Shared_ChargeMelee(180, timer last_charge_finish 6 s) -> oc_transient_charge.
//! The rifle fires while the anim holds the trigger: rifle_combat/shoot's shoot_idle.md6anim pulls
//! (ae_aIPullTrigger "right_hand") and releases (ae_aIReleaseTrigger) it in bursts; AnimEvent_AIPullTrigger
//! 0x1403e5640 -> the fire control (AI +0x444f*8) pull 0x1404437c0; the weapon then fires at its firingInterval.
//! In the FIREWHENREADY states (plasma_idle_default, plasma_reposition_default, oc_advance_to_enemy) the fire control
//! times the bursts itself from the weapon's skillSettings (burst 440..660, re-pull 1000..1300 ticks).
//! INTERIM / not ported: reposition / cover / strafe positions (position awareness), group roles (the lone soldier
//! takes rolePreferenceOrder[0] ROLE_DEFEND), the plasma slug sub FSM, melee combos, rage, firing while advancing.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decldb::DeclDb;

use super::imp::{fsm_link, fsm_state, predict_aim, DistanceCheck};
use super::perception::Awareness;
use super::{wrap180, yaw_to, AiEvent, AiOutput, Body, Brain, Locomotion, Moving, Target, World, TICKS_PER_SEC};
use crate::demons::projectile::AiProjectileDef;

/// The Possessed Soldier (plasma rifle).
pub const SOLDIER: &str = "ai/hellified/marine_rifle";
/// Its combat FSM decl.
pub const SOLDIER_FSM: &str = "states/plasmacombat";
/// The sub-web that plays AISUBWEB_COMBAT / RELAXED for a rifle. INTERIM: the aiSubWeb_t -> name mapping is not
/// decoded (the exe has a weapon anim prefix table at 0x142f9f140: "rifle_" / "rifle/", "shotgun_", "rocket_").
pub const COMBAT_SUBWEB: &str = "rifle_combat";
pub const RELAXED_SUBWEB: &str = "rifle_relaxed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoldierState {
    Relaxed,
    /// oc_sighted_primary [idCombat_SightedEnemy].
    Sighted,
    /// oc_idle_primary [idOpenCombat_AttackIdle, HOLDFIRE].
    Idle,
    /// idle_shoot_sequence (child plasma_idle).
    ShootSequence,
    /// reposition_sequence (child plasma_reposition_sequence).
    RepositionSequence,
    /// plasma_melee [idShared_Attack].
    Melee,
    /// oc_transient_charge [idShared_ChargeMelee].
    Charge,
    /// oc_advance_to_enemy [idShared_MoveTowardEnemy RUNNING].
    Advance,
}

impl SoldierState {
    pub fn decl_name(self) -> &'static str {
        match self {
            SoldierState::Relaxed => "behaviors_relaxed_shared",
            SoldierState::Sighted => "oc_sighted_primary",
            SoldierState::Idle => "oc_idle_primary",
            SoldierState::ShootSequence => "idle_shoot_sequence",
            SoldierState::RepositionSequence => "reposition_sequence",
            SoldierState::Melee => "plasma_melee",
            SoldierState::Charge => "oc_transient_charge",
            SoldierState::Advance => "oc_advance_to_enemy",
        }
    }
}

/// The soldier's decl parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct SoldierParms {
    /// p_idle_do_something -> reposition_sequence RandomChance (0.5).
    pub reposition_chance: f32,
    /// idle_shoot_sequence -> reposition_sequence: minSecondsInState (5), RandomChance (0.5), debounce (1 s).
    pub shoot_to_reposition_secs: f32,
    pub shoot_to_reposition_chance: f32,
    pub shoot_to_reposition_debounce: f32,
    /// reposition_sequence -> oc_idle_primary minSecondsInState (10).
    pub reposition_to_idle_secs: f32,
    /// plasma_shoot_idle -> plasma_idle_default minSecondsInState (1).
    pub shoot_min_secs: f32,
    /// p_immediate -> oc_transient_charge: Shared_ChargeMelee range (180), timer last_charge_finish (6 s).
    pub charge_range: f32,
    pub charge_timer_secs: f32,
    /// oc_transient_charge -> oc_idle_primary DefaultUse minSecondsInState (5).
    pub charge_max_secs: f32,
    /// aiPositioningParms hellsoldier/plasma: min / max / ideal distances from the enemy.
    pub min_enemy_dist: f32,
    pub max_enemy_dist: f32,
    pub min_optimal_enemy_dist: f32,
    pub max_optimal_enemy_dist: f32,
    /// rolePreferenceOrder[0] (INTERIM, as the Imp).
    pub role: String,
    /// The rifle: weapon decl firingInterval (ms) and its ammo (startingInventory).
    pub firing_interval_ms: f32,
    pub ammo: String,
    /// idDeclWeapon::skillSettings_t per skill (+0x7f0, 0x24 bytes each): min / maxRepullTriggerInterval,
    /// min / maxBurstDuration (raw game ticks: the fire control adds them to 960/s tick times).
    pub repull: [(i32, i32); 5],
    pub burst: [(i32, i32); 5],
}

impl SoldierParms {
    pub fn load(db: &DeclDb, entity: &str) -> Result<SoldierParms> {
        let e = db.get("entitydef", entity).with_context(|| format!("entityDef {entity}"))?;
        let edit = e.block("edit").context("entityDef without edit")?;
        let bdecl = edit.str("aiEditable.behaviors.decl").context("no behaviors decl")?;
        let bh = db.get("aibehavior", bdecl).with_context(|| format!("aiBehavior {bdecl}"))?;
        let bh = bh.block("edit").context("aiBehavior without edit")?;
        let fsm = db.raw("aifsmmanager", SOLDIER_FSM).with_context(|| format!("aiFSMManager {SOLDIER_FSM}"))?;
        let chance = |b: Option<&idres::decl::Block>| b.and_then(|b| b.f32("requiredConditions.item[0].object.chance")).unwrap_or(1.0);
        let min_secs = |b: Option<&idres::decl::Block>| b.and_then(|b| b.f32("minSecondsInState.value")).unwrap_or(0.0);
        let do_something = fsm_link(&fsm, "p_idle_do_something", "reposition_sequence", None);
        let iss_rs = fsm_link(&fsm, "idle_shoot_sequence", "reposition_sequence", None);
        let rs_idle = fsm_link(&fsm, "reposition_sequence", "oc_idle_primary", None);
        let shoot_back = fsm_link(&fsm, "plasma_shoot_idle", "plasma_idle_default", None);
        let charge = fsm_link(&fsm, "p_immediate", "oc_transient_charge", None).context("no charge link")?;
        let charge_out = fsm_link(&fsm, "oc_transient_charge", "oc_idle_primary", Some("Shared_DefaultUse"));
        let _ = fsm_state(&fsm, "plasma_shoot_idle").context("no plasma_shoot_idle")?;
        let pos = edit.str("aiConstants.positioningParms.item[0]").unwrap_or("hellsoldier/plasma");
        let pp = db.get("aipositioningparms", pos).with_context(|| format!("aiPositioningParms {pos}"))?;
        let pd = pp.block("edit.data");
        let pf = |k: &str| pd.and_then(|d| d.f32(k)).unwrap_or(0.0);
        // startingInventory: the first weapon and its ammo (weapon/zion/ai/hellified_soldier/plasma).
        let weapon = edit.str("startingInventory.item[0].inventoryDecl").unwrap_or("weapon/zion/ai/hellified_soldier/plasma").to_string();
        let w = db.get("weapon", &weapon).with_context(|| format!("weapon {weapon}"))?;
        let w = w.block("edit").context("weapon without edit")?;
        Ok(SoldierParms {
            reposition_chance: chance(do_something),
            shoot_to_reposition_secs: min_secs(iss_rs),
            shoot_to_reposition_chance: chance(iss_rs),
            shoot_to_reposition_debounce: iss_rs.and_then(|b| b.f32("timers_required.item[0].range.minRange")).unwrap_or(0.0),
            reposition_to_idle_secs: min_secs(rs_idle),
            shoot_min_secs: min_secs(shoot_back),
            charge_range: charge.f32("range").unwrap_or(0.0),
            charge_timer_secs: charge.f32("timers_required.item[0].range.minRange").unwrap_or(0.0),
            charge_max_secs: min_secs(charge_out),
            min_enemy_dist: pf("minDistanceFromEnemy"),
            max_enemy_dist: pf("maxDistanceFromEnemy"),
            min_optimal_enemy_dist: pf("minOptimalDistanceFromEnemy"),
            max_optimal_enemy_dist: pf("maxOptimalDistanceFromEnemy"),
            role: bh.str("rolePreferenceOrder.item[0]").unwrap_or("ROLE_NONE").to_string(),
            firing_interval_ms: w.f32("firingInterval").unwrap_or(0.0),
            ammo: w.str("initialAmmoDecl").unwrap_or("").to_string(),
            repull: skill(w, "RepullTriggerInterval"),
            burst: skill(w, "BurstDuration"),
        })
    }
}

/// skillSettings[i].min<key> / max<key> of a weapon decl.
fn skill(w: &idres::decl::Block, key: &str) -> [(i32, i32); 5] {
    let mut v = [(0, 0); 5];
    for (i, x) in v.iter_mut().enumerate() {
        let f = |k: &str| w.f32(&format!("skillSettings.skillSettings[{i}].{k}{key}")).unwrap_or(0.0) as i32;
        *x = (f("min"), f("max"));
    }
    v
}

/// aiFireMode_t of the active state (enum table 0x14351dc90).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireMode {
    /// AIFIREMODE_HOLDFIRE (0).
    Hold,
    /// AIFIREMODE_WEAPON_EXPLICIT_RELEASE (1): the anim's ae_aIPullTrigger / ae_aIReleaseTrigger hold the trigger.
    ExplicitRelease,
    /// AIFIREMODE_FIREWHENREADY (3): the fire control pulls and releases on its own timers. It is
    /// idOpenCombat_AttackIdle's default idleFireMode (ctor 0x1414c6940 stores 3 at +0x108).
    WhenReady,
}

/// endNode of every `link` block of an attack graph decl.
fn link_ends(b: &idres::decl::Block, out: &mut Vec<String>) {
    for (k, v) in &b.items {
        let idres::decl::Value::Block(c) = v else { continue };
        if k == "link" {
            if let Some(e) = c.str("endNode") {
                out.push(e.to_string());
            }
        } else {
            link_ends(c, out);
        }
    }
}

/// The game's LCG (gameLocal +0x285be8 / 0x50b7d use: x = x * 0x19660d + 0x3c6ef35f, value (x >> 10) & 0x7fff).
/// INTERIM: a per-demon seed (the global generator's state is not reproduced).
#[derive(Debug, Clone, Copy)]
pub struct Lcg(pub u32);

impl Lcg {
    pub fn next_unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(0x19660d).wrapping_add(0x3c6ef35f);
        ((self.0 >> 10) & 0x7fff) as f32 * 3.051851e-05
    }
}

pub struct SoldierBrain {
    pub base: Brain,
    pub state: SoldierState,
    pub parms: SoldierParms,
    pub projectile: Arc<AiProjectileDef>,
    pub run: Locomotion,
    pub sprint: Locomotion,
    /// plasma_idle child: shooting (plasma_shoot_idle) or not (plasma_idle_default), and since when.
    pub shooting: bool,
    shoot_since: i64,
    /// The rifle's trigger and the next shot (game ticks).
    pub trigger: bool,
    next_shot: i64,
    /// Fire control (AI +0x444f*8) timers for FIREWHENREADY: release time of the current burst and the next pull.
    release_at: i64,
    next_pull: i64,
    /// Skill index: game difficulty + AI +0xbea0 - 1 clamped to 0..4 (0x1403d4930). INTERIM: AI +0xbea0 = 1.
    pub skill: usize,
    timers: HashMap<&'static str, i64>,
    state_since: i64,
    rng: Lcg,
}

impl SoldierBrain {
    pub fn load(db: &DeclDb, entity: &str, seed: u32) -> Result<SoldierBrain> {
        let mut base = Brain::load_with(db, entity, COMBAT_SUBWEB)?;
        // A first attack comes from the graph's entry nodes: enabled nodes no link leads into (ai/plasma_soldier
        // "default": normal_end_lff / normal_end_rff; from_old is disabled). hit_2_* / hit_3_* are combo
        // continuations reached through the links after an attack's ae_makeAttackCheck. INTERIM: combos are not
        // ported, and the lff / rff choice (by the foot that is forward) is not decoded: both entry nodes, in order.
        let graph = db.raw("attackgraph", &base.def.attacks.name).ok();
        let mut ends = Vec::new();
        if let Some(g) = &graph {
            link_ends(g, &mut ends);
        }
        for sg in &mut base.def.attacks.subgraphs {
            sg.attacks = sg.nodes.iter().filter(|n| n.enabled && !ends.contains(&n.name)).flat_map(|n| n.attacks.iter().cloned()).collect();
        }
        let parms = SoldierParms::load(db, entity)?;
        let projectile = Arc::new(AiProjectileDef::load(db, &parms.ammo)?);
        let c = db.container();
        let web_src = c.read_by_name(&format!("generated/decls/animweb/{}.decl", base.def.anim_web))?;
        let web = idres::animweb::AnimWeb::parse(&String::from_utf8_lossy(&web_src))?;
        let loco = |node: &str| -> Locomotion {
            let speed = web
                .sub_web(COMBAT_SUBWEB)
                .and_then(|s| s.node(node))
                .and_then(|n| n.trees.iter().flat_map(|t| t.anims.iter()).find(|a| a.coordinate.first() == Some(&0.0)).or_else(|| n.trees.first()?.anims.first()))
                .and_then(|a| super::root_speed(c, &a.name))
                .map_or(0.0, |s| s.0);
            Locomotion { walk_node: format!("{}/{COMBAT_SUBWEB}/{node}", base.def.anim_web), idle_node: base.loco.idle_node.clone(), walk_speed: speed, body_turn_rate: base.loco.body_turn_rate }
        };
        let (run, sprint) = (loco("run"), loco("sprint"));
        Ok(SoldierBrain {
            base,
            state: SoldierState::Relaxed,
            parms,
            projectile,
            run,
            sprint,
            shooting: false,
            shoot_since: 0,
            trigger: false,
            next_shot: 0,
            release_at: 0,
            next_pull: 0,
            skill: 1,
            timers: HashMap::new(),
            state_since: 0,
            rng: Lcg(seed),
        })
    }

    fn secs(&self, since: i64) -> f32 {
        (self.base.now - since) as f32 / TICKS_PER_SEC as f32
    }

    /// A named timer (set to `now` when it ran out last) has been expired for `secs`.
    fn timer_for(&self, name: &str, secs: f32) -> bool {
        self.timers.get(name).is_none_or(|&t| self.secs(t) >= secs)
    }

    /// Shared_PositionDistanceValid 0x1405f0c50 with aiPositioningParms hellsoldier/plasma (see the Imp's).
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

    /// Shared_ChargeMelee 0x1405ebeb0 (range 180): CONFIRMED and seen, last known position within range.
    fn should_charge_melee(&self, body: &Body, target: &Target) -> bool {
        target.alive
            && self.base.sense.awareness == Awareness::Confirmed
            && self.base.sense.last_visible.is_some()
            && self.base.sense.last_known_pos.distance_squared(body.origin) <= self.parms.charge_range * self.parms.charge_range
    }

    /// p_immediate's checks in orderIndex order (module docs).
    fn immediate(&self, body: &Body, target: &Target) -> Option<SoldierState> {
        if self.base.choose_attack(body, target, "default").is_some() {
            return Some(SoldierState::Melee);
        }
        // #1 Shared_ShouldCharge: SHOULD_MOVE only in the charge group role.
        // #4 HasValidMinorPositions NO (INTERIM: no position data) -> SuperiorPositionAvailable NO ->
        // PositionDistanceValid(ideal) NO_GREATER_THAN -> EncounterRole exclude ROLE_DEFEND -> advance.
        if self.position_distance(body, target, true) == DistanceCheck::Greater && self.parms.role != "ROLE_DEFEND" {
            return Some(SoldierState::Advance);
        }
        // #7 Shared_ChargeMelee with timer last_charge_finish >= 6 s.
        if self.timer_for("last_charge_finish", self.parms.charge_timer_secs) && self.should_charge_melee(body, target) {
            return Some(SoldierState::Charge);
        }
        None
    }

    fn transition(&mut self, body: &Body, target: &Target) -> Option<SoldierState> {
        let confirmed = self.base.sense.awareness == Awareness::Confirmed && target.alive;
        let in_state = self.secs(self.state_since);
        match self.state {
            // plasmacombat oc_bookkeeping -> oc_default -> oc_idle -> oc_destroy_type -> oc_destroy_primary.
            SoldierState::Relaxed => confirmed.then_some(SoldierState::Idle),
            SoldierState::Sighted => {
                if self.base.choose_attack(body, target, "default").is_some() {
                    return Some(SoldierState::Melee);
                }
                // Shared_ChildFinished: INTERIM as the Possessed (faces the enemy).
                let face = wrap180(yaw_to(target.origin - body.origin) - body.yaw).abs();
                (face <= self.base.def.movement.alignment_tolerance).then_some(SoldierState::Idle)
            }
            SoldierState::Idle => {
                if self.base.sense.newly_aware {
                    return Some(SoldierState::Sighted);
                }
                if !confirmed {
                    return None;
                }
                // p_idle_do_something: #0 RandomChance -> reposition_sequence, #1 -> idle_shoot_sequence.
                if self.rng.next_unit() < self.parms.reposition_chance {
                    Some(SoldierState::RepositionSequence)
                } else {
                    Some(SoldierState::ShootSequence)
                }
            }
            SoldierState::ShootSequence => {
                if let Some(s) = self.immediate(body, target) {
                    return Some(s);
                }
                // #2 -> reposition_sequence: minSecondsInState 5, RandomChance 0.5, debounce timer (set on evaluate).
                if in_state >= self.parms.shoot_to_reposition_secs && self.timer_for("iss_to_rs_debounce", self.parms.shoot_to_reposition_debounce) {
                    self.timers.insert("iss_to_rs_debounce", self.base.now);
                    if self.rng.next_unit() < self.parms.shoot_to_reposition_chance {
                        return Some(SoldierState::RepositionSequence);
                    }
                }
                None
            }
            SoldierState::RepositionSequence => {
                if let Some(s) = self.immediate(body, target) {
                    return Some(s);
                }
                (in_state >= self.parms.reposition_to_idle_secs).then_some(SoldierState::Idle)
            }
            SoldierState::Melee => self.base.attack_done().then_some(SoldierState::Idle),
            SoldierState::Charge => {
                if self.base.choose_attack(body, target, "default").is_some() {
                    Some(SoldierState::Melee)
                } else if in_state >= self.parms.charge_max_secs || self.base.reached(body, target) {
                    Some(SoldierState::Idle)
                } else {
                    None
                }
            }
            SoldierState::Advance => {
                if self.base.choose_attack(body, target, "default").is_some() {
                    Some(SoldierState::Melee)
                } else {
                    // Shared_ReachedViablePosition (INTERIM: back inside the ideal distance).
                    (self.position_distance(body, target, true) != DistanceCheck::Greater).then_some(SoldierState::Idle)
                }
            }
        }
    }

    fn enter(&mut self, s: SoldierState, body: &Body, target: &Target, out: &mut AiOutput) {
        let from = self.state;
        // timers_setOnExit last_charge_finish (plasma_melee, oc_transient_charge).
        if matches!(from, SoldierState::Melee | SoldierState::Charge) {
            self.timers.insert("last_charge_finish", self.base.now);
        }
        self.state = s;
        self.state_since = self.base.now;
        self.shooting = false;
        out.events.push(AiEvent::DeclStateChanged { from: from.decl_name(), to: s.decl_name() });
        match s {
            SoldierState::Sighted => self.base.sense.newly_aware = false,
            SoldierState::Melee => {
                self.base.start_attack(body, target, "default", out);
            }
            _ => {}
        }
    }

    /// One game tick.
    pub fn tick(&mut self, world: &dyn World, body: &Body, target: &Target, now: i64, dt: f32) -> AiOutput {
        self.base.now = now;
        let mut out = AiOutput { yaw: body.yaw, ..Default::default() };
        if !body.alive || body.in_pain {
            self.trigger = false;
            return out;
        }
        self.base.sense_tick(world, body, target, now, dt, &mut out);
        self.base.moving = if matches!(self.state, SoldierState::Charge | SoldierState::Advance) { Moving::Running } else { Moving::Stopped };
        for _ in 0..5 {
            match self.transition(body, target) {
                Some(s) => self.enter(s, body, target, &mut out),
                None => break,
            }
        }
        // plasma_idle (child of idle_shoot_sequence): #0 idAICondition_Shared_TargetVisible -> plasma_shoot_idle;
        // back after minSecondsInState 1 once not visible. INTERIM: visible = seen within the last 1000 ticks.
        if self.state == SoldierState::ShootSequence {
            let visible = self.base.sense.is_visible(now);
            if !self.shooting && visible {
                self.shooting = true;
                self.shoot_since = now;
            } else if self.shooting && !visible && self.secs(self.shoot_since) >= self.parms.shoot_min_secs {
                self.shooting = false;
            }
        }
        self.fire_control(body, target);
        let web = self.base.def.anim_web.clone();
        let to_target = target.origin - body.origin;
        match self.state {
            SoldierState::Relaxed => out.web_node = Some(format!("{web}/{RELAXED_SUBWEB}/idle")),
            SoldierState::Idle | SoldierState::Sighted | SoldierState::ShootSequence | SoldierState::RepositionSequence => {
                // idAISnippet_Shared_OverrideIdleAnim idleAnim rifle_combat/shoot while plasma_shoot_idle runs.
                out.web_node = Some(if self.shooting { format!("{web}/{COMBAT_SUBWEB}/shoot") } else { self.base.loco.idle_node.clone() });
                // INTERIM: the aim / focus turns the body in place at the body turn rate.
                out.yaw = self.base.turn_toward(body.yaw, yaw_to(to_target), dt);
            }
            SoldierState::Melee => self.base.attack_tick(&mut out),
            SoldierState::Charge | SoldierState::Advance => {
                // idShared_ChargeMelee enter 0x1405e4e50 sets walk state 3 (INTERIM: sprint), advance RUNNING.
                let loco = if self.state == SoldierState::Charge { self.sprint.clone() } else { self.run.clone() };
                let keep = std::mem::replace(&mut self.base.loco, loco);
                self.base.walk(world, body, target, dt, &mut out);
                self.base.loco = keep;
            }
        }
        out
    }

    /// The fire mode of the current state: oc_idle_primary HOLDFIRE, plasma_shoot_idle WEAPON_EXPLICIT_RELEASE,
    /// plasma_idle_default / plasma_reposition_default (AttackIdle default) and oc_advance_to_enemy (fireMode)
    /// FIREWHENREADY. INTERIM: HOLDFIRE in the sighted, melee and charge states (not AttackIdle; their fire mode
    /// handling is not decoded).
    pub fn fire_mode(&self) -> FireMode {
        match self.state {
            SoldierState::ShootSequence if self.shooting => FireMode::ExplicitRelease,
            SoldierState::ShootSequence | SoldierState::RepositionSequence | SoldierState::Advance => FireMode::WhenReady,
            _ => FireMode::Hold,
        }
    }

    /// The fire control's random range: gameLocal LCG (x >> 10 & 0x7fff) % (max - min) + min, min when equal.
    fn rand_range(&mut self, r: (i32, i32)) -> i64 {
        self.rng.0 = self.rng.0.wrapping_mul(0x19660d).wrapping_add(0x3c6ef35f);
        let span = r.1 - r.0;
        let v = if span <= 0 { 0 } else { ((self.rng.0 >> 10) & 0x7fff) as i32 % span };
        (r.0 + v) as i64
    }

    /// The fire control (AI +0x444f*8) for FIREWHENREADY: pull (0x1404437c0) when the next-pull time has passed,
    /// the target is visible and the aim is on it; hold for skillSettings min..maxBurstDuration (0x1403d4930);
    /// release (0x140443a90) and wait min..maxRepullTriggerInterval (0x1403d5660). CheckPullTrigger 0x140442540
    /// requires the actual aim within dot 0.9 of the desired aim (INTERIM: the body yaw against the target
    /// direction) and the target seen within the lost-sight time (static table 0x1420449e0: 20000 ticks).
    /// INTERIM: the weaponUseBehaviors minimum hold and the blocked-shot early release are not ported.
    fn fire_control(&mut self, body: &Body, target: &Target) {
        let now = self.base.now;
        match self.fire_mode() {
            FireMode::Hold => {
                self.trigger = false;
                return;
            }
            FireMode::ExplicitRelease => return,
            FireMode::WhenReady => {}
        }
        if self.trigger {
            if now >= self.release_at {
                self.trigger = false;
                self.next_pull = now + self.rand_range(self.parms.repull[self.skill]);
            }
            return;
        }
        let f = Vec3::new(body.yaw.to_radians().cos(), body.yaw.to_radians().sin(), 0.0);
        let to = target.origin - body.origin;
        let aimed = f.dot(Vec3::new(to.x, to.y, 0.0).normalize_or_zero()) >= 0.9;
        let seen = self.base.sense.last_visible.is_some_and(|t| now - t < 20000) && self.base.sense.is_visible(now);
        if now >= self.next_pull && aimed && seen && target.alive {
            self.trigger = true;
            self.release_at = now + self.rand_range(self.parms.burst[self.skill]);
            self.next_shot = self.next_shot.max(now);
        }
    }

    /// ae_aIPullTrigger / ae_aIReleaseTrigger (AnimEvent_AIPullTrigger 0x1403e5640 -> fire control pull
    /// 0x1404437c0) in WEAPON_EXPLICIT_RELEASE mode; `now` in game ticks.
    pub fn set_trigger(&mut self, pulled: bool, now: i64) {
        if self.fire_mode() != FireMode::ExplicitRelease {
            return;
        }
        if pulled && !self.trigger {
            self.next_shot = self.next_shot.max(now);
        }
        self.trigger = pulled;
    }

    /// Shots due at game tick `now`: one every weapon firingInterval while the trigger is held (FinishFire's
    /// 960 * ms * 0.001 ticks: 120 for 125 ms). INTERIM: the first shot fires on the pull (idWeapon's full-auto
    /// timing for AI owners is not decoded).
    pub fn shots_due(&mut self, now: i64) -> u32 {
        let interval = (TICKS_PER_SEC as f32 * self.parms.firing_interval_ms * 0.001) as i64;
        if !self.trigger || interval <= 0 {
            return 0;
        }
        let mut n = 0;
        while self.next_shot <= now {
            n += 1;
            self.next_shot += interval;
        }
        n
    }

    /// One bolt from `muzzle` at the target's aim point `aim` (moving at `aim_vel`): the decoded lead
    /// ([`predict_aim`], lag 0), straight at the projectile speed. INTERIM: the weapon accuracy tables
    /// (weaponAccuracyList.accuracyVersusPlayer, e.g. assault_rifle_high_acc_long_dist) and the AI accuracy sample
    /// (AI +0x23db0, wandering per ai_difficulty_accuracyWanderSeconds) are not applied; no spread.
    pub fn shot_velocity(&self, muzzle: Vec3, aim: Vec3, aim_vel: Vec3) -> Option<Vec3> {
        let p = &self.projectile;
        let aim = predict_aim(aim, aim_vel, muzzle, p.speed, 0.0, 0.0);
        let d = aim - muzzle;
        (d.length() > 1e-3 && p.speed > 0.0).then(|| d.normalize() * p.speed)
    }
}

#[cfg(test)]
mod tests {
    //! Install-backed tests of the Possessed Soldier's decoded numbers and brain (skipped without an install).
    use super::*;
    use crate::ai::OpenWorld;
    use crate::demons::damage::AiDamageParms;
    use crate::demons::live::{player_damage, PlayerDamageScales};
    use crate::demons::projectile::{AiProjectile, Impact};

    const FRAME_TICKS: i64 = 16;
    const DT: f32 = 1.0 / 60.0;

    fn install() -> Option<crate::install::Install> {
        let doom = idres::find_install()?;
        Some(crate::install::load(&doom).expect("loading install"))
    }

    fn target_at(p: Vec3) -> Target {
        Target { origin: p, sight_point: p + Vec3::Z * 45.0, alive: true, half_width: 16.0 }
    }

    #[test]
    fn soldier_decls() {
        let Some(inst) = install() else { return };
        let p = SoldierParms::load(&inst.decls, SOLDIER).unwrap();
        assert_eq!((p.reposition_chance, p.shoot_to_reposition_secs, p.shoot_to_reposition_chance, p.shoot_to_reposition_debounce), (0.5, 5.0, 0.5, 1.0));
        assert_eq!((p.reposition_to_idle_secs, p.shoot_min_secs), (10.0, 1.0));
        assert_eq!((p.charge_range, p.charge_timer_secs, p.charge_max_secs), (180.0, 6.0, 5.0));
        assert_eq!((p.min_enemy_dist, p.max_enemy_dist, p.min_optimal_enemy_dist, p.max_optimal_enemy_dist), (384.0, 1024.0, 512.0, 768.0));
        assert_eq!(p.role, "ROLE_DEFEND");
        assert_eq!((p.firing_interval_ms, p.ammo.as_str()), (125.0, "ammo/zion/ai/hellified_soldier/plasma"));
        assert_eq!((p.repull, p.burst), ([(1000, 1300); 5], [(440, 660); 5]));
        let f = AiProjectileDef::load(&inst.decls, &p.ammo).unwrap();
        assert_eq!((f.name.as_str(), f.speed, f.gravity, f.parabolic), ("projectile/zion/ai/hellified_soldier/plasma", 1300.0, false, false));
        assert_eq!(f.damage_decl, "damage/zion/ai/hellified_soldier/plasma");
        // idPlayer::Damage: plasma 20 x playerDamageScale 0.25 x difficulty; rifle melee 40 x 0.25.
        let cvars = crate::config::CvarValues::default();
        let bolt = AiDamageParms::from_decl(&inst.decls, &f.damage_decl).unwrap();
        let by_diff: Vec<f32> = (0..5).map(|d| player_damage(&bolt, 600.0 * 600.0, 1.0, &PlayerDamageScales::load(&inst.decls, &cvars, d))).collect();
        assert_eq!(by_diff, [2.5, 5.0, 8.75, 15.0, 15.0]);
        let melee = AiDamageParms::from_decl(&inst.decls, "damage/zion/ai/hellified_soldier/weapon_melee").unwrap();
        assert_eq!(player_damage(&melee, 0.0, 1.0, &PlayerDamageScales::load(&inst.decls, &cvars, 1)), 10.0);
        let def = crate::demons::DemonDef::load(&inst.decls, SOLDIER).unwrap();
        assert_eq!(def.anim_web, "zion/characters/monsters/hellified_soldier");
        crate::demons::WebTags::load(inst.decls.container(), &def.anim_web).unwrap();
    }

    #[test]
    fn soldier_notices_and_fires_bursts() {
        let Some(inst) = install() else { return };
        let mut b = SoldierBrain::load(&inst.decls, SOLDIER, 1).unwrap();
        let w = OpenWorld { nav: None };
        let mut body = Body { origin: Vec3::ZERO, yaw: 0.0, alive: true, in_pain: false };
        let t = target_at(Vec3::new(600.0, 0.0, 0.0));
        let (mut shots, mut first_shot, mut trail) = (0, None, Vec::new());
        for i in 0..600 {
            let now = i * FRAME_TICKS;
            let o = b.tick(&w, &body, &t, now, DT);
            body.yaw = o.yaw;
            for e in &o.events {
                if let AiEvent::DeclStateChanged { to, .. } = e {
                    trail.push((i, *to));
                }
            }
            // The shooting anim's trigger events (rifle_combat/shoot: pull at frame 0) in explicit-release mode.
            if b.fire_mode() == FireMode::ExplicitRelease && !b.trigger {
                b.set_trigger(true, now);
            }
            let n = b.shots_due(now);
            if n > 0 && first_shot.is_none() {
                first_shot = Some((i, b.fire_mode()));
            }
            shots += n;
        }
        assert!(b.base.sense.awareness == Awareness::Confirmed, "{trail:?}");
        assert!(shots >= 4, "{shots} shots, {trail:?}");
        let (frame, mode) = first_shot.unwrap();
        assert!(frame < 120, "first shot at frame {frame} ({mode:?}) {trail:?}");
    }

    #[test]
    fn fire_control_and_bolt() {
        let Some(inst) = install() else { return };
        let mut b = SoldierBrain::load(&inst.decls, SOLDIER, 3).unwrap();
        // Explicit release: the anim holds the trigger, one bolt every 120 ticks (125 ms).
        b.state = SoldierState::ShootSequence;
        b.shooting = true;
        b.set_trigger(true, 1000);
        assert_eq!(b.shots_due(1000), 1);
        assert_eq!(b.shots_due(1119), 0);
        assert_eq!(b.shots_due(1480), 4);
        b.set_trigger(false, 1490);
        assert_eq!(b.shots_due(2000), 0);
        // A bolt from the muzzle flies straight at 1300 and hits the standing player.
        let t = target_at(Vec3::new(600.0, 0.0, 0.0));
        let mut world = crate::collision::World::default();
        world.add(crate::collision::Hull::cuboid(Vec3::new(-1000.0, -1000.0, -64.0), Vec3::new(1000.0, 1000.0, 0.0)));
        let muzzle = Vec3::new(20.0, 0.0, 60.0);
        let vel = b.shot_velocity(muzzle, t.sight_point, Vec3::ZERO).unwrap();
        assert!((vel.length() - 1300.0).abs() < 0.01);
        let player = (Vec3::new(584.0, -16.0, 0.0), Vec3::new(616.0, 16.0, 90.0));
        let mut p = AiProjectile { def: b.projectile.clone(), pos: muzzle, vel, age: 0.0, owner: 1, launch: muzzle };
        let hit = (0..120).find_map(|_| p.step(&world, 1066.0, player, DT));
        assert!(matches!(hit, Some(Impact::Player { .. })), "{hit:?}");
    }

    #[test]
    fn soldier_melees_up_close() {
        let Some(inst) = install() else { return };
        let mut b = SoldierBrain::load(&inst.decls, SOLDIER, 7).unwrap();
        let w = OpenWorld { nav: None };
        let mut body = Body { origin: Vec3::ZERO, yaw: 0.0, alive: true, in_pain: false };
        // 160 units: inside Shared_ChargeMelee's 180; the attack graph's melee (0..128, ANIM_DELTA) follows.
        let t = target_at(Vec3::new(160.0, 0.0, 0.0));
        let mut attack = None;
        for i in 0..240 {
            let o = b.tick(&w, &body, &t, i * FRAME_TICKS, DT);
            body.step(&o, DT);
            attack = attack.or_else(|| o.events.iter().find_map(|e| match e {
                AiEvent::AttackStart { attack, .. } => Some(attack.clone()),
                _ => None,
            }));
            if attack.is_some() {
                break;
            }
        }
        // (The test body has no repulsors: once charging it can overshoot, so any arc of the graph may fire.)
        assert!(attack.as_deref().is_some_and(|a| a.starts_with("melee_")), "{attack:?}");
        assert_eq!(b.state, SoldierState::Melee);
    }
}

