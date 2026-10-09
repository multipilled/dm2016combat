//! Powerups (BOARD row 26): the status effects SP powerup props give (prop/zion/statuseffects/sp/*: Berserk,
//! Quad Damage, Haste, Invulnerability), their timers and effects, the Praetor suit's powerup upgrades, and the Saving
//! Throw rune's focus (gamedata/re/powerups/POWERUPS.md).
//!
//! A powerup prop's `useableComponentDecl` is a propStatusEffect decl (idUseableStatusEffectComponent) naming a
//! statusEffect decl (idDeclStatusEffect); its `effectClass` picks the behaviour: idStatusEffect_Berzerk,
//! idStatusEffect_BonusDamage, idStatusEffect_Haste, idStatusEffect_Invulnerability.

use idres::decl::Block;
use idres::decldb::DeclDb;

use crate::upgrades::{ACTIVATE_FOCUS_ON_DEATH_BLOW, Suit};

/// The effect classes the SP powerups use.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectKind {
    Berserk,
    BonusDamage,
    Haste,
    Invulnerability,
    SlowMotion,
    Other(String),
}

/// An idDeclStatusEffect (0x218 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct StatusEffectDecl {
    pub name: String,
    pub kind: EffectKind,
    /// +0x78 lifeTimeMS (-1 = forever).
    pub lifetime_ms: i32,
    /// +0x88 noRefresh, +0xf7 isPowerUp, +0xf5 pauseDuringGloryKill.
    pub no_refresh: bool,
    pub is_powerup: bool,
    /// +0x90 startUsingSound, +0x98 stopUsingSound.
    pub start_sound: Option<String>,
    pub stop_sound: Option<String>,
    /// +0xc0 runningOutSound, +0xc8 runningOutSound_Time, +0xcc runningOutSound_Interval. INTERIM: an omitted Time
    /// is taken as 0 (no warning) and an omitted Interval as "once"; the decl defaults were not found.
    pub running_out_sound: Option<String>,
    pub running_out_time_ms: i32,
    pub running_out_interval_ms: i32,
    /// +0xac start_owner_StartFXCondition, +0xb4 stop_owner_StartFXCondition (screen FX conditions).
    pub start_fx: Option<String>,
    pub stop_fx: Option<String>,
    /// berserkData (+0x138): +0x140 damageMitigationMult (0 none .. 1 all), +0x144 speedBoostMult.
    pub mitigation: f32,
    pub speed_boost: f32,
    /// bonusDamageData (+0x148) bonusDamageAmount: extra damage fraction (1 = double).
    pub bonus_damage: f32,
    /// hasteData (+0x1b4) movementSpeedScalar, (+0x1b8) firingIntervalScalar.
    pub haste_move: f32,
    pub haste_fire: f32,
}

fn named(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty() && *s != "NULL").map(str::to_string)
}

impl StatusEffectDecl {
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("statuseffect", &name.to_lowercase())?;
        let e = b.block("edit").cloned().unwrap_or_default();
        Ok(Self::parse(name, &e))
    }

    fn parse(name: &str, e: &Block) -> Self {
        let f = |k: &str, d: f32| e.f32(k).unwrap_or(d);
        let flag = |k: &str| e.path(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let kind = match e.str("effectClass") {
            Some("idStatusEffect_Berzerk") => EffectKind::Berserk,
            Some("idStatusEffect_BonusDamage") => EffectKind::BonusDamage,
            Some("idStatusEffect_Haste") => EffectKind::Haste,
            Some("idStatusEffect_Invulnerability") => EffectKind::Invulnerability,
            Some("idStatusEffect_SlowMotion") => EffectKind::SlowMotion,
            other => EffectKind::Other(other.unwrap_or_default().to_string()),
        };
        StatusEffectDecl {
            name: name.to_string(),
            kind,
            lifetime_ms: f("lifeTimeMS", -1.0) as i32,
            no_refresh: flag("noRefresh"),
            is_powerup: flag("isPowerUp"),
            start_sound: named(e.str("startUsingSound")),
            stop_sound: named(e.str("stopUsingSound")),
            running_out_sound: named(e.str("runningOutSound")),
            running_out_time_ms: f("runningOutSound_Time", 0.0) as i32,
            running_out_interval_ms: f("runningOutSound_Interval", 0.0) as i32,
            start_fx: named(e.str("start_owner_StartFXCondition")),
            stop_fx: named(e.str("stop_owner_StartFXCondition")),
            // INTERIM defaults for omitted data: no mitigation, no boost (the struct defaults were not found).
            mitigation: f("berserkData.damageMitigationMult", 0.0),
            speed_boost: f("berserkData.speedBoostMult", 1.0),
            bonus_damage: f("bonusDamageData.bonusDamageAmount", 0.0),
            haste_move: f("hasteData.movementSpeedScalar", 1.0),
            haste_fire: f("hasteData.firingIntervalScalar", 1.0),
        }
    }
}

