//! Player upgrades (BOARD row 23): argent cells (health / armour / ammo capacity), Praetor suit upgrades and runes,
//! all held by the player's idEnvironmentSuit (gamedata/re/upgrades/UPGRADES.md).
//!
//! A perk (`perks/perk/...`, idDeclPerk) lists upgrade decls (`upgrade/...`, idDeclUpgrade); an upgrade carries
//! data modifiers (idUpgradeMod_Data: health / armour capacity, armour coefficient), equipment modifiers
//! (idUpgradeMod_Equipment: one per EQUIP_* type, the runes and suit mods), ability modifiers and weapon upgrade
//! packs (ammo capacity). The suit applies data mods in 0x140b833e0 and stores the first equipment mod of each
//! type in its slot (0x140b83770); the game systems then ask the suit (getters 0x140b85xxx).

use std::collections::HashMap;

use idres::decl::Block;
use idres::decldb::DeclDb;

use crate::pickups::Vitals;
use crate::weapons::arsenal::AmmoPool;

/// upgradeModOperator_t (enum table 0x143560430), applied by 0x140ec82e0 (jump table 0x140ec83d4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    ToggleBool,
    Add,
    Subtract,
    Multiply,
    Set,
    BasePct,
    AbsolutePct,
    AddPct,
}

impl Operator {
    pub fn parse(s: Option<&str>, default: Operator) -> Self {
        match s {
            Some("MOD_OPERATOR_TOGGLE_BOOL") => Operator::ToggleBool,
            Some("MOD_OPERATOR_ADD") => Operator::Add,
            Some("MOD_OPERATOR_SUBTRACT") => Operator::Subtract,
            Some("MOD_OPERATOR_MULTIPLY") => Operator::Multiply,
            Some("MOD_OPERATOR_SET") => Operator::Set,
            Some("MOD_OPERATOR_BASE_PCT") => Operator::BasePct,
            Some("MOD_OPERATOR_ABSOLUTE_PCT") => Operator::AbsolutePct,
            Some("MOD_OPERATOR_ADD_PCT") => Operator::AddPct,
            _ => default,
        }
    }

    /// 0x140ec82e0 on floats: `base` is the unmodified value, `cur` the current one.
    pub fn apply(self, base: f32, cur: f32, v: f32) -> f32 {
        match self {
            Operator::ToggleBool => {
                if v > 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Operator::Add => cur + v,
            Operator::Subtract => cur - v,
            Operator::Multiply => cur * v,
            Operator::Set => v,
            Operator::BasePct => base * v + base,
            Operator::AbsolutePct => cur * v + cur,
            Operator::AddPct => base * v + cur,
        }
    }

    /// The int flavour (0x140ec8470) used by weapon upgrades such as WMT_WEAPON_AMMO_UPGRADE_LEVEL. INTERIM: taken as
    /// the float operator on ints (only ADD is used by the shipped ammo upgrades).
    pub fn apply_int(self, cur: i32, v: i32) -> i32 {
        self.apply(cur as f32, cur as f32, v as f32) as i32
    }
}

/// idUpgradeMod_Data::dataModType_t (enum table 0x143560cd0), the ones the suit applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    HealthCapacity,
    ArmorCapacity,
    ArmorCoefficient,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataMod {
    pub kind: DataType,
    pub op: Operator,
    pub value: f32,
}

/// idUpgradeMod_Equipment::equipmentModType_t (enum table 0x143568c00), as named in the decls.
pub type EquipType = String;

pub const ABSORB_HEALTH_ON_DEATH: &str = "EQUIP_ABSORB_HEALTH_ON_DEATH";
pub const GLORY_KILLS_AWARD_ARMOR: &str = "EQUIP_GLORY_KILLS_AWARD_ARMOR";
pub const ACTIVATE_FOCUS_ON_DEATH_BLOW: &str = "EQUIP_ACTIVATE_FOCUS_ON_DEATH_BLOW";
pub const INFINITE_AMMO_ON_HEALTH_VALUE: &str = "EQUIP_INFINITE_AMMO_ON_HEALTH_VALUE";
pub const MODIFY_HEALTH_DROPS_ON_HEALTH_VALUE: &str = "EQUIP_MODIFY_HEALTH_DROPS_ON_HEALTH_VALUE";
pub const INCREASE_DROP_RADIUS: &str = "EQUIP_INCREASE_DROP_RADIUS";
pub const MODIFY_AMMO_DROPS: &str = "EQUIP_MODIFY_AMMO_DROPS";
pub const MODIFY_ARMOR_MITIGATION: &str = "EQUIP_MODIFY_ARMOR_MITIGATION";
pub const MODIFY_ENEMY_DAMAGE_ON_HEALTH_VALUE: &str = "EQUIP_MODIFY_ENEMY_DAMAGE_ON_HEALTH_VALUE";
pub const TAKE_AND_DEAL_MORE_DAMAGE: &str = "EQUIP_TAKE_AND_DEAL_MORE_DAMAGE";
pub const MODIFY_ENEMY_STAGGER_TOUGHNESS: &str = "EQUIP_MODIFY_ENEMY_STAGGER_TOUGHNESS";
pub const WEAPON_UPGRADE_PACK: &str = "EQUIP_WEAPON_UPGRADE_PACK";
/// The two types whose slot is replaced by a newer mod (0x140b83770 cases 0x1f / 0x20); every other slot keeps the
/// first mod it was given.
const REPLACING: [&str; 2] = ["EQUIP_MODIFY_COOLDOWN_TIME", "EQIUP_MODIFY_MAX_USES"];

/// idUpgradeMod_Equipment::infiniteAmmoMode_t as the suit update (0x140b86f50) switches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfiniteAmmoMode {
    None,
    Armor,
    Health,
    HealthAndArmor,
    CombatLevel,
}

