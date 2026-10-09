//! Install-backed tests of the Possessed brain (skipped without an install).

use glam::Vec3;

use super::*;

/// 60 Hz frames in game ticks (960/s).
const FRAME_TICKS: i64 = 16;
const DT: f32 = 1.0 / 60.0;

fn install() -> Option<crate::install::Install> {
    let doom = idres::find_install()?;
    Some(crate::install::load(&doom).expect("loading install"))
}

fn target_at(p: Vec3) -> Target {
    // player: clip-bounds centre at pm_normalheight / 2 = 45, half width pm_bboxwidth / 2 = 16
    Target { origin: p, sight_point: p + Vec3::Z * 45.0, alive: true, half_width: 16.0 }
}

fn body_at(p: Vec3, yaw: f32) -> Body {
    Body { origin: p, yaw, alive: true, in_pain: false }
}

/// Runs `n` frames; returns every output.
fn run(b: &mut Brain, w: &dyn World, body: &mut Body, t: &Target, start: i64, n: usize) -> Vec<AiOutput> {
    let mut outs = Vec::new();
    for i in 0..n {
        let o = b.tick(w, body, t, start + i as i64 * FRAME_TICKS, DT);
        body.step(&o, DT);
        outs.push(o);
    }
    outs
}

fn confirm(b: &mut Brain, t: &Target) {
    b.sense.awareness = Awareness::Confirmed;
    b.sense.last_visible = Some(0);
    b.sense.last_known_pos = t.origin;
    b.state = State::Default;
}

#[test]
fn possessed_decls() {
    let Some(inst) = install() else { return };
    let d = AiDef::load(&inst.decls, POSSESSED).unwrap();
    // aiEditable.perception over the 0x1406f3680 defaults.
    let p = &d.perception;
    assert_eq!((p.actor_radius, p.fov, p.event_radius), (750.0, 150.0, 750.0));
    assert_eq!((p.close_radius, p.fov_close, p.fov_focused, p.exposed_sight_time), (256.0, 300.0, 360.0, 0.3));
    assert_eq!(d.eye_height, 86.0);
    assert_eq!(d.movement.aas_name, "aas_monster48");
    assert_eq!(d.movement.alignment_tolerance, 15.0);
    let sg = d.attacks.subgraph("default").unwrap();
    let names: Vec<&str> = sg.attacks.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names[..4], ["melee_forward", "melee_left", "melee_right", "melee_back"]);
    let fwd = &sg.attacks[0];
    assert_eq!((fwd.arc_direction, fwd.arc_half_length, fwd.distance), (0.0, 45.0, (0.0, 130.0)));
    assert_eq!(fwd.via_node, "zion/characters/monsters/zombie/hands_melee/forward");
    let w = &d.melee_windows["md6/characters/monsters/zombie/base/motion/combat/melee_forward.md6anim"][0];
    assert_eq!((w.start_frame, w.end_frame, w.joint_group.as_str()), (4.0, 18.0, "left_arm"));
    let dmg = &d.damage[&w.damage_decl];
    assert_eq!((dmg.min_damage, dmg.max_damage, dmg.player_damage_scale), (60.0, 60.0, 0.25));
}

#[test]
fn notices_player_at_decoded_sight_conditions() {
    let Some(inst) = install() else { return };
    let base = Brain::load(&inst.decls, POSSESSED).unwrap();
    assert!(base.loco.walk_speed > 10.0, "walk root motion {}", base.loco.walk_speed);
    let open = OpenWorld { nav: None };
    // exposedSightTime 0.3 s accumulated per frame (f32, as the brain sums it)
    let frames_to_confirm = {
        let (mut e, mut k) = (0f32, 0usize);
        while e < 0.3 {
            e += DT;
            k += 1;
        }
        k
    };
    // (target xy relative to an AI at the origin facing +x, expected noticed)
    let at = |d: f32, deg: f32| Vec3::new(d * deg.to_radians().cos(), d * deg.to_radians().sin(), 0.0);
    let cases = [
        (at(600.0, 0.0), true),
        (at(600.0, 70.0), true),   // inside FOV 150 / 2
        (at(600.0, 80.0), false),  // outside
        (at(800.0, 0.0), false),   // beyond actorPerceptionRadius 750 (eye distance)
        (at(200.0, 140.0), true),  // inside closePerceptionRadius 256: FOV_close 300 / 2
        (at(200.0, 160.0), false), // behind even the close FOV
    ];
    for (p, expect) in cases {
        let mut b = Brain::load(&inst.decls, POSSESSED).unwrap();
        let mut body = body_at(Vec3::ZERO, 0.0);
        let t = target_at(p);
        let outs = run(&mut b, &open, &mut body, &t, 0, frames_to_confirm + 2);
        let noticed = outs.iter().position(|o| o.events.contains(&AiEvent::Noticed));
        assert_eq!(noticed.is_some(), expect, "target {p:?}");
        if let Some(i) = noticed {
            // exposedSightTime 0.3 s of continuous sight
            assert_eq!(i + 1, frames_to_confirm, "frames to confirm at {p:?}");
            // COMBAT_ShouldSightEnemy (EM_NEWLY_AWARE) sends Default -> zombie_sighted on the notice frame.
            assert!(outs[i].events.iter().any(|e| matches!(e, AiEvent::StateChanged { to: State::Sighted, .. })));
        }
    }
    // No line of sight: never noticed.
    struct Wall;
    impl World for Wall {
        fn line_of_sight(&self, _: Vec3, _: Vec3) -> bool {
            false
        }
    }
    let mut b = Brain::load(&inst.decls, POSSESSED).unwrap();
    let mut body = body_at(Vec3::ZERO, 0.0);
    let outs = run(&mut b, &Wall, &mut body, &target_at(at(300.0, 0.0)), 0, 60);
    assert!(outs.iter().all(|o| !o.events.contains(&AiEvent::Noticed)));
    assert_eq!(b.state, State::Relaxed);
}