/// A propStatusEffect decl (idUseableStatusEffectComponent): the effect it gives and its sounds.
#[derive(Debug, Clone, PartialEq)]
pub struct PropStatusEffect {
    pub name: String,
    pub effect: StatusEffectDecl,
    /// idDeclProp_Component sound_use / sound_pickup.
    pub use_sound: Option<String>,
    pub pickup_sound: Option<String>,
}

impl PropStatusEffect {
    pub fn load(db: &DeclDb, name: &str) -> anyhow::Result<Self> {
        let b = db.get("propstatuseffect", &name.to_lowercase())?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let effect = e.str("statusEffect").ok_or_else(|| anyhow::anyhow!("{name}: no statusEffect"))?;
        Ok(PropStatusEffect {
            name: name.to_string(),
            effect: StatusEffectDecl::load(db, effect)?,
            use_sound: named(e.str("sound_use")),
            pickup_sound: named(e.str("sound_pickup")),
        })
    }
}

/// A running status effect (idStatusEffect +0x58 timeLeft).
#[derive(Debug, Clone, PartialEq)]
pub struct Active {
    pub decl: StatusEffectDecl,
    pub time_left_ms: i32,
    pub lifetime_ms: i32,
    /// Next running-out warning, as time left (ms).
    pub warn_at_ms: Option<i32>,
}

/// What a frame of the effects did.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectEvent {
    RunningOut { name: String, sound: Option<String> },
    Ended { name: String, sound: Option<String>, fx: Option<String> },
}

/// What applying an effect did.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Started { lifetime_ms: i32 },
    Refreshed { lifetime_ms: i32 },
    /// noRefresh and already running.
    Rejected,
}

/// The player's status effects (idStatusEffectComponent).
#[derive(Debug, Clone, Default)]
pub struct Powerups {
    pub active: Vec<Active>,
}

/// idStatusEffect lifetime (0x140e93f50): statusEffect_lifetimeSec when > 0, else the decl's lifeTimeMS; a powerup's is
/// `(lifetime + duration add) * duration mult * scale` with the suit's ABILITY_MOD_POWERUP_DURATION_MOD values
/// (idPlayer +0x55b64 / +0x55b60). INTERIM: the player ability scale (+0x28000 component, 7) taken as 1.
pub fn lifetime(decl: &StatusEffectDecl, suit: &Suit, lifetime_cvar_sec: f32) -> i32 {
    if lifetime_cvar_sec > 0.0 {
        return (lifetime_cvar_sec * 1000.0) as i32;
    }
    let l = decl.lifetime_ms;
    if !decl.is_powerup || l == -1 {
        return l;
    }
    let (add, mult) = suit.powerup_duration.unwrap_or((0, 1.0));
    ((l + add) as f32 * mult) as i32
}

impl Powerups {
    pub fn get(&self, kind: &EffectKind) -> Option<&Active> {
        self.active.iter().find(|a| &a.decl.kind == kind)
    }

