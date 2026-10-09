//! The player-side half of movement: what the game's player code feeds into physics every frame
//! (speeds, step/jump heights, double-jump parameters) and the view built from the result.

use glam::Vec3;

use crate::cmd::{UserCmd, button};
use crate::collision::{Hull, Mat3, TraceFilter, World};
use crate::config::{LedgeGrabConfig, LedgeTestParms, MoveConfig};
use crate::physics::{DoubleJumpState, PhysicsCvars, PlayerPhysics, flags};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedState {
    Walk,
    Run,
}

/// The game's damped spring (constructor at 0x140db39c6, SetK 0x1409430e0, Update 0x1409442f0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub target: f32,
    pub pos: f32,
    pub vel: f32,
    /// Speed cap; 0 = none.
    pub max_speed: f32,
    pub k: f32,
    pub damping: f32,
    pub mass: f32,
    /// Distance from the target the spring rests at.
    pub rest: f32,
}

impl Default for Spring {
    fn default() -> Self {
        Self { target: 0.0, pos: 0.0, vel: 0.0, max_speed: 0.0, k: 1.0, damping: 2.0, mass: 1.0, rest: 0.0 }
    }
}

impl Spring {
    /// Stiffness capped at 10000; a negative damping selects critical damping 2*sqrt(k*mass).
    pub fn set_k(&mut self, k: f32, damping: f32) {
        self.k = k.min(10000.0);
        self.damping = if damping >= 0.0 { damping } else { 2.0 * (self.k * self.mass).sqrt() };
    }

    /// Semi-implicit Euler in sub-steps of at most 8.5 ms.
    pub fn update(&mut self, mut dt: f32) {
        while dt > 0.0 {
            let h = dt.min(0.0085);
            dt = if dt <= 0.0085 { 0.0 } else { dt - 0.0085 };
            let d = self.pos - self.target;
            let side = if d < 0.0 { -1.0 } else { 1.0 };
            self.vel += ((-((d.abs() - self.rest) * self.k * side) - self.damping * self.vel) / self.mass) * h;
            let dir = if self.vel < 0.0 { -1.0 } else { 1.0 };
            let mut speed = self.vel.abs();
            if self.max_speed > 0.0 && speed > self.max_speed {
                speed = self.max_speed;
            }
            self.vel = speed * dir;
            if self.vel.abs() <= f32::MIN_POSITIVE {
                self.vel = 0.0;
            }
            self.pos += h * self.vel;
            if self.pos.abs() <= f32::MIN_POSITIVE {
                self.pos = 0.0;
            }
        }
    }
}

pub struct Player {
    pub cfg: MoveConfig,
    pub physics: PlayerPhysics,
    pub has_jump_boots: bool,
    pub speed_state: SpeedState,
    /// Accumulated mouse look, degrees.
    pub view_angles: [f32; 3],
    pub time_ms: i32,
    /// Eye and hands offsets that absorb crouch and step height changes (idPlayer +0x14b7c / +0x14bac).
    pub view_spring: Spring,
    pub hands_spring: Spring,
    /// Crouch state the springs last saw (+0x14b78).
    spring_ducked: bool,
    pub ledge: LedgeGrab,
    /// The spring camera an animated ledge grab attaches to the body's camera joint.
    pub grab_camera: GrabCamera,
    /// View-angle limits a mechanic placed on the player controller (+0x32a8..0x32c4), and the angles
    /// it last let through.
    pub view_constraint: Option<ViewConstraint>,
    constrained_angles: [f32; 3],
    /// What happened this frame (reset at the start of every think).
    pub events: PlayerEvents,
    land: LandTracker,
}

impl Player {
    pub fn new(cfg: MoveConfig, origin: Vec3) -> Self {
        let tun = PhysicsCvars {
            crouch_toggle: cfg.crouch_toggle,
            stick_to_ground: cfg.walk_move_stick_to_ground,
            stick_to_ground_dot: cfg.walk_move_stick_to_ground_dot,
            double_jump_friction: cfg.double_jump_friction,
            zero_g_friction: cfg.zero_g_friction,
            enable_jump_extension: cfg.enable_jump_extension,
            jump_extension_height: cfg.jump_extension_height,
            jump_extension_up_height: cfg.jump_extension_up_height,
            jump_extension_length: cfg.jump_extension_length,
            jump_change_direction_scalar: cfg.jump_change_direction_scalar,
            lunge_horizontal_direction_scalar: cfg.lunge_horizontal_direction_scalar,
            double_jump_input_influence: cfg.double_jump_input_influence,
            double_jump_cooldown: cfg.double_jump_cooldown,
            allow_infinite_double_jumps: cfg.allow_infinite_double_jumps,
            pusher_walk_move_cushion: cfg.pusher_walk_move_cushion,
            pusher_dislodge_multiplier: cfg.pusher_dislodge_multiplier,
        };
        let shape = |height: f32| {
            let half = cfg.bbox_width * 0.5;
            Hull::player_trace_model(
                cfg.collision_style,
                Vec3::new(-half, -half, 0.0),
                Vec3::new(half, half, height),
                cfg.pencil_collision_angle,
                cfg.pencil_collision_taper_radius,
            )
        };
        let physics = PlayerPhysics::new(origin, shape(cfg.normal_height), shape(cfg.crouch_height), cfg.gravity, tun);
        let mut view_spring = Spring::default();
        let mut hands_spring = Spring::default();
        view_spring.set_k(cfg.step_up_view_spring_k, -1.0);
        hands_spring.set_k(cfg.step_up_hands_spring_k, -1.0);
        Self {
            cfg,
            physics,
            has_jump_boots: true,
            speed_state: SpeedState::Run,
            view_angles: [0.0; 3],
            time_ms: 0,
            view_spring,
            hands_spring,
            spring_ducked: false,
            ledge: LedgeGrab::default(),
            grab_camera: GrabCamera::default(),
            view_constraint: None,
            constrained_angles: [0.0; 3],
            events: PlayerEvents::default(),
            land: LandTracker { start_origin: origin, apex: origin, ..LandTracker::default() },
        }
    }

