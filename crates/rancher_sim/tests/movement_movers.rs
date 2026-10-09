//! Movers pushing and carrying the player (idPush → idPhysics_Player Translate/Rotate impulses, consumed
//! by AirMove / SlideMove). Needs the install for the player tuning; skipped without one.

use std::sync::Arc;

use rancher_sim::Vec3;
use rancher_sim::cmd::UserCmd;
use rancher_sim::collision::{Hull, Mat3, MoverProps, World};
use rancher_sim::install;
use rancher_sim::player::{Player, push_flags};

/// A box as a one-submodel collision model (solid surface, no KD nodes), in model space.
fn box_cm(min: Vec3, max: Vec3) -> idres::bcm::CollisionModel {
    let hull = Hull::cuboid(min, max);
    let mut sm = idres::bcm::SubModel {
        bounds: [min.to_array(), max.to_array()],
        contents: 1,
        surfaces: vec![idres::bcm::SurfaceInfo { contents: 1, surface_flags: 0, surface_type: 0, color: [0; 3] }],
        verts: hull.verts.iter().map(|v| v.to_array()).collect(),
        ..Default::default()
    };
    let ib = |v: Vec3, up: bool| -> [i16; 3] {
        let f = |x: f32| if up { x.ceil() as i16 } else { x.floor() as i16 };
        [f(v.x), f(v.y), f(v.z)]
    };
    for p in &hull.polys {
        let first = sm.edge_refs.len() as u16;
        for k in 0..p.verts.len() {
            let (a, b) = (p.verts[k], p.verts[(k + 1) % p.verts.len()]);
            let e = match sm.edges.iter().position(|e| (e[0] as usize, e[1] as usize) == (b, a)) {
                Some(e) => e as u16 | idres::bcm::EDGE_REVERSED,
                None => {
                    sm.edges.push([a as u16, b as u16]);
                    (sm.edges.len() - 1) as u16
                }
            };
            sm.edge_refs.push(e);
        }
        let pts: Vec<Vec3> = p.verts.iter().map(|&v| hull.verts[v]).collect();
        let lo = pts.iter().copied().reduce(Vec3::min).unwrap();
        let hi = pts.iter().copied().reduce(Vec3::max).unwrap();
        sm.polygons.push(idres::bcm::Polygon { bounds: [ib(lo, false), ib(hi, true)], surface: 0, num_edges: p.verts.len() as u8, first_edge: first });
    }
    sm.brushes.push(idres::bcm::Brush { bounds: [ib(min, false), ib(max, true)], surface: 0, num_planes: 6, first_plane: 0 });
    sm.planes = hull.polys.iter().map(|p| [p.normal.x, p.normal.y, p.normal.z, -p.dist]).collect();
    idres::bcm::CollisionModel {
        version: b'8',
        timestamp: 0,
        build: Default::default(),
        bounds: [min.to_array(), max.to_array()],
        unknown_70: 0,
        flags: [0; 4],
        tree: Vec::new(),
        submodels: vec![sm],
    }
}

fn floor() -> Hull {
    Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0))
}

struct Rig {
    w: World,
    p: Player,
    mover: usize,
    at: Vec3,
}

/// A floor, a 128×128×16 platform mover at `at`, and the player standing at `player`.
fn rig(at: Vec3, player: Vec3, props: MoverProps) -> Option<Rig> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    let mut w = World::default();
    w.add(floor());
    let mover = w.add_cm(Arc::new(box_cm(Vec3::new(-64.0, -64.0, 0.0), Vec3::new(64.0, 64.0, 16.0))), at, Mat3::IDENTITY);
    w.set_cm_mover(mover, props);
    let mut p = Player::new(inst.movement, player);
    for _ in 0..30 {
        p.think(&w, UserCmd::default(), 16);
    }
    Some(Rig { w, p, mover, at })
}

