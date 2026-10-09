//! Headless demon AI ("brain") for the Possessed (`ai/zombie/scientist`): perception, the decl-driven behaviour FSM,
//! navigation on the map's AAS2 data, locomotion requests to the anim web and melee attack events.
//!
//! Everything is read from the user's install (entityDef, aiFSMManager / attackGraph / md6Def / damage decls, the
//! map's `.baas_monster48`) and the exe analysis in gamedata/re/AI.md; values not yet decoded are marked INTERIM and
//! listed in UNVERIFIED.md. Game time is in game ticks (idTypesafeTime<int, 960>: 960 per second).
//!
//! Per tick the caller passes the world ([`World`]: line of sight + optional nav), the player's position and the
//! demon's pose; [`Brain::tick`] returns the requested move direction / speed, body facing, anim web node + scalars
//! and the attack events (melee sphere-trace windows with the decl damage).
//!
//! FSM (AI.md 4): before the player is CONFIRMED the AI is in the relaxed behaviour; once CONFIRMED
//! (behavior_shared shared_combat_check1: Shared_SocialRelationAvailable awareness_min AIAWARE_CONFIRMED) it runs
//! zombie_combat -> zombie_engage_primary, whose states and transitions (in orderIndex order) are:
//!   Default:  #1 COMBAT_ShouldSightEnemy -> Sighted, #2 ZC_ShouldMoveTowardEnemy -> MoveTowardEnemy,
//!             #3 OC_AttackIdle -> AttackIdle
//!   MoveTowardEnemy: #5 Shared_ShouldAttack -> Attack, #6 OC_MoveFailedAny -> Wander, #8/#9 OC_ReachedEnemy -> AttackIdle
//!   AttackIdle: #1 OC_EnemyIsDead -> Done, #2 Shared_ShouldAttack -> Attack, #3 ZC_ShouldMoveTowardEnemy -> MoveTowardEnemy
//!   Attack -> AttackIdle (Shared_ChildFinished), Sighted -> Default (Shared_ChildFinished).

pub mod attack;
pub mod def;
pub mod imp;
pub mod soldier;
pub mod nav;
pub mod perception;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use glam::Vec3;
use idres::Container;
use idres::decldb::DeclDb;

pub use attack::{Attack, AttackGraph};
pub use def::{AiDef, MeleeWindow, Perception};
pub use nav::Nav;
pub use perception::{Awareness, SenseRecord, SightTest};

/// The Possessed.
pub const POSSESSED: &str = "ai/zombie/scientist";
/// Game ticks per second (idTypesafeTime<int, 960>).
pub const TICKS_PER_SEC: i64 = 960;

/// ZC_ShouldMoveTowardEnemy 0x140647790 -> ReachedEnemy 0x1404f0540 arguments (reach radius argument, max |dz|).
pub const REACHED_RADIUS_ARG: f32 = 32.0;
pub const REACHED_MAX_DZ: f32 = 128.0;
/// 0x1404f06d0(pm, 1, 1000): a failed move toward the enemy blocks ZC_ShouldMoveTowardEnemy for 1000 ticks.
pub const MOVE_FAIL_BLOCK_TICKS: i64 = 1000;

/// zombie_engage_primary states (zombiecombat.decl), plus the relaxed behaviour before combat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// FSM_relaxed (behavior_shared behaviors_relaxed_shared): idle until the player is CONFIRMED.
    Relaxed,
    /// idZombieCombat_Default [idShared_Default].
    Default,
    /// zombie_sighted [idCombat_SightedEnemy].
    Sighted,
    /// idZombieCombat_MoveTowardEnemy [idShared_MoveTowardEnemy].
    MoveTowardEnemy,
    /// idZombieCombat_AttackIdle [idOpenCombat_AttackIdle].
    AttackIdle,
    /// ZombieCombat_Attack [idShared_Attack].
    Attack,
    /// ZombieCombat_Wander [idWander_Random] (not ported: stands; INTERIM).
    Wander,
    /// idZombieCombat_Done [idAIStateDone].
    Done,
}

