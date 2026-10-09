//! Player collision against a shipped map's collision model (`.bcm`). Heavy (intro's world.bcm is
//! 2.9M polygons), so ignored by default:
//! `cargo test --release -p rancher_sim --test movement_bcm -- --ignored --nocapture`

use std::sync::Arc;
use std::time::Instant;

use rancher_sim::Vec3;
use rancher_sim::cmd::UserCmd;
use rancher_sim::collision::{CLIP_EPSILON, Hull, Mat3, World};
use rancher_sim::install;
use rancher_sim::player::{Player, ledge_state};

fn intro_world() -> Option<(World, Arc<idres::bcm::CollisionModel>)> {
    let doom = idres::find_install()?;
    let c = idres::Container::open(&doom.join("base"), "gameresources").expect("container");
    let bytes = c.read_by_name("maps/game/sp/intro/intro/_combo/world.bcm").expect("world.bcm");
    let t = Instant::now();
    let cm = Arc::new(idres::bcm::CollisionModel::parse(&bytes).expect("parse"));
    println!("parsed {} polygons in {:.2}s", cm.polygon_count(), t.elapsed().as_secs_f32());
    let mut w = World::default();
    w.add_cm(cm.clone(), Vec3::ZERO, Mat3::IDENTITY);
    Some((w, cm))
}

/// The centroid of the largest solid, upward-facing, horizontal polygon with nothing solid above it.
fn open_floor(w: &World, cm: &idres::bcm::CollisionModel) -> Vec3 {
    let mut best = (0.0f32, Vec3::ZERO);
    for sm in &cm.submodels {
        for p in &sm.polygons {
            if sm.surfaces[p.surface as usize].contents & 1 == 0 {
                continue;
            }
            let pl = sm.polygon_plane(p);
            if pl[2] < 0.999 {
                continue;
            }
            let pts: Vec<Vec3> = sm.polygon_verts(p).map(|v| Vec3::from_array(sm.verts[v])).collect();
            let mut a = Vec3::ZERO;
            for i in 1..pts.len() - 1 {
                a += (pts[i] - pts[0]).cross(pts[i + 1] - pts[0]);
            }
            let area = a.length() * 0.5;
            let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            if area > best.0 && w.ray(c + Vec3::Z, Vec3::Z, 256.0).is_none() {
                best = (area, c);
            }
        }
    }
    best.1
}

#[test]
#[ignore]
fn walks_on_intro_world_collision() {
    let Some((w, cm)) = intro_world() else { return };
    let floor = open_floor(&w, &cm);
    println!("floor at {floor}");

    // A point trace straight down stops CLIP_EPSILON above the floor.
    let tr = w.translate(&Hull::trace_box(), floor + Vec3::Z * 100.0, floor - Vec3::Z * 10.0);
    assert!(tr.hit(), "no hit below {floor}");
    assert!((tr.endpos.z - (floor.z + 0.01 + CLIP_EPSILON)).abs() < 0.02, "rest {}", tr.endpos);

    let Some(doom) = idres::find_install() else { return };
    let inst = install::load(&doom).expect("loading install");
    let mut p = Player::new(inst.movement, floor + Vec3::Z * 64.0);
    assert!(w.position_clear(&p.physics.normal_shape, p.physics.origin));
    for _ in 0..60 {
        p.think(&w, UserCmd::default(), 16);
    }
    assert!(p.physics.walking, "never landed: {}", p.physics.origin);
    assert!((p.physics.origin.z - (floor.z + CLIP_EPSILON)).abs() < 0.05, "landed at {}", p.physics.origin);

    // Run around for five seconds, turning; the player stays on or above the floor and keeps moving.
    let t = Instant::now();
    let start = p.physics.origin;
    let mut travelled = 0.0;
    for i in 0..300 {
        p.view_angles[1] = (i as f32 * 1.5) % 360.0;
        let before = p.physics.origin;
        p.think(&w, UserCmd { forward: 127, ..Default::default() }, 16);
        assert!(p.physics.origin.is_finite());
        travelled += (p.physics.origin - before).length();
    }
    let ms = t.elapsed().as_secs_f32() * 1000.0 / 300.0;
    println!("ran {travelled:.0} units, now {} (from {start}), {ms:.3} ms per frame", p.physics.origin);
    assert!(travelled > 500.0);
    assert!(p.physics.origin.z > floor.z - 1.0, "fell through: {}", p.physics.origin);

    // Straight runs in eight directions until walls stop them: never inside solid geometry, and some
    // runs end against a wall (velocity clipped to nothing along the run).
    let mut blocked = 0;
    for k in 0..8 {
        p.physics.origin = start;
        p.physics.velocity = Vec3::ZERO;
        p.view_angles[1] = k as f32 * 45.0;
        for _ in 0..600 {
            p.think(&w, UserCmd { forward: 127, ..Default::default() }, 16);
            assert!(w.position_clear(&p.physics.normal_shape, p.physics.origin), "inside geometry at {}", p.physics.origin);
        }
        let speed = (p.physics.velocity.x.powi(2) + p.physics.velocity.y.powi(2)).sqrt();
        println!("yaw {}: ended {} speed {speed:.1}", k * 45, p.physics.origin);
        blocked += (speed < 50.0) as u32;
    }
    assert!(blocked > 0, "no run reached a wall");
}

