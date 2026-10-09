//! Animated first-person view models: hands rig + weapon rig, sampled from the install's md6 animations
//! and skinned on the CPU each frame.
//!
//! The game's hands anim web (`player/fp_hands`, run headless by `rancher_sim::animweb` on the game clock)
//! poses both the arms skeleton and the current weapon's skeleton, which hangs off the arms'
//! `righthandattach` joint.
//! INTERIM controller until the sim's idHands driver lands: requests follow gamedata/re/HANDS.md's
//! idHands::Update choices (idle via shoot / shootstate via shootstate_into / idle via bringdown / new
//! sub-web idle via bringup), triggered by the sim's own shots and weapon phase. The bob cycle, weapon lag
//! and additive layers are not applied yet.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use glam::{Mat4, Quat as GQuat, Vec3 as G3};
use idres::Container;
use idres::md6::{Md6Model, Md6Skel, unpack_normal};
use idres::md6anim::Pose;
use idres::md6def::AnimEvent;
use rancher_sim::weapons::{WeaponDef, WeaponPhase};

use rancher_sim::animweb::{AnimData, AnimWebRuntime};

use crate::animweb::{Clips, Locals};
use crate::assets;
use crate::rig::{ArmRig, PoseBuf};
use crate::vtmat::{VtMaterial, VtMaterials};
use crate::combat::{Combat, VIEW_LAYER};

pub struct Rig {
    pub skel: Md6Skel,
    pub inv_bind: Vec<Mat4>,
    pub attach: Option<usize>,
}

fn local(skel: &Md6Skel, j: usize, rot: Option<[f32; 4]>, trans: Option<[f32; 3]>, scale: Option<[f32; 3]>) -> Mat4 {
    let r = rot.unwrap_or(skel.rotations[j]);
    let t = trans.unwrap_or(skel.translations[j]);
    let s = scale.unwrap_or(skel.scales[j]);
    Mat4::from_scale_rotation_translation(G3::from(s), GQuat::from_xyzw(r[0], r[1], r[2], r[3]).normalize(), G3::from(t))
}

impl Rig {
    pub fn new(skel: Md6Skel) -> Self {
        let bind = Self::world(&skel, None);
        let inv_bind = bind.iter().map(|m| m.inverse()).collect();
        let attach = skel.names.iter().position(|n| n == "righthandattach");
        Self { skel, inv_bind, attach }
    }

    /// Joint world matrices (model space) for a pose; `None` gives the bind pose.
    pub fn world(skel: &Md6Skel, pose: Option<&Pose>) -> Vec<Mat4> {
        let n = skel.names.len();
        let (mut r, mut t, mut s) = (vec![None; n], vec![None; n], vec![None; n]);
        if let Some(p) = pose {
            for &(j, q) in &p.rot {
                if (j as usize) < n {
                    r[j as usize] = Some(q);
                }
            }
            for &(j, v) in &p.trans {
                if (j as usize) < n {
                    t[j as usize] = Some(v);
                }
            }
            for &(j, v) in &p.scale {
                if (j as usize) < n {
                    s[j as usize] = Some(v);
                }
            }
        }
        let mut out: Vec<Mat4> = Vec::with_capacity(n);
        for j in 0..n {
            let l = local(skel, j, r[j], t[j], s[j]);
            let p = skel.parents[j];
            out.push(if p >= 0 && (p as usize) < j { out[p as usize] * l } else { l });
        }
        out
    }
}

/// Bitangent sign for tangent bytes below 128. vertex.inc decodes localTangent.w = floor(t.w*255.1/128)*2-1
/// (+1 for >= 128) and the lighting pass uses w * -mvpMatrixDeterminantSign; with idTech 6's reversed-Z
/// projection that determinant sign is taken as +1, i.e. bytes >= 128 -> -1. INTERIM: the determinant sign
/// is inferred (it matched by inspection), not read from the exe.
const TANGENT_SIGN: f32 = 1.0;

