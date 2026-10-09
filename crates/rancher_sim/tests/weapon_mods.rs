//! Weapon mods against the user's own install (skipped without one): mod data from the perk / upgrade decls,
//! charge times in raw ticks, bursts, upgrade effects. Expected numbers come from the decls and the decoded
//! routines in gamedata/re/MODS.md (ChargeTime 0x140f121a0, the charge update 0x140f248c0, DischargeCharge
//! 0x140f06400, FinishFire 0x140f0d5c0, GetFiringInterval 0x140f12ef0).

use std::sync::Arc;

use rancher_sim::install;
use rancher_sim::weapons::arsenal::ms_to_ticks;
use rancher_sim::weapons::{load_arsenal, Arsenal, ChargeState, ModLoadout, WeaponDef, WeaponEvent, WeaponInput, WeaponPhase};

fn defs() -> Option<Vec<Arc<WeaponDef>>> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    Some(load_arsenal(&inst.decls))
}

fn idx(defs: &[Arc<WeaponDef>], suffix: &str) -> usize {
    defs.iter().position(|d| d.decl.ends_with(suffix)).unwrap_or_else(|| panic!("no {suffix}"))
}

/// `weapon` raised, mod family `family` active at upgrade `level` (0 base .. 4 mastery).
fn modded(defs: &[Arc<WeaponDef>], suffix: &str, family: usize, level: u8) -> Arsenal {
    let mut a = Arsenal::new(defs.to_vec());
    let w = idx(defs, suffix);
    a.current = w;
    a.phase = WeaponPhase::Ready;
    let m = a.defs[w].mods.clone().expect("weapon has mods");
    let mut l = ModLoadout::unlock_all(&m, level);
    l.active = Some(family);
    a.set_loadout(w, l);
    a.tick(1, &WeaponInput::default());
    a
}

fn inp(attack: bool, alt: bool) -> WeaponInput {
    WeaponInput { trigger: attack, altfire: alt, ..Default::default() }
}

/// Runs 1-tick frames until `end`, returning shot times; `f(t)` = (attack, altfire).
fn run(a: &mut Arsenal, end: i32, f: impl Fn(i32) -> (bool, bool)) -> Vec<rancher_sim::weapons::Shot> {
    let mut shots = Vec::new();
    while a.time_ms < end {
        let t = a.time_ms + 1;
        let (at, alt) = f(t);
        for e in a.tick(1, &inp(at, alt)) {
            if let WeaponEvent::Fired(s) = e {
                shots.push(s);
            }
        }
    }
    shots
}

#[test]
fn every_campaign_mod_loads_from_the_decls() {
    let Some(defs) = defs() else { return };
    let want = [
        ("/pistol", 1),
        ("/shotgun", 2),
        ("/heavy_rifle_heavy_ar", 2),
        ("/plasma_rifle", 2),
        ("/rocket_launcher", 2),
        ("/double_barrel", 1),
        ("/gauss_rifle", 2),
        ("/chaingun", 2),
    ];
    for (w, n) in want {
        let d = &defs[idx(&defs, w)];
        let m = d.mods.as_ref().unwrap_or_else(|| panic!("{w} mods"));
        assert_eq!(m.families.len(), n, "{w} families");
        for f in &m.families {
            let ups: Vec<_> = f.upgrades.iter().map(|p| p.name.rsplit('/').next().unwrap().to_string()).collect();
            println!("{w:24} {:22} slot {} upgrades {:?} mastery {}", f.name(), f.unhide_slot, ups, f.mastery.is_some());
            // RL detonate lists 2 upgrade perks, every other mod 3.
            assert!((2..=3).contains(&f.upgrades.len()), "{w} {} upgrades", f.name());
            // The SSG has no mod: its "default" base perk carries no upgrade.
            assert!(f.name() == "default" || !f.base.upgrades.is_empty(), "{w} {} base perk upgrades", f.name());
        }
        // Every family resolves its fire-mode decls fully upgraded.
        let mut a = Arsenal::new(defs.clone());
        let wi = idx(&defs, w);
        for fi in 0..n {
            let mut l = ModLoadout::unlock_all(m, 4);
            l.active = Some(fi);
            a.set_loadout(wi, l);
            let m1 = a.mode_def(wi, 1).map(|d| d.decl.clone()).unwrap_or_default();
            println!("    {} -> mode0 {} mode1 {m1} upgrades {:?}", m.families[fi].name(), a.mode_def(wi, 0).unwrap().decl, a.applied[wi].upgrades);
        }
    }
    let sg = &defs[idx(&defs, "/shotgun")];
    let f = &sg.mods.as_ref().unwrap().families;
    assert_eq!((f[0].name(), f[0].unhide_slot), ("secondary_charge_burst", 1));
    assert_eq!((f[1].name(), f[1].unhide_slot), ("pop_rocket", 2));
}