    /// Mouse deltas in counts, applied the idTech way: degrees = counts * m_yaw * m_sensitivity.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.view_angles[1] -= dx * self.cfg.m_yaw * self.cfg.m_sensitivity;
        self.view_angles[0] += dy * self.cfg.m_pitch * self.cfg.m_sensitivity;
        self.view_angles[0] = self.view_angles[0].clamp(self.cfg.min_view_pitch, self.cfg.max_view_pitch);
        self.view_angles[1] = self.view_angles[1].rem_euclid(360.0);
    }

    /// Per-frame speed selection (the player's speed update), then SetSpeed(walk, crouch).
    fn update_speed(&mut self, cmd: &UserCmd) {
        let c = &self.cfg;
        let fwd = cmd.forward as f32;
        let right = cmd.right as f32;
        let mag = (fwd * fwd + right * right).sqrt();
        self.speed_state = if mag <= c.walk_threshold as f32 || cmd.has(button::WALK) { SpeedState::Walk } else { SpeedState::Run };
        let frac = (mag / c.walk_threshold as f32).clamp(0.0, 1.0);
        let lerped = (1.0 - frac) * c.walk_speed + frac * c.run_speed;
        let mut speed = if self.speed_state == SpeedState::Walk { c.walk_speed } else { lerped };
        let back = -fwd * 0.007874016;
        if back > 0.0 {
            speed *= 1.0 - (1.0 - c.back_speed_ratio) * back;
        }
        if c.strafe_speed_ratio < 1.0 && right.abs() > 0.0 {
            speed *= 1.0 - (1.0 - c.strafe_speed_ratio) * ((right / (fwd.abs() + right.abs())) * (right / 127.0)).abs();
        }
        let walk = if c.max_scaled_run_speed > 0.0 { speed.min(c.max_scaled_run_speed) } else { speed };
        let crouch = walk.min(c.crouch_speed);
        self.physics.set_speed(walk, crouch);
    }

    fn push_physics_params(&mut self) {
        self.physics.max_step_height = self.cfg.step_size;
        self.physics.max_jump_height = self.cfg.jump_height;
        let dj = match (self.has_jump_boots, self.cfg.boots) {
            (true, Some(b)) => Some(b),
            _ => None,
        };
        let gravity = Vec3::new(0.0, 0.0, -self.cfg.gravity);
        self.physics.set_double_jump(match dj {
            Some(b) if self.has_jump_boots || self.cfg.always_allow_double_jump => DoubleJumpState {
                height: b.height,
                speed_scale: b.speed_scale,
                air_english_scale: b.air_english_scale,
                window_offset_ms: b.window_offset_ms,
                window_duration_ms: b.window_duration_ms,
                gravity,
            },
            _ => DoubleJumpState {
                height: 0.0,
                speed_scale: 0.0,
                air_english_scale: 0.0,
                window_offset_ms: 0,
                window_duration_ms: 0,
                gravity: Vec3::ZERO,
            },
        });
    }

    pub fn think(&mut self, world: &World, mut cmd: UserCmd, msec: i32) {
        self.time_ms += msec;
        self.events = PlayerEvents::default();
        self.land.frames += 1;
        // The controller limits the angles before they go into the usercmd.
        if let Some(c) = self.view_constraint {
            c.apply(self.constrained_angles, &mut self.view_angles, msec as f32 * 0.001);
        }
        self.constrained_angles = self.view_angles;
        cmd.angles = self.view_angles;
        self.update_speed(&cmd);
        self.push_physics_params();
        // idPlayer::Think keeps the pre-move origin and velocity (minus push velocity) for the landing
        // test (0x140e30259 → +0x14c50 / +0x14c5c).
        self.land.saved_origin = self.physics.origin;
        self.land.saved_velocity = self.physics.velocity;
        // Physics runs during grabs too; the grab overrides the resulting velocity (event 0x11).
        self.physics.move_player(world, cmd, msec, self.time_ms);
        // idPlayer::Think: Move (with the mechanics' event 0x11 and the landing test), then the
        // mechanics, then the view.
        self.ledge_after_move();
        self.update_landing(world);
        // The physics jump callback (0x140e2fbe0, physics +0x2188) and Think's jump block (0x140e31674)
        // both key off the per-frame JUMPED / DOUBLE_JUMPED movement flags.
        self.events.jumped = self.physics.flags & flags::JUMPED != 0;
        self.events.double_jumped = self.physics.flags & flags::DOUBLE_JUMPED != 0;
        self.ledge_handle(world, &cmd);
        self.update_view_springs(msec);
        // The third-person body and the spring camera think after the player.
        let eye = self.physics.origin + Vec3::Z * self.eye_height();
        let anims = &self.cfg.ledge.anims;
        self.grab_camera.update(self.time_ms, eye, |state, t| anims.get(state as usize).map(|a| a.camera_at(t)));
    }

    /// Where the first-person view is rendered from: the spring camera while a grab drives it, else
    /// the eye.
    pub fn view_origin(&self) -> Vec3 {
        if self.grab_camera.mode == CameraMode::Off {
            self.physics.origin + Vec3::Z * self.eye_height()
        } else {
            self.grab_camera.pos
        }
    }

    /// 0x140e36ea0, run each frame before the view is built: crouching or standing moves the springs by
    /// the view-height difference and a step by the step height; then both relax toward zero.
    fn update_view_springs(&mut self, msec: i32) {
        let ducked = self.physics.ducked();
        let (normal, crouch) = (self.cfg.normal_view_height, self.cfg.crouch_view_height);
        if !self.spring_ducked && ducked {
            self.view_spring.pos += normal - crouch;
            self.hands_spring.pos += normal - crouch;
            self.spring_ducked = true;
        } else if self.spring_ducked && !ducked {
            self.view_spring.pos += crouch - normal;
            self.hands_spring.pos += crouch - normal;
            self.spring_ducked = false;
        }
        if self.physics.flags & (flags::STEPPED_UP | flags::STEPPED_DOWN) != 0 {
            self.view_spring.pos -= self.physics.step_accum;
            self.hands_spring.pos -= self.physics.step_accum;
        }
        // The hands use the view constant while player flag 0xce46 bit 1 is set (not modelled: clear).
        if self.hands_spring.k != self.cfg.step_up_hands_spring_k {
            self.hands_spring.set_k(self.cfg.step_up_hands_spring_k, -1.0);
        }
        if self.view_spring.k != self.cfg.step_up_view_spring_k {
            self.view_spring.set_k(self.cfg.step_up_view_spring_k, -1.0);
        }
        let dt = msec as f32 * 0.001;
        self.view_spring.update(dt);
        self.hands_spring.update(dt);
    }

    /// Eye height above the origin (GetEyeOffset 0x140e3d290), plus the view spring when
    /// p_useStepUpSprings, pm_doom4BobCycle and handsBobCycle_Enable are all on (0x140e3beb0).
    /// Not included: the animation-driven view Z offset (player +0x27710, copied from +0x16fa0).
    pub fn eye_height(&self) -> f32 {
        let base = if self.physics.ducked() { self.cfg.crouch_view_height } else { self.cfg.normal_view_height };
        if self.cfg.use_step_up_springs && self.cfg.doom4_bob_cycle && self.cfg.hands_bob_cycle {
            base + self.view_spring.pos
        } else {
            base
        }
    }

    /// Vertical offset for the first-person hands (the hands spring, +0x14bb0).
    pub fn hands_offset(&self) -> f32 {
        self.hands_spring.pos
    }
}

// ---- Ledge grab (idPlayerMechanicLedgeGrab, vtbl 0x1422b1a58) ----------------------------------

/// idPlayerMechanicLedgeGrabState_t.
pub mod ledge_state {
    pub const NONE: i32 = -1;
    pub const PULL_UP: i32 = 0;
    pub const PULL_UP_MANTLE: i32 = 1;
    pub const PULL_UP_FOOT: i32 = 2;
    pub const PULL_UP_ROUNDED: i32 = 3;
    pub const PULL_UP_MANTLE_ROUNDED: i32 = 4;
    pub const PULL_UP_ANGLED: i32 = 5;
    pub const PULL_UP_MANTLE_ANGLED: i32 = 6;
    pub const CLIMB_UP: i32 = 7;
    pub const CLIMB_UP_MANTLE: i32 = 8;
    pub const CLIMB_UP_FOOT: i32 = 9;
    pub const RAILING_PULL_UP: i32 = 10;
    pub const RAILING_PULL_UP_MANTLE: i32 = 11;
    pub const RAILING_PULL_UP_FOOT: i32 = 12;
    pub const CUSTOM_PULL_UP: i32 = 13;

    /// Foot grabs move the player directly instead of playing a body animation (mask 0x1204).
    pub fn is_foot(s: i32) -> bool {
        (0..13).contains(&s) && (0x1204 >> s) & 1 != 0
    }
}

/// idPlayerMechanicLedgeInfo (reflection 0x1431a4a80), one per test direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgeInfo {
    pub blocked_forward: bool,
    pub valid_face: bool,
    pub valid_surface: bool,
    pub block_dist_forward: f32,
    pub clear_dist_up_forward: f32,
    pub ledge_forward: bool,
    pub ledge_delta_height: f32,
    pub ledge_height_above_ground: f32,
    pub ledge_depth: f32,
    pub ledge_thickness: f32,
    pub ledge_face_normal: Vec3,
    pub ledge_surface_normal: Vec3,
    pub can_stand: bool,
    pub ledge_pos: Vec3,
    /// 0 square, 1 rounded, 2 angled (the forward face's surface flags; brushes have none).
    pub edge_type: i32,
}

