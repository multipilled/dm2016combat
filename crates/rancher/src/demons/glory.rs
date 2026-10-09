//! Glory kills in the testbed: the sync entity (its anim web drives the player's body, the camera and the victim),
//! the choice on the melee press, the stagger glow, and the hand-over to the player / pickups. Decoded rules live in
//! rancher_sim::demons::glory; notes in gamedata/re/DEMONS.md section 16.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use glam::Mat4;
use idres::animweb::AnimWeb;
use rancher_sim::Vec3 as V;
use rancher_sim::animweb::{AnimData, AnimMeta, AnimWebRuntime, PoseNode};
use rancher_sim::demons::decl::GroupKind;
use rancher_sim::demons::glory::{self as gk, Candidate, DeltaCorrection, Situation, SyncMelee, participant, quadrant};

use super::{DemonKind, DemonTargets, Demons};
use crate::animweb::{Clips, Locals};
use crate::assets;
use crate::rig::PoseBuf;
use crate::viewanim::{Rig, SkinPart, skin_parts};
use crate::vtmat::VtMaterial;

/// What the player side reads while a glory kill is available or running (crates/rancher/src/main.rs place_camera /
/// tick, combat.rs weapons_tick).
#[derive(Resource, Default, Debug, Clone)]
pub struct SyncView {
    /// A sync holds the player (bypassPlayerPhysics: sync_forceBypassPlayerPhysics 1): no player physics, weapons
    /// or hands until ae_releasePlayerFromSync.
    pub active: bool,
    /// The view from the player body's `camera` joint (ae_attachCamera .. ae_detachCamera): idTech origin and axis
    /// rows (forward, left, up).
    pub view: Option<([f32; 3], [[f32; 3]; 3])>,
    /// sync_autoFOV 1: the FOV forced during the kill (sync_autoFOVValue 90).
    pub fov: Option<f32>,
    /// A glory kill is available this frame: the melee press starts it instead of the hands' melee
    /// (hands_syncMeleeInterruptsAll 1, 0x140d6b110 runs before the regular melee).
    pub available: bool,
}

/// One sync entity type with the player body it animates.
pub(super) struct SyncAssets {
    entity_def: String,
    web: Arc<AnimWeb>,
    data: Arc<AnimData>,
    ent_skel: idres::md6::Md6Skel,
    ent_bind: Locals,
    /// attach joints of the player (participant "player") and of the victim (type TARGET).
    attach: [usize; 2],
    body: Rig,
    body_bind: Locals,
    body_model: idres::md6::Md6Model,
    body_offset: [f32; 3],
    body_origin: Option<usize>,
    camera: usize,
    /// modelInfos index of the player body and of the victim's model.
    body_index: usize,
    victim_index: usize,
    /// (sub, state) -> the sync entity's attach joint transforms at frame 0 [player, victim] (model space).
    frame0: HashMap<(String, String), [Mat4; 2]>,
    /// The body's anims retargeted onto its mesh skeleton (lowercase anim name).
    body_clips: HashMap<String, Arc<idres::md6anim::Md6Anim>>,
}

/// A running glory kill.
pub(super) struct Active {
    pub victim: u32,
    kind: usize,
    name: String,
    node: (String, String),
    web: AnimWebRuntime,
    /// The sync entity's world transform (placed at the start).
    sync: Mat4,
    /// Delta correction of [player, victim] toward their attach joints (from the sync entity's events).
    corr: [Option<DeltaCorrection>; 2],
    camera: bool,
    pub released: bool,
    pub ended: bool,
    body_parts: Vec<SkinPart>,
    body_entities: Vec<Entity>,
    /// ae_kill fired for the victim this frame / earlier.
    pub kill: bool,
    pub killed: bool,
    /// ae_goreEnableByName wounds fired this frame for the victim.
    pub gore: Vec<String>,
    log_frame: i32,
}

/// The glory-kill state of the range.
#[derive(Default)]
pub(super) struct Glory {
    sync: HashMap<usize, SyncMelee>,
    assets: HashMap<usize, SyncAssets>,
    pub active: Option<Active>,
    /// Finished syncs of dead victims: their last pose holds (no ragdoll here).
    pub corpses: Vec<Active>,
    /// Demon ids killed by a glory kill (pickups: KillKind::Glory).
    pub killed: HashSet<u32>,
    /// The candidate a melee press would start (victim id, kind, interaction index, attacker position).
    choice: Option<(u32, usize, Candidate)>,
    overlays: HashMap<u32, Vec<Entity>>,
    overlay_mats: HashMap<u32, Handle<StandardMaterial>>,
}

