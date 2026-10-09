//! Player physics, mirroring DOOM (2016)'s player physics routines one function at a time:
//! MovePlayer → CheckGround / CheckDuck → WalkMove | AirMove → Friction, CmdScale, Accelerate,
//! CheckJump, CheckDoubleJump → SlideMove. Field defaults are the values the game's physics
//! constructor writes; tunables arrive from the player layer each frame, as in the game.

use glam::Vec3;

use crate::cmd::{UserCmd, button};
use crate::collision::{self, Contact, ContactKind, Hull, MoverProps, World};

pub mod flags {
    pub const DUCKED: u32 = 0x1;
    pub const JUMPED: u32 = 0x2;
    pub const STEPPED_UP: u32 = 0x4;
    pub const STEPPED_DOWN: u32 = 0x8;
    pub const JUMP_HELD: u32 = 0x10;
    pub const TIME_LAND: u32 = 0x20;
    pub const TIME_KNOCKBACK: u32 = 0x40;
    pub const TIME_WATERJUMP: u32 = 0x80;
    /// Velocity set from outside the move (SetLinearVelocity 0x1416b3e10): the next SlideMove keeps the
    /// velocity instead of deriving it from the displacement; MovePlayer clears it at its end.
    pub const VELOCITY_SET: u32 = 0x100;
    pub const DOUBLE_JUMPED: u32 = 0x200;
    /// Cleared at the start of every MovePlayer.
    pub const PER_FRAME: u32 = JUMPED | STEPPED_UP | STEPPED_DOWN | DOUBLE_JUMPED;
}