impl State {
    pub fn decl_name(self) -> &'static str {
        match self {
            State::Relaxed => "behaviors_relaxed_shared",
            State::Default => "idZombieCombat_Default",
            State::Sighted => "zombie_sighted",
            State::MoveTowardEnemy => "idZombieCombat_MoveTowardEnemy",
            State::AttackIdle => "idZombieCombat_AttackIdle",
            State::Attack => "ZombieCombat_Attack",
            State::Wander => "ZombieCombat_Wander",
            State::Done => "idZombieCombat_Done",
        }
    }
}

/// What the brain needs from the world each tick.
pub trait World {
    /// Line of sight for the visual sense (eye to target; the game traces against the collision world).
    fn line_of_sight(&self, from: Vec3, to: Vec3) -> bool;
    /// The map's nav data, if any (the firing range has none; then the AI walks straight at the target).
    fn nav(&self) -> Option<&Nav> {
        None
    }
}

/// An open world without walls.
pub struct OpenWorld<'a> {
    pub nav: Option<&'a Nav>,
}

impl World for OpenWorld<'_> {
    fn line_of_sight(&self, _from: Vec3, _to: Vec3) -> bool {
        true
    }
    fn nav(&self) -> Option<&Nav> {
        self.nav
    }
}

/// The demon's state the caller owns (its pose comes from root motion / physics).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub origin: Vec3,
    /// Body yaw, degrees (0 = +x, counter-clockwise).
    pub yaw: f32,
    pub alive: bool,
    /// The demon is in a pain / stagger / death reaction (alertCycle ac_pain / ac_stagger take over).
    pub in_pain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    /// The player's origin (feet).
    pub origin: Vec3,
    /// The point the visual sense tests: the origin plus the clip-bounds centre (0x14047de00; player: origin +
    /// pm_normalheight / 2).
    pub sight_point: Vec3,
    pub alive: bool,
    /// Max x / y half extent of the target clip bounds (player: pm_bboxwidth / 2 = 16 with default_sp.cfg).
    pub half_width: f32,
}

/// Brain events of one tick.
#[derive(Debug, Clone, PartialEq)]
pub enum AiEvent {
    /// The player became CONFIRMED (EM_NEWLY_AWARE).
    Noticed,
    StateChanged { from: State, to: State },
    /// Shared_ShouldAttack picked an attack (TRANSCODE_ATTACK); the web plays `via_node` then `dest_node`.
    AttackStart { attack: String, via_node: String, dest_node: String, anim: String },
    /// ae_startSphereModelTrace*: sweep the md6Def joint group's spheres and damage what they touch.
    MeleeTraceStart { joint_group: String, damage_decl: String, damage: f32, player_damage_scale: f32 },
    /// ae_endSphereModelTrace.
    MeleeTraceEnd,
    /// A demon FSM changed state (decl state names; demons with their own FSM such as the Imp).
    DeclStateChanged { from: &'static str, to: &'static str },
    /// idShared_ThrowProjectile entered: the web plays `node` (then `dest_node`, where its end-relative edge
    /// returns); its ae_launchItem launches `item` (an ammo decl).
    ThrowStart { node: String, dest_node: String, item: String },
}

/// Locomotion / anim web request of one tick.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AiOutput {
    pub state: Option<State>,
    /// Unit move direction (xy), zero when standing.
    pub move_dir: Vec3,
    /// Requested ground speed (units/s): the walk anim's root-motion speed while walking (INTERIM: the game moves
    /// the AI by the playing anim's root motion; this is the forward walk anim's average).
    pub move_speed: f32,
    /// Desired body yaw (degrees) after this tick's turn.
    pub yaw: f32,
    /// Full web node path to play ("zion/characters/monsters/zombie/hands_combat/walk").
    pub web_node: Option<String>,
    /// Web scalars to set (bodyMoveAngle while walking).
    pub scalars: Vec<(String, f32)>,
    pub events: Vec<AiEvent>,
}

struct ActiveAttack {
    attack: Attack,
    /// The anim the via node plays (for traces).
    #[allow(dead_code)]
    anim: String,
    start: i64,
    /// Seconds.
    duration: f32,
    frame_rate: f32,
    windows: Vec<MeleeWindow>,
    fired: Vec<(bool, bool)>,
}