/// DECODE_SKIN_WEIGHTS (renderprogs vertex.inc): w1 = (tangent.w & 127) / 254, w2 = (normal.w >> 4) / 45,
/// w3 = (normal.w & 15) / 60, w0 = 1 - (w1 + w2 + w3); joints are the four color bytes.
fn skin_weights(v: &idres::md6::DrawVert) -> [f32; 4] {
    // Unorm bytes reach the shader as byte / 255; the * 255.1 fractions are kept, as on the GPU.
    let hi = v.tangent[3] as f32 / 255.0 * 255.1;
    let w1 = (hi - (hi / 128.0).floor() * 128.0) * (1.0 / 254.0);
    let packed = v.normal[3] as f32 / 255.0 * 255.1;
    let w2 = (packed / 16.0).floor();
    let w3 = (packed - w2 * 16.0) * (1.0 / 60.0);
    let w2 = w2 * (1.0 / 45.0);
    [1.0 - (w1 + w2 + w3), w1, w2, w3]
}

/// One md6 mesh prepared for CPU skinning.
pub struct SkinPart {
    pub mesh: Handle<Mesh>,
    pub weapon: Option<usize>,
    pos: Vec<G3>,
    nrm: Vec<G3>,
    tan: Vec<G3>,
    /// Bitangent sign from the md6 tangent polarity byte (Bevy tangent w).
    tsign: Vec<f32>,
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
}

impl SkinPart {
    /// CPU skinning (CALC_SKINNING_LINEAR_MAT): each vertex by the weighted sum of its four joint matrices
    /// (`mats` = joint world * inverse bind, idTech space), offset by `off`, written to `mesh` in Bevy space.
    pub fn skin(&self, mats: &[Mat4], off: G3, mesh: &mut Mesh) {
        let to_bevy = |v: G3| G3::new(-v.y, v.z, -v.x);
        let mut pos = Vec::with_capacity(self.pos.len());
        let mut nrm = Vec::with_capacity(self.pos.len());
        let mut tan = Vec::with_capacity(self.pos.len());
        for k in 0..self.pos.len() {
            let mut m = Mat4::ZERO;
            for i in 0..4 {
                let w = self.weights[k][i];
                if w != 0.0 {
                    m += mats.get(self.joints[k][i] as usize).copied().unwrap_or(Mat4::IDENTITY) * w;
                }
            }
            let p = m.transform_point3(self.pos[k]);
            let n = m.transform_vector3(self.nrm[k]).normalize_or_zero();
            let t = m.transform_vector3(self.tan[k]).normalize_or_zero();
            pos.push(to_bevy(p + off).to_array());
            nrm.push(to_bevy(n).to_array());
            let tb = to_bevy(t);
            tan.push([tb.x, tb.y, tb.z, self.tsign[k]]);
        }
        if let Some(VertexAttributeValues::Float32x3(v)) = mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION) {
            *v = pos;
        }
        if let Some(VertexAttributeValues::Float32x3(v)) = mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL) {
            *v = nrm;
        }
        if let Some(VertexAttributeValues::Float32x4(v)) = mesh.attribute_mut(Mesh::ATTRIBUTE_TANGENT) {
            *v = tan;
        }
    }
}

pub struct WeaponRig {
    pub rig: Rig,
    pub bind: Locals,
    pub offset: [f32; 3],
}

/// Which model an anim event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Hands,
    Weapon,
}

/// An anim event dispatched this frame.
#[derive(Debug, Clone)]
pub struct Fired {
    pub slot: Slot,
    pub anim: String,
    pub event: AnimEvent,
}

/// Per-weapon decl flags the INTERIM controller needs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShootMode {
    pub looping: bool,
    pub again: bool,
    pub shoot_to_reload: bool,
}