/// Literal constants from the game's player physics code.
mod k {
    pub const STOP_SPEED: f32 = 100.0;
    pub const OVERCLIP: f32 = 1.001;
    pub const WATER_FEET: f32 = 0.425;
    pub const CMD_SCALE: f32 = 0.007874016; // 1/127
    pub const JUMP_INPUT: i8 = 9; // jumping needs up > 9
    pub const JUMP_RELEASE: i8 = 10; // held-jump clears when up < 10
    pub const HARD_LANDING_SPEED: f32 = -200.0;
    pub const LAND_TIME_MS: i32 = 250;
    pub const BLOCK_SPEED_FACTOR: f32 = 0.9;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveType {
    Normal,
    Noclip,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoubleJumpState {
    pub height: f32,
    pub speed_scale: f32,
    pub air_english_scale: f32,
    pub window_offset_ms: i32,
    pub window_duration_ms: i32,
    pub gravity: Vec3,
}

#[derive(Debug, Clone)]
pub struct PlayerPhysics {
    pub origin: Vec3,
    pub velocity: Vec3,
    /// playerPState_t.acceleration (+0x1adc, game units/s²): (velocity − the previous state's velocity) /
    /// (frame msec · 0.001), rewritten after each velocity change the move makes (Accelerate,
    /// Friction, CheckJump, CheckDoubleJump, the end of SlideMove, SetLinearVelocity); zero when the
    /// frame has no msec (SetLinearVelocity then leaves it). Read by the hands' weapon lag (0x140d8d2c0).
    pub acceleration: Vec3,
    /// The previous state's velocity (+0x1b60): copied at the end of MovePlayer (0x140e2bad0 at
    /// 0x1416b10xx), so velocity set afterwards (SetLinearVelocity) is not in it.
    pub prev_velocity: Vec3,
    /// Movers' pushes since the last move (playerPState_t): Translate's impulse (+0x1ae8, units/s) and
    /// Rotate's push velocity (+0x1af4) with the rotation axis point nearest the player (+0x1b00).
    /// Consumed by AirMove / SlideMove and cleared at the end of MovePlayer (vtbl+0x2d8, 0x1416ae670).
    pub push_impulse: Vec3,
    pub push_velocity: Vec3,
    pub push_center: Vec3,
    /// The previous state's groundPlane (+0x1bd4) and impulse + push velocity (+0x1b84 / +0x1b90).
    prev_ground_plane: bool,
    prev_push_sum: Vec3,
    /// CheckGround's ground entity (+0x20e4, kept while airborne) and the ground physics' mover flags
    /// (+0x2218, cleared with no contacts).
    pub ground_kind: GroundKind,
    pub ground_mover: Option<MoverProps>,
    pub cmd: UserCmd,
    pub view_angles: [f32; 3],
    pub view_forward: Vec3,
    pub view_right: Vec3,
    pub move_type: MoveType,
    pub flags: u32,
    pub timer_ms: i32,
    pub walking: bool,
    pub ground_plane: bool,
    pub ground_normal: Vec3,
    pub ground_slick: bool,
    pub contacts: Vec<Contact>,
    pub water_level: f32,

    pub gravity: Vec3,
    pub gravity_normal: Vec3,
    pub gravity_scale: f32,
    pub jump_scale: f32,

    pub walk_speed: f32,
    pub crouch_speed: f32,
    pub player_speed: f32,
    pub speed_scale: f32,
    pub walk_accel: f32,
    pub air_accel: f32,
    pub walk_friction: f32,
    pub air_friction: f32,
    pub double_jump_friction: f32,
    pub fly_friction: f32,
    pub water_friction: f32,
    pub walk_friction_alt: f32,
    pub air_control_forward: f32,
    pub air_control_right: f32,
    pub velocity_scale: Vec3,
    pub min_ground_separation: f32,

    pub max_step_height: f32,
    pub max_jump_height: f32,
    pub jump_count: i32,
    pub max_jumps: i32,
    pub extra_jumps: i32,
    pub jump_time: i32,
    pub jump_held_since_jump: bool,
    pub jump_held_left_ground: bool,
    pub last_double_jump_time: i32,
    pub double_jump_interrupted: bool,
    pub double_jump: DoubleJumpState,
    pub jump_extension_start: Vec3,

    pub crouch_latched: bool,
    pub stand_latched: bool,
    pub crouch_enabled: bool,
    pub last_crouch_toggle_time: i32,
    pub prev_up: i8,

    pub block_time: f32,
    pub block_normal: Vec3,

    pub time: i32,
    pub frame_msec: i32,
    pub frametime: f32,
    pub step_accum: f32,
    /// SlideMove's standing-still step lock (0x21e4) with the origin and step it saw (0x21e8, 0x21f4).
    pub step_lock: bool,
    pub step_lock_origin: Vec3,
    pub step_lock_step: f32,

    pub normal_shape: Hull,
    pub crouch_shape: Hull,

    /// Cvars the physics code reads directly.
    pub tun: PhysicsCvars,
}

#[derive(Debug, Clone)]
pub struct PhysicsCvars {
    pub crouch_toggle: bool,
    pub stick_to_ground: bool,
    pub stick_to_ground_dot: f32,
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
    /// pm_pusherWalkMoveCushion (10) / pm_pusherDislodgeMultiplier (1.5).
    pub pusher_walk_move_cushion: f32,
    pub pusher_dislodge_multiplier: f32,
}

/// The ground entity kind SlideMove's push code distinguishes (+0x20e4: ENTITYNUM_WORLD 0x1fffffd,
/// ENTITYNUM_NONE 0x1fffffe, or an entity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundKind {
    None,
    World,
    /// A `World::cms` index (a mover).
    Entity(usize),
}

const UP: Vec3 = Vec3::new(0.0, 0.0, 1.0);
const DOWN: Vec3 = Vec3::new(0.0, 0.0, -1.0);

pub fn angles_to_forward(a: [f32; 3]) -> Vec3 {
    let (sp, cp) = a[0].to_radians().sin_cos();
    let (sy, cy) = a[1].to_radians().sin_cos();
    Vec3::new(cp * cy, cp * sy, -sp)
}

/// Projects `v` onto the plane `n`, overclipping by 1.001 (the game's literal).
fn project_overclip(v: Vec3, n: Vec3) -> Vec3 {
    let d = v.dot(n);
    let d = if d >= 0.0 { d / k::OVERCLIP } else { d * k::OVERCLIP };
    v - n * d
}

impl PlayerPhysics {
    /// `normal_shape` / `crouch_shape` are the standing and crouched trace models (SetClipModel 0x1416b2aa0).
    pub fn new(origin: Vec3, normal_shape: Hull, crouch_shape: Hull, gravity: f32, tun: PhysicsCvars) -> Self {
        let g = Vec3::new(0.0, 0.0, -gravity);
        Self {
            origin,
            velocity: Vec3::ZERO,
            cmd: UserCmd::default(),
            view_angles: [0.0; 3],
            view_forward: Vec3::X,
            view_right: Vec3::NEG_Y,
            move_type: MoveType::Normal,
            flags: 0,
            timer_ms: 0,
            walking: false,
            ground_plane: false,
            ground_normal: Vec3::Z,
            ground_slick: false,
            contacts: Vec::new(),
            water_level: 0.0,
            gravity: g,
            gravity_normal: g.normalize(),
            gravity_scale: 1.0,
            jump_scale: 1.0,
            walk_speed: 0.0,
            crouch_speed: 0.0,
            player_speed: 0.0,
            speed_scale: 1.0,
            walk_accel: 10.0,
            air_accel: 1.0,
            walk_friction: 10.0,
            air_friction: 0.0,
            double_jump_friction: 0.0,
            fly_friction: 3.0,
            water_friction: 3.0,
            walk_friction_alt: 10.0,
            air_control_forward: 1.0,
            air_control_right: 1.0,
            velocity_scale: Vec3::ONE,
            min_ground_separation: 10.0,
            max_step_height: 0.0,
            max_jump_height: 0.0,
            jump_count: 0,
            max_jumps: 2,
            extra_jumps: 0,
            jump_time: -1,
            jump_held_since_jump: false,
            jump_held_left_ground: false,
            last_double_jump_time: 0,
            double_jump_interrupted: false,
            double_jump: DoubleJumpState {
                height: 0.0,
                speed_scale: 0.0,
                air_english_scale: 0.0,
                window_offset_ms: 0,
                window_duration_ms: 0,
                gravity: Vec3::ZERO,
            },
            jump_extension_start: origin,
            crouch_latched: false,
            stand_latched: false,
            crouch_enabled: true,
            last_crouch_toggle_time: 0,
            prev_up: 0,
            block_time: 0.0,
            block_normal: Vec3::ZERO,
            time: 0,
            frame_msec: 0,
            frametime: 0.0,
            step_accum: 0.0,
            step_lock: false,
            step_lock_origin: Vec3::ZERO,
            step_lock_step: 0.0,
            acceleration: Vec3::ZERO,
            prev_velocity: Vec3::ZERO,
            push_impulse: Vec3::ZERO,
            push_velocity: Vec3::ZERO,
            push_center: Vec3::ZERO,
            prev_ground_plane: false,
            prev_push_sum: Vec3::ZERO,
            ground_kind: GroundKind::None,
            ground_mover: None,
            normal_shape,
            crouch_shape,
            tun,
        }
    }

