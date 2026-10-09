//! Pickups (health, armour, ammo) and demon death drops (BOARD row 19; gamedata/re/pickups/PICKUPS.md).
//!
//! A placed or dropped pickup is an idProp2 entityDef whose `useableComponentDecl` names a propHealth decl
//! (idUseableHealthComponent: health and armour) or a propItem decl (idUseableAmmoComponent: ammo). Touching its
//! trigger (`triggerDef`, an idTrigger clip model) with `lootStyle LOOT_TOUCH` uses it when the component's CanUse
//! passes; a used prop is removed. Single-player props are not respawned (the PropSpawner that respawns pickups is
//! the MP / coop spawner).
//!
//! Health / armour amounts (idUseableHealthComponent::Use 0x1409bdd10): per heal item, amountToHeal (0x1406ed5b0),
//! replaced in the campaign by the item's gameDifficulty decl for the entity's difficultyScaleType when that value
//! is above 0 (DST_DROPPED: gameDifficultyDecl_Dropped; DST_PICKUP: gameDifficultyDecl_Pickup), at least 1; `set`
//! items assign the component (mega health), the others heal it up to its max.

pub mod loot;
pub mod powerups;

use crate::weapons::arsenal::AmmoPool;
use idres::decldb::DeclDb;

/// game difficulty 0..4: EASY, MEDIUM, HARD, ULTRA-VIOLENT, NIGHTMARE.
pub type Difficulty = usize;

/// playerHealthComponent_t (enum table 0x143571060).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Health,
    Armor,
}

/// difficultyScaleType_t of the prop entity (entity +0x1304; enum table 0x143574cf0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleType {
    Invalid,
    DroppedBoss,
    DroppedBossBfg,
    Dropped,
    Pickup,
}

impl ScaleType {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("DST_DROPPED_BOSS") => ScaleType::DroppedBoss,
            Some("DST_DROPPED_BOSS_BFG") => ScaleType::DroppedBossBfg,
            Some("DST_DROPPED") => ScaleType::Dropped,
            Some("DST_PICKUP") => ScaleType::Pickup,
            _ => ScaleType::Invalid,
        }
    }
}

/// An idDeclGameDifficulty (+0x70 easyScale, +0x74 mediumScale, +0x78 hardScale, +0x7c ultraViolentScale,
/// +0x80 nightmareScale; every field 1.0 by default, ctor 0x1406fee40).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameDifficulty(pub [f32; 5]);

impl GameDifficulty {
    pub fn load(db: &DeclDb, name: &str) -> Option<Self> {
        if name.is_empty() || name == "NULL" {
            return None;
        }
        let b = db.get("gamedifficulty", name).ok()?;
        let f = |k: &str| b.f32(&format!("edit.{k}")).unwrap_or(1.0);
        Some(GameDifficulty([f("easyScale"), f("mediumScale"), f("hardScale"), f("ultraViolentScale"), f("nightmareScale")]))
    }

    /// The value for a difficulty (0x1403909d0, and inline in 0x1409bdd10): easy +0x70, medium +0x74, hard +0x78,
    /// and +0x80 (nightmareScale) for both difficulty 3 and 4; +0x7c is never read.
    pub fn value(&self, d: Difficulty) -> f32 {
        match d {
            0..=2 => self.0[d],
            3 | 4 => self.0[4],
            _ => 1.0,
        }
    }
}

/// idHealthT component_t (0x4c bytes; player decl `playerHealth.components[i]` over the class defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthComponent {
    pub cur: f32,
    pub max: f32,
    pub drain_limit: f32,
    pub starting: f32,
    /// +0x28 absorptionCoefficient: the share of a hit this component takes before passing the rest on (armour).
    pub absorption: f32,
}

/// The player's health and armour components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vitals {
    pub health: HealthComponent,
    pub armor: HealthComponent,
}

impl Vitals {
    pub fn get(&self, c: Component) -> &HealthComponent {
        match c {
            Component::Health => &self.health,
            Component::Armor => &self.armor,
        }
    }
    pub fn get_mut(&mut self, c: Component) -> &mut HealthComponent {
        match c {
            Component::Health => &mut self.health,
            Component::Armor => &mut self.armor,
        }
    }

