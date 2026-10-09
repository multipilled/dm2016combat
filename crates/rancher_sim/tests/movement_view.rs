//! Eye-height springs and the adaptive frame clock. Player tests need the install (skipped without one).

use rancher_sim::Vec3;
use rancher_sim::cmd::{AdaptiveTick, UserCmd, button};
use rancher_sim::collision::{CLIP_EPSILON, Hull, World};
use rancher_sim::install;
use rancher_sim::player::{Player, Spring};

#[test]
fn spring_is_critically_damped_with_negative_damping() {
    let mut s = Spring::default();
    s.set_k(350.0, -1.0);
    assert!((s.damping - 2.0 * 350f32.sqrt()).abs() < 1e-4);
    s.pos = 41.0;
    let mut prev = s.pos;
    for _ in 0..200 {
        s.update(0.016);
        assert!(s.pos <= prev + 1e-6 && s.pos >= -1e-3, "overshoot {}", s.pos);
        prev = s.pos;
    }
    assert!(s.pos.abs() < 1e-3);
}

#[test]
fn spring_substeps_at_most_8_5_ms() {
    let mut a = Spring::default();
    a.set_k(350.0, -1.0);
    a.pos = 10.0;
    let mut b = a;
    a.update(0.016);
    b.update(0.0085);
    b.update(0.0075);
    assert_eq!(a, b);
}

#[test]
fn spring_stiffness_is_capped() {
    let mut s = Spring::default();
    s.set_k(1.0e6, 3.0);
    assert_eq!(s.k, 10000.0);
    assert_eq!(s.damping, 3.0);
}

#[test]
fn adaptive_tick_runs_slightly_faster_than_the_display() {
    // 60 Hz display: the game picks 62-63 Hz and whole-msec frames whose average is 0.96 of the display frame.
    let mut t = AdaptiveTick::new(30, 200);
    let n = 600;
    let mut total = 0i64;
    for _ in 0..n {
        let msec = t.next(16_667);
        assert!((15..=17).contains(&msec), "msec {msec}");
        assert!((62..=63).contains(&t.hz), "hz {}", t.hz);
        total += msec as i64;
    }
    let expect = n as f64 * 16.667 * 0.96;
    assert!((total as f64 - expect).abs() < 20.0, "{total} vs {expect}");
}

#[test]
fn adaptive_tick_clamps_to_min_and_max_hz() {
    let mut t = AdaptiveTick::new(30, 200);
    // A 100 ms hitch counts as 1e6/30 us.
    let m = t.next(100_000);
    assert_eq!(t.hz, 32);
    assert_eq!(m, 31);
    let mut t = AdaptiveTick::new(30, 200);
    // 1000 fps counts as 1e6/200 us.
    t.next(1_000);
    assert_eq!(t.hz, 209);
}

#[test]
fn set_hz_carries_the_fraction() {
    let mut t = AdaptiveTick::new(30, 200);
    let ms: i32 = (0..60).map(|_| t.set_hz(60)).sum();
    assert!((999..=1000).contains(&ms), "{ms}");
}

fn player_in(world: &World, at: Vec3) -> Option<Player> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    let mut p = Player::new(inst.movement, at);
    for _ in 0..60 {
        p.think(world, UserCmd::default(), 16);
    }
    Some(p)
}

fn floor() -> Hull {
    Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0))
}

#[test]
fn crouching_lowers_the_eye_smoothly() {
    let mut w = World::default();
    w.add(floor());
    let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, 1.0)) else { return };
    let (normal, crouch) = (p.cfg.normal_view_height, p.cfg.crouch_view_height);
    if !(p.cfg.use_step_up_springs && p.cfg.doom4_bob_cycle && p.cfg.hands_bob_cycle) {
        return;
    }
    assert!((p.eye_height() - normal).abs() < 1e-3);
    p.think(&w, UserCmd { up: -127, buttons: button::CROUCH, ..Default::default() }, 16);
    assert!(p.physics.ducked());
    let first = p.eye_height();
    // One 16 ms frame of a K=350 critically damped spring covers only part of the 41-unit drop.
    assert!(first < normal && first > (normal + crouch) * 0.5, "eye {first}");
    let mut prev = first;
    for _ in 0..60 {
        p.think(&w, UserCmd::default(), 16);
        let e = p.eye_height();
        assert!(e <= prev + 1e-4 && e >= crouch - 1e-3, "eye {e}");
        prev = e;
    }
    assert!((prev - crouch).abs() < 0.05, "eye {prev}");
}

#[test]
fn stepping_up_lags_the_eye() {
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::cuboid(Vec3::new(64.0, -256.0, 0.0), Vec3::new(1024.0, 256.0, 16.0)));
    let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, 1.0)) else { return };
    if !(p.cfg.use_step_up_springs && p.cfg.doom4_bob_cycle && p.cfg.hands_bob_cycle) {
        return;
    }
    let normal = p.cfg.normal_view_height;
    let fwd = UserCmd { forward: 127, ..Default::default() };
    let mut stepped = None;
    for i in 0..60 {
        p.think(&w, fwd, 16);
        if stepped.is_none() && p.physics.origin.z > 8.0 {
            stepped = Some(i);
            let eye_z = p.physics.origin.z + p.eye_height();
            // The origin jumped 16; the eye starts most of that below.
            assert!(eye_z < CLIP_EPSILON + normal + 8.0, "eye z {eye_z}");
            assert!(p.eye_height() < normal - 8.0);
        }
    }
    assert!(stepped.is_some());
    assert!((p.eye_height() - normal).abs() < 0.05, "eye {}", p.eye_height());
}