/// Floor spots near the intro's start: centroids of solid, horizontal, upward polygons of some size
/// with headroom, in a deterministic order.
fn floor_spots(w: &World, cm: &idres::bcm::CollisionModel, near: Vec3, radius: f32, max: usize) -> Vec<Vec3> {
    let mut spots = Vec::new();
    for sm in &cm.submodels {
        for p in &sm.polygons {
            if sm.surfaces[p.surface as usize].contents & 1 == 0 || sm.polygon_plane(p)[2] < 0.99 {
                continue;
            }
            let pts: Vec<Vec3> = sm.polygon_verts(p).map(|v| Vec3::from_array(sm.verts[v])).collect();
            let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            let mut a = Vec3::ZERO;
            for i in 1..pts.len() - 1 {
                a += (pts[i] - pts[0]).cross(pts[i + 1] - pts[0]);
            }
            if a.length() * 0.5 > 2000.0 && (c - near).length() < radius && w.ray(c + Vec3::Z, Vec3::Z, 128.0).is_none() {
                spots.push(c);
            }
        }
    }
    spots.sort_by(|a, b| (a.x, a.y, a.z).partial_cmp(&(b.x, b.y, b.z)).unwrap());
    let step = (spots.len() / max).max(1);
    spots.into_iter().step_by(step).take(max).collect()
}

#[test]
#[ignore]
fn ledge_grabs_on_intro_geometry() {
    let Some((w, cm)) = intro_world() else { return };
    let Some(doom) = idres::find_install() else { return };
    let inst = install::load(&doom).expect("loading install");
    let start = Vec3::new(-17904.0, -2805.0, 3072.0);
    let spots = floor_spots(&w, &cm, start, 6000.0, 120);
    println!("{} floor spots", spots.len());
    let (mut runs, mut grabs) = (0, 0);
    let mut by_state = std::collections::BTreeMap::new();
    let mut edges = [0; 3];
    for spot in spots {
        for k in 0..8 {
            let mut p = Player::new(inst.movement.clone(), spot + Vec3::Z * 2.0);
            if !w.position_clear(&p.physics.normal_shape, p.physics.origin) {
                continue;
            }
            p.view_angles[1] = k as f32 * 45.0;
            for _ in 0..20 {
                p.think(&w, UserCmd::default(), 16);
            }
            runs += 1;
            let mut grab_start: Option<(i32, Vec3)> = None;
            for _ in 0..150 {
                let before = p.ledge.state;
                p.think(&w, UserCmd { forward: 127, ..Default::default() }, 16);
                assert!(p.physics.origin.is_finite());
                if before == ledge_state::NONE && p.ledge.state != ledge_state::NONE {
                    grab_start = Some((p.ledge.state, p.physics.origin));
                    edges[p.ledge.info[0].edge_type.clamp(0, 2) as usize] += 1;
                }
                if before != ledge_state::NONE && p.ledge.state == ledge_state::NONE {
                    let (state, from) = grab_start.take().expect("grab ended without starting");
                    grabs += 1;
                    *by_state.entry(state).or_insert(0) += 1;
                    let o = p.physics.origin;
                    assert!(w.position_clear(&p.physics.normal_shape, o), "grab {state} from {from} ended inside geometry at {o}");
                    assert!(o.z > from.z - 1.0, "grab {state} from {from} ended lower at {o}");
                    break;
                }
            }
        }
    }
    println!("{runs} runs, {grabs} grabs, by state {by_state:?}, edge types square/rounded/angled {edges:?}");
    assert!(grabs > 0, "no ledge grabs on intro geometry");
}