    /// idStatusEffectComponent::AddStatusEffect -> ApplyEffect (0x140e935c0) / RefreshEffect (0x140e950f0): a new
    /// effect starts with timeLeft = lifetime; a running one of the same decl is refreshed (timeLeft = lifetime)
    /// unless noRefresh.
    pub fn apply(&mut self, decl: &StatusEffectDecl, suit: &Suit, lifetime_cvar_sec: f32) -> Applied {
        let life = lifetime(decl, suit, lifetime_cvar_sec);
        let warn = |life: i32| (decl.running_out_sound.is_some() && decl.running_out_time_ms > 0 && life != -1).then_some(decl.running_out_time_ms);
        if let Some(a) = self.active.iter_mut().find(|a| a.decl.name == decl.name) {
            if decl.no_refresh {
                return Applied::Rejected;
            }
            a.time_left_ms = life;
            a.lifetime_ms = life;
            a.warn_at_ms = warn(life);
            return Applied::Refreshed { lifetime_ms: life };
        }
        self.active.push(Active { decl: decl.clone(), time_left_ms: life, lifetime_ms: life, warn_at_ms: warn(life) });
        Applied::Started { lifetime_ms: life }
    }

    /// One frame of game time: timeLeft counts down (INTERIM: by the frame's milliseconds; the component's tick
    /// was not traced), the running-out sound plays once timeLeft reaches runningOutSound_Time (then every
    /// runningOutSound_Interval), and an effect at 0 ends (RemoveEffect 0x140e951c0: stop sound, stop FX condition).
    pub fn tick(&mut self, ms: i32) -> Vec<EffectEvent> {
        let mut out = Vec::new();
        for a in &mut self.active {
            if a.lifetime_ms == -1 {
                continue;
            }
            a.time_left_ms = (a.time_left_ms - ms).max(0);
            if let Some(w) = a.warn_at_ms
                && a.time_left_ms <= w
                && a.time_left_ms > 0
            {
                out.push(EffectEvent::RunningOut { name: a.decl.name.clone(), sound: a.decl.running_out_sound.clone() });
                a.warn_at_ms = (a.decl.running_out_interval_ms > 0).then(|| w - a.decl.running_out_interval_ms).filter(|w| *w > 0);
            }
        }
        self.active.retain(|a| {
            let done = a.lifetime_ms != -1 && a.time_left_ms <= 0;
            if done {
                out.push(EffectEvent::Ended { name: a.decl.name.clone(), sound: a.decl.stop_sound.clone(), fx: a.decl.stop_fx.clone() });
            }
            !done
        });
        out
    }

    /// Invulnerability sets idPlayer +0x45ea9 bit 0 on start (0x140e9a2e0) and clears it on end (0x140e9a4d0);
    /// idPlayer::Damage (0x140dbee00) drops the hit while it is set.
    pub fn invulnerable(&self) -> bool {
        self.get(&EffectKind::Invulnerability).is_some()
    }

    /// Damage taken: 0 while invulnerable; Berserk keeps `1 - damageMitigationMult` (field comment: 0 = no
    /// mitigation, 1 = mitigate all). INTERIM: where idPlayer::Damage reads the mitigation was not traced.
    pub fn damage_taken_scale(&self) -> f32 {
        if self.invulnerable() {
            return 0.0;
        }
        self.get(&EffectKind::Berserk).map_or(1.0, |b| 1.0 - b.decl.mitigation)
    }

    /// Damage dealt: 1 + bonusDamageAmount per bonus-damage effect (idStatusEffect_BonusDamage vtable +0x78
    /// 0x140e9a640 returns the amount; field comment: 1.0 = double), so Quad Damage (3) is x4.
    pub fn damage_dealt_scale(&self) -> f32 {
        self.active.iter().filter(|a| a.decl.kind == EffectKind::BonusDamage).map(|a| 1.0 + a.decl.bonus_damage).product()
    }

    /// Movement speed: Berserk's speedBoostMult (written to idPlayer +0x16f58 on start 0x140e96fc0, reset to 1 on
    /// end) and Haste's movementSpeedScalar (vtable +0x88 0x140e99030). INTERIM: combined as a product.
    pub fn move_speed_scale(&self) -> f32 {
        let b = self.get(&EffectKind::Berserk).map_or(1.0, |b| b.decl.speed_boost);
        let h = self.get(&EffectKind::Haste).map_or(1.0, |h| h.decl.haste_move);
        b * h
    }