    /// The player's health component taking a hit (entry 0x140423d80 -> idShieldHealthT::Damage, vtable +0x48
    /// 0x140423bd0, from the top component down; per component 0x140dbec20 with absorption on): armour takes
    /// `amount * absorptionCoefficient` until it is empty and passes the rest (the part it did not absorb plus what
    /// exceeded it) to health. INTERIM for health: cur - rest, floored at 0; the "no player death" branch (vtable
    /// +0x78 / +0x88 thresholds) and killThreshold are not ported.
    pub fn hit(&mut self, amount: f32) -> PlayerHit {
        let before = (self.health.cur, self.armor.cur);
        let a = &mut self.armor;
        let absorbed = amount * a.absorption;
        let rest = amount - absorbed;
        let pass = if absorbed >= a.cur {
            let extra = absorbed - a.cur;
            a.cur = 0.0;
            extra + rest
        } else {
            a.cur -= absorbed;
            rest
        };
        if pass > 0.0 {
            self.health.cur = (self.health.cur - pass).max(0.0);
        }
        PlayerHit { amount, health: (before.0, self.health.cur), armor: (before.1, self.armor.cur) }
    }
}

/// One hit on the player: the damage and the (before, after) of each component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerHit {
    pub amount: f32,
    pub health: (f32, f32),
    pub armor: (f32, f32),
}

/// The player's components from the player entityDef's `playerHealth.components` over the component_t defaults
/// (idShieldHealthT ctor 0x140db26b0: max 100, drainLimit 100, cur 100, starting 100, absorptionCoefficient 1);
/// current = starting.
/// The SP player: health 100 / 100, armour 0 / 50.
pub fn player_vitals(db: &DeclDb) -> Vitals {
    let p = db.get("entitydef", "player").ok();
    let comp = |i: usize| {
        let b = p.as_ref().and_then(|p| p.block(&format!("edit.playerHealth.components.components[{i}]")).cloned()).unwrap_or_default();
        let f = |k: &str| b.f32(k).unwrap_or(100.0);
        let starting = f("starting");
        HealthComponent { cur: starting, max: f("max"), drain_limit: f("drainLimit"), starting, absorption: b.f32("absorptionCoefficient").unwrap_or(1.0) }
    };
    Vitals { health: comp(0), armor: comp(1) }
}

/// idDeclProp_HealthComponent::healLimitType_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealLimit {
    None,
    Absolute,
    Starting,
    DrainLimit,
    Max,
}

/// idDeclProp_HealthComponent::healItem_t (0x38 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct HealItem {
    /// +0x00 componentToHeal, default HEALTH.
    pub component: Component,
    /// +0x08 amountToHeal.
    pub amount: i32,
    /// +0x0c bonusAmountToHeal (armour only, with a suit perk this port does not have).
    pub bonus: i32,
    /// +0x14 healLimitType / +0x18 healLimit.
    pub limit_type: HealLimit,
    pub limit: i32,
    /// +0x1c countSpecifier: amount is a percentage of the component max.
    pub percentage: bool,
    /// +0x20 overflow, +0x21 set, +0x22 healToMaxPlusAmount.
    pub overflow: bool,
    pub set: bool,
    pub heal_to_max_plus: bool,
    /// +0x28 gameDifficultyDecl_Dropped, +0x30 gameDifficultyDecl_Pickup.
    pub dropped: Option<GameDifficulty>,
    pub pickup: Option<GameDifficulty>,
}

/// A propHealth decl (idDeclProp_HealthComponent, used by idUseableHealthComponent).
#[derive(Debug, Clone, PartialEq)]
pub struct HealthDecl {
    pub name: String,
    /// +0x108 canBePickedUpIfNoEffect.
    pub always_usable: bool,
    /// +0x10c totalHealingForAllComponents (0 = no limit).
    pub total: f32,
    pub items: Vec<HealItem>,
    /// idDeclProp_Component +0x78 sound_pickup.
    pub sound: Option<String>,
    /// idDeclProp_UseComponent +0x100 pickupEffect (an fxCondition_t name).
    pub effect: Option<String>,
}

fn named(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty() && *s != "NULL").map(str::to_string)
}

