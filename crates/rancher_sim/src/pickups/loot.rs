//! Demon death drops: idLootDropComponent::DropLoot (0x1406c15a0) over the entityDef's
//! `lootDropComponent.droppableItems` (idDeclLootDrop decls), with idLootDropComponent::CalculateSpawnAmount
//! (0x1406c0980) per decl (gamedata/re/pickups/PICKUPS.md).
//!
//! DropLoot: only while numDroppedItems (+0x28) < maxDroppedItems (+0x2c), not (onlyDropOnce && hasDroppedLoot) and
//! loot_enable. Pass 1 asks every decl for its count (clamped to the room left) and its deficiency fraction; pass 2
//! walks the decls in order and, for a count > 0, by dropMode: CHANCE_OVERALL rolls once (`rand % 101 <
//! chance`), CHANCE_PER_ITEM rolls per item, ALWAYS drops them all. chance = (int)((dropChanceMax -
//! dropChanceMin) * deficiency + dropChanceMin). Each item is a `dropEntity` event posted to the owner
//! (0x1406c13c0) with a delay of (numDroppedItems + 1) * item_dropTimeIntervalMS + rand % 251 ms.
//!
//! The random numbers are gameLocal's idRandom (gameLocal +0x285be8): seed = seed * 1664525 + 1013904223,
//! value = (seed >> 10) & 0x7fff.

use super::{AmmoItem, HealthDecl, Vitals};
use crate::upgrades::Suit;
use crate::weapons::arsenal::AmmoPool;

/// lootDropMode_t (enum table 0x14356f310).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropMode {
    ChanceOverall,
    ChancePerItem,
    Always,
}

/// lootDropRestriction_t (enum table 0x14356ed00).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restriction {
    None,
    NormalKill,
    GloryKill,
    NormalOrGloryKill,
    ChainsawKill,
    EnvironmentSuit,
    DemoKill,
    RestrictAll,
}

/// lootItemType_t (enum table 0x14356e050).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemType {
    None,
    Ammo,
    Health,
    Armor,
    Energy,
    Equipment,
    Misc,
}

/// lootItemAmountCalculation_t (enum table 0x14356e7d0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountCalc {
    Explicit,
    Percentage,
    Needed,
    ExplicitMaxDrop,
}

/// How the demon died, for dropRestriction (CalculateSpawnAmount switch): a glory kill is the AI's sync-melee
/// handle (AI +0x12340) being set; a chainsaw kill is the last damage's type flags (AI +0x50b0 -> +0xf8) == 0x100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillKind {
    Normal,
    Glory,
    Chainsaw,
}

/// The item a loot decl drops (its `decl`, an idDeclProp_Component).
#[derive(Debug, Clone)]
pub enum LootItem {
    Health(HealthDecl),
    Ammo(AmmoItem),
    /// Anything this port does not drop (energy, equipment, misc, upgrade points).
    Other,
}

/// One idDeclLootDrop. Defaults are the decl ctor's (0x1406f44f0).
#[derive(Debug, Clone)]
pub struct LootDrop {
    pub name: String,
    /// +0x70, default CHANCE_OVERALL.
    pub drop_mode: DropMode,
    /// +0x74, default NORMAL_KILL.
    pub restriction: Restriction,
    /// +0x78 ignoreChallengeRestriction (only matters for challenge flags; the campaign has none).
    pub ignore_challenge: bool,
    /// +0x7c value_MinConditional, default 0; +0x80 value_MaxConditional, default 100 (percent of max).
    pub value_min: f32,
    pub value_max: f32,
    /// +0x84, default LOOT_NONE.
    pub item_type: ItemType,
    /// +0x88 the entityDef dropped.
    pub entity_def: String,
    /// +0x90, default NEEDED.
    pub amount_calc: AmountCalc,
    /// +0x94, default -1 (no cap).
    pub max_drop: i32,
    /// +0x98 / +0x9c, defaults 45 / 80.
    pub chance_min: i32,
    pub chance_max: i32,
    /// +0xa0, default 20000.
    pub removal_ms: i32,
    /// +0xa8 decl name and what it gives.
    pub decl: String,
    pub item: LootItem,
    /// +0xb0 item_percentage, default 100.
    pub percentage: f32,
    /// +0xb4 explicitSpawnOffset, default zero.
    pub spawn_offset: [f32; 3],
    /// +0xc0 item_dropTimeIntervalMS, default 0.
    pub interval_ms: i32,
    /// +0xc4 useForwardVectorForTrajectory, default false.
    pub use_forward: bool,
    /// +0xc8 / +0xcc item_Min/MaxLaunchVelocity, defaults 50 / 100 (not passed to the dropEntity event).
    pub launch_min: f32,
    pub launch_max: f32,
    /// +0xd0 envSuitRestricted, default false: needs the suit's +0xd4 envSuitModType equipment mod (0x140b858a0).
    pub env_suit_restricted: bool,
    pub env_suit_mod: Option<String>,
    /// +0xd8 ammoRequiresWeapon, default true; +0xdc ammoItemMaxCount, default 0.
    pub ammo_requires_weapon: bool,
    pub ammo_item_max_count: i32,
}

