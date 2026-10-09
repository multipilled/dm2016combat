//! idDeclFX (`generated/decls/fx/*.decl`) and the FX manager runtime (idFXManager).
//!
//! - conditions -> actions: 0x1416eefb0 (start conditions; extra conditions must OR to exactly the
//!   caller's extra mask; suppression; cycleEvents)
//! - action start: 0x1416ed600 (random delay from the manager's idRandom, end / fade times, random tag,
//!   random rotation, start origin and axis)
//! - update: 0x1416f0bc0 (frac over [start, end]; loop 0x1416ec730, restart, stop 0x1416ef870)
//! - defaults: the idFXSingleAction ctor 0x1417a6350

use std::sync::Arc;

use anyhow::{Context, Result};
use glam::{Vec3, Vec4};
use idres::decl::{Block, Value};
use idres::decldb::DeclDb;

use crate::decl::*;
use crate::names::{ACTION_TYPES, CONDITIONS, EXTRA_CONDITIONS};
use crate::particle::ParticleDecl;
use crate::sim::System;
use crate::table::Table;
use crate::{Axis, IdRandom};

pub use crate::names;

pub const FX_LIGHT: usize = 0;
pub const FX_PARTICLE: usize = 1;
pub const FX_DECAL: usize = 2;
pub const FX_MODEL: usize = 4;
pub const FX_SOUND: usize = 5;
pub const FX_SCREEN_SHAKE: usize = 6;
pub const FX_RENDERPARM: usize = 9;

pub const ORG_START_POS: usize = 0;
pub const ORG_TRACK_POS: usize = 1;

pub const ROT_START_AXIS: usize = 0;
pub const ROT_START_AXIS_PARENT: usize = 1;
pub const ROT_TRACK_AXIS: usize = 2;
pub const ROT_TRACK_AXIS_PARENT: usize = 3;
pub const ROT_EXPLICIT_ANGLES: usize = 4;

/// fxCondition_t value by name.
pub fn condition(name: &str) -> Option<u32> {
    CONDITIONS.iter().position(|c| c.eq_ignore_ascii_case(name)).map(|i| i as u32)
}

/// fxExtraCondition_t bit by name.
pub fn extra_condition(name: &str) -> u32 {
    EXTRA_CONDITIONS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|e| e.1).unwrap_or(0)
}

pub const EXTRA_PRIMARY_FIRE: u32 = 16;
pub const EXTRA_SECONDARY_FIRE: u32 = 32;