impl Glory {
    pub fn victim(&self) -> Option<u32> {
        self.active.as_ref().map(|a| a.victim)
    }

    /// The sync driving this demon's pose (running, or finished on its corpse).
    pub fn pose_of(&self, id: u32) -> Option<&Active> {
        self.active.iter().chain(self.corpses.iter()).find(|a| a.victim == id)
    }

    pub fn has_sync(&self, kind: usize) -> bool {
        self.assets.contains_key(&kind)
    }
}

fn joint(skel: &idres::md6::Md6Skel, name: &str) -> Option<usize> {
    skel.names.iter().position(|n| n.eq_ignore_ascii_case(name))
}

/// Loads the sync melee data of one demon kind (aiConstants.syncMelee, its first sync entityDef, the player body).
pub(super) fn load(d: &mut Demons, kind_index: usize) -> anyhow::Result<()> {
    let kind: &DemonKind = &d.kinds[kind_index];
    let sm = SyncMelee::load(&d.db, &kind.entity)?;
    let Some(ent_def) = sm.entity_defs.first().cloned() else { anyhow::bail!("{}: no syncMeleeEntityDefs", kind.entity) };
    let e = d.db.get("entitydef", &ent_def)?;
    let web_name = e.str("edit.animWeb").ok_or_else(|| anyhow::anyhow!("{ent_def}: no animWeb"))?.to_string();
    let ent_md6 = e.str("edit.renderModelInfo.model").ok_or_else(|| anyhow::anyhow!("{ent_def}: no model"))?.to_string();
    // Participants: the player and the TARGET's attach joints.
    let mut attach_names = [String::new(), String::new()];
    if let Some(p) = e.block("edit.participants") {
        for (k, v) in &p.items {
            let (Some(_), Some(b)) = (k.strip_prefix("item["), v.as_block()) else { continue };
            let jn = b.str("attachJointName").unwrap_or("").to_string();
            if b.str("entityDef") == Some("player") {
                attach_names[0] = jn;
            } else if b.str("type") == Some("TARGET") && attach_names[1].is_empty() {
                attach_names[1] = jn;
            }
        }
    }
    let src = String::from_utf8_lossy(&d.container.read_by_name(&format!("generated/decls/animweb/{web_name}.decl"))?).into_owned();
    let web = Arc::new(AnimWeb::parse(&src)?);
    let model_idx = |m: &str| web.model_infos.iter().position(|x| x.eq_ignore_ascii_case(m));
    let body_md6 = "player/tp_body.md6";
    let body_index = model_idx(body_md6).ok_or_else(|| anyhow::anyhow!("{web_name}: no {body_md6}"))?;
    // The victim's model: its md6Def name, else the first zombie-family model after the body (INTERIM fallback).
    let victim_index = model_idx(&kind.def.md6).unwrap_or(body_index + 1);
    let ent_index = model_idx(&ent_md6).unwrap_or(0);
    // Sync entity skeleton.
    let ent_def6 = assets::read_md6def(&d.container, &ent_md6)?;
    let ent_model = assets::load_model(&d.container, &ent_def6.mesh)?;
    let ent_skel = ent_model.skel.ok_or_else(|| anyhow::anyhow!("{ent_md6}: no skeleton"))?;
    let attach = [joint(&ent_skel, &attach_names[0]), joint(&ent_skel, &attach_names[1])];
    let [Some(a0), Some(a1)] = attach else { anyhow::bail!("{ent_md6}: attach joints {attach_names:?} missing") };
    let ent_bind = Locals::bind(&ent_skel);
    // Player body (the arms mesh with the camera joint).
    // The player participant renders the player's own model, the hands model (player entityDef handsModelDefault:
    // the Praetor suit), with the sync web's tp_body trees; tp_body.md6's own mesh is a placeholder (material
    // models/characters/player_naked). INFERRED from that material; the participant's model switch was not traced.
    let body_def = assets::read_md6def(&d.container, body_md6)?;
    let hands_md6 = d.db.get("entitydef", "player").ok().and_then(|e| e.str("edit.handsModelDefault").map(str::to_string));
    let mesh = hands_md6.as_deref().and_then(|m| assets::read_md6def(&d.container, m).ok()).map_or(body_def.mesh.clone(), |m| m.mesh);
    let body_loaded = assets::load_model(&d.container, &mesh)?;
    let body_skel = body_loaded.skel.ok_or_else(|| anyhow::anyhow!("{body_md6}: no skeleton"))?;
    let camera = joint(&body_skel, "camera").ok_or_else(|| anyhow::anyhow!("{body_md6}: no camera joint"))?;
    let body_origin = joint(&body_skel, "origin");
    let body = Rig::new(body_skel);
    let body_bind = Locals::bind(&body.skel);
    // Anims of the killedByPlayer nodes for the three models, and their lengths.
    let mut data = AnimData::load(&d.container, &web, &[]);
    let mut frame0 = HashMap::new();
    let mut body_clips = HashMap::new();
    let mut skels: HashMap<String, Option<idres::md6::Md6Skel>> = HashMap::new();
    if let Some(sw) = web.sub_web("killedByPlayer") {
        for n in &sw.nodes {
            for t in &n.trees {
                if ![ent_index, body_index, victim_index].contains(&t.model_index) {
                    continue;
                }
                for a in &t.anims {
                    if let Some(clip) = d.clips.load(&d.container, &a.name) {
                        data.meta.insert(a.name.to_ascii_lowercase(), AnimMeta { num_frames: clip.num_frames, frame_rate: clip.frame_rate });
                        // The body's anims are authored for another skeleton (marine.md6skl): by joint name onto the
                        // arms mesh's (rancher_sim::demons::glory::retarget).
                        if t.model_index == body_index {
                            let from = skels.entry(clip.skeleton.clone()).or_insert_with(|| {
                                let r = assets::skeleton_resource(&clip.skeleton);
                                d.container.read_by_name(&r).ok().and_then(|b| idres::md6::Md6Skel::parse(&b).ok())
                            });
                            let rt = match from {
                                Some(f) if f.names != body.skel.names => gk::retarget(&clip, f, &body.skel),
                                _ => (*clip).clone(),
                            };
                            body_clips.insert(a.name.to_ascii_lowercase(), Arc::new(rt));
                        }
                    }
                }
            }
            // The attach joints at frame 0 of the sync entity's tree.
            let leaf = n.trees.iter().find(|t| t.model_index == ent_index).and_then(|t| t.anims.first());
            if let Some(a) = leaf {
                let node = PoseNode::Leaf { anim: a.name.clone(), frame: 0, frac: 0.0 };
                let loc = crate::animweb::eval(Some(&node), &ent_bind, &d.clips);
                let mats = PoseBuf::new(&ent_skel, Some(&loc.to_pose())).mats();
                if d.log && (n.state == "kill_front_head" || n.state == "kill_back_upper") {
                    let f = |m: Mat4| format!("({:.1} {:.1} {:.1}) x ({:.2} {:.2} {:.2}) y ({:.2} {:.2} {:.2})", m.w_axis.x, m.w_axis.y, m.w_axis.z, m.x_axis.x, m.x_axis.y, m.x_axis.z, m.y_axis.x, m.y_axis.y, m.y_axis.z);
                    println!("[glory] {} frame 0 ({}): {} {} / {} {}; joint 0 {} {}", n.state, a.name, ent_skel.names[a0], f(mats[a0]), ent_skel.names[a1], f(mats[a1]), ent_skel.names[0], f(mats[0]));
                }
                frame0.insert(("killedByPlayer".to_string(), n.state.clone()), [mats[a0], mats[a1]]);
            }
        }
    }
    let assets_ = SyncAssets {
        entity_def: ent_def,
        web,
        data: Arc::new(data),
        ent_skel,
        ent_bind,
        attach: [a0, a1],
        body,
        body_bind,
        body_model: body_loaded.model,
        body_offset: body_def.offset,
        body_origin,
        camera,
        body_index,
        victim_index,
        frame0,
        body_clips,
    };
    if d.log {
        println!("[glory] {}: {} interactions, sync {} web {} (models: body {} victim {}), {} kill nodes", d.kinds[kind_index].entity, sm.interactions.len(), assets_.entity_def, web_name, assets_.body_index, assets_.victim_index, assets_.frame0.len());
    }
    d.glory.sync.insert(kind_index, sm);
    d.glory.assets.insert(kind_index, assets_);
    Ok(())
}