    /// Haste's firingIntervalScalar (vtable +0x90 0x140e99010), for the weapons' firing interval.
    pub fn fire_interval_scale(&self) -> f32 {
        self.get(&EffectKind::Haste).map_or(1.0, |h| h.decl.haste_fire)
    }

    /// Everything ends (death / respawn).
    pub fn clear(&mut self) -> Vec<EffectEvent> {
        self.active.drain(..).map(|a| EffectEvent::Ended { name: a.decl.name, sound: a.decl.stop_sound, fx: a.decl.stop_fx }).collect()
    }
}

/// The Saving Throw rune's focus (EQUIP_ACTIVATE_FOCUS_ON_DEATH_BLOW; idEnvironmentSuit +0x338, entered by
/// 0x140b884e0 from the damage path 0x140b829d0): a separate focus health (+0xb08) of focusMaxHealth, invulnerable
/// until now + focusInvulnerableTimeMs, active until now + focusAttackDurationMs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Focus {
    pub health: f32,
    pub ends_ms: i32,
    pub invulnerable_until_ms: i32,
    /// Focus ends once the player's health reaches focusDisablesAtHealth.
    pub disables_at_health: f32,
}

/// The rune's values (idUpgradeMod_Equipment +0x40 focusAttackDurationMs, +0x44 leftOverHealthValue,
/// +0x50 focusInvulnerableTimeMs, +0x54 focusMaxHealth, +0x5c focusDisablesAtHealth, +0x64 focusMaxUses).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavingThrow {
    pub duration_ms: i32,
    pub left_over_health: f32,
    pub invulnerable_ms: i32,
    pub max_health: f32,
    pub disables_at_health: f32,
    pub max_uses: i32,
}

impl SavingThrow {
    /// From the suit's mod, when it has one. The `_Upgraded` focus max health / reset time are picked once the rune
    /// is mastered (idPlayer +0x47d78).
    pub fn from_suit(suit: &Suit) -> Option<Self> {
        let m = suit.equip.get(ACTIVATE_FOCUS_ON_DEATH_BLOW)?;
        let f = &m.focus;
        Some(SavingThrow {
            duration_ms: f.duration_ms,
            left_over_health: f.left_over_health,
            invulnerable_ms: f.invulnerable_ms,
            max_health: if suit.is_mastered(ACTIVATE_FOCUS_ON_DEATH_BLOW) { f.max_health.1 } else { f.max_health.0 },
            disables_at_health: f.disables_at_health,
            max_uses: f.max_uses,
        })
    }