/// fxScreenShakeParms_t (action +0x268; ctor 0x1417a6350: magnitude 0, maxAngles / maxOffset 0.2 each, no decls).
/// maxAngles / maxOffset are not kept: the player ignores them (VIEWFX.md, 0x140e3b3e0).
#[derive(Debug, Clone, Default)]
pub struct ScreenShakeParms {
    pub magnitude: f32,
    /// viewShakeDecl (+0x288), a screenviewshake decl.
    pub view_shake: Option<String>,
    /// advancedShakeDecl (+0x290), an advancedscreenviewshake decl; it wins over the other parms.
    pub advanced: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LightParms {
    pub radius: Vec3,
    pub intensity: f32,
    pub specular_scale: f32,
    pub material: String,
    pub intensity_table: Option<Table>,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub name: String,
    pub group: String,
    pub kind: usize,
    pub duration: f32,
    pub delay: [f32; 2],
    pub restart: bool,
    pub looping: bool,
    pub force_loop_restart: bool,
    pub fade_in: f32,
    pub fade_out: f32,
    /// fadeStartDistance (+0x30, ctor 0) and distance (+0x34, ctor -1 = every view in range).
    pub fade_start_distance: f32,
    pub distance: f32,
    pub size: f32,
    pub color: Vec4,
    pub tags: Vec<String>,
    pub multi_tag: usize,
    pub start: Vec<u32>,
    pub stop: Vec<u32>,
    /// OR of the extraCondition list (None when the list is empty).
    pub extra: Option<u32>,
    pub suppress: Vec<u32>,
    pub origin_type: usize,
    pub offset: Vec3,
    pub local_offset: bool,
    pub rotation_type: usize,
    /// rotOffsetAngles as a matrix (rotOffsetCached).
    pub rot_offset: Axis,
    pub rnd_rot: [[f32; 2]; 3],
    pub explicit_angles: [f32; 3],
    pub light: LightParms,
    pub screen_shake: ScreenShakeParms,
    pub particle: Option<Arc<ParticleDecl>>,
    pub particle_name: String,
    pub track_velocity: bool,
    pub implicit: bool,
    pub triggered: bool,
}

#[derive(Debug, Clone)]
pub struct FxDecl {
    pub name: String,
    pub actions: Vec<Action>,
    pub cycle_max: i32,
    pub cycle_start: u32,
    pub cycle_events: bool,
}

fn conds(b: Option<&Block>, key: &str) -> Vec<u32> {
    list_str(b, key).iter().filter_map(|s| condition(s)).collect()
}

impl Action {
    fn read(db: &DeclDb, b: Option<&Block>) -> Action {
        let lp = block(b, "lightParms");
        let pp = block(b, "particleParms");
        let sp = block(b, "screenShakeParms");
        let rot = angles_or(b, "rotOffsetAngles");
        let extra_list = list_str(b, "extraCondition");
        let particle_name = str_of(pp, "declPrt").unwrap_or("").to_string();
        let kind = enum_or(b, "type", &ACTION_TYPES, 21);
        let particle = if kind == FX_PARTICLE && !particle_name.is_empty() {
            match ParticleDecl::load(db, &particle_name) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("fx: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let intensity_table = str_of(lp, "intensityTable").and_then(|t| db.container().read_by_name(&format!("generated/decls/table/{t}.decl")).ok()).and_then(|bytes| Table::parse(&String::from_utf8_lossy(&bytes)).ok());
        Action {
            name: str_of(b, "name").unwrap_or("").to_string(),
            group: str_of(b, "group").unwrap_or("").to_string(),
            kind,
            duration: f32_or(b, "duration", 2.0),
            delay: {
                let v = vec2_or(b, "delay", glam::Vec2::ZERO);
                [v.x, v.y]
            },
            restart: bool_or(b, "restart", false),
            looping: bool_or(b, "looping", false),
            force_loop_restart: bool_or(b, "forceLoopRestart", false),
            fade_in: f32_or(b, "fadeInTime", 0.0),
            fade_out: f32_or(b, "fadeOutTime", 0.0),
            fade_start_distance: f32_or(b, "fadeStartDistance", 0.0),
            distance: f32_or(b, "distance", -1.0),
            size: f32_or(b, "size", 1.0),
            color: vec4_or(b, "color", Vec4::ONE),
            tags: list_str(b, "tagNames"),
            multi_tag: enum_or(b, "multiTagUseType", &["FX_MULTI_TAG_USE_RND", "FX_MULTI_TAG_USE_EXPLICIT", "FX_MULTI_TAG_USE_ALL"], 0),
            start: conds(b, "startCondition"),
            stop: conds(b, "stopCondition"),
            extra: if extra_list.is_empty() { None } else { Some(extra_list.iter().map(|s| extra_condition(s)).fold(0, |a, v| a | v)) },
            suppress: conds(b, "suppressCondition"),
            origin_type: enum_or(b, "originType", &["FX_ORG_START_POS", "FX_ORG_TRACK_POS"], 0),
            offset: vec3_or(b, "offset", Vec3::ZERO),
            local_offset: bool_or(b, "localOffset", false),
            rotation_type: enum_or(
                b,
                "rotationType",
                &[
                    "FX_ROT_START_AXIS",
                    "FX_ROT_START_AXIS_PARENT",
                    "FX_ROT_TRACK_AXIS",
                    "FX_ROT_TRACK_AXIS_PARENT",
                    "FX_ROT_EXPLICIT_ANGLES",
                    "FX_ROT_EXPLICIT_TABLES",
                    "FX_ROT_EXPLICIT_TABLES_LOCAL",
                    "FX_ROT_EXPLICIT_CODE",
                ],
                0,
            ),
            rot_offset: Axis::from_angles(rot[0], rot[1], rot[2]),
            rnd_rot: {
                let x = vec2_or(b, "rndRotX", glam::Vec2::ZERO);
                let y = vec2_or(b, "rndRotY", glam::Vec2::ZERO);
                let z = vec2_or(b, "rndRotZ", glam::Vec2::ZERO);
                [[x.x, x.y], [y.x, y.y], [z.x, z.y]]
            },
            explicit_angles: angles_or(b, "explicitAngles"),
            light: LightParms {
                radius: vec3_or(lp, "radius", Vec3::ZERO),
                intensity: f32_or(lp, "intensity", 1.0),
                specular_scale: f32_or(lp, "specularScale", 1.0),
                material: str_of(lp, "lightMtr").unwrap_or("").to_string(),
                intensity_table,
            },
            screen_shake: ScreenShakeParms {
                magnitude: f32_or(sp, "magnitude", 0.0),
                view_shake: str_of(sp, "viewShakeDecl").filter(|s| !s.is_empty()).map(str::to_string),
                advanced: str_of(sp, "advancedShakeDecl").filter(|s| !s.is_empty()).map(str::to_string),
            },
            particle,
            particle_name,
            track_velocity: bool_or(pp, "trackVelocity", false),
            implicit: bool_or(b, "implicit", false),
            triggered: bool_or(b, "triggered", false),
        }
    }
}

impl FxDecl {
    pub fn load(db: &DeclDb, name: &str) -> Result<Arc<FxDecl>> {
        let b = match db.get("fx", name) {
            Ok(b) => b,
            Err(e) => {
                // Some shipped decls end before their last closing braces (fx/weapons/fists/fists_1st); the
                // engine's lexer closes open blocks at end of file. Retry with the braces balanced.
                let path = format!("generated/decls/fx/{name}.decl");
                let mut text = String::from_utf8_lossy(&db.container().read_by_name(&path).with_context(|| format!("fx decl {name}"))?).into_owned();
                let open = text.matches('{').count();
                let close = text.matches('}').count();
                if open <= close {
                    return Err(e).with_context(|| format!("fx decl {name}"));
                }
                text.push_str(&"}".repeat(open - close));
                Arc::new(idres::decl::parse(&text).with_context(|| format!("fx decl {name}"))?)
            }
        };
        let edit = block(Some(&b), "edit");
        let actions = list(edit, "editEvents").into_iter().map(|v: &Value| Action::read(db, v.as_block())).collect();
        Ok(Arc::new(FxDecl {
            name: name.to_string(),
            actions,
            cycle_max: i32_or(edit, "cycleConditionMax", -1),
            cycle_start: str_of(edit, "cycleStartCondition").and_then(condition).unwrap_or(0),
            cycle_events: bool_or(edit, "cycleEvents", false),
        }))
    }
}

/// idFXActionData: the manager's per-action runtime state.
#[derive(Debug, Clone, Default)]
pub struct ActionState {
    pub active: bool,
    pub stopping: bool,
    pub requested: bool,
    pub delay_ms: i32,
    pub trigger_ms: i32,
    pub end_ms: i32,
    pub fade_in_start_ms: i32,
    pub fade_in_end_ms: i32,
    pub fade_out_start_ms: i32,
    pub tag: i32,
    pub random_axis: Option<Axis>,
    pub start_origin: Vec3,
    pub start_axis: Axis,
    /// Origin and axis this frame.
    pub origin: Vec3,
    pub axis: Axis,
    pub system: Option<System>,
    /// Run-state bit 4: the action's one-shot start work (FX_SCREEN_SHAKE) is done.
    pub fired: bool,
}

impl ActionState {
    pub fn start_ms(&self) -> i32 {
        self.delay_ms + self.trigger_ms
    }
}

/// A light the FX wants this frame.
#[derive(Debug, Clone)]
pub struct LightOut {
    pub action: usize,
    pub origin: Vec3,
    pub axis: Axis,
    pub color: Vec4,
    pub intensity: f32,
    pub radius: Vec3,
    /// INTERIM fade (fade in/out times), see FX.md.
    pub fade: f32,
}

/// A FX_SCREEN_SHAKE start or stop for the views to apply (the manager does not know the listeners).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShakeEvent {
    /// First update of a running shake action (0x1416f2b72): `origin` is the manager's origin (the update's
    /// position argument), `action_origin` the action's own origin (the in-range test's point).
    Start { action: usize, origin: Vec3, action_origin: Vec3 },
    /// The final stop of an action without a shake decl (0x1416ef870 case 6): the camera shake goes to 0.
    Stop { action: usize },
}

/// What a screen-shake action asks of one listener (player) when it starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShakeCall<'a> {
    /// idPlayer vslot 0x38 (0x140e3acb0): advanced shake, magnitude from the distance between fadeStartDistance
    /// and distance.
    Advanced { decl: &'a str, distance: f32, start: f32, end: f32 },
    /// idPlayer vslot 0x30 (0x140e3b4f0): decl shake with the distance fade applied here.
    Decl { decl: &'a str, magnitude: f32 },
    /// idPlayer vslot 0x28 (0x140e3b3e0): the view's FX camera shake (the view applies the distance fade).
    Camera { magnitude: f32, position: Vec3, fade_start: f32, fade_end: f32 },
}

/// |a - b| as the shake start writes it: len2 * InvSqrt(len2), rsqrtss refined by two Newton steps on
/// max(len2, FLT_MIN) (0x1416f2c15..0x1416f2c93).
pub(crate) fn shake_distance(a: Vec3, b: Vec3) -> f32 {
    let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
    let len2 = dy * dy + dx * dx + dz * dz;
    let x = len2.max(f32::MIN_POSITIVE);
    let mut y = 1.0 / x.sqrt();
    y = (x * y * y - 3.0) * y * -0.5;
    y = (x * y * y - 3.0) * y * -0.5;
    y * len2
}

/// The FX_SCREEN_SHAKE start for one listener whose view is at `view` (0x1416f0bc0 case 6,
/// 0x1416f2b72..0x1416f2de8). None when the listener is out of range: in range is distance == -1 or
/// |view - action origin|^2 < distance^2 (the update's per-listener record from listener vslot 0x20).
pub fn screen_shake_start(a: &Action, origin: Vec3, action_origin: Vec3, view: Vec3) -> Option<ShakeCall<'_>> {
    shake_start(&a.screen_shake, a.fade_start_distance, a.distance, origin, action_origin, view)
}

fn shake_start(p: &ScreenShakeParms, start: f32, end: f32, origin: Vec3, action_origin: Vec3, view: Vec3) -> Option<ShakeCall<'_>> {
    let (rx, ry, rz) = (view.x - action_origin.x, view.y - action_origin.y, view.z - action_origin.z);
    let r2 = rx * rx + ry * ry + rz * rz;
    if !(end == -1.0 || r2 < end * end) {
        return None;
    }
    if let Some(decl) = p.advanced.as_deref() {
        return Some(ShakeCall::Advanced { decl, distance: shake_distance(origin, view), start, end });
    }
    if let Some(decl) = p.view_shake.as_deref() {
        let d = shake_distance(origin, view);
        let magnitude = if d > start { (1.0 - (d - start) / (end - start).max(0.001)).min(1.0).max(0.0) } else { 1.0 };
        return Some(ShakeCall::Decl { decl, magnitude });
    }
    Some(ShakeCall::Camera { magnitude: p.magnitude, position: origin, fade_start: start, fade_end: end })
}

/// Where a tag (model joint) is this frame, in the FX's world space.
pub trait TagSource {
    fn tag(&self, name: &str) -> Option<(Vec3, Axis)>;
    /// The parent entity's origin and axis (used when an action has no tag).
    fn parent(&self) -> (Vec3, Axis);
}

/// One idFXManager: a decl and its actions' state.
#[derive(Debug, Clone)]
pub struct FxManager {
    pub decl: Arc<FxDecl>,
    pub actions: Vec<ActionState>,
    pub random: IdRandom,
    /// systemColor (multiplies action colours).
    pub color: Vec4,
    pub size_scalar: f32,
    cycle_counter: i32,
    /// Particle systems of stopped actions still draining their live particles.
    pub draining: Vec<(usize, System, Vec3, Axis, Vec4)>,
    /// FX_SCREEN_SHAKE starts / stops since the caller last took them.
    pub shakes: Vec<ShakeEvent>,
}

impl FxManager {
    pub fn new(decl: Arc<FxDecl>, seed: u32) -> FxManager {
        let n = decl.actions.len();
        FxManager { decl, actions: vec![ActionState::default(); n], random: IdRandom(seed), color: Vec4::ONE, size_scalar: 1.0, cycle_counter: 0, draining: Vec::new(), shakes: Vec::new() }
    }

