//! Slide, step and contact behaviour against small brush worlds.
//! The collision-model tests need no install; the player tests use the install's tuning and are skipped
//! without one. Expected values come from the decoded constants (see gamedata/re/MOVEMENT.md).

use rancher_sim::Vec3;
use rancher_sim::cmd::UserCmd;
use rancher_sim::collision::{self, CLIP_EPSILON, ContactKind, Hull, World};
use rancher_sim::install;
use rancher_sim::physics::flags;
use rancher_sim::player::Player;

fn floor() -> Hull {
    Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0))
}

/// pm_playerCollisionStyle 1 with default_sp's pm_bboxwidth 32 and pm_normalheight 90.
fn player_shape() -> Hull {
    Hull::player_trace_model(1, Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 90.0), 0.0, 0.0)
}

// ---- collision model -------------------------------------------------------------------------

#[test]
fn trace_stops_clip_epsilon_short() {
    let mut w = World::default();
    w.add(floor());
    let s = player_shape();
    let tr = w.translate(&s, Vec3::new(0.0, 0.0, 10.0), Vec3::new(0.0, 0.0, -10.0));
    assert!(tr.fraction < 1.0 && tr.hit());
    assert!((tr.endpos.z - CLIP_EPSILON).abs() < 1e-4, "rest {}", tr.endpos.z);
    assert_eq!(tr.c.kind, ContactKind::TrmVertex);
    assert!((tr.c.normal - Vec3::Z).length() < 1e-6);
}

#[test]
fn trace_ends_within_epsilon_still_collides() {
    // An end point closer than the epsilon to the plane is still a hit (plane fraction d2 < 0.25).
    let mut w = World::default();
    w.add(floor());
    let s = player_shape();
    let tr = w.translate(&s, Vec3::new(0.0, 0.0, 10.0), Vec3::new(0.0, 0.0, 0.1));
    assert!(tr.fraction < 1.0);
    assert!((tr.endpos.z - CLIP_EPSILON).abs() < 1e-4);
}

#[test]
fn zero_length_trace_does_nothing() {
    let mut w = World::default();
    w.add(floor());
    let s = player_shape();
    let p = Vec3::new(0.0, 0.0, -5.0); // even embedded
    let tr = w.translate(&s, p, p);
    assert_eq!(tr.fraction, 1.0);
    assert_eq!(tr.endpos, p);
}

#[test]
fn flat_floor_gives_one_contact_per_bottom_vertex() {
    let mut w = World::default();
    w.add(floor());
    let s = player_shape();
    let c = w.contacts(&s, Vec3::new(0.0, 0.0, CLIP_EPSILON), Vec3::NEG_Z, collision::CONTACT_DISTANCE);
    assert_eq!(c.len(), 8, "{c:?}");
    assert!(c.iter().all(|c| c.kind == ContactKind::TrmVertex && (c.normal - Vec3::Z).length() < 1e-6));
    // Out of reach: 0.5 + epsilon below is the limit.
    assert!(w.contacts(&s, Vec3::new(0.0, 0.0, 0.76), Vec3::NEG_Z, collision::CONTACT_DISTANCE).is_empty());
    assert_eq!(w.contacts(&s, Vec3::new(0.0, 0.0, 0.74), Vec3::NEG_Z, collision::CONTACT_DISTANCE).len(), 8);
}

#[test]
fn edge_on_edge_hit_faces_against_motion() {
    // A wedge whose ridge runs along Y, approached by the cylinder's bottom edge from above.
    let mut w = World::default();
    w.add(Hull::from_points(&[
        Vec3::new(-32.0, -64.0, 0.0),
        Vec3::new(32.0, -64.0, 0.0),
        Vec3::new(0.0, -64.0, 32.0),
        Vec3::new(-32.0, 64.0, 0.0),
        Vec3::new(32.0, 64.0, 0.0),
        Vec3::new(0.0, 64.0, 32.0),
    ]));
    let s = player_shape();
    // Offset so no cylinder vertex sits exactly over the ridge: the cap's bottom edges meet it first.
    let tr = w.translate(&s, Vec3::new(3.0, 0.0, 64.0), Vec3::new(3.0, 0.0, 0.0));
    assert!(tr.hit());
    assert_eq!(tr.c.kind, ContactKind::Edge);
    assert!(tr.c.normal.z > 0.99, "{:?}", tr.c);
    // The bottom cap rests on the ridge, CLIP_EPSILON above it along the contact normal.
    assert!((tr.endpos.z - (32.0 + CLIP_EPSILON)).abs() < 0.01, "{}", tr.endpos.z);
}

