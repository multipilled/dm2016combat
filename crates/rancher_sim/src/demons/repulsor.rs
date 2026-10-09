//! Actor repulsion: the velocity modifier every actor physics runs on its move velocity (0x1416a6c60; idPhysics_AI
//! Evaluate 0x1404cae10 after the contact clip, idPhysics_Player Accelerate 0x1416a92e0). Repulsors are
//! idRepulsor records (reflection size 0x60): {spawnId +0x0, origin +0x4 (bottom centre of a cylinder), radius +0x10,
//! height +0x14, flags +0x18, repulsorStyle +0x1c, idBox box +0x20, repulseOnlyMoving +0x5c, repulseOnlyOnGround
//! +0x5d}. The body being moved owns a list of them (physics +0x258 pointer, +0x260 count) and its own repulsion
//! cylinder (+0x204 radius, +0x208 height, +0x10 spawn id). Notes: gamedata/re/DEMONS.md section 14.
//!
//! 0x1416a6c60(phys, out, origin, velocity, onGround): out = velocity; for each repulsor in list order that
//! - is not the body's own (spawnId != phys +0x10),
//! - repulseOnlyMoving: only while |velocity.xy| >= FLT_MIN,
//! - repulseOnlyOnGround: only while the body is on the ground,
//! - RS_BOX: the origin is inside the box (0x1405cbcd0); other styles: the cylinders overlap in z
//!   (rep.z <= origin.z + body height and origin.z <= rep.z + rep.height) and the xy distance is within the radius
//!   (+ the body's radius with REPULSOR_FLAG_SUM_RADII),
//!
//! out = style(rep, origin, out): the styles chain in list order. The z of the velocity is never changed except by
//! RS_BOX.

use glam::Vec3;

use crate::handlayers::inv_sqrt;

/// FLT_MIN (the clamp of idMath::InvSqrt and the repulseOnlyMoving threshold, 0x142f8f000).
const FLT_MIN: f32 = 1.175_494_4e-38;
/// FLT_EPSILON: RS_HARD_STOP's minimum squared speed (0x1416a7180).
const FLT_EPSILON: f32 = 1.192_092_9e-7;

/// repulsorStyle_t (enum table 0x14350ec30).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepulsorStyle {
    /// 0x1416a7650.
    TangentToCircle = 0,
    /// 0x1416a7180.
    HardStop = 1,
    /// 0x1416a7330.
    SoftSpring = 2,
    /// 0x1416a7000.
    Box = 3,
    /// 0x1416a7590: SoftSpring inside the radius (3D distance), HardStop outside.
    StopSpringHybrid = 4,
}

impl RepulsorStyle {
    /// pm_enemyRepulsorStyle / pm_friendlyRepulsorStyle values ("0-TangentToCircle; 1-HardStop; 2-SoftSpring").
    pub fn from_index(i: i32) -> Option<Self> {
        Some(match i {
            0 => Self::TangentToCircle,
            1 => Self::HardStop,
            2 => Self::SoftSpring,
            3 => Self::Box,
            4 => Self::StopSpringHybrid,
            _ => return None,
        })
    }
}

/// repulsorFlags (enum table 0x14350ecc0).
pub mod flags {
    pub const TEAM_MASK: u32 = 7;
    pub const SUM_RADII: u32 = 0x200;
    pub const FRIENDLY_RADIUS: u32 = 0x400;
    pub const ENEMY_RADIUS: u32 = 0x800;
    pub const SYNC_MELEE: u32 = 0x1000;
    pub const INCAPACITATED: u32 = 0x2000;
}

/// idBox (+0x20 centre, +0x2c extents, +0x38 axis rows).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RepulsorBox {
    pub centre: Vec3,
    pub extents: Vec3,
    pub axis: [Vec3; 3],
}

