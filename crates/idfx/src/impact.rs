//! idDeclProjectileImpactEffect (`generated/decls/projectileimpacteffect/*.decl`) and the impact spawn rules.
//!
//! - idGameLocal::ProjectileImpactEffect 0x140ee7fd0: effect = table[surfaceType] (0x1406ec7d0 maps the
//!   collision surface type (SURFTYPE_*) to a member below, see `surftype_effect`); the projectile's
//!   impactEffectTable plays for every hit,
//!   and impactEffectTableLimitedBySurfType plays at most maxImpactEffectsPerSurface times per surface type per
//!   game frame (counter at game+0x513920, reset when the frame number changes).
//! - FUN_140ee8c00 -> idGameLocal::ImpactEffect 0x140ee70e0 with the trace end position (+0x38), plane normal
//!   (+0x44) and surface colour: decal index = rand % count (game idRandom at game+0x285be8) unless teamBased,
//!   then particle index = rand % count; the particle's axis is normal.ToMat3 (0x1402ee4c0: z = normal,
//!   x = (-n.y, n.x, 0)/|n.xy|, y = z cross x; degenerate -> x=(1,0,0), y=(0,s,0), z=(0,0,s)).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::{Vec3, Vec4};
use idres::decl::Block;
use idres::decldb::DeclDb;

use crate::decl::*;
use crate::particle::ParticleDecl;
use crate::Axis;

/// projectileImpactEffect_t members of idDeclProjectileImpactEffect in order; index = surface type.
pub const SURFACES: [&str; 36] = [
    "defaultEffect",
    "metalEffect",
    "stoneEffect",
    "fleshEffect",
    "woodEffect",
    "cardboardEffect",
    "liquidEffect",
    "glassEffect",
    "plasticEffect",
    "asphaltEffect",
    "dirtEffect",
    "concreteEffect",
    "foilageEffect",
    "linoleumEffect",
    "fabricEffect",
    "rubberEffect",
    "rockEffect",
    "steamPipeEffect",
    "waterPipeEffect",
    "armorEffect",
    "shieldEffect",
    "sludgeEffect",
    "bloodPoolEffect",
    "mutantFleshEffect",
    "thickPaddingEffect",
    "ricketyMetal",
    "ricketyWood",
    "mancubusArmor",
    "mancubusFlesh",
    "pinkyArmor",
    "pinkyFlesh",
    "talismanArmor",
    "smmArmor",
    "smmFlesh",
    "bloodReplacementEffect",
    "forceFieldEffect",
];

pub const SURFACE_DEFAULT: usize = 0;
pub const SURFACE_FLESH: usize = 3;

/// Surface type index from a surface name ("metal", "stone", "flesh", ...); unknown -> default.
pub fn surface_index(name: &str) -> usize {
    if name.is_empty() {
        return SURFACE_DEFAULT;
    }
    let n = name.to_ascii_lowercase();
    SURFACES.iter().position(|s| s.to_ascii_lowercase().trim_end_matches("effect") == n).unwrap_or(SURFACE_DEFAULT)
}

#[derive(Debug, Clone, Default)]
pub struct Effect {
    pub decals: Vec<String>,
    pub decal_size: f32,
    pub decal_lifetime: i32,
    pub decal_fade_in: i32,
    pub decal_fade_out: i32,
    pub decal_emissive_lifetime: i32,
    pub decal_depth: f32,
    pub decal_angle: f32,
    /// xyz diffuse tint, w threshold blending.
    pub decal_diffuse_tint: Vec4,
    /// xyz emissive colour, w emissive power.
    pub decal_emissive_tint: Vec4,
    pub decal_specular_tint: Vec4,
    /// Opacity per channel: x albedo, y specular, z smoothness, w normal.
    pub decal_opacity: Vec4,
    /// particleImpact slots; NULL entries are empty but still count in the random pick (0x140ee70e0).
    pub particles: Vec<Option<Arc<ParticleDecl>>>,
    pub sound: String,
    pub sound_local: String,
    pub persistent: bool,
    pub use_smoke: bool,
    pub bloody: bool,
    pub team_based: bool,
    /// viewShakeDecl (+0xb8, an advancedscreenviewshake decl), viewShakeStartDistance (+0xc0, ctor 0) and
    /// viewShakeDistance (+0xc4, ctor -1).
    pub view_shake: Option<String>,
    pub view_shake_start: f32,
    pub view_shake_distance: f32,
}

