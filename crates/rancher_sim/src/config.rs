//! Movement tuning, assembled at startup from the user's own install:
//! cvar defaults scanned from the exe, the shipped default/default_sp configs, and decls.
//! Numeric literals that appear in this crate are only those hard-coded in the game's own code.

use std::collections::HashMap;

use glam::Vec3;

/// Raw cvar values after exe defaults and shipped configs have been applied.
#[derive(Debug, Clone, Default)]
pub struct CvarValues(pub HashMap<String, String>);

impl CvarValues {
    pub fn f(&self, name: &str) -> f32 {
        let v = self.0.get(name).unwrap_or_else(|| panic!("cvar {name} missing from the install's exe"));
        v.trim_end_matches('f').parse().unwrap_or_else(|_| panic!("cvar {name} = {v:?} is not a number"))
    }
    pub fn i(&self, name: &str) -> i32 {
        self.f(name) as i32
    }
    pub fn b(&self, name: &str) -> bool {
        self.f(name) != 0.0
    }
}

/// Double-jump parameters as pushed into physics every frame by the player (`SetDoubleJump`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoubleJump {
    pub height: f32,
    pub speed_scale: f32,
    /// Stored by the game but never read by its physics in this build.
    pub air_english_scale: f32,
    pub window_offset_ms: i32,
    /// Stored by the game but never read by its physics in this build.
    pub window_duration_ms: i32,
}

/// idPlayerMechanicLedgeGrab::testParms_t (reflection 0x1431a98c0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgeTestParms {
    pub forward_test_backup_dist: f32,
    pub forward_up_test_dist: f32,
    pub forward_up_test_dist2: f32,
    pub forward_up_test_dist3: f32,
    pub forward_clear_dist: f32,
    pub side_test_dist: f32,
    pub up_test_dist: f32,
    pub depth_test_dist: f32,
    pub min_dist: f32,
    pub max_dist: f32,
    pub min_height: f32,
    pub ignore_grab_from_fall_height_above_ground: f32,
    pub min_delta_height: f32,
    pub max_delta_height: f32,
    pub grab_mantle_delta_height: f32,
    pub grab_foot_delta_height: f32,
    pub min_depth: f32,
    pub min_clearance_to_hang: f32,
    pub max_delta_pitch: f32,
    pub max_delta_yaw: f32,
    pub max_railing_thickness: f32,
    pub input_time_buffer: i32,
}

impl LedgeTestParms {
    /// `defaultParms` as the mechanic's Clear (0x140da08a0) writes them; minHeight copies pm_jumpheight.
    pub fn defaults(jump_height: f32) -> Self {
        Self {
            forward_test_backup_dist: 20.0,
            forward_up_test_dist: 110.0,
            forward_up_test_dist2: 60.0,
            forward_up_test_dist3: 10.0,
            forward_clear_dist: 50.0,
            side_test_dist: 10.0,
            up_test_dist: 50.0,
            depth_test_dist: 200.0,
            min_dist: 0.0,
            max_dist: 50.0,
            min_height: jump_height,
            ignore_grab_from_fall_height_above_ground: 500.0,
            min_delta_height: 55.0,
            max_delta_height: 145.0,
            grab_mantle_delta_height: 140.0,
            grab_foot_delta_height: 30.0,
            min_depth: 10.0,
            min_clearance_to_hang: 10.0,
            max_delta_pitch: 45.0,
            max_delta_yaw: 45.0,
            max_railing_thickness: 8.0,
            input_time_buffer: 500,
        }
    }
}

/// Precached per-state body animation data (0x140d986c0): the "origin" joint at the last frame,
/// the "align" joint at frame 0, and the frame count. `camera` is the "camera" joint's model-space
/// position at every frame: the anims attach the player's spring camera to it at frame 0
/// (`ae_attachCamera "camera"`, tp_body.md6), and the grab moves the player with that camera.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgeAnim {
    pub origin_destination: Vec3,
    pub align_pos: Vec3,
    pub num_frames: u32,
    pub frame_rate: u32,
    pub camera: Vec<Vec3>,
}

