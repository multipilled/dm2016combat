//! Demons in the testbed: the Possessed (`ai/zombie/scientist`) standing in the firing range as a damageable
//! target. The rules run headless in `rancher_sim::demons` (damage chain, pain buckets / SDPS reactions,
//! stagger, death; gamedata/re/DEMONS.md); this module loads the model, runs each demon's anim web on the game
//! clock, skins it on the CPU with the engine's 4-weight decode (viewanim's skin parts), keeps the md6Def
//! hit-test spheres in world space for the weapon code, and applies the damage the weapon code reports.
//!
//! Interface for combat.rs: [`DemonTargets::trace`] / [`DemonTargets::in_radius`] (last frame's pose) and the
//! [`DemonDamage`] message (one idAI2::Damage call). Root motion moves the entity (see [`root_delta`]).
//! The AI (rancher_sim::ai::Brain) runs every frame: it notices the player, walks (the map's AAS2 path; the range
//! has no nav data, so straight at the player) and melees; root motion moves the entity through the AI physics
//! (rancher_sim::demons::live) and the melee anim's ae_startSphereModelTrace* window sweeps the md6Def trace group
//! against the player, whose health takes idPlayer::Damage (live::player_damage).
//! The demon keeps off the player through its repulsors (rancher_sim::demons::repulsor; its clipMask excludes the
//! player). INTERIM: no gore, no glory kills, no corpse fade shader (hidden at the end of fadeOutTime), and the
//! player is not yet repulsed by / blocked by the demon (player-side repulsors, AI playerCollisionSize clip).

mod glory;
mod imp;
mod soldier;

pub use glory::SyncView;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use glam::Mat4;
use idres::Container;
use idres::animweb::{AnimWeb, BlendParms};
use rancher_sim::Vec3 as V;
use rancher_sim::animweb::{AnimData, AnimMeta, AnimWebRuntime};
use rancher_sim::config::CvarValues;
use rancher_sim::demons::actor::AiCvars;
use rancher_sim::demons::damage::ParmsCache;
use rancher_sim::ai::{self, AiEvent, Brain, Nav};
use rancher_sim::demons::live::{AiPhysics, AiPhysicsParms, AiRepulsorRadii, MeleeTrace, PlayerDamageScales, Repulsion, TraceBox};
use rancher_sim::demons::repulsor::{self, PlayerRepulsorCvars, Repulsed, RepulsorCvars};
use rancher_sim::demons::{self, DamageEvent, Demon, DemonDef, DemonEvent, DemonPhase, HitSphere, WebRequest, WebTags};

use crate::animweb::{Clips, Locals};
use crate::assets;
use crate::rig::{Ik2, PoseBuf};
use crate::viewanim::{Rig, SkinPart, skin_parts};

/// Root (origin joint, model space) of `clip` at frame position `f`: (translation, yaw radians).
fn root_at(clip: &idres::md6anim::Md6Anim, origin: u16, f: f32) -> (V, f32) {
    let p = clip.sample(f);
    let t = p.trans.iter().find(|x| x.0 == origin).map_or(V::ZERO, |x| V::from(x.1));
    // Origin rotations are about z only in the zombie's anims (dump_death_anims).
    let yaw = p.rot.iter().find(|x| x.0 == origin).map_or(0.0, |x| {
        let [qx, qy, qz, qw] = x.1;
        (2.0 * (qw * qz + qx * qy)).atan2(1.0 - 2.0 * (qy * qy + qz * qz))
    });
    (t, yaw)
}

/// Motion from root `a` to root `b`, in `a`'s frame (= the entity's frame): (translation, yaw radians).
fn root_diff(a: (V, f32), b: (V, f32)) -> (V, f32) {
    let d = glam::Quat::from_rotation_z(-a.1) * (b.0 - a.0);
    let mut y = b.1 - a.1;
    if y > std::f32::consts::PI {
        y -= std::f32::consts::TAU;
    } else if y < -std::f32::consts::PI {
        y += std::f32::consts::TAU;
    }
    (d, y)
}

/// This frame's root motion of a pose tree: each leaf's origin delta between its previous and current frame
/// position (the engine samples the origin a second time at prevTime, ANIMWEB.md section 2), blended with the
/// tree's LERP alphas. The AI then moves by it: AnimFSM 0x1404ef900 turns the frame's origin delta into a velocity
/// (delta * 1000 / frame msec) for the AI physics object (entity+0xc2b8; 0x1404ce050 sets its gravity mode); the
/// nav-graph clip it computes (0x14050c240) is never used (rancher_sim::demons::live docs). The rendered pose keeps
/// the origin joint at identity.
/// INTERIM: a leaf that just entered contributes nothing on its first frame and a leaf whose frame went backwards
/// is taken as restarted from frame 0 (a looping leaf's wrap tail is dropped); the delta's frame convention and
/// the blend of deltas are not decoded (taking it in the previous root's frame keeps the authored path). The caller
/// moves the entity with it through rancher_sim::demons::live::AiPhysics (collision, step, gravity).
fn root_delta(node: Option<&rancher_sim::animweb::PoseNode>, clips: &Clips, origin: u16, prev: &HashMap<String, f32>, next: &mut HashMap<String, f32>) -> (V, f32) {
    use rancher_sim::animweb::PoseNode;
    match node {
        None | Some(PoseNode::Bind) => (V::ZERO, 0.0),
        Some(PoseNode::Leaf { anim, frame, frac }) => {
            let f = *frame as f32 + *frac;
            next.insert(anim.clone(), f);
            let (Some(clip), Some(&p)) = (clips.get(anim), prev.get(anim)) else { return (V::ZERO, 0.0) };
            let from = if p <= f { p } else { 0.0 };
            root_diff(root_at(clip, origin, from), root_at(clip, origin, f))
        }
        Some(PoseNode::Lerp { left, right, alpha }) => {
            let (a, ya) = root_delta(Some(left), clips, origin, prev, next);
            let (b, yb) = root_delta(Some(right), clips, origin, prev, next);
            (a.lerp(b, *alpha), ya + (yb - ya) * alpha)
        }
        // INTERIM: an additive op moves the entity by its base side only (ANIMWEB.md 3c: base = right for
        // ADD_LEFT / SUB_LEFT, left otherwise); the additive side's root track is not decoded. Both sides are
        // walked so their leaf clocks stay current.
        Some(PoseNode::Op { op, left, right, .. }) => {
            use rancher_sim::animweb::BlendOp;
            let l = root_delta(Some(left), clips, origin, prev, next);
            let r = root_delta(Some(right), clips, origin, prev, next);
            if matches!(op, BlendOp::AddLeft | BlendOp::SubLeft) { r } else { l }
        }
    }
}

/// RANCHER_TRACE: entity origin / yaw and the world positions of the root, hips, feet and toes.
fn trace_pose(id: u32, now: i32, what: &str, skel: &idres::md6::Md6Skel, joints: &[Mat4], m2w: Mat4, origin: V, yaw: f32) {
    let mut line = format!("[demon {id} {now}] pose {what}: entity ({:.1} {:.1} {:.1}) yaw {yaw:.1}", origin.x, origin.y, origin.z);
    for j in ["origin", "hips", "leftfoot", "lefttoebase", "rightfoot", "righttoebase"] {
        if let Some(i) = skel.names.iter().position(|n| n.eq_ignore_ascii_case(j)) {
            let w = m2w.transform_point3(joints[i].w_axis.truncate());
            line += &format!(" {j} ({:.1} {:.1} {:.1})", w.x, w.y, w.z);
        }
    }
    println!("{line}");
}
use crate::vtmat::{VtMaterial, VtMaterials};

/// Texture level for demons (level 1 = half the authored virtual-texture resolution, as the view models).
const DEMON_TEXTURE_LEVEL: usize = 1;
/// Difficulty for the range (DIFFICULTY_MEDIUM, "Hurt Me Plenty").
const DIFFICULTY: usize = 1;