/// The idTech (forward, left, up) rows of a transform.
fn rows(m: Mat4) -> [[f32; 3]; 3] {
    let (x, y, z) = (m.x_axis, m.y_axis, m.z_axis);
    [[x.x, x.y, x.z], [y.x, y.y, y.z], [z.x, z.y, z.z]]
}

/// Picks the glory kill a melee press would start this frame (0x140ea52c0 / 0x140ea57f0): the demons in their
/// vulnerable stagger within reach, their eligible interactions, the highest priority then the nearest projected
/// attacker position. `eye`, `fwd`, `left`, `up`: the player's view. Uses last frame's hit volumes for the focus.
#[allow(clippy::too_many_arguments)]
pub(super) fn evaluate(d: &mut Demons, targets: &DemonTargets, player: V, on_ground: bool, eye: V, fwd: V, left: V, up: V, weapon: &str, pressed: bool) {
    d.glory.choice = None;
    if d.glory.active.is_some() {
        return;
    }
    let focus_hit = targets.trace(eye, fwd, 4096.0);
    let mut all: Vec<(u32, usize, Candidate)> = Vec::new();
    for inst in &d.list {
        if inst.sim.is_dead() || !inst.sim.stagger_vulnerable() {
            continue;
        }
        let (Some(sm), Some(sa)) = (d.glory.sync.get(&inst.kind), d.glory.assets.get(&inst.kind)) else { continue };
        let kind = &d.kinds[inst.kind];
        // Focus: the damage group of the hit volume under the crosshair (INTERIM: the focus / screen-quadrant code of
        // the candidate validation was not decoded; the quadrant is the crosshair's side of the demon's hit-volume
        // centre as seen on screen).
        let focus = focus_hit.as_ref().filter(|h| h.id == inst.id).map(|h| {
            let g = kind.def.joint_groups.group_of(GroupKind::Damage, &h.joint).map(|g| g.name.to_ascii_lowercase()).unwrap_or_default();
            (g, h.joint.to_ascii_lowercase())
        });
        let centre = targets.demons.iter().find(|t| t.id == inst.id).map_or(inst.sim.origin, |t| (t.bounds.0 + t.bounds.1) * 0.5);
        let along = (centre - eye).dot(fwd).max(0.0);
        let aim = eye + fwd * along - centre;
        let top = aim.dot(up) >= 0.0;
        let leftq = aim.dot(left) >= 0.0;
        let q = match (top, leftq) {
            (true, true) => quadrant::TOP_LEFT,
            (true, false) => quadrant::TOP_RIGHT,
            (false, true) => quadrant::BOTTOM_LEFT,
            (false, false) => quadrant::BOTTOM_RIGHT,
        };
        let s = Situation {
            input: "INPUT_ATTACK2",
            victim_stagger_1: true,
            victim_health_ratio: inst.sim.health_fraction(),
            player_state: if on_ground { participant::ON_GROUND } else { participant::IN_AIR },
            weapon: weapon.to_string(),
            focus,
            focus_quadrant: q,
            to_player: player - inst.sim.origin,
        };
        if d.log && pressed {
            println!("[glory {}] press: focus {:?} quadrant {} rel {:.1} {:.1} {:.1} health {:.2}", inst.id, s.focus, s.focus_quadrant, s.to_player.x, s.to_player.y, s.to_player.z, s.victim_health_ratio);
        }
        for (i, si) in sm.interactions.iter().enumerate() {
            if !gk::eligible(si, &s) {
                continue;
            }
            let Some(node) = si.anims.first() else { continue };
            let Some(f0) = sa.frame0.get(node) else { continue };
            let (_, attacker) = gk::place(inst.sim.origin, inst.sim.yaw, si.clockwise_deg, f0[1], f0[0]);
            if d.log && pressed {
                println!("[glory {}]   candidate {} priority {} attacker ({:.1} {:.1} {:.1}) dist {:.1}", inst.id, si.name, si.priority, attacker.x, attacker.y, attacker.z, attacker.distance(player));
            }
            all.push((inst.id, inst.kind, Candidate { index: i, priority: si.priority, attacker }));
        }
    }
    let cands: Vec<Candidate> = all.iter().map(|c| c.2).collect();
    d.glory.choice = gk::choose(&cands, player).and_then(|w| all.iter().find(|c| c.2 == w).copied());
}