    pub fn ducked(&self) -> bool {
        self.flags & flags::DUCKED != 0
    }

    pub fn shape(&self) -> &Hull {
        if self.ducked() { &self.crouch_shape } else { &self.normal_shape }
    }

    pub fn set_speed(&mut self, walk: f32, crouch: f32) {
        self.walk_speed = walk;
        self.crouch_speed = crouch;
    }

    pub fn set_double_jump(&mut self, dj: DoubleJumpState) {
        self.double_jump = dj;
    }

    // ---- MovePlayer ------------------------------------------------------------------------

    pub fn move_player(&mut self, world: &World, cmd: UserCmd, msec: i32, time: i32) {
        self.prev_up = self.cmd.up;
        self.cmd = cmd;
        self.view_angles = cmd.angles;
        self.flags &= !flags::PER_FRAME;
        self.step_accum = 0.0;
        self.walking = false;
        self.ground_plane = false;
        self.time = time;
        self.frame_msec = msec;
        self.player_speed = self.walk_speed;
        self.frametime = msec as f32 * 0.001;
        if self.cmd.up < k::JUMP_RELEASE {
            self.flags &= !flags::JUMP_HELD;
        }
        self.view_forward = angles_to_forward(self.view_angles);
        self.view_right = self.gravity_normal.cross(self.view_forward).normalize_or_zero();

        if self.move_type == MoveType::Normal {
            self.check_ground();
            self.resolve_ground(world);
            self.check_duck(world);
        }
        if self.timer_ms != 0 {
            if self.frame_msec < self.timer_ms {
                self.timer_ms -= self.frame_msec;
            } else {
                self.flags &= !(flags::TIME_LAND | flags::TIME_KNOCKBACK | flags::TIME_WATERJUMP);
                self.timer_ms = 0;
            }
        }
        match self.move_type {
            MoveType::Noclip => self.noclip_move(),
            MoveType::Normal => {
                if self.walking {
                    self.walk_move(world, self.ground_normal);
                } else {
                    self.air_move(world);
                }
            }
        }
        // End of MovePlayer (0x1416b0e64 tail): the set-velocity flag is dropped, the current state
        // becomes the previous one, and the pushes are cleared (vtbl+0x2d8).
        self.flags &= !flags::VELOCITY_SET;
        self.prev_velocity = self.velocity;
        self.prev_ground_plane = self.ground_plane;
        self.prev_push_sum = self.push_impulse + self.push_velocity;
        self.push_impulse = Vec3::ZERO;
        self.push_velocity = Vec3::ZERO;
    }

    /// CheckGround's ground entity and pusher (0x1416acc40): from the first contact; with no contacts
    /// the pusher is dropped but the entity number is kept.
    fn resolve_ground(&mut self, world: &World) {
        match self.contacts.first() {
            None => self.ground_mover = None,
            Some(c) => match world.cm_of_contact(c.brush) {
                Some((ci, inst)) if inst.mover.is_some() => {
                    self.ground_kind = GroundKind::Entity(ci);
                    self.ground_mover = inst.mover;
                }
                _ => {
                    self.ground_kind = GroundKind::World;
                    self.ground_mover = None;
                }
            },
        }
    }

    // ---- Pushes from movers (idPush → idPhysics_Player) -------------------------------------

    /// idPhysics_Player::Translate (vtbl+0x70, 0x1416b6a70) as idPush calls it: the push becomes an
    /// impulse of `delta / frametime` the next move uses.
    pub fn translate_push(&mut self, delta: Vec3) {
        if self.frametime > f32::MIN_POSITIVE {
            self.push_impulse += delta / self.frametime;
        }
    }

    /// idPhysics_Player::Rotate (vtbl+0x1f8, 0x1416b24a0): the origin's displacement under the rotation
    /// (`rotated` = where the rotation takes it) becomes push velocity; `center` and unit `axis_dir`
    /// describe the rotation axis, whose point nearest the rotated origin is kept for the arc fix-ups.
    pub fn rotate_push(&mut self, center: Vec3, axis_dir: Vec3, rotated: Vec3) {
        if self.frametime > f32::MIN_POSITIVE {
            self.push_velocity += (rotated - self.origin) / self.frametime;
            self.push_center = center + axis_dir * (rotated - center).dot(axis_dir);
        }
    }