/// One idAI2::Damage call for demon `id`, written by the weapon code.
#[derive(Message, Clone, Debug)]
pub struct DemonDamage {
    pub id: u32,
    pub event: DamageEvent,
}

/// A demon the ray hit.
#[derive(Debug, Clone)]
pub struct DemonHit {
    pub id: u32,
    pub dist: f32,
    pub point: V,
    pub normal: V,
    /// Skeleton joint of the hit sphere (DamageEvent trace joint).
    pub joint: String,
}

/// The demons' hit volumes this frame (md6Def hitTestGroup spheres in world space, idTech units).
#[derive(Resource, Default)]
pub struct DemonTargets {
    pub demons: Vec<DemonTarget>,
}

pub struct DemonTarget {
    pub id: u32,
    /// (centre, radius, joint name).
    pub spheres: Vec<(V, f32, String)>,
    pub origin: V,
    pub bounds: (V, V),
    /// Weapon targeting (rancher_sim::weapons::targeting): GetTargetPoint per aimPoint_t 0..=7 (last frame's
    /// pose) and the physics clip box in world space (the abs bounds the best-target query reads).
    pub aim_points: [V; 8],
    pub clip_bounds: (V, V),
}

/// actorConstants.aimPointJoints of the entityDef as (aimPoint_t, skeleton joint), and the `head` joint the eye
/// position (idAI2 vslot +0x718 0x14074a6d0) reads.
#[derive(Default)]
struct AimJoints {
    points: Vec<(usize, usize)>,
    head: Option<usize>,
}

impl AimJoints {
    fn load(db: &idres::decldb::DeclDb, names: &[String]) -> Self {
        let joint = |n: &str| names.iter().position(|j| j.eq_ignore_ascii_case(n));
        let mut points = Vec::new();
        if let Some(l) = db.get("entitydef", demons::POSSESSED).ok().and_then(|e| e.block("edit.actorConstants.aimPointJoints").cloned()) {
            let n = l.f32("num").unwrap_or(0.0) as usize;
            for i in 0..n {
                let (Some(t), Some(name)) = (l.str(&format!("item[{i}].type")), l.str(&format!("item[{i}].name"))) else { continue };
                if let (Some(p), Some(j)) = (rancher_sim::weapons::targeting::aim_point(t), joint(name)) {
                    points.push((p, j));
                }
            }
        }
        Self { points, head: joint("head") }
    }

    /// World positions: joint w_axis through the model-to-world matrix. INTERIM: without a `head` joint the
    /// eye falls back to the origin (the exe's eyeOffset path vslot +0xad0 is not ported).
    fn world(&self, joints: &[Mat4], m2w: Mat4, origin: V, clip: (V, V)) -> [V; 8] {
        let at = |j: usize| joints.get(j).map(|m| m2w.transform_point3(m.w_axis.truncate()));
        let pts: Vec<(usize, V)> = self.points.iter().filter_map(|(p, j)| at(*j).map(|v| (*p, v))).collect();
        let eye = self.head.and_then(at).unwrap_or(origin);
        rancher_sim::weapons::targeting::aim_points(&pts, eye, origin, clip)
    }
}

impl DemonTargets {
    /// Nearest sphere along the ray within `max` (dir unit length). Dead demons have no hit volumes.
    pub fn trace(&self, start: V, dir: V, max: f32) -> Option<DemonHit> {
        let mut best: Option<DemonHit> = None;
        for d in &self.demons {
            for (c, r, j) in &d.spheres {
                if let Some(t) = demons::hit::ray_sphere(start, dir, max, *c, *r) {
                    if best.as_ref().is_none_or(|b| t < b.dist) {
                        let p = start + dir * t;
                        best = Some(DemonHit { id: d.id, dist: t, point: p, normal: (p - *c).normalize_or(-dir), joint: j.clone() });
                    }
                }
            }
        }
        best
    }

    /// Demons whose sphere bounds intersect the sphere (at, radius): (id, bounds min, max) for the radius
    /// damage code (0x1403861e0 works on bounds).
    pub fn in_radius(&self, at: V, radius: f32) -> impl Iterator<Item = (u32, V, V)> + '_ {
        self.demons.iter().filter(move |d| {
            let c = at.clamp(d.bounds.0, d.bounds.1);
            c.distance_squared(at) <= radius * radius
        }).map(|d| (d.id, d.bounds.0, d.bounds.1))
    }
}

/// The skinned model and anim data shared by every demon of one type.
struct DemonKind {
    /// The entityDef ("ai/zombie/scientist").
    entity: String,
    def: Arc<DemonDef>,
    tags: Arc<WebTags>,
    web: Arc<AnimWeb>,
    data: Arc<AnimData>,
    rig: Rig,
    bind: Locals,
    offset: [f32; 3],
    spheres: Vec<HitSphere>,
    meshes: Vec<(String, idres::md6::Md6Model)>,
    legs: LegRig,
    /// The skeleton's `origin` joint (root motion).
    origin: Option<usize>,
    /// md6Def traceGroup spheres by group name (SphereModelTrace).
    trace_groups: HashMap<String, Vec<HitSphere>>,
    phys: AiPhysicsParms,
    /// aiConstants.physics repulsor radii.
    radii: AiRepulsorRadii,
    aim: AimJoints,
}

/// The engine's standard rig, group "limbs_lower" (the legs). Every skeleton gets a rig when it loads (0x1416d1430 ->
/// 0x14171be90 -> 0x14171cdb0: groups limbs_lower 20 / limbs_upper 40 / deformation 60). The leg builder 0x141719b10
/// adds, per side (Left then Right) whose `{Side}UpLeg`, `{Side}Leg` and `{Side}Foot` exist, one `idRigIk2Segments`
/// node (0x141a3f570, the arm rig's solver in rig.rs) on UpLeg -> Leg -> Foot under UpLeg's parent, toward
/// `rig_leg_{side}_target` swivelled by `rig_leg_{side}_pole`, built from the bind pose. The blend interpreter
/// (0x14173c420) runs the rig on the final pose (0x141a3cc00 -> 0x141a480d0 -> 0x141a47c80; rig_skip 0,
/// rig_skipIk2Pole 0). Monster anims key those rig joints, not the leg joints.
/// The arm builder 0x14171a820 adds arm IK only for reversed (`*handattach`) skeletons here (0x14171cdb0 passes
/// its param_4 = 0), so a demon's arms stay FK.
/// INTERIM: a side with `rig_leg_{side}_ball` and a toe (`{Side}ToeBase` / `{Side}FootBall`) additionally gets a
/// reach node (0x141a3ef50, length = sum of 3 bone lengths, evaluator not decoded), a ball offset copy and a
/// target -> toe copy, and solves toward the ball; that path is not ported and such legs are left unsolved
/// (the Possessed has no ball joints). The ground adaptation layers (WalkIK `ik_*`, hybrid rig aligner
/// `rig_aligner*`) are not ported: on flat floors they leave the keyed targets as they are.
struct LegRig {
    legs: Vec<Ik2>,
}

impl LegRig {
    fn new(skel: &idres::md6::Md6Skel) -> Self {
        let find = |n: &str| skel.names.iter().position(|x| x.eq_ignore_ascii_case(n));
        let bind = PoseBuf::new(skel, None);
        let mut legs = Vec::new();
        for side in ["Left", "Right"] {
            let lo = side.to_ascii_lowercase();
            let (Some(up), Some(leg), Some(foot)) = (find(&format!("{side}UpLeg")), find(&format!("{side}Leg")), find(&format!("{side}Foot"))) else { continue };
            let (Some(target), Some(pole)) = (find(&format!("rig_leg_{lo}_target")), find(&format!("rig_leg_{lo}_pole"))) else { continue };
            let toe = find(&format!("{side}ToeBase")).or_else(|| find(&format!("{side}FootBall")));
            if toe.is_some() && find(&format!("rig_leg_{lo}_ball")).is_some() {
                eprintln!("demons: rig_leg_{lo}_ball present; the ball leg rig is not ported (leg left unsolved)");
                continue;
            }
            let Ok(parent) = usize::try_from(skel.parents[up]) else { continue };
            legs.push(Ik2::new(&bind.model, parent, up, leg, foot, target, pole));
        }
        LegRig { legs }
    }