    /// A death blow with focus available: the hit leaves the player at min(health, leftOverHealthValue) (0x140b884e0
    /// returns `health - leftOver` as the damage) and focus starts. INTERIM: a leftOverHealthValue the decl omits is
    /// taken as 1 (the default was not found), and focusMaxUses as once per life (usage rule not traced).
    pub fn enter(&self, now_ms: i32) -> Focus {
        Focus {
            health: self.max_health,
            ends_ms: now_ms + self.duration_ms,
            invulnerable_until_ms: now_ms + self.invulnerable_ms,
            disables_at_health: self.disables_at_health,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pickups::{PickupDef, Useable};
    use crate::upgrades::Perk;

    fn install() -> Option<crate::install::Install> {
        let doom = idres::find_install()?;
        Some(crate::install::load(&doom).expect("loading install"))
    }

    const SP: &str = "perk/zion/player/sp/enviroment_suit/";

    fn effect(db: &DeclDb, def: &str) -> StatusEffectDecl {
        let p = PickupDef::load(db, def).unwrap();
        let Useable::StatusEffect(ps) = p.useable else { panic!("{def}") };
        ps.effect
    }

    /// The four SP powerup props: kinds, lifetimes (with and without the Praetor duration upgrade) and effects.
    #[test]
    fn sp_powerups() {
        let Some(inst) = install() else { return };
        let db = &inst.decls;
        let defs = ["berzerk", "bonusdamage", "haste", "invulnerability"].map(|n| effect(db, &format!("prop/zion/statuseffects/sp/{n}/base")));
        assert_eq!(defs.iter().map(|d| d.kind.clone()).collect::<Vec<_>>(), [EffectKind::Berserk, EffectKind::BonusDamage, EffectKind::Haste, EffectKind::Invulnerability]);
        let mut suit = Suit::default();
        assert_eq!(defs.iter().map(|d| lifetime(d, &suit, 0.0)).collect::<Vec<_>>(), [20000, 20000, 20000, 13500]);
        let mut v = crate::pickups::player_vitals(db);
        let p = Perk::load(db, &format!("{SP}modify_powerup_duration")).unwrap();
        suit.grant(&p, 1, &mut v, &mut [], &[]);
        assert_eq!(suit.powerup_duration, Some((0, 1.5)));
        assert_eq!(defs.iter().map(|d| lifetime(d, &suit, 0.0)).collect::<Vec<_>>(), [30000, 30000, 30000, 20250]);
        // statusEffect_lifetimeSec overrides everything.
        assert_eq!(lifetime(&defs[0], &suit, 2.5), 2500);
        let mut pw = Powerups::default();
        let s0 = Suit::default();
        for d in &defs {
            assert!(matches!(pw.apply(d, &s0, 0.0), Applied::Started { .. }));
        }
        assert_eq!(pw.damage_dealt_scale(), 4.0, "quad: 1 + 3");
        assert_eq!(pw.damage_taken_scale(), 0.0, "invulnerable");
        assert_eq!((pw.move_speed_scale(), pw.fire_interval_scale()), (1.25 * 1.4, 0.6));
        // Invulnerability ends first (13.5 s): then Berserk halves damage taken.
        let ev = pw.tick(13500);
        assert!(ev.iter().any(|e| matches!(e, EffectEvent::Ended { name, .. } if name == "invulnerability/base")), "{ev:?}");
        assert_eq!(pw.damage_taken_scale(), 0.5);
        // Berserk's running-out warning at 3000 ms left, then the end with its stop sound.
        let ev = pw.tick(3500);
        assert!(ev.iter().any(|e| matches!(e, EffectEvent::RunningOut { name, .. } if name == "berzerk/base")), "{ev:?}");
        let ev = pw.tick(3000);
        assert!(ev.iter().any(|e| matches!(e, EffectEvent::Ended { name, sound: Some(_), .. } if name == "berzerk/base")), "{ev:?}");
        assert!(pw.active.is_empty());
        // Picking a running powerup again refreshes it.
        pw.apply(&defs[1], &s0, 0.0);
        pw.tick(15000);
        assert_eq!(pw.apply(&defs[1], &s0, 0.0), Applied::Refreshed { lifetime_ms: 20000 });
        assert_eq!(pw.active[0].time_left_ms, 20000);
    }

    /// Praetor powerup health / armour and the Saving Throw rune.
    #[test]
    fn praetor_and_saving_throw() {
        let Some(inst) = install() else { return };
        let db = &inst.decls;
        let mut v = crate::pickups::player_vitals(db);
        let mut suit = Suit::default();
        for n in ["powerup_health", "powerup_armor", "activate_focus_on_death_blow"] {
            let p = Perk::load(db, &format!("{SP}{n}")).unwrap();
            suit.grant(&p, 1, &mut v, &mut [], &[]);
        }
        let h = suit.powerup_health.clone().unwrap();
        assert_eq!((h.health.0, h.health.1), (-1, true));
        assert!(suit.powerup_armor.is_some());
        let st = SavingThrow::from_suit(&suit).unwrap();
        assert_eq!((st.duration_ms, st.invulnerable_ms, st.max_health, st.disables_at_health), (7000, 2000, 40.0, 40.0));
        let f = st.enter(1000);
        assert_eq!((f.health, f.ends_ms, f.invulnerable_until_ms), (40.0, 8000, 3000));
    }
}