#[derive(Resource)]
pub struct ViewRigs {
    pub arms: Rig,
    pub arms_bind: Locals,
    pub arm_rig: Option<ArmRig>,
    pub arms_offset: [f32; 3],
    pub weapons: Vec<Option<WeaponRig>>,
    /// Each weapon's hands sub-web (weapon decl `subweb_normal`).
    pub sub_webs: Vec<String>,
    pub shoot_modes: Vec<ShootMode>,
    pub parts: Vec<SkinPart>,
    pub clips: Clips,
    /// Arsenal fire time last seen by the controller.
    pub seen_fire_ms: Option<i32>,
    /// Phase request already issued (dedupes once-per-change requests).
    pub phase_req: String,
    /// Last state printed by RANCHER_TRACE.
    pub traced: String,
    /// RANCHER_TRACE timing: (total us, frames).
    pub perf: (u64, u32),
}

/// The hands anim web runtime, shared by the weapon code (the idHands driver in rancher_sim::weapons::hands
/// drives it through the HandsWeb trait) and the renderer (which reads poses and events from it).
#[derive(Resource)]
pub struct HandsWebRuntime {
    pub rt: AnimWebRuntime,
    /// Set by the weapon code when the idHands driver advances the web; otherwise the renderer INTERIM
    /// controller requests states and advances it.
    pub driven: bool,
}

/// Anim events dispatched this frame (hands and weapon), for sound / FX / gameplay consumers.
#[derive(Resource, Default)]
pub struct HandsEvents(pub Vec<Fired>);

/// The first-person models' joints this frame, for attaching FX (muzzle flash, shell eject, ...).
/// Each matrix maps joint-local idTech coordinates (x forward, y left, z up) to the view camera's local
/// Bevy space (the hands root is a child of the view camera).
#[derive(Resource, Default)]
pub struct ViewJoints {
    /// Index into the arsenal of the weapon on screen.
    pub weapon: Option<usize>,
    pub weapon_names: Vec<String>,
    pub weapon_joints: Vec<Mat4>,
    pub hands_names: Vec<String>,
    pub hands_joints: Vec<Mat4>,
}

impl ViewJoints {
    pub fn weapon_joint(&self, name: &str) -> Option<Mat4> {
        self.weapon_names.iter().position(|n| n.eq_ignore_ascii_case(name)).and_then(|i| self.weapon_joints.get(i).copied())
    }
    pub fn hands_joint(&self, name: &str) -> Option<Mat4> {
        self.hands_names.iter().position(|n| n.eq_ignore_ascii_case(name)).and_then(|i| self.hands_joints.get(i).copied())
    }
}

/// idTech (x forward, y left, z up) to Bevy (x right, y up, z back).
const ID_TO_BEVY: Mat4 = Mat4::from_cols(
    glam::Vec4::new(0.0, 0.0, -1.0, 0.0),
    glam::Vec4::new(-1.0, 0.0, 0.0, 0.0),
    glam::Vec4::new(0.0, 1.0, 0.0, 0.0),
    glam::Vec4::new(0.0, 0.0, 0.0, 1.0),
);

/// The hands anim web (player.decl handsAnimWeb).
const HANDS_WEB: &str = "generated/decls/animweb/player/fp_hands.decl";
/// Sub-web for weapons whose decl names none.
const DEFAULT_SUB_WEB: &str = "fists";