#[test]
fn slide_runs_along_a_wall() {
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::cuboid(Vec3::new(64.0, -512.0, 0.0), Vec3::new(96.0, 512.0, 128.0)));
    let s = player_shape();
    let origin = Vec3::new(0.0, 0.0, CLIP_EPSILON);
    // 45° into the wall: the X part is clipped away, Y survives.
    let delta = Vec3::new(100.0, 100.0, 0.0);
    let r = collision::slide_move_contacts(&w, &s, origin, delta, Vec3::NEG_Z, 0.0, 0.0);
    // Overclip leaves a 0.1% push back off the wall.
    assert!((r.endpos.x - (64.0 - 16.0 - CLIP_EPSILON)).abs() < 0.1, "{:?}", r.endpos);
    assert!(r.endpos.y > 90.0, "{:?}", r.endpos);
    assert!(r.displacement.x.abs() < 0.2 && (r.displacement.y - 100.0).abs() < 0.2, "{:?}", r.displacement);
    assert_eq!(r.first.kind, ContactKind::TrmVertex);
}

#[test]
fn slide_stops_in_a_corner() {
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::cuboid(Vec3::new(64.0, -512.0, 0.0), Vec3::new(96.0, 512.0, 128.0)));
    w.add(Hull::cuboid(Vec3::new(-512.0, 64.0, 0.0), Vec3::new(64.0, 96.0, 128.0)));
    let s = player_shape();
    let r = collision::slide_move_contacts(&w, &s, Vec3::new(0.0, 0.0, CLIP_EPSILON), Vec3::new(100.0, 100.0, 0.0), Vec3::NEG_Z, 0.0, 0.0);
    assert!(r.endpos.x < 64.0 - 16.0 && r.endpos.y < 64.0 - 13.0, "{:?}", r.endpos);
    assert!(r.displacement.length() < 0.5, "{:?}", r.displacement);
}

#[test]
fn clip_against_floor_overclips() {
    let v = collision::clip_velocity(Vec3::new(10.0, 0.0, -100.0), Vec3::Z);
    assert!((v.z - 0.1).abs() < 1e-4, "{v}");
}

#[test]
fn gravity_is_a_midpoint_step() {
    // No geometry: position moves by v·dt + ½g·dt², displacement carries the full g·dt².
    let w = World::default();
    let s = player_shape();
    let (g, dt) = (1066.0f32, 0.016f32);
    let gvec = Vec3::new(0.0, 0.0, -g) * dt * dt + Vec3::NEG_Z;
    let r = collision::slide_move_contacts(&w, &s, Vec3::ZERO, Vec3::new(0.0, 0.0, 100.0 * dt), gvec, 0.0, 0.0);
    let expect = 100.0 * dt - 0.5 * g * dt * dt;
    assert!((r.endpos.z - expect).abs() < 1e-4, "{} vs {expect}", r.endpos.z);
    assert!((r.displacement.z - (100.0 * dt - g * dt * dt)).abs() < 1e-4);
}

#[test]
fn step_trace_climbs_a_step_and_refuses_a_steep_landing() {
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::cuboid(Vec3::new(32.0, -64.0, 0.0), Vec3::new(128.0, 64.0, 16.0)));
    let s = player_shape();
    let start = Vec3::new(0.0, 0.0, CLIP_EPSILON);
    let st = collision::step_trace(&w, &s, start, start + Vec3::new(40.0, 0.0, 0.0), Vec3::NEG_Z, 16.0, 32.0);
    assert_eq!(st.fraction, 1.0);
    assert!((st.endpos.z - (16.0 + CLIP_EPSILON)).abs() < 1e-3, "{:?}", st.endpos);
    assert!((st.step - 16.0).abs() < 1e-3);
    assert!((st.endpos.x - 40.0).abs() < 1e-3);

    // The same step topped by a 60° roof is refused: the down trace lands on a steep plane.
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::from_points(&[
        Vec3::new(32.0, -64.0, 0.0),
        Vec3::new(32.0, 64.0, 0.0),
        Vec3::new(128.0, -64.0, 0.0),
        Vec3::new(128.0, 64.0, 0.0),
        Vec3::new(32.0, -64.0, 8.0),
        Vec3::new(32.0, 64.0, 8.0),
        Vec3::new(128.0, -64.0, 8.0 + 96.0 * 3f32.sqrt()),
        Vec3::new(128.0, 64.0, 8.0 + 96.0 * 3f32.sqrt()),
    ]));
    let st = collision::step_trace(&w, &s, start, start + Vec3::new(40.0, 0.0, 0.0), Vec3::NEG_Z, 16.0, 32.0);
    assert_eq!(st.step, 0.0);
    assert!(st.fraction < 1.0 && st.endpos.z < 1.0, "{st:?}");
}