    fn apply(&self, pb: &mut PoseBuf) {
        for ik in &self.legs {
            ik.solve(pb, false);
        }
    }
}

struct DemonInst {
    id: u32,
    /// Index into `Demons::kinds`.
    kind: usize,
    sim: Demon,
    web: AnimWebRuntime,
    parts: Vec<SkinPart>,
    entities: Vec<Entity>,
    /// Per part: its md6 mesh name and whether it is shown (gore wound mesh kits start hidden).
    part_names: Vec<String>,
    shown: Vec<bool>,
    joints: Vec<Mat4>,
    /// Each pose-tree leaf's frame position last update (root motion).
    leaf_frames: HashMap<String, f32>,
    /// Where it was spawned (the range respawns it there).
    spawn: (V, f32),
    hidden: bool,
    /// Last web node logged (RANCHER_TRACE).
    traced: String,
    brain: Option<Brain>,
    phys: AiPhysics,
    /// Last web node the brain requested ("sub/state"), and the via node of the attack being played.
    brain_node: String,
    attack_via: Option<String>,
    /// Forced attack node ("sub", "state") and the dest node requested once the web is in it.
    attack_follow: Option<((String, String), (String, String))>,
    /// Active ae_startSphereModelTrace* window.
    melee: Option<MeleeTrace>,
    /// The Imp's brain (rancher_sim::ai::imp) instead of `brain`.
    imp: Option<rancher_sim::ai::imp::ImpBrain>,
    /// The Possessed Soldier's brain (rancher_sim::ai::soldier) instead of `brain`.
    soldier: Option<rancher_sim::ai::soldier::SoldierBrain>,
    /// ae_launchItem joints fired this frame (launched once the pose is known).
    launches: Vec<String>,
}

#[derive(Resource)]
pub struct Demons {
    container: Arc<Container>,
    db: idres::decldb::DeclDb,
    vtm: VtMaterials,
    cvars: AiCvars,
    clips: Clips,
    kinds: Vec<DemonKind>,
    list: Vec<DemonInst>,
    parms: ParmsCache,
    next_id: u32,
    /// Where range demons stand: (entityDef, origin, yaw degrees).
    spawns: Vec<(String, V, f32)>,
    /// RANCHER_TRACE-style log of pains / deaths.
    pub log: bool,
    /// Self-test: RANCHER_DEMON_HITS=<t>:<damage decl>:<joint>[,...] damages the first demon at time t (s).
    hits: Vec<(f32, String, String)>,
    elapsed: f32,
    /// The map's AAS (aiConstants.movement.aasName) when a real map is loaded; the range has none.
    nav: Option<Nav>,
    map: Option<String>,
    player_scales: PlayerDamageScales,
    /// The player's health as the damage pipeline sees it (mirrored into the HUD).
    pub player_health: Option<f32>,
    /// No AI (RANCHER_DEMON_AI=0): the demon stands, as before.
    ai: bool,
    gravity: f32,
    difficulty: usize,
    /// ai_groundFriction_ContactFriction.
    contact_friction: f32,
    repulsor_cvars: RepulsorCvars,
    player_repulsor_cvars: PlayerRepulsorCvars,
    /// Demon projectiles in flight (the Imp's fireballs).
    fireballs: Vec<imp::Fireball>,
    /// Glory kills (glory.rs).
    glory: glory::Glory,
}

impl Demons {
    /// How a demon died, for its loot (pickups death drops): a glory kill when its sync's ae_kill killed it.
    pub fn kill_kind(&self, id: u32) -> rancher_sim::pickups::loot::KillKind {
        if self.glory.killed.contains(&id) { rancher_sim::pickups::loot::KillKind::Glory } else { rancher_sim::pickups::loot::KillKind::Normal }
    }
}

pub struct DemonsPlugin {
    pub doom: PathBuf,
    pub container: Arc<Container>,
    pub cvars: CvarValues,
    /// True on a real map (no range demons yet).
    pub map_mode: bool,
}

