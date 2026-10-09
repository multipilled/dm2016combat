//! First-person hand layers from rancher_sim::handlayers (gamedata/re/HANDSLAYERS.md): the weapon-lag pendulum
//! and the procedural weapon bob, applied as MODEL joint mods to a copy of the local pose and merged back with the
//! jointModLagAnimator's REF_LERP at 0.95 (ANIMWEB.md 3b/3c), before the arm IK; the hands' placement
//! spring; and the hands projection (horizontal FOV scaled by GetHandsFovScale, vertical FOV with
//! hands_fovVerticalScaleHack).

use bevy::camera::{CameraProjection, SubCameraView};
use bevy::math::Vec3A;
use bevy::prelude::*;
use glam::{Quat as GQuat, Vec3 as G3};
use rancher_sim::handlayers::{
    BobInput, BobStep, FovInput, HandsLayerCvars, HandsLayerDecl, HandsLayers, JointOrigins, LagInput, LocalJoint, ParentModel, hands_fov_scale, hands_projection_fov,
    lagged_local, zoom_hands_fov_target,
};
use rancher_sim::player::Player;

use rancher_sim::animweb::AnimWebRuntime;
use rancher_sim::cmd::UserCmd;
use rancher_sim::handlayers::additive::{AdditiveDecl, ChannelCmd, LoopingShootInput, OffsetInput, OffsetType, additive_offset, looping_shoot, looping_shoot_rate};
use rancher_sim::handlayers::bobcycle::{BobCycleCvars, BobCycleFootstep, BobCycleInput, HandsBobCycle, HandsBobCycleDecl, PlayerSpeeds};
use rancher_sim::animweb::PoseNode;

use crate::animweb::{Clips, Locals};
use crate::rig::{PoseBuf, Xf};

/// What the idHands driver reports each frame for the additive channel drivers (written by combat.rs).
#[derive(Resource, Default, Clone, Copy)]
pub struct HandsStatus {
    /// destHandsState is LOOPING_SHOOT_STATE (8) or CHARGE_LOOPING_SHOOT (11).
    pub dest_looping: bool,
    /// handsFlags byte 1 (hands +0x10375).
    pub flags1: u8,
    pub zoomed: bool,
}

/// One idManagedChannelAnimator playing additive hands anims (merged with ADD_RIGHT at its merge alpha).
/// INTERIM: channel anims loop (REPEAT); crossfade is linear over blend_frames 30 Hz frames; times are game ticks.
#[derive(Default)]
pub struct AdditiveChannel {
    /// (alias, anim, start tick, rate)
    cur: Option<(String, String, i32, f32)>,
    prev: Option<(String, String, i32, f32)>,
    xf_start: i32,
    xf_ticks: i32,
    pub merge: f32,
    merge_from: f32,
    merge_to: f32,
    merge_start: i32,
    merge_ticks: i32,
    drop_at_zero: bool,
}

impl AdditiveChannel {
    fn apply(&mut self, cmd: &ChannelCmd, now: i32, aliases: &std::collections::HashMap<String, String>) {
        match cmd {
            ChannelCmd::Play { alias, rate, blend_frames, alpha, .. } => {
                let Some(anim) = aliases.get(alias) else { return };
                let ticks = (*blend_frames).max(0) * 960 / 30;
                let empty = self.cur.is_none();
                self.prev = self.cur.take();
                self.cur = Some((alias.clone(), anim.clone(), now, *rate));
                self.xf_start = now;
                self.xf_ticks = ticks;
                self.merge_from = if empty { 0.0 } else { self.merge };
                self.merge_to = 1.0;
                self.merge_start = now;
                self.merge_ticks = ticks;
                self.drop_at_zero = false;
                if let Some(a) = alpha {
                    self.merge = *a;
                    self.merge_from = *a;
                    self.merge_to = *a;
                    self.merge_ticks = 0;
                }
            }
            ChannelCmd::Stop { ms } => {
                self.merge_from = self.merge;
                self.merge_to = 0.0;
                self.merge_start = now;
                self.merge_ticks = (*ms).max(0);
                self.drop_at_zero = true;
            }
        }
    }

    fn update(&mut self, now: i32) {
        let k = if self.merge_ticks <= 0 { 1.0 } else { ((now - self.merge_start) as f32 / self.merge_ticks as f32).clamp(0.0, 1.0) };
        self.merge = self.merge_from + (self.merge_to - self.merge_from) * k;
        if self.drop_at_zero && self.merge <= 0.0 {
            self.cur = None;
            self.prev = None;
            self.drop_at_zero = false;
        }
        if self.prev.is_some() && (self.xf_ticks <= 0 || now - self.xf_start >= self.xf_ticks) {
            self.prev = None;
        }
    }