// ---- player ----------------------------------------------------------------------------------

fn player_in(world: &World, at: Vec3) -> Option<Player> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    let mut p = Player::new(inst.movement, at);
    for _ in 0..30 {
        p.think(world, UserCmd::default(), 16);
    }
    Some(p)
}

fn run(p: &mut Player, w: &World, frames: usize, cmd: UserCmd) -> u32 {
    let mut seen = 0;
    for _ in 0..frames {
        p.think(w, cmd, 16);
        seen |= p.physics.flags;
    }
    seen
}

const FORWARD: UserCmd = UserCmd { forward: 127, right: 0, up: 0, buttons: 0, angles: [0.0; 3] };

#[test]
fn rests_clip_epsilon_above_the_floor() {
    let mut w = World::default();
    w.add(floor());
    let Some(p) = player_in(&w, Vec3::new(0.0, 0.0, 8.0)) else { return };
    assert!(p.physics.walking);
    assert!((p.physics.origin.z - CLIP_EPSILON).abs() < 1e-3, "z {}", p.physics.origin.z);
    // One contact per bottom vertex of the trace model.
    let bottom = p.physics.normal_shape.verts.iter().filter(|v| v.z == 0.0).count();
    assert_eq!(p.physics.contacts.len(), bottom);
}

#[test]
fn walks_up_steps_up_to_the_step_size() {
    for (h, climbs) in [(8.0, true), (16.0, true), (17.0, false), (24.0, false)] {
        let mut w = World::default();
        w.add(floor());
        w.add(Hull::cuboid(Vec3::new(64.0, -256.0, 0.0), Vec3::new(512.0, 256.0, h)));
        let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, 1.0)) else { return };
        assert!(h != 16.0 || p.cfg.step_size == 16.0, "test assumes pm_stepsize 16");
        let seen = run(&mut p, &w, 40, FORWARD);
        let z = p.physics.origin.z;
        if climbs {
            assert!((z - (h + CLIP_EPSILON)).abs() < 1e-2, "h {h}: z {z}");
            assert!(p.physics.origin.x > 100.0);
            assert!(h <= 1.0 || seen & flags::STEPPED_UP != 0, "h {h}: no STEPPED_UP");
        } else {
            assert!(z < 1.0, "h {h}: z {z}");
            assert!(p.physics.origin.x < 64.0 - 16.0, "h {h}: x {}", p.physics.origin.x);
        }
        assert!(p.physics.walking, "h {h}: should stay grounded");
    }
}

#[test]
fn walks_down_steps_and_falls_off_ledges() {
    // Start on a platform and run off its edge.
    for (h, stays_grounded) in [(16.0, true), (48.0, false)] {
        let mut w = World::default();
        w.add(floor());
        w.add(Hull::cuboid(Vec3::new(-512.0, -256.0, 0.0), Vec3::new(64.0, 256.0, h)));
        let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, h + 1.0)) else { return };
        assert!(p.physics.walking);
        let mut seen = 0;
        let mut airborne = false;
        for _ in 0..40 {
            p.think(&w, FORWARD, 16);
            seen |= p.physics.flags;
            airborne |= !p.physics.walking;
        }
        assert!(p.physics.origin.x > 64.0 + 16.0);
        assert!((p.physics.origin.z - CLIP_EPSILON).abs() < 1e-2, "h {h}: z {}", p.physics.origin.z);
        if stays_grounded {
            assert!(seen & flags::STEPPED_DOWN != 0, "h {h}: no STEPPED_DOWN");
            assert!(!airborne, "h {h}: left the ground on a step");
        } else {
            assert!(airborne, "h {h}: never fell");
        }
    }
}