pub fn skin_parts<S: AsRef<str>>(model: &Md6Model, mesh_assets: &mut Assets<Mesh>, weapon: Option<usize>, hidden: &[S]) -> Vec<(SkinPart, String)> {
    let mut out = Vec::new();
    for m in &model.meshes {
        if hidden.iter().any(|h| h.as_ref().eq_ignore_ascii_case(&m.name)) {
            continue;
        }
        // The model's remap table maps skeleton joint -> palette slot; vertices store palette slots
        // (relative to the mesh's joint offset), so invert it.
        let jo = m.trailer[0] as usize;
        let mut inv = vec![0u16; 256];
        for (joint, &slot) in model.joint_remap.iter().enumerate() {
            inv[slot as usize] = joint as u16;
        }
        let remap = |i: u8| -> u16 { inv[(jo + i as usize) & 0xff] };
        let pos: Vec<G3> = m.verts.iter().map(|v| G3::from(v.xyz)).collect();
        let nrm: Vec<G3> = m.verts.iter().map(|v| G3::from(unpack_normal(v.normal))).collect();
        let tan: Vec<G3> = m.verts.iter().map(|v| G3::from(unpack_normal(v.tangent))).collect();
        let tsign: Vec<f32> = m.verts.iter().map(|v| if v.tangent[3] >= 128 { -TANGENT_SIGN } else { TANGENT_SIGN }).collect();
        let joints = m.verts.iter().map(|v| [remap(v.color[0]), remap(v.color[1]), remap(v.color[2]), remap(v.color[3])]).collect();
        let weights = m.verts.iter().map(skin_weights).collect();
        let uv: Vec<[f32; 2]> = m.verts.iter().map(|v| v.st).collect();
        let mut idx: Vec<u32> = Vec::with_capacity(m.indices.len());
        for t in m.indices.chunks_exact(3) {
            idx.extend_from_slice(&[t[0] as u32, t[2] as u32, t[1] as u32]);
        }
        let n = pos.len();
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; n])
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n])
            .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0f32, 0.0, 0.0, 1.0]; n])
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_indices(Indices::U32(idx));
        out.push((SkinPart { mesh: mesh_assets.add(mesh), weapon, pos, nrm, tan, tsign, joints, weights }, m.material.clone()));
    }
    out
}

/// Texture mip used for view models (level 1 = half the authored virtual-texture resolution).
const VIEW_TEXTURE_LEVEL: usize = 1;