impl LedgeAnim {
    /// The camera joint `seconds` into the animation (rate 1), linear between frames, held at the ends.
    pub fn camera_at(&self, seconds: f32) -> Vec3 {
        let Some(&last) = self.camera.last() else { return Vec3::ZERO };
        let f = (seconds * self.frame_rate as f32).max(0.0);
        let i = f.floor() as usize;
        if i + 1 >= self.camera.len() {
            return last;
        }
        self.camera[i].lerp(self.camera[i + 1], f - i as f32)
    }
}

/// idDeclPlayerProps::playerFalling_t (props +0x4f0): landing classification and fall damage distances.
/// Defaults are the props constructor's (0x1406eb800 → 0x1406eb8e2..0x1406eb93c); the player's props decl
/// (`playerprops/player/default`, `edit.playerFalling`) overrides fields it names.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerFalling {
    /// Falls shorter than this play no landing sound.
    pub min_dist_for_sound: f32,
    pub small_landing_reaction_distance: f32,
    pub medium_landing_reaction_distance: f32,
    pub large_landing_reaction_distance: f32,
    pub extra_large_landing_reaction_distance: f32,
    pub min_damage_distance: f32,
    pub max_damage_distance: f32,
    pub fatal_damage_distance: f32,
    /// How far below the player to look for ground (no ground → a pit).
    pub ground_test_distance: f32,
    /// With no ground below, the fall that kills.
    pub no_ground_kill_player_fall_dist: f32,
}

impl Default for PlayerFalling {
    fn default() -> Self {
        Self {
            min_dist_for_sound: 43.0,
            small_landing_reaction_distance: 43.0,
            medium_landing_reaction_distance: 200.0,
            large_landing_reaction_distance: 400.0,
            extra_large_landing_reaction_distance: 850.0,
            min_damage_distance: 768.0,
            max_damage_distance: 1024.0,
            fatal_damage_distance: 2048.0,
            ground_test_distance: 10000.0,
            no_ground_kill_player_fall_dist: 2048.0,
        }
    }
}

impl PlayerFalling {
    /// The class defaults with the decl block's fields applied (`f(key)` reads one field).
    pub fn with_overrides(f: impl Fn(&str) -> Option<f32>) -> Self {
        let d = Self::default();
        Self {
            min_dist_for_sound: f("minDistForSound").unwrap_or(d.min_dist_for_sound),
            small_landing_reaction_distance: f("smallLandingReactionDistance").unwrap_or(d.small_landing_reaction_distance),
            medium_landing_reaction_distance: f("mediumLandingReactionDistance").unwrap_or(d.medium_landing_reaction_distance),
            large_landing_reaction_distance: f("largeLandingReactionDistance").unwrap_or(d.large_landing_reaction_distance),
            extra_large_landing_reaction_distance: f("extraLargeLandingReactionDistance").unwrap_or(d.extra_large_landing_reaction_distance),
            min_damage_distance: f("minDamageDistance").unwrap_or(d.min_damage_distance),
            max_damage_distance: f("maxDamageDistance").unwrap_or(d.max_damage_distance),
            fatal_damage_distance: f("fatalDamageDistance").unwrap_or(d.fatal_damage_distance),
            ground_test_distance: f("groundTestDistance").unwrap_or(d.ground_test_distance),
            no_ground_kill_player_fall_dist: f("noGroundKillPlayerFallDist").unwrap_or(d.no_ground_kill_player_fall_dist),
        }
    }
}