impl RepulsorBox {
    /// idBox::ContainsPoint (0x1405cbcd0): |dot(p - centre, axis[i])| <= extents[i] on every axis.
    pub fn contains(&self, p: Vec3) -> bool {
        let d = p - self.centre;
        (0..3).all(|i| d.dot(self.axis[i]).abs() <= self.extents[i])
    }
}

/// One idRepulsor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Repulsor {
    /// spawnId of the entity the body is repulsed from.
    pub owner: u32,
    /// Bottom centre of the repulsion cylinder.
    pub origin: Vec3,
    pub radius: f32,
    pub height: f32,
    pub flags: u32,
    pub style: RepulsorStyle,
    pub bbox: RepulsorBox,
    pub only_moving: bool,
    pub only_on_ground: bool,
}

/// The repulsed body: physics +0x10 (owner spawn id), +0x204 (radius), +0x208 (height).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Repulsed {
    pub owner: u32,
    pub radius: f32,
    pub height: f32,
}

/// The cvars the styles read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepulsorCvars {
    /// pm_softSpringRepulsorVelMin (-5; 0x144fd1dd0 value +0x34).
    pub soft_spring_vel_min: f32,
    /// pm_softSpringRepulsorVelMax (-30; 0x144fd1e70 value +0x34).
    pub soft_spring_vel_max: f32,
    /// pm_boxRepulsorScalar (8; 0x144fd1f10 value +0x34).
    pub box_scalar: f32,
}

impl Default for RepulsorCvars {
    fn default() -> Self {
        RepulsorCvars { soft_spring_vel_min: -5.0, soft_spring_vel_max: -30.0, box_scalar: 8.0 }
    }
}

impl RepulsorCvars {
    pub fn load(cvars: &crate::config::CvarValues) -> Self {
        let d = Self::default();
        let f = |n: &str, def: f32| cvars.0.get(n).and_then(|v| idres::decl::parse_number(v)).unwrap_or(def);
        RepulsorCvars {
            soft_spring_vel_min: f("pm_softSpringRepulsorVelMin", d.soft_spring_vel_min),
            soft_spring_vel_max: f("pm_softSpringRepulsorVelMax", d.soft_spring_vel_max),
            box_scalar: f("pm_boxRepulsorScalar", d.box_scalar),
        }
    }
}

/// The pm_* cvars of the game's player repulsor list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerRepulsorCvars {
    /// pm_friendlyRepulsorRadius (48).
    pub friendly_radius: f32,
    /// pm_enemyRepulsorRadius (64): also the fallback for a negative AI repulsor radius.
    pub enemy_radius: f32,
    /// pm_friendlyRepulsorStyle (0).
    pub friendly_style: i32,
    /// pm_enemyRepulsorStyle (0).
    pub enemy_style: i32,
    /// pm_friendlyRepulseOnlyOnGround (0).
    pub friendly_only_on_ground: bool,
}

impl Default for PlayerRepulsorCvars {
    fn default() -> Self {
        PlayerRepulsorCvars { friendly_radius: 48.0, enemy_radius: 64.0, friendly_style: 0, enemy_style: 0, friendly_only_on_ground: false }
    }
}

impl PlayerRepulsorCvars {
    pub fn load(cvars: &crate::config::CvarValues) -> Self {
        let d = Self::default();
        let f = |n: &str| cvars.0.get(n).and_then(|v| idres::decl::parse_number(v));
        PlayerRepulsorCvars {
            friendly_radius: f("pm_friendlyRepulsorRadius").unwrap_or(d.friendly_radius),
            enemy_radius: f("pm_enemyRepulsorRadius").unwrap_or(d.enemy_radius),
            friendly_style: f("pm_friendlyRepulsorStyle").map_or(d.friendly_style, |v| v as i32),
            enemy_style: f("pm_enemyRepulsorStyle").map_or(d.enemy_style, |v| v as i32),
            friendly_only_on_ground: f("pm_friendlyRepulseOnlyOnGround").map_or(d.friendly_only_on_ground, |v| v != 0.0),
        }
    }
}

