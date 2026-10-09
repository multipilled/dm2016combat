//! `damageParms` of damage decls and the game's distance falloff and radius (splash) damage scale.

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decldb::DeclDb;

/// `idDamageParms` fields used by weapons (ctor defaults 0x1406e42a0: min/max 1, selfDamageScale 1,
/// selfDamage true, expansionSpeed -1).
#[derive(Debug, Clone, PartialEq)]
pub struct DamageDef {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub point_blank: f32,
    pub start_pb: f32,
    pub end_pb: f32,
    pub start_falloff: f32,
    pub end_falloff: f32,
    pub radius: f32,
    pub radius_inner: f32,
    pub radius_outer_strength: f32,
    pub volume_height: f32,
    pub expansion_speed: f32,
    pub self_damage_scale: f32,
    pub self_damage: bool,
    pub player_damage_scale: f32,
    pub knock_back: i32,
    pub knock_up: i32,
    pub self_knockback_scale: f32,
}

impl Default for DamageDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            min: 1.0,
            max: 1.0,
            point_blank: 0.0,
            start_pb: 0.0,
            end_pb: 0.0,
            start_falloff: 0.0,
            end_falloff: 0.0,
            radius: 0.0,
            radius_inner: 0.0,
            radius_outer_strength: 0.0,
            volume_height: 0.0,
            expansion_speed: -1.0,
            self_damage_scale: 1.0,
            self_damage: true,
            player_damage_scale: 1.0,
            knock_back: 0,
            knock_up: 0,
            self_knockback_scale: 1.0,
        }
    }
}

/// g_radiusDamageMultiplier / g_radiusDamageRadiusMultiplier defaults.
pub const RADIUS_DAMAGE_MULTIPLIER: f32 = 1.0;
pub const RADIUS_DAMAGE_RADIUS_MULTIPLIER: f32 = 1.0;