#[allow(clippy::too_many_arguments)]
pub fn build(
    c: &Arc<Container>,
    vtm: &mut VtMaterials,
    defs: &[Arc<WeaponDef>],
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mats: &mut Assets<VtMaterial>,
    images: &mut Assets<Image>,
    camera: Entity,
) -> anyhow::Result<(ViewRigs, HandsWebRuntime)> {
    // player.decl handsModelDefault; it inherits fp_hands.md6 (offset) with the Praetor suit mesh.
    let t0 = std::time::Instant::now();
    let arms_def = assets::read_md6def(c, "zion/player/human/base/praetor.md6").or_else(|_| assets::read_md6def(c, "player/fp_hands.md6"))?;
    let arms = assets::load_model(c, &arms_def.mesh)?;
    let arms_rig = Rig::new(arms.skel.clone().ok_or_else(|| anyhow::anyhow!("arms skeleton missing"))?);
    let arm_rig = ArmRig::new(&arms_rig.skel);
    let arms_bind = Locals::bind(&arms_rig.skel);
    let web = String::from_utf8_lossy(&c.read_by_name(HANDS_WEB)?).into_owned();
    let web = Arc::new(idres::animweb::AnimWeb::parse(&web)?);
    let db = idres::decldb::DeclDb::new(c.clone());
    let root = commands.spawn((HandsRoot, Transform::default(), Visibility::default(), RenderLayers::layer(VIEW_LAYER))).id();
    commands.entity(camera).add_child(root);
    let mut colors = HashMap::new();
    let mut parts = Vec::new();
    for (p, mat) in skin_parts(&arms.model, meshes, None, &["legsMesh"]) {
        let m = vtm
            .material(&mat, VIEW_TEXTURE_LEVEL, images, mats)
            .unwrap_or_else(|| crate::vtmat::flat(mats, StandardMaterial { base_color: assets::placeholder_color(&mat, &mut colors), perceptual_roughness: 0.8, ..default() }));
        let e = commands.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(m), Transform::default(), RenderLayers::layer(VIEW_LAYER), NoFrustumCulling)).id();
        commands.entity(root).add_child(e);
        parts.push(p);
    }
    let t_arms = t0.elapsed();
    let mut weapons = Vec::new();
    let mut sub_webs = Vec::new();
    let mut shoot_modes = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        let decl = db.get("weapon", &d.decl).ok();
        let sub = decl.as_ref().and_then(|b| b.str("edit.subweb_normal").map(str::to_string)).filter(|s| web.sub_web(s).is_some()).unwrap_or_else(|| DEFAULT_SUB_WEB.to_string());
        // Stock look: showHideMeshInfo.meshesToHide (mod meshes). INTERIM: shellMesh (the shotgun's loose
        // shell) is kept hidden too; no view-model event shows it.
        let mut hidden: Vec<String> = vec!["shellMesh".into()];
        if let Some(h) = decl.as_ref().and_then(|b| b.block("edit.showHideMeshInfo.meshesToHide")) {
            hidden.extend(h.items.iter().filter(|(k, _)| k.starts_with("item[")).filter_map(|(_, v)| v.as_str().map(str::to_string)));
        }
        sub_webs.push(sub);
        let flag = |k: &str| decl.as_ref().and_then(|b| b.path(&format!("edit.{k}"))).and_then(|v| v.as_bool()).unwrap_or(false);
        shoot_modes.push(ShootMode { looping: flag("hasLoopingShootState"), again: flag("hasShootAgainState"), shoot_to_reload: flag("hasShootToReloadAnims") });
        let rig = (|| -> anyhow::Result<WeaponRig> {
            anyhow::ensure!(!d.hands_md6.is_empty(), "no handsModelMD6");
            let def = assets::read_md6def(c, &d.hands_md6)?;
            let model = assets::load_model(c, &def.mesh)?;
            for (p, mat) in skin_parts(&model.model, meshes, Some(i), &hidden) {
                let m = vtm
                    .material(&mat, VIEW_TEXTURE_LEVEL, images, mats)
                    .unwrap_or_else(|| crate::vtmat::flat(mats, StandardMaterial { base_color: assets::placeholder_color(&mat, &mut colors), metallic: 0.6, perceptual_roughness: 0.45, ..default() }));
                let e = commands.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(m), Transform::default(), Visibility::Hidden, RenderLayers::layer(VIEW_LAYER), WeaponMesh(i), NoFrustumCulling)).id();
                commands.entity(root).add_child(e);
                parts.push(p);
            }
            let skel = model.skel.clone().ok_or_else(|| anyhow::anyhow!("weapon skeleton missing"))?;
            let bind = Locals::bind(&skel);
            Ok(WeaponRig { rig: Rig::new(skel), bind, offset: def.offset })
        })();
        weapons.push(rig.map_err(|e| if !d.hands_md6.is_empty() { eprintln!("view model {}: {e:#}", d.decl) }).ok());
    }
    let t_weapons = t0.elapsed();
    // Every anim the used sub-webs can play.
    let mut clips = Clips::default();
    for name in &sub_webs {
        if let Some(sw) = web.sub_web(name) {
            for n in &sw.nodes {
                for t in &n.trees {
                    for a in &t.anims {
                        clips.load(c, &a.name);
                    }
                }
            }
        }
    }
    let t_clips = t0.elapsed();
    // Runtime data: anim lengths from the clips already parsed, events from every model's md6Def.
    let mut data = AnimData::load(c, &web, &[]);
    for name in &sub_webs {
        if let Some(sw) = web.sub_web(name) {
            for n in &sw.nodes {
                for t in &n.trees {
                    for a in &t.anims {
                        if let Some(clip) = clips.get(&a.name) {
                            data.meta.insert(a.name.to_ascii_lowercase(), rancher_sim::animweb::AnimMeta { num_frames: clip.num_frames, frame_rate: clip.frame_rate });
                        }
                    }
                }
            }
        }
    }
    let runtime = AnimWebRuntime::new(web, Arc::new(data));
    eprintln!(
        "view models: arms {:.2}s, weapons {:.2}s, clips {:.2}s, web data {:.2}s",
        t_arms.as_secs_f32(),
        (t_weapons - t_arms).as_secs_f32(),
        (t_clips - t_weapons).as_secs_f32(),
        (t0.elapsed() - t_clips).as_secs_f32()
    );
    Ok((ViewRigs {
        arms: arms_rig,
        arms_bind,
        arm_rig,
        arms_offset: arms_def.offset,
        weapons,
        sub_webs,
        shoot_modes,
        parts,
        clips,
        seen_fire_ms: None,
        phase_req: String::new(),
        traced: String::new(),
        perf: (0, 0),
    }, HandsWebRuntime { rt: runtime, driven: false }))
}