/// An idRepulsor as its constructor leaves it (list ctor 0x140364ff0): spawnId 0x1fffffe (none), zero origin /
/// radius / height / flags, RS_HARD_STOP, repulseOnlyMoving true, repulseOnlyOnGround false.
fn default_repulsor() -> Repulsor {
    Repulsor { owner: 0x1fff_ffe, origin: Vec3::ZERO, radius: 0.0, height: 0.0, flags: 0, style: RepulsorStyle::HardStop, bbox: RepulsorBox::default(), only_moving: true, only_on_ground: false }
}

/// One player's records in the game's player repulsor list (idGameLocal::playerRepulsors +0x513ae8, rebuilt by the
/// game update 0x1403ade50 per player slot), for a marine (the idDemonPlayer overrides of the radii / style / only
/// moving and the z offset 0x1403ae5af are MP-only): origin = the player's physics origin, height = its bounds'
/// max.z - min.z, spawnId = the player's;
/// - when pm_friendlyRepulsorRadius > 0: radius that, flags team | SUM_RADII | FRIENDLY_RADIUS, style
///   pm_friendlyRepulsorStyle (RS_STOP_SPRING_HYBRID when 0x140dd6e50(player)), repulseOnlyOnGround
///   pm_friendlyRepulseOnlyOnGround, repulseOnlyMoving left at the ctor's true;
/// - when pm_enemyRepulsorRadius > 0: radius that, flags team | SUM_RADII | ENEMY_RADIUS, style pm_enemyRepulsorStyle,
///   repulseOnlyMoving true, repulseOnlyOnGround left at the ctor's false.
///
/// Both add SYNC_MELEE when the player has a sync-melee partner and INCAPACITATED when vslot 0x178 says so (not
/// modelled). `team` = vslot 0x170.
pub fn player_records(owner: u32, origin: Vec3, height: f32, team: u32, cv: &PlayerRepulsorCvars) -> Vec<Repulsor> {
    let mut out = Vec::new();
    if cv.friendly_radius > 0.0 {
        let style = RepulsorStyle::from_index(cv.friendly_style).unwrap_or(RepulsorStyle::TangentToCircle);
        out.push(Repulsor { owner, origin, radius: cv.friendly_radius, height, flags: team | flags::SUM_RADII | flags::FRIENDLY_RADIUS, style, only_on_ground: cv.friendly_only_on_ground, ..default_repulsor() });
    }
    if cv.enemy_radius > 0.0 {
        let style = RepulsorStyle::from_index(cv.enemy_style).unwrap_or(RepulsorStyle::TangentToCircle);
        out.push(Repulsor { owner, origin, radius: cv.enemy_radius, height, flags: team | flags::SUM_RADII | flags::ENEMY_RADIUS, style, only_moving: true, ..default_repulsor() });
    }
    out
}

/// The AI's repulsors vs. players (the AI repulsor update 0x1403e1af0, per AI, list installed on its physics by
/// 0x1416a5ed0): when the AI's playerRepulsorStyle (idAIVolatile physics +0x1ce8, AI +0xdfa0) is a style (< 5), every
/// repulsor of the game's player list (gameLocal +0x513ae8, copied by 0x1403cfcc0) with the style replaced, the
/// radius replaced by aiConstants.physics.playerRepulsorRadius (a negative radius takes pm_enemyRepulsorRadius) and
/// REPULSOR_FLAG_SUM_RADII cleared. (Then come other AIs' repulsors with aiRepulsorStyle and the enemy's with
/// enemyRepulsorStyle / enemyRepulsorRadius / REPULSOR_FLAG_ENEMY_RADIUS; not ported, the range has one demon.)
pub fn ai_vs_player(players: &[Repulsor], style: i32, player_radius: f32, pm_enemy_repulsor_radius: f32) -> Vec<Repulsor> {
    let Some(style) = RepulsorStyle::from_index(style) else { return Vec::new() };
    let radius = if player_radius >= 0.0 { player_radius } else { pm_enemy_repulsor_radius };
    players.iter().map(|p| Repulsor { style, radius, flags: p.flags & !flags::SUM_RADII, ..*p }).collect()
}

