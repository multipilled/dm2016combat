//! Weapon definitions read from the install's decls (weapon -> ammo -> projectile -> damage), with the
//! decl class defaults the exe applies to fields a decl leaves out.

use std::sync::Arc;

use anyhow::{Context, Result};
use idres::decl::Block;
use idres::decldb::DeclDb;

use super::damage::DamageDef;
use super::kick::{kick_type, FeedBack, WeaponKick};
use super::spread::{SpreadParams, SpreadPattern};

#[derive(Debug, Clone, Default)]
pub struct ProjectileDef {
    pub name: String,
    pub hitscan: bool,
    pub spawn_count: i32,
    pub spawn_num_circles: i32,
    pub fixed_spread: bool,
    pub dartboard: bool,
    pub wiggle: f32,
    pub max_range: f32,
    pub speed: f32,
    pub gravity: bool,
    pub damage: DamageDef,
    pub splash: Option<DamageDef>,
    pub fire_sound: String,
    /// notHitscanInfo startSpeed (-1 = speed) / minStartSpeed (randomised start speed when >= 0 and < startSpeed).
    pub start_speed: f32,
    pub min_start_speed: f32,
    /// notHitscanInfo.grenadeInfo.canDetonateWithAltTrigger (decl +0x325): the RL detonate explodes it.
    pub can_detonate_with_alt_trigger: bool,
    /// The projectile entity (notHitscanInfo.entityDef): seekParms (+0x4290), acceleration (+0x46b0) and
    /// minAcceleration (+0x46b4); idProjectile ctor 0x140f32420 defaults 100 / -1.
    pub seek: super::seek::SeekParms,
    pub acceleration: f32,
    pub min_acceleration: f32,
}

impl ProjectileDef {
    fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        let b = db.get("projectile", name).with_context(|| format!("projectile decl {name}"))?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let ent = e.str("notHitscanInfo.entityDef").filter(|s| !s.is_empty() && *s != "NULL").and_then(|n| db.get("entitydef", n).ok()).and_then(|b| b.block("edit").cloned()).unwrap_or_default();
        let damage = match e.str("damageDecl") {
            Some(d) if !d.is_empty() && d != "NULL" => DamageDef::from_decl(db, d)?,
            _ => DamageDef::default(),
        };
        let splash = match e.str("splashDamageDecl") {
            Some(d) if !d.is_empty() && d != "NULL" => Some(DamageDef::from_decl(db, d)?),
            _ => None,
        };
        Ok(Self {
            name: name.to_string(),
            hitscan: e.path("hitscan").and_then(|v| v.as_bool()).unwrap_or(true),
            spawn_count: e.f32("spawnCount").unwrap_or(1.0) as i32,
            spawn_num_circles: e.f32("spawnNumCircles").unwrap_or(0.0) as i32,
            fixed_spread: flag(&e, "fixedSpreadRandomDecals", false),
            dartboard: flag(&e, "useDartboardSpread", false),
            wiggle: e.f32("fixedSpreadRandomDecalsWiggle").unwrap_or(0.0),
            max_range: e.f32("maxRange").unwrap_or(8192.0),
            speed: e.f32("notHitscanInfo.speed").unwrap_or(0.0),
            gravity: !e.path("notHitscanInfo.physicsProperties.noGravity").and_then(|v| v.as_bool()).unwrap_or(false),
            damage,
            splash,
            fire_sound: e.str("fireSound").unwrap_or_default().to_string(),
            start_speed: e.f32("notHitscanInfo.startSpeed").unwrap_or(-1.0),
            min_start_speed: e.f32("notHitscanInfo.minStartSpeed").unwrap_or(-1.0),
            can_detonate_with_alt_trigger: flag(&e, "notHitscanInfo.grenadeInfo.canDetonateWithAltTrigger", false),
            seek: ent.block("seekParms").map(super::seek::SeekParms::from_block).unwrap_or_default(),
            acceleration: ent.f32("acceleration").unwrap_or(100.0),
            min_acceleration: ent.f32("minAcceleration").unwrap_or(-1.0),
        })
    }

    pub fn pattern(&self) -> SpreadPattern {
        SpreadPattern { count: self.spawn_count, fixed_rings: self.fixed_spread, num_circles: self.spawn_num_circles, dartboard: self.dartboard }
    }
}

fn flag(b: &Block, key: &str, def: bool) -> bool {
    b.path(key).and_then(|v| v.as_bool()).unwrap_or(def)
}

/// `idDeclWeapon::heatInfo_t` (ctor 0x1406f0780 defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeatInfo {
    pub heat_increment: f32,
    pub heat_decrement: f32,
    pub max_heat_percent: f32,
    pub glow_increment: f32,
    pub glow_decrement: f32,
    pub max_glow: f32,
    pub cooling_delay_ms: i32,
    pub overheat_delay_ms: i32,
    pub cool_during_overheat_delay: bool,
    pub overheat_recovery_percent: f32,
}

impl Default for HeatInfo {
    fn default() -> Self {
        Self {
            heat_increment: 0.0,
            heat_decrement: 0.0,
            max_heat_percent: 1.0,
            glow_increment: 0.0,
            glow_decrement: 0.0,
            max_glow: 1.0,
            cooling_delay_ms: 0,
            overheat_delay_ms: 0,
            cool_during_overheat_delay: true,
            overheat_recovery_percent: -1.0,
        }
    }
}

/// `idDeclWeapon_ChaingunData` (ctor 0x1406f5aa0 defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChaingunData {
    pub spin_up_ms: i32,
    pub spin_down_ms: i32,
    pub barrel_align_degs_per_sec: f32,
    pub barrel_align_sectors: f32,
    pub num_barrels: i32,
    pub allow_idle_barrel_spin: bool,
    pub prevent_spin_up_while_dryfiring: bool,
    pub progressive_firing_interval: bool,
    pub firing_interval_min: i32,
}

impl Default for ChaingunData {
    fn default() -> Self {
        Self {
            spin_up_ms: 500,
            spin_down_ms: 250,
            barrel_align_degs_per_sec: 100.0,
            barrel_align_sectors: 6.0,
            num_barrels: 6,
            allow_idle_barrel_spin: false,
            prevent_spin_up_while_dryfiring: false,
            progressive_firing_interval: false,
            firing_interval_min: 1000,
        }
    }
}

/// One burstInfo_t entry (decl +0x558 burstInfo[bm], 0x30 bytes each; MODS.md section 3 "Bursts").
#[derive(Debug, Clone, PartialEq)]
pub struct BurstInfo {
    /// +0x590 (bm 1): shots per burst; < 0 disables the mode (IsBurstMode 0x140c4f6c0).
    pub burst_count: i32,
    /// +0x564: ms between the end of a burst and the next shot (FinishFire's last-shot branch).
    pub burst_interval: i32,
    /// +0x568: the in-burst firing interval (GetFiringInterval step 3), 0 = not used.
    pub burst_firing_interval: i32,
    pub can_queue_burst: bool,
    /// +0x56d: StartStopZoom keeps a burst running when false.
    pub can_interrupt_burst: bool,
    /// +0x588 ammoPerBurst (-1 = per shot).
    pub ammo_per_burst: i32,
    /// +0x570 burstShootState / +0x580 lastShotState: the hands web states of the burst shots
    /// (FIRE request 0xf9b / 0xf97).
    pub burst_shoot_state: String,
    pub last_shot_state: String,
    /// fakeBurstCount: shots inside one shoot anim (RL lock-on); 0 = none.
    pub fake_burst_count: i32,
}

impl Default for BurstInfo {
    fn default() -> Self {
        Self {
            burst_count: 0,
            burst_interval: 0,
            burst_firing_interval: 0,
            can_queue_burst: true,
            can_interrupt_burst: true,
            ammo_per_burst: -1,
            burst_shoot_state: String::new(),
            last_shot_state: String::new(),
            fake_burst_count: 0,
        }
    }
}

/// CHARGE_PROPERTY_* of a chargeItem_t (names from the decls; MODS.md section 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChargeProperty {
    None,
    BurstCount,
    DamageScale,
    DamageRadiusScale,
    MaxProjectiles,
    AmmoToUse,
    DeviateProjectiles,
    DeviateProjectilesOnDischarge,
    PrimaryChargeSecondaryDamageScale,
    Other(String),
}