fn enum_value<T: Copy>(s: Option<&str>, names: &[(&str, T)], default: T) -> T {
    s.and_then(|s| names.iter().find(|(n, _)| *n == s).map(|(_, v)| *v)).unwrap_or(default)
}

impl LootDrop {
    /// Reads `lootdrop/<name>` and its item decl from the install.
    pub fn load(db: &idres::decldb::DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("lootdrop", name)?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let f = |k: &str, d: f32| e.f32(k).unwrap_or(d);
        let i = |k: &str, d: i32| e.f32(k).map(|v| v as i32).unwrap_or(d);
        let flag = |k: &str, d: bool| e.path(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let drop_mode = enum_value(
            e.str("dropMode"),
            &[("LOOT_DROPMODE_CHANCE_OVERALL", DropMode::ChanceOverall), ("LOOT_DROPMODE_CHANCE_PER_ITEM", DropMode::ChancePerItem), ("LOOT_DROPMODE_ALWAYS", DropMode::Always)],
            DropMode::ChanceOverall,
        );
        let restriction = enum_value(
            e.str("dropRestriction"),
            &[
                ("LOOT_DROP_RESTRICTION_NONE", Restriction::None),
                ("LOOT_DROP_RESTRICTION_NORMAL_KILL", Restriction::NormalKill),
                ("LOOT_DROP_RESTRICTION_GLORY_KILL", Restriction::GloryKill),
                ("LOOT_DROP_RESTRICTION_NORMAL_KILL_OR_GLORY_KILL", Restriction::NormalOrGloryKill),
                ("LOOT_DROP_RESTRICTION_CHAINSAW_KILL", Restriction::ChainsawKill),
                ("LOOT_DROP_RESTRICTION_ENVIRONMENT_SUIT", Restriction::EnvironmentSuit),
                ("LOOT_DROP_DEMO_KILL", Restriction::DemoKill),
                ("LOOT_DROP_RESTRICT_ALL", Restriction::RestrictAll),
            ],
            Restriction::NormalKill,
        );
        let item_type = enum_value(
            e.str("itemType"),
            &[
                ("LOOT_NONE", ItemType::None),
                ("LOOT_AMMO", ItemType::Ammo),
                ("LOOT_HEALTH", ItemType::Health),
                ("LOOT_ARMOR", ItemType::Armor),
                ("LOOT_ENERGY", ItemType::Energy),
                ("LOOT_EQUIPMENT", ItemType::Equipment),
                ("LOOT_MISC", ItemType::Misc),
            ],
            ItemType::None,
        );
        let amount_calc = enum_value(
            e.str("item_amt_calc"),
            &[
                ("LOOT_ITEM_AMT_CALC_EXPLICIT", AmountCalc::Explicit),
                ("LOOT_ITEM_AMT_CALC_PERCENTAGE", AmountCalc::Percentage),
                ("LOOT_ITEM_AMT_CALC_NEEDED", AmountCalc::Needed),
                ("LOOT_ITEM_AMT_CALC_EXPLICIT_MAX_DROP", AmountCalc::ExplicitMaxDrop),
            ],
            AmountCalc::Needed,
        );
        let decl = e.str("decl").filter(|s| *s != "NULL").unwrap_or_default().to_string();
        let item = match item_type {
            ItemType::Health | ItemType::Armor => HealthDecl::load(db, &decl).map(LootItem::Health).unwrap_or(LootItem::Other),
            ItemType::Ammo => AmmoItem::load(db, &decl).map(LootItem::Ammo).unwrap_or(LootItem::Other),
            _ => LootItem::Other,
        };
        let off = |k: &str| e.f32(&format!("explicitSpawnOffset.{k}")).unwrap_or(0.0);
        Ok(LootDrop {
            name: name.to_string(),
            drop_mode,
            restriction,
            ignore_challenge: flag("ignoreChallengeRestriction", false),
            value_min: f("value_MinConditional", 0.0),
            value_max: f("value_MaxConditional", 100.0),
            item_type,
            entity_def: e.str("entityDef").filter(|s| *s != "NULL").unwrap_or_default().to_string(),
            amount_calc,
            max_drop: i("item_maxDrop", -1),
            chance_min: i("item_dropChanceMin", 45),
            chance_max: i("item_dropChanceMax", 80),
            removal_ms: i("item_removalTimeMS", 20000),
            decl,
            item,
            percentage: f("item_percentage", 100.0),
            spawn_offset: [off("x"), off("y"), off("z")],
            interval_ms: i("item_dropTimeIntervalMS", 0),
            use_forward: flag("useForwardVectorForTrajectory", false),
            launch_min: f("item_MinLaunchVelocity", 50.0),
            launch_max: f("item_MaxLaunchVelocity", 100.0),
            env_suit_restricted: flag("envSuitRestricted", false),
            env_suit_mod: e.str("envSuitModType").map(str::to_string),
            ammo_requires_weapon: flag("ammoRequiresWeapon", true),
            ammo_item_max_count: i("ammoItemMaxCount", 0),
        })
    }

    /// Whether the decl's dropRestriction lets this kill drop it (the CalculateSpawnAmount switch), and an
    /// envSuitRestricted decl's equipment mod is in the suit (0x140b858a0). ENVIRONMENT_SUIT asks the suit
    /// (func 0x140b82f90, not traced): INTERIM taken as "Ammo Boost mastered" (the only SP decl using it is the
    /// rune's BFG drop, ammo_boost_upgrade_drops, matching the rune's ammoDropModBFGPercentage). DEMO_KILL tests a
    /// demo game-mode flag (FUN_141566eb0 +0x28 +0x30) that is never set in the campaign.
    pub fn allowed(&self, kill: KillKind, suit: &Suit) -> bool {
        if self.env_suit_restricted && !self.env_suit_mod.as_deref().is_some_and(|m| suit.has(m)) {
            return false;
        }
        match self.restriction {
            Restriction::None => true,
            Restriction::NormalKill => kill == KillKind::Normal,
            Restriction::GloryKill => kill == KillKind::Glory,
            Restriction::NormalOrGloryKill => kill != KillKind::Chainsaw,
            Restriction::ChainsawKill => kill == KillKind::Chainsaw,
            Restriction::EnvironmentSuit => suit.is_mastered(crate::upgrades::MODIFY_AMMO_DROPS) && suit.has(crate::upgrades::MODIFY_AMMO_DROPS),
            Restriction::DemoKill | Restriction::RestrictAll => false,
        }
    }
}

/// What the player has of the item a decl drops, for CalculateSpawnAmount.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Holding {
    /// Current value (health, armour or ammo count) plus, with loot_tracking (default 1), the amount still lying
    /// in the world in dropped items of this entityDef (func 0x140e26940).
    pub cur: f32,
    pub max: f32,
    /// item_maxDrop after the suit: health maxDrop * the health-drop multiplier (0x140b85560), armour from the
    /// glory-kill armour mod (0x140b851a0) when it gives any.
    pub max_drop: i32,
}