    pub fn playing(&self, alias: &str) -> bool {
        self.cur.as_ref().is_some_and(|c| c.0 == alias)
    }
    pub fn done(&self) -> bool {
        self.cur.is_none()
    }

    fn leaf(c: &(String, String, i32, f32), now: i32, clips: &Clips) -> PoseNode {
        let Some(clip) = clips.get(&c.1) else { return PoseNode::Bind };
        let n = clip.num_frames.max(2);
        let el = ((now - c.2).max(0) as f32 * c.3) as f32;
        let f = el * clip.frame_rate as f32 / 960.0;
        let u = f as u32;
        PoseNode::Leaf { anim: c.1.clone(), frame: u % (n - 1), frac: f - u as f32 }
    }

    /// The channel's additive pose tree (crossfade from the previous anim).
    fn tree(&self, now: i32, clips: &Clips) -> Option<PoseNode> {
        let cur = self.cur.as_ref()?;
        let right = Self::leaf(cur, now, clips);
        Some(match &self.prev {
            Some(p) if self.xf_ticks > 0 => {
                let a = ((now - self.xf_start) as f32 / self.xf_ticks as f32).clamp(0.0, 1.0);
                PoseNode::Lerp { left: Box::new(Self::leaf(p, now, clips)), right: Box::new(right), alpha: a }
            }
            _ => right,
        })
    }
}

/// ADD_RIGHT kernel (ANIMWEB.md 3c) with full weight planes: q = nlerp(qBase, qAdd * qBase, t),
/// T = Tb + Ta * t, S = Sb - (Sb - Sb * Sa) * t.
fn add_right(base: &mut Locals, add: &Locals, t: f32) {
    for j in 0..base.rot.len() {
        let qb = base.rot[j];
        let p = add.rot[j] * qb;
        let sgn = if qb.dot(p) < 0.0 { -t } else { t };
        let q = GQuat::from_xyzw((qb.x - qb.x * t) + p.x * sgn, (qb.y - qb.y * t) + p.y * sgn, (qb.z - qb.z * t) + p.z * sgn, (qb.w - qb.w * t) + p.w * sgn);
        let l2 = q.length_squared();
        base.rot[j] = if l2 > 0.0 { q * (1.0 / l2.sqrt()) } else { qb };
        base.trans[j] += add.trans[j] * t;
        let sb = base.scale[j];
        base.scale[j] = sb - (sb - sb * add.scale[j]) * t;
    }
}

fn identity_locals(n: usize) -> Locals {
    Locals { rot: vec![GQuat::IDENTITY; n], trans: vec![G3::ZERO; n], scale: vec![G3::ONE; n] }
}

/// idHands' additive channels (looping-shoot kick, additive offset) and their per-weapon data.
pub struct AdditiveRig {
    pub decls: Vec<AdditiveDecl>,
    /// Per weapon: (rate, rate_zoomed) for the looping-shoot aliases.
    pub rates: Vec<(f32, f32)>,
    /// Hands md6Def alias -> md6anim (lowercase).
    pub aliases: std::collections::HashMap<String, String>,
    pub clips: Clips,
    pub shoot: AdditiveChannel,
    pub offset: AdditiveChannel,
    offset_type: OffsetType,
    /// hands_additiveAnimBlendMS.
    pub blend_ms: i32,
}

