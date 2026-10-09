//! Damage to an idAI2: the damage decl's idDamageParms fields the AI path reads, Damage_Calculate (0x1403fa6b0)
//! and ApplyLocationDamage (0x14082fc20). DEMONS.md section 2 has the full chain with addresses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use glam::Vec3;
use idres::decldb::DeclDb;

use super::decl::{DemonDef, list};

/// DAMAGETYPE_ flags (records {value, name, desc} at 0x142229068).
pub mod types {
    pub const HEALTH: u32 = 1;
    pub const EMP: u32 = 2;
    pub const ELECTRIC: u32 = 4;
    pub const FIRE: u32 = 8;
    pub const ARMOR_PIERCING: u32 = 0x10;
    pub const TELEPORT: u32 = 0x20;
    pub const TOWER: u32 = 0x40;
    pub const INVULNERABILITY: u32 = 0x80;
    pub const EXPLOSION: u32 = 0x100;
    pub const ARMOR_REDUCED: u32 = 0x200;
    pub const PAIN_CAUSING: u32 = 0x400;
    pub const PLAYER_KILL: u32 = 0x800;
    pub const SYNC: u32 = 0x1000;
    pub const BLINDING: u32 = 0x2000;
    pub const COLD: u32 = 0x4000;
    pub const HAZARD: u32 = 0x8000;
    pub const SPLASH: u32 = 0x10000;
    pub const EQUIPMENT: u32 = 0x20000;

    pub fn parse(s: &str) -> u32 {
        s.split_whitespace()
            .map(|w| match w {
                "DAMAGETYPE_HEALTH" => HEALTH,
                "DAMAGETYPE_EMP" => EMP,
                "DAMAGETYPE_ELECTRIC" => ELECTRIC,
                "DAMAGETYPE_FIRE" => FIRE,
                "DAMAGETYPE_ARMOR_PIERCING" => ARMOR_PIERCING,
                "DAMAGETYPE_TELEPORT" => TELEPORT,
                "DAMAGETYPE_TOWER" => TOWER,
                "DAMAGETYPE_INVULNERABILITY" => INVULNERABILITY,
                "DAMAGETYPE_EXPLOSION" => EXPLOSION,
                "DAMAGETYPE_ARMOR_REDUCED" => ARMOR_REDUCED,
                "DAMAGETYPE_PAIN_CAUSING" => PAIN_CAUSING,
                "DAMAGETYPE_PLAYER_KILL" => PLAYER_KILL,
                "DAMAGETYPE_SYNC" => SYNC,
                "DAMAGETYPE_BLINDING" => BLINDING,
                "DAMAGETYPE_COLD" => COLD,
                "DAMAGETYPE_HAZARD" => HAZARD,
                "DAMAGETYPE_SPLASH" => SPLASH,
                "DAMAGETYPE_EQUIPMENT" => EQUIPMENT,
                _ => 0,
            })
            .fold(0, |a, b| a | b)
    }
}

/// damageSource_t values (table 0x1435a0ba0).
pub fn damage_source(s: &str) -> u32 {
    s.split_whitespace()
        .map(|w| match w {
            "DAMAGESRC_BULLET" => 1,
            "DAMAGESRC_MELEE" => 2,
            "DAMAGESRC_SPRINT_MELEE" => 4,
            "DAMAGESRC_FIRE" => 8,
            "DAMAGESRC_ELECTRICAL" => 0x10,
            "DAMAGESRC_FALL" => 0x20,
            "DAMAGESRC_CRUSH" => 0x40,
            "DAMAGESRC_EXPLOSIVE" => 0x80,
            "DAMAGESRC_CHAINSAW" => 0x100,
            "DAMAGESRC_ACID" => 0x200,
            "DAMAGESRC_PLASMA" => 0x400,
            "DAMAGESRC_PLASMA_BARB" => 0x800,
            "DAMAGESRC_PUSH_BACK" => 0x10000,
            _ => 0,
        })
        .fold(0, |a, b| a | b)
}

/// idDamageParms::aiDamageMitigation_t.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mitigation {
    pub monster_types: u32,
    pub max_damage: f32,
    pub scalar: f32,
    pub per_frame: bool,
}