/// Locomotion constants from the install.
#[derive(Debug, Clone, PartialEq)]
pub struct Locomotion {
    /// Web node of WALKSTATE_WALKING in the combat subweb (aiMovementGraph zombie subgraph_walk node "walk",
    /// MOVE_NODE_DEFAULT; idShared_Default animwebSubweb AISUBWEB_COMBAT = hands_combat).
    pub walk_node: String,
    pub idle_node: String,
    /// Forward walk anim (hands_combat/walk alias coordinate 0) root-motion speed, units/s.
    pub walk_speed: f32,
    /// Body turn rate, degrees/s: INTERIM, cvar ai_maxBodyTurnRate (360) until the move FSM's turn code is decoded.
    pub body_turn_rate: f32,
}

pub struct Brain {
    pub def: AiDef,
    pub loco: Locomotion,
    pub state: State,
    pub sense: SenseRecord,
    /// Last tick time.
    pub now: i64,
    path: Vec<Vec3>,
    path_goal: Option<Vec3>,
    move_failed_at: Option<i64>,
    attack: Option<ActiveAttack>,
    /// Anim path -> (duration s, frame rate).
    anim_durations: std::collections::HashMap<String, (f32, f32)>,
    /// Attack via node -> the anim it plays.
    via_anims: Vec<(String, String)>,
    /// Anim -> horizontal root-motion delta length (ATTACK_VALIDATOR_ANIM_DELTA).
    anim_deltas: std::collections::HashMap<String, f32>,
    last_sight: SightTest,
    /// The AI's move state for the attack prefilter (usableWhileStopped / Walking / Running).
    pub moving: Moving,
}

/// Move state the attack prefilter tests (attack rejections "AI not moving / running / walking / not in move
/// cycle", 0x140529af0 and siblings; AI.md 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Moving {
    #[default]
    Stopped,
    Walking,
    /// WALKSTATE_RUNNING and WALKSTATE_SPRINTING.
    Running,
}

/// Root-motion speed of an anim: the origin joint's (joint 0) translation from the first to the last key over the
/// anim's duration.
pub fn root_speed(c: &Container, anim: &str) -> Option<(f32, f32, f32)> {
    let b = c.read_by_name(&idres::animweb::anim_resource(anim)).ok()?;
    let a = idres::md6anim::Md6Anim::parse(&b).ok()?;
    let dur = a.duration_secs();
    let ch = a.trans.joints.iter().position(|&j| j == 0);
    let speed = ch
        .and_then(|ch| {
            let k = &a.trans.keys[ch];
            let (f, l) = (k.first()?.1, k.last()?.1);
            let d = ((l[0] - f[0]).powi(2) + (l[1] - f[1]).powi(2)).sqrt();
            (dur > 0.0).then(|| d / dur)
        })
        .unwrap_or(0.0);
    Some((speed, dur, a.frame_rate as f32))
}

pub fn wrap180(mut d: f32) -> f32 {
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    d
}

pub fn yaw_to(v: Vec3) -> f32 {
    v.y.atan2(v.x).to_degrees()
}

impl Brain {
    pub fn load(db: &DeclDb, entity: &str) -> Result<Brain> {
        Self::load_with(db, entity, "hands_combat")
    }

