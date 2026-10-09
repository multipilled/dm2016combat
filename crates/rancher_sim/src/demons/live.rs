//! A demon in a live world: the AI physics move that carries its root motion (AnimFSM 0x1404ef900 ->
//! idPhysics_AI), the melee sphere-model trace against the player and idPlayer::Damage for that hit
//! (gamedata/re/DEMONS.md section 12).
//!
//! Root motion -> physics (AnimFSM 0x1404ef900): the frame's anim delta becomes the AI's own velocity,
//! `delta * 1000 / frame msec` (entity +0xdda4 = idPhysics_AI +0x1aec), and the physics move type is set by
//! 0x1404ce050 (0 = walking with gravity; 1 = no gravity, for AnimFSM states 4/5 or entity +0xb2d4 == 2). The
//! nav-graph clip 0x14050c240 (ai_clipDeltaToNavGraph 1: AAS trace of delta + 0.5-unit lookahead, scaled back by the
//! hit fraction) is computed there, but its result is dead: the velocity stores read the unclipped delta (asm
//! 0x1404efc59..0x1404efcce). So the AI is clipped by its physics, not by the AAS.
//!
//! idPhysics_AI::Evaluate 0x1404ca5e0: 0x1404cbfd0 consumes last frame's queries (StepMoveContacts contact normal,
//! GroundQuery contacts -> on ground when the averaged contact normal faces up by more than the floor cosine,
//! ground friction on the gravity velocity), then 0x1404cae10 integrates (gravity velocity += gravity * dt in the
//! air; on the ground its component along the gravity normal is removed; both velocities lose their component along
//! the last contact normal with the 1.001 overclip) and launches the next StepMoveContacts / GroundQuery (+-32 along
//! the gravity normal) / PushablesQuery from origin to origin + (self + gravity velocity) * dt.

use glam::Vec3;

use super::damage::AiDamageParms;
use super::repulsor::{repulse, Repulsed, Repulsor, RepulsorCvars};
use crate::collision::{Hull, World};

/// The repulsion inputs of one evaluate (see [`super::repulsor`]).
#[derive(Debug, Clone)]
pub struct Repulsion<'a> {
    pub body: Repulsed,
    pub list: &'a [Repulsor],
    pub cvars: RepulsorCvars,
}

/// idPhysics_AI constants of one demon type.
#[derive(Debug, Clone)]
pub struct AiPhysicsParms {
    /// Clip model: entityDef clipModelInfo (CLIPMODEL_BOX size x, y, z; origin at the bottom centre).
    pub hull: Hull,
    /// idPhysics_AI +0x44: gravity vector.
    pub gravity: Vec3,
    /// +0x1c1c: step height of the StepMoveContacts query. INTERIM: the AAS settings the map's nav was built for
    /// (idAAS2Settings maxStep, 18 for aas_monster48); the physics field's setter was not traced.
    pub step_height: f32,
    /// +0x1c20: min floor cosine of the ground test. INTERIM: AAS settings minFloorCos (0.7), as step_height.
    pub min_floor_cos: f32,
    /// ai_groundFriction_ContactFriction (100).
    pub contact_friction: f32,
}

impl AiPhysicsParms {
    /// `size` = clipModelInfo size (x, y, z).
    pub fn new(size: Vec3, gravity: Vec3, step_height: f32, min_floor_cos: f32, contact_friction: f32) -> Self {
        let hull = Hull::cuboid(Vec3::new(-size.x * 0.5, -size.y * 0.5, 0.0), Vec3::new(size.x * 0.5, size.y * 0.5, size.z));
        AiPhysicsParms { hull, gravity, step_height, min_floor_cos, contact_friction }
    }

    fn gravity_normal(&self) -> Vec3 {
        self.gravity.normalize_or(Vec3::NEG_Z)
    }
}