/// One weapon upgrade of an EQUIP_WEAPON_UPGRADE_PACK (inventoryUpgrade_t): its WMT_WEAPON_AMMO_UPGRADE_LEVEL.
#[derive(Debug, Clone, PartialEq)]
pub struct PackItem {
    /// The weapon inventory decl (`weapon/zion/player/sp/shotgun`).
    pub weapon: String,
    pub upgrade: String,
    pub op: Operator,
    pub value: i32,
    /// idDeclUpgrade maxNumUses (-1 = unlimited).
    pub max_uses: i32,
}

/// idUpgradeMod_Equipment (0x2d8 bytes): the fields the ported effects read. INTERIM: fields a decl omits take 1.0 for
/// the multipliers / percentages and 0 otherwise (the struct's default values were not found: it has no vtable and
/// its decl list elements are built by the reflection system).
#[derive(Debug, Clone, PartialEq)]
pub struct EquipMod {
    pub kind: EquipType,
    /// +0x10 armorDropPerGloryKill, +0x288 upgradeValues.
    pub armor_drop_per_glory_kill: i32,
    pub upgrade_values: Vec<f32>,
    /// +0x78.. infiniteAmmoAbove{Health,Armor,CombatLevel}Value and the _Upgraded set (+0x84..), +0x90 / +0x94 modes.
    pub infinite_ammo: [f32; 3],
    pub infinite_ammo_upgraded: [f32; 3],
    pub infinite_ammo_mode: InfiniteAmmoMode,
    /// +0x9c playerLowHealthLevel, +0xa0 healthDropModPercentage.
    pub player_low_health_level: f32,
    pub health_drop_mod: f32,
    /// +0xb0 / +0xb4 dropRadiusPercentage Mod / Upgrade, +0xb8 / +0xbc dropRadiusSpeedMultiplier Mod / Upgrade.
    pub drop_radius: (f32, f32),
    pub drop_speed: (f32, f32),
    /// +0xc0 / +0xc4 ammoDropModPercentage (base / _Upgraded), +0xc8 / +0xcc ammoPickupModPercentage.
    pub ammo_drop: (f32, f32),
    pub ammo_pickup: (f32, f32),
    /// +0xfc / +0x100 enemyDamageMultiplier (base / _Upgraded), +0x104 playerDamageMultiplier.
    pub enemy_damage: (f32, f32),
    pub player_damage: f32,
    /// +0x2b8 upgradePack.
    pub pack: Vec<PackItem>,
    /// The Saving Throw focus fields (+0x40.. +0x68).
    pub focus: FocusParms,
}

/// idUpgradeMod_Equipment's focus fields: +0x40 focusAttackDurationMs, +0x44 leftOverHealthValue, +0x50
/// focusInvulnerableTimeMs, +0x54 / +0x58 focusMaxHealth (base / _Upgraded), +0x5c focusDisablesAtHealth, +0x64
/// focusMaxUses. INTERIM: an omitted leftOverHealthValue is 1 and an omitted focusMaxUses 1 (defaults not found).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusParms {
    pub duration_ms: i32,
    pub left_over_health: f32,
    pub invulnerable_ms: i32,
    pub max_health: (f32, f32),
    pub disables_at_health: f32,
    pub max_uses: i32,
}

fn f(b: &Block, k: &str, d: f32) -> f32 {
    b.f32(k).unwrap_or(d)
}