/// Ledge grab tuning: the mechanic's own defaults (0x140da08a0), pmec_lg_* cvars, and animation data.
#[derive(Debug, Clone)]
pub struct LedgeGrabConfig {
    pub default_parms: LedgeTestParms,
    pub foot_parms: LedgeTestParms,
    pub in_water_parms: LedgeTestParms,
    pub railing_above_ledge_parms: LedgeTestParms,
    pub ray_test_depth: f32,
    pub forward_test_dist: f32,
    pub ledge_above_railing_additional_forward_test_dist: f32,
    pub railing_above_ledge_height: f32,
    pub railing_above_ledge_setback: f32,
    pub railing_above_ledge_thickness: f32,
    pub initiate_max_angle: f32,
    pub initiate_look_at_max_angle: f32,
    pub min_input_to_initiate: f32,
    pub face_max_degs_from_vertical: f32,
    pub surface_max_degs_from_horizontal: f32,
    pub no_grab_time_ms: i32,
    pub enable_foot_grab: bool,
    pub check_vertical_velocity: bool,
    pub use_any_valid_pos: bool,
    pub enable_railing_above_ledge_grab: bool,
    pub assume_always_forward_press: bool,
    pub require_look_at_ledge: bool,
    pub deferred_traces: bool,
    pub foot_grab_align_dist: f32,
    pub foot_grab_align_height_offset: f32,
    pub foot_grab_align_speed: f32,
    /// View constraint while an animated grab plays (0x140da2350 → 0x140d902a0).
    pub max_delta_pitch_up: f32,
    pub max_delta_pitch_down: f32,
    pub max_delta_yaw: f32,
    pub constrained_view_angles_rate: f32,
    /// Spring-camera blend in and out: the mechanic's viewBlendDurationMS (100, 0x140da08a0; the player
    /// decl does not set it) and the attach event's alignedEnt_defaultCameraBlendDurationMS, both
    /// overridden by springCam_ForceBlendDurationMS ≥ 0 (0x140a334a0).
    pub view_blend_duration_ms: i32,
    /// Indexed by idPlayerMechanicLedgeGrabState_t (0..16).
    pub anims: [LedgeAnim; 16],
}

