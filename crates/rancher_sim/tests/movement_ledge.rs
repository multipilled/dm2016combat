//! Ledge grab against simple brush worlds (needs the install; skipped without one).

use rancher_sim::Vec3;
use rancher_sim::cmd::UserCmd;
use rancher_sim::collision::{Hull, World};
use rancher_sim::install;
use rancher_sim::player::{CameraMode, Player, ledge_state};

#[test]
fn mantles_onto_a_waist_high_ledge() {
    let Some(doom) = idres::find_install() else { return };
    let inst = install::load(&doom).expect("loading install");
    let mut w = World::default();
    w.add(Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0)));
    // 64 high: above the foot-grab height (30), below the mantle/pull-up split (140).
    w.add(Hull::cuboid(Vec3::new(64.0, -256.0, 0.0), Vec3::new(512.0, 256.0, 64.0)));
    let mut p = Player::new(inst.movement, Vec3::new(0.0, 0.0, 1.0));
    for _ in 0..30 {
        p.think(&w, UserCmd::default(), 16);
    }
    let fwd = UserCmd { forward: 127, ..Default::default() };
    let mut started = None;
    let mut ended_at = None;
    for _ in 0..120 {
        p.think(&w, fwd, 16);
        if started.is_none() && p.ledge.state != ledge_state::NONE {
            started = Some(p.ledge.state);
        } else if started.is_some() && ended_at.is_none() && p.ledge.state == ledge_state::NONE {
            ended_at = Some(p.physics.origin);
        }
    }
    assert!(started.is_some(), "no ledge grab");
    let end = ended_at.expect("grab never ended");
    // Teleported onto the ledge top (surface 64), past the face at x = 64.
    assert!(end.z > 64.0 && end.z < 66.0 && end.x > 64.0 - 16.0, "ended at {end}");
}

/// A floor plus boxes `(x0, x1, z0, z1)` spanning y -256..256.
fn world_with(boxes: &[(f32, f32, f32, f32)]) -> World {
    let mut w = World::default();
    w.add(Hull::cuboid(Vec3::new(-4096.0, -4096.0, -64.0), Vec3::new(4096.0, 4096.0, 0.0)));
    for &(x0, x1, z0, z1) in boxes {
        w.add(Hull::cuboid(Vec3::new(x0, -256.0, z0), Vec3::new(x1, 256.0, z1)));
    }
    w
}

struct Run {
    /// Every state the mechanic went through, in order (starting with NONE).
    states: Vec<i32>,
    /// Origin and velocity on the frame the grab ended.
    end: Option<(Vec3, Vec3)>,
    player: Player,
}

/// Settles at `start` facing `yaw`, then runs `frames` frames of `cmd`, jumping once when the origin
/// passes `jump_at_x`.
fn run_at(w: &World, start: Vec3, yaw: f32, jump_at_x: Option<f32>, frames: usize, cmd: impl Fn(&Player) -> UserCmd) -> Option<Run> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    let mut p = Player::new(inst.movement, start);
    p.view_angles[1] = yaw;
    for _ in 0..10 {
        p.think(w, UserCmd::default(), 16);
    }
    let mut states = vec![p.ledge.state];
    let mut end = None;
    let mut jumped = false;
    for _ in 0..frames {
        let mut c = cmd(&p);
        if let Some(x) = jump_at_x {
            if !jumped && p.physics.origin.x >= x {
                c.up = 127;
                jumped = true;
            }
        }
        let before = p.ledge.state;
        p.think(w, c, 16);
        if before != ledge_state::NONE && p.ledge.state == ledge_state::NONE && end.is_none() {
            end = Some((p.physics.origin, p.physics.velocity));
        }
        if states.last() != Some(&p.ledge.state) {
            states.push(p.ledge.state);
        }
    }
    Some(Run { states, end, player: p })
}

fn forward(_: &Player) -> UserCmd {
    UserCmd { forward: 127, ..Default::default() }
}

#[test]
fn pulls_up_onto_a_chest_high_ledge() {
    // Delta height 120. Once the previous frame saw a ledge, the job's parms are left on
    // railingAboveLedgeParms (mantle split 120), so this is a pull-up rather than a mantle.
    let w = world_with(&[(400.0, 800.0, 0.0, 120.0)]);
    let Some(r) = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, None, 150, forward) else { return };
    assert_eq!(r.states, vec![ledge_state::NONE, ledge_state::PULL_UP, ledge_state::NONE]);
    let (end, vel) = r.end.unwrap();
    // EndLedgeGrab teleports to the destination + 0.25 up with zero velocity (0x140d9f120 → 0x14074f880).
    assert!(end.x > 400.0 && end.z > 120.0 && end.z < 122.0, "ended at {end}");
    assert_eq!(vel, Vec3::ZERO);
}

#[test]
fn foot_grab_from_a_running_jump() {
    // A running jump that meets the face with the ledge top less than 30 above the origin.
    let w = world_with(&[(400.0, 800.0, 0.0, 80.0)]);
    let Some(r) = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, Some(400.0 - 16.0 - 160.0), 150, forward) else { return };
    assert_eq!(r.states, vec![ledge_state::NONE, ledge_state::PULL_UP_FOOT, ledge_state::NONE]);
    let (end, _) = r.end.unwrap();
    // Aligned to the ledge point raised by FootGrabAlignHeightOffset (3).
    assert!((end.z - 83.25).abs() < 0.5 && end.x > 384.0, "ended at {end}");
    assert!(r.player.ledge.last_grab_was_foot_grab);
}