impl Plugin for DemonsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<DemonDamage>().init_resource::<DemonTargets>();
        // RANCHER_DEMON_AT=x,y[,yaw[,z]] places the demon (default: off the firing lane, facing the start), of the
        // entityDef RANCHER_DEMON_ENTITY (default the Possessed); on a real map (RANCHER_MAP) demons spawn only when
        // it is given.
        let parse_at = |var: &str| {
            std::env::var(var).ok().and_then(|s| {
                let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (v.len() >= 2).then(|| (V::new(v[0], v[1], v.get(3).copied().unwrap_or(0.0)), v.get(2).copied().unwrap_or(180.0)))
            })
        };
        let at = parse_at("RANCHER_DEMON_AT");
        if (self.map_mode && at.is_none()) || std::env::var("RANCHER_DEMONS").is_ok_and(|v| v == "0") {
            return;
        }
        let (pos, yaw) = at.unwrap_or((V::new(512.0, -160.0, 0.0), 180.0));
        let entity = std::env::var("RANCHER_DEMON_ENTITY").unwrap_or_else(|_| demons::POSSESSED.to_string());
        let mut spawns = vec![(entity, pos, yaw)];
        // RANCHER_IMP_AT=x,y[,yaw[,z]] adds an Imp (ai/demon/imp).
        if let Some((p, y)) = parse_at("RANCHER_IMP_AT") {
            spawns.push((rancher_sim::ai::imp::IMP.to_string(), p, y));
        }
        // RANCHER_SOLDIER_AT=x,y[,yaw[,z]] adds a Possessed Soldier (ai/hellified/marine_rifle).
        if let Some((p, y)) = parse_at("RANCHER_SOLDIER_AT") {
            spawns.push((rancher_sim::ai::soldier::SOLDIER.to_string(), p, y));
        }
        // RANCHER_DIFFICULTY=0..4 (EASY .. NIGHTMARE; default MEDIUM).
        let difficulty = std::env::var("RANCHER_DIFFICULTY").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(DIFFICULTY).min(4);
        let container = self.container.clone();
        let db = idres::decldb::DeclDb::new(container.clone());
        let player_scales = PlayerDamageScales::load(&db, &self.cvars, difficulty);
        let gravity = self.cvars.0.get("g_gravity").and_then(|v| idres::decl::parse_number(v)).unwrap_or(0.0);
        app.insert_resource(Demons {
            db,
            nav: None,
            map: if self.map_mode { std::env::var("RANCHER_MAP").ok() } else { None },
            player_scales,
            player_health: None,
            ai: !std::env::var("RANCHER_DEMON_AI").is_ok_and(|v| v == "0"),
            gravity,
            difficulty,
            contact_friction: self.cvars.0.get("ai_groundFriction_ContactFriction").and_then(|v| idres::decl::parse_number(v)).unwrap_or(100.0),
            repulsor_cvars: RepulsorCvars::load(&self.cvars),
            player_repulsor_cvars: PlayerRepulsorCvars::load(&self.cvars),
            container,
            vtm: VtMaterials::open(&self.doom),
            cvars: AiCvars::from_cvars(&self.cvars, difficulty),
            clips: Clips::default(),
            kinds: Vec::new(),
            list: Vec::new(),
            parms: ParmsCache::default(),
            next_id: 1,
            spawns,
            log: std::env::var("RANCHER_TRACE").is_ok_and(|v| v == "1"),
            hits: std::env::var("RANCHER_DEMON_HITS")
                .map(|v| {
                    v.split(',')
                        .filter_map(|h| {
                            let mut it = h.splitn(3, ':');
                            Some((it.next()?.trim().parse().ok()?, it.next()?.trim().to_string(), it.next()?.trim().to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            elapsed: 0.0,
            fireballs: Vec::new(),
            glory: glory::Glory::default(),
        })
        .add_systems(Startup, setup_demons)
        .init_resource::<SyncView>()
            .add_systems(Update, (apply_damage, tick_demons).chain().after(crate::combat::weapons_tick).before(crate::place_camera));
    }
}

fn load_kind(d: &mut Demons, entity: &str) -> anyhow::Result<DemonKind> {
    let def = DemonDef::load(&d.db, entity)?;
    let tags = WebTags::load(&d.container, &def.anim_web)?;
    let src = String::from_utf8_lossy(&d.container.read_by_name(&format!("generated/decls/animweb/{}.decl", def.anim_web))?).into_owned();
    let web = Arc::new(AnimWeb::parse(&src)?);
    let md6 = assets::read_md6def(&d.container, &def.md6)?;
    let model = assets::load_model(&d.container, &md6.mesh)?;
    let skel = model.skel.clone().ok_or_else(|| anyhow::anyhow!("{} has no skeleton", md6.mesh))?;
    let rig = Rig::new(skel);
    let bind = Locals::bind(&rig.skel);
    let spheres = demons::hit::spheres(&def.joint_groups, &rig.skel.names);
    let legs = LegRig::new(&rig.skel);
    let origin = rig.skel.names.iter().position(|n| n.eq_ignore_ascii_case("origin"));
    // The sub-webs the demon plays (hands_relaxed / hands_melee / hands_retaliate / hands_throw, rifle_* for the
    // Possessed Soldier: the AI's idle and
    // attacks).
    let subs = ["hands_combat", "hands_relaxed", "hands_melee", "hands_retaliate", "hands_throw", "rifle_combat", "rifle_relaxed", "rifle_melee", "falter", "stagger", "death", "knock_down", "stun_loop"];
    let mut data = AnimData::load(&d.container, &web, &[]);
    for name in subs {
        let Some(sw) = web.sub_web(name) else { continue };
        for n in &sw.nodes {
            for t in &n.trees {
                for a in &t.anims {
                    if let Some(clip) = d.clips.load(&d.container, &a.name) {
                        data.meta.insert(a.name.to_ascii_lowercase(), AnimMeta { num_frames: clip.num_frames, frame_rate: clip.frame_rate });
                    }
                }
            }
        }
    }
    let trace_groups = def.joint_groups.of_kind(demons::decl::GroupKind::Trace).map(|g| (g.name.to_ascii_lowercase(), demons::hit::trace_spheres(&def.joint_groups, &g.name, &rig.skel.names))).collect();
    let aim = AimJoints::load(&d.db, &rig.skel.names);
    Ok(DemonKind { entity: entity.to_string(), def, tags, web, data: Arc::new(data), rig, bind, offset: md6.offset, spheres, meshes: vec![(md6.mesh.clone(), model.model)], legs, origin, trace_groups, phys: physics_parms(d, entity)?, radii: AiRepulsorRadii::load(&d.db, entity), aim })
}

/// The AI physics constants: clip box = entityDef clipModelInfo size, gravity = g_gravity down, step height /
/// floor cosine from the AAS settings the monster's nav is built for (aiConstants.movement.aasName; the range has
/// no nav, so the intro's file, whose settings are the aas type's: maxStep 18, minFloorCos 0.7 for aas_monster48).
fn physics_parms(d: &Demons, entity: &str) -> anyhow::Result<AiPhysicsParms> {
    let e = d.db.get("entitydef", entity)?;
    let size = |k: &str| e.f32(&format!("edit.clipModelInfo.size.{k}")).unwrap_or(0.0);
    let size = V::new(size("x"), size("y"), size("z"));
    let aas_name = e.str("edit.aiConstants.movement.aasName").unwrap_or("aas_monster48").to_string();
    let intro;
    let settings = match &d.nav {
        Some(n) => &n.aas.settings,
        None => {
            intro = Nav::load(&d.container, "game/sp/intro/intro", &aas_name)?;
            &intro.aas.settings
        }
    };
    let friction = d.contact_friction;
    Ok(AiPhysicsParms::new(size, V::new(0.0, 0.0, -d.gravity), settings.max_step_height(), settings.min_floor_cos(), friction))
}

#[allow(clippy::too_many_arguments)]
fn spawn(d: &mut Demons, commands: &mut Commands, meshes: &mut Assets<Mesh>, mats: &mut Assets<VtMaterial>, images: &mut Assets<Image>, kind_index: usize, at: V, yaw: f32) {
    let Some(kind) = d.kinds.get(kind_index) else { return };
    let mut parts = Vec::new();
    let mut entities = Vec::new();
    let mut colors = HashMap::new();
    let mut part_names = Vec::new();
    let mut shown = Vec::new();
    let wound_meshes = kind.def.wound_meshes();
    for (_, model) in &kind.meshes {
        // skin_parts keeps the model's mesh order when nothing is filtered out.
        for ((p, mat), m6) in skin_parts(model, meshes, None, &[] as &[&str]).into_iter().zip(model.meshes.iter()) {
            let m = d
                .vtm
                .material(&mat, DEMON_TEXTURE_LEVEL, images, mats)
                .unwrap_or_else(|| crate::vtmat::flat(mats, StandardMaterial { base_color: assets::placeholder_color(&mat, &mut colors), perceptual_roughness: 0.8, ..default() }));
            let show = !wound_meshes.iter().any(|w| w.eq_ignore_ascii_case(&m6.name));
            let vis = if show { Visibility::Inherited } else { Visibility::Hidden };
            entities.push(commands.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(m), Transform::default(), vis, NoFrustumCulling)).id());
            parts.push(p);
            part_names.push(m6.name.clone());
            shown.push(show);
        }
    }
    let mut web = AnimWebRuntime::new(kind.web.clone(), kind.data.clone());
    // Monster webs use the base EdgeWeight (100), not idAnimWebHands' shoot/melee weights.
    web.hands_weights = false;
    let id = d.next_id;
    d.next_id += 1;
    let sim = Demon::new(kind.def.clone(), kind.tags.clone(), at, yaw, d.difficulty, d.cvars, 0x1234_5678 ^ id);
    let brain = if d.ai && kind.entity == demons::POSSESSED {
        Brain::load(&d.db, &kind.entity).map_err(|e| eprintln!("demons: brain: {e:#}")).ok()
    } else {
        None
    };
    let imp = if d.ai && kind.entity == rancher_sim::ai::imp::IMP {
        rancher_sim::ai::imp::ImpBrain::load(&d.db, &kind.entity, d.difficulty).map_err(|e| eprintln!("demons: imp brain: {e:#}")).ok()
    } else {
        None
    };
    let mut sim = sim;
    if kind.entity == rancher_sim::ai::soldier::SOLDIER {
        sim.set_combat_sub(rancher_sim::ai::soldier::COMBAT_SUBWEB);
    }
    let soldier = if d.ai && kind.entity == rancher_sim::ai::soldier::SOLDIER {
        rancher_sim::ai::soldier::SoldierBrain::load(&d.db, &kind.entity, 0x1234_5678 ^ id).map_err(|e| eprintln!("demons: soldier brain: {e:#}")).ok()
    } else {
        None
    };
    d.list.push(DemonInst { id, kind: kind_index, sim, web, parts, entities, part_names, shown, joints: Vec::new(), leaf_frames: HashMap::new(), spawn: (at, yaw), hidden: false, traced: String::new(), brain, phys: AiPhysics::default(), brain_node: String::new(), attack_via: None, attack_follow: None, melee: None, imp, soldier, launches: Vec::new() });
}

fn setup_demons(mut commands: Commands, mut d: ResMut<Demons>, mut sim: ResMut<crate::Sim>, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<VtMaterial>>, mut images: ResMut<Assets<Image>>) {
    let t0 = std::time::Instant::now();
    // Test hook: RANCHER_DEMON_WALL=x0,y0,z0,x1,y1,z1[;...] adds boxes to the collision world only (not rendered), e.g.
    // a wall between the demon's arm and the player for the melee sweep.
    if let Ok(s) = std::env::var("RANCHER_DEMON_WALL") {
        for b in s.split(';') {
            let v: Vec<f32> = b.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 6 {
                sim.world.add(rancher_sim::collision::Hull::cuboid(V::new(v[0], v[1], v[2]), V::new(v[3], v[4], v[5])));
                println!("[demons] test wall {v:?}");
            }
        }
    }
    // Test hook: RANCHER_PLAYER_HEALTH=<v> starts the player's health there (health-based loot, glory kill drops).
    if let Some(v) = std::env::var("RANCHER_PLAYER_HEALTH").ok().and_then(|v| v.parse::<f32>().ok()) {
        d.player_health = Some(v);
        println!("[demons] player health {v}");
    }
    let spawns = d.spawns.clone();
    // A real map's AAS for the AI's paths (aiConstants.movement.aasName; INTERIM: the first spawn's type).
    if let (Some(map), Some((entity, ..))) = (d.map.clone(), spawns.first()) {
        let aas = d.db.get("entitydef", entity).ok().and_then(|e| e.str("edit.aiConstants.movement.aasName").map(str::to_string)).unwrap_or_else(|| "aas_monster48".into());
        match Nav::load(&d.container, &map, &aas) {
            Ok(n) => d.nav = Some(n),
            Err(e) => eprintln!("demons: no nav: {e:#}"),
        }
    }
    for (entity, at, yaw) in spawns {
        let kind = match d.kinds.iter().position(|k| k.entity == entity) {
            Some(k) => k,
            None => match load_kind(&mut d, &entity) {
                Ok(k) => {
                    d.kinds.push(k);
                    let i = d.kinds.len() - 1;
                    if let Err(e) = glory::load(&mut d, i) {
                        eprintln!("demons: {entity}: no glory kills: {e:#}");
                    }
                    i
                }
                Err(e) => {
                    eprintln!("demons: {entity}: {e:#}");
                    continue;
                }
            },
        };
        spawn(&mut d, &mut commands, &mut meshes, &mut mats, &mut images, kind, at, yaw);
        eprintln!("demons: {entity} loaded in {:.2}s", t0.elapsed().as_secs_f32());
    }
}

/// Damage from the weapon code (idAI2::Damage per message).
fn apply_damage(mut d: ResMut<Demons>, mut msgs: ResMut<Messages<DemonDamage>>, sim: Res<crate::Sim>, time: Res<Time>) {
    let now = sim.game_ms;
    let d = &mut *d;
    // Self-test hits (from the player's origin, travelling player -> demon).
    d.elapsed += time.delta_secs();
    let due: Vec<(f32, String, String)> = d.hits.iter().filter(|h| h.0 <= d.elapsed).cloned().collect();
    d.hits.retain(|h| h.0 > d.elapsed);
    if let Some(first) = d.list.first() {
        let id = first.id;
        let target = first.sim.origin;
        for (_, decl, joint) in due {
            let from = sim.player.physics.origin;
            let event = DamageEvent {
                decl,
                traces: vec![demons::TraceHit { joint: Some(joint), point: target }],
                attacker_origin: Some(from),
                scale: 1.0,
                dir: (target - from).normalize_or(V::X),
                splash_fraction: -1.0,
            };
            msgs.write(DemonDamage { id, event });
        }
    }
    for m in msgs.drain() {
        let Some(inst) = d.list.iter_mut().find(|i| i.id == m.id) else { continue };
        let parms = match d.parms.get(&d.db, &m.event.decl) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("demons: {e:#}");
                continue;
            }
        };
        inst.sim.damage(&m.event, parms, now);
    }
}

