//! Weapon mods: the perk / upgrade data model read from the decls and idWeapon::ApplyUpgradeModifier
//! (gamedata/re/MODS.md sections 1-2, 6).
//!
//! weapon decl perkGroups[0] -> idDeclPerkGroup perkFamilies[] (base perk = the mod, unhideWeaponModSelect,
//! upgrades[3], mastery, weaponMastery) -> idDeclPerk upgrades[] -> idDeclUpgrade modifiersWeapon[]
//! (idUpgradeMod_Weapon: type WMT_*, opType (default SET), data, fireMode (-1 = the upgrade's mode)).
//! Weapon-state modifiers (WMT 24-27, 31-91, 93, 94, 107, 110-127) are written into per-mode override slots
//! by ApplyUpgradeModifier 0x140f2f8d0, whose value helpers start from 0 (so ADD acts as SET there).
//! Damage / projectile modifiers (WMT 0-23, 28-30, 92, 95-109, 128) are evaluated when a shot is produced,
//! from the base value (`UseTime`); that path is not decoded (MODS.md TODO 1): INTERIM, the operator is
//! applied to the base value in upgrade order.

use std::collections::HashMap;
use std::sync::Arc;

use idres::decl::Block;
use idres::decldb::DeclDb;

use super::decl::{burst_mode, trigger_mode, ChargeProperty, WeaponDef};
use super::zoom::ZoomMode;

/// MOD_OPERATOR_* (reflection enum table 0x143560430).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModOp {
    ToggleBool,
    Add,
    Subtract,
    Multiply,
    /// The idUpgradeMod_Weapon ctor default (0x1408446b0: opType 4).
    #[default]
    Set,
    BasePct,
    AbsolutePct,
    AddPct,
}

impl ModOp {
    fn parse(s: &str) -> Self {
        match s.strip_prefix("MOD_OPERATOR_").unwrap_or(s) {
            "TOGGLE_BOOL" => Self::ToggleBool,
            "ADD" => Self::Add,
            "SUBTRACT" => Self::Subtract,
            "MULTIPLY" => Self::Multiply,
            "BASE_PCT" => Self::BasePct,
            "ABSOLUTE_PCT" => Self::AbsolutePct,
            "ADD_PCT" => Self::AddPct,
            _ => Self::Set,
        }
    }
}

/// upgradeDataValue_t (0x58): the typed values a modifier carries (decl keys value*).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModData {
    pub int: Option<i32>,
    pub float: Option<f32>,
    pub bool_: Option<bool>,
    pub decl: String,
    pub sound: String,
    pub string: String,
    pub table: String,
    pub zoom_mode: Option<ZoomMode>,
    pub trigger_mode: Option<i32>,
    pub burst_mode: Option<i32>,
    pub charge_property: Option<ChargeProperty>,
}

/// idUpgradeMod_Weapon (0x78).
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponModifier {
    /// WMT_WEAPON_* without the prefix (enums_wmt.txt).
    pub kind: String,
    pub op: ModOp,
    pub data: ModData,
    /// +0x70: -1 = the upgrade's fire mode.
    pub fire_mode: i32,
}

/// idDeclUpgrade (0x240).
#[derive(Debug, Clone, PartialEq)]
pub struct Upgrade {
    pub name: String,
    /// +0x200. INTERIM: a decl without fireMode is taken as PRIMARY (0), the zero-initialised value.
    pub fire_mode: i32,
    /// +0x204: the modifiers apply to both fire modes.
    pub all_fire_modes: bool,
    pub modifiers: Vec<WeaponModifier>,
}

/// idDeclPerk (0x170): the upgrades it activates (removed again on deactivate).
#[derive(Debug, Clone, PartialEq)]
pub struct Perk {
    pub name: String,
    pub upgrades: Vec<Upgrade>,
    /// disablePerkWhenActivated: the weapon's other mod.
    pub disables: String,
    /// schematicData.buildPoints.
    pub build_points: i32,
}

/// idWeaponMastery (0x88), applied while MOD_MASTERY (+0x1891) is set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WeaponMastery {
    /// WMT_* weaponMasteryType name (e.g. SHOTGUN_TRIPLE_BURST).
    pub kind: String,
    pub damage_scale: f32,
    pub projectile_decl: String,
    pub passive_charge_time_ms: i32,
    pub upgrade: Option<Upgrade>,
}