    /// IsGroundClipModel (vtbl+0x1f0, 0x1416b04d0): a contact on `brush` that is not a ceiling.
    pub fn touches(&self, brush: usize) -> bool {
        self.contacts.iter().any(|c| c.brush == brush && c.normal.dot(self.gravity_normal) < 0.7)
    }

    // ---- CheckGround -----------------------------------------------------------------------

    fn check_ground(&mut self) {
        if self.contacts.is_empty() {
            self.ground_plane = false;
            self.walking = false;
            self.jump_held_left_ground = self.cmd.up > k::JUMP_INPUT;
            return;
        }
        let sum: Vec3 = self.contacts.iter().map(|c| c.normal).sum();
        let n = sum.normalize_or_zero();
        self.ground_normal = n;
        self.ground_slick = false;

        let up = -self.gravity_normal;
        if self.velocity.dot(self.gravity_normal) >= 0.0 || self.velocity.dot(n) <= self.min_ground_separation {
            if up.dot(n) < collision::MIN_WALK_NORMAL {
                self.ground_plane = true;
                self.extra_jumps = 0;
                self.walking = false;
                return;
            }
            if self.water_level < 0.9 {
                self.ground_plane = true;
                self.walking = true;
                self.extra_jumps = 0;
            }
            if self.flags & flags::TIME_WATERJUMP != 0 {
                self.timer_ms = 0;
                self.flags &= !(flags::TIME_WATERJUMP | flags::TIME_LAND);
            }
            if up.dot(self.velocity) < k::HARD_LANDING_SPEED {
                self.flags |= flags::TIME_LAND;
                self.timer_ms = k::LAND_TIME_MS;
            }
            if self.jump_held_left_ground {
                self.flags |= flags::JUMP_HELD;
                self.jump_held_left_ground = false;
            }
            if self.jump_count != 0 && self.walking {
                self.jump_count = 0;
                self.jump_time = -1;
            }
            if self.tun.enable_jump_extension {
                self.jump_extension_start = self.origin;
            }
        } else {
            self.ground_plane = false;
            self.walking = false;
        }
    }

    // ---- CheckDuck -------------------------------------------------------------------------

    /// 0x1416abbd0: crouched, the crouch model is swept up by the height difference and must not hit.
    fn can_stand(&self, world: &World) -> bool {
        if !self.ducked() {
            return true;
        }
        let rise = -self.gravity_normal * (self.normal_shape.max.z - self.crouch_shape.max.z);
        world.translate(&self.crouch_shape, self.origin, self.origin + rise).fraction >= 1.0
    }

    fn check_duck(&mut self, world: &World) {
        let crouch_btn = self.cmd.has(button::CROUCH);
        if self.crouch_latched && !crouch_btn {
            self.crouch_latched = false;
        }
        if self.stand_latched && self.cmd.up == 0 {
            self.stand_latched = false;
        }
        let swimming = !self.walking && self.water_level >= 0.9;
        let ducked = self.ducked();
        let mut request = false;
        if !self.tun.crouch_toggle {
            request = if !ducked { self.cmd.up < 0 } else { self.cmd.up >= 0 };
        } else if self.cmd.up < 0 && self.prev_up >= 0 && self.crouch_enabled && self.last_crouch_toggle_time < self.time {
            request = true;
        } else if self.cmd.up < 1 {
            if ducked && !self.crouch_latched && crouch_btn {
                request = true;
            }
        } else if ducked {
            request = true;
            // Jumping out of a crouch stands up first and holds the jump.
            self.flags |= flags::JUMP_HELD;
        }

        let stand = if !self.crouch_enabled {
            self.can_stand(world)
        } else {
            if !request {
                false
            } else {
                self.last_crouch_toggle_time = self.time;
                if !ducked && !swimming {
                    self.crouch_latched = true;
                    self.flags |= flags::DUCKED;
                    false
                } else if ducked && self.can_stand(world) {
                    self.stand_latched = true;
                    true
                } else {
                    false
                }
            }
        };
        if stand {
            self.flags &= !flags::DUCKED;
        }
        if self.ducked() {
            self.player_speed = self.crouch_speed;
        }
    }

    // ---- Friction --------------------------------------------------------------------------