impl EquipMod {
    pub fn parse(db: &DeclDb, b: &Block) -> Self {
        // infiniteAmmoMode_t (enum table 0x14356b450).
        let mode = |k: &str| match b.str(k) {
            Some("IAM_ARMOR") => InfiniteAmmoMode::Armor,
            Some("IAM_HEALTH") => InfiniteAmmoMode::Health,
            Some("IAM_BOTH") => InfiniteAmmoMode::HealthAndArmor,
            Some("IAM_COMBAT_LEVEL") => InfiniteAmmoMode::CombatLevel,
            _ => InfiniteAmmoMode::None,
        };
        let list = |k: &str| -> Vec<f32> {
            b.block(k).map(|l| (0..l.f32("num").unwrap_or(0.0) as usize).filter_map(|i| l.f32(&format!("item[{i}]"))).collect()).unwrap_or_default()
        };
        let mut pack = Vec::new();
        if let Some(l) = b.block("upgradePack") {
            for i in 0..l.f32("num").unwrap_or(0.0) as usize {
                let Some(it) = l.block(&format!("item[{i}]")) else { continue };
                let (Some(weapon), Some(upgrade)) = (it.str("inventoryDecl"), it.str("upgradeDecl")) else { continue };
                let u = db.get("upgrade", &upgrade.to_lowercase()).ok();
                let m = u.as_ref().and_then(|u| u.block("edit.modifiersWeapon.item[0]")).cloned().unwrap_or_default();
                pack.push(PackItem {
                    weapon: weapon.to_lowercase(),
                    upgrade: upgrade.to_string(),
                    op: Operator::parse(m.str("opType"), Operator::Set),
                    value: m.f32("data.valueInt").unwrap_or(0.0) as i32,
                    max_uses: u.as_ref().and_then(|u| u.f32("edit.maxNumUses")).map(|v| v as i32).unwrap_or(-1),
                });
            }
        }
        EquipMod {
            kind: b.str("type").unwrap_or_default().to_string(),
            armor_drop_per_glory_kill: f(b, "armorDropPerGloryKill", 0.0) as i32,
            upgrade_values: list("upgradeValues"),
            infinite_ammo: [f(b, "infiniteAmmoAboveHealthValue", 0.0), f(b, "infiniteAmmoAboveArmorValue", 0.0), f(b, "infiniteAmmoAboveCombatLevel", 0.0)],
            infinite_ammo_upgraded: [
                f(b, "infiniteAmmoAboveHealthValue_Upgraded", 0.0),
                f(b, "infiniteAmmoAboveArmorValue_Upgraded", 0.0),
                f(b, "infiniteAmmoAboveCombatLevel_Upgraded", 0.0),
            ],
            infinite_ammo_mode: mode("infiniteAmmoMode"),
            player_low_health_level: f(b, "playerLowHealthLevel", 0.0),
            health_drop_mod: f(b, "healthDropModPercentage", 1.0),
            drop_radius: (f(b, "dropRadiusPercentageMod", 1.0), f(b, "dropRadiusPercentageUpgrade", 0.0)),
            drop_speed: (f(b, "dropRadiusSpeedMultiplierMod", 1.0), f(b, "dropRadiusSpeedMultiplierUpgrade", 0.0)),
            ammo_drop: (f(b, "ammoDropModPercentage", 1.0), f(b, "ammoDropModPercentage_Upgraded", 1.0)),
            ammo_pickup: (f(b, "ammoPickupModPercentage", 1.0), f(b, "ammoPickupModPercentage_Upgraded", 1.0)),
            enemy_damage: (f(b, "enemyDamageMultiplier", 1.0), f(b, "enemyDamageMultiplier_Upgraded", 1.0)),
            player_damage: f(b, "playerDamageMultiplier", 1.0),
            pack,
            focus: FocusParms {
                duration_ms: f(b, "focusAttackDurationMs", 0.0) as i32,
                left_over_health: f(b, "leftOverHealthValue", 1.0),
                invulnerable_ms: f(b, "focusInvulnerableTimeMs", 0.0) as i32,
                max_health: (f(b, "focusMaxHealth", 0.0), f(b, "focusMaxHealth_Upgraded", f(b, "focusMaxHealth", 0.0))),
                disables_at_health: f(b, "focusDisablesAtHealth", 0.0),
                max_uses: f(b, "focusMaxUses", 1.0) as i32,
            },
        }
    }
}

/// An idDeclUpgrade.
#[derive(Debug, Clone, PartialEq)]
pub struct Upgrade {
    pub name: String,
    pub data: Vec<DataMod>,
    pub equip: Vec<EquipMod>,
    /// idUpgradeMod_Abilities types (double jump, powerups, weapon change / ledge grab speed, ...).
    pub abilities: Vec<String>,
    /// The powerup ones, parsed (the others are applied by systems the testbed does not have yet).
    pub ability_mods: Vec<AbilityMod>,
    pub max_uses: i32,
}

/// idUpgradeMod_Abilities (0x60 bytes), the Praetor suit's powerup fields. INTERIM defaults for omitted fields:
/// powerUpDurationModMs -1 (not applied), powerUpDurationModMult -1 (not applied), the Health / Armor values -1
/// (= max, per the field comments), OnEnter true (powerup_shockwave writes `powerUpArmorOnEnter = false`, so the
/// exporter's default is true), OnExit false; the struct default was not found.
#[derive(Debug, Clone, PartialEq)]
pub struct AbilityMod {
    pub kind: String,
    /// +0x1c powerUpDurationModMs, +0x20 powerUpDurationModMult.
    pub duration_ms: i32,
    pub duration_mult: f32,
    /// +0x24 powerUpArmorValue, +0x28 / +0x29 OnEnter / OnExit; +0x2c powerUpHealthValue, +0x30 / +0x31.
    pub armor: (i32, bool, bool),
    pub health: (i32, bool, bool),
}

impl AbilityMod {
    fn parse(b: &Block) -> Self {
        let flag = |k: &str, d: bool| b.path(k).and_then(|v| v.as_bool()).unwrap_or(d);
        AbilityMod {
            kind: b.str("type").unwrap_or_default().to_string(),
            duration_ms: f(b, "powerUpDurationModMs", -1.0) as i32,
            duration_mult: f(b, "powerUpDurationModMult", -1.0),
            armor: (f(b, "powerUpArmorValue", -1.0) as i32, flag("powerUpArmorOnEnter", true), flag("powerUpArmorOnExit", false)),
            health: (f(b, "powerUpHealthValue", -1.0) as i32, flag("powerUpHealthOnEnter", true), flag("powerUpHealthOnExit", false)),
        }
    }
}