/// CalculateSpawnAmount's result: how many items, the deficiency fraction the chance interpolates with, and the
/// amount one item gives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnAmount {
    pub count: i32,
    pub deficiency: f32,
    pub per_item: i32,
}

/// The per-item amount and holding of a decl's item for this player (the type branches of 0x1406c0980). `pools`
/// are the player's ammo pools; an ammo pool's presence stands for "owns a weapon using it" (the arsenal only makes
/// pools for weapons it has). None = no drop (wrong item decl, or ammo the player has no weapon for).
pub fn holding(d: &LootDrop, vitals: &Vitals, pools: &[AmmoPool], pending: f32, suit: &Suit) -> Option<(i32, Holding)> {
    let mut max_drop = d.max_drop;
    let (per_item, cur, max) = match (&d.item, d.item_type) {
        // HEALTH: the sum of the healItems on PLAYER_HEALTH_COMPONENT_HEALTH (0x1406ed5b0 each: amountToHeal, or a
        // percentage of the component max with countSpecifier), against health cur / max (+0xd8 / +0xe0); a
        // maxDrop other than -1 is scaled by the suit's health-drop multiplier.
        (LootItem::Health(h), ItemType::Health) => {
            if max_drop != -1 {
                max_drop = (suit.health_drop_scale(vitals.health.cur) * max_drop as f32) as i32;
            }
            (h.drop_amount(super::Component::Health, vitals), vitals.health.cur, vitals.health.max)
        }
        // ARMOR: the glory-kill armour mod's count (its rune upgrade counter, idPlayer +0x47d70, is 1 once
        // mastered), when it gives any, is both maxDrop and the per-item amount; else the sum over
        // PLAYER_HEALTH_COMPONENT_ARMOR. Against armour cur / max.
        (LootItem::Health(h), ItemType::Armor) => {
            let n = suit.glory_kill_armor_drops(usize::from(suit.is_mastered(crate::upgrades::GLORY_KILLS_AWARD_ARMOR)));
            let per_item = if n != 0 {
                max_drop = n;
                n
            } else {
                h.drop_amount(super::Component::Armor, vitals)
            };
            (per_item, vitals.armor.cur, vitals.armor.max)
        }
        // AMMO: the item's count (0x1406ed650); with ammoRequiresWeapon the ammo item and a weapon using it must
        // exist, else the decl's ammoItemMaxCount stands in for an absent pool (current 0).
        (LootItem::Ammo(a), ItemType::Ammo) => {
            let pool = pools.iter().find(|p| p.key == a.pool);
            match pool {
                Some(p) => (a.count_for_max(p.max), p.count as f32, p.max as f32),
                None if d.ammo_requires_weapon => return None,
                None => (a.count_for_max(d.ammo_item_max_count), 0.0, d.ammo_item_max_count as f32),
            }
        }
        _ => return None,
    };
    Some((per_item, Holding { cur: cur + pending, max, max_drop }))
}