#[test]
fn shotgun_charged_burst_charges_in_raw_ticks_and_fires_three_shots() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/shotgun", 0, 0);
    let w = a.current;
    let d1 = a.mode_def(w, 1).expect("secondary decl");
    assert!(d1.decl.ends_with("shotgun_secondary_charge_burst"));
    assert_eq!(a.secondary_trigger_mode(w), 7, "SECONDARY_HOLD_PRIMARY_PRESS");
    assert_eq!((a.charge_time(w, 1), a.discharge_timeout(w, 1)), (500, 2750));
    let t0 = a.time_ms;
    // Hold altfire: mode 1, chargePercent = ceil(min(t, 500)) / 500 with t in game ticks (not ms).
    run(&mut a, t0 + 250, |_| (false, true));
    assert_eq!(a.mstate[w].fire_mode, 1);
    let start = a.mstate[w].charge.start_time;
    let p = a.mstate[w].charge.percent;
    assert!((p - (t0 + 250 - start) as f32 / 500.0).abs() < 1e-6, "half charge {p}");
    run(&mut a, start + 499, |_| (false, true));
    assert!(a.mstate[w].charge.percent < 1.0);
    // Attack before full charge: the fire gate refuses (minChargeRequiredToDischarge 1).
    let early = run(&mut a, start + 499, |_| (true, true));
    assert!(early.is_empty());
    run(&mut a, start + 500, |_| (false, true));
    assert_eq!(a.mstate[w].charge.percent, 1.0);
    assert_eq!(a.mstate[w].charge.state, ChargeState::FullyCharged);
    let shells = a.ammo_for(w).unwrap();
    let t1 = a.time_ms;
    let shots = run(&mut a, t1 + 2000, |t| (t == t1 + 1, true));
    let times: Vec<i32> = shots.iter().map(|s| s.time_ms).collect();
    println!("burst shots {times:?}");
    assert_eq!(shots.len(), 3, "BURST_COUNT 3 at full charge");
    assert_eq!(times[1] - times[0], ms_to_ticks(150), "150 ms apart = 144 ticks");
    assert_eq!(times[2] - times[1], ms_to_ticks(150));
    assert!(shots.iter().all(|s| s.mode == 1 && s.dirs.len() == 10 && s.def.projectile.name.ends_with("shotgun_triple_burst")));
    assert_eq!(a.ammo_for(w).unwrap(), shells - 3, "one shell per shot");
    // DischargeCharge on the first shot: timeout 2750 raw ticks + 3 x 150 (burst value x interval, raw).
    let ch = &a.mstate[w].charge;
    assert_eq!(ch.can_charge_time, times[0] + 2750 + 3 * 150);
    assert_eq!(ch.percent, 0.0);
    // No recharge until then.
    let cct = ch.can_charge_time;
    run(&mut a, cct - 1, |_| (false, true));
    assert_eq!(a.mstate[w].charge.percent, 0.0);
    assert_eq!(a.mstate[w].charge.state, ChargeState::Cooling);
    run(&mut a, cct + 10, |_| (false, true));
    assert!(a.mstate[w].charge.percent > 0.0, "charging again after the timeout");

    // Released before firing: the charge is dropped with no timeout.
    let mut b = modded(&defs, "/shotgun", 0, 0);
    let t = b.time_ms;
    run(&mut b, t + 300, |_| (false, true));
    assert!(b.mstate[w].charge.percent > 0.0);
    run(&mut b, t + 301, |_| (false, false));
    assert_eq!((b.mstate[w].fire_mode, b.mstate[w].charge.percent), (0, 0.0));
    assert!(b.mstate[w].charge.can_charge_time <= t + 301);
}

