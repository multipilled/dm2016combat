//! fxprobe: load an FX decl from the user's install and print its actions, particle stages and a
//! simulated frame. `fxprobe <fx decl> [condition] [time ms ...]`
//! e.g. `fxprobe fx/weapons/combatshotgun/combatshotgun_1st FX_WEAPON_START_FIRE 0 16 50 100`

use std::sync::Arc;

use anyhow::{Context, Result};
use glam::{Vec3, Vec4};
use idfx::fx::{FxDecl, FxManager, TagSource};
use idfx::sim::{Frame, View};
use idfx::{Axis, fx};
use idres::Container;
use idres::decldb::DeclDb;

struct Tags;
impl TagSource for Tags {
    fn tag(&self, _name: &str) -> Option<(Vec3, Axis)> {
        Some((Vec3::new(30.0, -8.0, -6.0), Axis::IDENTITY))
    }
    fn parent(&self) -> (Vec3, Axis) {
        (Vec3::ZERO, Axis::IDENTITY)
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = args.first().context("usage: fxprobe <fx decl> [condition] [times...]")?;
    let doom = idres::find_install().context("DOOM install not found")?;
    let db = DeclDb::new(Arc::new(Container::open(&doom.join("base"), "gameresources")?));
    if let Some(pn) = name.strip_prefix("particle:") {
        return probe_particle(&db, pn, &args[1..]);
    }
    if let Some(en) = name.strip_prefix("shell:") {
        return probe_shell(&db, en);
    }
    let decl = FxDecl::load(&db, name)?;
    for (i, a) in decl.actions.iter().enumerate() {
        println!(
            "[{i}] {} type {} dur {} fade {}/{} loop {} tags {:?} start {:?} extra {:?} org {} rot {} prt {}",
            a.name,
            idfx::names::ACTION_TYPES.get(a.kind).unwrap_or(&"?"),
            a.duration,
            a.fade_in,
            a.fade_out,
            a.looping,
            a.tags,
            a.start.iter().map(|c| idfx::names::CONDITIONS[*c as usize]).collect::<Vec<_>>(),
            a.extra,
            a.origin_type,
            a.rotation_type,
            a.particle_name
        );
        if let Some(p) = &a.particle {
            for (si, s) in p.stages.iter().enumerate() {
                println!(
                    "     stage {si} {:20} gpu {} n {} cyc {} life {:.3}/{:?} cycle {}ms bunch {:.3} dist {} ori {} mat {}",
                    s.name, s.gpu, s.total_particles, s.cycles, s.max_particle_life, s.particle_life.calc, s.cycle_msec, s.bunch_time, s.dist_type, s.orientation, s.material
                );
            }
        }
    }
    let cond = args.get(1).map(String::as_str).unwrap_or("FX_WEAPON_START_FIRE");
    let times: Vec<i32> = args.iter().skip(2).filter_map(|t| t.parse().ok()).collect();
    let mut mgr = FxManager::new(decl, 0);
    let c = fx::condition(cond).context("unknown condition")?;
    let base = 10_000;
    println!("condition {cond} = {c}: started {}", mgr.condition(c, fx::EXTRA_PRIMARY_FIRE, base, &Tags));
    let mut prev = base;
    for t in times {
        let now = base + t;
        mgr.update(now, &Tags);
        let view = View { origin: Vec3::ZERO, right: Vec3::new(0.0, -1.0, 0.0), up: Vec3::Z };
        for l in mgr.lights(now) {
            println!("  t={t}ms light action {} at {:?} color {:?} intensity {} radius {:?} fade {:.3}", l.action, l.origin, l.color, l.intensity, l.radius, l.fade);
        }
        for (ai, sys, org, axis, color) in mgr.systems_mut() {
            let frame = Frame {
                time_ms: now,
                prev_time_ms: prev,
                origin: org,
                axis,
                view,
                entity_color: color,
                fade: 1.0,
                size_scale: 1.0,
                wind: Vec3::ZERO,
                shadow: 1.0,
                velocity: Vec3::ZERO,
            };
            let mut quads = Vec::new();
            sys.generate(&frame, &mut quads);
            println!("  t={t}ms action {ai}: {} quads", quads.len());
            for q in quads.iter().take(12) {
                let c = (q.verts[0].pos + q.verts[1].pos + q.verts[2].pos + q.verts[3].pos) * 0.25;
                let w = (q.verts[1].pos - q.verts[0].pos).length();
                let h = (q.verts[0].pos - q.verts[2].pos).length();
                println!("     stage {} centre ({:6.1} {:6.1} {:6.1}) {:5.1}x{:5.1} rgba {:?} uv0 {:?} uv3 {:?}", q.stage, c.x, c.y, c.z, w, h, q.verts[0].color, q.verts[0].uv, q.verts[3].uv);
            }
        }
        prev = now;
    }
    let _ = Vec4::ONE;
    Ok(())
}

/// `fxprobe particle:<decl> [times ms...]`: one system at the origin, axis z = +x (an impact facing the viewer
/// standing 64 units away on +x), printing each stage's quads.
fn probe_particle(db: &DeclDb, name: &str, times: &[String]) -> Result<()> {
    let p = idfx::particle::ParticleDecl::load(db, name)?;
    for (si, s) in p.stages.iter().enumerate() {
        println!(
            "stage {si} {:24} gpu {} n {} cyc {} life {:.3} dist {} {:?} ori {} dir {} speed {:?} size {:?} aspect {:?} grav {:?} mat {}",
            s.name, s.gpu, s.total_particles, s.cycles, s.max_particle_life, s.dist_type, s.dist_size.map(|d| (d.val0, d.val1)), s.orientation, s.dir_type,
            s.speed.map(|d| (d.val0, d.val1, d.calc)), s.size[0], s.aspect, (s.gravity.val0, s.gravity.val1, s.gravity_world), s.material
        );
    }
    for s in p.stages.iter().filter(|s| s.gpu) {
        println!("gpu {} {:#?}", s.name, s.gpu_stage);
    }
    let mut sys = idfx::sim::System::new(p.clone(), 10_000, 1234);
    let axis = idfx::impact::normal_axis(Vec3::X);
    let mut prev = 10_000;
    for t in times.iter().filter_map(|t| t.parse::<i32>().ok()) {
        let now = 10_000 + t;
        let frame = Frame {
            time_ms: now,
            prev_time_ms: prev,
            origin: Vec3::ZERO,
            axis,
            view: View { origin: Vec3::new(64.0, 0.0, 0.0), right: Vec3::new(0.0, 1.0, 0.0), up: Vec3::Z },
            entity_color: Vec4::ONE,
            fade: 1.0,
            size_scale: 1.0,
            wind: Vec3::ZERO,
            shadow: 1.0,
            velocity: Vec3::ZERO,
        };
        let mut quads = Vec::new();
        sys.generate(&frame, &mut quads);
        println!("t={t}ms {} quads", quads.len());
        for q in &quads {
            let c = (q.verts[0].pos + q.verts[1].pos + q.verts[2].pos + q.verts[3].pos) * 0.25;
            let w = (q.verts[1].pos - q.verts[0].pos).length();
            let h = (q.verts[0].pos - q.verts[2].pos).length();
            match q.verts[0].color_f {
                Some(cf) => println!("   stage {} ({:6.1} {:6.1} {:6.1}) {:6.2}x{:6.2} linear {:.3?} uv {:?}", q.stage, c.x, c.y, c.z, w, h, cf, q.verts[0].uv),
                None => println!("   stage {} ({:6.1} {:6.1} {:6.1}) {:6.1}x{:6.1} rgba {:?}", q.stage, c.x, c.y, c.z, w, h, q.verts[0].color),
            }
        }
        prev = now;
    }
    Ok(())
}

/// `fxprobe shell:<piece emitter entityDef>`: decl, piece properties and one throw onto a floor at z = 0.
fn probe_shell(db: &DeclDb, name: &str) -> Result<()> {
    struct Floor;
    impl idfx::pieces::PieceWorld for Floor {
        fn trace_point(&self, start: Vec3, end: Vec3, _: u32) -> idfx::pieces::PieceTrace {
            if end.z >= 0.0 || start.z < 0.0 {
                return idfx::pieces::PieceTrace { fraction: 1.0, endpos: end, point: end, normal: Vec3::Z, surface: 0 };
            }
            let f = start.z / (start.z - end.z);
            let p = start + (end - start) * f;
            idfx::pieces::PieceTrace { fraction: f, endpos: p, point: p, normal: Vec3::Z, surface: 0 }
        }
    }
    let mut e = idfx::pieces::PieceEmitter::load(db, name)?;
    println!("{:?}", e.def);
    println!("{:?}", e.decl);
    let p = e.pieces[0].body.props.clone();
    println!("mass {} inertia {:?} inverse {:?}", p.mass, p.inertia, p.inverse_inertia);
    let mut rng = idfx::IdRandom(1234);
    let axis = Axis { x: Vec3::new(0.0, -1.0, 0.0), y: Vec3::X, z: Vec3::Z };
    e.emit(1000, &mut rng, Vec3::new(0.0, 0.0, 50.0), &axis, Vec3::ZERO, 150.0, 12.0, 5.0);
    for f in 0..240 {
        let t = 1000 + f * 16;
        e.update(t, 16, &Floor);
        let b = &e.pieces[0].body;
        if f % 12 == 0 || e.pieces[0].collision.normal_velocity != Vec3::ZERO {
            println!("t {:4} pos ({:7.2} {:7.2} {:6.2}) v ({:7.1} {:7.1} {:7.1}) col {:?} settled {}", t - 1000, b.position.x, b.position.y, b.position.z,
                b.linear_momentum.x / p.mass, b.linear_momentum.y / p.mass, b.linear_momentum.z / p.mass, e.pieces[0].collision.normal_velocity, b.settled);
        }
    }
    Ok(())
}