impl Effect {
    /// The impact's view shake for one player (ImpactEffect 0x140ee70e0 tail, after the decal and particle):
    /// with a viewShakeDecl, every player whose clip bounds touch the box pos +- viewShakeDistance
    /// (idClip entities-touching-bounds 0x140390620; INTERIM: its handling of the inverted box a -1 distance gives is not
    /// decoded, every shipped viewShakeDecl sets a distance) gets listener vslot 0x38
    /// (0x140e3acb0: the advanced shake by distance) with |pos - view| as len2 * InvSqrt(len2).
    /// `player_bounds` are the player's absolute clip bounds.
    pub fn view_shake_call(&self, pos: Vec3, player_bounds: [Vec3; 2], view: Vec3) -> Option<crate::fx::ShakeCall<'_>> {
        let decl = self.view_shake.as_deref()?;
        let r = self.view_shake_distance;
        let (lo, hi) = (pos - Vec3::splat(r), pos + Vec3::splat(r));
        // idBounds::IntersectsBounds: touching counts.
        let [bmin, bmax] = player_bounds;
        if bmax.x < lo.x || bmax.y < lo.y || bmax.z < lo.z || bmin.x > hi.x || bmin.y > hi.y || bmin.z > hi.z {
            return None;
        }
        Some(crate::fx::ShakeCall::Advanced { decl, distance: crate::fx::shake_distance(pos, view), start: self.view_shake_start, end: r })
    }
}

#[derive(Debug, Clone)]
pub struct ImpactTable {
    pub name: String,
    pub effects: Vec<Effect>,
    pub hazard_fire: Vec<Effect>,
    pub hazard_steam: Vec<Effect>,
}

impl ImpactTable {
    /// The effect for a collision surface type (0x1406ec7d0): see `surftype_effect`; SURFTYPE_FIRE / _STEAM
    /// pick from the hazard lists with the game random (rand % count when count > 1). INTERIM: the gore
    /// setting's bloodReplacementEffect swap (bss 0x144454370) is taken as off.
    pub fn for_surftype(&self, surftype: u32, rng: &mut crate::IdRandom) -> Option<&Effect> {
        let list = match surftype {
            33 => &self.hazard_fire,
            34 => &self.hazard_steam,
            t => return self.effects.get(surftype_effect(t)),
        };
        let i = if list.len() > 1 { rng.next_int() as usize % list.len() } else { 0 };
        list.get(i)
    }
}

/// SURFTYPE_* (the exe's enum order) -> projectileImpactEffect_t member index (SURFACES), per 0x1406ec7d0:
/// 1..=19 straight, then sludge, shield, bloodPool, mutantFlesh, thickPadding, forceField, ricketyMetal,
/// ricketyWood, mancubus/pinky/talisman/SMM; the rest (IMP_NEST, HOLLOW_METAL, FLESH_PLAYER,
/// FLESH_DARKANGEL, ASH, NONE) use defaultEffect.
pub fn surftype_effect(t: u32) -> usize {
    match t {
        1..=19 => t as usize,
        20 => 21, // SLUDGE
        24 => 20, // SHIELD
        26 => 22, // BLOOD_POOL
        28 => 23, // FLESH_MUTANT
        29 => 24, // THICK_PADDING
        30 => 35, // FORCEFIELD
        31 => 25, // RICKETY_METAL
        32 => 26, // RICKETY_WOOD
        35 => 28, // MANCUBUS_FLESH
        36 => 27, // MANCUBUS_ARMOR
        37 => 30, // PINKY_FLESH
        38 => 29, // PINKY_ARMOR
        39 => 31, // TALISMAN_ARMOR
        40 => 33, // SMM_FLESH
        41 => 32, // SMM_ARMOR
        _ => SURFACE_DEFAULT,
    }
}