impl AdditiveRig {
    pub fn load(c: &idres::Container, db: &idres::decldb::DeclDb, defs: &[std::sync::Arc<rancher_sim::weapons::WeaponDef>], blend_ms: i32) -> anyhow::Result<Self> {
        let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/md6def/player/fp_hands.md6.decl")?).into_owned();
        let md6 = idres::md6def::Md6DefDecl::parse(&text)?;
        let aliases: std::collections::HashMap<String, String> = md6.aliases.iter().map(|(k, v)| (k.clone(), v.to_ascii_lowercase())).collect();
        let decls: Vec<AdditiveDecl> = defs.iter().map(|d| db.get("weapon", &d.decl).ok().and_then(|b| b.block("edit").map(AdditiveDecl::from_edit)).unwrap_or_default()).collect();
        let mut clips = Clips::default();
        let mut rates = Vec::new();
        for (d, def) in decls.iter().zip(defs) {
            for a in [&d.looping_shoot, &d.looping_shoot_zoomed, &d.offset, &d.offset_zoom, &d.offset_melee, &d.offset_empty] {
                if let Some(anim) = aliases.get(a.as_str()) {
                    clips.load(c, anim);
                }
            }
            let rate_of = |alias: &str, cycles: i32| {
                aliases.get(alias).and_then(|a| clips.get(a)).map(|clip| looping_shoot_rate(clip.num_frames, clip.frame_rate, cycles, def.firing_interval)).unwrap_or(1.0)
            };
            rates.push((rate_of(&d.looping_shoot, d.looping_shoot_cycles), rate_of(&d.looping_shoot_zoomed, d.looping_shoot_zoomed_cycles)));
        }
        Ok(AdditiveRig { decls, rates, aliases, clips, shoot: AdditiveChannel::default(), offset: AdditiveChannel::default(), offset_type: OffsetType::default(), blend_ms })
    }

    /// The drivers for this frame (UpdateWeapon 0x140d7ec11 / 0x140d7b850), then the channels advance.
    fn step(&mut self, weapon: usize, status: &HandsStatus, empty: bool, now: i32) {
        let Some(d) = self.decls.get(weapon).cloned() else { return };
        let (rate, rate_zoomed) = self.rates.get(weapon).copied().unwrap_or((1.0, 1.0));
        let inp = LoopingShootInput {
            enable: true,
            dest_looping: status.dest_looping,
            hands_flags1: status.flags1,
            zoomed: status.zoomed,
            channel_done: self.shoot.done(),
            playing_normal: self.shoot.playing(&d.looping_shoot),
            playing_zoomed: self.shoot.playing(&d.looping_shoot_zoomed),
            blend_ms: self.blend_ms,
            rate,
            rate_zoomed,
        };
        if let Some(cmd) = looping_shoot(&d, &inp) {
            self.shoot.apply(&cmd, now, &self.aliases);
        }
        let current = self.offset.cur.as_ref().map(|c| c.0.clone());
        let oinp = OffsetInput { right_hand: true, zoomed: status.zoomed, empty, blend_duration_cvar: -1, current_alias: current, ..Default::default() };
        if let Some(cmd) = additive_offset(&d, &oinp, &mut self.offset_type) {
            self.offset.apply(&cmd, now, &self.aliases);
        }
        self.shoot.update(now);
        self.offset.update(now);
    }

    fn merge(&self, base: &mut Locals, now: i32) {
        // Stack order: additive shoot, then additive offset.
        for ch in [&self.shoot, &self.offset] {
            if ch.merge <= 0.0 {
                continue;
            }
            if let Some(tree) = ch.tree(now, &self.clips) {
                let add = crate::animweb::eval(Some(&tree), &identity_locals(base.rot.len()), &self.clips);
                add_right(base, &add, ch.merge);
            }
        }
    }
}

/// The animated hands bob cycle (idHandsBobCycle, web player/fp_hands_bob_cycle), used by weapons whose
/// weaponBob is disabled (the fists): an additive layer merged onto the hands pose with ADD_RIGHT.
pub struct BobCycleRig {
    pub web: AnimWebRuntime,
    pub cv: BobCycleCvars,
    pub ctl: HandsBobCycle,
    /// Per arsenal weapon: the weapon decl's handsBobCycle, initialised against the web.
    pub decls: Vec<Option<HandsBobCycleDecl>>,
    /// The player's default bob-cycle decl (player/default).
    pub default_decl: HandsBobCycleDecl,
    pub clips: Clips,
    /// ADD_RIGHT merge alpha of the last update.
    pub alpha: f32,
    sub: String,
    /// Footsteps the bob cycle asked for this frame.
    pub feet: Vec<BobCycleFootstep>,
}