impl Upgrade {
    /// Decl references mix case (`enviroment_suit/health_capacity_Hard`); the game's lookup ignores it, the
    /// container's names are lower case.
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("upgrade", &name.to_lowercase())?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let items = |k: &str| -> Vec<Block> {
            e.block(k).map(|l| (0..l.f32("num").unwrap_or(0.0) as usize).filter_map(|i| l.block(&format!("item[{i}]")).cloned()).collect()).unwrap_or_default()
        };
        let data = items("modifiersData")
            .iter()
            .map(|m| DataMod {
                kind: match m.str("type") {
                    Some("DMT_HEALTH_CAPACITY") => DataType::HealthCapacity,
                    Some("DMT_ARMOR_CAPACITY") => DataType::ArmorCapacity,
                    Some("DMT_ARMOR_COEFFICIENT") => DataType::ArmorCoefficient,
                    _ => DataType::Other,
                },
                // INTERIM: a data mod without opType adds (the capacity decls omit it; SET would make 25 the max, so
                // the default is taken as ADD; the element default was not found).
                op: Operator::parse(m.str("opType"), Operator::Add),
                value: m.f32("data.valueFloat").unwrap_or(0.0),
            })
            .collect();
        Ok(Upgrade {
            name: name.to_string(),
            data,
            equip: items("modifiersEquipment").iter().map(|m| EquipMod::parse(db, m)).collect(),
            abilities: items("modifiersAbility").iter().filter_map(|m| m.str("type").map(str::to_string)).collect(),
            ability_mods: items("modifiersAbility").iter().map(AbilityMod::parse).collect(),
            max_uses: e.f32("maxNumUses").map(|v| v as i32).unwrap_or(-1),
        })
    }
}

/// What a perk is, from its descriptionFlags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerkKind {
    /// PKF_PLAYER_HEALTH / ARMOR / AMMO: argent cell capacity.
    Capacity,
    /// PKF_ENV_SUIT_MOD: a rune.
    Rune,
    /// PKF_PLAYER_ABILITY: a Praetor suit upgrade.
    Suit,
    Other,
}

/// An idDeclPerk.
#[derive(Debug, Clone, PartialEq)]
pub struct Perk {
    pub name: String,
    pub kind: PerkKind,
    pub upgrades: Vec<Upgrade>,
    /// +0xd4 maxNumUses (-1 unlimited).
    pub max_uses: i32,
    /// +0x160 difficultyLevel.
    pub difficulty: Option<usize>,
}

impl Perk {
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("perks", &name.to_lowercase())?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let flags = e.str("descriptionFlags").unwrap_or_default();
        let kind = match flags {
            "PKF_PLAYER_HEALTH" | "PKF_PLAYER_ARMOR" | "PKF_PLAYER_AMMO" => PerkKind::Capacity,
            "PKF_ENV_SUIT_MOD" => PerkKind::Rune,
            "PKF_PLAYER_ABILITY" => PerkKind::Suit,
            _ => PerkKind::Other,
        };
        let mut upgrades = Vec::new();
        if let Some(l) = e.block("upgrades") {
            for i in 0..l.f32("num").unwrap_or(0.0) as usize {
                if let Some(u) = l.str(&format!("item[{i}]")) {
                    upgrades.push(Upgrade::load(db, u)?);
                }
            }
        }
        let difficulty = match e.str("difficultyLevel") {
            Some("DIFFICULTY_EASY") => Some(0),
            Some("DIFFICULTY_MEDIUM") => Some(1),
            Some("DIFFICULTY_HARD") => Some(2),
            Some("DIFFICULTY_ULTRAVIOLENT") => Some(3),
            Some("DIFFICULTY_NIGHTMARE") => Some(4),
            _ => None,
        };
        Ok(Perk { name: name.to_string(), kind, upgrades, max_uses: e.f32("maxNumUses").map(|v| v as i32).unwrap_or(-1), difficulty })
    }
}

/// The argent cell perks per stat, as the abilities inventory items give them (inventoryitem
/// abilities/{health,armor,ammo}_upgrade givePerksOnReceive: one perk per difficulty).
pub fn argent_perks(stat: &str) -> [String; 5] {
    let base = format!("perk/zion/player/sp/enviroment_suit/{stat}_capacity");
    [format!("{base}_easy"), base.clone(), format!("{base}_hard"), format!("{base}_ultraviolent"), format!("{base}_nightmare")]
}

/// The pools a weapon's ammo upgrades raise: (weapon inventory decl, pool key, pool base maxCount).
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponPool {
    pub weapon: String,
    pub pool: String,
    pub base_max: i32,
}

/// The player's idEnvironmentSuit.
#[derive(Debug, Clone, Default)]
pub struct Suit {
    /// Equipment mod slots (+0x328..+0x400), keyed by type.
    pub equip: HashMap<EquipType, EquipMod>,
    /// +0x408 healthCapacityLevel, +0x40c armorCapacityLevel, +0x410 armorCoefficientLevel, +0x414 ammoCapacityLevel.
    pub health_level: i32,
    pub armor_level: i32,
    pub coefficient_level: i32,
    pub ammo_level: i32,
    /// Times each perk / upgrade was applied (maxNumUses).
    pub uses: HashMap<String, i32>,
    /// Per weapon: its ammo upgrade count (weapon +0x1bf0) and level (WMT_WEAPON_AMMO_UPGRADE_LEVEL, +0x1bf4).
    pub weapon_ammo: HashMap<String, (i32, i32)>,
    /// Mastered runes (the player's per-rune upgrade counters, idPlayer +0x47d78.. +0x47d94): the `_Upgraded` values.
    pub mastered: Vec<EquipType>,
    /// Abilities granted (names) and the powerup ones in effect.
    pub abilities: Vec<String>,
    /// The player's powerup duration add (+0x55b64, ms) and multiplier (+0x55b60), set by
    /// ABILITY_MOD_POWERUP_DURATION_MOD (0x140b83090: each when >= 0). None = untouched (0, 1).
    pub powerup_duration: Option<(i32, f32)>,
    /// ABILITY_MOD_POWERUP_HEALTH / ARMOR in the suit (idEnvironmentSuit +0x2e8 / +0x2e0).
    pub powerup_health: Option<AbilityMod>,
    pub powerup_armor: Option<AbilityMod>,
    /// armour component absorptionCoefficient as the coefficient mods leave it (component_t default 1.0).
    pub armor_absorption: Option<f32>,
}

