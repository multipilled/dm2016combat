//! Weapon mods through the idHands driver on the real fp_hands anim web (user's install; skipped without
//! one): trigger mode 7 alt-fire, burst shoot states, charge-scaled shots. Shots come from the web's
//! ae_fireWeaponRight events (gamedata/re/MODS.md 3b, HANDS.md).

use std::sync::Arc;

use idres::animweb::AnimWeb;
use rancher_sim::animweb::{AnimData, AnimWebRuntime};
use rancher_sim::install;
use rancher_sim::weapons::arsenal::ms_to_ticks;
use rancher_sim::weapons::hands::{Hands, HandsInput, HandsWeb};
use rancher_sim::weapons::{load_arsenal, Arsenal, ModLoadout, Shot, WeaponEvent, WeaponInput};

struct Rig {
    arsenal: Arsenal,
    hands: Hands,
    web: AnimWebRuntime,
    /// Report the view as zoomed while altfire is held (weapons::zoom is not driven here).
    zoom_with_alt: bool,
}

fn rig(weapon: &str, family: usize, level: u8) -> Option<Rig> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).ok()?;
    let defs = load_arsenal(&inst.decls);
    let c = inst.decls.container_arc();
    let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands.decl").ok()?).into_owned();
    let web = Arc::new(AnimWeb::parse(&text).unwrap());
    let subs: Vec<String> = defs.iter().map(|d| d.hands.subweb.clone()).collect();
    let sub_refs: Vec<&str> = subs.iter().map(String::as_str).collect();
    let data = Arc::new(AnimData::load(&c, &web, &sub_refs));
    let mut arsenal = Arsenal::new(defs.clone());
    let w = defs.iter().position(|d| d.decl.ends_with(weapon)).expect("weapon");
    arsenal.current = w;
    let m = defs[w].mods.clone().unwrap();
    let mut l = ModLoadout::unlock_all(&m, level);
    l.active = Some(family);
    arsenal.set_loadout(w, l);
    let mut web = AnimWebRuntime::new(web, data);
    let mut hands = Hands::new(defs.len());
    hands.start(&mut arsenal, &mut web);
    let mut r = Rig { arsenal, hands, web, zoom_with_alt: false };
    // Bring-up to idle.
    r.run(16, 1500, |_| (false, false));
    Some(r)
}

impl Rig {
    /// Frames of `ms` until game time `end`; `f(t)` = (attack, altfire). Returns shots and states entered.
    fn run(&mut self, ms: i32, end: i32, f: impl Fn(i32) -> (bool, bool)) -> (Vec<Shot>, Vec<(i32, String)>) {
        let mut shots = Vec::new();
        let mut states = Vec::new();
        let mut last = String::new();
        while self.arsenal.time_ms + ms <= end {
            let t = self.arsenal.time_ms + ms;
            let (a, alt) = f(t);
            let mut inp = HandsInput { weapon: WeaponInput { trigger: a, altfire: alt, ..Default::default() }, altfire: alt, ..Default::default() };
            inp.weapon.spread.zoomed = alt && self.zoom_with_alt;
            for e in self.hands.tick(&mut self.arsenal, &mut self.web, ms, &inp) {
                if let WeaponEvent::Fired(s) = e {
                    shots.push(s);
                }
            }
            let now = HandsWeb::current(&self.web).map(|(s, n)| format!("{s}/{n}")).unwrap_or_default();
            if now != last {
                states.push((self.arsenal.time_ms, now.clone()));
                last = now;
            }
        }
        (shots, states)
    }
}

fn times(s: &[Shot]) -> Vec<i32> {
    s.iter().map(|s| s.time_ms).collect()
}