    fn friction(&mut self) {
        let g = self.gravity_normal;
        let mut vel = self.velocity;
        if self.walking {
            // The game adds (not subtracts) the gravity-axis component here, as Doom 3 did.
            vel += g * vel.dot(g);
        }
        let speed = vel.length();
        if speed < 1.0 {
            let along = self.velocity.dot(g);
            self.velocity = if along.abs() < 1e-5 { Vec3::ZERO } else { g * along };
            return;
        }
        let mut drop = 0.0;
        let mut horizontal_only = false;
        let control;
        if self.gravity * self.gravity_scale == Vec3::ZERO {
            control = self.tun.zero_g_friction * speed;
        } else if self.walking && self.water_level <= k::WATER_FEET {
            if self.ground_slick {
                control = f32::NAN; // slick ground adds no friction
            } else {
                let fr = if self.flags & flags::TIME_KNOCKBACK != 0 { self.walk_friction_alt } else { self.walk_friction };
                control = speed.max(k::STOP_SPEED) * fr;
            }
        } else if self.water_level > 0.0 {
            control = speed * self.water_friction * self.water_level;
        } else if self.jump_count >= 2 && !self.double_jump_interrupted {
            if self.tun.double_jump_friction > 0.0 {
                self.double_jump_friction = self.tun.double_jump_friction;
            }
            control = speed * self.double_jump_friction;
            horizontal_only = true;
        } else {
            control = speed * self.air_friction;
        }
        if !control.is_nan() {
            drop += control * self.frametime;
        }
        let scale = (speed - drop).max(0.0) / speed;
        self.velocity.x *= scale;
        self.velocity.y *= scale;
        if !horizontal_only {
            self.velocity.z *= scale;
        }
        self.derive_acceleration();
    }

    // ---- CmdScale / Accelerate -------------------------------------------------------------

    fn cmd_scale(&self, ignore_up: bool) -> f32 {
        let f = self.cmd.forward as i32;
        let r = self.cmd.right as i32;
        let u = if (!self.walking || self.cmd.up == 0) && !ignore_up { self.cmd.up as i32 } else { 0 };
        let max = f.abs().max(r.abs()).max(u.abs());
        if max < 1 {
            return 0.0;
        }
        let total = ((r * r) as f32 + (f as f32) * (f as f32) + (u * u) as f32).sqrt();
        max as f32 * self.player_speed / total * k::CMD_SCALE
    }

    fn accelerate(&mut self, wishdir: Vec3, wishspeed: f32, accel: f32) {
        let g = self.gravity_normal;
        let wd = wishdir - g * g.dot(wishdir);
        let vh = self.velocity - g * g.dot(self.velocity);
        let push = wd * wishspeed - vh;
        let len = push.length();
        if len > 0.0 {
            let can = accel * self.frametime * wishspeed;
            self.velocity += push / len * len.min(can);
        }
        // 0x1416a92e0 writes the acceleration unconditionally after the repulsion step.
        self.derive_acceleration();
    }

    /// The acceleration write every velocity-changing step ends with.
    fn derive_acceleration(&mut self) {
        self.acceleration = if self.frame_msec == 0 {
            Vec3::ZERO
        } else {
            (self.velocity - self.prev_velocity) * (1.0 / (self.frame_msec as f32 * 0.001))
        };
    }

    /// idPhysics_Player::SetLinearVelocity (vtbl+0x98, 0x1416b3e10): sets the velocity and the
    /// VELOCITY_SET flag, and the acceleration when the last frame had msec. (Actor repulsion,
    /// 0x1416a6c60, has no actors to act on here.)
    pub fn set_linear_velocity(&mut self, v: Vec3) {
        self.velocity = v;
        self.flags |= flags::VELOCITY_SET;
        if self.frame_msec != 0 {
            self.derive_acceleration();
        }
    }

    fn horizontal_axes(&self) -> (Vec3, Vec3) {
        let g = self.gravity_normal;
        let f = (self.view_forward - g * g.dot(self.view_forward)).normalize_or_zero();
        let r = (self.view_right - g * g.dot(self.view_right)).normalize_or_zero();
        (f, r)
    }

    // ---- WalkMove --------------------------------------------------------------------------

    fn walk_move(&mut self, world: &World, n: Vec3) {
        if self.check_jump() {
            self.air_move(world);
            return;
        }
        self.friction();
        let scale = self.cmd_scale(false);
        let (f, r) = self.horizontal_axes();
        let f = project_overclip(f, n).normalize_or_zero();
        let r = project_overclip(r, n).normalize_or_zero();
        let wishvel = f * self.cmd.forward as f32 + r * self.cmd.right as f32;
        let len = wishvel.length();
        let mut wishdir = wishvel.normalize_or_zero();
        let mut wishspeed = len * scale * self.speed_scale;
        if self.block_time > 0.0 {
            let d = wishdir.dot(self.block_normal);
            if d > 0.0 {
                let g = self.gravity_normal;
                wishdir = (wishdir - self.block_normal * d - g * g.dot(wishdir - self.block_normal * d)).normalize_or_zero();
                wishspeed *= (1.0 - d) * k::BLOCK_SPEED_FACTOR;
            }
            self.block_time = 0.0;
        }
        let knockback = self.ground_slick || self.flags & flags::TIME_KNOCKBACK != 0;
        let accel = if knockback { self.air_accel } else { self.walk_accel };
        self.accelerate(wishdir, wishspeed, accel);
        if knockback {
            self.velocity += self.gravity * self.gravity_scale * self.frametime;
        }
        let old = self.velocity;
        let project = !self.tun.stick_to_ground || self.velocity.normalize_or_zero().dot(n) > self.tun.stick_to_ground_dot;
        if project {
            self.velocity = project_overclip(self.velocity, n);
        }
        if old.dot(self.velocity) > 0.0 {
            let new_sq = self.velocity.length_squared();
            let old_sq = old.length_squared();
            if new_sq > 1.0 && old_sq > 1.0 {
                self.velocity *= (old_sq / new_sq).sqrt();
            }
        }
        self.slide_move(world, false, true, true);
    }