/// aiConstants.physics repulsor radii (idAI2::idAIConstant::physics_t, reflection: idAI2 +0xb278 + 0x1c / 0x20 /
/// 0x24): "default radius for vs. player repulsion", "vs. AI repulsion, not current enemy", "vs. AI enemy
/// repulsion". The constructor (0x14041c500) sets 48 / -1 / -1; generated decls omit fields left at that default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AiRepulsorRadii {
    pub player: f32,
    pub ai: f32,
    pub enemy: f32,
}

impl Default for AiRepulsorRadii {
    fn default() -> Self {
        AiRepulsorRadii { player: 48.0, ai: -1.0, enemy: -1.0 }
    }
}

impl AiRepulsorRadii {
    /// From an AI entityDef (inherit chain resolved by the DeclDb).
    pub fn load(db: &idres::decldb::DeclDb, entity: &str) -> Self {
        let d = Self::default();
        let Ok(e) = db.get("entitydef", entity) else { return d };
        let f = |k: &str, def: f32| e.f32(&format!("edit.aiConstants.physics.{k}")).unwrap_or(def);
        AiRepulsorRadii { player: f("playerRepulsorRadius", d.player), ai: f("aiRepulsorRadius", d.ai), enemy: f("enemyRepulsorRadius", d.enemy) }
    }
}

/// idPhysics_AI state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiPhysics {
    /// +0x1aec: the AI's own (anim) velocity.
    pub self_velocity: Vec3,
    /// +0x1af8: gravity velocity.
    pub gravity_velocity: Vec3,
    /// +0x1bb0: last StepMoveContacts contact normal (zero = none).
    pub contact_normal: Vec3,
    /// +0x1a72.
    pub on_ground: bool,
    /// +0x1be0: averaged ground contact normal.
    pub ground_normal: Vec3,
    /// Diagnostics: the last evaluate's repulsion changed the velocity.
    pub repulsed: bool,
}

/// Removes the component along `n` with the 1.001 overclip of 0x1404cae10 (both signs: dot / 1.001 when >= 0,
/// dot * 1.001 when < 0).
fn clip_normal(v: Vec3, n: Vec3) -> Vec3 {
    let d = v.dot(n);
    let d = if d >= 0.0 { d / 1.001 } else { d * 1.001 };
    v - n * d
}

/// INTERIM gap between the clip model and the floor that still counts as a ground contact (the GroundQuery
/// contact gathering, 0x141638b60, is an async collision job that was not decoded).
pub const GROUND_EPSILON: f32 = 0.25;

impl AiPhysics {
    /// One evaluate of `dt` seconds. `delta` = this frame's root-motion delta in world space (the AnimFSM sets
    /// self_velocity = delta / dt), `gravity_on` = move type 0. Returns the new origin.
    pub fn step(&mut self, world: &World, p: &AiPhysicsParms, origin: Vec3, delta: Vec3, gravity_on: bool, dt: f32) -> Vec3 {
        self.step_repulsed(world, p, origin, delta, gravity_on, dt, None)
    }

    /// [`Self::step`] with the body's repulsors: 0x1404cae10 runs the repulsion (0x1416a6c60) on the total velocity
    /// (+0x1ad4 = self + gravity, after the contact clip) with the origin and last evaluate's on-ground flag
    /// (+0x1a72), when the physics has a clip model (vslot 0x48); the self / gravity velocities keep their values.
    #[allow(clippy::too_many_arguments)]
    pub fn step_repulsed(&mut self, world: &World, p: &AiPhysicsParms, origin: Vec3, delta: Vec3, gravity_on: bool, dt: f32, rep: Option<&Repulsion>) -> Vec3 {
        if dt <= 0.0 {
            return origin;
        }
        let gn = p.gravity_normal();
        self.self_velocity = delta / dt;
        // 0x1404cae10, move type 0 (gravity) / 1 (none).
        if gravity_on {
            let up_speed = -(self.self_velocity + self.gravity_velocity).dot(gn);
            if !self.on_ground || up_speed > 1.0 {
                self.gravity_velocity += p.gravity * dt;
            } else {
                self.gravity_velocity -= gn * self.gravity_velocity.dot(gn);
            }
        } else {
            self.gravity_velocity = Vec3::ZERO;
        }
        self.gravity_velocity = clip_normal(self.gravity_velocity, self.contact_normal);
        self.self_velocity = clip_normal(self.self_velocity, self.contact_normal);
        let mut total = self.self_velocity + self.gravity_velocity;
        self.repulsed = false;
        if let Some(r) = rep {
            let before = total;
            total = repulse(&r.body, r.list, origin, total, self.on_ground, &r.cvars);
            self.repulsed = total != before;
        }
        let (pos, normal) = step_move(world, p, origin, origin + total * dt, if gravity_on { p.step_height } else { 0.0 });
        // 0x1404cbfd0 (next evaluate in the game; INTERIM: applied in the same evaluate here, so the engine's
        // one-frame query latency is not reproduced).
        self.contact_normal = normal.unwrap_or(Vec3::ZERO);
        self.ground(world, p, pos);
        pos
    }