#[test]
fn shotgun_charged_burst_through_the_hands_web() {
    let Some(mut r) = rig("/shotgun", 0, 0) else { return };
    let w = r.arsenal.current;
    let t0 = r.arsenal.time_ms;
    r.run(1, t0 + 520, |_| (false, true));
    assert_eq!(r.arsenal.mstate[w].charge.percent, 1.0);
    let t1 = r.arsenal.time_ms;
    let (shots, states) = r.run(1, t1 + 1500, |t| (t <= t1 + 30, true));
    println!("shots {:?}\nstates {states:?}", times(&shots));
    assert_eq!(shots.len(), 3, "three burst shots from the shoot_burst state");
    assert!(states.iter().any(|(_, s)| s.ends_with("/shoot_burst")));
    assert!(shots.iter().all(|s| s.mode == 1 && s.dirs.len() == 10));
    let t = times(&shots);
    assert!(t[1] - t[0] >= ms_to_ticks(150) && t[2] - t[1] >= ms_to_ticks(150), "no faster than the 150 ms interval");
}

#[test]
fn heavy_ar_micro_missiles_through_the_hands_web() {
    let Some(mut r) = rig("/heavy_rifle_heavy_ar", 1, 0) else { return };
    let w = r.arsenal.current;
    assert!(r.arsenal.mode_def(w, 1).unwrap().decl.ends_with("burst_detonate_variation_a"));
    let t0 = r.arsenal.time_ms;
    r.run(1, t0 + 520, |_| (false, true));
    assert_eq!(r.arsenal.mstate[w].charge.percent, 1.0);
    let ammo = r.arsenal.ammo_for(w).unwrap();
    let t1 = r.arsenal.time_ms;
    let (shots, states) = r.run(1, t1 + 2500, |_| (true, true));
    println!("micro missiles {:?} ammo {} -> {}\nstates {states:?}", times(&shots), ammo, r.arsenal.ammo_for(w).unwrap());
    println!("charge {:?}", r.arsenal.mstate[w].charge);
    assert!(!shots.is_empty());
    assert!(shots.iter().all(|s| s.mode == 1 && s.def.projectile.name.contains("burst_detonate")));
    // The first missile's DischargeCharge(1) starts the 2100-tick timeout (x the full charge), and the fire
    // gate refuses the discharge mode while cooling (0x140f19b90 / 0x140f167c0).
    let t = times(&shots);
    assert!(t.windows(2).all(|p| p[1] - p[0] >= 2100), "one missile per timeout as decoded");
    // AMMO_TO_USE 3 bullets per missile.
    assert_eq!(ammo - r.arsenal.ammo_for(w).unwrap(), 3 * shots.len() as i32);
}

#[test]
fn heavy_ar_scope_zooms_mode_one_full_auto() {
    let Some(mut r) = rig("/heavy_rifle_heavy_ar", 0, 0) else { return };
    let w = r.arsenal.current;
    let t0 = r.arsenal.time_ms;
    let (shots, _) = r.run(1, t0 + 1000, |t| (t > t0 + 200, true));
    println!("scoped {:?}", times(&shots));
    assert!(shots.len() >= 4);
    assert!(shots.iter().all(|s| s.mode == 1 && s.def.decl.ends_with("heavy_rifle_heavy_zoom")));
    let t = times(&shots);
    assert!(t.windows(2).all(|p| p[1] - p[0] >= ms_to_ticks(144)));
    assert_eq!(r.arsenal.applied[w].modes[0].zoom_mode, Some(rancher_sim::weapons::ZoomMode::Weapon));
}