impl Rig {
    /// One frame: the player thinks, then the mover moves by `step` (entities move after the player).
    fn frame(&mut self, cmd: UserCmd, step: Vec3) -> f32 {
        self.p.think(&self.w, cmd, 16);
        let to = self.at + step;
        let f = self.p.push_mover(&mut self.w, self.mover, to, Mat3::IDENTITY, 0);
        if f > 0.0 {
            self.at = to;
        }
        f
    }
}

#[test]
fn elevator_carries_the_player_up_and_down() {
    let Some(mut r) = rig(Vec3::ZERO, Vec3::new(0.0, 0.0, 17.0), MoverProps::default()) else { return };
    assert!((r.p.physics.origin.z - 16.25).abs() < 0.01, "standing on the platform: {}", r.p.physics.origin);
    // Up 2 units a frame (125/s at 16 ms) for 100 frames.
    for i in 0..100 {
        assert_eq!(r.frame(UserCmd::default(), Vec3::new(0.0, 0.0, 2.0)), 1.0);
        let top = r.at.z + 16.0;
        let z = r.p.physics.origin.z;
        // The player moves in its own think with the previous push: one frame behind the platform.
        assert!(z > top - 2.5 && z < top + 1.0, "frame {i}: player {z}, platform top {top}");
    }
    assert!(r.p.physics.walking || r.p.physics.ground_plane);
    // Back down at the same speed: the downward push keeps the player on the platform.
    for i in 0..100 {
        r.frame(UserCmd::default(), Vec3::new(0.0, 0.0, -2.0));
        let top = r.at.z + 16.0;
        let z = r.p.physics.origin.z;
        assert!(z > top - 1.0 && z < top + 3.0, "down frame {i}: player {z}, platform top {top}");
    }
}