    /// Ground contacts (0x1404cbfd0): on ground when the averaged contact normal n satisfies
    /// dot(n, gravityNormal) < -minFloorCos; then the gravity velocity's part across the gravity normal loses
    /// min(its length, clamp(-dot(gn, n), 0, 1) * ai_groundFriction_ContactFriction).
    fn ground(&mut self, world: &World, p: &AiPhysicsParms, pos: Vec3) {
        let gn = p.gravity_normal();
        let tr = world.translate(&p.hull, pos, pos + gn * GROUND_EPSILON);
        self.on_ground = false;
        if !tr.hit() {
            return;
        }
        let n = tr.c.normal.normalize_or_zero();
        if n.dot(gn) >= -p.min_floor_cos {
            return;
        }
        self.on_ground = true;
        self.ground_normal = n;
        let k = (-gn.dot(n)).clamp(0.0, 1.0);
        let lat = self.gravity_velocity - gn * self.gravity_velocity.dot(gn);
        let len = lat.length();
        if len > 0.0 {
            let cut = (k * p.contact_friction).min(len).max(0.0);
            self.gravity_velocity -= lat / len * cut;
        }
    }
}

/// StepMoveContacts (query 0x141635340, consumed by 0x1404cbfd0 through 0x14157fba0): moves the clip model from
/// `start` toward `end`, stepping up to `step` units. Returns the end position and the blocking contact normal.
/// INTERIM: the query is an async collision job whose algorithm was not decoded; this is a plain
/// slide + step-up + step-down with the sim's collision world.
pub fn step_move(world: &World, p: &AiPhysicsParms, start: Vec3, end: Vec3, step: f32) -> (Vec3, Option<Vec3>) {
    let tr = world.translate(&p.hull, start, end);
    if !tr.hit() {
        return (tr.endpos, None);
    }
    let normal = tr.c.normal;
    let up = -p.gravity_normal();
    // Step: up, across, down; taken when it gets further and lands on a floor.
    if step > 0.0 && normal.dot(up) < p.min_floor_cos {
        let a = world.translate(&p.hull, start, start + up * step);
        let move_xy = end - start;
        let b = world.translate(&p.hull, a.endpos, a.endpos + move_xy);
        let c = world.translate(&p.hull, b.endpos, b.endpos - up * (step + GROUND_EPSILON));
        let landed = c.hit() && c.c.normal.dot(up) >= p.min_floor_cos;
        let flat = |v: Vec3| v - up * v.dot(up);
        if landed && flat(c.endpos - start).length() > flat(tr.endpos - start).length() + 0.01 {
            return (c.endpos, if b.hit() { Some(b.c.normal) } else { None });
        }
    }
    // Slide along the blocking plane for the rest of the move.
    let rest = clip_normal(end - tr.endpos, normal);
    let tr2 = world.translate(&p.hull, tr.endpos, tr.endpos + rest);
    (tr2.endpos, Some(if tr2.hit() { tr2.c.normal } else { normal }))
}

