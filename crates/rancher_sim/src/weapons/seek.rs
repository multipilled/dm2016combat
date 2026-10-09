//! idProjectile seeking (the RL lock-on rockets): seekParms_t from the projectile entityDef, the seek start
//! (SetSeekTarget 0x140f3e610, entity targets) and the per-frame update (UpdateSeek 0x140f3f8f0 -> the target /
//! wander logic 0x140f40120 -> the steering 0x140f3bdb0). gamedata/re/MODS.md section 8g.

use glam::Vec3;
use idres::decl::Block;

use super::GameRng;

/// seekState_t (reflection enum table 0x14352b928).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeekState {
    None = 0,
    /// The seek delay: steer at a point jittered around the target, re-picked every delayMin..MaxTargetDurationMS.
    Wander = 1,
    #[default]
    Seek = 2,
    /// Inside explodeRange: the projectile detonates (UpdateSeek's state-3 branch).
    Explode = 3,
}

/// What the seek code reads from its target entity each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeekTarget {
    /// GetTargetPoint(currentSeekPoint) (vslot +0x710; AIMPOINT_CENTER by default).
    pub aim_point: Vec3,
    /// Physics linear velocity (vslot +0xa8; leadTarget only).
    pub velocity: Vec3,
    /// vslot +0x6c8 false (alive): a dead target ends the seek.
    pub alive: bool,
}

/// The runtime half of seekParms_t for one projectile (the rest stays in the decl's SeekParms). INTERIM: lookForTarget
/// (a projectile acquiring its own target in seekConeDegs), seekTag, leadTarget's target velocity and
/// maxHeadingDeviationDegs are not ported.
#[derive(Debug, Clone, PartialEq)]
pub struct Seeker {
    /// target (+0x10): the entity sought; None = the seek ended (seekState NONE).
    pub target: Option<u32>,
    /// +0x48 curAngularAccel, +0x4c angularAccelAdjust, +0x50 overrideAngularAccel (weapon mods; 0 here).
    pub cur_angular_accel: f32,
    pub angular_accel_adjust: f32,
    pub override_angular_accel: f32,
    /// +0x54 curAngularVel (deg/s), +0x5c overrideMaxAngularVel (0 here).
    pub cur_angular_vel: f32,
    pub override_max_angular_vel: f32,
    /// +0x84 delayTargetPos, +0x90 delayNextTargetTimeMS (game ticks).
    pub delay_target_pos: Vec3,
    pub delay_next_target_time: i32,
    /// +0xa0 startTimeMS (game ticks), +0xa4 seekState.
    pub start_time: i32,
    pub state: SeekState,
}

/// RandomRange 0x140311b90: min when min >= max, else min + rand15 % (max - min + 1).
fn random_range(rng: &mut GameRng, min: i32, max: i32) -> i32 {
    if min >= max {
        return min;
    }
    min + (rng.next15() as i32) % (max - min + 1)
}

/// idMath::InvSqrt normalise with the FLT_MIN clamp.
fn normalize(v: Vec3) -> (Vec3, f32) {
    let l2 = v.length_squared();
    let inv = 1.0 / l2.max(f32::MIN_POSITIVE).sqrt();
    (v * inv, inv * l2)
}