#[test]
fn vaults_a_thin_railing() {
    // 2 thick (≤ maxRailingThickness 8), 50 high: a railing mantle over it onto the far floor.
    let w = world_with(&[(400.0, 402.0, 0.0, 50.0)]);
    let Some(r) = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, None, 150, forward) else { return };
    assert_eq!(r.states, vec![ledge_state::NONE, ledge_state::RAILING_PULL_UP_MANTLE, ledge_state::NONE]);
    let (end, _) = r.end.unwrap();
    assert!(end.x > 402.0 + 16.0 && end.z < 2.0, "ended at {end}");
    // The same shape 12 thick is a ledge.
    let w = world_with(&[(400.0, 412.0, 0.0, 50.0)]);
    let r = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, None, 150, forward).unwrap();
    assert_eq!(r.states[1], ledge_state::PULL_UP_MANTLE);
}

#[test]
fn refuses_ledges_out_of_reach() {
    // More than maxDeltaHeight (145) above a standing player.
    let w = world_with(&[(400.0, 800.0, 0.0, 200.0)]);
    let Some(r) = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, None, 150, forward) else { return };
    assert_eq!(r.states, vec![ledge_state::NONE]);
}

#[test]
fn refuses_without_forward_input_or_when_crouched() {
    let w = world_with(&[(400.0, 800.0, 0.0, 64.0)]);
    // Standing against the face without pressing forward.
    let Some(r) = run_at(&w, Vec3::new(400.0 - 16.5, 0.0, 1.0), 0.0, None, 60, |_| UserCmd::default()) else { return };
    assert_eq!(r.states, vec![ledge_state::NONE]);
    // Crouch-walking into it (CanGrab 0x140da12d0 refuses while crouched).
    let r = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, None, 200, |_| UserCmd { forward: 127, up: -127, ..Default::default() }).unwrap();
    assert_eq!(r.states, vec![ledge_state::NONE]);
}

#[test]
fn refuses_shallow_approach_angles() {
    // pmec_lg_InitiateMaxAngle 45: walking into the face 60° off its normal does not grab, 30° does.
    let w = world_with(&[(400.0, 800.0, 0.0, 64.0)]);
    let Some(r) = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 60.0, None, 200, forward) else { return };
    assert_eq!(r.states, vec![ledge_state::NONE]);
    let r = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 30.0, None, 200, forward).unwrap();
    assert_eq!(r.states[1], ledge_state::PULL_UP_MANTLE);
}

#[test]
fn no_regrab_until_the_no_grab_time_passes() {
    let w = world_with(&[(400.0, 800.0, 0.0, 64.0)]);
    let Some(r) = run_at(&w, Vec3::new(0.0, 0.0, 1.0), 0.0, None, 150, forward) else { return };
    let lg = &r.player.ledge;
    assert_eq!(lg.can_grab_time, lg.grab_end_time + 500, "pmec_lg_NoGrabTimeMS");
}

#[test]
fn animated_grab_moves_with_the_spring_camera() {
    let w = world_with(&[(400.0, 800.0, 0.0, 120.0)]);
    let Some(doom) = idres::find_install() else { return };
    let inst = install::load(&doom).expect("loading install");
    let mut p = Player::new(inst.movement, Vec3::new(0.0, 0.0, 1.0));
    for _ in 0..10 {
        p.think(&w, UserCmd::default(), 16);
    }
    let mut last_cam_vel = Vec3::ZERO;
    let mut grabbing_frames = 0;
    let mut saw_blend_out = false;
    for _ in 0..150 {
        let was = p.ledge.state;
        p.think(&w, forward(&p), 16);
        if was != ledge_state::NONE {
            // The constraint was in force for this frame's angles.
            let yaw = p.view_angles[1];
            let yaw = if yaw > 180.0 { yaw - 360.0 } else { yaw };
            assert!(yaw.abs() <= 45.0 + 1e-3 && p.view_angles[0] >= -45.0 - 1e-3, "angles {:?}", p.view_angles);
        }
        if was != ledge_state::NONE && p.ledge.state != ledge_state::NONE {
            // Event 0x11: after the move, the velocity is the camera's from its previous update.
            assert_eq!(p.physics.velocity, last_cam_vel);
            grabbing_frames += 1;
            assert_eq!(p.grab_camera.mode, CameraMode::Attached);
            assert_eq!(p.view_origin(), p.grab_camera.pos);
            // Try to look far left and up; the constraint keeps yaw within ±MaxDeltaYaw (45) of the
            // wall and pitch within -MaxDeltaPitchUp (45).
            p.look(-4000.0, -4000.0);
        }
        saw_blend_out |= p.grab_camera.mode == CameraMode::BlendOut;
        last_cam_vel = p.grab_camera.velocity;
    }
    assert!(grabbing_frames > 20, "{grabbing_frames} frames of grab");
    assert!(saw_blend_out);
    // The blend back to the eye ends after viewBlendDurationMS (100) and the constraint is lifted.
    assert_eq!(p.grab_camera.mode, CameraMode::Off);
    assert!(p.view_constraint.is_none());
    assert_eq!(p.view_origin(), p.physics.origin + Vec3::Z * p.eye_height());
}