/// idPerkFamily (0xc8).
#[derive(Debug, Clone, PartialEq)]
pub struct PerkFamily {
    pub base: Perk,
    /// unhideWeaponModSelect: the fp_hands generic_unhide_mod_select slot (0 = none).
    pub unhide_slot: i32,
    pub upgrades: Vec<Perk>,
    pub mastery: Option<Perk>,
    pub weapon_mastery: WeaponMastery,
}

/// A weapon's mods (its first perk group) and every weapon decl variant the upgrades can switch to.
#[derive(Debug, Default)]
pub struct WeaponMods {
    pub group: String,
    pub families: Vec<PerkFamily>,
    /// The weapon decl's own upgrades[] (UC_MANUAL). INTERIM: not applied; they conflict with the perk
    /// upgrades (e.g. shotgun secondary_cooldown_time_decrease SETs CHARGE_TIMEOUT 750 while the perk
    /// faster_recharge SETs 1520), so they are taken as unused in the campaign (MODS.md TODO 2).
    pub weapon_upgrades: Vec<String>,
    /// (weapon decl, ammo decl) -> resolved def, for DECL_WEAPON / DECL_AMMO.
    pub decls: HashMap<(String, String), Arc<WeaponDef>>,
    /// Charge item valueTables of the mod decls and CHARGE_VALUE_TABLE overrides, by decl name.
    pub tables: HashMap<String, Arc<crate::handlayers::reactions::DeclTable>>,
}

impl WeaponMods {
    pub fn def(&self, weapon: &str, ammo: &str) -> Option<Arc<WeaponDef>> {
        self.decls.get(&(weapon.to_string(), ammo.to_string())).cloned()
    }
}

fn list(b: &Block, key: &str) -> Vec<Block> {
    let Some(l) = b.block(key) else { return Vec::new() };
    let n = l.f32("num").unwrap_or(0.0) as usize;
    (0..n).filter_map(|i| l.block(&format!("item[{i}]")).cloned()).collect()
}

fn names(b: &Block, key: &str) -> Vec<String> {
    let Some(l) = b.block(key) else { return Vec::new() };
    let n = l.f32("num").unwrap_or(0.0) as usize;
    (0..n).filter_map(|i| l.str(&format!("item[{i}]")).map(str::to_string)).collect()
}

fn fire_mode(s: Option<&str>) -> Option<i32> {
    match s? {
        "WEAPONFIREMODE_PRIMARY" => Some(0),
        "WEAPONFIREMODE_SECONDARY" => Some(1),
        _ => None,
    }
}

fn modifier(m: &Block) -> WeaponModifier {
    let d = m.block("data").cloned().unwrap_or_default();
    let s = |k: &str| d.str(k).filter(|v| *v != "NULL").unwrap_or_default().to_string();
    let decl = d.str("valueDecl").or(d.str("valueDeclFX")).filter(|v| *v != "NULL").unwrap_or_default().to_string();
    WeaponModifier {
        kind: m.str("type").unwrap_or_default().trim_start_matches("WMT_WEAPON_").to_string(),
        op: m.str("opType").map(ModOp::parse).unwrap_or_default(),
        data: ModData {
            int: d.f32("valueInt").map(|v| v as i32),
            float: d.f32("valueFloat"),
            bool_: d.path("valueBool").and_then(|v| v.as_bool()),
            decl,
            sound: s("valueSound"),
            string: s("valueString"),
            table: s("valueTable"),
            zoom_mode: d.str("valueWeaponZoomMode").map(ZoomMode::parse),
            trigger_mode: d.str("valueWeaponTriggerMode").map(trigger_mode),
            burst_mode: d.str("valueWeaponBurstMode").map(burst_mode),
            charge_property: d.str("valueChargeProperty").map(ChargeProperty::parse),
        },
        fire_mode: fire_mode(m.str("fireMode")).unwrap_or(-1),
    }
}

pub fn load_upgrade(db: &DeclDb, name: &str) -> Option<Upgrade> {
    let b = db.get("upgrade", name).ok()?;
    let e = b.block("edit").cloned().unwrap_or_default();
    Some(Upgrade {
        name: name.to_string(),
        fire_mode: fire_mode(e.str("fireMode")).unwrap_or(0),
        all_fire_modes: e.path("allFireModes").and_then(|v| v.as_bool()).unwrap_or(false),
        modifiers: list(&e, "modifiersWeapon").iter().map(modifier).collect(),
    })
}