    // ---- AirMove ---------------------------------------------------------------------------

    fn air_move(&mut self, world: &World) {
        // Leaving the ground (groundPlane now clear, set in the previous state), the movers' pushes
        // become the player's own velocity (0x1416a9610 start).
        if self.push_impulse != Vec3::ZERO && !self.ground_plane && self.prev_ground_plane {
            self.velocity += self.push_impulse;
            self.push_impulse = Vec3::ZERO;
        }
        if self.push_velocity != Vec3::ZERO && !self.ground_plane && self.prev_ground_plane {
            let pv = self.push_velocity;
            let align = pv.dot(self.view_forward).abs() / pv.length().max(f32::MIN_POSITIVE);
            let r = self.origin - self.push_center;
            let p = r + pv;
            let keep = r.length() / p.length().max(f32::MIN_POSITIVE);
            self.velocity += (p * keep - r) * align;
            self.push_velocity = Vec3::ZERO;
        }
        if self.tun.enable_jump_extension {
            let d = self.origin - self.jump_extension_start;
            let dxy = (d.x * d.x + d.y * d.y).sqrt();
            let below = self.jump_extension_start.z - self.origin.z;
            if -self.tun.jump_extension_up_height < below
                && below <= self.tun.jump_extension_height
                && dxy <= self.tun.jump_extension_length
                && self.check_jump()
            {
                self.air_move(world);
                return;
            }
        }
        self.friction();
        if self.check_double_jump() {
            self.air_move(world);
            return;
        }
        if self.cmd.up > 10 {
            self.flags |= flags::JUMP_HELD;
        }
        let hspeed = (self.velocity.x * self.velocity.x + self.velocity.y * self.velocity.y).sqrt();
        self.player_speed = self.player_speed.max(hspeed);
        let scale = self.cmd_scale(true);
        let (f, r) = self.horizontal_axes();
        let g = self.gravity_normal;
        let mut wishvel = f * self.cmd.forward as f32 * self.air_control_forward + r * self.cmd.right as f32 * self.air_control_right;
        wishvel -= g * g.dot(wishvel);
        let len = wishvel.length();
        self.accelerate(wishvel.normalize_or_zero(), len * scale, self.air_accel);
        self.velocity *= self.velocity_scale;
        self.slide_move(world, true, false, false);
    }

    // ---- CheckJump / CheckDoubleJump -------------------------------------------------------

    fn jump_velocity(&self, gravity: Vec3, scale: f32, height: f32) -> Vec3 {
        let v = -gravity * (2.0 * scale * height);
        let len = v.length();
        if len <= 0.0 { Vec3::ZERO } else { v / len * len.sqrt() }
    }

    fn check_jump(&mut self) -> bool {
        if self.cmd.up <= k::JUMP_INPUT {
            return false;
        }
        if self.gravity != Vec3::ZERO && (self.flags & (flags::JUMP_HELD | flags::DUCKED) != 0 || self.stand_latched) {
            return false;
        }
        if self.jump_count >= self.max_jumps + self.extra_jumps {
            return false;
        }
        // The ground physics may forbid jumping (+0x2218 → preventPlayerJump).
        if self.ground_mover.is_some_and(|m| m.prevent_player_jump) {
            return false;
        }
        self.flags |= flags::JUMPED | flags::JUMP_HELD;
        self.ground_plane = false;
        self.walking = false;
        let g = self.gravity_normal;
        let h = self.velocity - g * g.dot(self.velocity);
        let jv = self.jump_velocity(self.gravity, self.jump_scale, self.max_jump_height);
        let s = self.tun.jump_change_direction_scalar;
        if s <= 0.0 {
            self.velocity = jv + h;
        } else {
            let (f, r) = self.horizontal_axes();
            let mut wish = f * (self.cmd.forward as f32 / 128.0) + r * (self.cmd.right as f32 / 128.0);
            wish = (wish - g * g.dot(wish)) * self.walk_speed * self.tun.lunge_horizontal_direction_scalar;
            self.velocity = (jv + wish) * s + (jv + h) * (1.0 - s);
        }
        self.derive_acceleration();
        self.jump_time = self.time;
        self.jump_held_since_jump = true;
        self.jump_count = 1;
        true
    }