impl ChargeProperty {
    pub fn parse(s: &str) -> Self {
        match s.strip_prefix("CHARGE_PROPERTY_").unwrap_or(s) {
            "NONE" => Self::None,
            "BURST_COUNT" => Self::BurstCount,
            "DAMAGE_SCALE" => Self::DamageScale,
            "DAMAGE_RADIUS_SCALE" => Self::DamageRadiusScale,
            "MAX_PROJECTILES" => Self::MaxProjectiles,
            "AMMO_TO_USE" => Self::AmmoToUse,
            "DEVIATE_PROJECTILES" => Self::DeviateProjectiles,
            "DEVIATE_PROJECTILES_ON_DISCHARGE" => Self::DeviateProjectilesOnDischarge,
            "PRIMARY_CHARGE_SECONDARY_DAMAGE_SCALE" => Self::PrimaryChargeSecondaryDamageScale,
            o => Self::Other(o.to_string()),
        }
    }
}

/// chargeItem_t (0x98): property +0, valueMin +4, valueMax +8, valueTable +0x10, intervalSound +0x18.
#[derive(Debug, Clone, PartialEq)]
pub struct ChargeItem {
    pub property: ChargeProperty,
    pub value_min: f32,
    pub value_max: f32,
    pub value_table: String,
    pub interval_sound: String,
}

/// chargeInfo_t.chargePerShot (decl +0x42c..; ctor 0x1406f0660 defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargePerShot {
    pub num_shots_per_charge: i32,
    pub charge_increment: f32,
    pub require_hit_to_charge: bool,
    pub max_charge: f32,
    pub num_misses_to_discharge: i32,
    pub num_charges_to_discharge: f32,
}

impl Default for ChargePerShot {
    fn default() -> Self {
        Self { num_shots_per_charge: 1, charge_increment: 1.0, require_hit_to_charge: false, max_charge: 0.0, num_misses_to_discharge: 1, num_charges_to_discharge: -1.0 }
    }
}

/// idDeclWeapon::chargeInfo_t at decl+0x330 (ctor 0x1406f0660 defaults: all 0 / false except
/// dischargeFireMode -1, dischargePercentPerShot 1.0, canChargeWhileThrowing, chargePerShot defaults).
/// The *MS fields are compared with game time as raw ticks (MODS.md section 3 "TIME UNITS").
#[derive(Debug, Clone, PartialEq)]
pub struct ChargeInfo {
    pub charge_time_ms: i32,
    /// CHARGE_TYPE_DEFAULT 0 / OSCILLATE 1.
    pub oscillate: bool,
    pub hold_time_before_charging_ms: i32,
    pub charge_time_max_ms: i32,
    pub discharge_timeout_ms: i32,
    pub scale_discharge_timeout_by_discharge_pct: bool,
    /// -1 none, 0 primary, 1 secondary, 2 both (the fire gate's "== 2").
    pub discharge_fire_mode: i32,
    pub discharge_percent_per_shot: f32,
    pub discharge_only_for_insufficient_ammo: bool,
    pub items: Vec<ChargeItem>,
    pub charge_anim_state_name: String,
    pub charge_anim_interval_ms: i32,
    pub no_discharge: bool,
    pub min_charge_required_to_discharge: f32,
    pub allow_partial_charge_with_insufficient_ammo: bool,
    pub keep_charge_when_cant_charge: bool,
    pub fire_at_full_charge: bool,
    pub wait_for_next_fire_time_before_charging: bool,
    pub can_only_charge_when_targeting: bool,
    pub dryfire_at_zero_charge: bool,
    pub scale_shoot_anim_to_match_charge_time: bool,
    pub override_primary_charge_info: bool,
    pub timeout_blocks_other_fire_mode: bool,
    pub charge_while_unequipped: bool,
    pub per_shot: ChargePerShot,
    pub start_sound: String,
    pub looping_sound: String,
    pub fully_charged_sound: String,
    pub ready_sound: String,
    pub not_ready_sound: String,
    pub discharge_sound: String,
}

impl Default for ChargeInfo {
    fn default() -> Self {
        Self {
            charge_time_ms: 0,
            oscillate: false,
            hold_time_before_charging_ms: 0,
            charge_time_max_ms: 0,
            discharge_timeout_ms: 0,
            scale_discharge_timeout_by_discharge_pct: false,
            discharge_fire_mode: -1,
            discharge_percent_per_shot: 1.0,
            discharge_only_for_insufficient_ammo: false,
            items: Vec::new(),
            charge_anim_state_name: String::new(),
            charge_anim_interval_ms: 0,
            no_discharge: false,
            min_charge_required_to_discharge: 0.0,
            allow_partial_charge_with_insufficient_ammo: false,
            keep_charge_when_cant_charge: false,
            fire_at_full_charge: false,
            wait_for_next_fire_time_before_charging: false,
            can_only_charge_when_targeting: false,
            dryfire_at_zero_charge: false,
            scale_shoot_anim_to_match_charge_time: false,
            override_primary_charge_info: false,
            timeout_blocks_other_fire_mode: false,
            charge_while_unequipped: false,
            per_shot: ChargePerShot::default(),
            start_sound: String::new(),
            looping_sound: String::new(),
            fully_charged_sound: String::new(),
            ready_sound: String::new(),
            not_ready_sound: String::new(),
            discharge_sound: String::new(),
        }
    }
}

impl ChargeInfo {
    pub fn item(&self, p: &ChargeProperty) -> Option<usize> {
        self.items.iter().position(|i| &i.property == p)
    }
}

fn fire_mode(s: &str) -> i32 {
    match s {
        "WEAPONFIREMODE_PRIMARY" => 0,
        "WEAPONFIREMODE_SECONDARY" => 1,
        "WEAPONFIREMODE_NUM" => 2,
        _ => -1,
    }
}