fn apply_requests(inst: &mut DemonInst, default_blend: f32) {
    for r in std::mem::take(&mut inst.sim.requests) {
        match r {
            WebRequest::Set { sub, state } => {
                inst.web.set_state(&sub, &state);
            }
            WebRequest::Force { sub, state, blend_frames } => {
                let dest = if blend_frames < 0 { default_blend } else { blend_frames as f32 };
                let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: dest, ..Default::default() };
                inst.web.force_state(Some(&sub), &state, parms);
            }
            WebRequest::Change { sub, state, via } => {
                inst.web.request(Some(&sub), &state, via.as_ref().map(|(s, v)| (Some(s.as_str()), v.as_str())), 1, 1);
            }
            WebRequest::Scalar { name, value } => inst.web.set_scalar(&name, value),
        }
    }
}

/// The brain's view of the world: line of sight through the sim's collision (shot mask) and the map's nav.
struct LiveWorld<'a> {
    world: &'a rancher_sim::collision::World,
    nav: Option<&'a Nav>,
}

impl ai::World for LiveWorld<'_> {
    fn line_of_sight(&self, from: V, to: V) -> bool {
        let d = to - from;
        let len = d.length();
        len < 1e-3 || self.world.ray(from, d / len, len).is_none_or(|(t, _, _)| t >= len - 0.01)
    }
    fn nav(&self) -> Option<&Nav> {
        self.nav
    }
}

/// "web/sub/state" -> (sub, state).
fn split_node<'a>(web: &str, node: &'a str) -> Option<(&'a str, &'a str)> {
    node.strip_prefix(web)?.strip_prefix('/')?.split_once('/')
}

/// One brain tick (rancher_sim::ai::Brain, game ticks at 960/s): Body from the demon entity, Target from the
/// player (sight point = origin + clip-bounds centre, pm_normalheight / 2; half width pm_bboxwidth / 2). While the
/// demon is not in a pain / stagger / death reaction its output drives the web (ChangeState to the requested node,
/// ChangeStateVia for an attack's via node -> dest node, the blend-space scalars such as bodyMoveAngle) and the
/// body yaw; the entity itself moves only by root motion.
#[allow(clippy::too_many_arguments)]
fn think(inst: &mut DemonInst, sim: &crate::Sim, nav: Option<&Nav>, player_health: Option<f32>, web: &str, default_blend: f32, now: i32, dt: f32, log: bool) {
    let (body, target, idle) = ai_inputs(inst, sim, player_health);
    let po = target.origin;
    let Some(brain) = inst.brain.as_mut() else { return };
    let ticks = now as i64 * ai::TICKS_PER_SEC / 1000;
    let out = brain.tick(&LiveWorld { world: &sim.world, nav }, &body, &target, ticks, dt);
    apply_ai_output(inst, &out, &body, po, idle, web, default_blend, now, log);
}

/// The brain target for the player (sight point = origin + clip-bounds centre, pm_normalheight / 2; half width
/// pm_bboxwidth / 2) and the demon's body.
fn ai_inputs(inst: &DemonInst, sim: &crate::Sim, player_health: Option<f32>) -> (ai::Body, ai::Target, bool) {
    let p = &sim.player;
    let po = p.physics.origin;
    let target = ai::Target { origin: po, sight_point: po + V::Z * (p.cfg.normal_height * 0.5), alive: player_health.is_none_or(|h| h > 0.0), half_width: p.cfg.bbox_width * 0.5 };
    let dead = inst.sim.is_dead();
    let idle = matches!(inst.sim.phase, DemonPhase::Idle);
    (ai::Body { origin: inst.sim.origin, yaw: inst.sim.yaw, alive: !dead, in_pain: !idle && !dead }, target, idle)
}