impl BobCycleRig {
    pub fn load(c: &idres::Container, db: &idres::decldb::DeclDb, cvars: &rancher_sim::config::CvarValues, weapon_decls: &[String], speeds: &PlayerSpeeds) -> anyhow::Result<Self> {
        let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands_bob_cycle.decl")?).into_owned();
        let web = std::sync::Arc::new(idres::animweb::AnimWeb::parse(&text)?);
        let names: Vec<Option<String>> = weapon_decls.iter().map(|w| db.get("weapon", w).ok().and_then(|b| b.str("edit.handsBobCycle").map(str::to_string))).collect();
        let mut default_decl = HandsBobCycleDecl::from_decl(db, "player/default")?;
        let mut decls: Vec<Option<HandsBobCycleDecl>> = names.iter().map(|n| n.as_ref().and_then(|n| HandsBobCycleDecl::from_decl(db, n).ok())).collect();
        let mut subs: Vec<String> = decls.iter().flatten().map(|d| d.subweb.clone()).chain(std::iter::once(default_decl.subweb.clone())).collect();
        subs.sort();
        subs.dedup();
        let sub_refs: Vec<&str> = subs.iter().map(String::as_str).collect();
        let data = std::sync::Arc::new(rancher_sim::animweb::AnimData::load(c, &web, &sub_refs));
        let mut rt = AnimWebRuntime::new(web.clone(), data);
        rt.hands_weights = false;
        default_decl.init(&rt, speeds);
        for d in decls.iter_mut().flatten() {
            d.init(&rt, speeds);
        }
        let mut clips = Clips::default();
        for s in &subs {
            if let Some(sw) = web.sub_web(s) {
                for n in &sw.nodes {
                    for t in &n.trees {
                        for a in &t.anims {
                            clips.load(c, &a.name);
                        }
                    }
                }
            }
        }
        let cv = BobCycleCvars::from_cvars(cvars);
        let ctl = HandsBobCycle::new(&cv);
        Ok(BobCycleRig { web: rt, cv, ctl, decls, default_decl, clips, alpha: 0.0, sub: String::new(), feet: Vec::new() })
    }
}

const LEFT_HAND: &str = "lefthandattach";
const RIGHT_HAND: &str = "righthandattach";
const LEFT_SHOULDER: &str = "rig_arm_left_root";
const RIGHT_SHOULDER: &str = "rig_arm_right_root";

#[derive(Resource)]
pub struct HandsLayersState {
    pub layers: HandsLayers,
    /// Per arsenal weapon.
    pub decls: Vec<HandsLayerDecl>,
    pub cvars: HandsLayerCvars,
    prev_view_angles: [f32; 3],
    weapon: Option<usize>,
    /// righthandattach's model-space origin in the last built hands pose (LagInput.right_hand_origin).
    right_hand_origin: G3,
    /// The footstep the bob asked for this frame (for the sound code).
    pub step: Option<BobStep>,
    pub bob_cycle: Option<BobCycleRig>,
    pub additive: Option<AdditiveRig>,
    /// Game time of the last step (for the additive pose).
    now: i32,
}

impl HandsLayersState {
    pub fn new(decls: Vec<HandsLayerDecl>, cvars: HandsLayerCvars) -> Self {
        HandsLayersState { layers: HandsLayers::default(), decls, cvars, prev_view_angles: [0.0; 3], weapon: None, right_hand_origin: G3::ZERO, step: None, bob_cycle: None, additive: None, now: 0 }
    }

    pub fn decl(&self, weapon: usize) -> Option<&HandsLayerDecl> {
        self.decls.get(weapon)
    }

