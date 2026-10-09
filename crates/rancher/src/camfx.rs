//! First-person camera effects around rancher_sim::viewfx (VIEWFX.md): the hands rig's animated `camera` joint,
//! the double-jump view pitch, and the screen shakes started by hands anim events. main.rs `place_camera` builds
//! the views from these.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use idres::decldb::DeclDb;
use rancher_sim::handlayers::IdMat3;
use rancher_sim::viewfx::{AdvancedViewShakeDecl, ViewFx, ViewShakeDecl};

#[derive(Resource)]
pub struct CamFx {
    /// The player view's idView effects (decl / FX screen shakes, advanced shakes).
    pub fx: ViewFx,
    db: DeclDb,
    shakes: HashMap<String, Option<ViewShakeDecl>>,
    advanced: HashMap<String, Option<Arc<AdvancedViewShakeDecl>>>,
    /// Double-jump view pitch change (degrees) and duration (ticks): the jump boots decl's
    /// doubleJumpViewPitchChange / Duration, the pm_ cvars for fields it omits.
    pub dj_change: f32,
    pub dj_duration: i32,
    /// playerPState lastDJTime once a double jump happened (the engine's -1 = never is None).
    pub last_dj: Option<i32>,
    /// The hands `camera` joint of the last built frame: translation and axes (rows = the joint's x / y / z
    /// axes) in hands-model space including the fp_hands md6Def offset, idTech axes.
    pub cam: Option<([f32; 3], IdMat3)>,
    /// FX_SCREEN_SHAKE calls on the player from this frame's FX updates (crate::fx), applied in order with the
    /// hands events; the advanced shakes draw from the game LCG.
    pub fx_shakes: Vec<FxShake>,
}

/// One idPlayer screen-shake call from a FX action (idfx::fx::ShakeCall with owned names).
#[derive(Debug, Clone, PartialEq)]
pub enum FxShake {
    /// vslot 0x38 (0x140e3acb0): StartAdvancedViewShake with advanced_distance_magnitude(distance, start, end).
    Advanced { decl: String, distance: f32, start: f32, end: f32 },
    /// vslot 0x30 (0x140e3b4f0): StartViewShake with the FX's distance-faded magnitude.
    Decl { decl: String, magnitude: f32 },
    /// vslot 0x28 (0x140e3b3e0): the view's FX camera shake; a stop sends magnitude 0, origin 0, fades 0 / -1.
    Camera { magnitude: f32, position: [f32; 3], fade_start: f32, fade_end: f32 },
}

impl From<idfx::fx::ShakeCall<'_>> for FxShake {
    fn from(c: idfx::fx::ShakeCall<'_>) -> Self {
        use idfx::fx::ShakeCall as C;
        match c {
            C::Advanced { decl, distance, start, end } => FxShake::Advanced { decl: decl.to_string(), distance, start, end },
            C::Decl { decl, magnitude } => FxShake::Decl { decl: decl.to_string(), magnitude },
            C::Camera { magnitude, position, fade_start, fade_end } => FxShake::Camera { magnitude, position: position.to_array(), fade_start, fade_end },
        }
    }
}

impl CamFx {
    pub fn new(db: DeclDb, cvars: &rancher_sim::config::CvarValues) -> Self {
        let boots = db.get("jumpboots", rancher_sim::install::SP_JUMP_BOOTS).ok();
        let edit = boots.as_ref().and_then(|b| b.block("edit"));
        let f = |key: &str, cvar: &str| edit.and_then(|e| e.f32(key)).unwrap_or_else(|| cvars.f(cvar));
        let dj_change = f("doubleJumpViewPitchChange", "pm_doubleJumpViewPitchChange");
        let dj_duration = f("doubleJumpViewPitchDuration", "pm_doubleJumpViewPitchDuration") as i32;
        CamFx { fx: ViewFx::default(), db, shakes: HashMap::new(), advanced: HashMap::new(), dj_change, dj_duration, last_dj: None, cam: None, fx_shakes: Vec::new() }
    }

