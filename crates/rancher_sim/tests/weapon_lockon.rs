//! RL lock-on mod (user's install; skipped without one): the targeting slots (weapons::targeting) against a
//! target in front of the player, through the arsenal directly and through the idHands driver on the real
//! fp_hands anim web (gamedata/re/MODS.md section 8b).

use std::sync::Arc;

use glam::Vec3;
use idres::animweb::AnimWeb;
use rancher_sim::animweb::{AnimData, AnimWebRuntime};
use rancher_sim::install;
use rancher_sim::weapons::hands::{Hands, HandsInput};
use rancher_sim::weapons::targeting::{aim_points, LockTarget, TargetWorld};
use rancher_sim::weapons::{load_arsenal, Arsenal, ChargeState, ModLoadout, Shot, TargetEvent, TargetState, WeaponEvent, WeaponInput};

const EYE: Vec3 = Vec3::new(0.0, 0.0, 64.0);

/// A Possessed-sized target `dist` units ahead along +x (yaw `side` units to the left).
fn target(id: u32, dist: f32, side: f32) -> LockTarget {
    let origin = Vec3::new(dist, side, 0.0);
    let bounds = (origin + Vec3::new(-16.0, -16.0, 0.0), origin + Vec3::new(16.0, 16.0, 86.0));
    let joints = [(2, origin + Vec3::new(0.0, 0.0, 60.0)), (3, origin + Vec3::new(0.0, 0.0, 50.0)), (4, origin + Vec3::new(0.0, 0.0, 40.0))];
    LockTarget { id, origin, aim_points: aim_points(&joints, origin + Vec3::new(0.0, 0.0, 76.0), origin, bounds), bounds, eligible: true, origin_visible: true, center_visible: true }
}

fn world(targets: Vec<LockTarget>) -> TargetWorld {
    TargetWorld { eye: EYE, forward: Vec3::X, targets }
}

fn arsenal_with(family_name: &str, level: u8) -> Option<(Arsenal, usize)> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).ok()?;
    let defs = load_arsenal(&inst.decls);
    let mut a = Arsenal::new(defs.clone());
    let w = defs.iter().position(|d| d.decl.ends_with("/rocket_launcher")).expect("rocket launcher");
    a.current = w;
    let m = defs[w].mods.clone().unwrap();
    let fam = m.families.iter().position(|f| f.name().contains(family_name)).expect("family");
    let mut l = ModLoadout::unlock_all(&m, level);
    l.active = Some(fam);
    a.set_loadout(w, l);
    Some((a, w))
}

#[test]
fn lock_mod_decl_data() {
    let Some((a, w)) = arsenal_with("lock", 0) else { return };
    let d = a.mode_def(w, 1).expect("lock-on decl");
    assert!(d.decl.ends_with("rocket_launcher_lock_mod"), "{}", d.decl);
    let l = &d.target_lock_normal;
    assert!(l.can_lock && l.lose_lock_on_fire && l.require_lock_to_fire && !l.automatically_maintain_lock);
    assert_eq!((l.lock_fov, l.lock_time_sec, l.lock_timeout_sec, l.clear_after_num_shots), (15.0, 0.8, 2.0, 3));
    assert_eq!((l.next_target_timeout_sec, l.max_targets, l.unlock_time_sec), (0.6, 1, 999999.0));
    assert!(l.players_block_line_of_sight, "ctor default kept");
    assert!(d.force_fire_mode_when_locked && d.charge.can_only_charge_when_targeting);
    assert_eq!(d.bursts[0].fake_burst_count, 3);
    // The rocket entity seeks: seekParms from projectile_ent/zion/player/sp/rocket_launcher_lockon.
    let s = &d.projectile.seek;
    assert!(s.can_seek && s.look_for_target);
    assert_eq!((s.angular_accel, s.min_angular_accel, s.max_angular_vel, s.seek_cone_degs), (2300.0, 1700.0, 900.0, 5.0));
    assert_eq!((s.delay_min_ms, s.delay_max_ms, s.delay_target_dist, s.delay_min_dist, s.explode_range), (100000, 100000, 750.0, 500.0, 0.0));
    assert_eq!((d.projectile.start_speed, d.projectile.min_start_speed, d.projectile.acceleration, d.projectile.min_acceleration), (1300.0, 1000.0, 1000.0, 500.0));
    // The base RL decl cannot lock.
    assert!(!a.defs[w].target_lock_normal.can_lock);
}