impl Seeker {
    /// SetSeekTarget 0x140f3e610 for an entity target (`center` = its physics bounds centre) at game time `now`:
    /// curAngularVel 0, curAngularAccel randomised in (minAngularAccel, angularAccel) when 0 < min < accel,
    /// startTimeMS = now + delayMin..MaxMS; with a delay the seek starts in WANDER at the centre jittered by
    /// +-delayTargetDist per axis, the next jitter after delayMin..MaxTargetDurationMS. None without canSeek.
    pub fn start(p: &SeekParms, target: u32, center: Vec3, now: i32, rng: &mut GameRng) -> Option<Self> {
        if !p.can_seek {
            return None;
        }
        let accel = if p.min_angular_accel <= 0.0 || p.angular_accel <= p.min_angular_accel {
            p.angular_accel
        } else {
            rng.next01() * (p.angular_accel - p.min_angular_accel) + p.min_angular_accel
        };
        let mut delay = p.delay_min_ms;
        if delay < p.delay_max_ms {
            delay += (rng.next15() as i32) % (p.delay_max_ms - delay + 1);
        }
        let mut s = Self {
            target: Some(target),
            cur_angular_accel: accel,
            angular_accel_adjust: 0.0,
            override_angular_accel: 0.0,
            cur_angular_vel: 0.0,
            override_max_angular_vel: 0.0,
            delay_target_pos: Vec3::ZERO,
            delay_next_target_time: 0,
            start_time: now + delay,
            state: SeekState::Seek,
        };
        if 0 < delay {
            s.state = SeekState::Wander;
            let r = p.delay_target_dist;
            let mut d = center;
            for i in 0..3 {
                d[i] += rng.next01() * (r - -r) + -r;
            }
            s.delay_target_pos = d;
            let mut t = p.delay_min_target_duration_ms;
            if t < p.delay_max_target_duration_ms {
                t += (rng.next15() as i32) % (p.delay_max_target_duration_ms - t + 1);
            }
            s.delay_next_target_time = now + t;
        }
        Some(s)
    }

    /// One UpdateSeek (0x140f3f8f0) for a projectile at `origin` flying at `vel`: the target / wander / explode
    /// logic (0x140f40120) and the steering (0x140f3bdb0). Returns the new velocity (None = unchanged).
    /// `dt` is the frame in seconds (gameLocal vslot +0x240), `now` the game time in ticks.
    pub fn update(&mut self, p: &SeekParms, dt: f32, now: i32, origin: Vec3, vel: Vec3, target: Option<SeekTarget>, rng: &mut GameRng) -> Option<Vec3> {
        self.target?;
        let Some(t) = target.filter(|t| t.alive) else {
            // A dead or removed target ends the seek (target cleared, seekState NONE).
            self.target = None;
            self.state = SeekState::None;
            return None;
        };
        let aim = t.aim_point;
        let d2 = (origin - aim).length_squared();
        if p.explode_range * p.explode_range > d2 {
            self.state = SeekState::Explode;
            return None;
        }
        if !matches!(self.state, SeekState::Wander | SeekState::Seek) {
            return None;
        }
        // Angular velocity: + dt * (overrideAngularAccel > 0 ? it : curAngularAccel), clamped to
        // (overrideMaxAngularVel > 0 ? it : maxAngularVel); curAngularAccel drifts by angularAccelAdjust.
        let rate = if 0.0 < self.override_angular_accel { self.override_angular_accel } else { self.cur_angular_accel };
        self.cur_angular_accel += dt * self.angular_accel_adjust;
        self.cur_angular_vel += dt * rate;
        let cap = if 0.0 < self.override_max_angular_vel { self.override_max_angular_vel } else { p.max_angular_vel };
        if self.cur_angular_vel > cap {
            self.cur_angular_vel = cap;
        }
        let goal = if self.state == SeekState::Wander {
            if now > self.start_time || p.delay_min_dist * p.delay_min_dist > d2 {
                // Into SEEK: the turn rate restarts from 0 with a fresh angular acceleration; this frame still
                // steers at the wander point.
                self.state = SeekState::Seek;
                self.cur_angular_vel = 0.0;
                self.cur_angular_accel = if 0.0 < p.min_angular_accel && p.min_angular_accel < p.angular_accel {
                    rng.next01() * (p.angular_accel - p.min_angular_accel) + p.min_angular_accel
                } else {
                    p.angular_accel
                };
            } else if now > self.delay_next_target_time {
                // A new wander point around the aim point; the jitter radius shrinks with d^2 / ramp^2 inside
                // delayTargetRampToZeroDist.
                self.delay_target_pos = aim;
                let mut r = p.delay_target_dist;
                let ramp = p.delay_target_ramp_to_zero_dist;
                if 0.0 < ramp {
                    let f = d2 / (ramp * ramp);
                    if 1.0 > f {
                        if r.abs() <= 1e-18 {
                            r = 0.0;
                        }
                        r = r * f + (1.0 - f) * 0.0;
                    }
                }
                let lo = -r;
                let span = r - lo;
                for i in 0..3 {
                    self.delay_target_pos[i] += rng.next01() * span + lo;
                }
                self.delay_next_target_time = random_range(rng, p.delay_min_target_duration_ms, p.delay_max_target_duration_ms) + now;
            }
            self.delay_target_pos
        } else {
            aim
        };
        steer(dt, origin, vel, goal, t.velocity, self.cur_angular_vel, p.lead_target)
    }
}

