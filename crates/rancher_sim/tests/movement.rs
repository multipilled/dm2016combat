//! Movement checks against the user's own install. Skipped when no DOOM install is found.
//! Expected values are derived from the install's own tuning, never hard-coded.

use rancher_sim::Vec3;
use rancher_sim::cmd::{UserCmd, button};
use rancher_sim::collision::{Hull, World};
use rancher_sim::install;
use rancher_sim::player::Player;

fn setup() -> Option<(Player, World)> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    let mut world = World::default();
    world.add(Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0)));
    let mut p = Player::new(inst.movement, Vec3::new(0.0, 0.0, 0.5));
    // Settle onto the floor.
    for _ in 0..30 {
        p.think(&world, UserCmd::default(), 16);
    }
    Some((p, world))
}

fn hspeed(p: &Player) -> f32 {
    let v = p.physics.velocity;
    (v.x * v.x + v.y * v.y).sqrt()
}

#[test]
fn settles_on_floor() {
    let Some((p, _)) = setup() else { return };
    assert!(p.physics.walking, "player should be walking on the floor");
    assert!(p.physics.origin.z.abs() < 1.0, "z = {}", p.physics.origin.z);
}

#[test]
fn runs_at_run_speed() {
    let Some((mut p, w)) = setup() else { return };
    let run = p.cfg.run_speed;
    for _ in 0..120 {
        p.think(&w, UserCmd { forward: 127, ..Default::default() }, 16);
    }
    assert!((hspeed(&p) - run).abs() < 0.5, "speed {} vs pm_runspeed {run}", hspeed(&p));
}

#[test]
fn backpedal_uses_back_ratio() {
    let Some((mut p, w)) = setup() else { return };
    let expect = p.cfg.run_speed * p.cfg.back_speed_ratio;
    for _ in 0..120 {
        p.think(&w, UserCmd { forward: -127, ..Default::default() }, 16);
    }
    assert!((hspeed(&p) - expect).abs() < 0.5, "speed {} vs {expect}", hspeed(&p));
}

#[test]
fn walk_button_uses_walk_speed() {
    let Some((mut p, w)) = setup() else { return };
    let expect = p.cfg.walk_speed;
    for _ in 0..120 {
        p.think(&w, UserCmd { forward: 127, buttons: button::WALK, ..Default::default() }, 16);
    }
    assert!((hspeed(&p) - expect).abs() < 0.5, "speed {} vs pm_walkspeed {expect}", hspeed(&p));
}

#[test]
fn jump_apex_matches_jump_height() {
    let Some((mut p, w)) = setup() else { return };
    let h = p.cfg.jump_height;
    let z0 = p.physics.origin.z;
    let mut apex: f32 = 0.0;
    let mut cmd = UserCmd { up: 127, ..Default::default() };
    for i in 0..90 {
        p.think(&w, cmd, 16);
        apex = apex.max(p.physics.origin.z);
        if i == 0 {
            cmd.up = 0;
        }
    }
    // The slide moves by v·dt + ½g·dt² each frame (0x141957580), so the sampled arc is exact.
    let (v0, g, dt) = ((2.0 * p.cfg.gravity * h).sqrt(), p.cfg.gravity, 0.016);
    let expect = z0 + (1..60).map(|n| v0 * n as f32 * dt - 0.5 * g * (n as f32 * dt).powi(2)).fold(0.0, f32::max);
    assert!((apex - expect).abs() < 0.01, "apex {apex} vs {expect} (pm_jumpheight {h} from rest z {z0})");
    assert!(p.physics.walking, "should land again");
}

#[test]
fn double_jump_after_window() {
    let Some((mut p, w)) = setup() else { return };
    let offset = p.cfg.boots.unwrap().window_offset_ms;
    p.think(&w, UserCmd { up: 127, ..Default::default() }, 16);
    let mut t = 16;
    while t < offset + 32 {
        p.think(&w, UserCmd::default(), 16);
        t += 16;
    }
    let vz_before = p.physics.velocity.z;
    p.think(&w, UserCmd { up: 127, ..Default::default() }, 16);
    assert!(p.physics.jump_count >= 2, "double jump should fire after the window offset");
    assert!(p.physics.velocity.z > vz_before, "double jump should restore upward speed");
}