#[test]
fn acquires_then_locks_after_lock_time() {
    let Some((mut a, w)) = arsenal_with("lock", 0) else { return };
    a.target_world = world(vec![target(7, 600.0, 0.0)]);
    a.time_ms = 10_000;
    a.set_fire_mode(w, 1);
    let mut events = Vec::new();
    let mut locked_at = None;
    let start = a.time_ms;
    for _ in 0..80 {
        a.time_ms += 16;
        a.think_targeting(w);
        events.append(&mut a.target_events);
        if locked_at.is_none() && a.targeting[w].slots[0].state == TargetState::Locked {
            locked_at = Some(a.time_ms);
        }
    }
    // The slot turns on when the mode becomes a locking mode, starts acquiring the next frame.
    let s = &a.targeting[w].slots[0];
    assert_eq!(s.candidate, Some(7));
    assert_eq!(s.enemy, Some(7));
    assert_eq!(s.lock_percent, 1.0);
    // lockTimeSec 0.8 -> (int)(960 * 0.8) = 768 ticks after targetStartTime.
    let lt = locked_at.expect("locked");
    assert!(lt - s.target_start_time >= 768 && lt - s.target_start_time < 768 + 16, "{lt} {}", s.target_start_time);
    assert!(s.target_start_time - start <= 32);
    // Rockets in the pool: clearAfterNumShots = min(ammo / 1, 3).
    assert_eq!(s.clear_after_num_shots, 3);
    // forceFireModeWhenLocked pins mode 1.
    assert_eq!(a.mstate[w].override_fire_mode, 1);
    let sounds: Vec<String> = events.iter().filter_map(|e| if let TargetEvent::Sound(s) = e { Some(s.clone()) } else { None }).collect();
    assert_eq!(sounds, ["play_wpn_rpg_sp_lock_acquiring", "play_wpn_rpg_sp_lock_acquired"]);
    // CanCharge needs slot 0's target.
    assert!(a.lock_candidate(w).is_some());
}

#[test]
fn no_target_no_charge_and_out_of_fov_target_is_ignored() {
    let Some((mut a, w)) = arsenal_with("lock", 0) else { return };
    // 60 units to the side at 100 units: ~31 degrees off, outside lockFOV / 2 = 7.5.
    a.target_world = world(vec![target(3, 100.0, 60.0)]);
    a.time_ms = 10_000;
    a.set_fire_mode(w, 1);
    for _ in 0..40 {
        a.time_ms += 16;
        a.think_targeting(w);
    }
    assert_eq!(a.targeting[w].slots[0].state, TargetState::None);
    assert!(!a.can_charge(w, a.time_ms), "canOnlyChargeWhenTargeting without a target");
}

#[test]
fn best_target_prefers_the_target_under_the_crosshair() {
    let Some((a, w)) = arsenal_with("lock", 0) else { return };
    // Both inside the cone; the farther one sits on the view line, the nearer one slightly off it.
    let far = target(1, 900.0, 0.0);
    let near = target(2, 400.0, 40.0);
    let wld = world(vec![far.clone(), near.clone()]);
    assert_eq!(a.best_lock_target(w, None, &wld.view()), Some(1));
    // The current target wins while it qualifies.
    assert_eq!(a.best_lock_target(w, Some(2), &wld.view()), Some(2));
}

// ---- through the hands ----

struct Rig {
    arsenal: Arsenal,
    hands: Hands,
    web: AnimWebRuntime,
}

fn rig(level: u8) -> Option<(Rig, usize)> {
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
    let w = defs.iter().position(|d| d.decl.ends_with("/rocket_launcher")).expect("rl");
    arsenal.current = w;
    let m = defs[w].mods.clone().unwrap();
    let fam = m.families.iter().position(|f| f.name().contains("lock")).expect("lock family");
    let mut l = ModLoadout::unlock_all(&m, level);
    l.active = Some(fam);
    arsenal.set_loadout(w, l);
    let mut web = AnimWebRuntime::new(web, data);
    let mut hands = Hands::new(defs.len());
    hands.start(&mut arsenal, &mut web);
    let mut r = Rig { arsenal, hands, web };
    r.run(16, 1500, |_| (false, false));
    Some((r, w))
}

impl Rig {
    fn run(&mut self, ms: i32, end: i32, f: impl Fn(i32) -> (bool, bool)) -> Vec<Shot> {
        let mut shots = Vec::new();
        while self.arsenal.time_ms + ms <= end {
            let t = self.arsenal.time_ms + ms;
            let (a, alt) = f(t);
            let inp = HandsInput { weapon: WeaponInput { trigger: a, altfire: alt, ..Default::default() }, altfire: alt, ..Default::default() };
            for e in self.hands.tick(&mut self.arsenal, &mut self.web, ms, &inp) {
                if let WeaponEvent::Fired(s) = e {
                    shots.push(s);
                }
            }
        }
        shots
    }
}