/// idLootDropComponent::CalculateSpawnAmount (0x1406c0980) for a decl that passed [`LootDrop::allowed`], the
/// player's holding and the item's per-item amount (from [`holding`]).
pub fn spawn_amount(d: &LootDrop, per_item: i32, h: Holding) -> SpawnAmount {
    // pct = floor(clamp(cur / max * 100, 0, 100)) (floorf 0x141ed13c8).
    let pct = ((h.cur / h.max) * 100.0).clamp(0.0, 100.0).floor();
    let none = SpawnAmount { count: 0, deficiency: 0.0, per_item: 0 };
    if d.value_max < pct || pct < d.value_min || per_item < 1 {
        return none;
    }
    let count = match d.amount_calc {
        AmountCalc::Explicit | AmountCalc::ExplicitMaxDrop => h.max_drop,
        // Enough items to fill the gap, the last one partial (fmodf 0x141ed7cf8), capped by item_maxDrop.
        AmountCalc::Needed => {
            let gap = h.max - h.cur;
            let n = (gap / per_item as f32) as i32 + i32::from(gap % per_item as f32 != 0.0);
            if h.max_drop != -1 && h.max_drop < n { h.max_drop } else { n }
        }
        // item_percentage of the max, in items.
        AmountCalc::Percentage => (d.percentage * 0.01 * h.max).max(0.0) as i32 / per_item,
    };
    SpawnAmount { count, deficiency: 1.0 - (pct - d.value_min) / (d.value_max - d.value_min), per_item }
}

/// gameLocal's idRandom (gameLocal +0x285be8; LCG 1664525 / 1013904223, 15-bit output `(seed >> 10) & 0x7fff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdRandom(pub u32);

impl IdRandom {
    pub fn random_int(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 10) & 0x7fff
    }
}