#[test]
fn mod_switch_hides_then_unhides_through_mod_select() {
    use rancher_sim::weapons::{ModSwitch, ModSwitchEvent};
    let Some(mut r) = rig("/shotgun", 0, 0) else { return };
    let w = r.arsenal.current;
    let mut sw = ModSwitch::default();
    let t0 = r.arsenal.time_ms;
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut last = String::new();
    while r.arsenal.time_ms < t0 + 4000 {
        let now = r.arsenal.time_ms + 1;
        if let Some(e) = sw.update(&mut r.arsenal, &mut r.hands, now, now == t0 + 1, false) {
            events.push((now, e));
        }
        r.run(1, now, |_| (false, false));
        let s = HandsWeb::current(&r.web).map(|(a, b)| format!("{a}/{b}")).unwrap_or_default();
        if s != last {
            states.push((now, s.clone()));
            last = s;
        }
    }
    println!("events {events:?}\nstates {states:?}");
    // +50 ticks: hide; +350: the mod swaps (pop rockets, slot 2) and the hands come back via mod select.
    assert!(matches!(events[0], (t, ModSwitchEvent::Hide { .. }) if t == t0 + 1 + 51));
    assert!(matches!(events[1], (t, ModSwitchEvent::Switched { family: 1, .. }) if t == t0 + 1 + 51 + 301));
    assert_eq!(r.arsenal.loadouts[w].active, Some(1));
    assert!(r.arsenal.mode_def(w, 1).unwrap().decl.ends_with("shotgun_secondary_pop_rockets"));
    assert!(states.iter().any(|(_, s)| s.ends_with("/generic_hide")));
    assert!(states.iter().any(|(_, s)| s.ends_with("/generic_unhide_mod_select")));
    assert_eq!(r.hands.scalars.unhide_weapon_mod_select, 2.0);
    assert!(last.ends_with("/idle"), "back to idle: {last}");
}

#[test]
fn gauss_precision_bolt_and_siege_mode_fire() {
    for (fam, name) in [(0, "charged_sniper"), (1, "siege_mode")] {
        let Some(mut r) = rig("/gauss_rifle", fam, 0) else { return };
        let w = r.arsenal.current;
        let d = r.arsenal.mode_def(w, 1).unwrap();
        println!("{name}: decl {} charge {} timeout {} interval {}", d.decl, r.arsenal.charge_time(w, 1), r.arsenal.discharge_timeout(w, 1), {
            r.arsenal.set_fire_mode(w, 1);
            let i = r.arsenal.firing_interval(w);
            r.arsenal.set_fire_mode(w, 0);
            i
        });
        let t0 = r.arsenal.time_ms;
        let (shots, states) = r.run(1, t0 + 4000, |t| (t > t0 + 2000 && t < t0 + 2040, true));
        println!("{name} shots {:?} charge {:?}\nstates {:?}", times(&shots), r.arsenal.mstate[w].charge.state, &states[..states.len().min(12)]);
        assert_eq!(shots.len(), 1, "{name}: one shot");
        assert_eq!(shots[0].mode, 1);
    }
}

/// hasChargeState mods (HANDS.md / MODS.md "hands charge states"): holding altfire while the weapon can charge
/// requests HANDSACTION_CHARGE (idle -> charge_idle through charge_into); the decl's
/// canUseChargeStateWhenNotCharging keeps the hands there; releasing altfire returns the weapon to mode 0,
/// whose decl has no charge state, so the hands leave through charge_out.
#[test]
fn charge_state_mods_enter_and_leave_charge_idle() {
    use rancher_sim::weapons::HandsState;
    for (weapon, fam, mod_decl) in [("/pistol", 0, "pistol_secondary_charge_shot"), ("/gauss_rifle", 1, "gauss_rifle_siege_mode"), ("/chaingun", 1, "chaingun_turret_secondary")] {
        let Some(mut r) = rig(weapon, fam, 0) else { return };
        let w = r.arsenal.current;
        let d = r.arsenal.mode_def(w, 1).unwrap();
        assert!(d.decl.ends_with(mod_decl), "{weapon}: {}", d.decl);
        assert!(d.has_charge_state && d.can_use_charge_state_when_not_charging);
        // charge_into plays at chargeIntoRateScale, i.e. over the charge time.
        let t0 = r.arsenal.time_ms;
        let (_, states) = r.run(1, t0 + r.arsenal.charge_time(w, 1) + 300, |_| (false, true));
        println!("{weapon} hold {states:?} charge {:?} rates {} {}", r.arsenal.mstate[w].charge.state, r.hands.scalars.charge_into_rate_scale, r.hands.scalars.charge_out_rate_scale);
        assert!(states.iter().any(|(_, s)| s.ends_with("/charge_idle")), "{weapon}: into charge_idle");
        assert_eq!(r.hands.target, HandsState::ChargeIdle);
        // chargeIntoRateScale = len(charge_into) / (ChargeTime / 960).
        let into = r.web.state_frames(&d.hands.subweb, "charge_into").unwrap() as f32 / 30.0;
        let t = r.arsenal.charge_time(w, 1) as f32 / 960.0;
        assert!((r.hands.scalars.charge_into_rate_scale - into / t).abs() < 1e-5);
        let t1 = r.arsenal.time_ms;
        let (_, states) = r.run(1, t1 + 1500, |_| (false, false));
        println!("{weapon} release {states:?}");
        assert!(states.iter().any(|(_, s)| s.ends_with("/charge_out") || s.ends_with("/idle")), "{weapon}: out of the charge state");
        assert_eq!(r.hands.target, HandsState::Idle, "{weapon}: back to idle");
    }
}