#[test]
fn shotgun_charged_burst_upgrades_change_charge_rate_and_timeout() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/shotgun", 0, 3);
    let w = a.current;
    // faster_charge CHARGE_TIME 250, faster_fire_rate FIRING_INTERVAL 120, faster_recharge CHARGE_TIMEOUT 1520.
    assert_eq!((a.charge_time(w, 1), a.discharge_timeout(w, 1)), (250, 1520));
    let t0 = a.time_ms;
    run(&mut a, t0 + 260, |_| (false, true));
    assert_eq!(a.mstate[w].charge.percent, 1.0);
    let t1 = a.time_ms;
    let shots = run(&mut a, t1 + 1000, |t| (t == t1 + 1, true));
    let times: Vec<i32> = shots.iter().map(|s| s.time_ms).collect();
    assert_eq!(shots.len(), 3);
    assert_eq!(times[1] - times[0], ms_to_ticks(120), "120 ms = 115 ticks");
    assert_eq!(a.mstate[w].charge.can_charge_time, times[0] + 1520 + 3 * 120);
}

#[test]
fn shotgun_pop_rockets_charge_burst_and_upgrades() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/shotgun", 1, 0);
    let w = a.current;
    assert!(a.mode_def(w, 1).unwrap().decl.ends_with("shotgun_secondary_pop_rockets"));
    assert_eq!((a.charge_time(w, 1), a.discharge_timeout(w, 1)), (300, 4000));
    let t0 = a.time_ms;
    run(&mut a, t0 + 310, |_| (false, true));
    assert_eq!(a.mstate[w].charge.percent, 1.0);
    let t1 = a.time_ms;
    let shots = run(&mut a, t1 + 1000, |t| (t == t1 + 1, true));
    assert_eq!(shots.len(), 1, "BURST_COUNT max 1");
    let s = &shots[0];
    println!("pop rocket: {} ammo {} cct {}", s.def.projectile.name, s.ammo_used, a.mstate[w].charge.can_charge_time);
    assert!(s.def.projectile.name.contains("pop_rocket") && !s.def.projectile.hitscan);
    assert_eq!(s.ammo_used, 1, "AMMO_TO_USE 1");
    assert_eq!(a.mstate[w].charge.can_charge_time, s.time_ms + 4000 + 200, "timeout + 1 x 200 raw");
    // Upgrades: faster_charge CHARGE_TIME 20, faster_recharge CHARGE_TIMEOUT 2500, larger_explosion DECL_AMMO.
    let b = modded(&defs, "/shotgun", 1, 3);
    assert_eq!((b.charge_time(w, 1), b.discharge_timeout(w, 1)), (20, 2500));
    assert!(b.mode_def(w, 1).unwrap().projectile.name.contains("larger_explosion"), "{}", b.mode_def(w, 1).unwrap().projectile.name);
}

#[test]
fn pistol_charge_shot_damage_scale_follows_the_charge() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/pistol", 0, 0);
    let w = a.current;
    let d1 = a.mode_def(w, 1).unwrap();
    assert_eq!(a.charge_time(w, 1), 2500);
    let t0 = a.time_ms;
    run(&mut a, t0 + 1250, |_| (false, true));
    let start = a.mstate[w].charge.start_time;
    let p = a.mstate[w].charge.percent;
    let t1 = a.time_ms;
    let shots = run(&mut a, t1 + 1, |_| (true, true));
    assert_eq!(shots.len(), 1);
    let s = &shots[0];
    // DAMAGE_SCALE item 1..5 at the charge: (1 - p) * 1 + p * 5.
    let pct = ((t1 - start) as f32).ceil() / 2500.0;
    assert!((p - pct).abs() < 1e-3);
    assert!((s.damage_scale - (1.0 + 4.0 * s.charge)).abs() < 1e-4, "scale {} at {}", s.damage_scale, s.charge);
    // Timeout 1600 raw, scaled by the charge discharged from when scaleDischargeTimeoutByDischargePct.
    let t = if d1.charge.scale_discharge_timeout_by_discharge_pct { (1600.0 * s.charge) as i32 } else { 1600 };
    assert_eq!(a.mstate[w].charge.can_charge_time, s.time_ms + t);
    // Upgrades: faster_charge 1450, higher_damage CHARGE_VALUE_MAX 8.
    let mut b = modded(&defs, "/pistol", 0, 4);
    assert_eq!(b.charge_time(w, 1), 1450);
    let t0 = b.time_ms;
    run(&mut b, t0 + 1460, |_| (false, true));
    let t1 = b.time_ms;
    let shots = run(&mut b, t1 + 1, |_| (true, true));
    assert_eq!(shots[0].charge, 1.0);
    assert!((shots[0].damage_scale - 8.0).abs() < 1e-4, "{}", shots[0].damage_scale);
}