/// Whether a melee press would start a glory kill (and on which demon).
pub(super) fn available(d: &Demons) -> Option<u32> {
    d.glory.choice.map(|(id, _, _)| id)
}

/// Starts the chosen glory kill (0x140ea3860 -> idSyncEntity): places the sync entity on the victim, plays the
/// interaction's node and spawns the player body.
pub(super) fn start(d: &mut Demons, commands: &mut Commands, meshes: &mut Assets<Mesh>, mats: &mut Assets<VtMaterial>, images: &mut Assets<Image>, player: V, player_yaw: f32, now: i32) -> bool {
    let Some((id, kind, c)) = d.glory.choice.take() else { return false };
    let (Some(sm), Some(sa)) = (d.glory.sync.get(&kind), d.glory.assets.get(&kind)) else { return false };
    let si = &sm.interactions[c.index];
    let Some(node) = si.anims.first().cloned() else { return false };
    let Some(f0) = sa.frame0.get(&node).copied() else { return false };
    let Some(inst) = d.list.iter().find(|i| i.id == id) else { return false };
    let (sync, attacker) = gk::place(inst.sim.origin, inst.sim.yaw, si.clockwise_deg, f0[1], f0[0]);
    let mut web = AnimWebRuntime::new(sa.web.clone(), sa.data.clone());
    web.hands_weights = false;
    // The runtime's clock first: set_state starts the node at it.
    web.update(now);
    web.set_state(&node.0, &node.1);
    // Delta correction windows from the sync entity anim's events (attach00 = player, attach01 = victim).
    let mut corr: [Option<DeltaCorrection>; 2] = [None, None];
    let ent_anim = sa.web.sub_web(&node.0).and_then(|sw| sw.nodes.iter().find(|n| n.state == node.1)).and_then(|n| n.trees.iter().find(|t| t.model_index == 0)).and_then(|t| t.anims.first()).map(|a| a.name.to_ascii_lowercase());
    let events = ent_anim.as_ref().and_then(|a| sa.data.events.get(&0).and_then(|m| m.get(a))).cloned().unwrap_or_default();
    let jn = |k: usize| sa.ent_skel.names[sa.attach[k]].to_ascii_lowercase();
    // INTERIM: the player is pulled onto attach00's track from frame 1 to 4; at frame 0 that joint is ~115 units
    // from the victim (the kill opens with a lunge), so a player standing closer is pulled back. The game's handling
    // of the lunge (checkLungeDistance, commonSlideMove, jointToCenterSlideScale, start frame) was not decoded.
    let player_attach = sync * f0[0];
    let starts = [(player - attacker, gk::wrap_deg(player_yaw - gk::yaw_of(player_attach))), (V::ZERO, 0.0)];
    for (k, slot) in corr.iter_mut().enumerate() {
        let start = events.iter().find(|e| e.name == "ae_animSyncStartDeltaCorrection" && e.param("string").and_then(|a| a.text()).map(str::to_ascii_lowercase) == Some(jn(k)));
        let end = events.iter().find(|e| e.name == "ae_animSyncDeltaCorrectionEndPos" && e.param("string").and_then(|a| a.text()).map(str::to_ascii_lowercase) == Some(jn(k)));
        if let (Some(s), Some(e)) = (start, end) {
            *slot = Some(DeltaCorrection { start: s.frame, end: e.frame, offset: starts[k].0, yaw_offset: starts[k].1 });
        }
    }
    // The player body (the arms mesh of player/tp_body.md6).
    let mut parts = Vec::new();
    let mut ents = Vec::new();
    let mut colors = HashMap::new();
    for (p, mat) in skin_parts(&sa.body_model, meshes, None, &[] as &[&str]) {
        let m = d.vtm.material(&mat, super::DEMON_TEXTURE_LEVEL, images, mats).unwrap_or_else(|| crate::vtmat::flat(mats, StandardMaterial { base_color: assets::placeholder_color(&mat, &mut colors), ..default() }));
        ents.push(commands.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(m), Transform::default(), Visibility::Hidden, NoFrustumCulling)).id());
        parts.push(p);
    }
    if d.log {
        let to = player - inst.sim.origin;
        println!("[glory {id} {now}] start {} -> {}/{} (priority {}, player at {:.1} {:.1} {:.1} rel, attacker spot {:.1} {:.1} {:.1}, slide {:.1})", si.name, node.0, node.1, si.priority, to.x, to.y, to.z, attacker.x, attacker.y, attacker.z, (player - attacker).length());
    }
    d.glory.active = Some(Active {
        victim: id,
        kind,
        name: si.name.clone(),
        node,
        web,
        sync,
        corr,
        camera: false,
        released: false,
        ended: false,
        body_parts: parts,
        body_entities: ents,
        kill: false,
        killed: false,
        gore: Vec::new(),
        log_frame: -1,
    });
    true
}