/// The steering 0x140f3bdb0: turn the velocity toward `goal` (led by `target_vel * dist / speed` with
/// leadTarget) by at most `ang_vel * dt` degrees, keeping the speed. None when already within 0.0001
/// (squared) of the goal direction.
pub fn steer(dt: f32, origin: Vec3, vel: Vec3, goal: Vec3, target_vel: Vec3, ang_vel: f32, lead: bool) -> Option<Vec3> {
    let (uv, speed) = normalize(vel);
    let mut aim = goal;
    if lead && 0.0 < speed {
        let (_, dist) = normalize(origin - goal);
        aim += target_vel * (dist * (1.0 / speed));
    }
    let (dir, _) = normalize(aim - origin);
    if (dir - uv).length_squared() <= 0.0001 {
        return None;
    }
    let dot = uv.dot(dir);
    let ang = if dot <= -1.0 {
        std::f32::consts::PI
    } else if dot >= 1.0 {
        0.0
    } else {
        dot.acos()
    };
    let max_turn = dt * ang_vel;
    let ang_deg = ang * 57.295776;
    // Axis dir x uV (normalised); antiparallel falls back to (1, 0, 0) / (dir.y, -dir.x, 0).
    let cross = dir.cross(uv);
    let (mut axis, len) = normalize(cross);
    if 0.001 > len {
        axis = if dir.x == dir.y { Vec3::X } else { normalize(Vec3::new(dir.y, -dir.x, 0.0)).0 };
    }
    // 0x1402ed980 rotates by idRotation(axis, -(-turn)); with idRotation's transposed product that is a
    // right-handed turn by -turn about dir x uV, i.e. toward `dir`.
    let turn = ang_deg.min(max_turn);
    let r = rotate(uv, axis, -turn);
    Some(r * speed)
}

/// Right-handed rotation of `v` about the unit `axis` by `deg` degrees (Rodrigues). INTERIM: the exe goes through
/// idRotation::ToMat3 (quaternion -> matrix, 0x140285860); same rotation, rounding may differ in the last ulp.
fn rotate(v: Vec3, axis: Vec3, deg: f32) -> Vec3 {
    let (s, c) = deg.to_radians().sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}

/// seekParms_t (0xe0, reflection 0x1430a0c90; idProjectile +0x4290). Defaults from its ctor 0x140f32b20.
#[derive(Debug, Clone, PartialEq)]
pub struct SeekParms {
    /// +0 canSeek, +1 lookForTarget.
    pub can_seek: bool,
    pub look_for_target: bool,
    /// +4 seekConeDegs (0), +8 seekConeDist (4000).
    pub seek_cone_degs: f32,
    pub seek_cone_dist: f32,
    /// +0x40 angularAccel, +0x44 minAngularAccel (deg/s^2; randomised in (min, accel) when 0 < min < accel).
    pub angular_accel: f32,
    pub min_angular_accel: f32,
    /// +0x58 maxAngularVel (deg/s).
    pub max_angular_vel: f32,
    /// +0x60 disableSeekDistance, +0x64 minDist.
    pub disable_seek_distance: f32,
    pub min_dist: f32,
    /// +0x68 delayMinMS / +0x6c delayMaxMS (raw game ticks before the real target is sought).
    pub delay_min_ms: i32,
    pub delay_max_ms: i32,
    /// +0x70 delayTargetDist (0), +0x74 delayTargetRampToZeroDist (1000).
    pub delay_target_dist: f32,
    pub delay_target_ramp_to_zero_dist: f32,
    /// +0x78 / +0x7c delayMin/MaxTargetDurationMS (500 / 1000).
    pub delay_min_target_duration_ms: i32,
    pub delay_max_target_duration_ms: i32,
    /// +0x80 delayMinDist (5).
    pub delay_min_dist: f32,
    /// +0x94 leadTarget.
    pub lead_target: bool,
    /// +0xa8 explodeRange (20), +0xac ignoreCollisionsWithTarget.
    pub explode_range: f32,
    pub ignore_collisions_with_target: bool,
    /// +0xd8 calculateSeekPoint (true), +0xd9 canSeekNonActors, +0xdc maxHeadingDeviationDegs.
    pub calculate_seek_point: bool,
    pub can_seek_non_actors: bool,
    pub max_heading_deviation_degs: f32,
}