fn load_perk(db: &DeclDb, name: &str) -> Option<Perk> {
    let b = db.get("perks", name).ok()?;
    let e = b.block("edit").cloned().unwrap_or_default();
    Some(Perk {
        name: name.to_string(),
        upgrades: names(&e, "upgrades").iter().filter_map(|u| load_upgrade(db, u)).collect(),
        disables: e.str("disablePerkWhenActivated").filter(|v| *v != "NULL").unwrap_or_default().to_string(),
        build_points: e.f32("schematicData.buildPoints").unwrap_or(0.0) as i32,
    })
}

fn unhide_slot(s: &str) -> i32 {
    match s {
        "UNHIDE_WEAPON_MOD_SELECT_1" => 1,
        "UNHIDE_WEAPON_MOD_SELECT_2" => 2,
        _ => 0,
    }
}

/// Every mod of `def` from the decls, with the decl variants its upgrades name (DECL_WEAPON and
/// DECL_AMMO combinations) resolved up front.
pub fn load_mods(db: &DeclDb, def: &WeaponDef) -> Option<WeaponMods> {
    let group = def.perk_groups.first()?.clone();
    let g = db.get("perkgroups", &group).ok()?;
    let e = g.block("edit").cloned().unwrap_or_default();
    let mut families = Vec::new();
    for f in list(&e, "perkFamilies") {
        let Some(base) = f.str("base").and_then(|p| load_perk(db, p)) else { continue };
        let wm = f.block("weaponMastery").cloned().unwrap_or_default();
        families.push(PerkFamily {
            base,
            unhide_slot: f.str("unhideWeaponModSelect").map(unhide_slot).unwrap_or(0),
            upgrades: names(&f, "upgrades").iter().filter_map(|p| load_perk(db, p)).collect(),
            mastery: f.str("mastery").and_then(|p| load_perk(db, p)),
            weapon_mastery: WeaponMastery {
                kind: wm.str("type").unwrap_or_default().trim_start_matches("WMT_").to_string(),
                damage_scale: wm.f32("damageScale").unwrap_or(0.0),
                projectile_decl: wm.str("projectileDecl").filter(|v| *v != "NULL").unwrap_or_default().to_string(),
                passive_charge_time_ms: wm.f32("passiveChargeTimeMS").unwrap_or(0.0) as i32,
                upgrade: wm.str("declUpgrade").and_then(|u| load_upgrade(db, u)),
            },
        });
    }
    let mut mods = WeaponMods { group, families, weapon_upgrades: def.upgrades.clone(), decls: HashMap::new(), tables: HashMap::new() };
    // Weapon decls the upgrades can select, and every ammo decl they can pair with.
    let mut weapons = vec![def.decl.clone()];
    if !def.secondary_fire_decl.is_empty() {
        weapons.push(def.secondary_fire_decl.clone());
    }
    let mut ammos = vec![String::new()];
    let mut tables: Vec<String> = Vec::new();
    let all: Vec<&Upgrade> = mods.families.iter().flat_map(|f| f.all_upgrades()).chain(mods.families.iter().filter_map(|f| f.weapon_mastery.upgrade.as_ref())).collect();
    for u in all {
        for m in &u.modifiers {
            match m.kind.as_str() {
                "DECL_WEAPON" if !m.data.decl.is_empty() && !weapons.contains(&m.data.decl) => weapons.push(m.data.decl.clone()),
                "DECL_AMMO" if !m.data.decl.is_empty() && !ammos.contains(&m.data.decl) => ammos.push(m.data.decl.clone()),
                "CHARGE_VALUE_TABLE" if !m.data.table.is_empty() => tables.push(m.data.table.clone()),
                _ => {}
            }
        }
    }
    for w in &weapons {
        for a in &ammos {
            let r = if a.is_empty() { WeaponDef::from_decl(db, w) } else { WeaponDef::from_decl_with_ammo(db, w, Some(a)) };
            match r {
                Ok(d) => {
                    mods.decls.insert((w.clone(), a.clone()), Arc::new(d));
                }
                Err(e) if a.is_empty() => eprintln!("mod decl {w}: {e:#}"),
                Err(_) => {}
            }
        }
    }
    let items = mods.decls.values().flat_map(|d| d.charge.items.iter()).chain(def.charge.items.iter());
    tables.extend(items.map(|i| i.value_table.clone()).filter(|t| !t.is_empty()));
    for t in tables {
        if let std::collections::hash_map::Entry::Vacant(e) = mods.tables.entry(t) {
            match crate::handlayers::reactions::DeclTable::load(db, e.key()) {
                Ok(tb) => {
                    e.insert(Arc::new(tb));
                }
                Err(err) => eprintln!("charge value table: {err:#}"),
            }
        }
    }
    Some(mods)
}

