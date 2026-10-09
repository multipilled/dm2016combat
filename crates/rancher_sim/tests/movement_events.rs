//! Jump and landing events (idPlayer after Move: jump callback 0x140e2fbe0, landing test 0x140e33110,
//! airborne test 0x140d63260). Needs the install; skipped without one.

use rancher_sim::Vec3;
use rancher_sim::cmd::UserCmd;
use rancher_sim::collision::{Hull, World};
use rancher_sim::install;
use rancher_sim::player::{LandSize, LandSound, Landing, Player};

fn floor() -> World {
    let mut w = World::default();
    w.add(Hull::cuboid(Vec3::new(-8192.0, -8192.0, -64.0), Vec3::new(8192.0, 8192.0, 0.0)));
    w
}

fn player_at(z: f32) -> Option<Player> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    Some(Player::new(inst.movement, Vec3::new(0.0, 0.0, z)))
}

/// Drops the player from about `height` above the floor; returns the landing and whether the
/// large-fall and fatal sounds fired on the way down and the large stop on landing.
///
/// A fall whose last frame ends within the contact distance without the slide touching the floor has
/// its vertical speed removed by SlideMove that frame, and the ground is seen a frame later; the
/// landing test then reads a zero fall speed and reports no reaction (as in the game, which saves the
/// velocity after each Think, 0x140e30130). Starts are nudged until the slide hits the floor directly.
fn drop_from(height: f32) -> Option<(Landing, bool, bool, bool)> {
    for nudge in 0..8 {
        let r = drop_once(height + nudge as f32 * 0.37)?;
        if r.0.impact_speed > 0.0 {
            return Some(r);
        }
        assert_eq!((r.0.size, r.0.impact_speed), (LandSize::None, 0.0));
    }
    panic!("no direct landing from {height}");
}

fn drop_once(height: f32) -> Option<(Landing, bool, bool, bool)> {
    let w = floor();
    let mut p = player_at(height)?;
    let (mut large_start, mut fatal, mut large_stop) = (false, false, false);
    for _ in 0..2000 {
        p.think(&w, UserCmd::default(), 16);
        large_start |= p.events.falling_large_start;
        fatal |= p.events.falling_fatal;
        large_stop |= p.events.falling_large_stop;
        if let Some(l) = p.events.landed {
            return Some((l, large_start, fatal, large_stop));
        }
    }
    panic!("never landed from {height}");
}

#[test]
fn landing_classes_follow_player_falling() {
    let Some(p) = player_at(0.0) else { return };
    let f = p.cfg.falling;
    // Class defaults (props constructor 0x1406eb800) with the decl's override.
    assert_eq!((f.min_dist_for_sound, f.small_landing_reaction_distance), (43.0, 43.0));
    assert_eq!((f.medium_landing_reaction_distance, f.large_landing_reaction_distance, f.extra_large_landing_reaction_distance), (200.0, 400.0, 850.0));
    assert_eq!(f.fatal_damage_distance, 2048.0);
    assert_eq!(f.no_ground_kill_player_fall_dist, 1500.0, "player/default overrides it");

    // Fall height is apex − landing + 16; each reaction needs the fall speed too.
    let cases = [
        (100.0, LandSize::Small, LandSound::Normal),
        (300.0, LandSize::Medium, LandSound::Normal),
        (500.0, LandSize::Large, LandSound::Heavy),
        (1000.0, LandSize::ExtraLarge, LandSound::None),
        (2100.0, LandSize::Fatal, LandSound::Normal),
    ];
    for (h, size, sound) in cases {
        let (l, large_start, fatal, large_stop) = drop_from(h).unwrap();
        assert_eq!(l.size, size, "drop {h}: {l:?}");
        assert_eq!(l.sound, sound, "drop {h}: {l:?}");
        // The test only starts tracking after ten frames, by which time the drop has begun.
        assert!(l.fall_height <= h + 19.0 && l.fall_height > h - 4.0, "drop {h}: fall {}", l.fall_height);
        // The wind starts once the fall passes the large distance at speed, whatever the final class.
        let large_or_worse = matches!(size, LandSize::Large | LandSize::ExtraLarge | LandSize::Fatal);
        assert_eq!(large_start, large_or_worse, "drop {h}: wind start");
        assert_eq!(large_stop, large_start, "drop {h}: wind stop");
        assert_eq!(fatal, size == LandSize::Fatal, "drop {h}: fatal sting");
    }
    assert_eq!(drop_from(100.0).unwrap().0.hands_action(), Some(18));
    assert_eq!(drop_from(300.0).unwrap().0.hands_action(), Some(19));
    assert_eq!(drop_from(500.0).unwrap().0.hands_action(), Some(20));
}

#[test]
fn jump_and_double_jump_events() {
    let w = floor();
    let Some(mut p) = player_at(1.0) else { return };
    for _ in 0..30 {
        p.think(&w, UserCmd::default(), 16);
    }
    assert!(!p.falling(), "standing");
    let jump = UserCmd { up: 127, ..Default::default() };
    p.think(&w, jump, 16);
    assert!(p.events.jumped && !p.events.double_jumped);
    assert!(p.falling(), "airborne after take-off");
    // Release, then press again in the air: the double jump (jump boots) fires once.
    let (mut jumps, mut doubles) = (1, 0);
    let mut landing = None;
    for i in 0..200 {
        let c = if (10..14).contains(&i) { jump } else { UserCmd::default() };
        p.think(&w, c, 16);
        jumps += p.events.jumped as u32;
        doubles += p.events.double_jumped as u32;
        if let Some(l) = p.events.landed {
            landing = Some(l);
            assert!(p.falling(), "still falling on the landing frame (0x140d63260 lags a frame)");
            p.think(&w, UserCmd::default(), 16);
            assert!(!p.falling());
            break;
        }
    }
    assert_eq!((jumps, doubles), (1, 1));
    let l = landing.expect("never landed");
    assert!(l.fall_height > 100.0 && l.size != LandSize::None, "{l:?}");
}