/// idLootDropComponent's per-entity state (+0x28 numDroppedItems, +0x2c maxDroppedItems, +0x68 onlyDropOnce,
/// +0x69 hasDroppedLoot).
#[derive(Debug, Clone)]
pub struct LootDropComponent {
    pub items: Vec<LootDrop>,
    pub num_dropped: i32,
    pub max_dropped: i32,
    pub only_drop_once: bool,
    pub has_dropped: bool,
}

/// One item to spawn: the dropEntity event's arguments and when it fires.
#[derive(Debug, Clone, PartialEq)]
pub struct Drop {
    /// Index into [`LootDropComponent::items`].
    pub decl: usize,
    pub entity_def: String,
    pub delay_ms: i32,
    pub spawn_offset: [f32; 3],
    pub removal_ms: i32,
    pub use_forward: bool,
    /// What one item gives (CalculateSpawnAmount's per-item amount, before the dropped item's own difficulty
    /// scaling at pickup).
    pub per_item: i32,
}

impl LootDropComponent {
    /// The entityDef's `lootDropComponent` (droppableItems; maxDroppedItems / onlyDropOnce from the block when set).
    pub fn load(db: &idres::decldb::DeclDb, entity_def: &str) -> anyhow::Result<Self> {
        let e = db.get("entitydef", entity_def)?;
        let c = e.block("edit.lootDropComponent").cloned().unwrap_or_default();
        let mut items = Vec::new();
        if let Some(list) = c.block("droppableItems") {
            let n = list.f32("num").unwrap_or(0.0) as usize;
            for k in 0..n {
                if let Some(name) = list.str(&format!("item[{k}]")) {
                    items.push(LootDrop::load(db, name)?);
                }
            }
        }
        Ok(LootDropComponent {
            items,
            num_dropped: 0,
            // ctor 0x1406c0830: maxDroppedItems 100.
            max_dropped: c.f32("maxDroppedItems").map(|v| v as i32).unwrap_or(100),
            only_drop_once: c.path("onlyDropOnce").and_then(|v| v.as_bool()).unwrap_or(false),
            has_dropped: false,
        })
    }