impl LedgeInfo {
    /// The clear state FUN_140d98d10 starts from (1e30 = 0x7149f2ca).
    fn cleared() -> Self {
        Self {
            blocked_forward: false,
            valid_face: false,
            valid_surface: false,
            block_dist_forward: 1e30,
            clear_dist_up_forward: 0.0,
            ledge_forward: false,
            ledge_delta_height: 1e30,
            ledge_height_above_ground: 1e30,
            ledge_depth: 0.0,
            ledge_thickness: 0.0,
            ledge_face_normal: Vec3::ZERO,
            ledge_surface_normal: Vec3::ZERO,
            can_stand: false,
            ledge_pos: Vec3::ZERO,
            edge_type: 0,
        }
    }
}

/// A surroundings query, issued one frame and evaluated the next (pmec_lg_enableDeferredTraces 1).
#[derive(Debug, Clone, Copy)]
struct LedgeQuery {
    pos: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    parms: LedgeTestParms,
    forward_test_dist: f32,
    railing: bool,
}

/// The mechanic's state plus its check-surroundings job data (DAT_1444b9ec8).
#[derive(Debug, Clone)]
pub struct LedgeGrab {
    pub state: i32,
    pub prev_state: i32,
    pub can_grab_time: i32,
    pub grab_start_time: i32,
    pub grab_end_time: i32,
    pub last_grab_was_foot_grab: bool,
    pub has_actual_press: bool,
    pub has_buffered_press: bool,
    pub valid_forward_press_time: i32,
    pub foot_grab_start_vel: Vec3,
    pub body_align_pos: Vec3,
    pub world_origin_destination: Vec3,
    /// Center, right, left.
    pub info: [LedgeInfo; 3],
    pub railing_info: [LedgeInfo; 3],
    pub found_ledge: bool,
    pub is_railing: bool,
    pub can_grab_and_pull_up: bool,
    pub can_grab_and_climb_up: bool,
    pub valid_ledge_pos: i32,
    /// Where the player was when an animated grab started (for the view; not used by the mechanic).
    pub start_origin: Vec3,
    parms_are_railing: bool,
    pending: Option<LedgeQuery>,
    pending_railing: Option<LedgeQuery>,
    last_found_frame: Option<u64>,
    frame: u64,
}

impl Default for LedgeGrab {
    fn default() -> Self {
        Self {
            state: ledge_state::NONE,
            prev_state: ledge_state::NONE,
            can_grab_time: 0,
            grab_start_time: 0,
            grab_end_time: 0,
            last_grab_was_foot_grab: false,
            has_actual_press: false,
            has_buffered_press: false,
            valid_forward_press_time: 0,
            foot_grab_start_vel: Vec3::ZERO,
            body_align_pos: Vec3::ZERO,
            world_origin_destination: Vec3::ZERO,
            info: [LedgeInfo::cleared(); 3],
            railing_info: [LedgeInfo::cleared(); 3],
            found_ledge: false,
            is_railing: false,
            can_grab_and_pull_up: false,
            can_grab_and_climb_up: false,
            valid_ledge_pos: 0,
            start_origin: Vec3::ZERO,
            parms_are_railing: false,
            pending: None,
            pending_railing: None,
            last_found_frame: None,
            frame: 0,
        }
    }
}

/// Ledge traces sweep idClip's ±0.01 default box (FUN_140d8eac0 → 0x141638b60). Returns
/// (hit, distance along the segment as FUN_140d8ee60 reports it, trace).
fn ledge_ray(world: &World, start: Vec3, end: Vec3) -> (bool, f32, crate::collision::Trace) {
    thread_local! {
        static BOX: Hull = Hull::trace_box();
    }
    let tr = BOX.with(|b| world.translate(b, start, end));
    let len = (end - start).length();
    let hit = tr.fraction < 1.0;
    (hit, if hit { tr.fraction * len } else { len }, tr)
}

impl LedgeGrab {
    fn selected(&self) -> &LedgeInfo {
        match self.valid_ledge_pos {
            1 => &self.info[1],
            2 => &self.info[2],
            _ => &self.info[0],
        }
    }

    /// FUN_140d9fa20: the custom grab parms aside, water uses inWaterParms, otherwise footParms when
    /// foot grabs are enabled.
    fn choose_parms(cfg: &LedgeGrabConfig, water_level: f32) -> LedgeTestParms {
        if water_level > 0.5 {
            cfg.in_water_parms
        } else if cfg.enable_foot_grab {
            cfg.foot_parms
        } else {
            cfg.default_parms
        }
    }

    /// One test direction (FUN_140d98d10; the deferred FUN_140d99ca0 runs the same traces).
    #[allow(clippy::too_many_arguments)]
    fn test_direction(
        world: &World,
        cfg: &LedgeGrabConfig,
        stand: &Hull,
        bbox_width: f32,
        pos: Vec3,
        fwd: Vec3,
        up: Vec3,
        forward_test_dist: f32,
        backup: f32,
        fwd_up: f32,
        up_test: f32,
        depth_test: f32,
        railing: bool,
    ) -> LedgeInfo {
        let mut info = LedgeInfo::cleared();
        let back = fwd * backup;
        let a0 = pos - back + up * fwd_up;
        let a1 = a0 + fwd * (forward_test_dist + backup);
        if !railing && ledge_ray(world, pos - back, a0).0 {
            return info;
        }
        let (hit, dist, face) = ledge_ray(world, a0, a1);
        info.block_dist_forward = dist;
        if hit {
            info.blocked_forward = true;
            // Edge type from the face's surface flags (0x140d98d10): LEDGE_GRAB_ROUNDED (2) → 1,
            // LEDGE_GRAB_ANGLED (4) → 2.
            let sf = face.c.surface_flags;
            info.edge_type = if sf & 2 != 0 { 1 } else { ((sf >> 1) & 2) as i32 };
            info.ledge_face_normal = face.c.normal;
            let nxy = Vec3::new(face.c.normal.x, face.c.normal.y, 0.0).normalize_or_zero();
            if railing {
                info.ledge_face_normal = nxy;
            }
            info.valid_face = cfg.face_max_degs_from_vertical.to_radians().cos() <= info.ledge_face_normal.dot(nxy);
        }
        info.block_dist_forward -= backup;
        if !info.valid_face {
            return info;
        }
        let rise = up * up_test;
        let b0 = a0 + rise;
        let b1 = a1 + rise;
        let c0 = a0 + fwd * (dist - cfg.ray_test_depth);
        if ledge_ray(world, a0, b0).0 || ledge_ray(world, c0, c0 + rise).0 {
            return info;
        }
        let top = ledge_ray(world, b0, b1).1 - backup;
        info.clear_dist_up_forward = top - info.block_dist_forward;
        if info.clear_dist_up_forward <= 0.0 {
            return info;
        }
        let d0 = b0 + fwd * (info.block_dist_forward + backup + cfg.ray_test_depth);
        let (hit, _, surf) = ledge_ray(world, d0, d0 - rise);
        if !hit {
            return info;
        }
        let surface_normal = if railing { Vec3::Z } else { surf.c.normal };
        info.ledge_surface_normal = surface_normal;
        info.valid_surface = cfg.surface_max_degs_from_horizontal.to_radians().cos() <= surface_normal.z;
        if !info.valid_surface {
            return info;
        }
        info.ledge_forward = true;
        // The face hit point carried up the face's vertical onto the surface plane.
        let v = (Vec3::Z - info.ledge_face_normal * Vec3::Z.dot(info.ledge_face_normal)).normalize_or_zero();
        let s = (surf.c.point - face.c.point).dot(surface_normal) / v.dot(surface_normal);
        info.ledge_pos = face.c.point + v * s;
        info.ledge_delta_height = info.ledge_pos.z - pos.z;
        let g0 = pos + up * up_test;
        let (hit, _, ground) = ledge_ray(world, g0, g0 - up * 200.0);
        info.ledge_height_above_ground = if hit { info.ledge_pos.z - ground.c.point.z } else { 200.0 };
        // Depth along the surface, then the back face for the thickness.
        let fwd_s = (fwd - surface_normal * fwd.dot(surface_normal)).normalize_or_zero();
        let e0 = pos + fwd_s * info.block_dist_forward + up * (fwd_up + up_test);
        let depth = ledge_ray(world, e0, e0 + fwd_s * depth_test).1;
        info.ledge_depth = depth;
        let f0 = info.ledge_pos - up * cfg.ray_test_depth;
        let g = f0 + fwd_s * (depth - cfg.ray_test_depth);
        let h0 = e0 + fwd_s * (depth - cfg.ray_test_depth);
        info.ledge_thickness = depth_test;
        if !ledge_ray(world, h0, g).0 {
            info.ledge_thickness = depth - ledge_ray(world, g, f0).1;
        }
        let nz = surface_normal.z.clamp(-1.0, 1.0);
        let stand_pos = info.ledge_pos + Vec3::Z * ((1.0 - nz * nz).sqrt() * bbox_width * 0.5 + 1.0);
        info.can_stand = world.position_clear(stand, stand_pos);
        info
    }