    /// One game frame for the weapon on screen (after the player think). Equipping another weapon resets the layers.
    #[allow(clippy::too_many_arguments)]
    pub fn step(&mut self, weapon: usize, player: &Player, cmd: &UserCmd, last_fire: Option<i32>, status: &HandsStatus, empty: bool, now: i32, msec: i32) {
        self.now = now;
        if let Some(a) = self.additive.as_mut() {
            a.step(weapon, status, empty, now);
        }
        if self.weapon != Some(weapon) {
            self.layers.reset();
            self.weapon = Some(weapon);
        }
        let Some(decl) = self.decls.get(weapon).copied() else { return };
        // playerPState_t.acceleration and nonPushedVelocity (physics current +0x30 / +0x24).
        let vel = player.physics.velocity;
        let lag = LagInput {
            acceleration: player.physics.acceleration,
            view_angles: player.view_angles,
            prev_view_angles: self.prev_view_angles,
            right_hand_origin: self.right_hand_origin,
            msec,
        };
        let bob = BobInput { velocity: vel, on_ground: player.physics.ground_plane, crouched: player.physics.ducked(), msec };
        self.step = self.layers.update(&decl, &self.cvars, &lag, &bob);
        self.prev_view_angles = player.view_angles;
        // The animated bob cycle (idHandsBobCycle::Update 0x140d83e80): zero alpha for weapons with weaponBob.
        if let Some(bc) = self.bob_cycle.as_mut() {
            let weapon_decl = bc.decls.get(weapon).is_some_and(Option::is_some);
            let mut d = bc.decls.get(weapon).cloned().flatten().unwrap_or_else(|| bc.default_decl.clone());
            if d.subweb != bc.sub {
                bc.web.set_state(&d.subweb, "idle");
                bc.sub = d.subweb.clone();
            }
            let f = player.physics.flags;
            let inp = BobCycleInput {
                forward_move: cmd.forward,
                right_move: cmd.right,
                velocity: player.physics.velocity,
                view_yaw: player.view_angles[1],
                on_ground: player.physics.ground_plane,
                crouched: player.physics.ducked(),
                jumped: f & rancher_sim::physics::flags::JUMPED != 0,
                double_jumped: f & rancher_sim::physics::flags::DOUBLE_JUMPED != 0,
                weapon_last_fire_time: last_fire,
                sprint_rate_cap: f32::MAX,
                now,
                msec,
                ..Default::default()
            };
            bc.alpha = bc.ctl.update(&mut bc.web, &mut d, &bc.cv, &inp, decl.bob.enable, weapon_decl);
            bc.feet.clear();
            if !decl.bob.enable {
                let fired = bc.web.update(now);
                bc.feet = bc.ctl.web_events(&bc.web, &fired, &bc.cv, now);
            }
            match bc.decls.get_mut(weapon) {
                Some(Some(slot)) => *slot = d,
                _ => bc.default_decl = d,
            }
        }
    }

    /// (shoot channel merge alpha, offset channel merge alpha, bob-cycle alpha) for traces.
    pub fn trace_alphas(&self) -> (f32, f32, f32) {
        let (s, o) = self.additive.as_ref().map(|a| (a.shoot.merge, a.offset.merge)).unwrap_or((0.0, 0.0));
        (s, o, self.bob_cycle.as_ref().map(|b| b.alpha).unwrap_or(0.0))
    }

    /// Merges the bob-cycle web pose onto the hands local pose with ADD_RIGHT (ANIMWEB.md 3c / 5): the bob anims
    /// are additive deltas (unkeyed joints identity); per joint q = nlerp(qBase, qAdd * qBase, t),
    /// T = Tb + Ta * t, S = Sb - (Sb - Sb * Sa) * t, with t = alpha (full weight planes).
    pub fn add_bob_cycle(&self, base: &mut Locals) {
        // Hands stack order (handlayers-re): web, bob cycle, hit reactions (not wired: no player damage yet),
        // additive shoot, additive offset; the weapon-lag mods and the IK follow in viewanim.
        if let Some(bc) = &self.bob_cycle {
            if bc.alpha > 0.0 {
                if let Some(tree) = bc.web.pose_tree(false) {
                    let add = crate::animweb::eval(Some(&tree), &identity_locals(base.rot.len()), &bc.clips);
                    add_right(base, &add, bc.alpha);
                }
            }
        }
        if let Some(a) = &self.additive {
            a.merge(base, self.now);
        }
    }

    /// Applies the lag / bob joint mods: each mod at full strength to a copy of the joint's local transform
    /// (0x141737280, MODEL | ROTATION | TRANSLATION against its parent's model transform), merged back at
    /// LAG_MERGE_ALPHA (0.95) with the LERP kernel, then FK. Run before the arm IK (its targets hang off the
    /// attach joints).
    pub fn apply(&mut self, pb: &mut PoseBuf, names: &[String]) {
        let find = |n: &str| names.iter().position(|x| x.eq_ignore_ascii_case(n));
        let (Some(lh), Some(rh), Some(ls), Some(rs)) = (find(LEFT_HAND), find(RIGHT_HAND), find(LEFT_SHOULDER), find(RIGHT_SHOULDER)) else { return };
        let origins = JointOrigins { left_hand: pb.model[lh].pos, right_hand: pb.model[rh].pos, left_shoulder: pb.model[ls].pos, right_shoulder: pb.model[rs].pos };
        self.right_hand_origin = origins.right_hand;
        let mut mods: Vec<(usize, rancher_sim::handlayers::JointMod)> = self.layers.joint_mods(&origins).into_iter().filter_map(|m| find(m.joint).map(|j| (j, m))).collect();
        mods.sort_by_key(|(j, _)| *j);
        for (j, m) in mods {
            let p = pb.parents[j];
            let parent = if p >= 0 && (p as usize) < j {
                let x = pb.model[p as usize];
                ParentModel { pos: x.pos, rot: x.rot, scale: x.scale }
            } else {
                ParentModel::IDENTITY
            };
            let l = pb.local[j];
            let out = lagged_local(LocalJoint { t: l.pos, q: l.rot, s: l.scale }, &parent, &m);
            pb.local[j] = Xf { pos: out.t, rot: out.q, scale: out.s };
        }
        pb.fk();
    }