impl PerkFamily {
    /// The family's perks in purchase order: base, upgrades[0..3], mastery.
    pub fn perks(&self) -> impl Iterator<Item = &Perk> {
        std::iter::once(&self.base).chain(self.upgrades.iter()).chain(self.mastery.iter())
    }

    pub fn all_upgrades(&self) -> impl Iterator<Item = &Upgrade> {
        self.perks().flat_map(|p| p.upgrades.iter())
    }

    /// Mod name (the base perk's last path element, e.g. "secondary_charge_burst").
    pub fn name(&self) -> &str {
        self.base.name.rsplit('/').next().unwrap_or(&self.base.name)
    }
}

/// The per-mode and weapon-wide override slots ApplyUpgradeModifier 0x140f2f8d0 writes (offsets are idWeapon's;
/// per-mode slots are at +400 * mode unless noted). Reset values (0x140f31150): -1 / 0 / -1.0.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeSlots {
    /// 31 DECL_WEAPON (+0x1898): the decl for the mode.
    pub decl: Option<String>,
    /// 34 DECL_AMMO (+0x18a0).
    pub ammo: Option<String>,
    /// 44 FIRING_INTERVAL (+0x14b0 + 4 * mode), 0 = none.
    pub firing_interval: i32,
    /// 46 OTHER_FIRE_MODE_FIRING_INTERVAL (+0x1970), -1 = none.
    pub other_fire_mode_firing_interval: i32,
    /// 58 ZOOM_FOV (+0x1918), 59 ZOOM_TIME (+0x1920), 60 ZOOM_DELAY (+0x191c): <= 0 = none.
    pub zoom_fov: f32,
    pub zoom_time: i32,
    pub zoom_delay: i32,
    /// 61 ZOOM_MODE (+0x1924), None = decl.
    pub zoom_mode: Option<ZoomMode>,
    /// 79 CHARGE_TIME (+0x18e0), 83 CHARGE_TIMEOUT (+0x1900): -1 = decl.
    pub charge_time: i32,
    pub charge_timeout: i32,
    /// Charge item overrides (list +0x18e8): (property, valueMin, valueMax, table), -1 = decl.
    pub charge_values: Vec<(ChargeProperty, f32, f32, String)>,
    /// CHARGE_PER_SHOT_INCREMENT (0x140f11a10), -1 = chargeIncrement.
    pub charge_per_shot_increment: f32,
    pub charge_start_sound: String,
    /// FIRE_DELAY / MOVEMENT_DELAY (chaingun turret) and SHOOT_DELAY_ANIM_DURATION_MS, -1 = none.
    pub fire_delay: i32,
    pub movement_delay: i32,
    pub shoot_delay_anim_duration_ms: i32,
    /// OVERRIDE_SHOOT_STATE (+0x1998 interval) and its firing interval / normal-shot count (SSG mastery).
    pub override_shoot_state: String,
    pub override_shoot_state_firing_interval: i32,
    pub override_shoot_then_normal_shots: i32,
    /// TARGET_LOCK_TIME_SEC (+0x19a8), TARGET_LOCK_FOV (+0x19ac), TARGET_OUT_OF_FOV_TIME_SEC (+0x19b0),
    /// TARGET_RECOVERY_SEC (+0x19b4), TARGET_MAX_TARGETS (+0x19bc): -1 = decl (reset 0x140f31150).
    pub target_lock_time_sec: f32,
    pub target_lock_fov: f32,
    pub target_out_of_fov_time_sec: f32,
    pub target_recovery_sec: f32,
    pub target_max_targets: i32,
    /// TARGET_UNBREAKABLE_LOCK (+0x19b8): the slot keeps its candidate without re-validation.
    /// PROJECTILE_LOCK (+0x1968): the mode counts as a locking mode (targeting 0x140f281f2).
    pub target_unbreakable_lock: bool,
    pub projectile_lock: bool,
    /// DETONATE_PROJECTILES (+0x1960) and DETONATE_PROJECTILES_MAX_NUM (+0x1964, -1 = none): RL remote detonate.
    pub detonate_projectiles: bool,
    pub detonate_projectiles_max_num: i32,
    /// OVERHEAT_DELAY, HEAT_MAX_PERCENT, HEAT_INCREMENT / DECREMENT: -1 = decl.
    pub overheat_delay: i32,
    pub heat_max_percent: f32,
    pub heat_increment: f32,
    pub heat_decrement: f32,
    /// MOVEMENT_SPEED_SCALE(_ZOOMED) (-1 = decl), MOVEMENT_PREVENT_JUMP, MOVEMENT_SPEED_SCALE_VS_MS_TABLE.
    pub movement_speed_scale: f32,
    pub movement_speed_scale_zoomed: f32,
    pub movement_prevent_jump: bool,
    pub movement_speed_scale_vs_ms_table: String,
    /// RELOAD_RATE_SCALE (-1 = none), FIRE_SOUND, RETICLE_DECL, FX_DECL.
    pub reload_rate_scale: f32,
    pub fire_sound: String,
    pub reticle: String,
    pub fx: String,
}