#[test]
fn a_moving_wall_pushes_the_player_and_a_pinned_player_blocks_it() {
    // Platform on its side as a wall moving +x toward the player at 2 units a frame.
    let Some(mut r) = rig(Vec3::new(-200.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), MoverProps::default()) else { return };
    let start_x = r.p.physics.origin.x;
    for _ in 0..80 {
        r.frame(UserCmd::default(), Vec3::new(2.0, 0.0, 0.0));
    }
    let x = r.p.physics.origin.x;
    let face = r.at.x + 64.0;
    assert!(x > start_x + 20.0, "never pushed: {x}");
    assert!(x >= face + 16.0 - 2.5, "inside the mover: player {x}, face {face}");

    // A fixed wall ahead: once pinned, the mover cannot move any further (fraction 0) ...
    let mut r = rig(Vec3::new(-200.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), MoverProps::default()).unwrap();
    r.w.add(Hull::cuboid(Vec3::new(40.0, -256.0, 0.0), Vec3::new(80.0, 256.0, 128.0)));
    let mut stopped = false;
    for _ in 0..200 {
        if r.frame(UserCmd::default(), Vec3::new(2.0, 0.0, 0.0)) == 0.0 {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "the mover never stopped");
    assert!(r.p.physics.origin.x <= 40.0 - 16.0 + 0.5, "pushed through the wall: {}", r.p.physics.origin);
    // ... unless it crushes.
    let to = r.at + Vec3::new(2.0, 0.0, 0.0);
    assert_eq!(r.p.push_mover(&mut r.w, r.mover, to, Mat3::IDENTITY, push_flags::CRUSH), 1.0);
}

#[test]
fn jumping_off_a_moving_platform_keeps_its_velocity() {
    let Some(mut r) = rig(Vec3::ZERO, Vec3::new(0.0, 0.0, 17.0), MoverProps::default()) else { return };
    for _ in 0..10 {
        r.frame(UserCmd::default(), Vec3::new(2.0, 0.0, 0.0));
    }
    // Carried sideways with the platform (a frame behind).
    let dx = r.p.physics.origin.x - r.at.x;
    assert!((-2.5..=0.5).contains(&dx), "not carried: player {} platform {}", r.p.physics.origin.x, r.at.x);
    // The jump frame leaves the ground; the next air move turns the platform's push into velocity.
    r.frame(UserCmd { up: 127, ..Default::default() }, Vec3::new(2.0, 0.0, 0.0));
    r.frame(UserCmd::default(), Vec3::ZERO);
    let vx = r.p.physics.velocity.x;
    assert!((vx - 125.0).abs() < 15.0, "inherited {vx}");
}

#[test]
fn prevent_player_jump_platforms_refuse_jumps() {
    let props = MoverProps { prevent_player_jump: true, ..MoverProps::default() };
    let Some(mut r) = rig(Vec3::ZERO, Vec3::new(0.0, 0.0, 17.0), props) else { return };
    r.frame(UserCmd { up: 127, ..Default::default() }, Vec3::ZERO);
    assert!(!r.p.events.jumped, "jumped off a preventPlayerJump mover");
    // The same platform as an ordinary mover allows it.
    let mut r = rig(Vec3::ZERO, Vec3::new(0.0, 0.0, 17.0), MoverProps::default()).unwrap();
    r.frame(UserCmd { up: 127, ..Default::default() }, Vec3::ZERO);
    assert!(r.p.events.jumped);
}

#[test]
fn a_turntable_carries_the_player_around() {
    let Some(mut r) = rig(Vec3::ZERO, Vec3::new(40.0, 0.0, 17.0), MoverProps::default()) else { return };
    let mut angle: f32 = 0.0;
    for _ in 0..90 {
        r.p.think(&r.w, UserCmd::default(), 16);
        angle += 1.0f32.to_radians();
        assert_eq!(r.p.push_mover(&mut r.w, r.mover, Vec3::ZERO, Mat3::from_rotation_z(angle), 0), 1.0);
    }
    r.p.think(&r.w, UserCmd::default(), 16);
    let o = r.p.physics.origin;
    // A quarter turn: (40, 0) → (0, 40), give or take the frame of lag and the chord steps.
    assert!(o.x.abs() < 3.0 && (o.y - 40.0).abs() < 3.0, "after 90°: {o}");
}

#[test]
fn a_bind_team_moves_together_or_not_at_all() {
    // Master (the platform under the player) and a bound wall part beside it, moving up together.
    let Some(mut r) = rig(Vec3::ZERO, Vec3::new(0.0, 0.0, 17.0), MoverProps::default()) else { return };
    let wall = r.w.add_cm(Arc::new(box_cm(Vec3::new(-8.0, -64.0, 0.0), Vec3::new(8.0, 64.0, 128.0))), Vec3::new(80.0, 0.0, 16.0), Mat3::IDENTITY);
    r.w.set_cm_mover(wall, MoverProps::default());
    let mut wall_at = Vec3::new(80.0, 0.0, 16.0);
    for _ in 0..30 {
        r.p.think(&r.w, UserCmd::default(), 16);
        let step = Vec3::new(0.0, 0.0, 2.0);
        assert!(r.p.push_team(&mut r.w, &[(r.mover, r.at + step, Mat3::IDENTITY), (wall, wall_at + step, Mat3::IDENTITY)], 0));
        r.at += step;
        wall_at += step;
    }
    assert!((r.p.physics.origin.z - (r.at.z + 16.0)).abs() < 2.5, "carried by the team: {}", r.p.physics.origin);
    // The bound part sweeping sideways into a player pinned by a fixed wall blocks the whole team.
    r.w.add(Hull::cuboid(Vec3::new(-200.0, -256.0, 0.0), Vec3::new(-16.5 - 0.25, 256.0, 400.0)));
    let before = (r.w.cm_transform(r.mover), r.w.cm_transform(wall));
    r.p.physics.origin.x = 0.0;
    let ok = r.p.push_team(&mut r.w, &[(r.mover, r.at, Mat3::IDENTITY), (wall, Vec3::new(-40.0, 0.0, wall_at.z), Mat3::IDENTITY)], 0);
    assert!(!ok);
    assert_eq!((r.w.cm_transform(r.mover), r.w.cm_transform(wall)), before, "the team was not restored");
}