    /// As [`Brain::load`] with the sub-web that plays AISUBWEB_COMBAT (`rifle_combat` for the Possessed Soldier).
    pub fn load_with(db: &DeclDb, entity: &str, combat: &str) -> Result<Brain> {
        let def = AiDef::load(db, entity)?;
        let c = db.container();
        let web_src = c.read_by_name(&format!("generated/decls/animweb/{}.decl", def.anim_web))?;
        let web = idres::animweb::AnimWeb::parse(&String::from_utf8_lossy(&web_src))?;
        // hands_combat/walk: lerp(blendy1(bodyMoveAngle, ...)); the coordinate-0 alias is the forward walk.
        let walk = web.sub_web(combat).and_then(|s| s.node("walk")).with_context(|| format!("no {combat}/walk"))?;
        let fwd = walk
            .trees
            .iter()
            .flat_map(|t| t.anims.iter())
            .find(|a| a.coordinate.first() == Some(&0.0))
            .context("no forward walk alias")?;
        let (walk_speed, _, _) = root_speed(c, &fwd.name).context("walk anim")?;
        let mut anim_durations = std::collections::HashMap::new();
        let mut anim_deltas = std::collections::HashMap::new();
        for (anim, _) in def.melee_windows.iter() {
            if let Some((_, d, fr)) = root_speed(c, anim) {
                anim_durations.insert(anim.clone(), (d, fr));
            }
        }
        for a in def.attacks.subgraphs.iter().flat_map(|s| s.attacks.iter()) {
            if let Some(anim) = first_anim(&web, &def.anim_web, &a.via_node) {
                if let Some((speed, d, fr)) = root_speed(c, &anim) {
                    anim_deltas.insert(anim.clone(), speed * d);
                    anim_durations.insert(anim, (d, fr));
                }
            }
        }
        let loco = Locomotion {
            walk_node: format!("{}/{combat}/walk", def.anim_web),
            idle_node: format!("{}/{combat}/idle", def.anim_web),
            walk_speed,
            body_turn_rate: 360.0,
        };
        let via: Vec<(String, String)> = def
            .attacks
            .subgraphs
            .iter()
            .flat_map(|s| s.attacks.iter())
            .filter_map(|a| Some((a.via_node.clone(), first_anim(&web, &def.anim_web, &a.via_node)?)))
            .collect();
        let mut b = Brain {
            def,
            loco,
            state: State::Relaxed,
            sense: SenseRecord::default(),
            now: 0,
            path: Vec::new(),
            path_goal: None,
            move_failed_at: None,
            attack: None,
            anim_durations,
            via_anims: via,
            anim_deltas,
            last_sight: SightTest::default(),
            moving: Moving::Stopped,
        };
        b.state = State::Relaxed;
        Ok(b)
    }
}

/// The first anim alias of a web node (the alias index 0 the blendEq `anim[ #meleeIndex ]` plays by default).
pub fn first_anim(web: &idres::animweb::AnimWeb, web_name: &str, node: &str) -> Option<String> {
    let (sub, state) = node.strip_prefix(&format!("{web_name}/"))?.split_once('/')?;
    web.sub_web(sub)?.node(state)?.trees.first()?.anims.first().map(|a| a.name.clone())
}

/// INTERIM waypoint acceptance radius (xy) of the path follower.
pub const WAYPOINT_RADIUS_INTERIM: f32 = 16.0;
/// INTERIM: re-path when the goal moved this far since the last path.
pub const REPATH_DIST_INTERIM: f32 = 64.0;

impl Body {
    /// Test helper: applies a tick's locomotion as root motion would (move along `move_dir` at `move_speed`, face `yaw`).
    pub fn step(&mut self, out: &AiOutput, dt: f32) {
        self.origin += out.move_dir * out.move_speed * dt;
        self.yaw = out.yaw;
    }
}

impl Brain {
    /// 0x1404eb9c0(pm, enemy, 32): the AI clip bounds radius + the enemy clip bounds radius (0 unless the enemy is
    /// an actor) + 32, where a bounds radius is 0x140287600 = max of the x / y half extents.
    pub fn reach_radius(&self, target: &Target) -> f32 {
        self.def.clip_half_width + target.half_width + REACHED_RADIUS_ARG
    }

    pub fn reached(&self, body: &Body, target: &Target) -> bool {
        let d = target.origin - body.origin;
        d.z.abs() <= REACHED_MAX_DZ && d.length() < self.reach_radius(target)
    }

    /// Shared_ShouldAttack 0x1405f1a40 for an attack graph category: the target must be CONFIRMED and visible
    /// (seen within 1000 ticks); then the first valid attack ([`attack_valid`], decoded) of the sub graph, toward
    /// the target last known position. INTERIM: list order picks among several valid attacks (the attack
    /// component pick 0x140530a70 with weights / timers is not decoded; the Possessed attacks all weigh 1).
    pub fn choose_attack(&self, body: &Body, target: &Target, category: &str) -> Option<&Attack> {
        if self.sense.awareness != Awareness::Confirmed || !self.sense.is_visible(self.now) || !target.alive {
            return None;
        }
        let sg = self.def.attacks.subgraph(category)?;
        let to = self.sense.last_known_pos - body.origin;
        sg.attacks.iter().find(|a| {
            // INTERIM: the usable* flags against the move state (the prefilter's exact tests are not decoded).
            let usable = match self.moving {
                Moving::Stopped => a.usable_stopped,
                Moving::Walking => a.usable_walking,
                Moving::Running => a.usable_running,
            };
            if !usable {
                return false;
            }
            let delta = self.via_anims.iter().find(|(v, _)| *v == a.via_node).and_then(|(_, an)| self.anim_deltas.get(an)).copied().unwrap_or(0.0);
            attack_valid(a, body.yaw, to, delta)
        })
    }