impl Default for ModeSlots {
    fn default() -> Self {
        Self {
            decl: None,
            ammo: None,
            firing_interval: 0,
            other_fire_mode_firing_interval: -1,
            zoom_fov: -1.0,
            zoom_time: -1,
            zoom_delay: -1,
            zoom_mode: None,
            charge_time: -1,
            charge_timeout: -1,
            charge_values: Vec::new(),
            charge_per_shot_increment: -1.0,
            charge_start_sound: String::new(),
            fire_delay: -1,
            movement_delay: -1,
            shoot_delay_anim_duration_ms: -1,
            override_shoot_state: String::new(),
            override_shoot_state_firing_interval: 0,
            override_shoot_then_normal_shots: 0,
            target_lock_time_sec: -1.0,
            target_lock_fov: -1.0,
            target_out_of_fov_time_sec: -1.0,
            target_unbreakable_lock: false,
            projectile_lock: false,
            detonate_projectiles: false,
            detonate_projectiles_max_num: -1,
            target_recovery_sec: -1.0,
            target_max_targets: -1,
            overheat_delay: -1,
            heat_max_percent: -1.0,
            heat_increment: -1.0,
            heat_decrement: -1.0,
            movement_speed_scale: -1.0,
            movement_speed_scale_zoomed: -1.0,
            movement_prevent_jump: false,
            movement_speed_scale_vs_ms_table: String::new(),
            reload_rate_scale: -1.0,
            fire_sound: String::new(),
            reticle: String::new(),
            fx: String::new(),
        }
    }
}

/// Weapon-wide slots.
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponSlots {
    /// 32 PRIMARY_TRIGGER_MODE (+0x1944), 33 SECONDARY_TRIGGER_MODE (+0x1ad4).
    pub primary_trigger_mode: Option<i32>,
    pub secondary_trigger_mode: Option<i32>,
    /// 74 BURST_MODE (+0x96c, -1 = decl), 75 BURST_SIZE (+0x968, 0 = none), 76 BURST_INTERVAL (+0x970),
    /// 77 BURST_AMMO_COST (+0x974): -1 = decl.
    pub burst_mode: i32,
    pub burst_size: i32,
    pub burst_interval: i32,
    pub burst_ammo_cost: i32,
    /// 125 MOD_MASTERY (+0x1891).
    pub mastery: bool,
    /// CHAINGUN_SPIN_UP_TIME_MS (-1 = weapondata).
    pub chaingun_spin_up_ms: i32,
}

impl Default for WeaponSlots {
    fn default() -> Self {
        Self { primary_trigger_mode: None, secondary_trigger_mode: None, burst_mode: -1, burst_size: 0, burst_interval: -1, burst_ammo_cost: -1, mastery: false, chaingun_spin_up_ms: -1 }
    }
}

/// Use-time values (damage / projectile WMTs evaluated from the base value when a shot is made).
#[derive(Debug, Clone, PartialEq)]
pub struct UseTime {
    pub damage_scale: f32,
    pub headshot_add_damage_scale: f32,
    pub radius_inner_scale: f32,
    pub radius_outer_scale: f32,
    pub penetration_max_num: i32,
    pub penetration_energy: i32,
    pub plasma_emp_lifetime_scale: f32,
    pub dot: bool,
    pub projectile_fx: String,
    pub explode_sound: String,
    pub ai_killed_targets_decl: String,
}