/// One projectileImpactEffect_t (ctor 0x1406f6490 defaults for omitted fields).
fn effect(db: &DeclDb, e: Option<&Block>, particles: &mut HashMap<String, Arc<ParticleDecl>>) -> Effect {
        let mut prt = Vec::new();
        for p in list_str(e, "particleImpact") {
            if p.eq_ignore_ascii_case("NULL") {
                prt.push(None);
                continue;
            }
            if let Some(d) = particles.get(&p) {
                prt.push(Some(d.clone()));
                continue;
            }
            match ParticleDecl::load(db, &p) {
                Ok(d) => {
                    particles.insert(p, d.clone());
                    prt.push(Some(d));
                }
                Err(err) => {
                    eprintln!("impact: {err:#}");
                    prt.push(None);
                }
            }
        }
        // projectileImpactEffect_t ctor 0x1406f6490 defaults.
        Effect {
            decals: list_str(e, "decalMaterial"),
            decal_size: f32_or(e, "decalSize", 8.0),
            decal_lifetime: i32_or(e, "decalLifetime", 8000),
            decal_fade_in: i32_or(e, "decalFadeInTime", 0),
            decal_fade_out: i32_or(e, "decalFadeOutTime", 2000),
            decal_emissive_lifetime: i32_or(e, "decalEmissiveLifetime", 0),
            decal_depth: f32_or(e, "decalDepth", 4.0),
            decal_angle: f32_or(e, "decalAngle", 0.0),
            decal_diffuse_tint: vec4_or(e, "decalDiffuseTint", Vec4::new(1.0, 1.0, 1.0, 0.0)),
            decal_emissive_tint: vec4_or(e, "decalEmmissiveTint", Vec4::ZERO),
            decal_specular_tint: vec4_or(e, "decalSpecularTint", Vec4::ONE),
            decal_opacity: vec4_or(e, "decalOpacityPerChannel", Vec4::ONE),
            particles: prt,
            sound: str_of(e, "sndImpact").unwrap_or("").to_string(),
            sound_local: str_of(e, "sndImpact_local").unwrap_or("").to_string(),
            persistent: bool_or(e, "decalIsPersistent", false),
            use_smoke: bool_or(e, "useSmokeSystem", false),
            bloody: bool_or(e, "isBloody", false),
            team_based: bool_or(e, "teamBased", false),
            view_shake: str_of(e, "viewShakeDecl").filter(|s| !s.is_empty()).map(str::to_string),
            view_shake_start: f32_or(e, "viewShakeStartDistance", 0.0),
            view_shake_distance: f32_or(e, "viewShakeDistance", -1.0),
        }
}

impl ImpactTable {
    pub fn load(db: &DeclDb, name: &str, particles: &mut HashMap<String, Arc<ParticleDecl>>) -> Result<ImpactTable> {
        let b = db.get("projectileimpacteffect", name).with_context(|| format!("projectileImpactEffect {name}"))?;
        let edit = block(Some(&b), "edit");
        let effects = SURFACES.iter().map(|s| effect(db, block(edit, s), particles)).collect();
        let hazard = |k: &str, particles: &mut HashMap<String, Arc<ParticleDecl>>| {
            let l = block(edit, k);
            let n = i32_or(l, "num", 0).max(0) as usize;
            (0..n).map(|i| effect(db, block(l, &format!("item[{i}]")), particles)).collect::<Vec<_>>()
        };
        let hazard_fire = hazard("hazardFire", particles);
        let hazard_steam = hazard("hazardSteam", particles);
        Ok(ImpactTable { name: name.to_string(), effects, hazard_fire, hazard_steam })
    }
}