/// The idDamageParms fields the AI damage path uses (ctor 0x1406e42a0 defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct AiDamageParms {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub point_blank: f32,
    pub start_pb: f32,
    pub end_pb: f32,
    pub start_falloff: f32,
    pub end_falloff: f32,
    pub self_damage_scale: f32,
    /// playerDamageScale (idDamageParms +0xfc, "scale to apply when a player takes damage"; idPlayer damage
    /// 0x140dbee00). INFERRED default 1.0 (ctor not decoded).
    pub player_damage_scale: f32,
    /// DAMAGETYPE_ bits; ctor default HEALTH.
    pub damage_types: u32,
    pub damage_source: u32,
    pub ai_stimulus_scale: f32,
    pub stagger_time_scale: f32,
    pub is_melee: bool,
    pub causes_pain: bool,
    pub ignore_armor: bool,
    pub no_death: bool,
    pub damage_is_current_health: bool,
    pub instant_ragdoll: bool,
    pub headshot_focus_damage: bool,
    /// damageIntensity_t override (0 = from the damage amount).
    pub intensity: u32,
    pub mitigation: Vec<Mitigation>,
}

impl Default for AiDamageParms {
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
            self_damage_scale: 1.0,
            player_damage_scale: 1.0,
            damage_types: types::HEALTH,
            damage_source: 0,
            ai_stimulus_scale: 1.0,
            stagger_time_scale: 1.0,
            is_melee: false,
            causes_pain: true,
            ignore_armor: false,
            no_death: false,
            damage_is_current_health: false,
            instant_ragdoll: false,
            headshot_focus_damage: false,
            intensity: 0,
            mitigation: Vec::new(),
        }
    }
}

impl AiDamageParms {
    pub fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("damage", name).with_context(|| format!("damage decl {name}"))?;
        let p = b.block("edit.damageParms").cloned().unwrap_or_default();
        let d = Self::default();
        let f = |k: &str, def: f32| p.f32(k).unwrap_or(def);
        let flag = |k: &str, def: bool| p.path(k).and_then(|v| v.as_bool()).unwrap_or(def);
        Ok(Self {
            name: name.to_string(),
            min: f("minDamage", d.min),
            max: f("maxDamage", d.max),
            point_blank: f("pointBlankDamage", 0.0),
            start_pb: f("startPBFalloffDistance", 0.0),
            end_pb: f("endPBFalloffDistance", 0.0),
            start_falloff: f("startFalloffDistance", 0.0),
            end_falloff: f("endFalloffDistance", 0.0),
            self_damage_scale: f("selfDamageScale", 1.0),
            player_damage_scale: f("playerDamageScale", 1.0),
            damage_types: p.str("damageTypes").map(types::parse).unwrap_or(d.damage_types),
            damage_source: p.str("damageSource").map(damage_source).unwrap_or(0),
            ai_stimulus_scale: f("aiStimulusScale", 1.0),
            stagger_time_scale: f("staggerTimeScale", 1.0),
            is_melee: flag("isMelee", false),
            causes_pain: flag("causesPain", true),
            ignore_armor: flag("ignoreArmor", false),
            no_death: flag("noDeath", false),
            damage_is_current_health: flag("damageIsCurrentHealth", false),
            instant_ragdoll: flag("instantRagdoll", false),
            headshot_focus_damage: flag("headshotFocusDamage", false),
            intensity: match p.str("intensity").unwrap_or("") {
                "DAMAGEINTENSITY_LIGHT" => 1,
                "DAMAGEINTENSITY_MEDIUM" => 2,
                "DAMAGEINTENSITY_HEAVY" => 3,
                _ => 0,
            },
            mitigation: list(p.block("aiDamageMitigation"))
                .into_iter()
                .map(|m| Mitigation {
                    monster_types: m.and_then(|m| m.str("monsterType")).map(super::decl::monster_type).unwrap_or(0),
                    max_damage: m.and_then(|m| m.f32("maxDamage")).unwrap_or(-1.0),
                    scalar: m.and_then(|m| m.f32("damageScalar")).unwrap_or(1.0),
                    per_frame: m.and_then(|m| m.path("damagePerFrame")).and_then(|v| v.as_bool()).unwrap_or(false),
                })
                .collect(),
        })
    }

    /// GetDamage (0x1406e4a10) for a squared distance (SPEC.md "Damage by distance").
    pub fn at_distance_sq(&self, d2: f32) -> f32 {
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
}