    fn suppressed(&self, now: i32) -> Vec<u32> {
        let mut out = Vec::new();
        for (i, a) in self.actions.iter().enumerate() {
            if a.active && a.start_ms() <= now {
                out.extend_from_slice(&self.decl.actions[i].suppress);
            }
        }
        out
    }

    /// Starts every action whose start condition is `cond` and whose extra conditions match `extra`
    /// (0x1416eefb0). Returns the number started.
    pub fn condition(&mut self, cond: u32, extra: u32, now: i32, tags: &dyn TagSource) -> usize {
        let decl = self.decl.clone();
        let mut cond = cond;
        if decl.cycle_events && decl.cycle_start == cond {
            let k = if decl.cycle_max > 0 { self.cycle_counter % decl.cycle_max } else { self.cycle_counter };
            self.cycle_counter += 1;
            cond = (cond + k as u32) % 0x240;
        }
        let suppressed = self.suppressed(now);
        let mut started = 0;
        for (i, a) in decl.actions.iter().enumerate() {
            if !a.start.contains(&cond) {
                continue;
            }
            if a.extra.is_some_and(|e| e != extra) {
                continue;
            }
            if a.start.iter().any(|c| suppressed.contains(c)) {
                continue;
            }
            self.start_action(i, now, tags);
            started += 1;
        }
        started
    }