impl HealthDecl {
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("prophealth", name)?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let mut items = Vec::new();
        if let Some(list) = e.block("healItems") {
            let n = list.f32("num").unwrap_or(0.0) as usize;
            for k in 0..n {
                let Some(it) = list.block(&format!("item[{k}]")) else { continue };
                let flag = |key: &str| it.path(key).and_then(|v| v.as_bool()).unwrap_or(false);
                items.push(HealItem {
                    component: if it.str("componentToHeal") == Some("PLAYER_HEALTH_COMPONENT_ARMOR") { Component::Armor } else { Component::Health },
                    amount: it.f32("amountToHeal").unwrap_or(0.0) as i32,
                    bonus: it.f32("bonusAmountToHeal").unwrap_or(0.0) as i32,
                    limit_type: match it.str("healLimitType") {
                        Some("HEAL_LIMIT_ABSOLUTE") => HealLimit::Absolute,
                        Some("HEAL_LIMIT_STARTING") => HealLimit::Starting,
                        Some("HEAL_LIMIT_DRAINLIMIT") => HealLimit::DrainLimit,
                        Some("HEAL_LIMIT_MAX") => HealLimit::Max,
                        _ => HealLimit::None,
                    },
                    limit: it.f32("healLimit").unwrap_or(0.0) as i32,
                    percentage: it.str("countSpecifier").is_some_and(|s| s.ends_with("PERCENTAGE")),
                    overflow: flag("overflow"),
                    set: flag("set"),
                    heal_to_max_plus: flag("healToMaxPlusAmount"),
                    dropped: it.str("gameDifficultyDecl_Dropped").and_then(|n| GameDifficulty::load(db, n)),
                    pickup: it.str("gameDifficultyDecl_Pickup").and_then(|n| GameDifficulty::load(db, n)),
                });
            }
        }
        Ok(HealthDecl {
            name: name.to_string(),
            always_usable: e.path("canBePickedUpIfNoEffect").and_then(|v| v.as_bool()).unwrap_or(false),
            total: e.f32("totalHealingForAllComponents").unwrap_or(0.0),
            items,
            sound: named(e.str("sound_pickup")),
            effect: named(e.str("pickupEffect")),
        })
    }

    /// 0x1406ed5b0: an item's base amount (amountToHeal; with countSpecifier a percentage of the component max).
    pub fn base_amount(item: &HealItem, vitals: &Vitals) -> i32 {
        if !item.percentage {
            return item.amount;
        }
        (vitals.get(item.component).max * (item.amount as f32 / 100.0)) as i32
    }

    /// CalculateSpawnAmount's per-item amount for one component: the base amounts of its heal items, summed.
    pub fn drop_amount(&self, c: Component, vitals: &Vitals) -> i32 {
        self.items.iter().filter(|i| i.component == c).map(|i| Self::base_amount(i, vitals)).sum()
    }
}

/// A propItem decl with idUseableAmmoComponent (idDeclProp_ItemPickupComponent).
#[derive(Debug, Clone, PartialEq)]
pub struct AmmoItem {
    pub name: String,
    /// +0x108 inventoryDecl (an idDeclAmmo), lower case: the arsenal's pool key.
    pub pool: String,
    /// +0x118 countSpecifier (percentage of the ammo's maxCount) and +0x11c inventoryCount (a float).
    pub percentage: bool,
    pub count: f32,
    pub sound: Option<String>,
    /// The ammo decl's gameDifficultyDecl_Dropped (+0x320) / gameDifficultyDecl_Pickup (+0x328).
    pub dropped: Option<GameDifficulty>,
    pub pickup: Option<GameDifficulty>,
    /// idDeclAmmo +0x51d isBfgAmmo: given as counted, no difficulty.
    pub bfg: bool,
}