    /// CheckSurroundings for one query (FUN_140d9b4d0 / deferred FUN_140d9bb90): the left–right span
    /// must be clear, the center tried at up to three heights, then both sides at the height that worked.
    fn evaluate(&self, world: &World, cfg: &LedgeGrabConfig, stand: &Hull, bbox_width: f32, q: &LedgeQuery) -> Option<([LedgeInfo; 3], i32)> {
        let p = &q.parms;
        let right_pos = q.pos + q.right * p.side_test_dist;
        let left_pos = q.pos - q.right * p.side_test_dist;
        if ledge_ray(world, left_pos, right_pos).0 {
            return None;
        }
        let test = |at: Vec3, fwd_up: f32, up_test: f32| {
            Self::test_direction(world, cfg, stand, bbox_width, at, q.forward, q.up, q.forward_test_dist, p.forward_test_backup_dist, fwd_up, up_test, p.depth_test_dist, q.railing)
        };
        let mut heights = vec![(p.forward_up_test_dist, p.up_test_dist)];
        if p.forward_up_test_dist > p.forward_up_test_dist2 {
            heights.push((p.forward_up_test_dist2, p.forward_up_test_dist - p.forward_up_test_dist2));
            if p.forward_up_test_dist2 > p.forward_up_test_dist3 {
                heights.push((p.forward_up_test_dist3, p.forward_up_test_dist2 - p.forward_up_test_dist3));
            }
        }
        let (center, fwd_up, up_test) = heights.into_iter().find_map(|(fu, ut)| {
            let c = test(q.pos, fu, ut);
            c.ledge_forward.then_some((c, fu, ut))
        })?;
        let right = test(right_pos, fwd_up, up_test);
        if !right.ledge_forward && !cfg.use_any_valid_pos {
            return None;
        }
        let left = test(left_pos, fwd_up, up_test);
        if !left.ledge_forward && !cfg.use_any_valid_pos {
            return None;
        }
        let valid = if !cfg.use_any_valid_pos || center.can_stand {
            0
        } else if right.can_stand {
            1
        } else if left.can_stand {
            2
        } else {
            0
        };
        Some(([center, right, left], valid))
    }

    fn stands(&self, infos: &[LedgeInfo; 3], any: bool) -> bool {
        if any { infos.iter().any(|i| i.can_stand) } else { infos.iter().all(|i| i.can_stand) }
    }

    /// FUN_140d98930.
    fn check_valid(&self, cfg: &LedgeGrabConfig, p: &LedgeTestParms, velocity: Vec3, gravity: Vec3) -> bool {
        if !self.found_ledge {
            return false;
        }
        let sel = self.selected();
        let (dh, hag, dist) = (sel.ledge_delta_height, sel.ledge_height_above_ground, sel.block_dist_forward);
        let depth = self.info[0].ledge_depth.max(self.info[1].ledge_depth).max(self.info[2].ledge_depth);
        if hag < p.min_height || dh < p.min_delta_height || dh > p.max_delta_height || depth < p.min_depth || dist < p.min_dist || dist > p.max_dist {
            return false;
        }
        if cfg.check_vertical_velocity && velocity.z > 0.0 && gravity.z != 0.0 {
            // Skip ledges the jump will carry the player above anyway.
            let t = -(velocity.z / gravity.z);
            if dh < gravity.z * 0.5 * t * t + t * velocity.z {
                return false;
            }
        }
        true
    }
}

impl Player {
    /// The view heading the mechanic uses (FUN_140d9f8a0): with no grab running, the view forward
    /// flattened; during a grab, into the ledge face.
    fn ledge_heading(&self) -> Vec3 {
        let lg = &self.ledge;
        let f = if lg.state == ledge_state::NONE || lg.info[0].ledge_face_normal == Vec3::ZERO {
            crate::physics::angles_to_forward(self.view_angles)
        } else {
            -lg.info[0].ledge_face_normal
        };
        Vec3::new(f.x, f.y, 0.0).normalize_or_zero()
    }

    /// Event 0x11, broadcast by idPlayer::Move after physics (0x140e2f8e5): while a grab runs (and on
    /// the frame it ended) the velocity is overwritten.
    fn ledge_after_move(&mut self) {
        let lg = &self.ledge;
        if lg.state == ledge_state::NONE && lg.grab_end_time != self.time_ms {
            return;
        }
        let v = if lg.state == ledge_state::PULL_UP_FOOT || lg.last_grab_was_foot_grab {
            lg.foot_grab_start_vel
        } else {
            // The spring camera's velocity (player vtbl+0x9d0 → idSpringCamera +0xf58).
            self.grab_camera.velocity
        };
        self.physics.set_linear_velocity(v);
    }

    /// idPlayerMechanicLedgeGrab::Handle (0x140d9fc50), after the player's move.
    fn ledge_handle(&mut self, world: &World, cmd: &UserCmd) {
        let cfg = self.cfg.ledge.clone();
        let now = self.time_ms;
        self.ledge.frame += 1;
        let frame = self.ledge.frame;

        // Job setup (FUN_140da0dc0).
        let g = self.physics.gravity_normal;
        let forward = self.ledge_heading();
        let right = Vec3::new(forward.z * g.y - forward.y * g.z, forward.x * g.z - forward.z * g.x, forward.y * g.x - forward.x * g.y);
        let up = -g;
        let mut parms = LedgeGrab::choose_parms(&cfg, self.physics.water_level);
        self.ledge.parms_are_railing = false;
        self.ledge.found_ledge = false;
        self.ledge.is_railing = false;
        self.ledge.can_grab_and_pull_up = false;
        self.ledge.can_grab_and_climb_up = false;

        // Deferred surroundings check (FUN_140d9ccd0(1)): last frame's query is evaluated now.
        let stand = self.physics.normal_shape.clone();
        let width = self.cfg.bbox_width;
        let seen_last_frame = self.ledge.last_found_frame == Some(frame - 1);
        let saved_ledge_pos = self.ledge.info[0].ledge_pos;
        let saved_block = self.ledge.info[0].block_dist_forward;
        if let Some(q) = self.ledge.pending.take() {
            let railing_q = self.ledge.pending_railing.take();
            if let Some((infos, valid)) = self.ledge.evaluate(world, &cfg, &stand, width, &q) {
                let lg = &mut self.ledge;
                lg.info = infos;
                lg.valid_ledge_pos = valid;
                lg.last_found_frame = Some(frame);
                lg.found_ledge = true;
                let thickest = infos[0].ledge_thickness.max(infos[1].ledge_thickness).max(infos[2].ledge_thickness);
                lg.is_railing = thickest <= parms.max_railing_thickness;
                if lg.check_valid(&cfg, &parms, self.physics.velocity, self.physics.gravity * self.physics.gravity_scale) && cfg.enable_railing_above_ledge_grab {
                    let railing = railing_q.and_then(|rq| lg.evaluate(world, &cfg, &stand, width, &rq));
                    match railing {
                        Some((rinfos, _)) if 2.0 * cfg.railing_above_ledge_setback > rinfos[0].block_dist_forward - lg.info[0].block_dist_forward => {
                            lg.railing_info = rinfos;
                            lg.can_grab_and_climb_up = lg.stands(&rinfos, cfg.use_any_valid_pos);
                        }
                        _ => lg.can_grab_and_pull_up = lg.stands(&lg.info, cfg.use_any_valid_pos),
                    }
                }
            }
        }
        if self.ledge.state == ledge_state::NONE {
            self.ledge.pending = Some(LedgeQuery { pos: self.physics.origin, forward, right, up, parms, forward_test_dist: cfg.forward_test_dist, railing: false });
            if seen_last_frame {
                // The job keeps pointing at the railing parms afterwards (DAT[7] = DAT[0x3e]).
                self.ledge.parms_are_railing = true;
                self.ledge.pending_railing = Some(LedgeQuery {
                    pos: saved_ledge_pos,
                    forward,
                    right,
                    up,
                    parms: cfg.railing_above_ledge_parms,
                    forward_test_dist: saved_block + cfg.ledge_above_railing_additional_forward_test_dist,
                    railing: true,
                });
            }
        }
        if self.ledge.parms_are_railing {
            parms = cfg.railing_above_ledge_parms;
        }

        // Forward input, buffered for moving ledges (only used against movers, which this world lacks).
        let min_in = cfg.min_input_to_initiate.clamp(0.0, 1.0);
        if (cmd.forward as f32) / 127.0 < min_in {
            self.ledge.has_actual_press = false;
            self.ledge.has_buffered_press = false;
            self.ledge.valid_forward_press_time = 0;
        } else {
            self.ledge.valid_forward_press_time = now + parms.input_time_buffer;
            self.ledge.has_actual_press = true;
            self.ledge.has_buffered_press = true;
        }

        if self.ledge.state == ledge_state::NONE {
            if self.ledge_can_grab() {
                self.ledge_try_start(world, &cfg, &parms, now);
            }
            if ledge_state::is_foot(self.ledge.state) && self.ledge_foot_align(world, &cfg, now) {
                self.ledge_set_state(world, ledge_state::NONE, now);
            }
        } else if ledge_state::is_foot(self.ledge.state) {
            if self.ledge_foot_align(world, &cfg, now) {
                self.ledge_set_state(world, ledge_state::NONE, now);
            }
        } else {
            let anim = &cfg.anims[self.ledge.state as usize];
            let frame_now = (now - self.ledge.grab_start_time) as i64 * anim.frame_rate as i64 / 1000;
            if frame_now >= anim.num_frames as i64 - 1 {
                self.ledge_set_state(world, ledge_state::NONE, now);
            }
        }
    }