#[test]
#[ignore]
fn intro_surface_flag_census() {
    let Some((_, cm)) = intro_world() else { return };
    let mut counts = std::collections::BTreeMap::new();
    for sm in &cm.submodels {
        for p in &sm.polygons {
            let f = sm.surfaces[p.surface as usize].surface_flags;
            for bit in [2u32, 4, 32, 128, 512, 1024] {
                if f & bit != 0 {
                    *counts.entry(bit).or_insert(0usize) += 1;
                }
            }
        }
    }
    println!("polygons with surface flag bits (2 rounded, 4 angled, 32 slick, 128 ladder, 512 nosteps, 1024 stairs): {counts:?}");
}

#[test]
#[ignore]
fn rounded_and_angled_ledges_on_intro() {
    let Some((w, cm)) = intro_world() else { return };
    let Some(doom) = idres::find_install() else { return };
    let inst = install::load(&doom).expect("loading install");
    // Faces flagged LEDGE_GRAB_ROUNDED (2) / ANGLED (4) that are near vertical: approach them head on.
    let mut faces = Vec::new();
    for sm in &cm.submodels {
        for p in &sm.polygons {
            let f = sm.surfaces[p.surface as usize].surface_flags;
            if f & 6 == 0 {
                continue;
            }
            let pl = sm.polygon_plane(p);
            let n = Vec3::new(pl[0], pl[1], pl[2]);
            if n.z.abs() > 0.3 {
                continue;
            }
            let pts: Vec<Vec3> = sm.polygon_verts(p).map(|v| Vec3::from_array(sm.verts[v])).collect();
            let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            faces.push((c, n, if f & 2 != 0 { 1 } else { 2 }));
        }
    }
    faces.sort_by(|a, b| (a.0.x, a.0.y).partial_cmp(&(b.0.x, b.0.y)).unwrap());
    let step = (faces.len() / 150).max(1);
    let mut seen = [0; 3];
    let mut grabs = 0;
    for &(c, n, _) in faces.iter().step_by(step) {
        let probe = c + Vec3::new(n.x, n.y, 0.0).normalize_or_zero() * 64.0;
        let Some((d, _, _)) = w.ray(probe, -Vec3::Z, 400.0) else { continue };
        let foot = probe - Vec3::Z * d + Vec3::Z * 2.0;
        let mut p = Player::new(inst.movement.clone(), foot);
        if !w.position_clear(&p.physics.normal_shape, foot) {
            continue;
        }
        p.view_angles[1] = (-n.y).atan2(-n.x).to_degrees();
        for _ in 0..20 {
            p.think(&w, UserCmd::default(), 16);
        }
        for _ in 0..60 {
            let before = p.ledge.state;
            p.think(&w, UserCmd { forward: 127, ..Default::default() }, 16);
            if before == ledge_state::NONE && p.ledge.state != ledge_state::NONE {
                grabs += 1;
                seen[p.ledge.info[0].edge_type.clamp(0, 2) as usize] += 1;
                break;
            }
        }
    }
    println!("{} flagged faces, {grabs} grabs, edge types square/rounded/angled {seen:?}", faces.len());
}