/// 0x1416a6c60: the body's velocity after its repulsors.
pub fn repulse(body: &Repulsed, list: &[Repulsor], origin: Vec3, velocity: Vec3, on_ground: bool, cv: &RepulsorCvars) -> Vec3 {
    let s2 = velocity.x * velocity.x + velocity.y * velocity.y;
    let speed = inv_sqrt(s2) * s2;
    let mut out = velocity;
    for r in list {
        if (r.only_moving && speed < FLT_MIN) || r.owner == body.owner || (r.only_on_ground && !on_ground) {
            continue;
        }
        let touches = if r.style == RepulsorStyle::Box {
            r.bbox.contains(origin)
        } else {
            let radius = if r.flags & flags::SUM_RADII == 0 { r.radius } else { body.radius + r.radius };
            let (dx, dy) = (r.origin.x - origin.x, r.origin.y - origin.y);
            r.origin.z <= origin.z + body.height && origin.z <= r.origin.z + r.height && dy * dy + dx * dx <= radius * radius
        };
        if touches {
            out = match r.style {
                RepulsorStyle::TangentToCircle => tangent_to_circle(r, origin, out),
                RepulsorStyle::HardStop => hard_stop(r, origin, out),
                RepulsorStyle::SoftSpring => soft_spring(r, origin, out, cv),
                RepulsorStyle::Box => box_push(r, origin, out, cv),
                RepulsorStyle::StopSpringHybrid => {
                    let d = r.origin - origin;
                    let d2 = d.y * d.y + d.x * d.x + d.z * d.z;
                    if inv_sqrt(d2) * d2 < r.radius {
                        soft_spring(r, origin, out, cv)
                    } else {
                        hard_stop(r, origin, out)
                    }
                }
            };
        }
    }
    out
}

/// RS_TANGENT_TO_CIRCLE (0x1416a7650): steer the xy velocity toward the tangent of the repulsor's circle, keeping
/// the speed. Outside the radius r the tangent direction is the direction to the repulsor turned (toward the side
/// the velocity passes on) by asin(r * f / dist), f = 1 - (dist - r) / r (0 deg at 2r, 90 deg at r); inside, it is
/// the perpendicular. When the velocity heads closer to the repulsor than that direction does, the result is
/// normalize(v + t * speed) * speed, else the velocity is kept.
fn tangent_to_circle(r: &Repulsor, p: Vec3, v: Vec3) -> Vec3 {
    let dy = r.origin.y - p.y;
    let dx = r.origin.x - p.x;
    let s2 = v.x * v.x + v.y * v.y;
    let speed = inv_sqrt(s2) * s2;
    let d2 = dy * dy + dx * dx;
    let dist = inv_sqrt(d2) * d2;
    let side = if -dy * v.x + dx * v.y < 0.0 { -1.0 } else { 1.0 };
    let rad = r.radius;
    let (tx, ty) = if rad <= dist {
        let mut f = 1.0;
        if rad < dist {
            f = 1.0 - (dist - rad) / rad;
        }
        let sin = (rad * f) / dist;
        let c2 = (1.0 - sin * sin).abs();
        let cos = inv_sqrt(c2) * c2;
        let tx = dx * cos - dy * sin * side;
        let ty = dx * sin * side + dy * cos;
        let n = inv_sqrt(ty * ty + tx * tx);
        (tx * n, ty * n)
    } else {
        let ty = dx * side;
        let tx = dy * side * -1.0;
        let n = inv_sqrt(ty * ty + tx * tx);
        (tx * n, ty * n)
    };
    let dyn_ = dy * (1.0 / dist);
    let dxn = dx * (1.0 / dist);
    let vn = inv_sqrt(s2);
    let (mut ax, mut ay) = (0.0, 0.0);
    if dyn_ * ty + dxn * tx <= dyn_ * v.y * vn + dxn * v.x * vn {
        ay = ty * speed + 0.0;
        ax = tx * speed + 0.0;
    }
    let nx = ax + v.x;
    let ny = ay + v.y;
    let n = inv_sqrt(ny * ny + nx * nx);
    Vec3::new(nx * n * speed, ny * n * speed, v.z)
}