/// One SphereModelTrace sphere of an md6Def traceGroup (JOINTGROUP_TRACE, group type 12) in world space.
/// The component (0x1406c9510) builds a clip model per entry from the bounds +-radius around joint + offset, and
/// its update (0x1406cb7d0) sweeps it from the previous to the current pose with the clip world query 0x141638b60
/// (contents mask 0x108409 from AnimEvent_StartSphereModelTrace 0x1403df200).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceBox {
    pub centre: Vec3,
    pub radius: f32,
}

/// Whether the box +-`r` swept from `a` to `b` touches the axis-aligned box `lo..hi` (the player's clip box):
/// segment a->b against the box grown by r (slab test).
pub fn sweep_hits_box(a: Vec3, b: Vec3, r: f32, lo: Vec3, hi: Vec3) -> Option<f32> {
    let (lo, hi) = (lo - Vec3::splat(r), hi + Vec3::splat(r));
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if a[i] < lo[i] || a[i] > hi[i] {
                return None;
            }
            continue;
        }
        let (mut ta, mut tb) = ((lo[i] - a[i]) / d[i], (hi[i] - a[i]) / d[i]);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// An active ae_startSphereModelTrace(WithAutoStop) .. ae_endSphereModelTrace window.
#[derive(Debug, Clone, PartialEq)]
pub struct MeleeTrace {
    pub joint_group: String,
    pub damage_decl: String,
    /// ae_startSphereModelTraceWithAutoStop's bool / the AutoStop variant: stored at slot +0xcc8 (0x1406ca090) and
    /// read by none of the component's functions (setup 0x1406c9510, start 0x1406ca090, update 0x1406cb7d0,
    /// stop-by-id 0x1406c87c0 -> 0x1406c97b0, which ends the slots whose +0xcb4 id matches).
    /// INTERIM: taken as "the trace ends at its first contact", the player's or the world's: the sweep's mask includes
    /// CONTENTS_SOLID, which only matters if world contacts affect the trace, and without that a sphere crosses a thin
    /// wall over a few frames and hits from the far side (range run: 5-9 frames of wall contact, then a hit). The
    /// consumer of the async query results (the update stores each sweep's query handle at entry +0x28) was not
    /// located, nor an already-hit list. Without auto stop a world contact only blocks that frame, and each entity is
    /// still damaged once per window.
    pub auto_stop: bool,
    /// Last frame's world boxes (None on the first frame: the update sweeps start == end then, a static test).
    pub prev: Option<Vec<TraceBox>>,
    pub hit_player: bool,
    /// A sweep that would have reached the player hit the world first (at least once in this window).
    pub world_blocked: bool,
    /// Diagnostics: frames of this window (before it hit or stopped) where some sphere's sweep touched the world.
    pub world_touches: u32,
    pub done: bool,
}

impl MeleeTrace {
    pub fn new(joint_group: &str, damage_decl: &str, auto_stop: bool) -> Self {
        MeleeTrace { joint_group: joint_group.into(), damage_decl: damage_decl.into(), auto_stop, prev: None, hit_player: false, world_blocked: false, world_touches: 0, done: false }
    }

    /// One update with this frame's boxes against the player's clip box: true when the player is hit this frame
    /// (once per window).
    pub fn update(&mut self, boxes: Vec<TraceBox>, player_lo: Vec3, player_hi: Vec3) -> bool {
        self.update_in(None, boxes, player_lo, player_hi)
    }