#[derive(Component)]
pub struct WeaponMesh(pub usize);

/// The first-person hands' root entity (child of the view camera); placed by main.rs place_camera.
#[derive(Component)]
pub struct HandsRoot;

/// INTERIM hands-web controller (HANDS.md idHands::Update choices), driven by the sim's shots and phase.
fn control(rigs: &mut ViewRigs, rt: &mut AnimWebRuntime, combat: &Combat) {
    let cur = combat.arsenal.current;
    let sub = rigs.sub_webs.get(cur).cloned().unwrap_or_else(|| DEFAULT_SUB_WEB.to_string());
    let mode = rigs.shoot_modes.get(cur).copied().unwrap_or_default();
    let fired = combat.arsenal.last_fire_ms.is_some() && combat.arsenal.last_fire_ms != rigs.seen_fire_ms;
    rigs.seen_fire_ms = combat.arsenal.last_fire_ms;
    let loaded = combat.arsenal.has_ammo(cur);
    let held = combat.arsenal.trigger_pulled;
    // weaponLoadedSelect / weaponLoadedBlend: 0 loaded, 1 unloaded. INTERIM: snaps (the game springs, K 500).
    rt.set_scalar("weaponLoadedSelect", if loaded { 0.0 } else { 1.0 });
    rt.set_scalar("weaponLoadedBlend", if loaded { 0.0 } else { 1.0 });
    if rt.current().is_none() {
        let first = if matches!(combat.arsenal.phase, WeaponPhase::Raising { .. }) { "bringup" } else { "idle" };
        rt.set_state(&sub, first);
        return;
    }
    let (cur_sub, cur_state) = rt.current().map(|(a, b)| (a.to_string(), b.to_string())).unwrap();
    match combat.arsenal.phase {
        WeaponPhase::Lowering { next, .. } => {
            // idHands BRINGDOWN: same sub-web -> idle via bringdown; else the new sub-web's idle via its
            // bringup (FindPath goes through this sub-web's bringdown and the toSubWeb edge).
            let next_sub = rigs.sub_webs.get(next).cloned().unwrap_or_else(|| DEFAULT_SUB_WEB.to_string());
            let key = format!("lower:{next_sub}");
            if rigs.phase_req != key {
                if next_sub == cur_sub {
                    rt.change_state_via(None, "idle", Some("bringdown"));
                } else {
                    rt.change_state_via(Some(&next_sub), "idle", Some("bringup"));
                }
                rigs.phase_req = key;
            }
        }
        WeaponPhase::Raising { .. } | WeaponPhase::Ready if cur_sub != sub && rt.pending_target().map(|t| t.0) != Some(sub.as_str()) && !rigs.phase_req.ends_with(&sub) => {
            let key = format!("raise:{sub}");
            if rigs.phase_req != key {
                rt.change_state_via(Some(&sub), "idle", Some("bringup"));
                rigs.phase_req = key;
            }
        }
        WeaponPhase::Raising { .. } => {}
        WeaponPhase::Ready => {
            rigs.phase_req.clear();
            if fired {
                // shootAnimSelect 0 (FIRE) on request.
                rt.set_scalar("shootAnimSelect", 0.0);
                if mode.looping {
                    if cur_state != "shootstate" && cur_state != "shootstate_into" {
                        rt.change_state_via(None, "shootstate", Some("shootstate_into"));
                    }
                } else if mode.shoot_to_reload {
                    rt.change_state_via(None, "idle", Some("shoot_delay"));
                } else if mode.again && cur_state == "shoot" {
                    rt.change_state_via(None, "idle", Some("shoot_again"));
                } else {
                    rt.change_state_via(None, "idle", Some("shoot"));
                }
            } else if mode.looping && !held && (cur_state == "shootstate" || cur_state == "shootstate_into") && rt.pending_target().map(|t| t.1) != Some("idle") {
                // CEASEFIRE: idle via shootstate_recovery, shootAnimSelect 1 (IDLE).
                rt.set_scalar("shootAnimSelect", 1.0);
                rt.change_state_via(None, "idle", Some("shootstate_recovery"));
            }
        }
    }
}