/// The pistol charge shot fires from charge_idle through charge_shoot (0x1c29) and its DAMAGE_SCALE item scales
/// with the charge (1..5).
#[test]
fn pistol_charge_shot_fires_through_charge_shoot() {
    let Some(mut r) = rig("/pistol", 0, 0) else { return };
    let w = r.arsenal.current;
    let t0 = r.arsenal.time_ms;
    r.run(1, t0 + 2700, |_| (false, true));
    assert_eq!(r.arsenal.mstate[w].charge.percent, 1.0);
    let t1 = r.arsenal.time_ms;
    let (shots, states) = r.run(1, t1 + 800, |t| (t <= t1 + 20, true));
    println!("pistol charge shot {:?}\nstates {states:?}", times(&shots));
    assert_eq!(shots.len(), 1);
    assert_eq!(shots[0].mode, 1);
    assert!((shots[0].damage_scale - 5.0).abs() < 1e-4, "full charge: x5 ({})", shots[0].damage_scale);
    assert!(states.iter().any(|(_, s)| s.ends_with("/charge_shoot")));
}

/// Gauss precision bolt (idRailGun ADS charge, weaponDataRailGun gauss_cannon_charged_sniper: chargeTimeMS 1200,
/// afterFireChargeDelay 0, chargeDamageScaleTable gauss_cannon_charge): zoomed in mode 1 the charge ramps over
/// 1200 ticks; the shot's damage scale is the table at the charge (4.0 full); the shot clears the charge and the
/// heatIncrement 1 overheats the gun for 1200 ticks, which pushes the next charge start past the overheat.
#[test]
fn gauss_precision_bolt_charges_while_zoomed() {
    let Some(mut r) = rig("/gauss_rifle", 0, 0) else { return };
    r.zoom_with_alt = true;
    let w = r.arsenal.current;
    let d1 = r.arsenal.mode_def(w, 1).unwrap();
    let rg = d1.railgun.clone().expect("railgun data");
    assert!(!rg.use_base_charge_behavior && rg.charge_requires_zoom);
    assert_eq!((rg.charge_time_ms, rg.after_fire_charge_delay), (1200, 0.0));
    let t0 = r.arsenal.time_ms;
    r.run(1, t0 + 600, |_| (false, true));
    let start = r.arsenal.railgun[w].ads_charge_start;
    let half = r.arsenal.railgun_charge_percent(w, r.arsenal.time_ms);
    println!("start {start} pct at +600 {half}");
    assert!(start > t0 && (half - (r.arsenal.time_ms - start) as f32 / 1200.0).abs() < 1e-6);
    r.run(1, t0 + 1400, |_| (false, true));
    assert_eq!(r.arsenal.railgun_charge_percent(w, r.arsenal.time_ms), 1.0);
    assert!(r.arsenal.railgun[w].fully_charged);
    let t1 = r.arsenal.time_ms;
    let (shots, _) = r.run(1, t1 + 200, |t| (t <= t1 + 20, true));
    assert_eq!(shots.len(), 1);
    assert!((shots[0].damage_scale - 4.0).abs() < 1e-5, "full charge scale {}", shots[0].damage_scale);
    assert_eq!(r.arsenal.states[w].overheat > 0, true, "heatIncrement 1 overheats");
    // Overheated: the charge restarts at now + overheat timer, and reads 0 until then.
    let s = r.arsenal.railgun[w].ads_charge_start;
    println!("after shot: start {s} now {} overheat {}", r.arsenal.time_ms, r.arsenal.states[w].overheat);
    assert!(s >= r.arsenal.time_ms);
    assert_eq!(r.arsenal.railgun_charge_percent(w, r.arsenal.time_ms), 0.0);
    // Unzoomed (altfire released): mode 0, no charge.
    r.run(1, r.arsenal.time_ms + 50, |_| (false, false));
    assert_eq!(r.arsenal.railgun[w].ads_charge_start, 0);
}