/// Drives the demon's web and yaw from one brain output (any demon brain): trace log, ChangeState to the requested
/// node, the forced attack via node then its dest node, blend-space scalars.
#[allow(clippy::too_many_arguments)]
fn apply_ai_output(inst: &mut DemonInst, out: &ai::AiOutput, body: &ai::Body, po: V, idle: bool, web: &str, default_blend: f32, now: i32, log: bool) {
    if log {
        for e in &out.events {
            match e {
                AiEvent::MeleeTraceStart { joint_group, damage, player_damage_scale, .. } => println!("[demon {} {now}] brain melee window {joint_group} {damage} x{player_damage_scale}", inst.id),
                AiEvent::MeleeTraceEnd => println!("[demon {} {now}] brain melee window end", inst.id),
                e => println!("[demon {} {now}] ai {e:?} at ({:.1} {:.1} {:.1}) yaw {:.1}, player ({:.1} {:.1})", inst.id, body.origin.x, body.origin.y, body.origin.z, body.yaw, po.x, po.y),
            }
        }
    }
    if !idle {
        // The reaction owns the web; re-request the brain's node once it is back.
        inst.brain_node.clear();
        inst.attack_via = None;
        inst.attack_follow = None;
        inst.melee = None;
        return;
    }
    inst.sim.yaw = out.yaw;
    for (k, v) in &out.scalars {
        inst.web.set_scalar(k, *v);
    }
    for e in &out.events {
        let forced = match e {
            AiEvent::AttackStart { via_node, dest_node, .. } => Some((via_node, dest_node)),
            // idShared_ThrowProjectile enter 0x1405e71b0 requests throwAnim through the same AI web request
            // (0x140520dd0 / 0x14051f240). INTERIM: forced like an attack via node, so a throw right after a throw
            // replays the anim (the request's flags were not decoded).
            AiEvent::ThrowStart { node, dest_node, .. } => Some((node, dest_node)),
            _ => None,
        };
        if let Some((via_node, dest_node)) = forced
            && let (Some((vs, vn)), Some((ds, dn))) = (split_node(web, via_node), split_node(web, dest_node))
        {
            // The AI's web request 0x14051f2c0 with its force flag (bit 3) forces the via node (ForceState
            // 0x141705510 / force-via 0x1417046b0) instead of pathing to it: no edge leads into hands_melee.
            // INTERIM: the attack request's blend parms (request +0x50) are not decoded; the web's default blend.
            let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: default_blend, ..Default::default() };
            inst.web.force_state(Some(vs), vn, parms);
            inst.attack_follow = Some(((vs.to_string(), vn.to_string()), (ds.to_string(), dn.to_string())));
            inst.attack_via = Some(via_node.clone());
            inst.brain_node = dest_node.clone();
        }
    }
    // Once in the attack node, ask for the dest node: the node's own edge (end-relative window) leaves it.
    if let Some(((vs, vn), (ds, dn))) = inst.attack_follow.as_ref()
        && inst.web.current().is_some_and(|(s, n)| s == vs && n == vn)
    {
        inst.web.request(Some(ds), dn, None, 1, 1);
        inst.attack_follow = None;
    }
    // The attack is over once the brain no longer asks for its via node.
    if out.web_node.is_none() || out.web_node != inst.attack_via {
        inst.attack_via = None;
    }
    if let Some(n) = &out.web_node
        && inst.attack_via.as_ref() != Some(n)
        && *n != inst.brain_node
    {
        if let Some((s, st)) = split_node(web, n)
            && !inst.web.request(Some(s), st, None, 1, 1)
            && log
        {
            println!("[demon {} {now}] web has no node {n}", inst.id);
        }
        inst.brain_node = n.clone();
    }
}

/// ae_goreEnableByName "<wound>": show that wound's mesh kits.
fn apply_gore(inst: &mut DemonInst, kind: &DemonKind, wounds: &[String], vis: &mut Query<&mut Visibility>, log: bool, now: i32) {
    for w in wounds {
        if let Some((_, kits)) = kind.def.wounds.iter().find(|(n, _)| n.eq_ignore_ascii_case(w)) {
            for (i, n) in inst.part_names.iter().enumerate() {
                if !inst.shown[i] && kits.iter().any(|k| k.eq_ignore_ascii_case(n)) {
                    inst.shown[i] = true;
                    if let Ok(mut v) = vis.get_mut(inst.entities[i]) {
                        *v = Visibility::Inherited;
                    }
                }
            }
            if log {
                println!("[demon {} {now}] gore enable {w}", inst.id);
            }
        }
    }
}

/// The player's spawn id in repulsor records (distinct from every demon id).
const PLAYER_SPAWN_ID: u32 = 0xffff_0001;

/// Model (md6Def offset), then yaw about z, then the origin.
fn model_to_world(origin: V, yaw: f32, offset: [f32; 3]) -> Mat4 {
    Mat4::from_translation(origin) * Mat4::from_rotation_z(yaw.to_radians()) * Mat4::from_translation(V::from(offset))
}