impl AmmoItem {
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("propitem", name)?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let pool = e.str("inventoryDecl").unwrap_or_default().to_lowercase();
        let ammo = db.get("ammo", &pool)?;
        let diff = |k: &str| ammo.str(&format!("edit.{k}")).and_then(|n| GameDifficulty::load(db, n));
        Ok(AmmoItem {
            name: name.to_string(),
            percentage: e.str("countSpecifier").is_some_and(|s| s.ends_with("PERCENTAGE")),
            count: e.f32("inventoryCount").unwrap_or(0.0),
            sound: named(e.str("sound_pickup")),
            dropped: diff("gameDifficultyDecl_Dropped"),
            pickup: diff("gameDifficultyDecl_Pickup"),
            bfg: ammo.path("edit.isBfgAmmo").and_then(|v| v.as_bool()).unwrap_or(false),
            pool,
        })
    }

    /// idUseableItemComponent's give (0x1409ae110) for the campaign: the item count, replaced for DST_DROPPED /
    /// DST_PICKUP props by the ammo decl's gameDifficulty value (truncated), and by the pool's max for
    /// DST_DROPPED_BOSS; BFG ammo keeps its count. Then `floor(count * multiplier + 0.5)`; `suit_scale` is the suit's
    /// ammo bonus for the prop's scale type (Suit::ammo_drop_scale 0x140b850d0 for DST_DROPPED, ammo_pickup_scale
    /// 0x140b85160 for DST_PICKUP, 1 otherwise); BFG ammo ignores it.
    pub fn give_count(&self, max: i32, scale: ScaleType, difficulty: Difficulty, suit_scale: f32) -> i32 {
        let mut count = self.count_for_max(max);
        let mut mult = 1.0;
        if !self.bfg {
            match scale {
                ScaleType::Dropped => count = self.dropped.map_or(count, |g| g.value(difficulty) as i32),
                ScaleType::Pickup => count = self.pickup.map_or(count, |g| g.value(difficulty) as i32),
                ScaleType::DroppedBoss => count = max,
                _ => {}
            }
            mult = suit_scale;
        }
        (count as f32 * mult + 0.5).floor() as i32
    }

    /// 0x1406ed650: the item's count, `(int)(maxCount * inventoryCount / 100)` with the percentage specifier,
    /// else `(int)inventoryCount`.
    pub fn count_for_max(&self, max: i32) -> i32 {
        if self.percentage { (max as f32 * (self.count / 100.0)) as i32 } else { self.count as i32 }
    }
}

/// What a pickup gives.
#[derive(Debug, Clone, PartialEq)]
pub enum Useable {
    Health(HealthDecl),
    Ammo(AmmoItem),
    /// A powerup (propStatusEffect: idUseableStatusEffectComponent).
    StatusEffect(powerups::PropStatusEffect),
}

/// The touch volume of an idTrigger `triggerDef` (CLIPMODEL_CYLINDER / BOX `size`, plus `offset` only with
/// ignoreUnfixCollisionOffsetBug; idClipModelInfo +0x40).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trigger {
    /// Half extents of the bounds (x, y, z).
    pub half: glam::Vec3,
    /// Bounds centre relative to the entity origin.
    pub centre: glam::Vec3,
    pub cylinder: bool,
}

impl Trigger {
    /// Whether the player's clip box (`lo`..`hi`, world) touches the trigger of a prop at `origin`.
    pub fn touches(&self, origin: glam::Vec3, lo: glam::Vec3, hi: glam::Vec3) -> bool {
        let c = origin + self.centre;
        if hi.z < c.z - self.half.z || lo.z > c.z + self.half.z {
            return false;
        }
        // Nearest point of the player's box to the trigger axis.
        let p = glam::Vec2::new(c.x.clamp(lo.x, hi.x), c.y.clamp(lo.y, hi.y));
        let d = p - c.truncate();
        if self.cylinder {
            d.length() <= self.half.x
        } else {
            d.x.abs() <= self.half.x && d.y.abs() <= self.half.y
        }
    }
}

/// A pickup entityDef (idProp2).
#[derive(Debug, Clone, PartialEq)]
pub struct PickupDef {
    pub name: String,
    pub model: Option<String>,
    /// renderModelInfo.scale (default 1).
    pub model_scale: glam::Vec3,
    pub useable: Useable,
    pub trigger: Trigger,
    /// difficultyScaleType.
    pub scale: ScaleType,
    /// droppedFromAI (dropped items count in loot tracking while they lie in the world).
    pub dropped_from_ai: bool,
}