#[test]
fn crouch_slows_to_crouch_speed() {
    let Some((mut p, w)) = setup() else { return };
    let expect = p.cfg.crouch_speed;
    // Toggle crouch on (press once), then run.
    p.think(&w, UserCmd { up: -127, buttons: button::CROUCH, ..Default::default() }, 16);
    for _ in 0..120 {
        p.think(&w, UserCmd { forward: 127, ..Default::default() }, 16);
    }
    if p.cfg.crouch_toggle {
        assert!(p.physics.ducked());
        assert!((hspeed(&p) - expect).abs() < 0.5, "speed {} vs pm_crouchspeed {expect}", hspeed(&p));
    }
}

#[test]
fn arsenal_loads_from_install() {
    let Some(doom) = idres::find_install() else { return };
    let inst = install::load(&doom).unwrap();
    let defs = rancher_sim::weapons::load_arsenal(&inst.decls);
    for d in &defs {
        println!(
            "{:44} grp {:26} md6 {:50} int {:6} up {:.2} dn {:.2} ammo {:3}/{:3} proj hitscan={} n={} spd={} dmg {}-{} pb {} splash {:?}",
            d.decl, d.selection_group, d.hands_md6, d.firing_interval_ms, d.bringup_s, d.bringdown_s, d.ammo_start, d.ammo_max,
            d.projectile.hitscan, d.projectile.spawn_count, d.projectile.speed, d.projectile.damage.min, d.projectile.damage.max,
            d.projectile.damage.point_blank, d.projectile.splash.as_ref().map(|s| (s.max, s.radius))
        );
    }
    assert!(defs.len() >= 10, "loaded {} weapons", defs.len());
    let sg = defs.iter().find(|d| d.decl.ends_with("/shotgun")).unwrap();
    // Falloff checks derived from the decl's own breakpoints.
    let dmg = &sg.projectile.damage;
    assert_eq!(dmg.at_distance(0.0), dmg.point_blank);
    assert_eq!(dmg.at_distance(dmg.end_pb), dmg.max);
    assert_eq!(dmg.at_distance(dmg.end_falloff + 1.0), dmg.min);
}

#[test]
fn acceleration_is_the_frame_velocity_change() {
    // playerPState_t.acceleration: (velocity - previous state's velocity) / frame seconds, rewritten by
    // every velocity change of the move and last by SlideMove.
    let Some((mut p, w)) = setup() else { return };
    assert_eq!(p.physics.acceleration, Vec3::ZERO, "standing still");
    let mut peak: f32 = 0.0;
    for (i, msec) in [16, 17, 15, 16, 16, 17, 16, 15].into_iter().cycle().take(60).enumerate() {
        let before = p.physics.velocity;
        let cmd = UserCmd { forward: if i < 40 { 127 } else { 0 }, ..Default::default() };
        p.think(&w, cmd, msec);
        let expected = (p.physics.velocity - before) / (msec as f32 * 0.001);
        assert!((p.physics.acceleration - expected).length() < 1e-2, "frame {i}: {} vs {expected}", p.physics.acceleration);
        peak = peak.max(p.physics.acceleration.x.abs());
    }
    assert!(peak > 100.0, "never accelerated ({peak})");
    // SetLinearVelocity derives it from the previous state's velocity.
    p.think(&w, UserCmd::default(), 16);
    let prev = p.physics.prev_velocity;
    p.physics.set_linear_velocity(Vec3::new(160.0, 0.0, 0.0));
    let expected = (Vec3::new(160.0, 0.0, 0.0) - prev) / 0.016;
    assert!((p.physics.acceleration - expected).length() < 1e-1, "{} vs {expected}", p.physics.acceleration);
}