#[test]
fn heavy_ar_micro_missile_upgrades_and_scope_use_time_values() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/heavy_rifle_heavy_ar", 1, 3);
    let w = a.current;
    // faster_charge_time CHARGE_TIME 100, faster_recharge CHARGE_TIMEOUT 1500, lower_ammo_cost AMMO_TO_USE 2.
    assert_eq!((a.charge_time(w, 1), a.discharge_timeout(w, 1)), (100, 1500));
    let t0 = a.time_ms;
    run(&mut a, t0 + 110, |_| (false, true));
    let t1 = a.time_ms;
    let shots = run(&mut a, t1 + 10, |t| (t == t1 + 1, true));
    assert_eq!(shots.len(), 1);
    assert_eq!(shots[0].ammo_used, 2, "lower_ammo_cost");
    assert_eq!(a.mstate[w].charge.can_charge_time, shots[0].time_ms + 1500, "timeout x full charge 1.0");
    // Mastery: BURST_MODE FULLAUTO, the mastery decl and ammo.
    let m = modded(&defs, "/heavy_rifle_heavy_ar", 1, 4);
    assert_eq!(m.burst_mode(w), 2);
    assert!(m.mode_def(w, 1).unwrap().decl.ends_with("burst_detonate_mastery"));
    // Scope: use-time values from its upgrades (penetration 8 / 1000, headshot + 1.25; mastery DAMAGE_SCALE 1.75
    // and the mastered ammo).
    let s = modded(&defs, "/heavy_rifle_heavy_ar", 0, 4);
    let u = &s.applied[w].use_time[1];
    assert_eq!((u.penetration_max_num, u.penetration_energy), (8, 1000));
    assert!((u.headshot_add_damage_scale - 1.25).abs() < 1e-6 && (u.damage_scale - 1.75).abs() < 1e-6, "{u:?}");
    assert!(s.mode_def(w, 1).unwrap().projectile.name.contains("mastered") || s.mode_def(w, 1).unwrap().ammo_decl.contains("mastered"));
}

#[test]
fn gauss_siege_mode_charge_and_upgrades() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/gauss_rifle", 1, 0);
    let w = a.current;
    assert_eq!((a.charge_time(w, 1), a.discharge_timeout(w, 1)), (1600, 1000));
    let t0 = a.time_ms;
    run(&mut a, t0 + 1599, |_| (false, true));
    let te = a.time_ms + 1;
    let early = run(&mut a, te, |_| (true, true));
    assert!(early.is_empty(), "minChargeRequiredToDischarge 1: no shot before the 1600-tick charge");
    let t1 = a.time_ms;
    let shots = run(&mut a, t1 + 100, |_| (true, true));
    println!("siege early {} shots {:?}", early.len(), shots.iter().map(|s| s.time_ms).collect::<Vec<_>>());
    assert_eq!(shots.len(), 1);
    // scaleShootAnimToMatchChargeTime: GetFiringInterval = chargeTime / shotsPerShootAnim.
    assert_eq!(a.firing_interval(w), 1600);
    let b = modded(&defs, "/gauss_rifle", 1, 2);
    assert!(b.mode_def(w, 1).unwrap().decl.ends_with("siege_mode_upgraded"), "outer_beam DECL_WEAPON");
    assert_eq!((b.charge_time(w, 1), b.discharge_timeout(w, 1)), (1000, 750));
}