impl Default for UseTime {
    fn default() -> Self {
        Self {
            damage_scale: 1.0,
            headshot_add_damage_scale: 0.0,
            radius_inner_scale: 1.0,
            radius_outer_scale: 1.0,
            penetration_max_num: 0,
            penetration_energy: 0,
            plasma_emp_lifetime_scale: 1.0,
            dot: false,
            projectile_fx: String::new(),
            explode_sound: String::new(),
            ai_killed_targets_decl: String::new(),
        }
    }
}

/// The int helper 0x140ec8400 (float 0x140ec82c0 alike) from 0: ADD / SUBTRACT / MULTIPLY / SET; any other
/// operator logs an error and the modifier is not applied (None).
fn slot_value(op: ModOp, v: f32) -> Option<f32> {
    match op {
        ModOp::Add => Some(v),
        ModOp::Subtract => Some(-v),
        ModOp::Multiply => Some(0.0),
        ModOp::Set => Some(v),
        _ => None,
    }
}

/// Use-time operator on the base value. INTERIM (path not decoded): SET / ADD / SUBTRACT / MULTIPLY.
fn use_value(op: ModOp, base: f32, v: f32) -> f32 {
    match op {
        ModOp::Add => base + v,
        ModOp::Subtract => base - v,
        ModOp::Multiply => base * v,
        _ => v,
    }
}

/// The weapon's applied upgrades: mode slots, weapon slots and use-time values per mode.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub modes: [ModeSlots; 2],
    pub weapon: WeaponSlots,
    pub use_time: [UseTime; 2],
    /// Upgrade names applied, in order (trace output).
    pub upgrades: Vec<String>,
}

impl Applied {
    /// ApplyUpgradeModifier 0x140f2f8d0 for every modifier of `u` (and the modes allFireModes names).
    pub fn apply(&mut self, u: &Upgrade) {
        self.upgrades.push(u.name.clone());
        let modes: &[i32] = if u.all_fire_modes { &[0, 1] } else { std::slice::from_ref(&u.fire_mode) };
        for &um in modes {
            for m in &u.modifiers {
                let mode = if m.fire_mode != -1 { m.fire_mode } else { um }.clamp(0, 1) as usize;
                self.apply_modifier(m, mode);
            }
        }
    }