pub fn animate(
    mut rigs: ResMut<ViewRigs>,
    combat: Res<Combat>,
    sim: Res<crate::Sim>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut vis: Query<(&WeaponMesh, &mut Visibility)>,
    mut out_events: ResMut<HandsEvents>,
    mut joints: ResMut<ViewJoints>,
    web: Option<ResMut<HandsWebRuntime>>,
    mut layers: Option<ResMut<crate::hands_layers::HandsLayersState>>,
    status: Res<crate::hands_layers::HandsStatus>,
    auto: Res<crate::autotest::AutoTest>,
) {
    let rigs = &mut *rigs;
    let mut web = web;
    let t_frame = std::time::Instant::now();
    if let Some(w) = web.as_mut() {
        if !w.driven {
            control(rigs, &mut w.rt, &combat);
            w.rt.update(sim.game_ms);
        }
    }
    out_events.0.clear();
    if let Some(w) = web.as_ref() {
        for e in &w.rt.last_fired {
            out_events.0.push(Fired { slot: if e.model_index == 0 { Slot::Hands } else { Slot::Weapon }, anim: e.anim.clone(), event: e.event.clone() });
        }
    }
    // The weapon on screen is the one whose sub-web the hands are in.
    let rt = web.as_ref().map(|w| &w.rt);
    let shown = rt.and_then(|w| w.current()).and_then(|(s, _)| rigs.sub_webs.iter().position(|x| x == s)).unwrap_or(combat.arsenal.current);
    // Inherited, not Visible: the hands root's visibility (hideHandsOnZoom, a glory kill holding the player) hides the
    // weapon with the arms.
    for (w, mut v) in &mut vis {
        *v = if w.0 == shown { Visibility::Inherited } else { Visibility::Hidden };
    }
    if auto.trace {
        if let Some(rt) = rt {
            let now = rt.current().map(|(a, b)| format!("{a}/{b}")).unwrap_or_default();
            if now != rigs.traced {
                let (anim, frame) = rt.playhead().map(|(a, f)| (a.rsplit('/').next().unwrap_or(&a).to_string(), f)).unwrap_or_default();
                println!("[anim {:7.3}] state {now} ({anim} f{frame}) game {}ms", auto.elapsed, sim.game_ms);
                rigs.traced = now;
            }
        }
        for f in &out_events.0 {
            let params: Vec<String> = f.event.params.iter().map(|(k, v)| format!("{k}={}", v.text().unwrap_or("?"))).collect();
            println!("[anim {:7.3}] event {:?} {} f{} {} {}", auto.elapsed, f.slot, f.event.name, f.event.frame, f.anim.rsplit('/').next().unwrap_or(&f.anim), params.join(" "));
        }
    }
    let wr = rigs.weapons.get(shown).and_then(|w| w.as_ref());
    // Hand layers advance before the pose (the bob-cycle web feeds the hands pose).
    if let Some(l) = layers.as_mut() {
        l.step(shown, &sim.player, &sim.last_cmd, combat.arsenal.last_fire_ms, &status, !combat.arsenal.has_ammo(shown), sim.game_ms, sim.msec_last);
    }
    let (hands_pose, weapon_pose): (Option<Pose>, Option<Pose>) = match rt {
        Some(rt) => {
            let mut hl = crate::animweb::eval(rt.pose_tree(false).as_ref(), &rigs.arms_bind, &rigs.clips);
            if let Some(l) = layers.as_ref() {
                l.add_bob_cycle(&mut hl);
            }
            let h = hl.to_pose();
            let w = wr.map(|w| crate::animweb::eval(rt.pose_tree(true).as_ref(), &w.bind, &rigs.clips).to_pose());
            (Some(h), w)
        }
        None => (None, None),
    };
    let mut arms_pose = PoseBuf::new(&rigs.arms.skel, hands_pose.as_ref());
    // Weapon lag / bob joint mods in model space, then the arm IK (its targets hang off the attach joints).
    if let Some(l) = layers.as_mut() {
        l.apply(&mut arms_pose, &rigs.arms.skel.names);
    }
    if let Some(r) = &rigs.arm_rig {
        r.apply(&mut arms_pose);
    }
    let arms_world = arms_pose.mats();
    let off = G3::from(rigs.arms_offset);

    let weapon_world: Option<Vec<Mat4>> = wr.map(|w| {
        let attach = rigs.arms.attach.map(|j| arms_world[j]).unwrap_or(Mat4::IDENTITY);
        Rig::world(&w.rig.skel, weapon_pose.as_ref()).into_iter().map(|m| attach * Mat4::from_translation(G3::from(w.offset)) * m).collect()
    });
    {
        let to_view = ID_TO_BEVY * Mat4::from_translation(off);
        joints.hands_names.clone_from(&rigs.arms.skel.names);
        joints.hands_joints = arms_world.iter().map(|m| to_view * *m).collect();
        joints.weapon = wr.map(|_| shown);
        match (wr, &weapon_world) {
            (Some(w), Some(ww)) => {
                joints.weapon_names.clone_from(&w.rig.skel.names);
                joints.weapon_joints = ww.iter().map(|m| to_view * *m).collect();
            }
            _ => {
                joints.weapon_names.clear();
                joints.weapon_joints.clear();
            }
        }
    }

    for part in &rigs.parts {
        let (world, inv) = match part.weapon {
            None => (&arms_world, &rigs.arms.inv_bind),
            Some(i) if i == shown => match (&weapon_world, rigs.weapons.get(i).and_then(|w| w.as_ref())) {
                (Some(ww), Some(wr)) => (ww, &wr.rig.inv_bind),
                _ => continue,
            },
            Some(_) => continue,
        };
        let Some(mut mesh) = meshes.get_mut(&part.mesh) else { continue };
        let mats: Vec<Mat4> = world.iter().zip(inv.iter()).map(|(w, i)| *w * *i).collect();
        part.skin(&mats, off, &mut mesh);
    }
    if auto.trace {
        let us = t_frame.elapsed().as_micros() as u64;
        rigs.perf.0 += us;
        rigs.perf.1 += 1;
        if rigs.perf.1 % 30 == 0 {
            if let Some(l) = layers.as_ref() {
                let (a, b, c) = l.trace_alphas();
                println!("[layers {:7.3}] shoot additive {a:.2} offset {b:.2} bob cycle {c:.2}", auto.elapsed);
            }
        }
        if rigs.perf.1 == 120 {
            println!("[perf] animate avg {} us/frame over 120 frames", rigs.perf.0 / 120);
            rigs.perf = (0, 0);
        }
    }
}