    /// idLootDropComponent::DropLoot (0x1406c15a0) for a kill: `hold(decl)` gives the per-item amount and holding
    /// (see [`holding`]), None when the decl cannot drop for this player.
    pub fn drop_loot(&mut self, kill: KillKind, suit: &Suit, rng: &mut IdRandom, mut hold: impl FnMut(&LootDrop) -> Option<(i32, Holding)>) -> Vec<Drop> {
        let mut out = Vec::new();
        if self.num_dropped >= self.max_dropped || (self.only_drop_once && self.has_dropped) {
            return out;
        }
        let room = self.max_dropped - self.num_dropped;
        let amounts: Vec<SpawnAmount> = self
            .items
            .iter()
            .map(|d| {
                let a = match (d.allowed(kill, suit), hold(d)) {
                    (true, Some((per_item, h))) => spawn_amount(d, per_item, h),
                    _ => SpawnAmount { count: 0, deficiency: 0.0, per_item: 0 },
                };
                SpawnAmount { count: a.count.min(room), ..a }
            })
            .collect();
        for (k, a) in amounts.iter().enumerate() {
            if a.count <= 0 {
                continue;
            }
            let d = &self.items[k];
            let chance = ((d.chance_max as f32 - d.chance_min as f32) * a.deficiency + d.chance_min as f32) as i32;
            let roll = |rng: &mut IdRandom| ((rng.random_int() % 101) as i32) < chance;
            let overall = d.drop_mode != DropMode::ChanceOverall || roll(rng);
            for _ in 0..a.count {
                if !overall || (d.drop_mode == DropMode::ChancePerItem && !roll(rng)) {
                    continue;
                }
                // 0x1406c13c0: the event delay takes one more random number.
                let delay_ms = (self.num_dropped + 1) * d.interval_ms + (rng.random_int() % 251) as i32;
                out.push(Drop {
                    decl: k,
                    entity_def: d.entity_def.clone(),
                    delay_ms,
                    spawn_offset: d.spawn_offset,
                    removal_ms: d.removal_ms,
                    use_forward: d.use_forward,
                    per_item: a.per_item,
                });
                self.num_dropped += 1;
            }
        }
        self.has_dropped = true;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pickups::{HealthComponent, Vitals};

    fn drop(calc: AmountCalc, min: f32, max: f32, max_drop: i32) -> LootDrop {
        LootDrop {
            name: "t".into(),
            drop_mode: DropMode::ChanceOverall,
            restriction: Restriction::None,
            ignore_challenge: false,
            value_min: min,
            value_max: max,
            item_type: ItemType::Health,
            entity_def: "e".into(),
            amount_calc: calc,
            max_drop,
            chance_min: 20,
            chance_max: 40,
            removal_ms: 20000,
            decl: String::new(),
            item: LootItem::Other,
            percentage: 100.0,
            spawn_offset: [0.0; 3],
            interval_ms: 0,
            use_forward: false,
            launch_min: 50.0,
            launch_max: 100.0,
            env_suit_restricted: false,
            env_suit_mod: None,
            ammo_requires_weapon: true,
            ammo_item_max_count: 0,
        }
    }

    #[test]
    fn spawn_amounts() {
        let h = |cur: f32, max_drop: i32| Holding { cur, max: 100.0, max_drop };
        // EXPLICIT: item_maxDrop inside [min, max] (percent, floored), nothing outside.
        let d = drop(AmountCalc::Explicit, 0.0, 25.0, 4);
        assert_eq!(spawn_amount(&d, 10, h(20.0, 4)), SpawnAmount { count: 4, deficiency: 1.0 - 20.0 / 25.0, per_item: 10 });
        assert_eq!(spawn_amount(&d, 10, h(25.9, 4)).count, 4, "25.9% floors to 25");
        assert_eq!(spawn_amount(&d, 10, h(26.0, 4)).count, 0);
        assert_eq!(spawn_amount(&d, 0, h(10.0, 4)).count, 0, "per-item amount below 1");
        // NEEDED: enough to fill, the last item partial, capped by item_maxDrop.
        let n = drop(AmountCalc::Needed, 0.0, 100.0, -1);
        assert_eq!(spawn_amount(&n, 10, h(65.0, -1)).count, 4);
        assert_eq!(spawn_amount(&n, 10, h(70.0, -1)).count, 3);
        assert_eq!(spawn_amount(&drop(AmountCalc::Needed, 0.0, 100.0, 2), 10, h(10.0, 2)).count, 2);
        // PERCENTAGE: item_percentage of the max, in items.
        let p = LootDrop { percentage: 50.0, ..drop(AmountCalc::Percentage, 0.0, 101.0, -1) };
        assert_eq!(spawn_amount(&p, 10, Holding { cur: 20.0, max: 50.0, max_drop: -1 }).count, 2);
    }

    #[test]
    fn restrictions() {
        let r = |restriction| LootDrop { restriction, ..drop(AmountCalc::Explicit, 0.0, 100.0, 1) };
        assert!(r(Restriction::None).allowed(KillKind::Chainsaw, &Suit::default()));
        assert!(r(Restriction::NormalKill).allowed(KillKind::Normal, &Suit::default()));
        assert!(!r(Restriction::NormalKill).allowed(KillKind::Glory, &Suit::default()));
        assert!(!r(Restriction::GloryKill).allowed(KillKind::Normal, &Suit::default()));
        assert!(r(Restriction::NormalOrGloryKill).allowed(KillKind::Glory, &Suit::default()));
        assert!(!r(Restriction::NormalOrGloryKill).allowed(KillKind::Chainsaw, &Suit::default()));
        assert!(r(Restriction::ChainsawKill).allowed(KillKind::Chainsaw, &Suit::default()));
        assert!(!r(Restriction::EnvironmentSuit).allowed(KillKind::Normal, &Suit::default()));
        assert!(!LootDrop { env_suit_restricted: true, ..r(Restriction::None) }.allowed(KillKind::Normal, &Suit::default()));
    }

    #[test]
    fn rolls_and_delays() {
        // seed 0: first values 7100 (% 101 = 30), then the next.
        let mut rng = IdRandom(0);
        assert_eq!(rng.random_int(), 7100);
        // CHANCE_OVERALL at full deficiency: chance 40, roll 30 -> all 3 drop; each spawn takes one more number.
        let mut c = LootDropComponent { items: vec![drop(AmountCalc::Explicit, 0.0, 25.0, 3)], num_dropped: 0, max_dropped: 100, only_drop_once: false, has_dropped: false };
        let mut rng = IdRandom(0);
        let out = c.drop_loot(KillKind::Normal, &Suit::default(), &mut rng, |_| Some((10, Holding { cur: 0.0, max: 100.0, max_drop: 3 })));
        assert_eq!(out.len(), 3);
        let mut r2 = IdRandom(0);
        r2.random_int();
        let delays: Vec<i32> = (0..3).map(|_| (r2.random_int() % 251) as i32).collect();
        assert_eq!(out.iter().map(|d| d.delay_ms).collect::<Vec<_>>(), delays);
        assert_eq!(rng, r2);
        assert_eq!(c.num_dropped, 3);
        // maxDroppedItems caps the count.
        let mut c = LootDropComponent { max_dropped: 2, num_dropped: 0, ..c };
        assert_eq!(c.drop_loot(KillKind::Normal, &Suit::default(), &mut IdRandom(0), |_| Some((10, Holding { cur: 0.0, max: 100.0, max_drop: 3 }))).len(), 2);
        assert!(c.drop_loot(KillKind::Normal, &Suit::default(), &mut IdRandom(0), |_| Some((10, Holding { cur: 0.0, max: 100.0, max_drop: 3 }))).is_empty());
    }

    /// The Possessed's droppableItems on a normal kill (install-backed).
    #[test]
    fn possessed_drops() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let db = &inst.decls;
        let comp = LootDropComponent::load(db, crate::demons::POSSESSED).unwrap();
        assert_eq!(comp.items.len(), 24);
        assert_eq!(comp.max_dropped, 100);
        let h = &comp.items[0];
        assert_eq!((h.name.as_str(), h.restriction, h.value_min, h.value_max, h.amount_calc, h.max_drop, h.chance_min, h.chance_max), ("zion/health/health10_explicit_0_20pct", Restriction::None, 0.0, 25.0, AmountCalc::Explicit, 4, 100, 100));
        assert_eq!((h.drop_mode, h.removal_ms, h.entity_def.as_str()), (DropMode::ChanceOverall, 20000, "prop/zion/lootpinata/items/health_small"));
        let pools = |count: i32| {
            ["shells", "bullets", "cells", "rockets"]
                .map(|n| AmmoPool { key: format!("ammo/zion/sharedammopool/{n}"), count, max: 100 })
                .to_vec()
        };
        let vit = |health: f32| Vitals {
            health: HealthComponent { cur: health, max: 100.0, drain_limit: 100.0, starting: 100.0, absorption: 1.0 },
            armor: HealthComponent { cur: 0.0, max: 50.0, drain_limit: 50.0, starting: 0.0, absorption: 1.0 },
        };
        let suit = Suit::default();
        let run = |health: f32, ammo: i32, seed: u32| {
            let mut c = comp.clone();
            let (v, p) = (vit(health), pools(ammo));
            c.drop_loot(KillKind::Normal, &suit, &mut IdRandom(seed), |d| holding(d, &v, &p, 0.0, &suit)).into_iter().map(|d| (c.items[d.decl].name.clone(), d.per_item)).collect::<Vec<_>>()
        };
        // Full health and ammo: nothing.
        assert!(run(100.0, 100, 0).is_empty());
        // 20 health, full ammo: four 10-health items (chance 100 of 101; seed 0 rolls 30).
        let v = run(20.0, 100, 0);
        assert_eq!(v, vec![("zion/health/health10_explicit_0_20pct".to_string(), 10); 4]);
        // 30 health: two items (26..40 band).
        assert_eq!(run(30.0, 100, 0).len(), 2);
        // 50 health: the 41..60 band at chance (int)(66 * (1 - 9/19)) = 34: seed 0 rolls 30 -> two items.
        assert_eq!(run(50.0, 100, 0), vec![("zion/health/health10_explicit_41_80pct".to_string(), 10); 2]);
        // Empty ammo at full health: one item per ammo type at most, by chance (shotgun 40, rockets 15, cells 0..50
        // band does not apply at 0 %, bullets 40).
        let got = run(100.0, 0, 0);
        assert!(got.iter().all(|(n, _)| n.contains("explicit_0_20pct") && !n.ends_with("_gk")), "{got:?}");
    }
}