#[test]
fn lock_on_charges_on_a_target_and_fires_three_seeking_rockets() {
    let Some((mut r, w)) = rig(0) else { return };
    r.arsenal.target_world = world(vec![target(5, 700.0, 0.0)]);
    let t0 = r.arsenal.time_ms;
    // Hold altfire; press attack once the lock and the charge are complete.
    let shots = r.run(16, t0 + 3000, |t| (t > t0 + 1400 && t < t0 + 1500, t < t0 + 2600));
    let ch = &r.arsenal.mstate[w].charge;
    assert!(!shots.is_empty(), "the lock-on fired (charge {:?})", ch.state);
    assert!(shots.iter().all(|s| s.mode == 1 && s.def.decl.ends_with("rocket_launcher_lock_mod")));
    assert!(shots.iter().all(|s| s.target == Some(5)), "{:?}", shots.iter().map(|s| s.target).collect::<Vec<_>>());
    assert_eq!(shots.len(), 3, "fakeBurstCount 3: {:?}", shots.iter().map(|s| s.time_ms).collect::<Vec<_>>());
    // The third shot clears the lock (loseLockOnFire) and targeting waits lockTimeoutSec 2 s.
    let s0 = &r.arsenal.targeting[w].slots[0];
    let last = shots.last().unwrap().time_ms;
    assert_eq!(s0.can_target_time, last + 1920);
    let _ = ChargeState::None;
}

#[test]
fn without_a_target_the_lock_on_neither_charges_nor_fires() {
    let Some((mut r, w)) = rig(0) else { return };
    let t0 = r.arsenal.time_ms;
    let shots = r.run(16, t0 + 2500, |t| (t > t0 + 1400 && t < t0 + 1500, t < t0 + 2400));
    assert!(shots.is_empty());
    assert_eq!(r.arsenal.mstate[w].charge.percent, 0.0);
}

#[test]
fn releasing_altfire_drops_the_lock() {
    let Some((mut r, w)) = rig(0) else { return };
    r.arsenal.target_world = world(vec![target(5, 700.0, 0.0)]);
    let t0 = r.arsenal.time_ms;
    r.run(16, t0 + 1300, |_| (false, true));
    assert_eq!(r.arsenal.targeting[w].slots[0].state, TargetState::Locked);
    r.arsenal.target_events.clear();
    // AltFireReleased (0x140f1f590): lock-on lock data does not automaticallyMaintainLock.
    r.run(16, t0 + 1400, |_| (false, false));
    assert_eq!(r.arsenal.targeting[w].slots[0].state, TargetState::None);
    assert_eq!(r.arsenal.mstate[w].override_fire_mode, -1);
    let sounds: Vec<String> = r.arsenal.target_events.iter().filter_map(|e| if let TargetEvent::Sound(s) = e { Some(s.clone()) } else { None }).collect();
    assert!(sounds.is_empty(), "the release clears without the slot's sound pass: {sounds:?}");
}

// ---- remote detonate ----

#[test]
fn detonate_mod_trigger_and_list() {
    let Some((mut a, w)) = arsenal_with("detonate", 0) else { return };
    // SECONDARY_TRIGGER_MODE SECONDARY_PRESS lives in mode 1's slot although there is no mode-1 decl.
    assert!(a.mode_def(w, 1).is_none());
    assert_eq!(a.secondary_trigger_mode(w), 3);
    let d = a.mode_def(w, 0).unwrap();
    assert!(d.decl.ends_with("rocket_launcher_detonate"), "{}", d.decl);
    assert!(d.projectile.can_detonate_with_alt_trigger);
    assert!(a.detonates_projectiles(w));
    a.time_ms = 5000;
    // Nothing in the air: no detonation.
    assert!(!a.can_detonate(w, a.time_ms));
    a.add_launched(w, &d, 11, 5000);
    a.add_launched(w, &d, 12, 5010);
    a.time_ms = 5020;
    assert!(a.can_detonate(w, a.time_ms));
    let ev = a.alt_fire_pressed(w);
    match &ev[..] {
        [rancher_sim::weapons::detonate::DetonateEvent::Explode { projectiles, .. }] => assert_eq!(projectiles, &vec![12, 11]),
        other => panic!("{other:?}"),
    }
    assert!(a.launched[w].list.is_empty());
    // An exploded rocket leaves the list.
    a.add_launched(w, &d, 13, 5030);
    a.launched_state(13, rancher_sim::weapons::detonate::PROJECTILE_EXPLODED);
    assert!(a.launched[w].list.is_empty());
}