    fn shake(&mut self, name: &str) -> Option<ViewShakeDecl> {
        let db = &self.db;
        self.shakes
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| ViewShakeDecl::from_decl(db, name).map_err(|e| eprintln!("screenViewShake {name}: {e:#}")).ok())
            .clone()
    }

    fn advanced_shake(&mut self, name: &str) -> Option<Arc<AdvancedViewShakeDecl>> {
        let db = &self.db;
        self.advanced
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| AdvancedViewShakeDecl::from_decl(db, name).map(Arc::new).map_err(|e| eprintln!("advancedScreenViewShake {name}: {e:#}")).ok())
            .clone()
    }

    /// The hands anim events that start view shakes (idHands 0x140d57d00 / 0x140d57ff0 / 0x140d57b80): the decl
    /// shake is ignored while another one runs; the advanced shake draws from the game LCG. Applies the queued FX
    /// screen shakes ([`FxShake`]) first.
    /// INTERIM: ae_startCameraShakeBerserk (berserk anims only; params anim + advancedViewShake, registered at
    /// 0x140187f60, not in idHands' event map) is not handled.
    pub fn hands_events(&mut self, events: &[crate::viewanim::Fired], now: i32, rng: &mut rancher_sim::weapons::GameRng) {
        for s in std::mem::take(&mut self.fx_shakes) {
            if std::env::var("RANCHER_FX_TRACE").is_ok() {
                println!("[camfx {now}] {s:?}");
            }
            match s {
                FxShake::Advanced { decl, distance, start, end } => {
                    if let Some(d) = self.advanced_shake(&decl) {
                        let mag = rancher_sim::viewfx::advanced_distance_magnitude(distance, start, end);
                        self.fx.start_advanced_shake(d, mag, now, rng);
                    }
                }
                FxShake::Decl { decl, magnitude } => {
                    if let Some(d) = self.shake(&decl) {
                        self.fx.start_view_shake(&d, magnitude, now);
                    }
                }
                FxShake::Camera { magnitude, position, fade_start, fade_end } => self.fx.set_camera_shake(magnitude, position, fade_start, fade_end),
            }
        }
        for f in events {
            let ev = &f.event;
            match ev.name.as_str() {
                "ae_startCameraShake" | "ae_startScreenShake" => {
                    if let Some(d) = ev.param("screenViewShake").and_then(|a| a.text()).and_then(|n| self.shake(n)) {
                        self.fx.start_view_shake(&d, 1.0, now);
                    }
                }
                "ae_startAdvancedScreenShake" => {
                    if let Some(d) = ev.param("advancedScreenViewShake").and_then(|a| a.text()).and_then(|n| self.advanced_shake(n)) {
                        self.fx.start_advanced_shake(d, 1.0, now, rng);
                    }
                }
                _ => {}
            }
        }
    }

    /// Keeps the hands `camera` joint for the next frame's view (the engine reads the last built hands frame).
    pub fn capture_camera_joint(&mut self, joints: &crate::viewanim::ViewJoints) {
        // ViewJoints maps idTech model space (+ the md6Def offset) to view-local Bevy space; undo the axis swap.
        let from_bevy = |v: glam::Vec4| [-v.z, -v.x, v.y];
        self.cam = joints.hands_joint("camera").map(|m| (from_bevy(m.w_axis), [from_bevy(m.x_axis), from_bevy(m.y_axis), from_bevy(m.z_axis)]));
    }
}

/// idTech position to Bevy.
pub fn pos_to_bevy(v: [f32; 3]) -> Vec3 {
    Vec3::new(-v[1], v[2], -v[0])
}

/// A view axis (rows forward / left / up, idTech) as a Bevy camera rotation (camera looks down -Z, +Y up).
pub fn axis_to_bevy(a: &IdMat3) -> Quat {
    let right = pos_to_bevy([-a[1][0], -a[1][1], -a[1][2]]);
    let up = pos_to_bevy(a[2]);
    let back = pos_to_bevy([-a[0][0], -a[0][1], -a[0][2]]);
    Quat::from_mat3(&Mat3::from_cols(right, up, back)).normalize()
}