impl LedgeGrabConfig {
    pub fn from_cvars(cv: &CvarValues, anims: [LedgeAnim; 16]) -> Self {
        let default_parms = LedgeTestParms::defaults(cv.f("pm_jumpheight"));
        let foot_parms = LedgeTestParms { forward_up_test_dist3: 0.0, up_test_dist: 35.0, min_height: 40.0, min_delta_height: 0.0, ..default_parms };
        let in_water_parms = LedgeTestParms { forward_up_test_dist: 90.0, up_test_dist: 35.0, grab_mantle_delta_height: 120.0, ..default_parms };
        let (railing_height, railing_setback, railing_thickness) = (52.0, 4.0, 2.0);
        let railing_up = railing_height - railing_thickness * 0.5;
        let railing_above_ledge_parms = LedgeTestParms {
            forward_up_test_dist: railing_up,
            forward_up_test_dist2: railing_up + 1.0,
            forward_up_test_dist3: railing_up + 1.0,
            min_height: 130.0,
            grab_mantle_delta_height: 120.0,
            ..default_parms
        };
        Self {
            default_parms,
            foot_parms,
            in_water_parms,
            railing_above_ledge_parms,
            ray_test_depth: 1.0,
            forward_test_dist: 150.0,
            ledge_above_railing_additional_forward_test_dist: 20.0,
            railing_above_ledge_height: railing_height,
            railing_above_ledge_setback: railing_setback,
            railing_above_ledge_thickness: railing_thickness,
            initiate_max_angle: cv.f("pmec_lg_InitiateMaxAngle"),
            initiate_look_at_max_angle: cv.f("pmec_lg_InitiateLookATMaxAngle"),
            min_input_to_initiate: cv.f("pmec_lg_MinInputToInitiate"),
            face_max_degs_from_vertical: cv.f("pmec_lg_LedgeFaceMaxDegsFromVertical"),
            surface_max_degs_from_horizontal: cv.f("pmec_lg_LedgeSurfaceMaxDegsFromHorizontal"),
            no_grab_time_ms: cv.i("pmec_lg_NoGrabTimeMS"),
            enable_foot_grab: cv.b("pmec_lg_EnableFootGrab"),
            check_vertical_velocity: cv.b("pmec_lg_CheckVerticalVelocity"),
            use_any_valid_pos: cv.b("pmec_lg_UseAnyValidPos"),
            enable_railing_above_ledge_grab: cv.b("pmec_lg_EnableRailingAboveLedgeGrab"),
            assume_always_forward_press: cv.b("pmec_lg_AssumeAlwaysForwardPress"),
            require_look_at_ledge: cv.b("pmec_lg_RequireLookAtLedge"),
            deferred_traces: cv.i("pmec_lg_enableDeferredTraces") == 1,
            foot_grab_align_dist: cv.f("pmec_lg_FootGrabAlignDist"),
            foot_grab_align_height_offset: cv.f("pmec_lg_FootGrabAlignHeightOffset"),
            foot_grab_align_speed: cv.f("pmec_lg_FootGrabAlignSpeed"),
            max_delta_pitch_up: cv.f("pmec_lg_MaxDeltaPitchUp"),
            max_delta_pitch_down: cv.f("pmec_lg_MaxDeltaPitchDown"),
            max_delta_yaw: cv.f("pmec_lg_MaxDeltaYaw"),
            constrained_view_angles_rate: cv.f("pmec_lg_ConstrainedViewAnglesRate"),
            view_blend_duration_ms: match cv.i("springCam_ForceBlendDurationMS") {
                f if f >= 0 => f,
                _ => cv.i("alignedEnt_defaultCameraBlendDurationMS"),
            },
            anims,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MoveConfig {
    pub run_speed: f32,
    pub walk_speed: f32,
    pub crouch_speed: f32,
    pub noclip_speed: f32,
    pub walk_threshold: i32,
    pub back_speed_ratio: f32,
    pub strafe_speed_ratio: f32,
    pub max_scaled_run_speed: f32,
    pub jump_height: f32,
    pub step_size: f32,
    pub gravity: f32,
    pub normal_height: f32,
    pub crouch_height: f32,
    pub bbox_width: f32,
    pub normal_view_height: f32,
    pub crouch_view_height: f32,
    pub crouch_toggle: bool,
    pub walk_move_stick_to_ground: bool,
    pub walk_move_stick_to_ground_dot: f32,
    pub double_jump_friction: f32,
    pub zero_g_friction: f32,
    pub enable_jump_extension: bool,
    pub jump_extension_height: f32,
    pub jump_extension_up_height: f32,
    pub jump_extension_length: f32,
    pub jump_change_direction_scalar: f32,
    pub lunge_horizontal_direction_scalar: f32,
    pub double_jump_input_influence: f32,
    pub double_jump_cooldown: f32,
    pub allow_infinite_double_jumps: bool,
    pub always_allow_double_jump: bool,
    /// From the jump-boots decl (SP equips `jumpboots/base`).
    pub boots: Option<DoubleJump>,
    pub fov: f32,
    pub m_yaw: f32,
    pub m_pitch: f32,
    pub m_sensitivity: f32,
    pub min_view_pitch: f32,
    pub max_view_pitch: f32,
    /// View/hands springs that absorb crouch and step height changes (player 0x140e36ea0).
    pub use_step_up_springs: bool,
    pub doom4_bob_cycle: bool,
    /// handsBobCycle_Enable: also gates adding the view spring to the eye (0x140e3beb0).
    pub hands_bob_cycle: bool,
    pub step_up_view_spring_k: f32,
    pub step_up_hands_spring_k: f32,
    /// Frame clock (com_fixedTic / com_adaptiveTick*, 0x1415a2500 → 0x1415a3890).
    pub fixed_tic: bool,
    pub adaptive_tick: bool,
    pub adaptive_tick_immediate: bool,
    pub adaptive_tick_min_hz: i32,
    pub adaptive_tick_max_hz: i32,
    /// Player trace model shape (physics SetClipModel 0x1416b2aa0): 0 box, 1 eight-sided cylinder, 2 pencil.
    pub collision_style: i32,
    pub pencil_collision_angle: f32,
    pub pencil_collision_taper_radius: f32,
    pub ledge: LedgeGrabConfig,
    /// The player props decl's landing/fall distances (install.rs fills it from the decl).
    pub falling: PlayerFalling,
    pub pusher_walk_move_cushion: f32,
    pub pusher_dislodge_multiplier: f32,
}

impl MoveConfig {
    pub fn from_cvars(cv: &CvarValues, boots: Option<DoubleJump>, ledge_anims: [LedgeAnim; 16]) -> Self {
        Self {
            run_speed: cv.f("pm_runspeed"),
            walk_speed: cv.f("pm_walkspeed"),
            crouch_speed: cv.f("pm_crouchspeed"),
            noclip_speed: cv.f("pm_noclipspeed"),
            walk_threshold: cv.i("pm_walkthreshold"),
            back_speed_ratio: cv.f("pm_backSpeedRatio"),
            strafe_speed_ratio: cv.f("pm_strafeSpeedRatio"),
            max_scaled_run_speed: cv.f("pm_maxScaledRunSpeed"),
            jump_height: cv.f("pm_jumpheight"),
            step_size: cv.f("pm_stepsize"),
            gravity: cv.f("g_gravity"),
            normal_height: cv.f("pm_normalheight"),
            crouch_height: cv.f("pm_crouchheight"),
            bbox_width: cv.f("pm_bboxwidth"),
            normal_view_height: cv.f("pm_normalViewHeight"),
            crouch_view_height: cv.f("pm_crouchviewheight"),
            crouch_toggle: cv.b("pm_crouchToggle"),
            walk_move_stick_to_ground: cv.b("pm_walkMoveStickToGround"),
            walk_move_stick_to_ground_dot: cv.f("pm_walkMoveStickToGroundDot"),
            double_jump_friction: cv.f("pm_doubleJumpFriction"),
            zero_g_friction: cv.f("pm_zeroG_Friction"),
            enable_jump_extension: cv.b("pm_enableJumpExtension"),
            jump_extension_height: cv.f("pm_jumpExtensionHeight"),
            jump_extension_up_height: cv.f("pm_jumpExtensionUpHeight"),
            jump_extension_length: cv.f("pm_jumpExtensionLength"),
            jump_change_direction_scalar: cv.f("pm_jumpChangeDirectionScalar"),
            lunge_horizontal_direction_scalar: cv.f("pm_lungeHorizontalDirectionScalar"),
            double_jump_input_influence: cv.f("pm_doubleJumpInputInfluence"),
            double_jump_cooldown: cv.f("pm_doubleJumpCooldown"),
            allow_infinite_double_jumps: cv.b("pm_allowInfiniteDoubleJumps"),
            always_allow_double_jump: cv.b("pm_alwaysAllowDoubleJump"),
            boots,
            fov: cv.f("g_fov"),
            m_yaw: cv.f("m_yaw"),
            m_pitch: cv.f("m_pitch"),
            m_sensitivity: cv.f("m_sensitivity"),
            min_view_pitch: cv.f("pm_minviewpitch"),
            max_view_pitch: cv.f("pm_maxviewpitch"),
            use_step_up_springs: cv.b("p_useStepUpSprings"),
            doom4_bob_cycle: cv.b("pm_doom4BobCycle"),
            hands_bob_cycle: cv.b("handsBobCycle_Enable"),
            step_up_view_spring_k: cv.f("p_stepUpViewSpringK"),
            step_up_hands_spring_k: cv.f("p_stepUpHandsSpringK"),
            fixed_tic: cv.b("com_fixedTic"),
            adaptive_tick: cv.b("com_adaptiveTick"),
            adaptive_tick_immediate: cv.b("com_adaptiveTickImmediateMode"),
            adaptive_tick_min_hz: cv.i("com_adaptiveTickMinHz"),
            adaptive_tick_max_hz: cv.i("com_adaptiveTickMaxHz"),
            collision_style: cv.i("pm_playerCollisionStyle"),
            pencil_collision_angle: cv.f("pm_pencilCollisionAngle"),
            pencil_collision_taper_radius: cv.f("pm_pencilCollisionTaperRadius"),
            ledge: LedgeGrabConfig::from_cvars(cv, ledge_anims),
            falling: PlayerFalling::default(),
            pusher_walk_move_cushion: cv.f("pm_pusherWalkMoveCushion"),
            pusher_dislodge_multiplier: cv.f("pm_pusherDislodgeMultiplier"),
        }
    }
}