/// What a grant changed, for the trace.
#[derive(Debug, Clone, PartialEq)]
pub enum Granted {
    Skipped(String),
    HealthMax { max: f32, cur: f32 },
    ArmorMax { max: f32, cur: f32 },
    ArmorAbsorption(f32),
    Equip(EquipType),
    AmmoMax { pool: String, max: i32 },
    Ability(String),
}

impl Suit {
    pub fn has(&self, t: &str) -> bool {
        self.equip.contains_key(t)
    }

    pub fn is_mastered(&self, t: &str) -> bool {
        self.mastered.iter().any(|m| m == t)
    }

    /// Gives a perk: every upgrade it lists, each limited by its maxNumUses. Perks with a difficultyLevel only apply
    /// at that difficulty (INTERIM: the "special case" check behind idDeclPerk +0x160 was not traced; the argent
    /// cell items give one perk per difficulty, so only the matching one can count).
    pub fn grant(&mut self, perk: &Perk, difficulty: usize, vitals: &mut Vitals, pools: &mut [AmmoPool], weapon_pools: &[WeaponPool]) -> Vec<Granted> {
        let mut out = Vec::new();
        if perk.difficulty.is_some_and(|d| d != difficulty) {
            return out;
        }
        let used = self.uses.get(&perk.name).copied().unwrap_or(0);
        if perk.max_uses >= 0 && used >= perk.max_uses {
            out.push(Granted::Skipped(format!("{} used {used} / {}", perk.name, perk.max_uses)));
            return out;
        }
        *self.uses.entry(perk.name.clone()).or_default() += 1;
        for u in &perk.upgrades {
            let used = self.uses.get(&u.name).copied().unwrap_or(0);
            if u.max_uses >= 0 && used >= u.max_uses {
                out.push(Granted::Skipped(format!("{} used {used} / {}", u.name, u.max_uses)));
                continue;
            }
            *self.uses.entry(u.name.clone()).or_default() += 1;
            for m in &u.data {
                out.extend(self.apply_data(m, vitals));
            }
            for e in &u.equip {
                if e.kind == WEAPON_UPGRADE_PACK {
                    out.extend(self.apply_pack(e, pools, weapon_pools));
                } else if !self.equip.contains_key(&e.kind) || REPLACING.contains(&e.kind.as_str()) {
                    // 0x140b83770: the slot keeps the first mod of its type.
                    self.equip.insert(e.kind.clone(), e.clone());
                    out.push(Granted::Equip(e.kind.clone()));
                }
            }
            for a in &u.abilities {
                self.abilities.push(a.clone());
                out.push(Granted::Ability(a.clone()));
            }
            for a in &u.ability_mods {
                match a.kind.as_str() {
                    "ABILITY_MOD_POWERUP_DURATION_MOD" => {
                        let (mut ms, mut mult) = self.powerup_duration.unwrap_or((0, 1.0));
                        if a.duration_ms >= 0 {
                            ms = a.duration_ms;
                        }
                        if a.duration_mult >= 0.0 {
                            mult = a.duration_mult;
                        }
                        self.powerup_duration = Some((ms, mult));
                    }
                    "ABILITY_MOD_POWERUP_HEALTH" => self.powerup_health = Some(a.clone()),
                    "ABILITY_MOD_POWERUP_ARMOR" => self.powerup_armor = Some(a.clone()),
                    _ => {}
                }
            }
        }
        out
    }

    /// 0x140b833e0 for the data mods: the capacity mod goes through the operator on the component max
    /// (idPlayerHealth 0x1406b9260: health max +0x14, armour max +0x60, armour absorptionCoefficient +0x84), then a
    /// component below its new max is set to it (vtable +0xb8); the coefficient changes nothing else.
    fn apply_data(&mut self, m: &DataMod, vitals: &mut Vitals) -> Option<Granted> {
        match m.kind {
            DataType::HealthCapacity => {
                let c = &mut vitals.health;
                c.max = m.op.apply(c.max, c.max, m.value);
                if c.cur < c.max && c.max > 0.0 {
                    c.cur = c.max;
                }
                self.health_level += 1;
                Some(Granted::HealthMax { max: c.max, cur: c.cur })
            }
            DataType::ArmorCapacity => {
                let c = &mut vitals.armor;
                c.max = m.op.apply(c.max, c.max, m.value);
                if c.cur < c.max {
                    c.cur = c.max;
                }
                self.armor_level += 1;
                Some(Granted::ArmorMax { max: c.max, cur: c.cur })
            }
            DataType::ArmorCoefficient => {
                let a = self.armor_absorption.unwrap_or(1.0);
                let a = m.op.apply(a, a, m.value);
                self.armor_absorption = Some(a);
                self.coefficient_level += 1;
                Some(Granted::ArmorAbsorption(a))
            }
            DataType::Other => None,
        }
    }