    fn check_double_jump(&mut self) -> bool {
        let dj = self.double_jump;
        if self.jump_scale * dj.height <= 0.0 {
            return false;
        }
        if self.cmd.up < k::JUMP_RELEASE {
            self.jump_held_since_jump = false;
            return false;
        }
        let jumps_left = (self.jump_count < self.extra_jumps + self.max_jumps && self.max_jumps > 1) || self.tun.allow_infinite_double_jumps;
        let window_open = self.jump_time < 0 || self.jump_time + dj.window_offset_ms <= self.time;
        let cooled = self.last_double_jump_time + (self.tun.double_jump_cooldown * 1000.0) as i32 <= self.time;
        let free = self.flags & (flags::JUMP_HELD | flags::DUCKED) == 0 && !self.stand_latched;
        if self.jump_held_since_jump || !jumps_left || !window_open || !cooled || !free {
            return false;
        }
        self.flags |= flags::JUMP_HELD | flags::DOUBLE_JUMPED;
        self.last_double_jump_time = self.time;
        self.ground_plane = false;
        self.walking = false;
        let g = self.gravity_normal;
        if self.tun.double_jump_input_influence > 0.0 {
            let (f, r) = self.horizontal_axes();
            let wish = f * (self.cmd.forward as f32 * 0.0078125) + r * (self.cmd.right as f32 * 0.0078125);
            let target = if wish.x * wish.x + wish.y * wish.y >= 1.1920929e-7 {
                wish.normalize() * self.walk_speed
            } else {
                self.velocity
            };
            let w = self.tun.double_jump_input_influence;
            self.velocity = self.velocity * (1.0 - w) + target * w;
        }
        let up = self.jump_velocity(dj.gravity, self.jump_scale, dj.height) * dj.speed_scale;
        let target = up.dot(g);
        if target < self.velocity.dot(g) {
            self.velocity -= g * self.velocity.dot(g);
            self.velocity += g * target;
        }
        self.derive_acceleration();
        self.jump_count = (self.jump_count + 1).max(2);
        self.double_jump_interrupted = false;
        true
    }

    // ---- Noclip ----------------------------------------------------------------------------

    fn noclip_move(&mut self) {
        let speed = self.walk_speed;
        let f = angles_to_forward(self.view_angles);
        let r = self.view_right;
        let up = -self.gravity_normal;
        let wish = f * self.cmd.forward as f32 + r * self.cmd.right as f32 + up * self.cmd.up as f32;
        self.velocity = wish.normalize_or_zero() * speed * (wish.length() / 127.0).min(1.0);
        self.origin += self.velocity * self.frametime;
        self.contacts.clear();
    }

    // ---- SlideMove (0x1416b48f0) --------------------------------------------------------------

    /// SlideMove's push preamble (0x1416b4a54..0x1416b5800): with a push pending, a probe trace clips
    /// the velocity; riding a pusher, a downward push is lengthened 1.5x and traced; standing on the
    /// world or nothing, rotational pushes are arc-corrected. Returns the push to move with and whether
    /// the downward-push probe ran.
    fn pusher_pre_slide(&mut self, world: &World, mut push: Vec3) -> (Vec3, bool) {
        let dt = self.frametime;
        let riding = self.ground_mover.is_some_and(|m| m.is_pusher);
        let on_world = self.ground_kind == GroundKind::World;
        let no_ground = self.ground_kind == GroundKind::None;
        let mut probe = self.velocity * dt;
        let mut lift = Vec3::ZERO;
        if riding {
            probe += self.velocity.normalize_or_zero() * self.tun.pusher_walk_move_cushion;
            if push.x == 0.0 && push.y == 0.0 && push.z > 0.0 {
                lift = push * dt;
            }
        } else {
            probe -= push * dt;
        }
        let shape = self.shape().clone();
        let tr = world.translate(&shape, self.origin + lift, self.origin + lift + probe);
        let mut n = if tr.fraction < 1.0 { tr.c.normal } else { Vec3::ZERO };
        if n != Vec3::ZERO && riding {
            let d = n.dot(UP);
            if (-0.9..=0.9).contains(&d) {
                n = Vec3::new(n.x, n.y, 0.0).normalize_or_zero();
            }
        }
        if probe != Vec3::ZERO {
            if n != Vec3::ZERO {
                let d = n.dot(self.velocity);
                if d < 0.0 {
                    self.velocity -= n * d;
                }
            }
            if riding && self.ground_mover.is_some_and(|m| m.prevent_player_jump) {
                self.velocity.z = 0.0;
            }
        }
        let mut down_probe = false;
        if riding {
            if self.prev_push_sum == Vec3::ZERO {
                down_probe = push.normalize_or_zero().dot(DOWN) >= 0.95;
                if down_probe {
                    push *= 1.5;
                    let t = world.translate(&shape, self.origin, self.origin + push * dt);
                    push *= t.fraction;
                }
            }
        } else if on_world || no_ground {
            let r = self.origin - self.push_center;
            let d = r.normalize_or_zero().dot(n);
            let aligned = (-0.9..=0.9).contains(&d);
            if aligned {
                let pd = push.normalize_or_zero();
                let k = pd.dot(self.velocity);
                if k < 0.0 {
                    self.velocity += pd * -(k + k);
                }
                // (Added here and again below with the general push: the exe does both.)
                self.velocity += push;
            }
            if self.push_velocity != Vec3::ZERO && self.push_impulse == Vec3::ZERO {
                let arc = |vel: Vec3, push: Vec3, origin: Vec3, center: Vec3| {
                    let q = (vel + push) * dt + origin - center;
                    let keep = (origin - center).length() / q.length().max(f32::MIN_POSITIVE);
                    (center + q * keep - origin) / dt
                };
                if on_world {
                    if aligned {
                        push = arc(self.velocity, push, self.origin, self.push_center);
                    } else if n != Vec3::ZERO {
                        push = n * (self.velocity.length() + push.length());
                    }
                } else if aligned {
                    push = arc(self.velocity, push, self.origin, self.push_center);
                }
                self.velocity.x = 0.0;
                self.velocity.y = 0.0;
            }
        }
        (push, down_probe)
    }