    fn apply_modifier(&mut self, m: &WeaponModifier, mode: usize) {
        let d = &m.data;
        let fv = d.float.or(d.int.map(|i| i as f32)).unwrap_or(0.0);
        let iv = d.int.or(d.float.map(|f| f as i32)).unwrap_or(0);
        let s = &mut self.modes[mode];
        let w = &mut self.weapon;
        let u = &mut self.use_time[mode];
        let slot_i = |op| slot_value(op, iv as f32).map(|v| v as i32);
        let slot_f = |op| slot_value(op, fv);
        match m.kind.as_str() {
            "DECL_WEAPON" => s.decl = Some(d.decl.clone()).filter(|x| !x.is_empty()),
            "DECL_AMMO" => s.ammo = Some(d.decl.clone()).filter(|x| !x.is_empty()),
            "PRIMARY_TRIGGER_MODE" => w.primary_trigger_mode = d.trigger_mode,
            "SECONDARY_TRIGGER_MODE" => w.secondary_trigger_mode = d.trigger_mode,
            "FIRING_INTERVAL" => s.firing_interval = slot_i(m.op).unwrap_or(s.firing_interval),
            "OTHER_FIRE_MODE_FIRING_INTERVAL" => s.other_fire_mode_firing_interval = slot_i(m.op).unwrap_or(s.other_fire_mode_firing_interval),
            // +0x1918 stores the float of the int value.
            "ZOOM_FOV" => s.zoom_fov = slot_i(m.op).map(|v| v as f32).unwrap_or(s.zoom_fov),
            "ZOOM_TIME" => s.zoom_time = slot_i(m.op).unwrap_or(s.zoom_time),
            "ZOOM_DELAY" => s.zoom_delay = slot_i(m.op).unwrap_or(s.zoom_delay),
            "ZOOM_MODE" => s.zoom_mode = d.zoom_mode,
            "BURST_MODE" => w.burst_mode = d.burst_mode.unwrap_or(w.burst_mode),
            "BURST_SIZE" => w.burst_size = slot_i(m.op).unwrap_or(w.burst_size),
            "BURST_INTERVAL" => w.burst_interval = slot_i(m.op).unwrap_or(w.burst_interval),
            "BURST_AMMO_COST" => w.burst_ammo_cost = slot_i(m.op).unwrap_or(w.burst_ammo_cost),
            // burst_detonate_mastery carries CHARGE_TIMEOUT with no value: the int helper SETs 0.
            "CHARGE_TIME" => s.charge_time = slot_i(m.op).unwrap_or(s.charge_time),
            "CHARGE_TIMEOUT" => s.charge_timeout = slot_i(m.op).unwrap_or(s.charge_timeout),
            "CHARGE_VALUE_MIN" | "CHARGE_VALUE_MAX" | "CHARGE_VALUE_TABLE" => {
                let p = d.charge_property.clone().unwrap_or(ChargeProperty::None);
                let i = match s.charge_values.iter().position(|c| c.0 == p) {
                    Some(i) => i,
                    None => {
                        s.charge_values.push((p, -1.0, -1.0, String::new()));
                        s.charge_values.len() - 1
                    }
                };
                let e = &mut s.charge_values[i];
                match m.kind.as_str() {
                    "CHARGE_VALUE_MIN" => e.1 = slot_f(m.op).unwrap_or(e.1),
                    "CHARGE_VALUE_MAX" => e.2 = slot_f(m.op).unwrap_or(e.2),
                    _ => e.3 = d.table.clone(),
                }
            }
            "CHARGE_PER_SHOT_INCREMENT" => s.charge_per_shot_increment = slot_f(m.op).unwrap_or(s.charge_per_shot_increment),
            "CHARGE_START_SOUND" => s.charge_start_sound = d.sound.clone(),
            "MOD_MASTERY" => w.mastery = true,
            "FIRE_DELAY" => s.fire_delay = slot_i(m.op).unwrap_or(s.fire_delay),
            "MOVEMENT_DELAY" => s.movement_delay = slot_i(m.op).unwrap_or(s.movement_delay),
            "SHOOT_DELAY_ANIM_DURATION_MS" => s.shoot_delay_anim_duration_ms = slot_i(m.op).unwrap_or(s.shoot_delay_anim_duration_ms),
            "OVERRIDE_SHOOT_STATE" => s.override_shoot_state = d.string.clone(),
            "OVERRIDE_SHOOT_STATE_FIRING_INTERVAL" => s.override_shoot_state_firing_interval = slot_i(m.op).unwrap_or(0),
            "OVERRIDE_SHOOT_THEN_THIS_MANY_NORMAL_SHOTS" => s.override_shoot_then_normal_shots = slot_i(m.op).unwrap_or(0),
            "TARGET_LOCK_TIME_SEC" => s.target_lock_time_sec = d.float.unwrap_or(fv),
            "TARGET_RECOVERY_SEC" => s.target_recovery_sec = d.float.unwrap_or(fv),
            "TARGET_MAX_TARGETS" => s.target_max_targets = iv,
            "TARGET_LOCK_FOV" => s.target_lock_fov = d.float.unwrap_or(fv),
            "TARGET_OUT_OF_FOV_TIME_SEC" => s.target_out_of_fov_time_sec = d.float.unwrap_or(fv),
            "TARGET_UNBREAKABLE_LOCK" => s.target_unbreakable_lock = d.bool_.unwrap_or(true),
            "PROJECTILE_LOCK" => s.projectile_lock = d.bool_.unwrap_or(true),
            "DETONATE_PROJECTILES_MAX_NUM" => s.detonate_projectiles_max_num = iv,
            "OVERHEAT_DELAY" => s.overheat_delay = slot_i(m.op).unwrap_or(s.overheat_delay),
            "HEAT_MAX_PERCENT" => s.heat_max_percent = slot_f(m.op).unwrap_or(s.heat_max_percent),
            "HEAT_INCREMENT" => s.heat_increment = slot_f(m.op).unwrap_or(s.heat_increment),
            "HEAT_DECREMENT" => s.heat_decrement = slot_f(m.op).unwrap_or(s.heat_decrement),
            "MOVEMENT_SPEED_SCALE" => s.movement_speed_scale = fv,
            "MOVEMENT_SPEED_SCALE_ZOOMED" => s.movement_speed_scale_zoomed = fv,
            "MOVEMENT_PREVENT_JUMP" => s.movement_prevent_jump = true,
            "MOVEMENT_SPEED_SCALE_VS_MS_TABLE" => s.movement_speed_scale_vs_ms_table = d.table.clone(),
            "RELOAD_RATE_SCALE" => s.reload_rate_scale = fv,
            "FIRE_SOUND" => s.fire_sound = d.sound.clone(),
            "RETICLE_DECL" => s.reticle = d.decl.clone(),
            "FX_DECL" => s.fx = d.decl.clone(),
            "CHAINGUN_SPIN_UP_TIME_MS" => w.chaingun_spin_up_ms = iv,
            "DETONATE_PROJECTILES" => s.detonate_projectiles = d.bool_.unwrap_or(true),
            // Use-time group (from the base value).
            "DAMAGE_SCALE" => u.damage_scale = use_value(m.op, u.damage_scale, fv),
            "DAMAGE_HEADSHOT_ADD_DAMAGE_SCALE" => u.headshot_add_damage_scale = use_value(m.op, u.headshot_add_damage_scale, fv),
            "DAMAGE_RADIUS_INNER_SCALE" => u.radius_inner_scale = use_value(m.op, u.radius_inner_scale, fv),
            "DAMAGE_RADIUS_OUTER_SCALE" => u.radius_outer_scale = use_value(m.op, u.radius_outer_scale, fv),
            "DAMAGE_BULLET_PENETRATION_MAX_NUM" => u.penetration_max_num = use_value(m.op, u.penetration_max_num as f32, fv) as i32,
            "DAMAGE_BULLET_PENETRATION_ENERGY" => u.penetration_energy = use_value(m.op, u.penetration_energy as f32, fv) as i32,
            "PROJECTILE_PLASMA_EMP_LIFETIME" => u.plasma_emp_lifetime_scale = use_value(m.op, u.plasma_emp_lifetime_scale, fv),
            "PROJECTILE_DOT" => u.dot = d.bool_.unwrap_or(true),
            "PROJECTILE_FX_DECL" => u.projectile_fx = d.decl.clone(),
            "PROJECTILE_EXPLODE_SOUND" => u.explode_sound = d.sound.clone(),
            "DAMAGE_PROJECTILE_AI_KILLED_TARGETS_DECL" => u.ai_killed_targets_decl = d.decl.clone(),
            _ => {}
        }
    }
}