/// RS_HARD_STOP (0x1416a7180): when the xy velocity (|v|^2 >= FLT_EPSILON) has a component toward the repulsor,
/// it is replaced by its projection on the perpendicular of the direction to the repulsor.
fn hard_stop(r: &Repulsor, p: Vec3, v: Vec3) -> Vec3 {
    let mut out = v;
    let s2 = v.x * v.x + v.y * v.y;
    if FLT_EPSILON <= s2 {
        let vn = inv_sqrt(s2);
        let dy = r.origin.y - p.y;
        let dx = r.origin.x - p.x;
        let n = inv_sqrt(dy * dy + dx * dx);
        let (dx, dy) = (dx * n, dy * n);
        if 0.0 < v.y * vn * dy + v.x * vn * dx {
            let ny = -dy;
            let k = ny * v.x + v.y * dx;
            out.x = ny * k;
            out.y = k * dx;
        }
    }
    out
}

/// RS_SOFT_SPRING (0x1416a7330): adds the direction to the repulsor times lerp(velMin, velMax, f) (negative: away),
/// f = clamp((2r - dist) / 2r, 0, 1).
fn soft_spring(r: &Repulsor, p: Vec3, v: Vec3, cv: &RepulsorCvars) -> Vec3 {
    let r2 = r.radius + r.radius;
    let dx = r.origin.x - p.x;
    let dy = r.origin.y - p.y;
    let d2 = dy * dy + dx * dx;
    let mut f = (r2 - inv_sqrt(d2) * d2) / r2;
    if 1.0 <= f {
        f = 1.0;
    }
    if f <= 0.0 {
        f = 0.0;
    }
    let k = (cv.soft_spring_vel_max - cv.soft_spring_vel_min) * f + cv.soft_spring_vel_min;
    let n = inv_sqrt(d2);
    let py = dy * n * k;
    let px = n * dx * k;
    Vec3::new(px + v.x, py + v.y, v.z + 0.0)
}