    /// vtbl+0x18 (0x140da12d0): past the no-grab time, not swimming, not crouched.
    fn ledge_can_grab(&self) -> bool {
        self.time_ms >= self.ledge.can_grab_time && self.physics.water_level <= 0.9 && !self.physics.ducked()
    }

    fn ledge_try_start(&mut self, world: &World, cfg: &LedgeGrabConfig, parms: &LedgeTestParms, now: i32) {
        use ledge_state::*;
        let heading = self.ledge_heading();
        let face = self.ledge.info[0].ledge_face_normal;
        let n = Vec3::new(face.x, face.y, 0.0).normalize_or_zero();
        let c = -heading.dot(n);
        let angle = if c <= -1.0 { std::f32::consts::PI } else if c >= 1.0 { 0.0 } else { c.acos() };
        let pressed = self.ledge.has_actual_press;
        if angle * 57.295776 > cfg.initiate_max_angle || !(pressed || cfg.assume_always_forward_press) {
            return;
        }
        let lg = &mut self.ledge;
        let dh = lg.selected().ledge_delta_height;
        let edge = lg.info[0].edge_type;
        let state;
        if !lg.is_railing {
            if lg.can_grab_and_pull_up {
                if dh < parms.grab_foot_delta_height && cfg.enable_foot_grab {
                    lg.grab_start_time = now;
                    lg.grab_end_time = now + 500;
                    self.ledge_set_state(world, PULL_UP_FOOT, now);
                    return;
                }
                state = if parms.grab_mantle_delta_height <= dh {
                    [PULL_UP, PULL_UP_ROUNDED, PULL_UP_ANGLED][edge.clamp(0, 2) as usize]
                } else {
                    [PULL_UP_MANTLE, PULL_UP_MANTLE_ROUNDED, PULL_UP_MANTLE_ANGLED][edge.clamp(0, 2) as usize]
                };
            } else if lg.can_grab_and_climb_up {
                if parms.grab_foot_delta_height <= dh || !cfg.enable_foot_grab {
                    if dh < parms.grab_mantle_delta_height {
                        // The railing's infos replace the ledge's, and its height is added on top.
                        lg.info = lg.railing_info;
                        lg.info[0].ledge_delta_height += lg.railing_info[0].ledge_delta_height;
                        state = RAILING_PULL_UP;
                    } else {
                        if lg.selected().ledge_height_above_ground < parms.min_height {
                            return;
                        }
                        state = CLIMB_UP;
                    }
                } else {
                    lg.info[0] = lg.railing_info[0];
                    state = RAILING_PULL_UP_MANTLE;
                }
            } else {
                return;
            }
        } else if lg.can_grab_and_pull_up {
            if dh < parms.grab_foot_delta_height && cfg.enable_foot_grab {
                lg.grab_start_time = now;
                lg.grab_end_time = now + 500;
                self.ledge_set_state(world, RAILING_PULL_UP_FOOT, now);
                return;
            }
            state = if parms.grab_mantle_delta_height <= dh { RAILING_PULL_UP } else { RAILING_PULL_UP_MANTLE };
        } else {
            return;
        }
        if self.ledge_check_origin_destination(world, cfg, state) {
            self.ledge_set_state(world, state, now);
        }
    }

    /// CheckOriginDestination (0x140d9af70): where the grab will end, from the animation's origin/align
    /// joints rotated by the heading, and whether the standing model fits there.
    fn ledge_check_origin_destination(&mut self, world: &World, cfg: &LedgeGrabConfig, state: i32) -> bool {
        let anim = &cfg.anims[state as usize];
        let d = anim.origin_destination - anim.align_pos;
        let h = self.ledge_heading();
        let up = Vec3::Z;
        let side = h.cross(up);
        let world_d = up * d.z + h * d.x + side * d.y;
        let sel = *self.ledge.selected();
        let n = sel.ledge_surface_normal;
        let mut dest = sel.ledge_pos + world_d;
        dest.z += world_d.z - world_d.dot(n) * n.z;
        let nz = n.z.clamp(-1.0, 1.0);
        dest.z += (1.0 - nz * nz).sqrt() * self.cfg.bbox_width * 0.5 + 1.0;
        let (hit, _, tr) = ledge_ray(world, dest + up * self.cfg.normal_height, dest);
        if hit {
            dest = tr.c.point;
        }
        self.ledge.world_origin_destination = dest;
        world.position_clear(&self.physics.normal_shape, dest)
    }