    fn enter(&mut self, s: State, body: &Body, target: &Target, out: &mut AiOutput) {
        let from = self.state;
        self.state = s;
        out.events.push(AiEvent::StateChanged { from, to: s });
        match s {
            // idCombat_SightedEnemy enter 0x14060ac50: clears EM_NEWLY_AWARE, stops the move.
            State::Sighted => {
                self.sense.newly_aware = false;
                self.path.clear();
            }
            // idShared_MoveTowardEnemy enter 0x1405e6310: move goal = the enemy.
            State::MoveTowardEnemy => {
                self.path.clear();
                self.path_goal = None;
            }
            State::Attack => {
                self.start_attack(body, target, "default", out);
            }
            _ => {}
        }
    }

    /// idShared_Attack: plays the attack Shared_ShouldAttack picked from the attack graph `category` (AttackStart,
    /// then the via node's melee windows through [`Brain::attack_tick`]). False when no attack is valid.
    pub fn start_attack(&mut self, body: &Body, target: &Target, category: &str, out: &mut AiOutput) -> bool {
        let started = if let Some(a) = self.choose_attack(body, target, category).cloned() {
            let anim = self.via_anims.iter().find(|(v, _)| *v == a.via_node).map(|(_, an)| an.clone()).unwrap_or_default();
            let (duration, frame_rate) = self.anim_durations.get(&anim).copied().unwrap_or((0.0, 30.0));
            let windows = self.def.melee_windows.get(&anim).cloned().unwrap_or_default();
            out.events.push(AiEvent::AttackStart {
                attack: a.name.clone(),
                via_node: a.via_node.clone(),
                dest_node: a.dest_node.clone(),
                anim: anim.clone(),
            });
            let fired = vec![(false, false); windows.len()];
            self.attack = Some(ActiveAttack { attack: a, anim, start: self.now, duration, frame_rate, windows, fired });
            true
        } else {
            false
        };
        self.path.clear();
        started
    }

    /// The attack anim has played to its end (Shared_ChildFinished of idShared_Attack; INTERIM: the anim duration).
    pub fn attack_done(&self) -> bool {
        self.attack.as_ref().is_none_or(|a| (self.now - a.start) as f32 / TICKS_PER_SEC as f32 >= a.duration)
    }

    /// The transition of the current state that fires this think, in orderIndex order (module docs).
    fn transition(&self, body: &Body, target: &Target) -> Option<State> {
        let has_enemy = target.alive;
        let recently_failed = self.move_failed_at.is_some_and(|t| self.now - t <= MOVE_FAIL_BLOCK_TICKS);
        // ZC_ShouldMoveTowardEnemy 0x140647790.
        let should_move = !recently_failed && has_enemy && !self.reached(body, target);
        let should_attack = || self.choose_attack(body, target, "default").is_some();
        match self.state {
            State::Relaxed => (self.sense.awareness == Awareness::Confirmed && has_enemy).then_some(State::Default),
            State::Default => {
                if self.sense.newly_aware {
                    Some(State::Sighted)
                } else if should_move {
                    Some(State::MoveTowardEnemy)
                } else if has_enemy {
                    Some(State::AttackIdle)
                } else {
                    None
                }
            }
            State::Sighted => {
                // Shared_ChildFinished: INTERIM, the sighted child ends once the body faces the enemy
                // (alignmentTolerance); the child FSM (0x1445aea80 / 0x1445af140) is not decoded.
                let face = wrap180(yaw_to(target.origin - body.origin) - body.yaw).abs();
                (face <= self.def.movement.alignment_tolerance).then_some(State::Default)
            }
            State::MoveTowardEnemy => {
                if should_attack() {
                    Some(State::Attack)
                } else if self.move_failed_at == Some(self.now) {
                    Some(State::Wander)
                } else if self.reached(body, target) {
                    Some(State::AttackIdle)
                } else {
                    None
                }
            }
            State::AttackIdle => {
                if !has_enemy {
                    Some(State::Done)
                } else if should_attack() {
                    Some(State::Attack)
                } else if should_move {
                    Some(State::MoveTowardEnemy)
                } else {
                    None
                }
            }
            State::Attack => self.attack_done().then_some(State::AttackIdle),
            // Shared_ShouldMoveTowardEnemy (range 1) -> AttackIdle; INTERIM: once the move-fail block expired.
            State::Wander => (!recently_failed).then_some(State::AttackIdle),
            State::Done => None,
        }
    }