    /// Hands projection FOVs (degrees) for a view with horizontal FOV `view_fov_x` at `aspect` (w / h).
    /// Hands projection FOVs (degrees) for the view's fov_x / fov_y (see [`view_fov`]). `zoom` =
    /// (GetZoomFraction, zoomHandsWeaponFovRatio) from the weapon code, None when it isn't running.
    pub fn projection_fov(&self, weapon: usize, view_fov_x: f32, view_fov_y: f32, zoom: Option<(f32, f32)>) -> (f32, f32) {
        let decl = self.decls.get(weapon).copied().unwrap_or_default();
        let (zoom_fraction, zoom_ratio) = zoom.unwrap_or((0.0, zoom_hands_fov_target(&decl)));
        let s = hands_fov_scale(&decl, &self.cvars, &FovInput { has_item: true, zoom_fraction, zoom_ratio, force_scale: 0.0, local_view: true });
        hands_projection_fov(s, view_fov_x, view_fov_y, self.cvars.hands_fov_vertical_scale_hack)
    }
}

/// The camera's fov_x / fov_y in degrees (0x140e6bc00): `fov` is horizontal at 16:9 (fov_y = 2 atan(tan(fov/2) *
/// 0.5625)); the horizontal FOV follows the render aspect clamped to [1, 3] (Hor+). A fov below 1 falls back
/// to 90 (with the engine's warning).
pub fn view_fov(fov: f32, aspect: f32) -> (f32, f32) {
    let fov = if fov < 1.0 { 90.0 } else { fov };
    let t = (fov * 0.017453292 * 0.5).tan();
    let fy = (t * 0.5625).atan();
    let fy = fy * 57.295776 + fy * 57.295776;
    let a = aspect.clamp(1.0, 3.0);
    let fx = ((fy * 0.017453292 * 0.5).tan() * a).atan();
    (fx * 57.295776 + fx * 57.295776, fy)
}

/// Infinite reverse-Z perspective with independent horizontal / vertical scales (the hands' fovX / fovY).
#[derive(Debug, Clone)]
pub struct HandsProjection {
    pub x_scale: f32,
    pub y_scale: f32,
    pub near: f32,
    pub far: f32,
}

impl HandsProjection {
    pub fn from_fov(fov_x_deg: f32, fov_y_deg: f32, near: f32, far: f32) -> Self {
        HandsProjection { x_scale: 1.0 / (fov_x_deg.to_radians() * 0.5).tan(), y_scale: 1.0 / (fov_y_deg.to_radians() * 0.5).tan(), near, far }
    }
}

impl CameraProjection for HandsProjection {
    fn get_clip_from_view(&self) -> Mat4 {
        Mat4::from_cols(Vec4::new(self.x_scale, 0.0, 0.0, 0.0), Vec4::new(0.0, self.y_scale, 0.0, 0.0), Vec4::new(0.0, 0.0, 0.0, -1.0), Vec4::new(0.0, 0.0, self.near, 0.0))
    }
    fn get_clip_from_view_for_sub(&self, _sub_view: &SubCameraView) -> Mat4 {
        self.get_clip_from_view()
    }
    fn update(&mut self, _width: f32, _height: f32) {}
    fn far(&self) -> f32 {
        self.far
    }
    fn get_frustum_corners(&self, z_near: f32, z_far: f32) -> [Vec3A; 8] {
        let (ax, ay) = (z_near.abs() / self.x_scale, z_near.abs() / self.y_scale);
        let (bx, by) = (z_far.abs() / self.x_scale, z_far.abs() / self.y_scale);
        [
            Vec3A::new(ax, -ay, z_near),
            Vec3A::new(ax, ay, z_near),
            Vec3A::new(-ax, ay, z_near),
            Vec3A::new(-ax, -ay, z_near),
            Vec3A::new(bx, -by, z_far),
            Vec3A::new(bx, by, z_far),
            Vec3A::new(-bx, by, z_far),
            Vec3A::new(-bx, -by, z_far),
        ]
    }
}