/// idVec3::ToMat3 as used for impacts (0x1402ee4c0): the axis whose z is the (normalised) vector.
pub fn normal_axis(n: Vec3) -> Axis {
    let d2 = n.x * n.x + n.y * n.y;
    if d2.abs() <= f32::MIN_POSITIVE {
        let s = if 0.0 > n.z { -1.0 } else { 1.0 };
        return Axis { x: Vec3::X, y: Vec3::new(0.0, s, 0.0), z: Vec3::new(0.0, 0.0, s) };
    }
    let z = crate::normalize(n);
    let d = 1.0 / d2.max(1e-30).sqrt();
    let x = Vec3::new(-n.y * d, n.x * d, 0.0);
    let y = z.cross(x);
    Axis { x, y, z }
}

/// A projectile decl's impact tables.
#[derive(Debug, Clone)]
pub struct ProjectileImpacts {
    pub table: Option<Arc<ImpactTable>>,
    pub limited: Option<Arc<ImpactTable>>,
    pub max_per_surface: i32,
}

/// Loads (and caches) impact tables by projectile decl.
#[derive(Default)]
pub struct ImpactCache {
    pub tables: HashMap<String, Option<Arc<ImpactTable>>>,
    pub projectiles: HashMap<String, Arc<ProjectileImpacts>>,
    pub particles: HashMap<String, Arc<ParticleDecl>>,
}

impl ImpactCache {
    fn table(&mut self, db: &DeclDb, name: Option<&str>) -> Option<Arc<ImpactTable>> {
        let name = name?.to_string();
        if let Some(t) = self.tables.get(&name) {
            return t.clone();
        }
        let t = match ImpactTable::load(db, &name, &mut self.particles) {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                eprintln!("impact: {e:#}");
                None
            }
        };
        self.tables.insert(name, t.clone());
        t
    }

    pub fn projectile(&mut self, db: &DeclDb, projectile: &str) -> Arc<ProjectileImpacts> {
        if let Some(p) = self.projectiles.get(projectile) {
            return p.clone();
        }
        let b = db.get("projectile", projectile).ok();
        let edit = b.as_deref().and_then(|b| b.block("edit"));
        let table = self.table(db, str_of(edit, "impactEffectTable"));
        let limited = self.table(db, str_of(edit, "impactEffectTableLimitedBySurfType"));
        let p = Arc::new(ProjectileImpacts { table, limited, max_per_surface: i32_or(edit, "maxImpactEffectsPerSurface", 0) });
        self.projectiles.insert(projectile.to_string(), p.clone());
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauss_impact_view_shake() {
        // projectileimpacteffect/gauss_rifle_sp: viewShakeDecl screenviewshake/sp/gauss_rifle, 150 / 600.
        let e = Effect { view_shake: Some("screenviewshake/sp/gauss_rifle".into()), view_shake_start: 150.0, view_shake_distance: 600.0, ..Default::default() };
        let player = |x: f32| [Vec3::new(x - 16.0, -16.0, 0.0), Vec3::new(x + 16.0, 16.0, 74.0)];
        let hit = Vec3::new(0.0, 0.0, 10.0);
        let call = e.view_shake_call(hit, player(300.0), Vec3::new(300.0, 0.0, 10.0)).unwrap();
        assert_eq!(call, crate::fx::ShakeCall::Advanced { decl: "screenviewshake/sp/gauss_rifle", distance: 300.0, start: 150.0, end: 600.0 });
        // The box is pos +- 600 against the player's bounds (touching counts), not a sphere.
        assert!(e.view_shake_call(hit, player(616.0), Vec3::new(616.0, 0.0, 10.0)).is_some());
        assert!(e.view_shake_call(hit, player(617.0), Vec3::new(617.0, 0.0, 10.0)).is_none());
        // No decl: nothing.
        assert!(Effect::default().view_shake_call(hit, player(0.0), hit).is_none());
    }
}