    /// The manager's stop entry (0x1416ec640 -> 0x1416f00c0): stops running actions listing `cond` as a
    /// stop condition (with their fade-out).
    pub fn stop_condition(&mut self, cond: u32, now: i32) {
        for i in 0..self.actions.len() {
            if self.actions[i].active && self.decl.actions[i].stop.contains(&cond) {
                self.stop_action(i, now, false);
            }
        }
    }

    /// Stops every running looping action (with its fade-out); one-shot actions run out.
    pub fn stop_looping(&mut self, now: i32) {
        for i in 0..self.actions.len() {
            if self.actions[i].active && self.decl.actions[i].looping {
                self.stop_action(i, now, false);
            }
        }
    }

    /// True while an action runs or a stopped action's particles still drain.
    pub fn busy(&self) -> bool {
        self.actions.iter().any(|a| a.active) || !self.draining.is_empty()
    }

    /// 0x1416ed600
    fn start_action(&mut self, i: usize, now: i32, tags: &dyn TagSource) {
        let a = &self.decl.actions[i];
        let st = &self.actions[i];
        if st.active && a.looping && !a.force_loop_restart && !st.stopping {
            return;
        }
        let r = self.random.random_float();
        let delay = ((r * (a.delay[1] - a.delay[0]) + a.delay[0]) * 1000.0) as i32;
        let start = delay + now;
        let mut tag = 0;
        if a.multi_tag == 0 && a.tags.len() > 1 {
            tag = (self.random.next_int() % a.tags.len() as u32) as i32;
        }
        let rx = self.random.random_float() * (a.rnd_rot[0][1] - a.rnd_rot[0][0]) + a.rnd_rot[0][0];
        let ry = self.random.random_float() * (a.rnd_rot[1][1] - a.rnd_rot[1][0]) + a.rnd_rot[1][0];
        let rz = self.random.random_float() * (a.rnd_rot[2][1] - a.rnd_rot[2][0]) + a.rnd_rot[2][0];
        // INTERIM: angle order (pitch, yaw, roll) <- (rndRotX, rndRotY, rndRotZ) via idAngles::ToQuat.
        let random_axis = Axis::from_angles(rx, ry, rz);
        let (org, axis) = self.place(i, &random_axis, tags);
        let st = &mut self.actions[i];
        st.active = true;
        st.stopping = false;
        st.requested = true;
        st.delay_ms = delay;
        st.trigger_ms = now;
        st.end_ms = (delay - (a.duration * -1000.0) as i32) + now;
        st.fade_in_start_ms = start;
        st.fade_in_end_ms = start - (a.fade_in * -1000.0) as i32;
        st.fade_out_start_ms = start - ((a.duration - a.fade_out) * -1000.0) as i32;
        st.tag = tag;
        st.random_axis = Some(random_axis);
        st.fired = false;
        st.start_origin = org;
        st.start_axis = axis;
        st.origin = org;
        st.axis = axis;
        if let Some(sys) = st.system.take() {
            let c = self.color * a.color;
            self.draining.push((i, sys, st.origin, st.axis, c));
        }
    }