    /// SetState (0x140da19a0) with StartFootGrab (0x140da2280), StartLedgeGrab (0x140d98380),
    /// EndLedgeGrab (0x140d9f120) and EndLedgeGrabSetup (0x140d9f570).
    fn ledge_set_state(&mut self, _world: &World, new_state: i32, now: i32) {
        use ledge_state::*;
        let cfg_no_grab = self.cfg.ledge.no_grab_time_ms;
        if new_state == NONE {
            let lg = &mut self.ledge;
            let old = lg.state;
            lg.can_grab_time = now + cfg_no_grab;
            lg.grab_end_time = now;
            if !is_foot(old) {
                // EndLedgeGrab (0x140d9f120): the camera's velocity, then idPlayer::Teleport
                // (vtbl+0xb10 → 0x140de7bd0) to worldOriginDestination: SetOrigin(pos + 0.25 up) and
                // SetLinearVelocity(vec3_zero) (0x14074f880), push velocities and contacts cleared
                // (0x140ddc7f0). The camera blends back to the eye and the view constraint is lifted.
                self.physics.set_linear_velocity(self.grab_camera.velocity);
                self.physics.origin = lg.world_origin_destination + Vec3::Z * 0.25;
                self.physics.set_linear_velocity(Vec3::ZERO);
                self.physics.contacts.clear();
                self.grab_camera.detach(now);
                self.view_constraint = None;
                // player +0xce47: &= ~8, |= 4 (the next landing plays a forced landing sound).
                self.land.after_foot_grab = false;
                self.land.after_anim_grab = true;
            } else {
                // Foot grabs: +0xce47 &= ~4, |= 8 (the next landing is not reported).
                self.land.after_anim_grab = false;
                self.land.after_foot_grab = true;
            }
        } else {
            // StartLedgeGrabBase (0x140da2530): jump must be released before the next double jump.
            self.physics.jump_held_since_jump = true;
            let lg = &mut self.ledge;
            lg.start_origin = self.physics.origin;
            if is_foot(new_state) {
                lg.body_align_pos = lg.info[0].ledge_pos;
                lg.last_grab_was_foot_grab = true;
                lg.foot_grab_start_vel = self.physics.velocity;
            } else {
                // StartLedgeGrab (0x140da2350): the body is placed with its align joint on the ledge
                // (0x140d98380 → 0x140756340 mode 2), facing the face normal of info[0]; frame 0 of
                // its animation attaches the camera; the view is constrained around the wall.
                let sel = *lg.selected();
                lg.body_align_pos = sel.ledge_pos;
                lg.grab_start_time = now;
                lg.last_grab_was_foot_grab = false;
                let n = lg.info[0].ledge_face_normal;
                let fwd = -Vec3::new(n.x, n.y, 0.0).normalize_or_zero();
                let axis = [fwd, Vec3::Z.cross(fwd), Vec3::Z];
                let align = self.cfg.ledge.anims[new_state as usize].align_pos;
                let body = sel.ledge_pos - (axis[0] * align.x + axis[1] * align.y + axis[2] * align.z);
                self.grab_camera.attach(now, new_state, body, axis, self.cfg.ledge.view_blend_duration_ms);
                self.view_constraint = Some(ViewConstraint::around(-n, &self.cfg.ledge));
            }
        }
        let lg = &mut self.ledge;
        lg.prev_state = lg.state;
        lg.state = new_state;
    }

    /// Foot-grab alignment (0x140da2980 → 0x140d8f9b0): each frame the origin moves toward the ledge
    /// point raised by FootGrabAlignHeightOffset by min(dt·dist·FootGrabAlignSpeed, dist); done when
    /// within FootGrabAlignDist, or forced there once grabEndTime has passed.
    fn ledge_foot_align(&mut self, _world: &World, cfg: &LedgeGrabConfig, now: i32) -> bool {
        let target = self.ledge.body_align_pos + Vec3::Z * cfg.foot_grab_align_height_offset;
        let d = target - self.physics.origin;
        let dist = d.length();
        let mut moved = false;
        if cfg.foot_grab_align_dist < dist {
            let step = (self.physics.frametime * dist * cfg.foot_grab_align_speed).min(dist);
            self.physics.origin += d / dist * step;
            moved = true;
        }
        let timed_out = self.ledge.grab_end_time < now;
        if timed_out {
            self.physics.origin = target;
        }
        timed_out || !moved
    }

    /// Whether an animated grab is playing.
    pub fn ledge_grab_animating(&self) -> bool {
        self.ledge.state != ledge_state::NONE && !ledge_state::is_foot(self.ledge.state)
    }
}

// ---- Spring camera and view constraint as the ledge grab uses them ------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraMode {
    /// idSpringCamera state 0: not driving the view.
    #[default]
    Off,
    /// Attached to the body's "camera" joint (state 1, set up by ae_attachCamera).
    Attached,
    /// Blending back to the eye (state 5, 0x140a2fa40(cam, 0) → 0x140a34490).
    BlendOut,
}

/// The player's spring camera (idSpringCamera, vtbl 0x1421c0d10, at player +0x46ed8) reduced to what
/// the ledge grab uses. Frame 0 of every ledge animation fires `ae_attachCamera "camera"`
/// (DURATION_ANIM, LOOK_NONE, BLEND_SMOOTH_SNAP_SMOOTH; tp_body.md6), so the camera follows the body's
/// camera joint; event 0x11 copies its velocity into the player's physics every frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GrabCamera {
    pub mode: CameraMode,
    /// +0xf24.
    pub pos: Vec3,
    /// +0xf58: (pos − previous pos) / game time between updates (0x140a31da0).
    pub velocity: Vec3,
    /// +0xf64: game time of the last update.
    last_time: i32,
    /// +0xfd6 clear: the next update starts from the eye with no velocity.
    restart: bool,
    /// +0xd14 / +0xd10: blend start and duration (0x140a334a0).
    blend_start: i32,
    blend_ms: i32,
    /// +0xd24: the position blended away from.
    from: Vec3,
    anim_state: i32,
    anim_start: i32,
    body_origin: Vec3,
    /// Forward, left, up.
    body_axis: [Vec3; 3],
}

impl GrabCamera {
    fn attach(&mut self, now: i32, anim_state: i32, body_origin: Vec3, body_axis: [Vec3; 3], blend_ms: i32) {
        self.mode = CameraMode::Attached;
        self.restart = true;
        self.blend_start = now;
        self.blend_ms = blend_ms;
        self.anim_state = anim_state;
        self.anim_start = now;
        self.body_origin = body_origin;
        self.body_axis = body_axis;
    }

    fn detach(&mut self, now: i32) {
        if self.mode == CameraMode::Attached {
            self.mode = CameraMode::BlendOut;
            self.blend_start = now;
            self.from = self.pos;
        }
    }

    /// Weight of the blend source (0x140a31510): t³ − 2t² + 1 over the blend duration.
    fn source_weight(&self, now: i32) -> f32 {
        if self.blend_ms <= 0 {
            return 0.0;
        }
        let t = ((now - self.blend_start) as f32 / self.blend_ms as f32).min(1.0);
        t * t * t - (t + t) * t + 1.0
    }

    /// The camera's think (0x140a31da0): the target is the joint while attached and the eye while
    /// blending out; the position blends from the source to it; the velocity is the finite difference.
    /// `joint(state, seconds)` gives the camera joint in model space at that animation time.
    pub fn update(&mut self, now: i32, eye: Vec3, joint: impl Fn(i32, f32) -> Option<Vec3>) {
        if self.mode == CameraMode::Off {
            return;
        }
        let prev = self.pos;
        let w = self.source_weight(now);
        if self.restart {
            // First update after attaching: the camera starts at the owner's view.
            self.restart = false;
            self.pos = eye;
            self.from = eye;
            self.last_time = now;
            self.velocity = Vec3::ZERO;
            return;
        }
        let target = match self.mode {
            CameraMode::Attached => {
                let t = (now - self.anim_start) as f32 * 0.001;
                let c = joint(self.anim_state, t).unwrap_or(Vec3::ZERO);
                let a = self.body_axis;
                self.body_origin + a[0] * c.x + a[1] * c.y + a[2] * c.z
            }
            _ => eye,
        };
        self.pos = target * (1.0 - w) + self.from * w;
        if now != self.last_time {
            self.velocity = (self.pos - prev) / ((now - self.last_time) as f32 * 0.001);
        }
        self.last_time = now;
        if self.mode == CameraMode::BlendOut && w <= 0.001 {
            self.mode = CameraMode::Off;
        }
    }
}

/// View-angle limits on the player controller (SetConstraint 0x140df7650, applied by 0x140df68e0).
/// A rate ≤ 0 clamps hard; a positive rate pushes angles outside the range back at that many degrees
/// per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewConstraint {
    /// Pitch, yaw, roll (the yaw minimum is unwrapped to ≤ the maximum).
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub yaw_rate: f32,
    pub pitch_rate: f32,
}

