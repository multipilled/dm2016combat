//! Parses every `.bmodel`, `.bcm` and `.entities` resource of the user's install with idres and
//! reports failures and statistics (validation for gamedata/re/MAPS.md).
//! `cargo run --release -p idres --example mapcheck [filter]`

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Result;
use idres::{Container, bcm, bmodel, entities};

fn main() -> Result<()> {
    let filter = std::env::args().nth(1).unwrap_or_default();
    let doom = idres::find_install().expect("DOOM install not found");
    let c = Container::open(&doom.join("base"), "gameresources")?;
    let mut names: Vec<_> = c.live_entries().map(|e| e.full_name.clone()).filter(|n| n.contains(&filter)).collect();
    names.sort();

    let (mut ok, mut fail) = (BTreeMap::<&str, usize>::new(), BTreeMap::<&str, usize>::new());
    let mut errs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut verts, mut tris, mut polys) = (0usize, 0usize, 0usize);
    let mut bcm_versions = BTreeMap::new();
    let mut streamed = 0;
    let mut vmtr_surfaces = 0usize;
    for n in &names {
        let kind = if n.ends_with(".bmodel") {
            "bmodel"
        } else if n.ends_with(".bcm") {
            "bcm"
        } else if n.ends_with(".entities") {
            "entities"
        } else {
            continue;
        };
        let bytes = match c.read_by_name(n) {
            Ok(b) => b,
            Err(e) => {
                *fail.entry(kind).or_default() += 1;
                errs.entry(format!("{kind}: read: {e:#}")).or_default().push(n.clone());
                continue;
            }
        };
        let t = Instant::now();
        let r: Result<String> = match kind {
            "bmodel" => bmodel::StaticModel::parse(&bytes).map(|m| {
                for s in &m.surfaces {
                    verts += s.verts.len();
                    tris += s.indices.len() / 3;
                    vmtr_surfaces += (!s.vmtrs.is_empty()) as usize;
                }
                format!("{} surfaces", m.surfaces.len())
            }),
            "bcm" => bcm::CollisionModel::parse(&bytes).map(|m| {
                polys += m.polygon_count();
                *bcm_versions.entry(m.version as char).or_insert(0) += 1;
                streamed += (m.flags[3] != 0) as usize;
                format!("{} submodels {} polygons", m.submodels.len(), m.polygon_count())
            }),
            _ => entities::parse(&String::from_utf8_lossy(&bytes)).map(|f| {
                let start = f.initial_player_start().map(|e| format!("{} at {:?}", e.name, e.origin())).unwrap_or_default();
                format!("{} entities, start {start}", f.entities.len())
            }),
        };
        match r {
            Ok(s) => {
                *ok.entry(kind).or_default() += 1;
                if bytes.len() > 20 << 20 || kind == "entities" && n.contains(&filter) && !filter.is_empty() {
                    println!("{n}: {s} ({:.2}s, {} MB)", t.elapsed().as_secs_f32(), bytes.len() >> 20);
                }
            }
            Err(e) => {
                *fail.entry(kind).or_default() += 1;
                let msg = format!("{e:#}");
                let key = format!("{kind}: {}", msg.split(':').next_back().unwrap_or(&msg).trim());
                errs.entry(key).or_default().push(format!("{n}: {msg}"));
            }
        }
    }
    println!("ok {ok:?}\nfail {fail:?}");
    println!("bmodel: {verts} verts, {tris} tris, {vmtr_surfaces} surfaces with per-vertex VT materials");
    println!("bcm: versions {bcm_versions:?}, {streamed} streamed, {polys} polygons");
    for (k, v) in &errs {
        println!("{} x {k}\n   e.g. {}", v.len(), v[0]);
    }
    Ok(())
}