    /// [`Self::update`] in a world: the sweep's contents mask 0x108409 (CONTENTS_SOLID | PLAYERCLIP | AI | PLAYER |
    /// SOLIDPUSHABLE, AnimEvent_StartSphereModelTrace 0x1403df200) makes the clip world block it, so a sphere that
    /// reaches a wall before the player's box does not hit the player that frame.
    pub fn update_in(&mut self, world: Option<&World>, boxes: Vec<TraceBox>, player_lo: Vec3, player_hi: Vec3) -> bool {
        let mut hit = false;
        if !self.done && !self.hit_player {
            let mut touched = false;
            for (i, b) in boxes.iter().enumerate() {
                let a = self.prev.as_ref().and_then(|p| p.get(i)).map_or(b.centre, |p| p.centre);
                let player = sweep_hits_box(a, b.centre, b.radius, player_lo, player_hi);
                let r = Vec3::splat(b.radius);
                let wall = world.map(|w| w.translate(&Hull::cuboid(-r, r), a, b.centre)).filter(|tr| tr.hit()).map(|tr| tr.fraction);
                touched |= wall.is_some();
                match (player, wall) {
                    (Some(tp), Some(tw)) if tw < tp => self.world_blocked = true,
                    (Some(_), _) => {
                        hit = true;
                        break;
                    }
                    _ => {}
                }
            }
            if touched {
                self.world_touches += 1;
                // INTERIM (see auto_stop): an auto-stop window ends at its first world contact.
                if self.auto_stop && !hit {
                    self.world_blocked = true;
                    self.done = true;
                }
            }
        }
        if hit {
            self.hit_player = true;
            if self.auto_stop {
                self.done = true;
            }
        }
        self.prev = Some(boxes);
        hit
    }
}

/// Player-side damage settings (idPlayer::Damage 0x140dc30a0 -> 0x140dbee00).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerDamageScales {
    /// 0x140393200: gameDifficulty decl (gameLocal +0xb4bc8; INFERRED campaign/playerincomingdamage by name) per
    /// difficulty: +0x70 easyScale, +0x74 mediumScale, +0x78 hardScale, +0x80 nightmareScale for both
    /// ULTRA-VIOLENT and NIGHTMARE (the exe skips +0x7c ultraViolentScale); 1.0 without a decl; used when > 0.
    pub difficulty: f32,
    /// g_damageScale (applied when > 0).
    pub g_damage_scale: f32,
}

impl PlayerDamageScales {
    /// From the install's decls and cvars for `difficulty` 0..4 (EASY, MEDIUM, HARD, ULTRA-VIOLENT, NIGHTMARE).
    pub fn load(db: &idres::decldb::DeclDb, cvars: &crate::config::CvarValues, difficulty: usize) -> Self {
        let decl = db.get("gamedifficulty", "campaign/playerincomingdamage").ok();
        let field = ["easyScale", "mediumScale", "hardScale", "nightmareScale", "nightmareScale"][difficulty.min(4)];
        // A generated decl omits fields left at the ctor default; INFERRED 1.0 (what 0x140393200 returns
        // without a decl).
        let difficulty = decl.and_then(|b| b.block("edit").and_then(|e| e.f32(field))).unwrap_or(1.0);
        let g = cvars.0.get("g_damageScale").and_then(|v| idres::decl::parse_number(v)).unwrap_or(1.0);
        PlayerDamageScales { difficulty, g_damage_scale: g }
    }
}

impl Default for PlayerDamageScales {
    fn default() -> Self {
        PlayerDamageScales { difficulty: 1.0, g_damage_scale: 1.0 }
    }
}