    /// EQUIP_WEAPON_UPGRADE_PACK (0x140b83770 case 0x16): every owned weapon whose ammo upgrade count equals the
    /// suit's ammoCapacityLevel gets its upgrade (idWeapon::ApplyUpgradeModifier 0x140f2f8d0 case 0x7c: level = op(level),
    /// count + 1, and the level becomes its shared pool's extraCapacity, 0x140ef3cb0); the level then rises by one.
    /// Pool max = (int)((maxCount + extraCapacity) * g_AmmoScalar) (idAmmoItem 0x140ef3200; g_AmmoScalar 1, the
    /// player's ammo scale component taken as 1: INTERIM).
    fn apply_pack(&mut self, e: &EquipMod, pools: &mut [AmmoPool], weapon_pools: &[WeaponPool]) -> Vec<Granted> {
        let mut out = Vec::new();
        let mut applied = false;
        for it in &e.pack {
            let Some(wp) = weapon_pools.iter().find(|w| w.weapon == it.weapon) else { continue };
            let (count, level) = self.weapon_ammo.get(&it.weapon).copied().unwrap_or((0, 0));
            if count != self.ammo_level || (it.max_uses >= 0 && count >= it.max_uses) {
                continue;
            }
            let level = it.op.apply_int(level, it.value);
            self.weapon_ammo.insert(it.weapon.clone(), (count + 1, level));
            if let Some(p) = pools.iter_mut().find(|p| p.key == wp.pool) {
                p.max = wp.base_max + level;
                out.push(Granted::AmmoMax { pool: p.key.clone(), max: p.max });
            }
            applied = true;
        }
        if applied {
            self.ammo_level += 1;
        }
        out
    }

    /// 0x140b85560: the health drop maxDrop multiplier: healthDropModPercentage while health <= playerLowHealthLevel.
    pub fn health_drop_scale(&self, health: f32) -> f32 {
        match self.equip.get(MODIFY_HEALTH_DROPS_ON_HEALTH_VALUE) {
            Some(m) if health <= m.player_low_health_level => m.health_drop_mod,
            _ => 1.0,
        }
    }

    /// 0x140b851a0: armour items a glory kill drops with EQUIP_GLORY_KILLS_AWARD_ARMOR: armorDropPerGloryKill plus
    /// one upgradeValues entry per suit upgrade level (player +0x2d0 -> +0x47d70), 0 without the mod.
    pub fn glory_kill_armor_drops(&self, upgrade_level: usize) -> i32 {
        let Some(m) = self.equip.get(GLORY_KILLS_AWARD_ARMOR) else { return 0 };
        if m.armor_drop_per_glory_kill <= 0 {
            return 0;
        }
        let mut n = m.armor_drop_per_glory_kill;
        for v in m.upgrade_values.iter().take(upgrade_level) {
            n = (n as f32 + v) as i32;
        }
        n
    }

    /// 0x140b850d0: dropped ammo multiplier (ammoDropModPercentage, the _Upgraded one once mastered).
    pub fn ammo_drop_scale(&self) -> f32 {
        match self.equip.get(MODIFY_AMMO_DROPS) {
            Some(m) if self.is_mastered(MODIFY_AMMO_DROPS) => m.ammo_drop.1,
            Some(m) => m.ammo_drop.0,
            None => 1.0,
        }
    }

    /// 0x140b85160: placed ammo multiplier (ammoPickupModPercentage, the _Upgraded one once mastered).
    pub fn ammo_pickup_scale(&self) -> f32 {
        match self.equip.get(MODIFY_AMMO_DROPS) {
            Some(m) if self.is_mastered(MODIFY_AMMO_DROPS) => m.ammo_pickup.1,
            Some(m) => m.ammo_pickup.0,
            None => 1.0,
        }
    }

    /// idMovementThinkComponent popup 0x1409bcde0: the attraction radius and speed with EQUIP_INCREASE_DROP_RADIUS:
    /// pct = dropRadiusPercentageMod + n * dropRadiusPercentageUpgrade, radius = max(pct * base, base), speed =
    /// (dropRadiusSpeedMultiplierMod + n * dropRadiusSpeedMultiplierUpgrade) * attractSpeed; n = the rune's upgrade
    /// count (idPlayer +0x47d90, 1 once mastered). Without the mod the base radius and speed.
    pub fn drop_attraction(&self, base_radius: f32, base_speed: f32) -> (f32, f32) {
        let Some(m) = self.equip.get(INCREASE_DROP_RADIUS) else { return (base_radius, base_speed) };
        let n = if self.is_mastered(INCREASE_DROP_RADIUS) { 1.0 } else { 0.0 };
        let pct = m.drop_radius.0 + n * m.drop_radius.1;
        let speed = (m.drop_speed.0 + n * m.drop_speed.1) * base_speed;
        ((pct * base_radius).max(base_radius), speed)
    }