#[test]
fn plasma_heat_blast_charges_from_primary_shots() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/plasma_rifle", 0, 0);
    let w = a.current;
    assert_eq!(a.secondary_trigger_mode(w), 5);
    let t0 = a.time_ms;
    let shots = run(&mut a, t0 + 960, |_| (true, false));
    let pct = a.mstate[w].charge.percent;
    println!("plasma: {} primary shots -> charge {pct}", shots.len());
    // chargeIncrement 0.6 per primary shot, maxCharge 25.
    assert!((pct - (shots.len() as f32 * 0.6 / 25.0).min(1.0)).abs() < 1e-3, "{pct}");
    // Altfire press: one heat blast shot (mode 1), the charge discharges.
    let t1 = a.time_ms;
    let blast = run(&mut a, t1 + 2000, |t| (false, t == t1 + 1));
    assert_eq!(blast.len(), 1);
    assert_eq!(blast[0].mode, 1);
    assert_eq!(a.mstate[w].charge.percent, 0.0);
    // faster_charge CHARGE_PER_SHOT_INCREMENT 0.85.
    let b = modded(&defs, "/plasma_rifle", 0, 1);
    assert!((b.applied[w].modes[1].charge_per_shot_increment - 0.85).abs() < 1e-6);
}

/// Heat blast damage: the secondary shot's damage scale is PRIMARY_CHARGE_SECONDARY_DAMAGE_SCALE at the built-up
/// charge, looked up in the item's valueTable weapon/plasma_rifle/energybuildupdamage (spline, min 1 max 350;
/// idLookupTable without the left/right remap); more_damage swaps the table (CHARGE_VALUE_TABLE, max 600).
#[test]
fn plasma_heat_blast_damage_scale_from_the_value_table() {
    use rancher_sim::weapons::charge::lookup_raw;
    use rancher_sim::weapons::ChargeProperty as P;
    let Some(defs) = defs() else { return };
    for (level, table, max) in [(0u8, "weapon/plasma_rifle/energybuildupdamage", 350.0f32), (3, "weapon/plasma_rifle/energybuildupdamage_more_damage", 600.0)] {
        let mut a = modded(&defs, "/plasma_rifle", 0, level);
        let w = a.current;
        let t = a.defs[w].mods.as_ref().unwrap().tables.get(table).cloned().expect("table loaded");
        assert_eq!((t.min, t.max), (1.0, max));
        let t0 = a.time_ms;
        run(&mut a, t0 + 960, |_| (true, false));
        let pct = a.mstate[w].charge.percent;
        assert!(0.0 < pct && pct < 1.0);
        a.set_fire_mode(w, 1);
        let v = a.charge_value(w, &P::PrimaryChargeSecondaryDamageScale, pct);
        a.set_fire_mode(w, 0);
        assert!((v - lookup_raw(&t, pct)).abs() < 1e-4, "level {level}: {v} vs table");
        let t1 = a.time_ms;
        let blast = run(&mut a, t1 + 2000, |tt| (false, tt == t1 + 1));
        println!("level {level}: charge {pct} -> blast damage scale {}", blast[0].damage_scale);
        assert_eq!(blast.len(), 1);
        assert!((blast[0].damage_scale - v).abs() < 1e-3 * v.max(1.0));
    }
}

#[test]
fn heavy_ar_micro_missiles_explode_on_their_own_after_the_delay() {
    let Some(defs) = defs() else { return };
    let mut a = modded(&defs, "/heavy_rifle_heavy_ar", 1, 0);
    let w = a.current;
    let d = a.mode_def(w, 1).unwrap();
    // explodeProjectilesAutomaticallyDelay 750 raw ticks: the weapon think's timed ExplodeLaunchedProjectiles.
    assert_eq!(d.explode.automatically_delay, 750);
    let t0 = a.time_ms;
    a.add_launched(w, &d, 7, t0);
    let mut fired = None;
    for _ in 0..800 {
        for e in a.tick(1, &WeaponInput::default()) {
            if let WeaponEvent::Detonate { projectiles, .. } = e {
                assert_eq!(projectiles, vec![7]);
                fired = Some(a.time_ms);
            }
        }
        if fired.is_some() {
            break;
        }
    }
    assert_eq!(fired, Some(t0 + 750));
    assert!(a.launched[w].list.is_empty());
}