    /// Origin and axis from the tag (or parent), rotation offset and random rotation (0x1416ed600 tail).
    fn place(&self, i: usize, random_axis: &Axis, tags: &dyn TagSource) -> (Vec3, Axis) {
        let a = &self.decl.actions[i];
        let st = &self.actions[i];
        let tag_name = a.tags.get(st.tag.max(0) as usize).or(a.tags.first());
        let (torg, taxis) = tag_name.and_then(|t| tags.tag(t)).unwrap_or_else(|| tags.parent());
        let base = match a.rotation_type {
            ROT_START_AXIS_PARENT | ROT_TRACK_AXIS_PARENT => tags.parent().1,
            ROT_EXPLICIT_ANGLES => Axis::from_angles(a.explicit_angles[0], a.explicit_angles[1], a.explicit_angles[2]),
            _ => taxis,
        };
        let axis = a.rot_offset.mul(random_axis).mul(&base);
        let off = if a.local_offset { axis.to_local(a.offset) } else { axis.to_parent(a.offset) };
        (torg + off, axis)
    }

    /// 0x1416ef870: fade out (stopping) when the action has a fade-out time, else stop now.
    pub fn stop_action(&mut self, i: usize, now: i32, immediate: bool) {
        let a = &self.decl.actions[i];
        let st = &mut self.actions[i];
        if !st.active {
            return;
        }
        if a.fade_out > 0.0 && !immediate && !st.stopping && (st.end_ms - now > 0 || a.looping) {
            if now < st.fade_out_start_ms {
                st.fade_out_start_ms = now;
                st.end_ms = now - (a.fade_out * -1000.0) as i32;
            }
            st.stopping = true;
            return;
        }
        st.active = false;
        st.stopping = false;
        st.requested = false;
        if a.kind == FX_SCREEN_SHAKE && a.screen_shake.advanced.is_none() && a.screen_shake.view_shake.is_none() {
            // Listener vslot 0x28 with magnitude 0, origin 0, fades 0 / -1.
            self.shakes.push(ShakeEvent::Stop { action: i });
        }
        if let Some(mut sys) = st.system.take() {
            // INTERIM: the engine hands the particle model back to the FX resource manager; here its live
            // particles finish their life (no new spawns after the stop time).
            sys.stop_ms = now;
            let c = self.color * a.color;
            self.draining.push((i, sys, st.origin, st.axis, c));
        }
    }