/// A weapon's owned / active mods and upgrade levels (the perk component's state for this weapon).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModLoadout {
    /// Per family: owned (base perk unlocked).
    pub owned: Vec<bool>,
    /// Per family: perks bought after the base (0..=3 upgrades, 4 = + mastery).
    pub level: Vec<u8>,
    /// Active family (player+0xc41c), None = no mod active.
    pub active: Option<usize>,
}

impl ModLoadout {
    pub fn new(mods: &WeaponMods) -> Self {
        let n = mods.families.len();
        Self { owned: vec![false; n], level: vec![0; n], active: None }
    }

    /// Testbed unlock: every family owned at `level` (0..=4).
    pub fn unlock_all(mods: &WeaponMods, level: u8) -> Self {
        let n = mods.families.len();
        Self { owned: vec![true; n], level: vec![level.min(4); n], active: None }
    }

    /// The upgrades the active family's ACTIVE perks apply, in perk order: the base perk, the bought
    /// upgrade perks, the mastery perk, then (while mastered) the weaponMastery declUpgrade.
    pub fn applied(&self, mods: &WeaponMods) -> Applied {
        let mut a = Applied::default();
        let Some(fi) = self.active else { return a };
        let Some(f) = mods.families.get(fi) else { return a };
        let lvl = self.level.get(fi).copied().unwrap_or(0) as usize;
        let mut perks: Vec<&Perk> = vec![&f.base];
        perks.extend(f.upgrades.iter().take(lvl.min(3)));
        if lvl >= 4 {
            perks.extend(f.mastery.iter());
        }
        for p in perks {
            for u in &p.upgrades {
                a.apply(u);
            }
        }
        if a.weapon.mastery {
            if let Some(u) = &f.weapon_mastery.upgrade {
                a.apply(u);
            }
        }
        a
    }
}