/// RS_BOX (0x1416a7000): adds the box's first axis * pm_boxRepulsorScalar, signed by which side of the box centre
/// the body is on.
fn box_push(r: &Repulsor, p: Vec3, v: Vec3, cv: &RepulsorCvars) -> Vec3 {
    let d = p - r.bbox.centre;
    let n = inv_sqrt(d.x * d.x + d.y * d.y + d.z * d.z);
    let a = r.bbox.axis[0];
    let side = if d.y * n * a.y + d.x * n * a.x + d.z * n * a.z < 0.0 { -1.0 } else { 1.0 };
    Vec3::new(cv.box_scalar * a.x * side + v.x, cv.box_scalar * a.y * side + v.y, cv.box_scalar * a.z * side + v.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(at: Vec3, radius: f32, style: RepulsorStyle) -> Repulsor {
        Repulsor { owner: 7, origin: at, radius, height: 96.0, flags: flags::SUM_RADII, style, bbox: RepulsorBox::default(), only_moving: false, only_on_ground: false }
    }

    const BODY: Repulsed = Repulsed { owner: 1, radius: 24.0, height: 96.0 };

    fn run(r: Repulsor, v: Vec3) -> Vec3 {
        repulse(&BODY, &[r], Vec3::ZERO, v, true, &RepulsorCvars::default())
    }

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn hard_stop_keeps_only_the_tangential_part() {
        let r = rep(Vec3::new(40.0, 0.0, 0.0), 24.0, RepulsorStyle::HardStop);
        assert!(near(run(r, Vec3::new(100.0, 0.0, 5.0)), Vec3::new(0.0, 0.0, 5.0)));
        assert!(near(run(r, Vec3::new(100.0, 100.0, 0.0)), Vec3::new(0.0, 100.0, 0.0)));
        // Moving away: unchanged.
        assert_eq!(run(r, Vec3::new(-100.0, 30.0, 0.0)), Vec3::new(-100.0, 30.0, 0.0));
    }

    #[test]
    fn reach_height_owner_and_flags() {
        let v = Vec3::new(100.0, 0.0, 0.0);
        // SUM_RADII: 24 + 24 = 48 reaches 47 but not 49; without the flag only 24.
        assert_ne!(run(rep(Vec3::new(47.0, 0.0, 0.0), 24.0, RepulsorStyle::HardStop), v), v);
        assert_eq!(run(rep(Vec3::new(49.0, 0.0, 0.0), 24.0, RepulsorStyle::HardStop), v), v);
        let mut r = rep(Vec3::new(30.0, 0.0, 0.0), 24.0, RepulsorStyle::HardStop);
        r.flags = 0;
        assert_eq!(run(r, v), v);
        // Cylinders that do not overlap in z.
        assert_eq!(run(rep(Vec3::new(30.0, 0.0, 97.0), 24.0, RepulsorStyle::HardStop), v), v);
        assert_ne!(run(rep(Vec3::new(30.0, 0.0, 96.0), 24.0, RepulsorStyle::HardStop), v), v);
        assert_eq!(run(rep(Vec3::new(30.0, 0.0, -97.0), 24.0, RepulsorStyle::HardStop), v), v);
        // The body's own repulsor; only-on-ground; only-moving.
        let mut r = rep(Vec3::new(30.0, 0.0, 0.0), 24.0, RepulsorStyle::HardStop);
        r.owner = BODY.owner;
        assert_eq!(run(r, v), v);
        r.owner = 7;
        r.only_on_ground = true;
        assert_eq!(repulse(&BODY, &[r], Vec3::ZERO, v, false, &RepulsorCvars::default()), v);
        let mut r = rep(Vec3::new(30.0, 0.0, 0.0), 24.0, RepulsorStyle::SoftSpring);
        r.only_moving = true;
        assert_eq!(run(r, Vec3::new(0.0, 0.0, -3.0)), Vec3::new(0.0, 0.0, -3.0));
    }

    #[test]
    fn soft_spring_pushes_away() {
        // f = (48 - 24) / 48 = 0.5: -5 + (-30 - -5) * 0.5 = -17.5 along the direction to the repulsor.
        let got = run(rep(Vec3::new(24.0, 0.0, 0.0), 24.0, RepulsorStyle::SoftSpring), Vec3::ZERO);
        assert!(near(got, Vec3::new(-17.5, 0.0, 0.0)), "{got}");
        // dist 40: f = 8 / 48, -5 - 25 / 6.
        let got = run(rep(Vec3::new(0.0, 40.0, 0.0), 24.0, RepulsorStyle::SoftSpring), Vec3::new(10.0, 0.0, 0.0));
        assert!(near(got, Vec3::new(10.0, -5.0 - 25.0 / 6.0, 0.0)), "{got}");
    }

    #[test]
    fn tangent_to_circle_steers_around() {
        // At distance r the tangent is the perpendicular: half way between it and the velocity, same speed.
        let r = rep(Vec3::new(24.0, 0.0, 0.0), 24.0, RepulsorStyle::TangentToCircle);
        let got = run(r, Vec3::new(100.0, 0.0, 3.0));
        let s = 100.0 * std::f32::consts::FRAC_1_SQRT_2;
        assert!(near(got, Vec3::new(s, s, 3.0)), "{got}");
        // Passing on the other side turns the other way.
        let got = run(r, Vec3::new(100.0, -1.0, 0.0));
        assert!(got.y < -60.0 && (got.length() - Vec3::new(100.0, -1.0, 0.0).length()).abs() < 1e-3, "{got}");
        // Heading away: unchanged.
        assert!(near(run(r, Vec3::new(-100.0, 0.0, 0.0)), Vec3::new(-100.0, 0.0, 0.0)));
        // Near 2r (the reach) the tangent nearly points at the repulsor, so a straight approach barely turns.
        let r = rep(Vec3::new(47.999, 0.0, 0.0), 24.0, RepulsorStyle::TangentToCircle);
        let got = run(r, Vec3::new(100.0, 0.0, 0.0));
        assert!((got - Vec3::new(100.0, 0.0, 0.0)).length() < 0.01, "{got}");
    }

    #[test]
    fn player_records_and_ai_copy() {
        let cv = PlayerRepulsorCvars::default();
        let o = Vec3::new(10.0, 20.0, 1.0);
        let recs = player_records(9, o, 72.0, 0, &cv);
        assert_eq!(recs.len(), 2);
        assert_eq!((recs[0].radius, recs[0].flags, recs[0].style, recs[0].only_moving, recs[0].only_on_ground), (48.0, 0x600, RepulsorStyle::TangentToCircle, true, false));
        assert_eq!((recs[1].radius, recs[1].flags, recs[1].style, recs[1].only_moving, recs[1].only_on_ground), (64.0, 0xa00, RepulsorStyle::TangentToCircle, true, false));
        assert!(recs.iter().all(|r| r.owner == 9 && r.origin == o && r.height == 72.0));
        // The Possessed's copies: HardStop, playerRepulsorRadius 24, SUM_RADII cleared; style 5 = none.
        let ai = ai_vs_player(&recs, 1, 24.0, cv.enemy_radius);
        assert!(ai.iter().all(|r| r.style == RepulsorStyle::HardStop && r.radius == 24.0 && r.flags & flags::SUM_RADII == 0));
        assert_eq!((ai[0].flags, ai[1].flags), (0x400, 0x800));
        assert_eq!(ai_vs_player(&recs, 1, -1.0, cv.enemy_radius)[0].radius, 64.0);
        assert!(ai_vs_player(&recs, 5, 24.0, cv.enemy_radius).is_empty());
        // The AI stops 24 from the player's origin, whatever its own radius.
        let body = Repulsed { owner: 1, radius: 32.0, height: 96.0 };
        let at = |x: f32| repulse(&body, &ai, Vec3::new(o.x - x, o.y, 0.0), Vec3::new(100.0, 0.0, 0.0), true, &RepulsorCvars::default());
        assert_eq!(at(24.5), Vec3::new(100.0, 0.0, 0.0));
        assert!(near(at(23.5), Vec3::ZERO));
    }

    #[test]
    fn hybrid_and_box() {
        // 3D distance 20 < 24: spring; 30: hard stop.
        let v = Vec3::new(100.0, 0.0, 0.0);
        let got = run(rep(Vec3::new(20.0, 0.0, 0.0), 24.0, RepulsorStyle::StopSpringHybrid), v);
        assert!(got.x > 70.0 && got.x < 100.0, "{got}");
        assert!(near(run(rep(Vec3::new(30.0, 0.0, 0.0), 24.0, RepulsorStyle::StopSpringHybrid), v), Vec3::ZERO));
        let mut r = rep(Vec3::new(1000.0, 0.0, 0.0), 0.0, RepulsorStyle::Box);
        r.bbox = RepulsorBox { centre: Vec3::new(10.0, 0.0, 0.0), extents: Vec3::splat(20.0), axis: [Vec3::X, Vec3::Y, Vec3::Z] };
        assert!(near(run(r, Vec3::ZERO), Vec3::new(-8.0, 0.0, 0.0)));
        r.bbox.centre.x = 30.0;
        assert_eq!(run(r, Vec3::ZERO), Vec3::ZERO);
    }
}