    /// Advances every running action to `now` (0x1416f0bc0). Creates particle systems as actions start.
    pub fn update(&mut self, now: i32, tags: &dyn TagSource) {
        for i in 0..self.actions.len() {
            if !self.actions[i].active {
                continue;
            }
            let start = self.actions[i].start_ms();
            if now < start {
                continue;
            }
            let a = self.decl.actions[i].clone();
            let st = &self.actions[i];
            let frac = (now as f32 - start as f32) / (st.end_ms as f32 - start as f32);
            if frac >= 1.0 {
                if a.looping && !st.stopping {
                    // 0x1416ec730
                    let st = &mut self.actions[i];
                    st.trigger_ms = now;
                    st.end_ms = now - (a.duration * -1000.0) as i32;
                    st.fade_out_start_ms = now - ((a.duration - a.fade_out) * -1000.0) as i32;
                } else if a.restart && !st.stopping {
                    self.start_action(i, now, tags);
                } else {
                    self.stop_action(i, now, false);
                    continue;
                }
            }
            // Tracking.
            let ra = self.actions[i].random_axis.unwrap_or(Axis::IDENTITY);
            let (org, axis) = self.place(i, &ra, tags);
            let st = &mut self.actions[i];
            st.origin = if a.origin_type == ORG_TRACK_POS { org } else { st.start_origin };
            st.axis = if matches!(a.rotation_type, ROT_TRACK_AXIS | ROT_TRACK_AXIS_PARENT) { axis } else { st.start_axis };
            if a.kind == FX_SCREEN_SHAKE && !st.fired {
                // Fired once per start, whether or not a listener was in range (bit 4 set after the loop).
                st.fired = true;
                let action_origin = st.origin;
                self.shakes.push(ShakeEvent::Start { action: i, origin: tags.parent().0, action_origin });
            }
            let st = &mut self.actions[i];
            if a.kind == FX_PARTICLE && st.system.is_none() {
                if let Some(p) = &a.particle {
                    // Diversity parm: one draw from the manager's random (0x1416f3755).
                    let d = self.random.random_float();
                    // INTERIM: the render model's seed from the diversity parm.
                    let seed = (d * 32767.0) as u32;
                    self.actions[i].system = Some(System::new(p.clone(), now, seed));
                }
            }
        }
        self.draining.retain(|(_, s, ..)| !s.finished(now));
    }