impl Default for SeekParms {
    fn default() -> Self {
        Self {
            can_seek: false,
            look_for_target: false,
            seek_cone_degs: 0.0,
            seek_cone_dist: 4000.0,
            angular_accel: 0.0,
            min_angular_accel: 0.0,
            max_angular_vel: 0.0,
            disable_seek_distance: 0.0,
            min_dist: 0.0,
            delay_min_ms: 0,
            delay_max_ms: 0,
            delay_target_dist: 0.0,
            delay_target_ramp_to_zero_dist: 1000.0,
            delay_min_target_duration_ms: 500,
            delay_max_target_duration_ms: 1000,
            delay_min_dist: 5.0,
            lead_target: false,
            explode_range: 20.0,
            ignore_collisions_with_target: false,
            calculate_seek_point: true,
            can_seek_non_actors: false,
            max_heading_deviation_degs: 0.0,
        }
    }
}

impl SeekParms {
    pub fn from_block(b: &Block) -> Self {
        let d = Self::default();
        let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
        let flag = |k: &str, def: bool| b.path(k).and_then(|v| v.as_bool()).unwrap_or(def);
        Self {
            can_seek: flag("canSeek", d.can_seek),
            look_for_target: flag("lookForTarget", d.look_for_target),
            seek_cone_degs: f("seekConeDegs", d.seek_cone_degs),
            seek_cone_dist: f("seekConeDist", d.seek_cone_dist),
            angular_accel: f("angularAccel", d.angular_accel),
            min_angular_accel: f("minAngularAccel", d.min_angular_accel),
            max_angular_vel: f("maxAngularVel", d.max_angular_vel),
            disable_seek_distance: f("disableSeekDistance", d.disable_seek_distance),
            min_dist: f("minDist", d.min_dist),
            delay_min_ms: f("delayMinMS", d.delay_min_ms as f32) as i32,
            delay_max_ms: f("delayMaxMS", d.delay_max_ms as f32) as i32,
            delay_target_dist: f("delayTargetDist", d.delay_target_dist),
            delay_target_ramp_to_zero_dist: f("delayTargetRampToZeroDist", d.delay_target_ramp_to_zero_dist),
            delay_min_target_duration_ms: f("delayMinTargetDurationMS", d.delay_min_target_duration_ms as f32) as i32,
            delay_max_target_duration_ms: f("delayMaxTargetDurationMS", d.delay_max_target_duration_ms as f32) as i32,
            delay_min_dist: f("delayMinDist", d.delay_min_dist),
            lead_target: flag("leadTarget", d.lead_target),
            explode_range: f("explodeRange", d.explode_range),
            ignore_collisions_with_target: flag("ignoreCollisionsWithTarget", d.ignore_collisions_with_target),
            calculate_seek_point: flag("calculateSeekPoint", d.calculate_seek_point),
            can_seek_non_actors: flag("canSeekNonActors", d.can_seek_non_actors),
            max_heading_deviation_degs: f("maxHeadingDeviationDegs", d.max_heading_deviation_degs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// projectile_ent/zion/player/sp/rocket_launcher_lockon seekParms.
    fn lockon() -> SeekParms {
        SeekParms {
            can_seek: true,
            look_for_target: true,
            seek_cone_degs: 5.0,
            angular_accel: 2300.0,
            min_angular_accel: 1700.0,
            max_angular_vel: 900.0,
            delay_min_ms: 100000,
            delay_max_ms: 100000,
            delay_target_dist: 750.0,
            delay_target_ramp_to_zero_dist: 3000.0,
            delay_min_target_duration_ms: 75,
            delay_max_target_duration_ms: 100,
            delay_min_dist: 500.0,
            explode_range: 0.0,
            ..SeekParms::default()
        }
    }

    #[test]
    fn steer_turns_at_most_rate_times_dt_and_keeps_speed() {
        let v = Vec3::new(1000.0, 0.0, 0.0);
        // Goal 90 degrees to the left; 90 deg/s for 0.1 s = 9 degrees.
        let out = steer(0.1, Vec3::ZERO, v, Vec3::new(0.0, 500.0, 0.0), Vec3::ZERO, 90.0, false).unwrap();
        assert!((out.length() - 1000.0).abs() < 0.05);
        let ang = out.y.atan2(out.x).to_degrees();
        assert!((ang - 9.0).abs() < 1e-3, "{ang}");
        // Small remaining angle: lands on the goal direction.
        let out = steer(0.1, Vec3::ZERO, v, Vec3::new(500.0, 10.0, 0.0), Vec3::ZERO, 900.0, false).unwrap();
        assert!((out.normalize() - Vec3::new(500.0, 10.0, 0.0).normalize()).length() < 1e-4);
        // Already aligned: unchanged.
        assert!(steer(0.1, Vec3::ZERO, v, Vec3::new(500.0, 0.0, 0.0), Vec3::ZERO, 900.0, false).is_none());
    }

    #[test]
    fn lockon_rockets_wander_then_seek_inside_delay_min_dist() {
        let p = lockon();
        let mut rng = GameRng(0x1234_5678);
        let mut s = Seeker::start(&p, 9, Vec3::new(2000.0, 0.0, 40.0), 1000, &mut rng).unwrap();
        // delayMin == delayMax: no roll, 100000 ticks; WANDER with the jitter inside +-750.
        assert_eq!(s.state, SeekState::Wander);
        assert_eq!(s.start_time, 101_000);
        assert!((s.delay_target_pos - Vec3::new(2000.0, 0.0, 40.0)).abs().max_element() <= 750.0);
        assert!((1000 + 75..=1000 + 100).contains(&s.delay_next_target_time));
        assert!((1700.0..=2300.0).contains(&s.cur_angular_accel));
        let t = SeekTarget { aim_point: Vec3::new(2000.0, 0.0, 50.0), velocity: Vec3::ZERO, alive: true };
        let mut pos = Vec3::new(0.0, 0.0, 60.0);
        let mut vel = Vec3::new(2500.0, 0.0, 0.0);
        let mut now = 1000;
        let mut seek_at = None;
        for _ in 0..120 {
            now += 16;
            if let Some(v) = s.update(&p, 0.016, now, pos, vel, Some(t), &mut rng) {
                vel = v;
            }
            // The turn rate is capped at maxAngularVel.
            assert!(s.cur_angular_vel <= 900.0);
            if seek_at.is_none() && s.state == SeekState::Seek {
                seek_at = Some((pos - t.aim_point).length());
            }
            pos += vel * 0.016;
            if (pos - t.aim_point).length() < 30.0 {
                break;
            }
        }
        let d = seek_at.expect("switched to SEEK");
        assert!(d < 500.0 + 2500.0 * 0.016 + 1.0, "{d}");
        assert!((pos - t.aim_point).length() < 60.0, "reached the target: {pos:?}");
    }

    #[test]
    fn dead_target_ends_the_seek() {
        let p = lockon();
        let mut rng = GameRng(7);
        let mut s = Seeker::start(&p, 1, Vec3::new(800.0, 0.0, 0.0), 0, &mut rng).unwrap();
        let dead = SeekTarget { aim_point: Vec3::ZERO, velocity: Vec3::ZERO, alive: false };
        assert!(s.update(&p, 0.016, 16, Vec3::ZERO, Vec3::X * 100.0, Some(dead), &mut rng).is_none());
        assert_eq!((s.target, s.state), (None, SeekState::None));
    }
}
