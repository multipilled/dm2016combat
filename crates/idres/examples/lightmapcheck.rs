//! Checks that `_world.bmodel` lightmap texcoords (`st` of surfaces in the map's unique virtual
//! texture) land on stored pages of `virtualtextures/maps/<map>.pages`, near a point of interest.
//! `cargo run --release -p idres --example lightmapcheck -- game/sp/intro/intro -17812 -1985 3072 [radius] [level]`

use anyhow::Result;
use idres::{Container, bmodel, vtex::unique::UniqueVt};

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let map = a.get(1).map(String::as_str).unwrap_or("game/sp/intro/intro");
    let p: Vec<f32> = a.iter().skip(2).take(3).filter_map(|v| v.parse().ok()).collect();
    let p = if p.len() == 3 { [p[0], p[1], p[2]] } else { [-17812.0, -1985.0, 3072.0] };
    let radius: f32 = a.get(5).and_then(|v| v.parse().ok()).unwrap_or(512.0);
    let level: usize = a.get(6).and_then(|v| v.parse().ok()).unwrap_or(3);
    let doom = idres::find_install().expect("DOOM install not found");
    let c = Container::open(&doom.join("base"), "gameresources")?;
    let u = UniqueVt::open(&doom, &format!("maps/{map}"))?;
    let pages = u.level_pages(level);
    println!("unique VT: {} pages wide at level 0, {} levels; level {level}: {pages} pages wide", u.pages_wide, u.levels);
    let bytes = c.read_by_name(&format!("maps/{map}/_combo/_world.bmodel"))?;
    let mut rd = bmodel::SurfaceReader::new(&bytes)?;
    let (mut present, mut missing) = (0usize, 0usize);
    let (mut smin, mut smax) = ([f32::MAX; 2], [f32::MIN; 2]);
    let mut shown = 0;
    while let Some(s) = rd.next_surface()? {
        if !s.material.ends_with("/mega.decl") && !s.material.ends_with("/megatrans.decl") {
            continue;
        }
        for v in &s.verts {
            let d = ((v.xyz[0] - p[0]).powi(2) + (v.xyz[1] - p[1]).powi(2) + (v.xyz[2] - p[2]).powi(2)).sqrt();
            if d > radius {
                continue;
            }
            for k in 0..2 {
                smin[k] = smin[k].min(v.st[k]);
                smax[k] = smax[k].max(v.st[k]);
            }
            let (px, py) = ((v.st[0] * pages as f32) as i64, (v.st[1] * pages as f32) as i64);
            let ok = px >= 0 && py >= 0 && (px as u32) < pages && (py as u32) < pages && u.page(level, px as u32, py as u32).is_some();
            if ok { present += 1 } else { missing += 1 }
            if shown < 8 {
                shown += 1;
                println!("  vert {:?} st {:?} -> page ({px}, {py}) {}", v.xyz, v.st, if ok { "present" } else { "MISSING" });
            }
        }
    }
    println!("verts within {radius}: {present} on stored pages, {missing} not; st range {smin:?}..{smax:?}");
    Ok(())
}