/// Per frame: the pain / death update, the anim web on the game clock, pose + skinning, hit spheres.
#[allow(clippy::too_many_arguments)]
fn tick_demons(
    mut commands: Commands,
    mut d: ResMut<Demons>,
    mut sim: ResMut<crate::Sim>,
    mut targets: ResMut<DemonTargets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<VtMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut vis: Query<&mut Visibility>,
    mut hud: Option<ResMut<crate::swf_hud::HudState>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut xf: Query<&mut Transform>,
    defense: Option<Res<crate::pickups::PlayerDefense>>,
    actions: Res<crate::input::Actions>,
    mut sync_view: ResMut<SyncView>,
    combat: Option<Res<crate::combat::Combat>>,
) {
    let now = sim.game_ms;
    let d = &mut *d;
    // Glory kills: the candidate a melee press would start (last frame's hit volumes for the focus), the start on the
    // press (0x140d6b110 runs before the hands' melee), then the running sync (player body, camera, events).
    {
        let pl = &sim.player;
        let axis = rancher_sim::viewfx::angles_to_mat3(pl.view_angles);
        let (fwd, left, up) = (V::from(axis[0]), V::from(axis[1]), V::from(axis[2]));
        let weapon = combat.as_ref().map(|c| c.arsenal.defs[c.arsenal.current].decl.clone()).unwrap_or_default();
        let pressed = actions.pressed("_attack2");
        glory::evaluate(d, &targets, pl.physics.origin, !pl.falling(), pl.view_origin(), fwd, left, up, &weapon, pressed);
        sync_view.available = glory::available(d).is_some();
        if pressed && sync_view.available && glory::start(d, &mut commands, &mut meshes, &mut mats, &mut images, pl.physics.origin, pl.view_angles[1], now) && d.log {
            println!("[glory {now}] melee press started a glory kill");
        }
    }
    let player_frame = glory::update(d, &mut sync_view, &mut meshes, &mut vis, now);
    targets.demons.clear();
    let mut respawn = Vec::new();
    let mut melee_hits: Vec<(u32, String, f32)> = Vec::new();
    // The game's player repulsor list this frame (0x1403ade50; see repulsor::player_records).
    let pp = &sim.player.physics;
    let players = repulsor::player_records(PLAYER_SPAWN_ID, pp.origin, pp.shape().max.z - pp.shape().min.z, 0, &d.player_repulsor_cvars);
    let mut launched = Vec::new();
    for inst in &mut d.list {
        let kind = &d.kinds[inst.kind];
        let default_blend = kind.web.default_blend_duration;
        let offset = kind.offset;
        // A glory kill's victim (running or finished): the sync entity's web drives it (no requests, brain, physics).
        let synced = d.glory.pose_of(inst.id).is_some();
        if let Some(a) = d.glory.active.as_mut().filter(|a| a.victim == inst.id) {
            if a.kill && !a.killed {
                a.killed = true;
                inst.sim.sync_kill(now);
                d.glory.killed.insert(inst.id);
                if d.log {
                    println!("[demon {} {now}] glory kill: ae_kill, dead", inst.id);
                }
            }
            apply_gore(inst, kind, &a.gore, &mut vis, d.log, now);
        }
        if !synced {
            // Requests from spawn / damage first, so a pain issued this frame starts with this update.
            apply_requests(inst, default_blend);
        }
        let cur = inst.web.current().map(|(a, b)| (a.to_string(), b.to_string()));
        let events = inst.sim.update(now, cur.as_ref().map(|(a, b)| (a.as_str(), b.as_str())));
        let dt_s = sim.msec_last as f32 * 0.001;
        if synced {
            inst.sim.requests.clear();
        } else {
            apply_requests(inst, default_blend);
            if inst.imp.is_some() {
                imp::think(inst, &sim, d.nav.as_ref(), d.player_health, &kind.def.anim_web, default_blend, now, dt_s, d.log);
            } else if inst.soldier.is_some() {
                soldier::think(inst, &sim, d.nav.as_ref(), d.player_health, &kind.def.anim_web, default_blend, now, dt_s, d.log);
            } else {
                think(inst, &sim, d.nav.as_ref(), d.player_health, &kind.def.anim_web, default_blend, now, dt_s, d.log);
            }
        }
        let fired = if synced { Vec::new() } else { inst.web.update(now) };
        soldier::trigger_events(inst, &fired, now, d.log);
        // AnimEvent_StartSphereModelTrace (0x1403df200) .. ae_endSphereModelTrace from the playing melee anim.
        for f in &fired {
            let name = f.event.name.as_str();
            if name.starts_with("ae_startSphereModelTrace") {
                let group = f.event.param("string").and_then(|a| a.text()).unwrap_or("");
                let decl = f.event.param("damage").and_then(|a| a.text()).unwrap_or("");
                if d.log {
                    println!("[demon {} {now}] {name} {group} {decl} ({})", inst.id, f.anim);
                }
                inst.melee = (!inst.sim.is_dead()).then(|| MeleeTrace::new(group, decl, name.ends_with("WithAutoStop")));
            } else if name == "ae_endSphereModelTrace" {
                if let (true, Some(m)) = (d.log, inst.melee.as_ref()) {
                    println!("[demon {} {now}] {name} ({}) hit player {} blocked by world {} world-touch frames {}", inst.id, f.anim, m.hit_player, m.world_blocked, m.world_touches);
                }
                inst.melee = None;
            }
        }
        // ae_launchItem "<joint>" (idAI2::AnimEvent_LaunchItem 0x1403e9b60): launched below from this frame's pose.
        for f in &fired {
            if f.event.name == "ae_launchItem" && inst.imp.is_some() && !inst.sim.is_dead() {
                inst.launches.push(f.event.param("string").and_then(|a| a.text()).unwrap_or("").to_string());
            }
        }
        // ae_goreEnableByName "<wound>" (death anims): show that wound's mesh kits.
        let wounds: Vec<String> = fired.iter().filter(|f| f.event.name == "ae_goreEnableByName").filter_map(|f| f.event.param("string").and_then(|a| a.text()).map(str::to_string)).collect();
        apply_gore(inst, kind, &wounds, &mut vis, d.log, now);
        let mut trace = None;
        if d.log {
            let node = inst.web.current().map(|(a, b)| format!("{a}/{b}")).unwrap_or_default();
            if node != inst.traced {
                trace = Some(node.clone());
                let anim = match inst.web.pose_tree_model(0) {
                    Some(rancher_sim::animweb::PoseNode::Leaf { anim, .. }) => anim,
                    _ => String::new(),
                };
                println!("[demon {} {now}] web {node} {anim}", inst.id);
                inst.traced = node;
            }
        }
        for e in &events {
            match e {
                DemonEvent::Damaged(r) if d.log => println!("[demon {} {now}] damage {:.1} (base {:.1}) joint {:?} hp {:.1}", inst.id, r.health, r.base, r.joint, inst.sim.health),
                DemonEvent::Pain { reaction, sub, state } if d.log => println!("[demon {} {now}] pain {reaction:?} -> {sub}/{state}", inst.id),
                DemonEvent::Death { tags } if d.log => println!("[demon {} {now}] death tags {tags:?}", inst.id),
                DemonEvent::StaggerEnd if d.log => println!("[demon {} {now}] stagger end, hp {:.1}", inst.id, inst.sim.health),
                DemonEvent::Removed => {
                    respawn.push(inst.id);
                    if d.log {
                        trace = Some("removed".into());
                    }
                }
                _ => {}
            }
        }
        // Pose: the web's model-0 tree, evaluated like the view models; a glory kill's victim takes its sync tree and
        // its attach joint's placement.
        let node = inst.web.pose_tree_model(0);
        let sync_pose = glory::victim_pose(&d.glory, inst.id, kind, &d.clips);
        let mut loc = match &sync_pose {
            Some((l, o, y)) => {
                inst.sim.origin = *o;
                inst.sim.yaw = *y;
                inst.melee = None;
                l.clone()
            }
            None => crate::animweb::eval(node.as_ref(), &kind.bind, &d.clips),
        };
        // Root motion moves the entity; the mesh keeps the origin joint at identity.
        if let (Some(o), false) = (kind.origin, synced) {
            let mut next = HashMap::new();
            let (dt, dyaw) = root_delta(node.as_ref(), &d.clips, o as u16, &inst.leaf_frames, &mut next);
            inst.leaf_frames = next;
            // AnimFSM 0x1404ef900: velocity = delta * 1000 / msec into the AI physics, move type 0 (gravity).
            // INTERIM: under gravity only the x/y of the delta drive the body (the node delta modes DELTA_DEFAULT /
            // DELTA_FULL_GRAVITY / ... are not decoded; the anims' origin z offsets would lift it off the floor).
            let step = glam::Quat::from_rotation_z(inst.sim.yaw.to_radians()) * V::new(dt.x, dt.y, 0.0);
            // The AI's repulsors (0x1403e1af0): the players' records with playerRepulsorStyle / playerRepulsorRadius.
            // playerRepulsorStyle: idAI2 collision enable 0x1403dd000 sets RS_HARD_STOP (aiRepulsorRadius != 0, else 5 =
            // none); INTERIM: kept while dying (whether death disables collision through it was not traced).
            let style = if kind.radii.ai != 0.0 { 1 } else { 5 };
            let list = repulsor::ai_vs_player(&players, style, kind.radii.player, d.player_repulsor_cvars.enemy_radius);
            // Body: physics myRepulsor (+0x1f4; radius +0x204, height +0x208), from the AI's own record (radius
            // aiRepulsorRadius, height = clip bounds height; copied by 0x1403dccb0 in 0x1403e1af0). It only
            // matters for SUM_RADII records, which the AI's player copies clear.
            let body = Repulsed { owner: inst.id, radius: kind.radii.ai, height: kind.phys.hull.max.z - kind.phys.hull.min.z };
            let rep = Repulsion { body, list: &list, cvars: d.repulsor_cvars };
            let was = inst.phys.repulsed;
            inst.sim.origin = inst.phys.step_repulsed(&sim.world, &kind.phys, inst.sim.origin, step, true, dt_s, Some(&rep));
            if d.log && was != inst.phys.repulsed {
                let to = sim.player.physics.origin - inst.sim.origin;
                println!("[demon {} {now}] repulsor {} at xy distance {:.2} to the player", inst.id, if inst.phys.repulsed { "on" } else { "off" }, to.truncate().length());
            }
            inst.sim.yaw += dyaw.to_degrees();
            loc.rot[o] = Default::default();
            loc.trans[o] = Default::default();
        }
        let mut pb = PoseBuf::new(&kind.rig.skel, Some(&loc.to_pose()));
        kind.legs.apply(&mut pb);
        let joints = pb.mats();
        let m2w = model_to_world(inst.sim.origin, inst.sim.yaw, offset);
        if let Some(m) = inst.melee.as_mut() {
            let spheres = kind.trace_groups.get(&m.joint_group.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or(&[]);
            let boxes: Vec<TraceBox> = demons::hit::world_spheres(spheres, &joints, m2w).into_iter().map(|(centre, radius)| TraceBox { centre, radius }).collect();
            let p = &sim.player;
            let (w, po) = (p.cfg.bbox_width * 0.5, p.physics.origin);
            let (lo, hi) = (po + V::new(-w, -w, 0.0), po + V::new(w, w, p.cfg.normal_height));
            if m.update_in(Some(&sim.world), boxes, lo, hi) {
                melee_hits.push((inst.id, m.damage_decl.clone(), inst.sim.origin.distance_squared(po)));
            }
        }
        for joint in std::mem::take(&mut inst.launches) {
            let j = kind.rig.skel.names.iter().position(|n| n.eq_ignore_ascii_case(&joint));
            let hand = j.map_or(inst.sim.origin, |j| m2w.transform_point3(joints[j].w_axis.truncate()));
            if let Some(p) = imp::launch(inst, hand, &sim, d.gravity, d.log, now) {
                launched.push(p);
            }
        }
        launched.extend(soldier::fire(inst, &kind.rig.skel.names, &joints, m2w, &sim, now, d.log));
        let skin: Vec<Mat4> = joints.iter().zip(kind.rig.inv_bind.iter()).map(|(w, i)| m2w * *w * *i).collect();
        if let Some(what) = &trace {
            trace_pose(inst.id, now, what, &kind.rig.skel, &joints, m2w, inst.sim.origin, inst.sim.yaw);
        }
        let first = d.log && inst.joints.is_empty();
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for (i, p) in inst.parts.iter().enumerate() {
            if !inst.shown[i] {
                continue;
            }
            if let Some(mut mesh) = meshes.get_mut(&p.mesh) {
                p.skin(&skin, V::ZERO, &mut mesh);
                if first {
                    if let Some(bevy::mesh::VertexAttributeValues::Float32x3(v)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
                        for q in v {
                            lo = lo.min(Vec3::from(*q));
                            hi = hi.max(Vec3::from(*q));
                        }
                    }
                }
            }
        }
        if first {
            // Bevy space (x right, y up, z back): idTech x = -z, y = -x, z = y.
            println!("[demon {} {now}] skinned bounds idTech x {:.1}..{:.1} y {:.1}..{:.1} z {:.1}..{:.1}", inst.id, -hi.z, -lo.z, -hi.x, -lo.x, lo.y, hi.y);
            for j in ["origin", "hips", "leftfoot", "lefttoebase", "rightfoot", "head"] {
                if let Some(i) = kind.rig.skel.names.iter().position(|n| n.eq_ignore_ascii_case(j)) {
                    let w = joints[i].w_axis;
                    let b = kind.rig.inv_bind[i].inverse().w_axis;
                    println!("[demon {} {now}] joint {j}: pose ({:.1} {:.1} {:.1}) bind ({:.1} {:.1} {:.1})", inst.id, w.x, w.y, w.z, b.x, b.y, b.z);
                }
            }
        }
        inst.joints = joints;
        // Corpse: hidden once removeTime + fadeOutTime have passed (no fade shader yet).
        let hide = matches!(inst.sim.phase, DemonPhase::Removed);
        if hide != inst.hidden {
            inst.hidden = hide;
            for (i, e) in inst.entities.iter().enumerate() {
                if let Ok(mut v) = vis.get_mut(*e) {
                    *v = if hide || !inst.shown[i] { Visibility::Hidden } else { Visibility::Inherited };
                }
            }
        }
        // A glory kill's victim stays a target until its ae_kill (pickups see the death then, as a glory kill).
        if !inst.sim.is_dead() {
            let spheres: Vec<(V, f32, String)> = demons::hit::world_spheres(&kind.spheres, &inst.joints, m2w).into_iter().zip(kind.spheres.iter()).map(|((c, r), s)| (c, r, s.joint_name.clone())).collect();
            let (mut lo, mut hi) = (V::splat(f32::MAX), V::splat(f32::MIN));
            for (c, r, _) in &spheres {
                lo = lo.min(*c - V::splat(*r));
                hi = hi.max(*c + V::splat(*r));
            }
            let o = inst.sim.origin;
            let clip_bounds = (o + kind.phys.hull.min, o + kind.phys.hull.max);
            let aim_points = kind.aim.world(&inst.joints, m2w, o, clip_bounds);
            targets.demons.push(DemonTarget { id: inst.id, spheres, origin: o, bounds: (lo, hi), aim_points, clip_bounds });
        }
    }
    // Melee hits on the player: idPlayer::Damage (damage scale 1, no armour), health mirrored into the HUD.
    for (id, decl, d2) in melee_hits {
        let parms = match d.parms.get(&d.db, &decl) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("demons: {e:#}");
                continue;
            }
        };
        let dmg = demons::live::player_damage(&parms, d2, 1.0, &d.player_scales);
        let before = d.player_health.or(hud.as_ref().map(|h| h.health)).unwrap_or(100.0);
        // The suit's damage scale and the armour split (shared player damage path, crate::pickups::hit_player).
        let hit = crate::pickups::hit_player(&mut d.player_health, hud.as_deref_mut(), defense.as_deref(), dmg);
        // sync_useNoPlayerDeath 1: the sync sets the player's no-death bit (player +0x45ea8 & 8, 0x140a46d30), which
        // makes idPlayer::Damage pass "cannot kill" to the health component. INTERIM: the floor it keeps (1).
        if sync_view.active {
            let floor = before.min(1.0);
            if d.player_health.is_some_and(|h| h < floor) {
                d.player_health = Some(floor);
                if let Some(h) = hud.as_mut() {
                    h.health = floor;
                }
            }
        }
        if d.log {
            let ((h0, h1), (a0, a1)) = (hit.health, hit.armor);
            println!("[demon {id} {now}] melee hit player: {decl} damage {dmg:.2} (difficulty x{:.2}) health {h0:.1} -> {h1:.1} armour {a0:.1} -> {a1:.1}", d.player_scales.difficulty);
        }
    }
    // The player follows its attach joint while the sync holds it; on release it stands there looking along the
    // sync camera.
    if let Some(o) = player_frame.hold {
        sim.player.physics.origin = o;
        sim.player.physics.velocity = V::ZERO;
    }
    if let Some((o, ang)) = player_frame.release {
        sim.player.physics.origin = o;
        sim.player.physics.velocity = V::ZERO;
        sim.player.view_angles = ang;
    }
    glory::finish(d, &mut commands);
    glory::glow(d, &mut commands, &mut std_mats, &mut vis, now);
    for p in launched {
        let fb = imp::spawn_fireball(&mut commands, &mut meshes, &mut std_mats, p);
        d.fireballs.push(fb);
    }
    imp::fly(d, &sim, &mut hud, defense.as_deref(), &mut commands, &mut xf, now);
    // Range: a removed demon is replaced by a fresh one at its spot.
    for id in respawn {
        if let Some(i) = d.list.iter().position(|x| x.id == id) {
            let old = d.list.remove(i);
            for e in old.entities {
                commands.entity(e).despawn();
            }
            glory::forget(d, id, &mut commands);
            spawn(d, &mut commands, &mut meshes, &mut mats, &mut images, old.kind, old.spawn.0, old.spawn.1);
        }
    }
}