    /// Fade factor of an action at `now` from its fade in/out times (INTERIM, see FX.md).
    pub fn fade(&self, i: usize, now: i32) -> f32 {
        let st = &self.actions[i];
        let mut f = 1.0f32;
        if now < st.fade_in_end_ms && st.fade_in_end_ms > st.fade_in_start_ms {
            f *= ((now - st.fade_in_start_ms) as f32 / (st.fade_in_end_ms - st.fade_in_start_ms) as f32).clamp(0.0, 1.0);
        }
        if now > st.fade_out_start_ms && st.end_ms > st.fade_out_start_ms {
            f *= ((st.end_ms - now) as f32 / (st.end_ms - st.fade_out_start_ms) as f32).clamp(0.0, 1.0);
        }
        f
    }

    /// Lights of running FX_LIGHT actions.
    pub fn lights(&self, now: i32) -> Vec<LightOut> {
        let mut out = Vec::new();
        for (i, st) in self.actions.iter().enumerate() {
            let a = &self.decl.actions[i];
            if !st.active || a.kind != FX_LIGHT || now < st.start_ms() {
                continue;
            }
            let frac = ((now - st.start_ms()) as f32 / (st.end_ms - st.start_ms()).max(1) as f32).clamp(0.0, 1.0);
            let intensity = a.light.intensity_table.as_ref().map(|t| t.lookup(frac)).unwrap_or(a.light.intensity);
            out.push(LightOut { action: i, origin: st.origin, axis: st.axis, color: self.color * a.color, intensity, radius: a.light.radius, fade: self.fade(i, now) });
        }
        out
    }