/// The sync entity's model-0 leaf frame (for the delta corrections).
fn ent_frame(web: &AnimWebRuntime) -> f32 {
    match web.pose_tree_model(0) {
        Some(PoseNode::Leaf { frame, frac, .. }) => frame as f32 + frac,
        _ => 0.0,
    }
}

/// A participant's world transform: its attach joint, minus the delta-correction offset still left.
fn participant(a: &Active, sa: &SyncAssets, ent: &[Mat4], k: usize, frame: f32) -> Mat4 {
    let w = a.sync * ent[sa.attach[k]];
    let Some(c) = a.corr[k] else { return w };
    let s = c.weight(frame);
    let pos = w.w_axis.truncate() + c.offset * s;
    let yaw = (gk::yaw_of(w) + c.yaw_offset * s).to_radians();
    Mat4::from_rotation_translation(glam::Quat::from_rotation_z(yaw), pos)
}

/// What the running sync did to the player this frame.
pub(super) struct PlayerFrame {
    /// The player's origin (feet) while held, and the release (origin, view angles) when ae_releasePlayerFromSync
    /// fired.
    pub hold: Option<V>,
    pub release: Option<(V, [f32; 3])>,
}

/// Advances the running sync: its web, the events of the three models, the player body and the camera.
#[allow(clippy::too_many_arguments)]
pub(super) fn update(d: &mut Demons, view: &mut SyncView, meshes: &mut Assets<Mesh>, vis: &mut Query<&mut Visibility>, now: i32) -> PlayerFrame {
    let mut out = PlayerFrame { hold: None, release: None };
    let log = d.log;
    let clips = &d.clips;
    let Some(a) = d.glory.active.as_mut() else {
        view.active = false;
        view.view = None;
        view.fov = None;
        return out;
    };
    let Some(sa) = d.glory.assets.get(&a.kind) else { return out };
    let fired = a.web.update(now);
    a.gore.clear();
    for f in &fired {
        let name = f.event.name.as_str();
        let model = f.model_index;
        if model == sa.victim_index {
            match name {
                "ae_kill" => a.kill = true,
                "ae_goreEnableByName" => {
                    if let Some(w) = f.event.param("string").and_then(|x| x.text()) {
                        a.gore.push(w.to_string());
                    }
                }
                _ => {}
            }
        } else if model == sa.body_index {
            match name {
                "ae_attachCamera" => a.camera = true,
                "ae_detachCamera" => a.camera = false,
                "ae_releasePlayerFromSync" => a.released = true,
                _ => {}
            }
        } else if model == 0 && name == "ae_syncEnd" {
            a.ended = true;
        }
        if log && (model == 0 || model == sa.body_index || model == sa.victim_index) && !name.starts_with("ae_animSyncBounds") {
            println!("[glory {} {now}] event {name} (model {model}, {})", a.victim, f.anim);
        }
    }
    let frame = ent_frame(&a.web);
    // Sync entity pose -> attach joints.
    let ent_loc = crate::animweb::eval(a.web.pose_tree_model(0).as_ref(), &sa.ent_bind, clips);
    let ent = PoseBuf::new(&sa.ent_skel, Some(&ent_loc.to_pose())).mats();
    let body_w = participant(a, sa, &ent, 0, frame);
    // Player body pose from its retargeted clip (its origin joint zeroed: the attach joint carries the motion).
    let mut loc = body_pose(sa, a.web.pose_tree_model(sa.body_index).as_ref(), clips);
    if let Some(o) = sa.body_origin {
        loc.rot[o] = Default::default();
        loc.trans[o] = Default::default();
    }
    let joints = PoseBuf::new(&sa.body.skel, Some(&loc.to_pose())).mats();
    // INTERIM: the body is drawn in the world at its attach joint; in the range runs its arms stay outside the sync
    // camera's view (the right hand ~25 units beside the camera joint for the whole of kill_left_upper), so how the
    // engine draws the first-person body during a sync (view-model pass, its FOV / camera handling) is not followed.
    let m2w = body_w * Mat4::from_translation(V::from(sa.body_offset));
    let skin: Vec<Mat4> = joints.iter().zip(sa.body.inv_bind.iter()).map(|(w, i)| m2w * *w * *i).collect();
    for p in &a.body_parts {
        if let Some(mut mesh) = meshes.get_mut(&p.mesh) {
            p.skin(&skin, V::ZERO, &mut mesh);
        }
    }
    let show = !a.released;
    for e in &a.body_entities {
        if let Ok(mut v) = vis.get_mut(*e) {
            *v = if show { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
    let cam = m2w * joints[sa.camera];
    if log && (frame as i32) / 10 != a.log_frame / 10 {
        a.log_frame = frame as i32;
        let c = cam.w_axis;
        println!("[glory {} {now}] frame {frame:.1} body ({:.1} {:.1} {:.1}) yaw {:.1} camera ({:.1} {:.1} {:.1}) yaw {:.1}", a.victim, body_w.w_axis.x, body_w.w_axis.y, body_w.w_axis.z, gk::yaw_of(body_w), c.x, c.y, c.z, gk::yaw_of(cam));
    }
    if !a.released {
        out.hold = Some(body_w.w_axis.truncate());
        view.active = true;
        // sync_autoFOV / sync_autoFOVValue. INTERIM: the blend back (sync_autoFOVLerpTimeMS 600) is not done.
        view.fov = Some(90.0);
        view.view = a.camera.then(|| (cam.w_axis.truncate().to_array(), rows(cam)));
    } else if view.active {
        // ae_releasePlayerFromSync + ae_setViewAnglesFromCamera: the player stands at its attach joint, looking along
        // the camera joint (pitch from its forward axis, yaw, no roll).
        let f = cam.x_axis.truncate();
        let pitch = -f.z.clamp(-1.0, 1.0).asin().to_degrees();
        out.release = Some((body_w.w_axis.truncate(), [pitch, gk::yaw_of(cam), 0.0]));
        view.active = false;
        view.view = None;
        view.fov = None;
        if log {
            println!("[glory {} {now}] release player at {} view {:?}", a.victim, body_w.w_axis.truncate(), out.release.map(|r| r.1));
        }
    }
    out
}

/// The body's local pose: a leaf samples its retargeted clip; anything else (not used by the sync webs) goes
/// through the shared evaluator.
fn body_pose(sa: &SyncAssets, node: Option<&PoseNode>, clips: &Clips) -> Locals {
    if let Some(PoseNode::Leaf { anim, frame, frac }) = node
        && let Some(clip) = sa.body_clips.get(&anim.to_ascii_lowercase())
    {
        let mut loc = sa.body_bind.clone();
        let p = clip.sample(*frame as f32 + frac);
        let n = loc.rot.len();
        for &(j, q) in &p.rot {
            if (j as usize) < n {
                loc.rot[j as usize] = glam::Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize();
            }
        }
        for &(j, t) in &p.trans {
            if (j as usize) < n {
                loc.trans[j as usize] = V::from(t);
            }
        }
        for &(j, t) in &p.scale {
            if (j as usize) < n {
                loc.scale[j as usize] = V::from(t);
            }
        }
        return loc;
    }
    crate::animweb::eval(node, &sa.body_bind, clips)
}

/// The victim's pose and placement from its sync (running or finished): (local pose, origin, yaw).
pub(super) fn victim_pose(g: &Glory, id: u32, kind: &DemonKind, clips: &Clips) -> Option<(Locals, V, f32)> {
    let a = g.pose_of(id)?;
    let sa = g.assets.get(&a.kind)?;
    let frame = ent_frame(&a.web);
    let ent_loc = crate::animweb::eval(a.web.pose_tree_model(0).as_ref(), &sa.ent_bind, clips);
    let ent = PoseBuf::new(&sa.ent_skel, Some(&ent_loc.to_pose())).mats();
    let w = participant(a, sa, &ent, 1, frame);
    let mut loc = crate::animweb::eval(a.web.pose_tree_model(sa.victim_index).as_ref(), &kind.bind, clips);
    if let Some(o) = kind.origin {
        loc.rot[o] = Default::default();
        loc.trans[o] = Default::default();
    }
    Some((loc, w.w_axis.truncate(), gk::yaw_of(w)))
}

/// Ends the running sync after its last event: the body goes, the victim keeps the sync's last pose as a corpse.
pub(super) fn finish(d: &mut Demons, commands: &mut Commands) {
    let done = d.glory.active.as_ref().is_some_and(|a| a.ended && a.released);
    if !done {
        return;
    }
    let mut a = d.glory.active.take().unwrap();
    for e in a.body_entities.drain(..) {
        commands.entity(e).despawn();
    }
    if d.log {
        println!("[glory {}] sync {} ended", a.victim, a.name);
    }
    d.glory.corpses.push(a);
}

/// INTERIM stagger glow (sync_useVisualCue 1; the highlight's render path and colours were not decoded): an additive
/// unlit copy of the demon's skinned meshes pulsing at r_highlightFrequency (0.75 Hz), r_highlightFrequencyFast (2 Hz)
/// once less than sync_stagger_pulseThreshold (0.2) of the stagger is left; blue while staggered, orange while a melee
/// press would glory kill it.
#[allow(clippy::too_many_arguments)]
pub(super) fn glow(d: &mut Demons, commands: &mut Commands, std_mats: &mut Assets<StandardMaterial>, vis: &mut Query<&mut Visibility>, now: i32) {
    let avail = available(d);
    let victim = d.glory.victim();
    for inst in &d.list {
        let on = !inst.sim.is_dead() && inst.sim.stagger_vulnerable() && victim != Some(inst.id);
        let ents = d.glory.overlays.entry(inst.id).or_insert_with(|| {
            let m = std_mats.add(StandardMaterial { unlit: true, alpha_mode: AlphaMode::Add, base_color: Color::NONE, ..default() });
            d.glory.overlay_mats.insert(inst.id, m.clone());
            inst.parts.iter().map(|p| commands.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(m.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling)).id()).collect()
        });
        for (i, e) in ents.iter().enumerate() {
            if let Ok(mut v) = vis.get_mut(*e) {
                *v = if on && inst.shown.get(i).copied().unwrap_or(false) { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
        if !on {
            continue;
        }
        let left = match inst.sim.phase {
            rancher_sim::demons::DemonPhase::Stagger { until, .. } => {
                let len = inst.sim.def.behaviors.stagger_vulnerable_ms.max(1) as f32;
                ((until - now) as f32 / len).clamp(0.0, 1.0)
            }
            _ => 1.0,
        };
        let hz = if left < 0.2 { 2.0 } else { 0.75 };
        let pulse = 0.5 - 0.5 * (now as f32 * 0.001 * hz * std::f32::consts::TAU).cos();
        let rgb = if avail == Some(inst.id) { [1.0, 0.45, 0.05] } else { [0.05, 0.35, 1.0] };
        if let Some(mut m) = d.glory.overlay_mats.get(&inst.id).and_then(|h| std_mats.get_mut(h)) {
            m.base_color = Color::srgb(rgb[0] * pulse * 0.6, rgb[1] * pulse * 0.6, rgb[2] * pulse * 0.6);
        }
    }
}

/// Drops what a removed demon left: its finished sync and its glow overlay.
pub(super) fn forget(d: &mut Demons, id: u32, commands: &mut Commands) {
    d.glory.corpses.retain(|a| a.victim != id);
    for e in d.glory.overlays.remove(&id).unwrap_or_default() {
        commands.entity(e).despawn();
    }
    d.glory.overlay_mats.remove(&id);
}

/// The interaction name of the current choice (logs).
pub(super) fn choice_name(d: &Demons) -> Option<String> {
    let (_, kind, c) = d.glory.choice?;
    d.glory.sync.get(&kind).map(|s| s.interactions[c.index].name.clone())
}