/// Chaingun turret: SetFireMode applies FIRE_DELAY (secondary 850, primary 250 ticks) as canAttackTime of the
/// new mode, so the deployed turret cannot fire for 850 ticks after altfire goes down.
#[test]
fn chaingun_turret_fire_delay_on_mode_change() {
    let Some(mut r) = rig("/chaingun", 1, 0) else { return };
    let w = r.arsenal.current;
    assert_eq!((r.arsenal.applied[w].modes[1].fire_delay, r.arsenal.applied[w].modes[0].fire_delay), (850, 250));
    let t0 = r.arsenal.time_ms;
    let (shots, states) = r.run(1, t0 + 2000, |_| (true, true));
    let m1 = r.arsenal.states[w].fire_delay_until[1];
    println!("turret canAttackTime[1] {m1} (t0 {t0}) first shot {:?} shots {}
states {states:?}", shots.first().map(|s| s.time_ms), shots.len());
    // The deployed turret shoots from charge_shootstate (ae_fireWeaponRightSecondaryOnly events).
    assert!(states.iter().any(|(_, s)| s.ends_with("/charge_shootstate") || s.ends_with("/charge_shootstate_into")));
    assert!(m1 >= t0 + 850 && m1 <= t0 + 852);
    assert!(!shots.is_empty() && shots.iter().all(|s| s.mode == 1 && s.time_ms >= m1));
    let t1 = r.arsenal.time_ms;
    r.run(1, t1 + 5, |_| (false, false));
    assert_eq!(r.arsenal.states[w].fire_delay_until[0], t1 + 1 + 250);
}

/// Gatling rotator: its weaponData (chaingun_gatling, allowIdleBarrelSpin) lets ALTFIRE spin the barrel without
/// firing (SetSpinRequest 0x140ec9ea0); spinUpTimeMS 1350, CHAINGUN_SPIN_UP_TIME_MS 810 with faster_spinup.
#[test]
fn chaingun_gatling_altfire_spins_up() {
    for (level, up) in [(0u8, 1350.0f32), (1, 810.0)] {
        let Some(mut r) = rig("/chaingun", 0, level) else { return };
        let w = r.arsenal.current;
        assert!(r.arsenal.chaingun_data(w).unwrap().0.allow_idle_barrel_spin);
        let t0 = r.arsenal.time_ms;
        let (shots, _) = r.run(1, t0 + 400, |_| (false, true));
        let spin = r.arsenal.states[w].barrel.spin;
        println!("gatling level {level}: spin after 400 ticks {spin}");
        assert!(shots.is_empty());
        assert!((spin - 400.0 * 0.001 / (up * 0.001)).abs() < 0.01, "spin {spin} vs spin-up {up}");
    }
}