fn ramp(rise: f32) -> Hull {
    Hull::from_points(&[
        Vec3::new(64.0, -256.0, 0.0),
        Vec3::new(64.0, 256.0, 0.0),
        Vec3::new(320.0, -256.0, 0.0),
        Vec3::new(320.0, 256.0, 0.0),
        Vec3::new(320.0, -256.0, rise),
        Vec3::new(320.0, 256.0, rise),
    ])
}

#[test]
fn climbs_walkable_ramps_only() {
    // Normal Z of a ramp rising `rise` over 256: 128 → 0.89, 224 → 0.75 (both walkable), 288 → 0.66 (steep).
    for (rise, climbs) in [(128.0, true), (224.0, true), (288.0, false)] {
        let mut w = World::default();
        w.add(floor());
        w.add(ramp(rise));
        let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, 1.0)) else { return };
        let mut top: f32 = 0.0;
        for _ in 0..60 {
            p.think(&w, FORWARD, 16);
            top = top.max(p.physics.origin.z);
        }
        if climbs {
            assert!(top > rise * 0.9, "rise {rise}: reached {top}");
        } else {
            assert!(top < rise * 0.3, "rise {rise}: reached {top}");
        }
    }
}

#[test]
fn runs_along_a_wall_at_an_angle() {
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::cuboid(Vec3::new(64.0, -2048.0, 0.0), Vec3::new(96.0, 2048.0, 256.0)));
    let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, 1.0)) else { return };
    p.view_angles[1] = 30.0; // 30° from the wall normal's opposite → 60° into the wall
    run(&mut p, &w, 60, FORWARD);
    let ph = &p.physics;
    assert!((ph.origin.x - (64.0 - 16.0 - CLIP_EPSILON)).abs() < 0.05, "x {}", ph.origin.x);
    assert!(ph.velocity.x.abs() < 1.0, "v {}", ph.velocity);
    assert!(ph.velocity.y > 100.0, "v {}", ph.velocity);
}

#[test]
fn stands_up_only_with_headroom() {
    let mut w = World::default();
    w.add(floor());
    // Ceiling 80 above the floor ahead: crouched (60) fits under it, standing (90) does not.
    w.add(Hull::cuboid(Vec3::new(64.0, -256.0, 80.0), Vec3::new(512.0, 256.0, 96.0)));
    let Some(mut p) = player_in(&w, Vec3::new(0.0, 0.0, 1.0)) else { return };
    if !p.cfg.crouch_toggle {
        return;
    }
    let crouch = UserCmd { up: -127, buttons: rancher_sim::cmd::button::CROUCH, ..Default::default() };
    p.think(&w, crouch, 16);
    run(&mut p, &w, 50, FORWARD);
    assert!(p.physics.ducked());
    assert!(p.physics.origin.x > 64.0 + 16.0, "x {}", p.physics.origin.x);
    // Toggle again under the ceiling: the crouch model swept up 30 units hits it.
    p.think(&w, crouch, 16);
    run(&mut p, &w, 5, UserCmd::default());
    assert!(p.physics.ducked(), "stood up under an 80-unit ceiling");
    // Back out and toggle: stands.
    run(&mut p, &w, 60, UserCmd { forward: -127, ..Default::default() });
    assert!(p.physics.origin.x < 64.0 - 16.0, "x {}", p.physics.origin.x);
    p.think(&w, crouch, 16);
    run(&mut p, &w, 5, UserCmd::default());
    assert!(!p.physics.ducked(), "could not stand in the open");
}

// ---- brush seams -------------------------------------------------------------------------------