impl PickupDef {
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("entitydef", name)?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let useable_name = e.str("useableComponentDecl").unwrap_or_default();
        let useable = match HealthDecl::load(db, useable_name) {
            Ok(h) => Useable::Health(h),
            Err(_) => match powerups::PropStatusEffect::load(db, useable_name) {
                Ok(p) => Useable::StatusEffect(p),
                Err(_) => Useable::Ammo(AmmoItem::load(db, useable_name)?),
            },
        };
        let trigger_def = e.str("triggerDef").unwrap_or("triggers/prop/default");
        let t = db.get("entitydef", trigger_def)?;
        let cm = t.block("edit.clipModelInfo").cloned().unwrap_or_default();
        let v = |k: &str, d: f32| glam::Vec3::new(cm.f32(&format!("{k}.x")).unwrap_or(d), cm.f32(&format!("{k}.y")).unwrap_or(d), cm.f32(&format!("{k}.z")).unwrap_or(d));
        let size = v("size", 0.0);
        let offset = if cm.path("ignoreUnfixCollisionOffsetBug").and_then(|v| v.as_bool()).unwrap_or(false) { v("offset", 0.0) } else { glam::Vec3::ZERO };
        // INTERIM: bounds = size centred on the origin + offset (the clipModelInfo -> bounds code was not traced).
        let trigger = Trigger { half: size * 0.5, centre: offset, cylinder: cm.str("type") == Some("CLIPMODEL_CYLINDER") };
        let sc = |k: &str| e.f32(&format!("renderModelInfo.scale.{k}")).unwrap_or(1.0);
        Ok(PickupDef {
            name: name.to_string(),
            model: named(e.str("renderModelInfo.model")),
            model_scale: glam::Vec3::new(sc("x"), sc("y"), sc("z")),
            useable,
            trigger,
            scale: ScaleType::parse(e.str("difficultyScaleType")),
            dropped_from_ai: e.path("droppedFromAI").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

/// One component's change from a pickup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Healed {
    pub component: Component,
    pub before: f32,
    pub after: f32,
}

impl HealthDecl {
    /// The amount Use gives for one item (before limits): the base amount, replaced by the difficulty decl of the
    /// entity's scale type when > 0 (campaign, FUN_14038fbf0 == 0), at least 1.
    pub fn use_amount(item: &HealItem, vitals: &Vitals, scale: ScaleType, difficulty: Difficulty) -> f32 {
        let mut amount = Self::base_amount(item, vitals) as f32;
        let v = match scale {
            ScaleType::Dropped => item.dropped.map(|g| g.value(difficulty)),
            ScaleType::Pickup => item.pickup.map(|g| g.value(difficulty)),
            _ => None,
        };
        if let Some(v) = v.filter(|v| *v > 0.0) {
            amount = v;
        }
        amount.max(1.0)
    }

    /// idUseableHealthComponent CanUse (vtable +0x78, 0x1409b0880): canBePickedUpIfNoEffect, or any item with an
    /// amount that sets, heals to max+amount, is below its heal limit, or can heal its component.
    pub fn can_use(&self, vitals: &Vitals) -> bool {
        if self.always_usable {
            return true;
        }
        self.items.iter().filter(|i| i.amount > 0).any(|i| {
            if i.set || i.heal_to_max_plus {
                return true;
            }
            // A limit (truncated to int) below 1 counts as none; under a limit the component must also heal.
            match limit_value(i, vitals).map(|l| l as i32) {
                Some(l) if l >= 1 => vitals.get(i.component).cur < l as f32 && can_heal(vitals, i.component, false),
                _ => can_heal(vitals, i.component, i.overflow),
            }
        })
    }

    /// idUseableHealthComponent::Use (vtable +0x88, 0x1409bdd10). Returns the changes; empty = not used.
    pub fn apply(&self, vitals: &mut Vitals, scale: ScaleType, difficulty: Difficulty) -> Vec<Healed> {
        let mut out = Vec::new();
        let mut healed = false;
        let limited = self.total > 0.0;
        let mut remaining = self.total;
        for item in &self.items {
            let mut amount = Self::use_amount(item, vitals, scale, difficulty);
            if limited && remaining < amount {
                amount = remaining;
            }
            let before = vitals.get(item.component).cur;
            let limit = limit_value(item, vitals).unwrap_or(0.0);
            if limit <= 0.0 {
                if amount > 0.0 {
                    if !item.set {
                        healed |= heal(vitals, item.component, amount, item.overflow);
                    } else {
                        let mut target = amount;
                        if item.heal_to_max_plus {
                            target = (vitals.get(item.component).max / 25.0).floor() * 25.0 + amount;
                        }
                        if before < target {
                            set(vitals, item.component, target);
                        }
                        healed = true;
                    }
                }
            } else {
                let new = (before + amount).min(limit);
                if new <= before {
                    amount = 0.0;
                } else {
                    amount = new - before;
                    healed |= heal(vitals, item.component, amount, false);
                }
            }
            let after = vitals.get(item.component).cur;
            if after != before {
                out.push(Healed { component: item.component, before, after });
            }
            remaining = (remaining - amount).max(0.0);
        }
        if !healed {
            out.clear();
        }
        out
    }
}

/// healLimitType's limit: ABSOLUTE healLimit, STARTING / DRAINLIMIT / MAX the component's (health vtable +0xa8 /
/// +0xb0 / +0xa0); None for HEAL_LIMIT_NONE.
fn limit_value(item: &HealItem, vitals: &Vitals) -> Option<f32> {
    let c = vitals.get(item.component);
    match item.limit_type {
        HealLimit::None => None,
        HealLimit::Absolute => Some(item.limit as f32),
        HealLimit::Starting => Some(c.starting),
        HealLimit::DrainLimit => Some(c.drain_limit),
        HealLimit::Max => Some(c.max),
    }
}

/// idHealthT CanHeal (vtable +0x68, 0x140423820): below the max, or with overflow the next component can heal.
fn can_heal(vitals: &Vitals, c: Component, overflow: bool) -> bool {
    let h = vitals.get(c);
    if h.max > h.cur {
        return true;
    }
    overflow && c == Component::Health && can_heal(vitals, Component::Armor, overflow)
}

/// idPlayerHealth Heal (vtable +0x58, 0x1406b9510 -> idHealthT::Heal 0x1404246e0, "above max" argument false): the
/// cap is max(cur, max), so an overcharged value is kept. Armour (component 1) starts from max(cur, 0) and is capped;
/// health is capped and, with overflow, the excess heals armour. True when a value changed.
fn heal(vitals: &mut Vitals, c: Component, amount: f32, overflow: bool) -> bool {
    if amount < 0.0 {
        return false;
    }
    let h = vitals.get_mut(c);
    let old = h.cur;
    let cap = old.max(h.max);
    let mut spilled = false;
    match c {
        Component::Armor => {
            h.cur = h.cur.max(0.0) + amount;
            if h.cur >= cap {
                h.cur = cap;
            }
        }
        Component::Health => {
            h.cur = old + amount;
            let excess = h.cur - cap;
            if excess > 0.0 {
                h.cur = cap;
                if overflow {
                    spilled = heal(vitals, Component::Armor, excess, overflow);
                }
            }
        }
    }
    spilled || vitals.get(c).cur != old
}

/// idPlayerHealth Set (0x140428e90 -> vtable +0xb8 0x140428ef0): stores the value unclamped (health <= 0 is ignored).
fn set(vitals: &mut Vitals, c: Component, value: f32) {
    if c == Component::Health && value <= 0.0 {
        return;
    }
    vitals.get_mut(c).cur = value;
}

impl AmmoItem {
    /// idUseableAmmoComponent CanUse (vtable +0x78, 0x1409b03b0): a weapon in the inventory uses this ammo and its
    /// count is below the max. The arsenal makes a pool only for ammo its weapons use.
    pub fn can_use(&self, pools: &[AmmoPool]) -> bool {
        pools.iter().any(|p| p.key == self.pool && p.count < p.max)
    }

    /// Use: adds [`AmmoItem::give_count`] to the pool. INTERIM: capped at the pool max (the inventory add's own cap
    /// was not traced). Returns (before, after), or None if unusable.
    pub fn apply(&self, pools: &mut [AmmoPool], scale: ScaleType, difficulty: Difficulty, suit_scale: f32) -> Option<(i32, i32)> {
        if !self.can_use(pools) {
            return None;
        }
        let p = pools.iter_mut().find(|p| p.key == self.pool)?;
        let before = p.count;
        p.count = (p.count + self.give_count(p.max, scale, difficulty, suit_scale)).min(p.max);
        Some((before, p.count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(cur: f32, max: f32) -> HealthComponent {
        HealthComponent { cur, max, drain_limit: max, starting: max, absorption: 1.0 }
    }

    fn vitals(health: f32, armor: f32) -> Vitals {
        Vitals { health: comp(health, 100.0), armor: HealthComponent { starting: 0.0, ..comp(armor, 50.0) } }
    }

    fn item(c: Component, amount: i32) -> HealItem {
        HealItem {
            component: c,
            amount,
            bonus: 0,
            limit_type: HealLimit::None,
            limit: 0,
            percentage: false,
            overflow: false,
            set: false,
            heal_to_max_plus: false,
            dropped: None,
            pickup: None,
        }
    }

    fn decl(items: Vec<HealItem>) -> HealthDecl {
        HealthDecl { name: "t".into(), always_usable: false, total: 0.0, items, sound: None, effect: None }
    }

    #[test]
    fn heal_caps_and_overflow() {
        let mut v = vitals(90.0, 0.0);
        assert!(heal(&mut v, Component::Health, 25.0, false));
        assert_eq!(v.health.cur, 100.0);
        // At the max nothing changes: Heal reports false and the pickup is not used.
        assert!(!heal(&mut v, Component::Health, 25.0, false));
        // Overflow spills the excess into armour.
        let mut v = vitals(90.0, 10.0);
        assert!(heal(&mut v, Component::Health, 25.0, true));
        assert_eq!((v.health.cur, v.armor.cur), (100.0, 25.0));
        // An overcharged value is kept (cap = max(cur, max)).
        let mut v = vitals(150.0, 0.0);
        assert!(!heal(&mut v, Component::Health, 25.0, false));
        assert_eq!(v.health.cur, 150.0);
        // Armour starts from max(cur, 0).
        let mut v = vitals(100.0, -3.0);
        assert!(heal(&mut v, Component::Armor, 5.0, false));
        assert_eq!(v.armor.cur, 5.0);
    }

    #[test]
    fn can_use_rules() {
        let h = decl(vec![item(Component::Health, 25)]);
        assert!(h.can_use(&vitals(99.0, 0.0)));
        assert!(!h.can_use(&vitals(100.0, 0.0)));
        // Mega-health style `set` items are always usable and assign unclamped.
        let mut mega = decl(vec![HealItem { set: true, ..item(Component::Health, 200) }, HealItem { set: true, ..item(Component::Armor, 200) }]);
        let mut v = vitals(100.0, 50.0);
        assert!(mega.can_use(&v));
        assert_eq!(mega.apply(&mut v, ScaleType::Invalid, 1).len(), 2);
        assert_eq!((v.health.cur, v.armor.cur), (200.0, 200.0));
        // Set does not lower a higher value but still counts as used.
        let mut v = vitals(100.0, 0.0);
        v.health.cur = 250.0;
        assert_eq!(mega.apply(&mut v, ScaleType::Invalid, 1).len(), 1);
        assert_eq!(v.health.cur, 250.0);
        mega.items.clear();
        assert!(!mega.can_use(&v));
        // healLimit ABSOLUTE 60: below it heals up to it, at it the item cannot be used.
        let lim = decl(vec![HealItem { limit_type: HealLimit::Absolute, limit: 60, ..item(Component::Health, 25) }]);
        let mut v = vitals(50.0, 0.0);
        assert!(lim.can_use(&v));
        lim.apply(&mut v, ScaleType::Invalid, 1);
        assert_eq!(v.health.cur, 60.0);
        assert!(!lim.can_use(&v));
    }

    #[test]
    fn difficulty_replaces_amount() {
        let g = GameDifficulty([8.0, 5.0, 3.0, 7.0, 1.1]);
        // Difficulty 3 reads nightmareScale like 4 (+0x7c is never read).
        assert_eq!([0, 1, 2, 3, 4].map(|d| g.value(d)), [8.0, 5.0, 3.0, 1.1, 1.1]);
        let it = HealItem { pickup: Some(g), dropped: Some(GameDifficulty([0.0; 5])), ..item(Component::Health, 5) };
        let v = vitals(10.0, 0.0);
        assert_eq!(HealthDecl::use_amount(&it, &v, ScaleType::Pickup, 0), 8.0);
        // A value not above 0 keeps amountToHeal; no scale type keeps it too.
        assert_eq!(HealthDecl::use_amount(&it, &v, ScaleType::Dropped, 0), 5.0);
        assert_eq!(HealthDecl::use_amount(&it, &v, ScaleType::Invalid, 0), 5.0);
        // At least 1.
        let tiny = HealItem { pickup: Some(GameDifficulty([0.25; 5])), ..it };
        assert_eq!(HealthDecl::use_amount(&tiny, &v, ScaleType::Pickup, 3), 1.0);
    }

    #[test]
    fn trigger_touch() {
        // triggers/prop/sp_health: 48 x 48 x 16 cylinder.
        let t = Trigger { half: glam::Vec3::new(24.0, 24.0, 8.0), centre: glam::Vec3::ZERO, cylinder: true };
        let o = glam::Vec3::new(100.0, 0.0, 0.0);
        let at = |x: f32| (glam::Vec3::new(x - 16.0, -16.0, 0.0), glam::Vec3::new(x + 16.0, 16.0, 74.0));
        let (lo, hi) = at(100.0 - 40.0);
        assert!(t.touches(o, lo, hi));
        let (lo, hi) = at(100.0 - 41.0);
        assert!(!t.touches(o, lo, hi));
    }

    /// The decls the campaign places, per difficulty (install-backed).
    #[test]
    fn placed_pickups_from_decls() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let db = &inst.decls;
        let pv = player_vitals(db);
        assert_eq!((pv.health.cur, pv.health.max, pv.armor.cur, pv.armor.max), (100.0, 100.0, 0.0, 50.0));
        let amount = |def: &str, d: Difficulty| -> Vec<f32> {
            let p = PickupDef::load(db, def).unwrap();
            let Useable::Health(h) = &p.useable else { panic!("{def}") };
            h.items.iter().map(|i| HealthDecl::use_amount(i, &pv, p.scale, d)).collect()
        };
        let per_difficulty = |def: &str| (0..5).map(|d| amount(def, d)[0]).collect::<Vec<_>>();
        assert_eq!(per_difficulty("prop/health/health_vial"), [8.0, 5.0, 3.0, 1.1, 1.1]);
        assert_eq!(per_difficulty("prop/health/health_pickup25"), [30.0, 25.0, 20.0, 15.0, 15.0]);
        assert_eq!(per_difficulty("prop/health/health_pickup50"), [50.0, 50.0, 40.0, 30.0, 30.0]);
        assert_eq!(per_difficulty("prop/armor/armor_shard"), [5.0, 5.0, 3.0, 1.0, 1.0]);
        assert_eq!(per_difficulty("prop/zion/lootpinata/items/health_small"), [15.0, 10.0, 8.0, 5.0, 5.0]);
        assert_eq!(amount("prop/powerup/megahealth", 1), [200.0, 200.0]);
        // A medium vial at 50 health, then +50, +25 (refused at full health), armour and mega.
        let use_def = |def: &str, v: &mut Vitals| {
            let p = PickupDef::load(db, def).unwrap();
            let Useable::Health(h) = &p.useable else { panic!("{def}") };
            if h.can_use(v) { h.apply(v, p.scale, 1).len() } else { 0 }
        };
        let mut v = pv;
        v.health.cur = 50.0;
        assert_eq!(use_def("prop/health/health_vial", &mut v), 1);
        assert_eq!(v.health.cur, 55.0);
        assert_eq!(use_def("prop/health/health_pickup50", &mut v), 1);
        assert_eq!(v.health.cur, 100.0);
        assert_eq!(use_def("prop/health/health_pickup25", &mut v), 0, "full health refuses health");
        assert_eq!(use_def("prop/armor/armor_pickup50", &mut v), 1);
        assert_eq!(v.armor.cur, 50.0);
        assert_eq!(use_def("prop/armor/armor_shard", &mut v), 0, "full armour refuses a shard");
        assert_eq!(use_def("prop/powerup/megahealth", &mut v), 2);
        assert_eq!((v.health.cur, v.armor.cur), (200.0, 200.0));
        // Placed shells give the ammo decl's ammopickup_shotgun value (8), capped at the pool max.
        let p = PickupDef::load(db, "prop/ammo/shotgun_ammo").unwrap();
        let Useable::Ammo(a) = &p.useable else { panic!() };
        assert_eq!(a.pool, "ammo/zion/sharedammopool/shells");
        let mut pools = vec![AmmoPool { key: a.pool.clone(), count: 5, max: 20 }];
        assert_eq!(a.apply(&mut pools, p.scale, 1, 1.0), Some((5, 13)));
        assert_eq!(a.apply(&mut pools, p.scale, 1, 1.0), Some((13, 20)));
        assert_eq!(a.apply(&mut pools, p.scale, 1, 1.0), None, "full ammo refuses");
        for (def, count) in [("prop/ammo/heavy_rifle_ammo", 20), ("prop/ammo/plasma_rifle_ammo", 30), ("prop/ammo/rocket_launcher", 5)] {
            let p = PickupDef::load(db, def).unwrap();
            let Useable::Ammo(a) = &p.useable else { panic!("{def}") };
            assert_eq!(a.give_count(500, p.scale, 1, 1.0), count, "{def}");
        }
        // Dropped shells (lootpinata ammo_shotgun_10, DST_DROPPED): ammodropped_shotgun per difficulty.
        let p = PickupDef::load(db, "prop/zion/ammo/lootpinata/items/ammo_shotgun_10").unwrap();
        let Useable::Ammo(a) = &p.useable else { panic!() };
        assert_eq!((0..5).map(|d| a.give_count(20, p.scale, d, 1.0)).collect::<Vec<_>>(), [5, 5, 4, 3, 3]);
    }
}