    /// The suit update 0x140b86f50 (Rich Get Richer): whether the player's infinite-ammo flag (+0xce47 bit 7, read
    /// by idWeapon::HasInfiniteAmmo 0x140f16900) is on. A negative threshold means "at the component's max".
    pub fn infinite_ammo(&self, vitals: &Vitals, combat_level: f32, max_combat_level: f32) -> bool {
        let Some(m) = self.equip.get(INFINITE_AMMO_ON_HEALTH_VALUE) else { return false };
        let t = if self.is_mastered(INFINITE_AMMO_ON_HEALTH_VALUE) { m.infinite_ammo_upgraded } else { m.infinite_ammo };
        let at = |cur: f32, thr: f32, max: f32| if thr < 0.0 { cur >= max } else { cur >= thr };
        let health = at(vitals.health.cur, t[0], vitals.health.max);
        let armor = at(vitals.armor.cur, t[1], vitals.armor.max);
        let combat = at(combat_level, t[2], max_combat_level);
        match m.infinite_ammo_mode {
            InfiniteAmmoMode::None => false,
            InfiniteAmmoMode::Armor => armor,
            InfiniteAmmoMode::Health => health,
            InfiniteAmmoMode::HealthAndArmor => health && armor,
            InfiniteAmmoMode::CombatLevel => combat,
        }
    }

    /// 0x140b829d0: the suit's scale on damage the player takes: armorMitigation's playerDamageMultiplier while armour
    /// is above 0, and takeAndDealMoreDamage's always.
    pub fn damage_taken_scale(&self, armor: f32) -> f32 {
        let mut s = 1.0;
        if let Some(m) = self.equip.get(MODIFY_ARMOR_MITIGATION)
            && armor > 0.0
        {
            s *= m.player_damage;
        }
        if let Some(m) = self.equip.get(TAKE_AND_DEAL_MORE_DAMAGE) {
            s *= m.player_damage;
        }
        s
    }