    /// Every particle system to draw: (action, system, origin, axis, colour).
    pub fn systems_mut(&mut self) -> impl Iterator<Item = (usize, &mut System, Vec3, Axis, Vec4)> {
        let color = self.color;
        let decl = self.decl.clone();
        let live = self.actions.iter_mut().enumerate().filter_map(move |(i, st)| {
            let c = color * decl.actions[i].color;
            let (o, a) = (st.origin, st.axis);
            st.system.as_mut().map(|s| (i, s, o, a, c))
        });
        let drain = self.draining.iter_mut().map(|(i, s, o, a, c)| (*i, s, *o, *a, *c));
        live.chain(drain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parms(view: Option<&str>, advanced: Option<&str>) -> ScreenShakeParms {
        ScreenShakeParms { magnitude: 0.05, view_shake: view.map(str::to_string), advanced: advanced.map(str::to_string) }
    }

    #[test]
    fn screen_shake_branches() {
        let o = Vec3::new(100.0, 0.0, 0.0);
        // fx/breakables/barrel_01 screenShake: advancedShakeDecl, distance 1024, fadeStartDistance 0 (ctor).
        let adv = parms(None, Some("screenviewshake/sp/generic_explosion"));
        let call = shake_start(&adv, 0.0, 1024.0, o, o, Vec3::new(400.0, 0.0, 0.0)).unwrap();
        assert_eq!(call, ShakeCall::Advanced { decl: "screenviewshake/sp/generic_explosion", distance: 300.0, start: 0.0, end: 1024.0 });
        // Out of range: |view - action origin|^2 must be < distance^2 (strict).
        assert_eq!(shake_start(&adv, 0.0, 1024.0, o, o, Vec3::new(1124.0, 0.0, 0.0)), None);
        // distance -1: every view.
        assert!(shake_start(&adv, 0.0, -1.0, o, o, Vec3::new(1.0e5, 0.0, 0.0)).is_some());
        // Decl shake: 1 inside fadeStartDistance, then 1 - (d - start) / max(end - start, 0.001), clamped.
        let decl = parms(Some("sp/x"), None);
        let mag = |d: f32| match shake_start(&decl, 200.0, 1000.0, Vec3::ZERO, Vec3::ZERO, Vec3::new(0.0, d, 0.0)) {
            Some(ShakeCall::Decl { magnitude, .. }) => magnitude,
            c => panic!("{c:?}"),
        };
        assert_eq!(mag(150.0), 1.0);
        assert_eq!(mag(600.0), 0.5);
        // The advanced decl wins over the decl and the plain parms.
        assert!(matches!(shake_start(&parms(Some("a"), Some("b")), 0.0, -1.0, o, o, o), Some(ShakeCall::Advanced { decl: "b", .. })));
        // Plain: magnitude and the fade distances go to the view, which fades by distance itself.
        let plain = parms(None, None);
        assert_eq!(shake_start(&plain, 0.0, -1.0, o, Vec3::ZERO, Vec3::ZERO), Some(ShakeCall::Camera { magnitude: 0.05, position: o, fade_start: 0.0, fade_end: -1.0 }));
    }

    #[test]
    fn shake_distance_is_the_refined_rsqrt() {
        // Two Newton steps make len2 * InvSqrt(len2) land on the true length for these.
        assert_eq!(shake_distance(Vec3::new(3.0, 4.0, 0.0), Vec3::ZERO), 5.0);
        assert_eq!(shake_distance(Vec3::ZERO, Vec3::ZERO), 0.0);
    }
}