/// Damage decls parsed once per name.
#[derive(Default)]
pub struct ParmsCache(Mutex<HashMap<String, Arc<AiDamageParms>>>);

impl ParmsCache {
    pub fn get(&self, db: &DeclDb, name: &str) -> Result<Arc<AiDamageParms>> {
        if let Some(p) = self.0.lock().unwrap().get(name) {
            return Ok(p.clone());
        }
        let p = Arc::new(AiDamageParms::from_decl(db, name)?);
        self.0.lock().unwrap().insert(name.to_string(), p.clone());
        Ok(p)
    }
}

/// One trace of a damage event that reached the demon.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceHit {
    /// Skeleton joint name of the hit sphere (trace c.trmFeature on a CONTACT_SPHERE contact); None for
    /// contacts without a joint.
    pub joint: Option<String>,
    pub point: Vec3,
}

/// One idAI2::Damage call.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageEvent {
    /// Damage decl name (e.g. `damage/zion/firearm/sp/pistol`).
    pub decl: String,
    /// Traces of this call (empty for radius damage: the model's default joint is used).
    pub traces: Vec<TraceHit>,
    /// Attacker physics origin; None for world damage.
    pub attacker_origin: Option<Vec3>,
    /// Damage(...) scale argument (record +0x630; radius damage passes its falloff scale).
    pub scale: f32,
    /// Direction the damage travels (attacker -> target), for pain / death direction.
    pub dir: Vec3,
    /// Splash radius fraction for the SDPS splash window (dist^2 / radius^2), -1 for direct hits.
    pub splash_fraction: f32,
}

/// What Damage_Calculate produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DamageResult {
    /// Base damage before location scaling (record +0x1a28).
    pub base: f32,
    /// Scaled damage (record +0x1a2c, after difficulty / mitigation / cvar scales).
    pub scaled: f32,
    /// Health damage (record +0x1a30) actually applied.
    pub health: f32,
    pub armor: f32,
    /// A headShot-flagged location was hit.
    pub head_shot: bool,
    /// The hit joint used for pain / death selection (first trace).
    pub joint: Option<String>,
    /// Index of the damage group hit (first trace), or None.
    pub group: Option<usize>,
    /// The demon died from this damage.
    pub killed: bool,
}

/// One location's damage (ApplyLocationDamage 0x14082fc20): [health, armor, group, soft].
pub fn location_damage(def: &DemonDef, parms: &AiDamageParms, group: usize, armor: f32, dmg: f32) -> ([f32; 4], bool) {
    let mut out = [0.0f32; 4];
    let Some(g) = def.groups.get(group) else { return (out, false) };
    let (mut scale, mut armored, mut armor_scale, mut head) = (1.0f32, 1.0f32, 1.0f32, false);
    if !parms.is_melee {
        // An entry for this exact decl wins; else the last entry without a decl.
        let mut found = None;
        for (i, s) in g.scalars.iter().enumerate() {
            match &s.decl {
                Some(d) if *d == parms.name => {
                    found = Some(i);
                    break;
                }
                None => found = Some(i),
                _ => {}
            }
        }
        if let Some(s) = found.map(|i| &g.scalars[i]) {
            scale = s.damage_scale;
            armored = s.armored_damage_scale;
            armor_scale = s.armor_damage_scale;
            head = s.head_shot;
        }
    }
    let mut pierce = 0.0f32;
    if parms.damage_types & types::ARMOR_PIERCING != 0 {
        armor_scale = 2.0;
        pierce = 0.3;
    }
    if parms.damage_types & types::ARMOR_REDUCED != 0 {
        armor_scale = 0.5;
    }
    if parms.damage_types & types::EXPLOSION != 0 {
        armor_scale = 2.0;
        pierce = 0.3;
    }
    let armored = (pierce * scale + armored).min(1.0);
    out[1] = (armor_scale * dmg).min(armor);
    let a = dmg.min(armor);
    let rest = if armor_scale.abs() >= f32::MIN_POSITIVE { (dmg - armor / armor_scale).max(0.0) } else { dmg };
    out[2] = rest + a * armored;
    if g.soft_target {
        out[3] = dmg;
    }
    if g.affects_overall_health {
        out[0] = rest * scale + a * armored * scale;
    }
    if parms.ignore_armor {
        out[2] = dmg;
        out[0] = dmg;
    }
    (out, head)
}