impl ViewConstraint {
    /// The ledge grab's constraint (0x140da2350 → 0x140d902a0): centred on `dir`'s angles,
    /// pitch −MaxDeltaPitchUp..+MaxDeltaPitchDown, yaw ±MaxDeltaYaw, roll 0; yaw eased at
    /// ConstrainedViewAnglesRate, pitch clamped.
    pub fn around(dir: Vec3, cfg: &LedgeGrabConfig) -> Self {
        let c = vec_to_angles(dir);
        let mut min = [c[0] - cfg.max_delta_pitch_up, c[1] - cfg.max_delta_yaw, c[2]];
        let max = [c[0] + cfg.max_delta_pitch_down, c[1] + cfg.max_delta_yaw, c[2]];
        while min[1] > max[1] {
            min[1] -= 360.0;
        }
        Self { min, max, yaw_rate: cfg.constrained_view_angles_rate, pitch_rate: -1.0 }
    }

    /// 0x140df68e0: limit `new` given the angles the constraint let through last frame.
    pub fn apply(&self, prev: [f32; 3], new: &mut [f32; 3], dt: f32) {
        let mut prev = prev;
        let center = (self.min[1] + self.max[1]) * 0.5;
        while new[1] < center - 180.0 {
            new[1] += 360.0;
        }
        if prev[1] < center - 180.0 {
            prev[1] += 360.0;
        }
        while new[1] > center + 180.0 {
            new[1] -= 360.0;
        }
        while prev[1] > center + 180.0 {
            prev[1] -= 360.0;
        }
        new[1] = Self::limit(new[1], prev[1], self.min[1], self.max[1], self.yaw_rate, dt);
        if new[1] >= 360.0 || new[1] < 0.0 {
            new[1] -= (new[1] * (1.0 / 360.0)).floor() * 360.0;
        }
        if new[1] > 180.0 {
            new[1] -= 360.0;
        }
        new[0] = Self::limit(new[0], prev[0], self.min[0], self.max[0], self.pitch_rate, dt);
    }

    fn limit(v: f32, prev: f32, min: f32, max: f32, rate: f32, dt: f32) -> f32 {
        if rate <= 0.0 {
            return v.min(max).max(min);
        }
        if v > max {
            if v <= prev {
                v - dt * rate
            } else if max < prev {
                prev - dt * rate
            } else {
                max
            }
        } else if v < min {
            if prev <= v {
                v + dt * rate
            } else if prev < min {
                prev + dt * rate
            } else {
                min
            }
        } else {
            v
        }
    }
}

/// idVec3::ToAngles then Normalize180: pitch, yaw, roll in degrees (idTech pitch is positive down).
fn vec_to_angles(v: Vec3) -> [f32; 3] {
    if v.x == 0.0 && v.y == 0.0 {
        return [if v.z > 0.0 { -90.0 } else { 90.0 }, 0.0, 0.0];
    }
    let forward = (v.x * v.x + v.y * v.y).sqrt();
    [-v.z.atan2(forward).to_degrees(), v.y.atan2(v.x).to_degrees(), 0.0]
}

// ---- Jump and landing events (idPlayer, after Move) ---------------------------------------------

/// What the player did this frame, for the hands (pending actions), sound and rumble layers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayerEvents {
    /// CheckJump took off (movement flag JUMPED 0x2): the jump callback 0x140e2fbe0 sets the hands'
    /// pending action JUMP (14), and Think (0x140e31674) plays playerProps sndJump with the
    /// controllerRumble singleJump rumble.
    pub jumped: bool,
    /// CheckDoubleJump fired (DOUBLE_JUMPED 0x200): the same callback sets JUMP (14), plays the jump boots'
    /// doubleJumpSound and rumbles; Think plays sndJump with the doubleJump rumble.
    pub double_jumped: bool,
    /// The landing test (0x140e33110) saw the player land.
    pub landed: Option<Landing>,
    /// While falling, the fall became a large landing or worse: playerProps sndFallingLargeLandingStart
    /// (once per fall).
    pub falling_large_start: bool,
    /// Landed after sndFallingLargeLandingStart: play sndFallingLargeLandingStop.
    pub falling_large_stop: bool,
    /// While falling, the fall reached fatalDamageDistance (or a pit): sndFallingFatal (once per fall;
    /// the handle is stopped on landing).
    pub falling_fatal: bool,
}

/// The landing classification of 0x140e33110, against playerFalling distances. Each reaction also
/// needs the previous frame's fall speed |v.z| above 0.95·sqrt(2·|g|·distance).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandSize {
    /// Below smallLandingReactionDistance (or too slow): no hands reaction.
    None,
    /// Hands LAND_SM (18).
    Small,
    /// Hands LAND_MED (19).
    Medium,
    /// Hands LAND_LG (20); the landing sound uses the heavy table.
    Large,
    /// extraLargeLandingReactionDistance: no hands action and no landing sound from this path; the
    /// game plays a full-body reaction (0x140df5260) instead.
    ExtraLarge,
    /// fatalDamageDistance reached, or no ground below and noGroundKillPlayerFallDist exceeded.
    Fatal,
}

/// Which landing sound plays (idPlayer vtbl+0xc08 kind → effect table, 0x14074b0a0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandSound {
    None,
    /// Kind 4: the landing effect table (`landings/player`).
    Normal,
    /// Kind 8: the heavy landing table (`landings/player_heavy`), for large landings.
    Heavy,
    /// A contact with surface flag NODAMAGE (0x10): playerProps sndLandNoDamage instead.
    NoDamage,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landing {
    pub size: LandSize,
    /// Apex z − landing z + 16 (0x140e2d390).
    pub fall_height: f32,
    /// |v.z| before the landing frame's move.
    pub impact_speed: f32,
    pub sound: LandSound,
}

impl Landing {
    /// The idHands pending action (SetPendingAction_ 0x140d68d60): LAND_SM 18, LAND_MED 19, LAND_LG 20.
    pub fn hands_action(&self) -> Option<u32> {
        match self.size {
            LandSize::Small => Some(18),
            LandSize::Medium => Some(19),
            LandSize::Large => Some(20),
            _ => None,
        }
    }
}

/// The landing test's state on idPlayer.
#[derive(Debug, Clone, Copy, Default)]
struct LandTracker {
    /// +0x14d98: on the ground as of the last test.
    on_ground: bool,
    /// +0x14d78 / +0x14d84: where and when the tracking was last reset (every grounded frame).
    start_origin: Vec3,
    start_time: i32,
    /// +0x14d88 / +0x14d94: highest point since.
    apex: Vec3,
    /// +0x14d99: sndFallingLargeLandingStart played this fall.
    large_wind: bool,
    /// +0x14da0: sndFallingFatal playing.
    fatal_sound: bool,
    /// +0x14c50 / +0x14c5c: origin and velocity before this frame's move.
    saved_origin: Vec3,
    saved_velocity: Vec3,
    /// Game frames run (the test is skipped for the first ten).
    frames: u32,
    /// Player +0xce47 bit 8 (set when a foot grab ends: skip the next landing) and bit 4 (set when an
    /// animated grab ends: force a normal landing sound).
    after_foot_grab: bool,
    after_anim_grab: bool,
}

impl Player {
    fn reset_fall(&mut self) {
        let o = self.physics.origin;
        self.land.start_origin = o;
        self.land.start_time = self.time_ms;
        self.land.apex = o;
    }