    fn slide_move(&mut self, world: &World, gravity: bool, step_up: bool, step_down: bool) {
        // Movers' pushes ride along with this move and are taken back out of the velocity afterwards.
        let raw_push = self.push_impulse + self.push_velocity;
        let has_push = raw_push != Vec3::ZERO;
        let (push, down_probe) = if has_push { self.pusher_pre_slide(world, raw_push) } else { (Vec3::ZERO, false) };
        self.velocity += push;
        for c in [&mut self.velocity.x, &mut self.velocity.y, &mut self.velocity.z] {
            if c.abs() < 0.0001 {
                *c = 0.0;
            }
        }
        let up = if step_up && self.ground_plane { self.max_step_height } else { 0.0 };
        let down = if step_down { self.max_step_height } else { 0.0 };
        let dt = self.frametime;
        let delta = self.velocity * dt;
        // Gravity enters as the gravity normal lengthened by gravity·dt² (the slide splits it into a half step).
        let gvec = if gravity { self.gravity * self.gravity_scale * dt * dt + self.gravity_normal } else { self.gravity_normal };
        let res = collision::slide_move_contacts(world, self.shape(), self.origin, delta, gvec, up, up + down);
        let mut endpos = res.endpos;
        let mut step = res.step;

        // Standing still, a sub-unit step that changes from frame to frame is cancelled (0x21e4 lock).
        // `lock_event` is the exe's flag reused by the dislodge test (set when the lock engages or is
        // dropped by moving).
        let mut lock_event = false;
        if delta != Vec3::ZERO || res.fraction != 1.0 || step.abs() <= 0.0 || step.abs() >= 1.0 {
            if self.step_lock {
                self.step_lock = false;
                lock_event = true;
            }
        } else if !self.step_lock {
            self.step_lock = true;
            self.step_lock_origin = self.origin;
            self.step_lock_step = step;
            lock_event = true;
        } else if self.origin.x == self.step_lock_origin.x && self.origin.y == self.step_lock_origin.y && step != self.step_lock_step {
            endpos = self.origin;
            step = 0.0;
            self.step_lock_step = 0.0;
        } else {
            self.step_lock = false;
        }

        let start = self.origin;
        self.origin = endpos;
        if self.flags & flags::VELOCITY_SET == 0 {
            let used = self.velocity;
            self.velocity = res.displacement / dt;
            if has_push {
                // The push is not the player's own velocity: take it back out; an unobstructed move keeps
                // the velocity it had, and one turned around stops horizontally.
                let own = used - push;
                self.velocity -= push;
                if endpos == start + delta {
                    self.velocity = own;
                }
                if own.dot(self.velocity) <= 0.0 {
                    self.velocity.x = 0.0;
                    self.velocity.y = 0.0;
                }
            }
            if has_push {
                // Pinned against something while a pusher moves: dislodge (pm_pusherDislodgeMultiplier).
                let miss = start + delta - endpos;
                let mut blocked = false;
                if miss.length_squared() >= 1.0 {
                    let dir = push.normalize_or_zero();
                    blocked = world.contacts(self.shape(), self.origin, dir, push.length()).iter().any(|c| dir.dot(c.normal) < -0.1);
                }
                let _ = down_probe; // (the crush callback +0x21b8 is skipped for downward-push probes)
                if self.ground_mover.is_some_and(|m| m.dislodge_player) && (res.fraction < 1.0 || lock_event || blocked) {
                    let kick = push + UP * (push.length() * self.tun.pusher_dislodge_multiplier);
                    self.velocity += kick;
                    self.origin += kick * dt;
                }
            }
            // Landing inside the contact distance without the slide touching anything: drop the speed into the ground.
            if self.contacts.is_empty() && res.first.kind == ContactKind::None && !res.contacts.is_empty() {
                let n = res.contacts.iter().map(|c| c.normal).sum::<Vec3>().normalize_or_zero();
                self.velocity -= n * self.velocity.dot(n);
            }
        }
        if step > 1.0 {
            self.flags |= flags::STEPPED_UP;
            self.step_accum += step;
        } else if step < -1.0 {
            self.flags |= flags::STEPPED_DOWN;
            self.step_accum += step;
        }
        self.contacts = if res.contacts.is_empty() {
            // "SlideMoveContacts2": one more query along the unnormalized gravity vector.
            world.contacts(self.shape(), self.origin, gvec, 1.0)
        } else {
            res.contacts
        };
        if res.first.kind != ContactKind::None {
            self.check_ground();
            self.resolve_ground(world);
        }
        // (0x1416b2270, with push velocity: local-origin bookkeeping for bound players; nothing here.)
        self.derive_acceleration();
    }
}