    pub fn turn_toward(&self, yaw: f32, want: f32, dt: f32) -> f32 {
        let d = wrap180(want - yaw);
        let step = self.loco.body_turn_rate * dt;
        wrap180(yaw + d.clamp(-step, step))
    }

    /// The visual sense of one tick (sight test + awareness update; `Noticed` when the target becomes CONFIRMED).
    pub fn sense_tick(&mut self, world: &dyn World, body: &Body, target: &Target, now: i64, dt: f32, out: &mut AiOutput) {
        self.now = now;
        let eye = body.origin + Vec3::Z * self.def.eye_height;
        let los = world.line_of_sight(eye, target.sight_point);
        let st = perception::sight_test(&self.def.perception, &self.sense, body.origin, self.def.eye_height, body.yaw, target.sight_point, los);
        self.last_sight = st;
        let was = self.sense.awareness;
        if target.alive {
            self.sense.update(&self.def.perception, &st, target.origin, now, dt);
        }
        if was != Awareness::Confirmed && self.sense.awareness == Awareness::Confirmed {
            out.events.push(AiEvent::Noticed);
        }
    }

    /// One game tick: senses, FSM transitions (up to 5 per think, as the FSM update 0x14064cb90), then the state
    /// locomotion / attack output. `now` in game ticks, `dt` in seconds.
    pub fn tick(&mut self, world: &dyn World, body: &Body, target: &Target, now: i64, dt: f32) -> AiOutput {
        self.now = now;
        let mut out = AiOutput { yaw: body.yaw, ..Default::default() };
        if !body.alive || body.in_pain {
            out.state = Some(self.state);
            return out;
        }
        self.sense_tick(world, body, target, now, dt, &mut out);
        self.moving = if self.state == State::MoveTowardEnemy { Moving::Walking } else { Moving::Stopped };
        // FSM.
        for _ in 0..5 {
            match self.transition(body, target) {
                Some(s) => self.enter(s, body, target, &mut out),
                None => break,
            }
        }
        out.state = Some(self.state);
        let to_target = target.origin - body.origin;
        match self.state {
            State::Relaxed => out.web_node = Some(format!("{}/hands_relaxed/idle", self.def.anim_web)),
            State::Default | State::Sighted | State::AttackIdle | State::Wander | State::Done => {
                out.web_node = Some(self.loco.idle_node.clone());
                // idShared_Default focusOnEnemy / sighted: face the enemy (INTERIM: in-place turn at the body turn
                // rate; the game turns with the stepTransitions turn anims).
                if self.state != State::Done && self.state != State::Wander {
                    out.yaw = self.turn_toward(body.yaw, yaw_to(to_target), dt);
                }
            }
            State::MoveTowardEnemy => self.walk(world, body, target, dt, &mut out),
            State::Attack => self.attack_tick(&mut out),
        }
        out
    }