#[test]
fn walks_with_decoded_locomotion_requests_and_melees() {
    let Some(inst) = install() else { return };
    let mut b = Brain::load(&inst.decls, POSSESSED).unwrap();
    let open = OpenWorld { nav: None };
    // Player 500 units ahead and 30 degrees left: noticed -> sighted (turns) -> walks.
    let p = Vec3::new(500.0 * 30f32.to_radians().cos(), 500.0 * 30f32.to_radians().sin(), 0.0);
    let t = target_at(p);
    let mut body = body_at(Vec3::ZERO, 0.0);
    let outs = run(&mut b, &open, &mut body, &t, 0, 60 * 10);
    let walk = outs.iter().find(|o| o.state == Some(State::MoveTowardEnemy)).expect("walks");
    assert_eq!(walk.web_node.as_deref(), Some("zion/characters/monsters/zombie/hands_combat/walk"));
    assert!(walk.scalars.iter().any(|(k, _)| k == "bodyMoveAngle"));
    assert_eq!(walk.move_speed, b.loco.walk_speed);
    // Sighted turned the body to the player before walking.
    let first_walk = outs.iter().position(|o| o.state == Some(State::MoveTowardEnemy)).unwrap();
    assert!(outs[..first_walk].iter().any(|o| o.state == Some(State::Sighted)));
    // The melee starts once the player is inside an attack's range (lunge 130..180 or melee_forward 0..130).
    let start = outs.iter().flat_map(|o| o.events.iter()).find_map(|e| match e {
        AiEvent::AttackStart { attack, .. } => Some(attack.clone()),
        _ => None,
    });
    let a = start.expect("attacks");
    assert!(["melee_forward", "lunge_short", "lunge_long"].contains(&a.as_str()), "{a}");
}