/// Health damage of one hit on the player (no armour): 0x140dbee00 then 0x140dc30a0.
/// base = GetDamage(decl, |attacker - player|^2) (0x1406e4a10) * location scale (0x14074a090: 1.0 for a contact
/// without a joint, as the melee trace's box contacts) [+ inflictor term, inflictorDamageRate * inflictor value
/// capped by maxInflictorDamage: 0 for these decls], * difficulty (0x140393200, when > 0), floored at 0.017,
/// * damage scale argument * playerDamageScale (decl +0xfc); then * g_damageScale (when > 0).
/// The health curve (0x140dbee00: idPlayer easyTable / normalTable / hardTable / nightmareTable, reflection
/// +0x46e40..+0x46e58, picked by gameLocal vslot 0x670 0 / 1 / 2 / 4 and evaluated at the current health by
/// 0x140284580) is skipped: no player entityDef (player, player/sp/intro, player/start, player/mp) sets the tables.
/// INTERIM 1.0: the attacker-class difficulty object (+0x28 of gameSystem vslot 0x340 -> 0x2b8, applied to the damage scale for AI attackers), perk / powerup
/// and inventory scales, the armour split (health component vslot 0x48) and the "leave 1 health" protection
/// branch.
pub fn player_damage(parms: &AiDamageParms, dist_sq: f32, scale: f32, s: &PlayerDamageScales) -> f32 {
    let mut d = parms.at_distance_sq(dist_sq);
    if s.difficulty > 0.0 {
        d *= s.difficulty;
    }
    let mut d = d.max(0.017) * scale * parms.player_damage_scale;
    if s.g_damage_scale > 0.0 {
        d *= s.g_damage_scale;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor_world() -> World {
        let mut w = World::default();
        w.add(Hull::cuboid(Vec3::new(-1000.0, -1000.0, -64.0), Vec3::new(1000.0, 1000.0, 0.0)));
        w
    }

    fn parms() -> AiPhysicsParms {
        AiPhysicsParms::new(Vec3::new(48.0, 48.0, 96.0), Vec3::new(0.0, 0.0, -1066.0), 18.0, 0.7, 100.0)
    }

    #[test]
    fn falls_and_lands() {
        let w = floor_world();
        let p = parms();
        let mut ph = AiPhysics::default();
        let mut o = Vec3::new(0.0, 0.0, 40.0);
        for _ in 0..120 {
            o = ph.step(&w, &p, o, Vec3::ZERO, true, 1.0 / 60.0);
        }
        assert!(ph.on_ground, "{o}");
        assert!(o.z.abs() < 0.5, "{o}");
        assert!(ph.gravity_velocity.length() < 1.0);
    }

    #[test]
    fn walks_on_floor_and_stops_at_wall() {
        let mut w = floor_world();
        w.add(Hull::cuboid(Vec3::new(100.0, -500.0, 0.0), Vec3::new(132.0, 500.0, 200.0)));
        let p = parms();
        let mut ph = AiPhysics { on_ground: true, ..Default::default() };
        let mut o = Vec3::ZERO;
        for _ in 0..120 {
            o = ph.step(&w, &p, o, Vec3::new(2.0, 0.5, 0.0), true, 1.0 / 60.0);
        }
        // Blocked at x = 100 - 24, slides along y.
        assert!((o.x - 76.0).abs() < 0.5, "{o}");
        assert!(o.y > 30.0, "{o}");
        assert!(o.z.abs() < 0.5, "{o}");
    }

    #[test]
    fn steps_up_a_stair() {
        let mut w = floor_world();
        w.add(Hull::cuboid(Vec3::new(50.0, -500.0, 0.0), Vec3::new(400.0, 500.0, 16.0)));
        let p = parms();
        let mut ph = AiPhysics { on_ground: true, ..Default::default() };
        let mut o = Vec3::ZERO;
        for _ in 0..60 {
            o = ph.step(&w, &p, o, Vec3::new(2.0, 0.0, 0.0), true, 1.0 / 60.0);
        }
        assert!(o.x > 100.0 && (o.z - 16.0).abs() < 0.5, "{o}");
    }

    #[test]
    fn sweep_box() {
        let (lo, hi) = (Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 90.0));
        assert!(sweep_hits_box(Vec3::new(-50.0, 0.0, 50.0), Vec3::new(50.0, 0.0, 50.0), 6.0, lo, hi).is_some());
        assert!(sweep_hits_box(Vec3::new(-50.0, 30.0, 50.0), Vec3::new(50.0, 30.0, 50.0), 6.0, lo, hi).is_none());
        assert!(sweep_hits_box(Vec3::new(-50.0, 21.0, 50.0), Vec3::new(50.0, 21.0, 50.0), 6.0, lo, hi).is_some());
        let mut t = MeleeTrace::new("left_arm", "d", true);
        let far = vec![TraceBox { centre: Vec3::new(-60.0, 0.0, 50.0), radius: 6.0 }];
        let near = vec![TraceBox { centre: Vec3::new(30.0, 0.0, 50.0), radius: 6.0 }];
        assert!(!t.update(far, lo, hi));
        assert!(t.update(near.clone(), lo, hi));
        assert!(!t.update(near, lo, hi));
        assert!(t.done);
    }

    #[test]
    fn walk_stops_at_hard_stop_repulsor() {
        use crate::demons::repulsor::{flags, RepulsorBox, RepulsorStyle};
        let w = floor_world();
        let p = parms();
        let list = [Repulsor { owner: 2, origin: Vec3::new(100.0, 0.0, 0.0), radius: 24.0, height: 72.0, flags: flags::SUM_RADII, style: RepulsorStyle::HardStop, bbox: RepulsorBox::default(), only_moving: false, only_on_ground: false }];
        let rep = Repulsion { body: Repulsed { owner: 1, radius: 24.0, height: 96.0 }, list: &list, cvars: RepulsorCvars::default() };
        let mut ph = AiPhysics { on_ground: true, ..Default::default() };
        let mut o = Vec3::ZERO;
        for _ in 0..120 {
            o = ph.step_repulsed(&w, &p, o, Vec3::new(2.0, 0.0, 0.0), true, 1.0 / 60.0, Some(&rep));
        }
        // Stopped once inside 24 + 24 of the repulsor.
        assert!((51.9..54.1).contains(&o.x) && o.y.abs() < 1e-3, "{o}");
    }

    #[test]
    fn melee_sweep_blocked_by_wall() {
        let (lo, hi) = (Vec3::new(30.0, -16.0, 0.0), Vec3::new(62.0, 16.0, 90.0));
        let a = vec![TraceBox { centre: Vec3::new(-10.0, 0.0, 50.0), radius: 6.0 }];
        let b = vec![TraceBox { centre: Vec3::new(40.0, 0.0, 50.0), radius: 6.0 }];
        let open = World::default();
        let mut t = MeleeTrace::new("right_arm", "d", true);
        t.update_in(Some(&open), a.clone(), lo, hi);
        assert!(t.update_in(Some(&open), b.clone(), lo, hi));
        // A wall at x 10..14 between the arm and the player.
        let mut walled = World::default();
        walled.add(Hull::cuboid(Vec3::new(10.0, -100.0, 0.0), Vec3::new(14.0, 100.0, 200.0)));
        let mut t = MeleeTrace::new("right_arm", "d", true);
        t.update_in(Some(&walled), a.clone(), lo, hi);
        assert!(!t.update_in(Some(&walled), b, lo, hi));
        assert!(t.world_blocked && t.done);
        // An arm that crosses the wall over several frames: auto stop ends the window at the first wall contact;
        // without it each frame stands alone and the sphere hits from the far side.
        let path = [12.0, 26.0, 40.0].map(|x| vec![TraceBox { centre: Vec3::new(x, 0.0, 50.0), radius: 6.0 }]);
        for (auto, hits) in [(true, false), (false, true)] {
            let mut t = MeleeTrace::new("right_arm", "d", auto);
            t.update_in(Some(&walled), a.clone(), lo, hi);
            let got = path.iter().fold(false, |h, b| t.update_in(Some(&walled), b.clone(), lo, hi) || h);
            assert_eq!((got, t.world_touches > 0), (hits, true), "auto stop {auto}");
        }
    }

    /// aiConstants.physics of the Possessed (install-backed).
    #[test]
    fn possessed_repulsor_radii() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let r = AiRepulsorRadii::load(&inst.decls, crate::demons::POSSESSED);
        assert_eq!((r.player, r.ai, r.enemy), (24.0, 32.0, 24.0));
    }

    /// damage/zion/ai/zombie/melee on the player per difficulty (install-backed).
    #[test]
    fn possessed_melee_on_player() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let parms = AiDamageParms::from_decl(&inst.decls, "damage/zion/ai/zombie/melee").unwrap();
        assert_eq!((parms.min, parms.max, parms.player_damage_scale), (60.0, 60.0, 0.25));
        let cv = crate::config::CvarValues(inst.cvars.0.clone());
        let got: Vec<f32> = (0..5).map(|d| player_damage(&parms, 64.0 * 64.0, 1.0, &PlayerDamageScales::load(&inst.decls, &cv, d))).collect();
        assert_eq!(got, [7.5, 15.0, 26.25, 45.0, 45.0]);
    }
}