#[test]
fn lock_on_rockets_are_not_detonated_by_altfire() {
    let Some((mut a, w)) = arsenal_with("lock", 0) else { return };
    let d = a.mode_def(w, 1).unwrap();
    a.add_launched(w, &d, 1, 100);
    a.time_ms = 2000;
    assert!(!a.detonates_projectiles(w));
    assert!(a.alt_fire_pressed(w).is_empty());
    assert_eq!(a.launched[w].list.len(), 1);
}

#[test]
fn detonate_through_the_hands_on_alt_press() {
    let Some(doom) = idres::find_install() else { return };
    let Ok(inst) = install::load(&doom) else { return };
    let defs = load_arsenal(&inst.decls);
    let c = inst.decls.container_arc();
    let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands.decl").unwrap()).into_owned();
    let web = Arc::new(AnimWeb::parse(&text).unwrap());
    let subs: Vec<String> = defs.iter().map(|d| d.hands.subweb.clone()).collect();
    let sub_refs: Vec<&str> = subs.iter().map(String::as_str).collect();
    let data = Arc::new(AnimData::load(&c, &web, &sub_refs));
    let mut arsenal = Arsenal::new(defs.clone());
    let w = defs.iter().position(|d| d.decl.ends_with("/rocket_launcher")).unwrap();
    arsenal.current = w;
    let m = defs[w].mods.clone().unwrap();
    let mut l = ModLoadout::unlock_all(&m, 0);
    l.active = m.families.iter().position(|f| f.name().contains("detonate"));
    arsenal.set_loadout(w, l);
    let mut web = AnimWebRuntime::new(web, data);
    let mut hands = Hands::new(defs.len());
    hands.start(&mut arsenal, &mut web);
    let mut r = Rig { arsenal, hands, web };
    r.run(16, 1500, |_| (false, false));
    let t0 = r.arsenal.time_ms;
    let mut next_id = 0;
    let mut detonated = Vec::new();
    let mut fired = Vec::new();
    while r.arsenal.time_ms + 16 <= t0 + 1200 {
        let t = r.arsenal.time_ms + 16;
        let (a, alt) = (t < t0 + 100, t > t0 + 600 && t < t0 + 700);
        let inp = HandsInput { weapon: WeaponInput { trigger: a, altfire: alt, ..Default::default() }, altfire: alt, ..Default::default() };
        for e in r.hands.tick(&mut r.arsenal, &mut r.web, 16, &inp) {
            match e {
                // The game side's FinishFire append (combat.rs does this for real projectiles).
                WeaponEvent::Fired(s) => {
                    next_id += 1;
                    r.arsenal.add_launched(s.weapon, &s.def, next_id, s.time_ms);
                    fired.push(s.time_ms);
                }
                WeaponEvent::Detonate { projectiles, .. } => detonated.push((r.arsenal.time_ms, projectiles)),
                _ => {}
            }
        }
    }
    assert_eq!(fired.len(), 1, "one rocket");
    assert_eq!(detonated.len(), 1, "{detonated:?}");
    let (t, ids) = &detonated[0];
    assert_eq!(ids, &vec![1]);
    assert!(*t > t0 + 600 && *t <= t0 + 620, "on the alt press frame: {t}");
}

#[test]
fn mastery_locks_two_targets_and_alternates_the_rockets() {
    // Level 4: decrease_lock_time (TARGET_LOCK_TIME_SEC 0.4), faster_recovery (TARGET_RECOVERY_SEC 1.25), mastery
    // TARGET_MAX_TARGETS 3.
    let Some((mut r, w)) = rig(4) else { return };
    // The mastery's TARGET_MAX_TARGETS sits in mode 1's slot (0x140f135c0 reads the current mode's).
    assert_eq!(r.arsenal.applied[w].modes[1].target_max_targets, 3);
    assert_eq!(r.arsenal.max_targets(w), 1, "mode 0: the base decl's lock data");
    // Two targets inside lockFOV / 2 (7.5 degrees): one on the view line, one 3 degrees off.
    let a = target(5, 700.0, 0.0);
    let b = target(6, 700.0, 700.0 * 3f32.to_radians().tan());
    r.arsenal.target_world = world(vec![a, b]);
    let t0 = r.arsenal.time_ms;
    let shots = r.run(16, t0 + 3000, |t| (t > t0 + 1800 && t < t0 + 1900, t < t0 + 2600));
    let targets: Vec<Option<u32>> = shots.iter().map(|s| s.target).collect();
    assert_eq!(targets, [Some(5), Some(6), Some(5)], "slot 0 locks the view-line target first, slot 1 the other");
    // faster_recovery: the lock comes back after 1.25 s.
    let last = shots.last().unwrap().time_ms;
    assert_eq!(r.arsenal.targeting[w].slots[0].can_target_time, last + 1200);
}