impl DamageDef {
    pub(crate) fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("damage", name).with_context(|| format!("damage decl {name}"))?;
        let p = b.block("edit.damageParms").cloned().unwrap_or_default();
        let d = Self::default();
        let f = |k: &str, def: f32| p.f32(k).unwrap_or(def);
        let flag = |k: &str, def: bool| p.path(k).and_then(|v| v.as_bool()).unwrap_or(def);
        Ok(Self {
            name: name.to_string(),
            min: f("minDamage", d.min),
            max: f("maxDamage", d.max),
            point_blank: f("pointBlankDamage", d.point_blank),
            start_pb: f("startPBFalloffDistance", 0.0),
            end_pb: f("endPBFalloffDistance", 0.0),
            start_falloff: f("startFalloffDistance", 0.0),
            end_falloff: f("endFalloffDistance", 0.0),
            radius: f("radius", 0.0),
            radius_inner: f("radiusInner", 0.0),
            radius_outer_strength: f("radiusOuterDamageStrength", 0.0),
            volume_height: f("volumeHeight", 0.0),
            expansion_speed: f("expansionSpeed", d.expansion_speed),
            self_damage_scale: f("selfDamageScale", d.self_damage_scale),
            self_damage: flag("selfDamage", d.self_damage),
            player_damage_scale: f("playerDamageScale", d.player_damage_scale),
            knock_back: f("knockBack", 0.0) as i32,
            knock_up: f("knockUp", 0.0) as i32,
            self_knockback_scale: f("selfKnockbackScale", d.self_knockback_scale),
        })
    }

    /// The game's damage-by-distance routine (0x1406e4a10, deterministic).
    pub fn at_distance(&self, dist: f32) -> f32 {
        let d2 = dist * dist;
        let snap = |x: f32| if x.abs() <= 1e-18 { 0.0 } else { x };
        if self.point_blank > self.max && self.end_pb > self.start_pb {
            if d2 < self.start_pb * self.start_pb {
                return self.point_blank;
            }
            if d2 < self.end_pb * self.end_pb {
                let t = (d2.sqrt() - self.start_pb) / (self.end_pb - self.start_pb);
                return (1.0 - t) * snap(self.point_blank) + t * snap(self.max);
            }
        }
        if self.max > self.min && self.end_falloff > self.start_falloff {
            if self.start_falloff * self.start_falloff > d2 {
                return self.max;
            }
            if d2 >= self.end_falloff * self.end_falloff {
                return self.min;
            }
            let t = (d2.sqrt() - self.start_falloff) / (self.end_falloff - self.start_falloff);
            return (1.0 - t) * snap(self.max) + t * snap(self.min);
        }
        self.max
    }

    /// Splash radius after g_radiusDamageRadiusMultiplier (at least 1 unit).
    pub fn splash_radius(&self) -> f32 {
        let r = RADIUS_DAMAGE_RADIUS_MULTIPLIER * self.radius;
        if 1.0 <= r { r } else { 1.0 }
    }

    /// Radius damage scale for a target whose bounds are `dist_sq` (squared) away from the explosion
    /// (0x1403861e0). Falloff is linear in squared distance between radiusInner and radius, down to
    /// radiusOuterDamageStrength; `None` outside the radius.
    pub fn splash_scale(&self, dist_sq: f32) -> Option<f32> {
        let r = self.splash_radius();
        let mut inner = self.radius_inner;
        if r <= inner {
            inner = r;
        }
        if inner <= 0.0 {
            inner = 0.0;
        }
        let mut outer = self.radius_outer_strength;
        if 1.0 <= outer {
            outer = 1.0;
        }
        if outer <= 0.0 {
            outer = 0.0;
        }
        let r2 = r * r;
        let i2 = inner * inner;
        if dist_sq > r2 {
            return None;
        }
        let s = if r2 <= i2 || dist_sq <= i2 { 1.0 } else { 1.0 - ((dist_sq - i2) / (r2 - i2)) * (1.0 - outer) };
        Some(RADIUS_DAMAGE_MULTIPLIER * s)
    }

    /// Squared distance from `at` to an axis-aligned box (0 inside), as the radius damage measures it;
    /// with volumeHeight the box must overlap the slab and only the horizontal distance counts.
    pub fn splash_dist_sq(&self, at: Vec3, mins: Vec3, maxs: Vec3) -> Option<f32> {
        let axis = |p: f32, lo: f32, hi: f32| if p < lo { lo - p } else if p > hi { hi - p } else { 0.0 };
        let (dx, dy) = (axis(at.x, mins.x, maxs.x), axis(at.y, mins.y, maxs.y));
        if self.volume_height != 0.0 {
            if maxs.z < at.z - self.volume_height || at.z + self.volume_height < mins.z {
                return None;
            }
            return Some(dx * dx + dy * dy);
        }
        let dz = axis(at.z, mins.z, maxs.z);
        Some(dx * dx + dy * dy + dz * dz)
    }

    /// Splash damage on a box (scale times this decl's damage at distance 0); `None` when out of range.
    pub fn splash_on_box(&self, at: Vec3, mins: Vec3, maxs: Vec3) -> Option<f32> {
        let d2 = self.splash_dist_sq(at, mins, maxs)?;
        self.splash_scale(d2).map(|s| s * self.at_distance(0.0))
    }

    /// Splash damage at a straight-line distance (point target).
    pub fn splash_at(&self, dist: f32) -> f32 {
        self.splash_scale(dist * dist).map(|s| s * self.at_distance(0.0)).unwrap_or(0.0)
    }

    /// Delay before an expanding shockwave (expansionSpeed > 0, g_damage_useExpansionSpeed 1) reaches
    /// something `dist` away; 0 for instant damage.
    pub fn expansion_delay_ms(&self, dist: f32) -> f32 {
        if self.expansion_speed > 0.0 { dist / self.expansion_speed * 1000.0 } else { 0.0 }
    }
}