    /// 0x140b85200: the suit's scale on damage the player deals: enemyDamageMultiplier (+0xfc) of
    /// takeAndDealMoreDamage (always), armorMitigation (armour above 0) and modEnemyDamageOnHealth (health at or
    /// below its playerLowHealthLevel), and of modEnemyStaggerToughness on a staggered target (its _Upgraded +0x100
    /// once mastered); the combat scoring multiplier (+0x43c) is 1 here.
    pub fn damage_dealt_scale(&self, vitals: &Vitals, target_staggered: bool) -> f32 {
        let mut s = 1.0;
        if let Some(m) = self.equip.get(TAKE_AND_DEAL_MORE_DAMAGE) {
            s *= m.enemy_damage.0;
        }
        if let Some(m) = self.equip.get(MODIFY_ARMOR_MITIGATION)
            && vitals.armor.cur > 0.0
        {
            s *= m.enemy_damage.0;
        }
        if let Some(m) = self.equip.get(MODIFY_ENEMY_DAMAGE_ON_HEALTH_VALUE)
            && vitals.health.cur <= m.player_low_health_level
        {
            s *= m.enemy_damage.0;
        }
        if let Some(m) = self.equip.get(MODIFY_ENEMY_STAGGER_TOUGHNESS)
            && target_staggered
        {
            s *= if self.is_mastered(MODIFY_ENEMY_STAGGER_TOUGHNESS) { m.enemy_damage.1 } else { m.enemy_damage.0 };
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators() {
        let ops = [Operator::ToggleBool, Operator::Add, Operator::Subtract, Operator::Multiply, Operator::Set, Operator::BasePct, Operator::AbsolutePct, Operator::AddPct];
        let got: Vec<f32> = ops.iter().map(|o| o.apply(10.0, 20.0, 0.5)).collect();
        assert_eq!(got, [1.0, 20.5, 19.5, 10.0, 0.5, 15.0, 30.0, 25.0]);
    }

    fn install() -> Option<crate::install::Install> {
        let doom = idres::find_install()?;
        Some(crate::install::load(&doom).expect("loading install"))
    }

    const SP: &str = "perk/zion/player/sp/enviroment_suit/";

    fn weapon_pools() -> Vec<WeaponPool> {
        let wp = |w: &str, p: &str, m: i32| WeaponPool { weapon: format!("weapon/zion/player/sp/{w}"), pool: format!("ammo/zion/sharedammopool/{p}"), base_max: m };
        vec![
            wp("shotgun", "shells", 20),
            wp("double_barrel", "shells", 20),
            wp("plasma_rifle", "cells", 150),
            wp("rocket_launcher", "rockets", 15),
            wp("heavy_rifle_heavy_ar", "bullets", 90),
            wp("chainsaw", "fuel", 3),
        ]
    }

    /// Argent cells at medium: health and armour +25 per cell (4 uses), refilled; ammo per the weapon packs.
    #[test]
    fn argent_cells() {
        let Some(inst) = install() else { return };
        let db = &inst.decls;
        let mut v = crate::pickups::player_vitals(db);
        v.health.cur = 40.0;
        let mut suit = Suit::default();
        let mut pools: Vec<AmmoPool> = weapon_pools().iter().map(|w| AmmoPool { key: w.pool.clone(), count: 1, max: w.base_max }).collect();
        pools.dedup_by(|a, b| a.key == b.key);
        let wps = weapon_pools();
        let grant = |stat: &str, suit: &mut Suit, v: &mut Vitals, pools: &mut Vec<AmmoPool>| {
            let mut out = Vec::new();
            for name in argent_perks(stat) {
                let p = Perk::load(db, &name).unwrap();
                out.extend(suit.grant(&p, 1, v, pools, &wps));
            }
            out
        };
        assert_eq!(grant("health", &mut suit, &mut v, &mut pools), [Granted::HealthMax { max: 125.0, cur: 125.0 }]);
        for _ in 0..3 {
            grant("health", &mut suit, &mut v, &mut pools);
        }
        assert_eq!((v.health.max, v.health.cur, suit.health_level), (200.0, 200.0, 4));
        assert!(matches!(grant("health", &mut suit, &mut v, &mut pools)[..], [Granted::Skipped(_)]), "4 uses");
        assert_eq!(grant("armor", &mut suit, &mut v, &mut pools), [Granted::ArmorMax { max: 75.0, cur: 75.0 }]);
        let max = |pools: &[AmmoPool], k: &str| pools.iter().find(|p| p.key.ends_with(k)).unwrap().max;
        grant("ammo", &mut suit, &mut v, &mut pools);
        assert_eq!(["shells", "cells", "rockets", "bullets", "fuel"].map(|k| max(&pools, k)), [30, 200, 20, 120, 4]);
        grant("ammo", &mut suit, &mut v, &mut pools);
        assert_eq!(["shells", "cells", "rockets", "bullets", "fuel"].map(|k| max(&pools, k)), [40, 250, 25, 150, 5]);
        // The weapon upgrades stop at their maxNumUses 2; the rocket launcher's has none.
        grant("ammo", &mut suit, &mut v, &mut pools);
        grant("ammo", &mut suit, &mut v, &mut pools);
        assert_eq!(["shells", "cells", "rockets", "bullets", "fuel"].map(|k| max(&pools, k)), [40, 250, 35, 150, 5]);
        assert_eq!(suit.ammo_level, 4);
    }

    /// The runes the testbed applies, from their perk decls.
    #[test]
    fn runes() {
        let Some(inst) = install() else { return };
        let db = &inst.decls;
        let mut v = crate::pickups::player_vitals(db);
        let mut suit = Suit::default();
        let mut give = |name: &str, suit: &mut Suit| {
            let p = Perk::load(db, &format!("{SP}{name}")).unwrap();
            assert_eq!(p.kind, PerkKind::Rune, "{name}");
            suit.grant(&p, 1, &mut v, &mut [], &[])
        };
        // Vacuum: attraction radius x2.25 and speed x2, mastered x4 / x3.
        give("increase_drop_radius", &mut suit);
        assert_eq!(suit.drop_attraction(280.0, 800.0), (630.0, 1600.0));
        suit.mastered.push(INCREASE_DROP_RADIUS.into());
        assert_eq!(suit.drop_attraction(280.0, 800.0), (1120.0, 2400.0));
        // Ammo Boost: the mastered multipliers are 2 (the base ones are omitted by the decl: INTERIM 1).
        give("modify_ammo_drops", &mut suit);
        assert_eq!((suit.ammo_drop_scale(), suit.ammo_pickup_scale()), (1.0, 1.0));
        suit.mastered.push(MODIFY_AMMO_DROPS.into());
        assert_eq!((suit.ammo_drop_scale(), suit.ammo_pickup_scale()), (2.0, 2.0));
        // Rich Get Richer: infinite ammo while armour >= 100 (mastered >= 75).
        give("infinite_ammo_on_health_value", &mut suit);
        let mut vit = crate::pickups::player_vitals(db);
        vit.armor.max = 150.0;
        vit.armor.cur = 99.0;
        assert!(!suit.infinite_ammo(&vit, 0.0, 0.0));
        vit.armor.cur = 100.0;
        assert!(suit.infinite_ammo(&vit, 0.0, 0.0));
        vit.armor.cur = 75.0;
        assert!(!suit.infinite_ammo(&vit, 0.0, 0.0));
        suit.mastered.push(INFINITE_AMMO_ON_HEALTH_VALUE.into());
        assert!(suit.infinite_ammo(&vit, 0.0, 0.0));
        // Armored Offensive: 2 armour drops per glory kill, 4 mastered.
        give("glory_kills_award_armor", &mut suit);
        assert_eq!((suit.glory_kill_armor_drops(0), suit.glory_kill_armor_drops(1)), (2, 4));
        // The first mod of a type stays (0x140b83770).
        let before = suit.equip.len();
        give("increase_drop_radius", &mut suit);
        assert_eq!(suit.equip.len(), before);
    }

    /// Damage scales of the (cut) take-and-deal runes, and the armour split of a hit.
    #[test]
    fn damage_paths() {
        let Some(inst) = install() else { return };
        let db = &inst.decls;
        let mut v = crate::pickups::player_vitals(db);
        let mut suit = Suit::default();
        assert_eq!(suit.damage_taken_scale(0.0), 1.0);
        let p = Perk::load(db, &format!("{SP}take_and_deal_less_damage")).unwrap();
        suit.grant(&p, 1, &mut v, &mut [], &[]);
        assert_eq!((suit.damage_taken_scale(0.0), suit.damage_dealt_scale(&v, false)), (0.5, 0.5));
        // Armour 30 absorbs a 45 hit up to its value; the rest reaches health.
        v.armor.cur = 30.0;
        let hit = v.hit(45.0);
        assert_eq!((hit.armor, hit.health), ((30.0, 0.0), (100.0, 85.0)));
        let hit = v.hit(10.0);
        assert_eq!((hit.armor, hit.health), ((0.0, 0.0), (85.0, 75.0)));
        // An armour coefficient below 1 lets the rest through.
        v.armor.cur = 50.0;
        v.armor.absorption = 0.5;
        let hit = v.hit(20.0);
        assert_eq!((hit.armor, hit.health), ((50.0, 40.0), (75.0, 65.0)));
    }
}