#[test]
fn touching_brushes_lose_their_shared_faces() {
    // Two boxes pressed together on x = 0 collide like one 128 x 128 x 64 box.
    let mut w = World::default();
    w.add(Hull::cuboid(Vec3::new(-64.0, -64.0, 0.0), Vec3::new(0.0, 64.0, 64.0)));
    w.add(Hull::cuboid(Vec3::new(0.0, -64.0, 0.0), Vec3::new(64.0, 64.0, 64.0)));
    let mut exposed = 0;
    for m in w.collision_models() {
        assert_eq!(m.polys.len(), 5, "the face on x = 0 is gone");
        for e in &m.edges {
            let (a, b) = (m.verts[e.v[0]], m.verts[e.v[1]]);
            let seam = a.x.abs() < 1e-3 && b.x.abs() < 1e-3;
            assert_eq!(e.internal, seam, "edge {a} {b}");
            exposed += !e.internal as usize;
        }
    }
    // The merged box's 12 edges, the four along x split in two.
    assert_eq!(exposed, 16);
    // A box standing on the floor: the floor keeps only the area around it and the edges where the
    // two meet are concave, hence internal.
    let mut w = World::default();
    w.add(floor());
    w.add(Hull::cuboid(Vec3::new(64.0, -64.0, 0.0), Vec3::new(128.0, 64.0, 32.0)));
    let cm = w.collision_models();
    for m in cm {
        for e in &m.edges {
            let (a, b) = (m.verts[e.v[0]], m.verts[e.v[1]]);
            if a.z.abs() < 1e-3 && b.z.abs() < 1e-3 && a.x >= 64.0 - 1e-3 && a.x <= 128.0 + 1e-3 && b.x >= 64.0 - 1e-3 && b.x <= 128.0 + 1e-3 && a.y.abs() <= 64.0 + 1e-3 && b.y.abs() <= 64.0 + 1e-3 {
                assert!(e.internal, "base edge {a} {b}");
            }
        }
    }
    assert!(cm[1].polys.iter().all(|p| p.normal.z > -0.5), "the box's bottom face is gone");
}

/// The same input in two worlds; returns both final origins.
fn paths(a: &World, b: &World, start: Vec3, yaw: f32, frames: usize) -> Option<(Vec3, Vec3)> {
    let mut out = [Vec3::ZERO; 2];
    for (w, o) in [a, b].into_iter().zip(out.iter_mut()) {
        let mut p = player_in(w, start)?;
        p.view_angles[1] = yaw;
        run(&mut p, w, frames, FORWARD);
        *o = p.physics.origin;
    }
    Some((out[0], out[1]))
}

#[test]
fn brush_seams_do_not_snag() {
    // A wall of two brushes (seam at y = 0) over a floor of two tiles (seam at x = 0), against the
    // same shapes as single brushes: running along the wall across both seams follows the same path.
    let mut one = World::default();
    one.add(floor());
    one.add(Hull::cuboid(Vec3::new(64.0, -2048.0, 0.0), Vec3::new(96.0, 2048.0, 256.0)));
    let mut split = World::default();
    split.add(Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(0.0, 4096.0, 0.0)));
    split.add(Hull::cuboid(Vec3::new(0.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0)));
    split.add(Hull::cuboid(Vec3::new(64.0, -2048.0, 0.0), Vec3::new(96.0, 0.0, 256.0)));
    split.add(Hull::cuboid(Vec3::new(64.0, 0.0, 0.0), Vec3::new(96.0, 2048.0, 256.0)));
    let Some((a, b)) = paths(&one, &split, Vec3::new(-64.0, -400.0, 1.0), 60.0, 120) else { return };
    assert!(a.y > 200.0, "never crossed the seam: {a}");
    assert!((a - b).length() < 1e-3, "one brush {a}, two brushes {b}");
    // A ramp ending flush against a platform (the firing range's ramps) against one convex brush.
    let pts = [
        Vec3::new(64.0, -256.0, 0.0),
        Vec3::new(64.0, 256.0, 0.0),
        Vec3::new(320.0, -256.0, 0.0),
        Vec3::new(320.0, 256.0, 0.0),
        Vec3::new(320.0, -256.0, 128.0),
        Vec3::new(320.0, 256.0, 128.0),
    ];
    let mut one = World::default();
    one.add(floor());
    let mut all = pts.to_vec();
    all.extend([Vec3::new(384.0, -256.0, 0.0), Vec3::new(384.0, 256.0, 0.0), Vec3::new(384.0, -256.0, 128.0), Vec3::new(384.0, 256.0, 128.0)]);
    one.add(Hull::from_points(&all));
    let mut split = World::default();
    split.add(floor());
    split.add(Hull::from_points(&pts));
    split.add(Hull::cuboid(Vec3::new(320.0, -256.0, 0.0), Vec3::new(384.0, 256.0, 128.0)));
    let (a, b) = paths(&one, &split, Vec3::new(0.0, 0.0, 1.0), 10.0, 60).unwrap();
    assert!(a.x > 340.0, "never reached the platform: {a}");
    assert!((a - b).length() < 1e-3, "one brush {a}, two brushes {b}");
}