    /// Move toward the target: the map's AAS path (straight without nav), the body turning toward the move
    /// direction, the [`Locomotion`] walk node with its bodyMoveAngle scalar.
    pub fn walk(&mut self, world: &dyn World, body: &Body, target: &Target, dt: f32, out: &mut AiOutput) {
        let goal = target.origin;
        if let Some(nav) = world.nav() {
            let stale = self.path_goal.is_none_or(|g| g.distance(goal) > REPATH_DIST_INTERIM) || self.path.is_empty();
            if stale {
                self.path = nav.path(body.origin, goal);
                self.path_goal = Some(goal);
                if self.path.is_empty() {
                    self.move_failed_at = Some(self.now);
                    out.web_node = Some(self.loco.idle_node.clone());
                    return;
                }
            }
        } else {
            self.path = vec![goal];
        }
        // Drop reached waypoints.
        while self.path.len() > 1 {
            let d = self.path[0] - body.origin;
            if Vec3::new(d.x, d.y, 0.0).length() <= WAYPOINT_RADIUS_INTERIM {
                self.path.remove(0);
            } else {
                break;
            }
        }
        let wp = self.path[0];
        let d = Vec3::new(wp.x - body.origin.x, wp.y - body.origin.y, 0.0);
        let dir = d.normalize_or_zero();
        let move_yaw = yaw_to(dir);
        // allowStrafing false: the body turns toward the move direction.
        out.yaw = self.turn_toward(body.yaw, move_yaw, dt);
        out.move_dir = dir;
        out.move_speed = self.loco.walk_speed;
        out.web_node = Some(self.loco.walk_node.clone());
        // hands_combat/walk blendy1(bodyMoveAngle): the move direction relative to the body, degrees (+ = left).
        out.scalars.push(("bodyMoveAngle".to_string(), wrap180(move_yaw - out.yaw)));
    }

    /// The playing attack's via node and its melee windows (MeleeTraceStart / MeleeTraceEnd by anim frame).
    pub fn attack_tick(&mut self, out: &mut AiOutput) {
        let now = self.now;
        let Some(a) = self.attack.as_mut() else { return };
        out.web_node = Some(a.attack.via_node.clone());
        let frame = (now - a.start) as f32 / TICKS_PER_SEC as f32 * a.frame_rate;
        for (w, fired) in a.windows.iter().zip(a.fired.iter_mut()) {
            if !fired.0 && frame >= w.start_frame {
                fired.0 = true;
                let dmg = self.def.damage.get(&w.damage_decl);
                out.events.push(AiEvent::MeleeTraceStart {
                    joint_group: w.joint_group.clone(),
                    damage_decl: w.damage_decl.clone(),
                    damage: dmg.map_or(0.0, |d| d.max_damage),
                    player_damage_scale: dmg.map_or(1.0, |d| d.player_damage_scale),
                });
            }
            if fired.0 && !fired.1 && frame >= w.end_frame {
                fired.1 = true;
                out.events.push(AiEvent::MeleeTraceEnd);
            }
        }
    }

    /// The last sight test (for traces and tests).
    pub fn last_sight(&self) -> SightTest {
        self.last_sight
    }
}

/// Per-attack validity, 0x140525c70 (debug strings "invalid -- distance range", "distance_vertical range",
/// "not in attack arc"): d = target - AI origin; distance = |d| (|d.xy| with use2DDistanceChecks), for
/// ATTACK_VALIDATOR_ANIM_DELTA minus the horizontal length of the attack anim root-motion delta (`anim_delta`;
/// INTERIM: the anim end point is taken straight ahead, the game evaluates it with 0x1404de8b0), must lie in
/// distanceRange; d.z + 48 must lie in distanceRange_Vertical; the arc test is
/// cos(arcHalfLength) <= dot(forward rotated by arcDirection, unit(d.xy)). (arcVerticalHalfLength > 0 adds a
/// vertical test, unused by the Possessed.)
pub fn attack_valid(a: &Attack, body_yaw: f32, to: Vec3, anim_delta: f32) -> bool {
    if a.disabled {
        return false;
    }
    let mut dist = if a.use_2d_distance { Vec3::new(to.x, to.y, 0.0).length() } else { to.length() };
    if a.validator == attack::Validator::AnimDelta {
        dist -= anim_delta;
    }
    if dist < a.distance.0 || dist > a.distance.1 {
        return false;
    }
    let vertical = to.z + 48.0;
    if vertical < a.distance_vertical.0 || vertical > a.distance_vertical.1 {
        return false;
    }
    let dir = (body_yaw + a.arc_direction).to_radians();
    let arc_axis = Vec3::new(dir.cos(), dir.sin(), 0.0);
    let flat = Vec3::new(to.x, to.y, 0.0).normalize_or_zero();
    (a.arc_half_length * 0.017453292).cos() <= arc_axis.dot(flat)
}