#[test]
fn melee_starts_at_decoded_distance_with_decl_damage() {
    let Some(inst) = install() else { return };
    let open = OpenWorld { nav: None };
    let attack_at = |d: f32| {
        let mut b = Brain::load(&inst.decls, POSSESSED).unwrap();
        let t = target_at(Vec3::new(d, 0.0, 0.0));
        confirm(&mut b, &t);
        let mut body = body_at(Vec3::ZERO, 0.0);
        // Hold the AI in place (it would walk); only the first frame matters for the choice.
        let o = b.tick(&open, &body, &t, 0, DT);
        body.step(&o, 0.0);
        let first = o.events.iter().find_map(|e| match e {
            AiEvent::AttackStart { attack, anim, .. } => Some((attack.clone(), anim.clone())),
            _ => None,
        });
        (b, first)
    };
    // melee_forward: distanceRange 0..130, arc 0 +- 45.
    let (mut b, first) = attack_at(120.0);
    let (name, anim) = first.expect("melee at 120");
    assert_eq!(name, "melee_forward");
    assert_eq!(anim, "md6/characters/monsters/zombie/base/motion/combat/melee_forward.md6anim");
    // Its sphere trace opens at frame 4 with the decl damage.
    let t = target_at(Vec3::new(120.0, 0.0, 0.0));
    let body = body_at(Vec3::ZERO, 0.0);
    let mut hit = None;
    for i in 1..120 {
        let o = b.tick(&open, &body, &t, i * FRAME_TICKS, DT);
        if let Some(e) = o.events.iter().find(|e| matches!(e, AiEvent::MeleeTraceStart { .. })) {
            hit = Some((i, e.clone()));
            break;
        }
    }
    let (i, e) = hit.expect("melee trace");
    assert_eq!(
        e,
        AiEvent::MeleeTraceStart {
            joint_group: "left_arm".into(),
            damage_decl: "damage/zion/ai/zombie/melee".into(),
            damage: 60.0,
            player_damage_scale: 0.25
        }
    );
    // frame 4 at the anim's frame rate
    let fr = b.anim_durations[&anim].1;
    assert_eq!(i, (4.0 / fr * TICKS_PER_SEC as f32 / FRAME_TICKS as f32).ceil() as i64);
    // lunges (ATTACK_VALIDATOR_ANIM_DELTA): distance minus the lunge anim root-motion delta in 130..180.
    let lunge = b.anim_deltas["md6/characters/monsters/zombie/base/motion/combat/melee_lunge_short_left_arm.md6anim"];
    assert!(lunge > 0.0);
    assert_eq!(attack_at(lunge + 150.0).1.map(|x| x.0).as_deref(), Some("lunge_short"));
    assert_ne!(attack_at(lunge + 190.0).1.map(|x| x.0).as_deref(), Some("lunge_short"));
    // between melee_forward (130) and the lunge window: no attack, it walks instead.
    let (b, first) = attack_at(130.0 + lunge.min(20.0) * 0.5);
    assert!(first.is_none());
    assert_eq!(b.state, State::MoveTowardEnemy);
    // outside the forward arc (player at 90 degrees left, 100 units): melee_left (70 +- 30, 0..110).
    let mut b = Brain::load(&inst.decls, POSSESSED).unwrap();
    let t = target_at(Vec3::new(0.0, 100.0, 0.0));
    confirm(&mut b, &t);
    let o = b.tick(&open, &body_at(Vec3::ZERO, 0.0), &t, 0, DT);
    assert!(o.events.iter().any(|e| matches!(e, AiEvent::AttackStart { attack, .. } if attack == "melee_left")));
}

#[test]
fn paths_toward_player_on_intro_nav() {
    let Some(inst) = install() else { return };
    let c = inst.decls.container();
    let nav = Nav::load(c, "game/sp/intro/intro", "aas_monster48").unwrap();
    assert_eq!(nav.aas.areas.len(), 6211);
    assert_eq!(nav.aas.reach.len(), 38226);
    assert_eq!(nav.aas.settings.file_extension, "aas_monster48");
    assert_eq!((nav.aas.settings.gravity(), nav.aas.settings.max_step_height()), (1066.0, 18.0));
    // Point queries land in an area whose bounds contain the point (BSP side convention check).
    let mut hits = 0;
    for a in (1..nav.aas.areas.len()).step_by(97) {
        let p = Vec3::from_array(nav.aas.area_center(a)) + Vec3::Z * 1.0;
        if let Some(found) = nav.point_area(p) {
            assert!(nav.in_bounds(found, p, 1.0), "area {a} -> {found}");
            hits += 1;
        }
    }
    assert!(hits > 40, "{hits} point queries hit");
    // A start area and a goal 400..700 units away that the route reaches.
    let start_area = (1..nav.aas.areas.len()).find(|&a| nav.aas.reach_from(a).count() >= 3).unwrap();
    let s = Vec3::from_array(nav.aas.area_center(start_area)) + Vec3::Z;
    let goal_area = (1..nav.aas.areas.len())
        .filter(|&g| {
            let d = Vec3::from_array(nav.aas.area_center(g)).distance(s);
            (400.0..700.0).contains(&d)
        })
        .find(|&g| nav.point_area(Vec3::from_array(nav.aas.area_center(g)) + Vec3::Z) == Some(g) && !nav.path(s, Vec3::from_array(nav.aas.area_center(g)) + Vec3::Z).is_empty())
        .expect("a reachable goal");
    let g = Vec3::from_array(nav.aas.area_center(goal_area)) + Vec3::Z;
    let path = nav.path(s, g);
    assert!(path.len() >= 2);
    // The brain walks the path toward the player standing at the goal.
    let mut b = Brain::load(&inst.decls, POSSESSED).unwrap();
    let t = target_at(g);
    confirm(&mut b, &t);
    let w = OpenWorld { nav: Some(&nav) };
    let mut body = body_at(s, 0.0);
    let d0 = s.distance(g);
    let outs = run(&mut b, &w, &mut body, &t, 0, 60 * 30);
    assert!(outs.iter().any(|o| o.state == Some(State::MoveTowardEnemy)));
    let d1 = body.origin.distance(g);
    // It walks the path until an attack (the long lunge starts far out) or arrival.
    let attacked = outs.iter().any(|o| o.events.iter().any(|e| matches!(e, AiEvent::AttackStart { .. })));
    assert!(d1 < d0 - 100.0 && (attacked || d1 < d0 * 0.5), "walked from {d0} to {d1}, attacked {attacked}");
}