fn charge_info(b: &Block) -> ChargeInfo {
    let d = ChargeInfo::default();
    let i = |k: &str, def: i32| b.f32(k).map(|v| v as i32).unwrap_or(def);
    let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
    let s = |k: &str| b.str(k).filter(|v| *v != "NULL").unwrap_or_default().to_string();
    let mut items = Vec::new();
    if let Some(l) = b.block("chargeItems") {
        let n = l.f32("num").unwrap_or(0.0) as usize;
        for k in 0..n {
            if let Some(it) = l.block(&format!("item[{k}]")) {
                items.push(ChargeItem {
                    property: ChargeProperty::parse(it.str("chargeProperty").unwrap_or("CHARGE_PROPERTY_NONE")),
                    value_min: it.f32("valueMin").unwrap_or(0.0),
                    value_max: it.f32("valueMax").unwrap_or(0.0),
                    value_table: it.str("valueTable").filter(|v| *v != "NULL").unwrap_or_default().to_string(),
                    interval_sound: it.str("intervalSound").filter(|v| *v != "NULL").unwrap_or_default().to_string(),
                });
            }
        }
    }
    let pd = ChargePerShot::default();
    let ps = b.block("chargePerShot").cloned().unwrap_or_default();
    ChargeInfo {
        charge_time_ms: i("chargeTimeMS", d.charge_time_ms),
        oscillate: b.str("chargeType") == Some("CHARGE_TYPE_OSCILLATE"),
        hold_time_before_charging_ms: i("holdTimeBeforeChargingMS", d.hold_time_before_charging_ms),
        charge_time_max_ms: i("chargeTimeMaxMS", d.charge_time_max_ms),
        discharge_timeout_ms: i("dischargeTimeoutMS", d.discharge_timeout_ms),
        scale_discharge_timeout_by_discharge_pct: flag(b, "scaleDischargeTimeoutByDischargePct", false),
        discharge_fire_mode: b.str("dischargeFireMode").map(fire_mode).unwrap_or(d.discharge_fire_mode),
        discharge_percent_per_shot: f("dischargePercentPerShot", d.discharge_percent_per_shot),
        discharge_only_for_insufficient_ammo: flag(b, "dischargeOnlyForInsuffientAmmo", false),
        items,
        charge_anim_state_name: s("chargeAnimStateName"),
        charge_anim_interval_ms: i("chargeAnimIntervalMS", 0),
        no_discharge: flag(b, "noDischarge", false),
        min_charge_required_to_discharge: f("minChargeRequiredToDischarge", 0.0),
        allow_partial_charge_with_insufficient_ammo: flag(b, "allowPartialChargeWithInsufficientAmmo", false),
        keep_charge_when_cant_charge: flag(b, "keepChargeWhenCantCharge", false),
        fire_at_full_charge: flag(b, "fireAtFullCharge", false),
        wait_for_next_fire_time_before_charging: flag(b, "waitForNextFireTimeBeforeCharging", false),
        can_only_charge_when_targeting: flag(b, "canOnlyChargeWhenTargeting", false),
        dryfire_at_zero_charge: flag(b, "dryfireAtZeroCharge", false),
        scale_shoot_anim_to_match_charge_time: flag(b, "scaleShootAnimToMatchChargeTime", false),
        override_primary_charge_info: flag(b, "overridePrimaryChargeInfo", false),
        timeout_blocks_other_fire_mode: flag(b, "timeoutBlocksOtherFireMode", false),
        charge_while_unequipped: flag(b, "chargeWhileUnequipped", false),
        per_shot: ChargePerShot {
            num_shots_per_charge: ps.f32("numShotsPerCharge").map(|v| v as i32).unwrap_or(pd.num_shots_per_charge),
            charge_increment: ps.f32("chargeIncrement").unwrap_or(pd.charge_increment),
            require_hit_to_charge: flag(&ps, "requireHitToCharge", pd.require_hit_to_charge),
            max_charge: ps.f32("maxCharge").unwrap_or(pd.max_charge),
            num_misses_to_discharge: ps.f32("numMissesToDischarge").map(|v| v as i32).unwrap_or(pd.num_misses_to_discharge),
            num_charges_to_discharge: ps.f32("numChargesToDischarge").unwrap_or(pd.num_charges_to_discharge),
        },
        start_sound: s("startSound"),
        looping_sound: s("loopingSound"),
        fully_charged_sound: s("fullyChargedSound"),
        ready_sound: s("readySound"),
        not_ready_sound: s("notReadySound"),
        discharge_sound: s("dischargeSound"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct WeaponDef {
    pub decl: String,
    pub display_name: String,
    pub selection_group: String,
    /// weaponSelectionGroup as its index (WEAPONSELECTIONGROUP_N -> N; ctor default 10 = none).
    pub selection_group_index: i32,
    /// Selection flags (ctor 0x1406f0780: selectable, autoSelectable, quickSwapRemember, canBeSlotted 1;
    /// cantSwitchFrom 0).
    pub selectable: bool,
    pub auto_selectable: bool,
    pub quick_swap_remember: bool,
    pub can_be_slotted: bool,
    pub cant_switch_from: bool,
    pub hands_md6: String,
    /// decl.firingInterval (ctor default 500 ms).
    pub firing_interval_ms: f32,
    pub firing_interval: i32,
    pub added_firing_interval: i32,
    pub bringup_s: f32,
    pub bringdown_s: f32,
    pub single_tap: bool,
    pub single_tap_ads: bool,
    pub allow_shot_queueing: bool,
    pub ignore_firing_interval: bool,
    pub infinite_ammo: bool,
    pub ammo_decl: String,
    /// Shared ammo pool key (sharedAmmoPoolDecl, lower case) or the ammo decl itself.
    pub ammo_pool: String,
    pub ammo_max: i32,
    pub ammo_start: i32,
    pub ammo_per_shot: i32,
    pub projectile: ProjectileDef,
    pub spread_params: SpreadParams,
    /// Compatibility mirrors of spreadParams.
    pub spread: f32,
    pub spread_crouch: f32,
    pub h_spread_scale: f32,
    pub v_spread_scale: f32,
    pub feedback: FeedBack,
    pub kick_pitch: WeaponKick,
    pub kick_yaw: WeaponKick,
    pub kick_fov: WeaponKick,
    pub heat: HeatInfo,
    pub chaingun: Option<ChaingunData>,
    /// weaponData as idDeclWeapon_RailGunData (gauss): idRailGun's own charge (weapons::railgun).
    pub railgun: Option<RailGunData>,
    pub bursts: [BurstInfo; 4],
    /// initialBurstMode (+0x558): BURSTMODE_SINGLE 0, BURST 1, FULLAUTO 2, SELECTFIRE 3 (enum table
    /// 0x143569270).
    pub initial_burst_mode: i32,
    /// chargeInfo (+0x330).
    pub charge: ChargeInfo,
    /// hasChargeState (+0x6a1): the hands use the charge_* states.
    pub has_charge_state: bool,
    /// canUseChargeStateWhenNotCharging (+0x6a2): the hands stay in charge_idle while not charging.
    pub can_use_charge_state_when_not_charging: bool,
    /// chargeShootToChargeIdleState (+0x6a8): its length adds to charge_shoot's for shootChargeAnimRate.
    pub charge_shoot_to_charge_idle_state: String,
    /// secondaryFireDecl (+0x480): the mode-1 decl without a DECL_WEAPON override.
    pub secondary_fire_decl: String,
    /// otherFireModeFiringInterval (+0x538): raw ticks the OTHER mode is blocked after a shot.
    /// INTERIM: a decl without the key is taken as 0 (the ctor default is not read).
    pub other_fire_mode_firing_interval: i32,
    /// forbidZoomIfCannotCharge (+0xe35).
    pub forbid_zoom_if_cannot_charge: bool,
    /// forbidFireModeWithInsufficientAmmo.
    pub forbid_fire_mode_with_insufficient_ammo: bool,
    /// usePrimaryFireButton: mode 1 fires on BUTTON_ATTACK1 (trigger mode 7 decls).
    pub use_primary_fire_button: bool,
    /// perkGroups (+0x2a8) and noPerkSwitcher (+0x627): the mod families (weapons::mods).
    pub perk_groups: Vec<String>,
    pub no_perk_switcher: bool,
    /// shotChargeDuration (+0x534): how long the pre-shot charge anim plays.
    pub shot_charge_duration: i32,
    /// zoomShootState (+0xe20).
    pub zoom_shoot_state: String,
    pub hands_fov_scale: f32,
    pub max_range: f32,
    pub post_sprint_fire_penalty_ms: i32,
    /// Weapon mods (upgrade decls) listed by the decl; not applied.
    pub upgrades: Vec<String>,
    /// Hands anim-web data (idHands, gamedata/re/HANDS.md).
    pub hands: HandsDecl,
    /// ironSightZoom (+0xde8), zoomMode (+0xe30), forbidZoomWithInsufficientAmmo (+0xe34), zoomIn/OutSound
    /// (+0xe80 / +0xe88): weapons::zoom.
    pub zoom: super::zoom::ZoomInfo,
    pub zoom_mode: super::zoom::ZoomMode,
    pub forbid_zoom_with_insufficient_ammo: bool,
    pub zoom_in_sound: String,
    pub zoom_out_sound: String,
    /// Melee data (weapons::melee).
    pub melee: super::melee::MeleeDecl,
    /// targetLockNormal (+0xa80) / targetLockZoomed (+0xaf8): weapons::targeting.
    pub target_lock_normal: TargetLockData,
    pub target_lock_zoomed: TargetLockData,
    /// forceFireModeWhenLocked (+0x4bd): a lock overrides the fire mode with the locking mode (0x140f1cb70).
    pub force_fire_mode_when_locked: bool,
    /// targetsFriendlies (+0x624): the targeting team test (0x140f199e0) wants friendlies.
    pub targets_friendlies: bool,
    /// explodeProjectiles* (+0x65c..+0x694): the launched-projectile list (remote detonate).
    pub explode: ExplodeProjectiles,
    /// reticle (+0x12b0), reticleWhenZoomed (+0x12b8), lockedReticle (+0x12c0): idDeclWeaponReticle names
    /// (GetReticleDecl 0x140f12c70, Arsenal::reticle_decl).
    pub reticle: String,
    pub reticle_when_zoomed: String,
    pub locked_reticle: String,
    /// The weapon's mods (weapons::mods, loaded by load_arsenal; None for mod decl variants).
    pub mods: Option<Arc<super::mods::WeaponMods>>,
}

/// The idDeclWeapon fields idHands reads to drive the fp_hands anim web (ctor defaults from 0x1406f0780).
#[derive(Debug, Clone, PartialEq)]
pub struct HandsDecl {
    /// subweb_normal: the subWeb the weapon's states live in.
    pub subweb: String,
    /// +0x478 triggerMode (WEAPONTRIGGERMODE_PRIMARY 1, PRIMARY_PRESS_OR_SECONDARY_PRESS 0xd for fists).
    pub trigger_mode: i32,
    pub has_looping_shoot_state: bool,
    pub has_shoot_again_state: bool,
    pub has_last_shot_anims: bool,
    pub has_shoot_to_reload_anims: bool,
    pub has_looping_dryfire_state: bool,
    pub shoot_to_idle_alt: bool,
    pub has_pre_shoot_charge: bool,
    pub has_intro_bringup: bool,
    pub has_intro_accent_bringup: bool,
    pub has_shoot_alternating_barrels: bool,
    pub shots_per_shoot_anim: i32,
    pub shots_per_looping_shoot_anim: i32,
    pub scale_shoot_anim_to_firing_interval: bool,
    pub validate_looping_state_burst_fire: bool,
    pub fire_breaks_reload: bool,
    pub can_only_fire_when_zoomed: bool,
    pub melee_from_fire_input: bool,
    /// shootAnimAliasName (+0x9a8): the weapon-model md6Def alias whose length scales shootAnimRate.
    pub shoot_anim_alias: String,
}

impl Default for HandsDecl {
    fn default() -> Self {
        Self {
            subweb: String::new(),
            trigger_mode: 1,
            has_looping_shoot_state: false,
            has_shoot_again_state: false,
            has_last_shot_anims: false,
            has_shoot_to_reload_anims: false,
            has_looping_dryfire_state: false,
            shoot_to_idle_alt: false,
            has_pre_shoot_charge: false,
            has_intro_bringup: false,
            has_intro_accent_bringup: false,
            has_shoot_alternating_barrels: false,
            shots_per_shoot_anim: 1,
            shots_per_looping_shoot_anim: 1,
            scale_shoot_anim_to_firing_interval: false,
            validate_looping_state_burst_fire: true,
            fire_breaks_reload: false,
            can_only_fire_when_zoomed: false,
            melee_from_fire_input: false,
            shoot_anim_alias: "shoot".to_string(),
        }
    }
}

/// WEAPONTRIGGERMODE_* (reflection enum table 0x14356be30).
pub fn trigger_mode(name: &str) -> i32 {
    const T: [&str; 14] = [
        "NONE",
        "PRIMARY_PRESS",
        "PRIMARY_RELEASE",
        "SECONDARY_PRESS",
        "SECONDARY_TAP",
        "SECONDARY_PRESS_DOES_NOT_INTERRUPT_PRIMARY",
        "SECONDARY_PRESS_INTERRUPTS_PRIMARY",
        "SECONDARY_HOLD_PRIMARY_PRESS",
        "SECONDARY_HOLD_PRIMARY_RELEASE",
        "SECONDARY_HOLD_PRIMARY_NONE",
        "SECONDARY_RELEASE",
        "SECONDARY_RELEASE_PRIMARY_NONE",
        "SECONDARY_TOGGLE",
        "PRIMARY_PRESS_OR_SECONDARY_PRESS",
    ];
    let n = name.strip_prefix("WEAPONTRIGGERMODE_").unwrap_or(name);
    T.iter().position(|t| *t == n).map(|i| i as i32).unwrap_or(1)
}

fn hands_decl(e: &Block) -> HandsDecl {
    let d = HandsDecl::default();
    let i = |k: &str, def: i32| e.f32(k).map(|v| v as i32).unwrap_or(def);
    HandsDecl {
        subweb: e.str("subweb_normal").unwrap_or_default().to_string(),
        trigger_mode: e.str("triggerMode").map(trigger_mode).unwrap_or(d.trigger_mode),
        has_looping_shoot_state: flag(e, "hasLoopingShootState", d.has_looping_shoot_state),
        has_shoot_again_state: flag(e, "hasShootAgainState", d.has_shoot_again_state),
        has_last_shot_anims: flag(e, "hasLastShotAnims", d.has_last_shot_anims),
        has_shoot_to_reload_anims: flag(e, "hasShootToReloadAnims", d.has_shoot_to_reload_anims),
        has_looping_dryfire_state: flag(e, "hasLoopingDryfireState", d.has_looping_dryfire_state),
        shoot_to_idle_alt: flag(e, "shootToIdleAlt", d.shoot_to_idle_alt),
        has_pre_shoot_charge: flag(e, "hasPreShootCharge", d.has_pre_shoot_charge),
        has_intro_bringup: flag(e, "hasIntroBringup", d.has_intro_bringup),
        has_intro_accent_bringup: flag(e, "hasIntroAccentBringup", d.has_intro_accent_bringup),
        has_shoot_alternating_barrels: flag(e, "hasShootAlternatingBarrels", d.has_shoot_alternating_barrels),
        shots_per_shoot_anim: i("shotsPerShootAnim", d.shots_per_shoot_anim),
        shots_per_looping_shoot_anim: i("shotsPerLoopingShootAnim", d.shots_per_looping_shoot_anim),
        scale_shoot_anim_to_firing_interval: flag(e, "scaleShootAnimToFiringInterval", d.scale_shoot_anim_to_firing_interval),
        validate_looping_state_burst_fire: flag(e, "validateLoopingStateBurstFire", d.validate_looping_state_burst_fire),
        fire_breaks_reload: flag(e, "fireBreaksReload", d.fire_breaks_reload),
        can_only_fire_when_zoomed: flag(e, "canOnlyFireWhenZoomed", d.can_only_fire_when_zoomed),
        melee_from_fire_input: flag(e, "meleeFromFireInput", d.melee_from_fire_input),
        shoot_anim_alias: e.str("shootAnimAliasName").filter(|s| !s.is_empty()).unwrap_or("shoot").to_string(),
    }
}

fn kick(b: Option<&Block>) -> WeaponKick {
    let d = WeaponKick::default();
    let Some(b) = b else { return d };
    WeaponKick {
        kind: b.str("type").map(kick_type).unwrap_or(d.kind),
        kick: b.f32("kick").unwrap_or(d.kick),
        max_kick: b.f32("maxKick").unwrap_or(d.max_kick),
        recoil_ms: b.f32("recoilMS").map(|v| v as i32).unwrap_or(d.recoil_ms),
        recovery_ms: b.f32("recoveryMS").map(|v| v as i32).unwrap_or(d.recovery_ms),
        recovery_delay_ms: b.f32("recoveryDelayMS").map(|v| v as i32).unwrap_or(d.recovery_delay_ms),
        dampen: flag(b, "dampen", d.dampen),
    }
}

fn feedback(b: &Block) -> FeedBack {
    let d = FeedBack::default();
    let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
    FeedBack {
        do_unique_kick: flag(b, "doUniqueKick", d.do_unique_kick),
        do_smooth_kick: flag(b, "doSmoothKick", d.do_smooth_kick),
        pitch_kick_amount: f("pitchKickAmount", d.pitch_kick_amount),
        pitch_kick_amount_delta: f("pitchKickAmountDelta", d.pitch_kick_amount_delta),
        pitch_kick_top_bound: f("pitchKickTopBound", d.pitch_kick_top_bound),
        yaw_kick_amount: f("yawKickAmount", d.yaw_kick_amount),
        yaw_kick_amount_delta: f("yawKickAmountDelta", d.yaw_kick_amount_delta),
        pitch_kick_speed_into: f("pitchKickSpeedInto", d.pitch_kick_speed_into),
        pitch_kick_speed_into_min: f("pitchKickSpeedIntoMin", d.pitch_kick_speed_into_min),
        pitch_kick_speed_into_per_shot: f("pitchKickSpeedIntoPerShot", d.pitch_kick_speed_into_per_shot),
        pitch_kick_speed_from: f("pitchKickSpeedFrom", d.pitch_kick_speed_from),
        yaw_kick_speed_into: f("yawKickSpeedInto", d.yaw_kick_speed_into),
        yaw_kick_speed_from: f("yawKickSpeedFrom", d.yaw_kick_speed_from),
        kick_recovery_delay: f("kickRecoveryDelay", d.kick_recovery_delay as f32) as i32,
        use_new_kick_system: flag(b, "useNewKickSystem", d.use_new_kick_system),
        kick_yaw: kick(b.block("kickYaw")),
        kick_pitch: kick(b.block("kickPitch")),
        kick_roll: kick(b.block("kickRoll")),
        kick_fov: kick(b.block("kickFov")),
        weapon_knockback: f("weaponKnockback", d.weapon_knockback),
    }
}

fn spread_params(b: &Block) -> SpreadParams {
    let d = SpreadParams::default();
    let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
    SpreadParams {
        spread: f("spread", d.spread),
        base_zoom: f("spreadBaseZoom", d.base_zoom),
        base_crouch: f("spreadBaseCrouch", d.base_crouch),
        base_zoom_crouch: f("spreadBaseZoomCrouch", d.base_zoom_crouch),
        increased_by_movement: f("spreadIncreasedByMovement", d.increased_by_movement),
        increased_by_movement_zoom: f("spreadIncreasedByMovementZoom", d.increased_by_movement_zoom),
        increased_by_aiming: f("spreadIncreasedByAiming", d.increased_by_aiming),
        addition_per_shot: f("spreadAdditionPerShot", d.addition_per_shot),
        addition_max: f("spreadAdditionMax", d.addition_max),
        addition_per_shot_zoom: f("spreadAdditionPerShotZoom", d.addition_per_shot_zoom),
        addition_per_shot_moving: f("spreadAdditionPerShotMoving", d.addition_per_shot_moving),
        addition_per_shot_zoom_moving: f("spreadAdditionPerShotZoomMoving", d.addition_per_shot_zoom_moving),
        addition_max_zoom: f("spreadAdditionMaxZoom", d.addition_max_zoom),
        return_delay: f("spreadReturnDelay", d.return_delay),
        return_time: f("spreadReturnTime", d.return_time),
        band_strength: f("spreadBandStrength", d.band_strength as f32) as i32,
        horizontal_scale: f("horizontalSpreadScale", d.horizontal_scale),
        horizontal_even_spacing_lerp: f("horizontalSpreadScaleEvenSpacingLerp", d.horizontal_even_spacing_lerp),
        vertical_scale: f("verticalSpreadScale", d.vertical_scale),
        vertical_even_spacing_lerp: f("verticalSpreadScaleEvenSpacingLerp", d.vertical_even_spacing_lerp),
    }
}

/// A melee projectile and its melee-trace overrides (idDeclProjectile +0x214 / +0x218 / +0x21c; their
/// defaults are taken as "no override": BOUNDS_NONE, MELEE_NONE, -1).
fn melee_projectile(db: &DeclDb, name: Option<&str>) -> Option<super::melee::MeleeProjectile> {
    use super::melee::{MeleeBounds, MeleeDamageType, MeleeProjectile};
    let name = name.filter(|n| !n.is_empty() && *n != "NULL")?;
    let def = ProjectileDef::from_decl(db, name).ok()?;
    let e = db.get("projectile", name).ok()?.block("edit").cloned().unwrap_or_default();
    Some(MeleeProjectile {
        def,
        bounds: e.str("meleeTraceBoundsType").and_then(MeleeBounds::parse).unwrap_or(MeleeBounds::None),
        damage_type: e.str("meleeTraceDamageType").and_then(MeleeDamageType::parse).unwrap_or(MeleeDamageType::None),
        damage_cap: e.f32("meleeTraceDamageCap").unwrap_or(-1.0),
    })
}

/// The idDeclWeapon melee fields (ctor 0x1406f0780 defaults in MeleeDecl::default).
fn melee_decl(db: &DeclDb, e: &Block) -> super::melee::MeleeDecl {
    use super::melee::{MeleeBounds, MeleeDamageType, MeleeDecl};
    let d = MeleeDecl::default();
    MeleeDecl {
        melee_from_fire_input: flag(e, "meleeFromFireInput", d.melee_from_fire_input),
        fire_from_melee_input: flag(e, "fireFromMeleeInput", d.fire_from_melee_input),
        melee_from_melee_input: flag(e, "meleeFromMeleeInput", d.melee_from_melee_input),
        melee_to_shoot_state: flag(e, "meleeToShootState", d.melee_to_shoot_state),
        melee_alternating: flag(e, "meleeAlternating", d.melee_alternating),
        melee_ltrt: flag(e, "meleeLTRT", d.melee_ltrt),
        has_sprint_melee: flag(e, "hasSprintMelee", d.has_sprint_melee),
        has_directional_melee: flag(e, "hasDirectionalMelee", d.has_directional_melee),
        has_directional_and_staged_melee: flag(e, "hasDirectionalAndStagedMelee", d.has_directional_and_staged_melee),
        projectile: melee_projectile(db, e.str("meleeProjectile")),
        slash_projectile: melee_projectile(db, e.str("meleeSlashProjectile")),
        sprint_projectile: melee_projectile(db, e.str("meleeSprintProjectile")),
        one_hit_kill_projectile: melee_projectile(db, e.str("meleeOneHitKillProjectile")),
        directional_projectile: melee_projectile(db, e.str("meleeDirectionalProjectile")),
        trace_offset: e.f32("meleeTraceOffset").unwrap_or(d.trace_offset),
        bounds: e.str("meleeTraceBoundsType").and_then(MeleeBounds::parse).unwrap_or(d.bounds),
        damage_type: e.str("meleeTraceDamageType").and_then(MeleeDamageType::parse).unwrap_or(d.damage_type),
        damage_cap: e.f32("meleeTraceDamageCap").unwrap_or(d.damage_cap),
        damage_interval_ms: e.f32("meleeTraceDamageIntervalMS").map(|v| v as i32).unwrap_or(d.damage_interval_ms),
        damage_interval_world_ms: e.f32("meleeTraceDamageIntervalWorldMS").map(|v| v as i32).unwrap_or(d.damage_interval_world_ms),
        immediate_transition: flag(e, "meleeImmediateTransition", d.immediate_transition),
    }
}

/// idDeclWeapon::zoomInfo_t (ctor 0x1406f0780 defaults in ZoomInfo::default).
fn zoom_info(b: &Block) -> super::zoom::ZoomInfo {
    let d = super::zoom::ZoomInfo::default();
    let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
    super::zoom::ZoomInfo {
        zoomed_fov: f("zoomedFOV", d.zoomed_fov),
        zoomed_hands_fov: f("zoomedHandsFOV", d.zoomed_hands_fov),
        can_zoom_while_jumping: flag(b, "canZoomWhileJumping", d.can_zoom_while_jumping),
        sensitivity_scale_controller: f("sensitivity_scale_controller", d.sensitivity_scale_controller),
        sensitivity_scale_mouse: f("sensitivity_scale_mouse", d.sensitivity_scale_mouse),
        zoom_delay: f("zoomDelay", d.zoom_delay as f32) as i32,
        zoom_time: f("zoomTime", d.zoom_time as f32) as i32,
        has_blended_zoom: flag(b, "hasBlendedZoom", d.has_blended_zoom),
        zoom_blend_time: f("zoomBlendTime", d.zoom_blend_time as f32) as i32,
        hide_hands_on_zoom: flag(b, "hideHandsOnZoom", d.hide_hands_on_zoom),
        hide_hands_on_zoom_delay: f("hideHandsOnZoomDelay", d.hide_hands_on_zoom_delay as f32) as i32,
    }
}

fn heat_info(b: &Block) -> HeatInfo {
    let d = HeatInfo::default();
    let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
    HeatInfo {
        heat_increment: f("heatIncrement", d.heat_increment),
        heat_decrement: f("heatDecrement", d.heat_decrement),
        max_heat_percent: f("maxHeatPercent", d.max_heat_percent),
        glow_increment: f("glowIncrement", d.glow_increment),
        glow_decrement: f("glowDecrement", d.glow_decrement),
        max_glow: f("maxGlow", d.max_glow),
        cooling_delay_ms: f("coolingDelayMS", d.cooling_delay_ms as f32) as i32,
        overheat_delay_ms: f("overheatDelayMS", d.overheat_delay_ms as f32) as i32,
        cool_during_overheat_delay: flag(b, "coolDuringOverheatDelay", d.cool_during_overheat_delay),
        overheat_recovery_percent: f("overheatRecoveryPercent", d.overheat_recovery_percent),
    }
}

fn chaingun_data(db: &DeclDb, name: &str) -> Option<ChaingunData> {
    let b = db.get("weapondatachaingun", name).ok()?;
    let e = b.block("edit").cloned().unwrap_or_default();
    let d = ChaingunData::default();
    let f = |k: &str, def: f32| e.f32(k).unwrap_or(def);
    Some(ChaingunData {
        spin_up_ms: f("spinUpTimeMS", d.spin_up_ms as f32) as i32,
        spin_down_ms: f("spinDownTimeMS", d.spin_down_ms as f32) as i32,
        barrel_align_degs_per_sec: f("barrelAlignDegsPerSec", d.barrel_align_degs_per_sec),
        barrel_align_sectors: f("barrelAlignSectors", d.barrel_align_sectors),
        num_barrels: f("numBarrels", d.num_barrels as f32) as i32,
        allow_idle_barrel_spin: flag(&e, "allowIdleBarrelSpin", d.allow_idle_barrel_spin),
        prevent_spin_up_while_dryfiring: flag(&e, "preventSpinUpWhileDryfiring", d.prevent_spin_up_while_dryfiring),
        progressive_firing_interval: flag(&e, "progressiveFiringInterval.useProgressiveFiringInterval", d.progressive_firing_interval),
        firing_interval_min: f("progressiveFiringInterval.firingIntervalMin", d.firing_interval_min as f32) as i32,
    })
}

/// idDeclWeapon_RailGunData (0xf8; ctor 0x1406f6060 defaults: useBaseChargeBehavior false,
/// canChargeWithInsufficientAmmo true, afterFireChargeDelay 1000, secondaryFire chargeTimeMS 0,
/// ammoUsedAtMaxCharge 1, requiresFullChargeToFire false, chargeRequiresZoom true).
#[derive(Debug, Clone, PartialEq)]
pub struct RailGunData {
    pub use_base_charge_behavior: bool,
    pub can_charge_with_insufficient_ammo: bool,
    /// +0x74, compared with game ticks (lastFinishFireTime[1] + delay).
    pub after_fire_charge_delay: f32,
    /// secondaryFire (+0x78): chargeTimeMS +0, fullyChargedProjectileDecl +8, chargeDamageScaleTable +0x30,
    /// ammoUsedAtMaxCharge +0x38, requiresFullChargeToFire +0x50, chargeRequiresZoom +0x52.
    pub charge_time_ms: i32,
    pub fully_charged_projectile: String,
    pub start_sound: String,
    pub charge_sound: String,
    pub fully_charged_sound: String,
    pub discharge_sound: String,
    pub charge_damage_scale_table: Option<crate::handlayers::reactions::DeclTable>,
    pub ammo_used_at_max_charge: i32,
    pub requires_full_charge_to_fire: bool,
    pub charge_requires_zoom: bool,
}

fn railgun_data(db: &DeclDb, name: &str) -> Option<RailGunData> {
    let b = db.get("weapondatarailgun", name).ok()?;
    let e = b.block("edit").cloned().unwrap_or_default();
    let sf = e.block("secondaryFire").cloned().unwrap_or_default();
    let s = |b: &Block, k: &str| b.str(k).filter(|v| !v.is_empty()).unwrap_or_default().to_string();
    Some(RailGunData {
        use_base_charge_behavior: flag(&e, "useBaseChargeBehavior", false),
        can_charge_with_insufficient_ammo: flag(&e, "canChargeWithInsufficientAmmo", true),
        after_fire_charge_delay: e.f32("afterFireChargeDelay").unwrap_or(1000.0),
        charge_time_ms: sf.f32("chargeTimeMS").unwrap_or(0.0) as i32,
        fully_charged_projectile: s(&sf, "fullyChargedProjectileDecl"),
        start_sound: s(&sf, "startSound"),
        charge_sound: s(&sf, "chargeSound"),
        fully_charged_sound: s(&sf, "fullyChargedSound"),
        discharge_sound: s(&sf, "dischargeSound"),
        charge_damage_scale_table: sf.str("chargeDamageScaleTable").filter(|t| !t.is_empty()).and_then(|t| crate::handlayers::reactions::DeclTable::load(db, t).ok()),
        ammo_used_at_max_charge: sf.f32("ammoUsedAtMaxCharge").unwrap_or(1.0) as i32,
        requires_full_charge_to_fire: flag(&sf, "requiresFullChargeToFire", false),
        charge_requires_zoom: flag(&sf, "chargeRequiresZoom", true),
    })
}

/// idWeaponTargetLockData_t (0x78, reflection 0x1430ea100): decl targetLockNormal (+0xa80) / targetLockZoomed
/// (+0xaf8). Defaults from the idDeclWeapon ctor 0x1406f0780 (0x1406f0c9b..0x1406f0d4d). *Sec values are converted
/// to game ticks with the 960/s rate by the targeting code (weapons::targeting).
#[derive(Debug, Clone, PartialEq)]
pub struct TargetLockData {
    /// +0 canLock (false).
    pub can_lock: bool,
    /// +4 lockFOV (45), +8 lockMaxDist (0 = no limit).
    pub lock_fov: f32,
    pub lock_max_dist: f32,
    /// +0xc lockTimeSec (0.5), +0x10 lockTimeoutSec (0.5), +0x14 unlockTimeSec (4), +0x18 outOfFovTimeSec (0.5),
    /// +0x1c outOfLOSTimeSec (1).
    pub lock_time_sec: f32,
    pub lock_timeout_sec: f32,
    pub unlock_time_sec: f32,
    pub out_of_fov_time_sec: f32,
    pub out_of_los_time_sec: f32,
    /// +0x20 automaticallyMaintainLock (true), +0x21 automaticallyInitiateLock (false).
    pub automatically_maintain_lock: bool,
    pub automatically_initiate_lock: bool,
    /// +0x28 / +0x30 / +0x38 / +0x40 sounds, +0x50 noTargetDenySound.
    pub sound_acquiring: String,
    pub sound_locked: String,
    pub sound_lock_broken: String,
    pub sound_lock_disengage: String,
    pub no_target_deny_sound: String,
    /// +0x48 loseLockOnFire, +0x49 requireLockToFire (false).
    pub lose_lock_on_fire: bool,
    pub require_lock_to_fire: bool,
    /// +0x58 maxTargets (1), +0x5c nextTargetTimeoutSec (0.5), +0x60 clearAfterNumShots (0).
    pub max_targets: i32,
    pub next_target_timeout_sec: f32,
    pub clear_after_num_shots: i32,
    /// +0x64 playersBlockLineOfSight (true), +0x70 autoBreakLock (false).
    pub players_block_line_of_sight: bool,
    pub auto_break_lock: bool,
}

impl Default for TargetLockData {
    fn default() -> Self {
        Self {
            can_lock: false,
            lock_fov: 45.0,
            lock_max_dist: 0.0,
            lock_time_sec: 0.5,
            lock_timeout_sec: 0.5,
            unlock_time_sec: 4.0,
            out_of_fov_time_sec: 0.5,
            out_of_los_time_sec: 1.0,
            automatically_maintain_lock: true,
            automatically_initiate_lock: false,
            sound_acquiring: String::new(),
            sound_locked: String::new(),
            sound_lock_broken: String::new(),
            sound_lock_disengage: String::new(),
            no_target_deny_sound: String::new(),
            lose_lock_on_fire: false,
            require_lock_to_fire: false,
            max_targets: 1,
            next_target_timeout_sec: 0.5,
            clear_after_num_shots: 0,
            players_block_line_of_sight: true,
            auto_break_lock: false,
        }
    }
}

fn target_lock_data(b: Option<&Block>) -> TargetLockData {
    let d = TargetLockData::default();
    let Some(b) = b else { return d };
    let f = |k: &str, def: f32| b.f32(k).unwrap_or(def);
    let s = |k: &str| b.str(k).filter(|v| !v.is_empty() && *v != "NULL").unwrap_or_default().to_string();
    TargetLockData {
        can_lock: flag(b, "canLock", d.can_lock),
        lock_fov: f("lockFOV", d.lock_fov),
        lock_max_dist: f("lockMaxDist", d.lock_max_dist),
        lock_time_sec: f("lockTimeSec", d.lock_time_sec),
        lock_timeout_sec: f("lockTimeoutSec", d.lock_timeout_sec),
        unlock_time_sec: f("unlockTimeSec", d.unlock_time_sec),
        out_of_fov_time_sec: f("outOfFovTimeSec", d.out_of_fov_time_sec),
        out_of_los_time_sec: f("outOfLOSTimeSec", d.out_of_los_time_sec),
        automatically_maintain_lock: flag(b, "automaticallyMaintainLock", d.automatically_maintain_lock),
        automatically_initiate_lock: flag(b, "automaticallyInitiateLock", d.automatically_initiate_lock),
        sound_acquiring: s("soundAcquiring"),
        sound_locked: s("soundLocked"),
        sound_lock_broken: s("soundLockBroken"),
        sound_lock_disengage: s("soundLockDisengage"),
        no_target_deny_sound: s("noTargetDenySound"),
        lose_lock_on_fire: flag(b, "loseLockOnFire", d.lose_lock_on_fire),
        require_lock_to_fire: flag(b, "requireLockToFire", d.require_lock_to_fire),
        max_targets: f("maxTargets", d.max_targets as f32) as i32,
        next_target_timeout_sec: f("nextTargetTimeoutSec", d.next_target_timeout_sec),
        clear_after_num_shots: f("clearAfterNumShots", d.clear_after_num_shots as f32) as i32,
        players_block_line_of_sight: flag(b, "playersBlockLineOfSight", d.players_block_line_of_sight),
        auto_break_lock: flag(b, "autoBreakLock", d.auto_break_lock),
    }
}

/// The idDeclWeapon fields of the launched-projectile list (remote detonate; ctor 0x1406f0780: all 0).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExplodeProjectiles {
    /// +0x65c explodeProjectilesOnAltFire, +0x65d explodeProjectilesOnAltFireRelease.
    pub on_alt_fire: bool,
    pub on_alt_fire_release: bool,
    /// +0x660 explodeProjectilesAltFireDelay: raw game ticks between a launch and the earliest detonation.
    pub alt_fire_delay: i32,
    /// +0x668 explodeProjectilesAltFireSound.
    pub alt_fire_sound: String,
    /// +0x67c explodeProjectilesMaxNum (0 = unlimited), +0x680 exceeded sound, +0x688 denial sound.
    pub max_num: i32,
    pub exceeded_max_sound: String,
    pub denial_sound: String,
    /// +0x694 explodeProjectilesAutomaticallyDelay (raw ticks after the launch; 0 = never).
    pub automatically_delay: i32,
}

fn explode_projectiles(e: &Block) -> ExplodeProjectiles {
    let s = |k: &str| e.str(k).filter(|v| !v.is_empty() && *v != "NULL").unwrap_or_default().to_string();
    ExplodeProjectiles {
        on_alt_fire: flag(e, "explodeProjectilesOnAltFire", false),
        on_alt_fire_release: flag(e, "explodeProjectilesOnAltFireRelease", false),
        alt_fire_delay: e.f32("explodeProjectilesAltFireDelay").unwrap_or(0.0) as i32,
        alt_fire_sound: s("explodeProjectilesAltFireSound"),
        max_num: e.f32("explodeProjectilesMaxNum").unwrap_or(0.0) as i32,
        exceeded_max_sound: s("explodeProjectilesExceededMaxSound"),
        denial_sound: s("explodeProjectilesDenialSound"),
        automatically_delay: e.f32("explodeProjectilesAutomaticallyDelay").unwrap_or(0.0) as i32,
    }
}

/// BURSTMODE_* (reflection enum table 0x143569270).
pub fn burst_mode(name: &str) -> i32 {
    match name {
        "BURSTMODE_SINGLE" => 0,
        "BURSTMODE_BURST" => 1,
        "BURSTMODE_FULLAUTO" => 2,
        "BURSTMODE_SELECTFIRE" => 3,
        _ => 0,
    }
}

fn list(b: &Block, key: &str) -> Vec<String> {
    let Some(l) = b.block(key) else { return Vec::new() };
    let n = l.f32("num").unwrap_or(0.0) as usize;
    (0..n).filter_map(|i| l.str(&format!("item[{i}]")).map(str::to_string)).collect()
}

impl WeaponDef {
    pub fn from_decl(db: &DeclDb, name: &str) -> Result<Self> {
        Self::from_decl_with_ammo(db, name, None)
    }

    /// The decl with another ammo decl (a WMT DECL_AMMO upgrade, slot +0x18a0): ammo, pool and projectile
    /// come from `ammo` instead of initialAmmoDecl.
    pub fn from_decl_with_ammo(db: &DeclDb, name: &str, ammo: Option<&str>) -> Result<Self> {
        let b = db.get("weapon", name).with_context(|| format!("weapon decl {name}"))?;
        let e = b.block("edit").cloned().unwrap_or_default();
        let clip0 = e.block("validAmmoClips.item[0]").cloned();
        // Ammo the weapon fires: initialAmmoDecl, else the first valid clip (the super shotgun has no initialAmmoDecl).
        let mut ammo_decl = ammo.or(e.str("initialAmmoDecl")).filter(|s| !s.is_empty() && *s != "NULL").unwrap_or_default().to_string();
        if ammo_decl.is_empty() {
            ammo_decl = clip0.as_ref().and_then(|c| c.str("validAmmoDecl")).unwrap_or_default().to_string();
        }
        // ammoPerShot from the matching ammoClip (ctor 0x1415284e0 default 1).
        let mut ammo_per_shot = 1;
        if let Some(clips) = e.block("validAmmoClips") {
            let n = clips.f32("num").unwrap_or(0.0) as usize;
            for i in 0..n {
                if let Some(c) = clips.block(&format!("item[{i}]")) {
                    if c.str("validAmmoDecl") == Some(ammo_decl.as_str()) {
                        ammo_per_shot = c.f32("ammoPerShot").unwrap_or(1.0) as i32;
                        break;
                    }
                }
            }
        }
        let (mut ammo_max, mut ammo_start, mut proj_name, mut ammo_pool) = (0, 0, String::new(), ammo_decl.to_lowercase());
        if !ammo_decl.is_empty() {
            if let Ok(a) = db.get("ammo", &ammo_decl) {
                ammo_max = a.f32("edit.maxCount").unwrap_or(0.0) as i32;
                ammo_start = a.f32("edit.count").unwrap_or(0.0) as i32;
                proj_name = a.str("edit.projectileDecl").unwrap_or_default().to_string();
                if let Some(pool) = a.str("edit.sharedAmmoPoolDecl").filter(|s| !s.is_empty() && *s != "NULL") {
                    ammo_pool = pool.to_lowercase();
                    if let Ok(p) = db.get("ammo", &ammo_pool) {
                        ammo_max = p.f32("edit.maxCount").unwrap_or(ammo_max as f32) as i32;
                        ammo_start = p.f32("edit.count").unwrap_or(ammo_start as f32) as i32;
                    }
                }
            }
        }
        let projectile = if proj_name.is_empty() { ProjectileDef::default() } else { ProjectileDef::from_decl(db, &proj_name)? };
        let sp = spread_params(&e.block("spreadParams").cloned().unwrap_or_default());
        let fb = feedback(&e.block("weaponFeedBack").cloned().unwrap_or_default());
        let heat = heat_info(&e.block("heatInfo").cloned().unwrap_or_default());
        let chaingun = e.str("weaponData").filter(|s| !s.is_empty()).and_then(|wd| chaingun_data(db, wd));
        let railgun = e.str("weaponData").filter(|s| !s.is_empty()).and_then(|wd| railgun_data(db, wd));
        let mut bursts: [BurstInfo; 4] = Default::default();
        for (i, bi) in bursts.iter_mut().enumerate() {
            if let Some(x) = e.block(&format!("burstInfo.burstInfo[{i}]")) {
                let f = |k: &str, def: f32| x.f32(k).unwrap_or(def);
                let s = |k: &str| x.str(k).filter(|v| *v != "NULL").unwrap_or_default().to_string();
                *bi = BurstInfo {
                    burst_count: f("burstCount", bi.burst_count as f32) as i32,
                    burst_interval: f("burstInterval", bi.burst_interval as f32) as i32,
                    burst_firing_interval: f("burstFiringInterval", bi.burst_firing_interval as f32) as i32,
                    can_queue_burst: flag(x, "canQueueBurst", bi.can_queue_burst),
                    can_interrupt_burst: flag(x, "canInterruptBurst", bi.can_interrupt_burst),
                    ammo_per_burst: f("ammoPerBurst", bi.ammo_per_burst as f32) as i32,
                    burst_shoot_state: s("burstShootState"),
                    last_shot_state: s("lastShotState"),
                    fake_burst_count: f("fakeBurstCount", 0.0) as i32,
                };
            }
        }
        let firing_interval = e.f32("firingInterval").map(|v| v as i32).unwrap_or(500);
        Ok(Self {
            decl: name.to_string(),
            display_name: e.str("displayName").unwrap_or(name).to_string(),
            selection_group: e.str("weaponSelectionGroup").unwrap_or_default().to_string(),
            selection_group_index: e.str("weaponSelectionGroup").and_then(|g| g.strip_prefix("WEAPONSELECTIONGROUP_")).and_then(|n| n.parse().ok()).unwrap_or(10),
            selectable: flag(&e, "selectable", true),
            auto_selectable: flag(&e, "autoSelectable", true),
            quick_swap_remember: flag(&e, "quickSwapRemember", true),
            can_be_slotted: flag(&e, "canBeSlotted", true),
            cant_switch_from: flag(&e, "cantSwitchFrom", false),
            hands_md6: e.str("handsModelMD6").unwrap_or_default().to_string(),
            firing_interval_ms: firing_interval as f32,
            firing_interval,
            added_firing_interval: e.f32("addedFiringInterval").unwrap_or(0.0) as i32,
            bringup_s: e.f32("desiredBringupDurationSecs").unwrap_or(0.0),
            bringdown_s: e.f32("desiredBringdownDurationSecs").unwrap_or(0.0),
            single_tap: flag(&e, "singleTapFire", false),
            single_tap_ads: flag(&e, "singleTapFireADS", false),
            allow_shot_queueing: flag(&e, "allowShotQueueing", false),
            ignore_firing_interval: flag(&e, "ignoreFiringInterval", false),
            infinite_ammo: flag(&e, "infiniteAmmo", false),
            ammo_decl,
            ammo_pool,
            ammo_max,
            ammo_start,
            ammo_per_shot,
            projectile,
            spread_params: sp,
            spread: sp.spread,
            spread_crouch: sp.base_crouch,
            h_spread_scale: sp.horizontal_scale,
            v_spread_scale: sp.vertical_scale,
            kick_pitch: fb.kick_pitch,
            kick_yaw: fb.kick_yaw,
            kick_fov: fb.kick_fov,
            feedback: fb,
            heat,
            chaingun,
            railgun,
            bursts,
            initial_burst_mode: e.str("initialBurstMode").map(burst_mode).unwrap_or(0),
            charge: charge_info(&e.block("chargeInfo").cloned().unwrap_or_default()),
            has_charge_state: flag(&e, "hasChargeState", false),
            can_use_charge_state_when_not_charging: flag(&e, "canUseChargeStateWhenNotCharging", false),
            charge_shoot_to_charge_idle_state: e.str("chargeShootToChargeIdleState").filter(|s| !s.is_empty() && *s != "NULL").unwrap_or_default().to_string(),
            secondary_fire_decl: e.str("secondaryFireDecl").filter(|s| !s.is_empty() && *s != "NULL").unwrap_or_default().to_string(),
            other_fire_mode_firing_interval: e.f32("otherFireModeFiringInterval").unwrap_or(0.0) as i32,
            forbid_zoom_if_cannot_charge: flag(&e, "forbidZoomIfCannotCharge", false),
            forbid_fire_mode_with_insufficient_ammo: flag(&e, "forbidFireModeWithInsufficientAmmo", false),
            use_primary_fire_button: flag(&e, "usePrimaryFireButton", false),
            perk_groups: list(&e, "perkGroups"),
            no_perk_switcher: flag(&e, "noPerkSwitcher", false),
            shot_charge_duration: e.f32("shotChargeDuration").unwrap_or(0.0) as i32,
            zoom_shoot_state: e.str("zoomShootState").filter(|s| *s != "NULL").unwrap_or_default().to_string(),
            // idDeclInventory +0xf8; the ctor (vftable store 0x1406e8828) sets 0.7 at 0x1406e88bb.
            hands_fov_scale: e.f32("handsFovScale").unwrap_or(0.7),
            max_range: e.f32("maxRange").unwrap_or(8192.0),
            post_sprint_fire_penalty_ms: e.f32("postSprintFirePenaltyMS").unwrap_or(0.0) as i32,
            upgrades: list(&e, "upgrades"),
            hands: hands_decl(&e),
            zoom: zoom_info(&e.block("ironSightZoom").cloned().unwrap_or_default()),
            zoom_mode: e.str("zoomMode").map(super::zoom::ZoomMode::parse).unwrap_or_default(),
            forbid_zoom_with_insufficient_ammo: flag(&e, "forbidZoomWithInsufficientAmmo", false),
            zoom_in_sound: e.str("zoomInSound").unwrap_or_default().to_string(),
            zoom_out_sound: e.str("zoomOutSound").unwrap_or_default().to_string(),
            melee: melee_decl(db, &e),
            target_lock_normal: target_lock_data(e.block("targetLockNormal")),
            target_lock_zoomed: target_lock_data(e.block("targetLockZoomed")),
            force_fire_mode_when_locked: flag(&e, "forceFireModeWhenLocked", false),
            targets_friendlies: flag(&e, "targetsFriendlies", false),
            explode: explode_projectiles(&e),
            reticle: e.str("reticle").filter(|v| !v.is_empty() && *v != "NULL").unwrap_or_default().to_string(),
            reticle_when_zoomed: e.str("reticleWhenZoomed").filter(|v| !v.is_empty() && *v != "NULL").unwrap_or_default().to_string(),
            locked_reticle: e.str("lockedReticle").filter(|v| !v.is_empty() && *v != "NULL").unwrap_or_default().to_string(),
            mods: None,
        })
    }

    /// The decl's GetFiringInterval without mods (0x140f12ef0); the chaingun lerps it by barrel spin.
    pub fn firing_interval_at(&self, barrel_spin: f32) -> i32 {
        match &self.chaingun {
            Some(c) if c.progressive_firing_interval => ((self.firing_interval as f32 - c.firing_interval_min as f32) * barrel_spin + c.firing_interval_min as f32) as i32,
            _ => self.firing_interval,
        }
    }

    /// FUN_1413dadb0 (the weapon is the chainsaw); next/prev weapon skip it.
    pub fn is_chainsaw(&self) -> bool {
        self.decl.ends_with("/chainsaw")
    }

    pub fn uses_heat(&self) -> bool {
        self.heat.heat_increment > 0.0
    }
}

/// The campaign arsenal in weapon-wheel order (decl names from the install).
pub const SP_WEAPONS: &[&str] = &[
    "weapon/zion/player/sp/fists",
    "weapon/zion/player/sp/pistol",
    "weapon/zion/player/sp/shotgun",
    "weapon/zion/player/sp/heavy_rifle_heavy_ar",
    "weapon/zion/player/sp/plasma_rifle",
    "weapon/zion/player/sp/rocket_launcher",
    "weapon/zion/player/sp/double_barrel",
    "weapon/zion/player/sp/gauss_rifle",
    "weapon/zion/player/sp/chaingun",
    "weapon/zion/player/sp/bfg",
    "weapon/zion/player/sp/chainsaw",
];

pub fn load_arsenal(db: &DeclDb) -> Vec<Arc<WeaponDef>> {
    SP_WEAPONS
        .iter()
        .filter_map(|n| WeaponDef::from_decl(db, n).map_err(|e| eprintln!("weapon {n}: {e:#}")).ok())
        .map(|mut d| {
            d.mods = super::mods::load_mods(db, &d).map(Arc::new);
            Arc::new(d)
        })
        .collect()
}
