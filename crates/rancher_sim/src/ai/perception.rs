//! Sight of one target (the player) by an AI: idAISenses' per-entity record (awareness, last visible time, last
//! known position, EM_NEWLY_AWARE) and the visual sense that fills it.
//!
//! Decoded queries (AI.md 3): Entity_GetAwareness 0x1404a85b0, Entity_IsVisible 0x1404a8b60 (visible = seen within
//! the last 1000 game ticks; game time is idTypesafeTime<int, 960>, 960 ticks per second), Entity_GetLastKnownPosition 0x1404a87d0. Settings: [`Perception`] (AI.md 2).
//! Candidate selection of the sense update (0x14047de00, AI.md 3): the eye is the midpoint of the model's
//! eyereferenceleft / right joints (fallback 50 units above the origin; INTERIM here: actorConstants eyeOffset.z),
//! the target point is the entity's origin plus its clip-bounds centre. Not-yet-focused targets must be within
//! actorPerceptionRadius (3D, squared compare), focused ones within actorRefreshRadius (-1 = no limit). The view
//! cone test is `cos(fov / 2) <= dot(unit(target - eye), viewAxis)` with fov = fieldOfView_focused for a focused
//! target, else fieldOfView_close when the distance is <= closePerceptionRadius, else fieldOfView (the cosines are
//! taken of fov * 0.5 * 0.017453292). Candidates passing get load-balanced visibility traces. INTERIM: the view axis
//! is the body's horizontal forward (the game uses its eye / view axis), "focused" = the CONFIRMED enemy, and the
//! awareness ramp (exposure accumulated while seen, CONFIRMED + EM_NEWLY_AWARE after exposedSightTime) stands in
//! for the consolidation job, which is not decoded.

use glam::Vec3;

use super::def::Perception;

/// aiAwareness_t (AIAWARE_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Awareness {
    #[default]
    Unaware = 0,
    Lost = 1,
    Suspected = 2,
    Confirmed = 3,
}

/// idAISenses' record for one entity (0x2b40 bytes in the game; the fields the brain reads).
#[derive(Debug, Clone, Default)]
pub struct SenseRecord {
    pub awareness: Awareness,
    /// Game time (ticks, 960/s) the target was last seen (+0x14); None = never.
    pub last_visible: Option<i64>,
    /// +0x18.
    pub last_known_pos: Vec3,
    /// entityModelFlags_t EM_NEWLY_AWARE: set when the target becomes CONFIRMED, cleared once the sighted
    /// reaction consumed it.
    pub newly_aware: bool,
    /// Seconds of continuous exposure toward exposedSightTime.
    pub exposure: f32,
}

/// What one sight test saw (for traces and tests).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SightTest {
    pub distance: f32,
    /// Absolute yaw between the AI's forward axis and the target, degrees.
    pub angle: f32,
    pub fov_used: f32,
    pub in_range: bool,
    pub in_fov: bool,
    pub los: bool,
}

impl SightTest {
    pub fn sees(&self) -> bool {
        self.in_range && self.in_fov && self.los
    }
}

/// Unsigned yaw angle (degrees, 0..180) between the forward axis at `yaw_deg` and `to` (xy).
pub fn yaw_off(yaw_deg: f32, to: Vec3) -> f32 {
    let target = to.y.atan2(to.x).to_degrees();
    let mut d = target - yaw_deg;
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    d.abs()
}

/// The sight test of `target` (its origin + clip-bounds centre) from an AI at `origin` facing `yaw_deg`.
pub fn sight_test(p: &Perception, rec: &SenseRecord, origin: Vec3, eye_height: f32, yaw_deg: f32, target: Vec3, los: bool) -> SightTest {
    let eye = origin + Vec3::Z * eye_height;
    let to = target - eye;
    let distance = to.length();
    let angle = yaw_off(yaw_deg, to);
    let focused = rec.awareness == Awareness::Confirmed;
    let (radius, fov_used) = if focused {
        (p.actor_refresh_radius, p.fov_focused)
    } else if distance <= p.close_radius {
        (p.actor_radius, p.fov_close)
    } else {
        (p.actor_radius, p.fov)
    };
    let in_range = radius < 0.0 || radius * radius >= to.length_squared();
    let axis = Vec3::new(yaw_deg.to_radians().cos(), yaw_deg.to_radians().sin(), 0.0);
    let cos_half = (fov_used * 0.5 * 0.017453292).cos();
    let in_fov = cos_half <= to.normalize_or_zero().dot(axis);
    SightTest { distance, angle, fov_used, in_range, in_fov, los }
}

impl SenseRecord {
    /// One sense update of `dt` seconds at game time `now` (ticks).
    pub fn update(&mut self, p: &Perception, t: &SightTest, target: Vec3, now: i64, dt: f32) {
        if t.sees() {
            self.last_visible = Some(now);
            self.last_known_pos = target;
            if self.awareness < Awareness::Confirmed {
                self.exposure += dt;
                if self.awareness < Awareness::Suspected {
                    self.awareness = Awareness::Suspected;
                }
                if self.exposure >= p.exposed_sight_time {
                    self.awareness = Awareness::Confirmed;
                    self.newly_aware = true;
                }
            }
        } else if self.awareness < Awareness::Confirmed {
            self.exposure = 0.0;
        }
    }

    /// Entity_IsVisible 0x1404a8b60: `gameTime - 1000 <= lastVisibleTime`.
    pub fn is_visible(&self, now: i64) -> bool {
        self.last_visible.is_some_and(|t| now - 1000 <= t)
    }
}