    /// The landing test (0x140e33110), run at the end of idPlayer::Move. Not modelled: the wall-grab
    /// and rail-ride mechanics (absent), fall damage and the landing view kick (0x140e328f0).
    fn update_landing(&mut self, world: &World) {
        let water = self.physics.water_level;
        if self.land.frames <= 9 || water >= 0.99 {
            if self.land.frames > 9 {
                self.land.on_ground = true;
            }
            self.reset_fall();
            return;
        }
        if self.physics.gravity == Vec3::ZERO {
            self.reset_fall();
            return;
        }
        let ground = self.physics.ground_plane;
        if self.land.on_ground && ground {
            self.reset_fall();
            return;
        }
        self.land.on_ground = ground;
        let cfg = self.cfg.falling;
        let origin = self.physics.origin;
        let fall = self.land.apex.z - origin.z + 16.0;
        // groundTestDistance trace below the pre-move origin (deferred in the game, read a frame later).
        let below = self.land.saved_origin - Vec3::Z * cfg.ground_test_distance;
        let pit = world.translate(&Hull::trace_box(), self.land.saved_origin, below).fraction >= 1.0 && fall > cfg.no_ground_kill_player_fall_dist;
        let g2 = self.physics.gravity.z.abs() * 2.0;
        let speed = self.land.saved_velocity.z.abs();
        let fast = |h: f32| speed > (h * g2).max(f32::MIN_POSITIVE).sqrt() * 0.95;
        let size = if pit {
            LandSize::Fatal
        } else if cfg.fatal_damage_distance <= fall {
            if !self.land.fatal_sound {
                self.land.fatal_sound = true;
                self.events.falling_fatal = true;
            }
            LandSize::Fatal
        } else if fall >= cfg.extra_large_landing_reaction_distance && fast(cfg.extra_large_landing_reaction_distance) {
            LandSize::ExtraLarge
        } else if fall >= cfg.large_landing_reaction_distance && fast(cfg.large_landing_reaction_distance) {
            if !self.land.large_wind {
                self.land.large_wind = true;
                self.events.falling_large_start = true;
            }
            LandSize::Large
        } else if fall >= cfg.medium_landing_reaction_distance && fast(cfg.medium_landing_reaction_distance) {
            LandSize::Medium
        } else if fall >= cfg.small_landing_reaction_distance && fast(cfg.small_landing_reaction_distance) {
            LandSize::Small
        } else {
            LandSize::None
        };
        if !(ground || pit) {
            // Still in the air: track the highest point above the take-off height.
            if origin.z > self.land.start_origin.z && origin.z > self.land.apex.z {
                self.land.apex = origin;
            }
            return;
        }
        self.land.fatal_sound = false;
        if self.land.large_wind {
            self.land.large_wind = false;
            self.events.falling_large_stop = true;
        }
        if self.land.after_foot_grab {
            self.land.after_foot_grab = false;
            return;
        }
        let mut sound = LandSound::None;
        if fall >= cfg.min_dist_for_sound || self.land.after_anim_grab {
            if self.physics.contacts.iter().any(|c| c.surface_flags & 0x10 != 0) {
                sound = LandSound::NoDamage;
            } else if pit {
            } else if !self.land.after_anim_grab {
                if size != LandSize::ExtraLarge {
                    sound = if size == LandSize::Large { LandSound::Heavy } else { LandSound::Normal };
                }
            } else {
                self.land.after_anim_grab = false;
                sound = LandSound::Normal;
            }
        }
        self.events.landed = Some(Landing { size, fall_height: fall, impact_speed: speed, sound });
    }

    /// The hands' airborne test (0x140d63260): the landing test has not reset the fall tracking this
    /// frame, i.e. the player was not on the ground (also true on the landing frame itself). Its other
    /// conditions (bound to a master, ladder, wall grab, rail ride, g_stopTime) never apply here.
    pub fn falling(&self) -> bool {
        self.time_ms - self.land.start_time > 0
    }
}

// ---- Movers pushing and carrying the player (idPush for the player) --------------------------------

/// idPush flags (ClipPush's `flags`) the player push looks at.
pub mod push_flags {
    /// PUSHFL_CRUSH (0x10): keep moving into a pinned player instead of stopping.
    pub const CRUSH: u32 = 0x10;
}

impl Player {
    /// Moves the mover `id` (a `World::cms` index) to `origin` / `axis` with idPush's rules for the
    /// player (ClipTranslationalPush 0x141792130 / ClipRotationalPush 0x141791960 →
    /// TryTranslatePushPhysicsObject 0x141793b50): a player touching the mover (IsGroundClipModel) is
    /// carried — moved with it as far as the world allows; otherwise the mover's sweep pushes the
    /// player when it reaches them. Pushes go into the player's physics as Translate impulse / Rotate
    /// push velocity (the next move applies them with collision). A player pinned against the world
    /// stops the mover unless `flags` has CRUSH. Returns the fraction of the move made (and applied to
    /// the world); call it after `think`, as entities move after the player in the game.
    /// Rotational sweeps are tested as the player's displacement under the rotation (not a true arc).
    pub fn push_mover(&mut self, world: &mut World, id: usize, origin: Vec3, axis: Mat3, flags: u32) -> f32 {
        let (old_origin, old_axis) = (world.cms[id].origin, world.cms[id].axis);
        let pusher = world.brushes.len() + id;
        let ph = &self.physics;
        let shape = ph.shape().clone();
        let p = ph.origin;
        // Where the mover's motion takes the player's origin.
        let carried_to = origin + axis * (old_axis.transpose() * (p - old_origin));
        let translation = origin - old_origin;
        let rotation_delta = carried_to - p - translation;
        let riding = ph.touches(pusher);
        let mut pushed = Vec3::ZERO;
        let mut blocked = false;
        for (part, is_rotation) in [(translation, false), (rotation_delta, true)] {
            if part == Vec3::ZERO {
                continue;
            }
            let at = p + pushed;
            let mv = if riding {
                // Carried: as far as the world (without the mover) lets the player go; a shortfall
                // the mover would run into is a block.
                let t = world.translate_filtered(&shape, at, at + part, TraceFilter::Without(id));
                if t.fraction < 1.0 {
                    let back = world.translate_filtered(&shape, at, at + part * (t.fraction - 1.0), TraceFilter::Only(id));
                    if back.fraction < 1.0 {
                        blocked = true;
                        break;
                    }
                }
                part * t.fraction
            } else {
                // Pushed: only if the mover's sweep reaches the player, by the rest of the move.
                let hit = world.translate_filtered(&shape, at, at - part, TraceFilter::Only(id));
                if hit.fraction >= 1.0 {
                    continue;
                }
                let rest = part * (1.0 - hit.fraction);
                let t = world.translate_filtered(&shape, at, at + rest, TraceFilter::Without(id));
                if t.fraction < 1.0 {
                    blocked = true;
                    break;
                }
                rest
            };
            pushed += mv;
            if is_rotation {
                let (axis_dir, _) = rotation_axis(axis * old_axis.transpose());
                self.physics.rotate_push(old_origin, axis_dir, at + mv);
            } else {
                self.physics.translate_push(mv);
            }
        }
        if blocked && flags & push_flags::CRUSH == 0 {
            return 0.0;
        }
        world.set_cm_transform(id, origin, axis);
        1.0
    }
}

impl Player {
    /// A bind team moving together (idEntity::RunPhysics runs every part's physics, master first; each
    /// pusher part does its own ClipPush, and a blocked part restores the whole team and what it pushed —
    /// idTech4 lineage, not separately traced in this exe): `moves` = (cm id, origin, axis) per part
    /// with collision, master first. Returns false (and leaves the world and the player's pushes as
    /// they were) when any part is blocked.
    pub fn push_team(&mut self, world: &mut World, moves: &[(usize, Vec3, Mat3)], flags: u32) -> bool {
        let saved: Vec<(usize, Vec3, Mat3)> = moves.iter().map(|&(id, _, _)| {
            let (o, a) = world.cm_transform(id);
            (id, o, a)
        }).collect();
        let (impulse, push_velocity, center) = (self.physics.push_impulse, self.physics.push_velocity, self.physics.push_center);
        for &(id, origin, axis) in moves {
            if self.push_mover(world, id, origin, axis, flags) < 1.0 {
                for &(id, o, a) in &saved {
                    world.set_cm_transform(id, o, a);
                }
                self.physics.push_impulse = impulse;
                self.physics.push_velocity = push_velocity;
                self.physics.push_center = center;
                return false;
            }
        }
        true
    }
}

/// Unit axis and angle (radians) of a rotation matrix.
fn rotation_axis(m: Mat3) -> (Vec3, f32) {
    let (axis, angle) = glam::Quat::from_mat3(&m).to_axis_angle();
    (axis, angle)
}
