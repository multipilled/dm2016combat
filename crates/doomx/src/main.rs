//! `doomx`: inspect and extract resources from the user's own DOOM (2016) install.
//! Extracted files go to a local, git-ignored folder and are never redistributed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use idres::{Container, Entry, crypt};

#[derive(Parser)]
struct Cli {
    /// DOOM install folder (defaults to $DOOM_DIR or known Steam paths)
    #[arg(long, global = true)]
    doom: Option<PathBuf>,
    /// Container name inside base/ (gameresources or snap_gameresources)
    #[arg(long, global = true, default_value = "gameresources")]
    container: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Resource counts and sizes per type
    Stats,
    /// List live resources whose full name contains every filter word
    List {
        filters: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Print one resource to stdout (decrypting binaryFiles)
    Cat { name: String },
    /// Show every index entry with this full name and the head of its raw bytes
    Probe { name: String },
    /// Print cvar defaults recovered from the exe (optionally filtered by prefix)
    Cvars { prefix: Option<String> },
    /// Texture header statistics: per format byte, bytes per pixel of mip 0 and an example
    ImgStats,
    /// Parse every .bmd6model (or those matching the filters) and report failures / a summary
    Models { filters: Vec<String>, #[arg(long)] verbose: bool },
    /// Print a decl with its inherit chain resolved (e.g. `decl weapon weapon/zion/player/sp/shotgun`)
    Decl { kind: String, name: String },
    /// Parse every .bmd6anim matching the filters (validates decoding; --verbose prints channels)
    Anims { filters: Vec<String>, #[arg(long)] verbose: bool },
    /// Parse every animWeb decl and every md6Def's events (or those matching the filters); --verbose
    /// prints a summary of each web, and checks that every referenced anim exists
    AnimWebs { filters: Vec<String>, #[arg(long)] verbose: bool },
    /// Skin statistics of one .bmd6model: per mesh, how vertices use the 4 palette bytes and weight bytes
    Md6Skin { name: String, #[arg(long, default_value_t = 0)] dump: usize },
    /// Parse every .bmd6skl matching the filters
    Skels { filters: Vec<String>, #[arg(long)] verbose: bool },
    /// Decode virtual-texture pages of one .mega2 file to PNGs (first N present slots, or one slot)
    VtPage {
        /// File name inside virtualtextures/ (e.g. _vmtr_sq16.mega2)
        file: String,
        #[arg(long)]
        slot: Option<usize>,
        #[arg(long, default_value_t = 4)]
        count: usize,
        #[arg(long, default_value_t = 1)]
        overlap: u8,
        #[arg(long, default_value = "gamedata/vt")]
        out: PathBuf,
    },
    /// Check page ordering: border continuity of two horizontally adjacent pages (level, page x, page y)
    VtOrder { file: String, level: usize, px: u32, py: u32 },
    /// Assemble a material's virtual-texture layers at a mip level into PNGs
    VtMaterial {
        material: String,
        #[arg(long, default_value_t = 2)]
        level: usize,
        #[arg(long, default_value = "gamedata/vt")]
        out: PathBuf,
    },
    /// Extract matching resources into a folder, keeping their paths
    Extract {
        filters: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value = "extracted")]
        out: PathBuf,
    },
}

fn matches(e: &Entry, filters: &[String], kind: &Option<String>) -> bool {
    kind.as_ref().is_none_or(|k| e.kind.eq_ignore_ascii_case(k))
        && filters.iter().all(|f| e.full_name.to_ascii_lowercase().contains(&f.to_ascii_lowercase()))
}

fn payload(c: &Container, e: &Entry) -> Result<Vec<u8>> {
    let bytes = c.read(e)?;
    if e.kind == "binaryFile" {
        return crypt::decrypt(&bytes, &e.short_name).with_context(|| format!("decrypting {}", e.full_name));
    }
    Ok(bytes)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let doom = cli.doom.or_else(idres::find_install).context("DOOM install not found; pass --doom or set DOOM_DIR")?;
    if let Cmd::Cvars { prefix } = &cli.cmd {
        let table = idres::exe::scan_cvars(&doom.join("DOOMx64.exe"))?;
        let mut names: Vec<_> = table.cvars.iter().filter(|(n, _)| prefix.as_ref().is_none_or(|p| n.starts_with(p.as_str()))).collect();
        names.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        println!("{} cvars", table.cvars.len());
        for (n, c) in names {
            println!("{n}	{}	{:#x}", c.default, c.object_va);
        }
        return Ok(());
    }
    if let Cmd::VtMaterial { material, level, out } = &cli.cmd {
        let t0 = std::time::Instant::now();
        let vt = idres::vtex::VirtualTexture::open(&doom)?;
        let r = vt.rect(material).with_context(|| format!("{material} not in the vmtr tables"))?;
        let img = vt.assemble(r, *level)?;
        println!("{material}: {r:?} -> {}x{} at level {level}, {} pages missing, {:.2}s", img.width, img.height, img.missing_pages, t0.elapsed().as_secs_f32());
        std::fs::create_dir_all(out)?;
        let base = material.replace('/', "_");
        for (i, l) in img.layers.iter().enumerate() {
            write_png(&out.join(format!("{base}_L{level}_img{i}.png")), img.width, img.height, png::ColorType::Rgba, l)?;
        }
        write_png(&out.join(format!("{base}_L{level}_lz.png")), img.width, img.height, png::ColorType::Grayscale, &img.lz)?;
        write_png(&out.join(format!("{base}_L{level}_cover.png")), img.width, img.height, png::ColorType::Grayscale, &img.cover)?;
        return Ok(());
    }
    if let Cmd::VtOrder { file, level, px, py } = &cli.cmd {
        let m = idres::vt::Mega2::open(&doom.join("virtualtextures").join(file))?;
        let l = m.levels[*level];
        type Ix = fn(&idres::vt::Level, u32, u32) -> u64;
        let row: Ix = |l, x, y| l.first as u64 + ((y - l.page_y) * l.width + (x - l.page_x)) as u64;
        let mor: Ix = |l, x, y| l.first as u64 + morton(x - l.page_x, y - l.page_y);
        for (name, idx) in [("row-major", row), ("morton", mor)] {
            let a = m.slot(idx(&l, *px, *py) as usize).and_then(|s| m.page(s)).map(|b| idres::vt::decode_page(b, Default::default()));
            let b = m.slot(idx(&l, *px + 1, *py) as usize).and_then(|s| m.page(s)).map(|b| idres::vt::decode_page(b, Default::default()));
            match (a, b) {
                (Some(Ok(a)), Some(Ok(b))) => {
                    let (ia, ib) = (a.images[0].as_ref().unwrap(), b.images[0].as_ref().unwrap());
                    let mut err = 0f64;
                    for y in 8..120 {
                        for k in 0..4 {
                            for c in 0..3 {
                                let va = ia[(y * 128 + 124 + k) * 4 + c] as f64;
                                let vb = ib[(y * 128 + 4 + k) * 4 + c] as f64;
                                err += (va - vb) * (va - vb);
                            }
                        }
                    }
                    println!("{name}: border mse {:.2}", err / (112.0 * 4.0 * 3.0));
                }
                _ => println!("{name}: page missing"),
            }
        }
        return Ok(());
    }
    if let Cmd::VtPage { file, slot, count, overlap, out } = &cli.cmd {
        return vt_page(&doom, file, *slot, *count, idres::vt::HdpOptions { overlap: *overlap }, out);
    }
    let c = Container::open(&doom.join("base"), &cli.container)?;

    match cli.cmd {
        Cmd::Stats => {
            let mut by_kind: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
            for e in c.live_entries() {
                let s = by_kind.entry(&e.kind).or_default();
                s.0 += 1;
                s.1 += e.size as u64;
            }
            println!("{} entries ({} live)", c.entries.len(), c.live_entries().count());
            for (k, (n, bytes)) in by_kind {
                println!("{k:28} {n:7} {:10.1} MB", bytes as f64 / 1e6);
            }
        }
        Cmd::List { filters, kind } => {
            let mut hits: Vec<_> = c.live_entries().filter(|e| matches(e, &filters, &kind)).collect();
            hits.sort_by(|a, b| a.full_name.cmp(&b.full_name));
            for e in hits {
                println!("{:22} {:9} p{} {}", e.kind, e.size, e.patch, e.full_name);
            }
        }
        Cmd::Cat { name } => {
            let e = c.get(&name).with_context(|| format!("no resource named {name}"))?;
            use std::io::Write;
            std::io::stdout().write_all(&payload(&c, e)?)?;
        }
        Cmd::Cvars { .. } | Cmd::VtPage { .. } | Cmd::VtOrder { .. } | Cmd::VtMaterial { .. } => unreachable!(),
        Cmd::Models { filters, verbose } => {
            let (mut ok, mut bad) = (0, 0);
            for e in c.live_entries().filter(|e| e.full_name.ends_with(".bmd6model") && matches(e, &filters, &None)) {
                let bytes = c.read(e)?;
                match idres::md6::Md6Model::parse(&bytes) {
                    Ok(m) => {
                        ok += 1;
                        if verbose {
                            println!("{} skel={} joints={} meshes={}", e.full_name, m.skeleton, m.joint_remap.len(), m.meshes.len());
                            for me in &m.meshes {
                                println!("   {:24} {:60} tc={} fl={} v={} t={} trailer={:?} {}", me.name, me.material, me.texcoord_sets, me.flags, me.verts.len(), me.indices.len() / 3, me.trailer, me.trailer_flag);
                                if let (Some(a), Some(b)) = (me.verts.first(), me.verts.get(me.verts.len() / 2)) {
                                    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                                    for v in &me.verts { for k in 0..2 { lo[k] = lo[k].min(v.st[k]); hi[k] = hi[k].max(v.st[k]); } }
                                    println!("      st range {:?}..{:?}  v0 st {:?} extra {:02x?}  vmid extra {:02x?}", lo, hi, a.st, a.extra, b.extra);
                                }
                            }
                        }
                    }
                    Err(err) => {
                        if bad < 8 { println!("FAIL {}: {err:#}", e.full_name); }
                        bad += 1;
                    }
                }
            }
            println!("parsed {ok}, failed {bad}");
        }
        Cmd::Skels { filters, verbose } => {
            let (mut ok, mut bad) = (0, 0);
            for e in c.live_entries().filter(|e| e.full_name.ends_with(".bmd6skl") && matches(e, &filters, &None)) {
                match idres::md6::Md6Skel::parse(&c.read(e)?) {
                    Ok(s) => {
                        ok += 1;
                        if verbose {
                            println!("{} joints={}", e.full_name, s.names.len());
                            for j in 0..s.names.len() {
                                println!("   {j:3} {:28} parent {:3} t {:?} q {:?}", s.names[j], s.parents[j], s.translations[j], s.rotations[j]);
                            }
                        }
                    }
                    Err(err) => {
                        if bad < 6 { println!("FAIL {}: {err:#}", e.full_name); }
                        bad += 1;
                    }
                }
            }
            println!("parsed {ok}, failed {bad}");
        }
        Cmd::Decl { kind, name } => {
            let db = idres::decldb::DeclDb::new(std::sync::Arc::new(c));
            let b = db.get(&kind, &name)?;
            fn dump(b: &idres::decl::Block, depth: usize) {
                for (k, v) in &b.items {
                    let pad = "  ".repeat(depth);
                    match v {
                        idres::decl::Value::Block(inner) => {
                            println!("{pad}{k} {{");
                            dump(inner, depth + 1);
                            println!("{pad}}}");
                        }
                        idres::decl::Value::Str(s) => println!("{pad}{k} = \"{s}\""),
                        idres::decl::Value::Atom(s) => println!("{pad}{k} = {s}"),
                    }
                }
            }
            dump(&b, 0);
            return Ok(());
        }
        Cmd::Md6Skin { name, dump } => {
            let e = c.live_entries().find(|e| e.full_name.ends_with(".bmd6model") && e.full_name.contains(&name)).context("no such model")?;
            let m = idres::md6::Md6Model::parse(&c.read(e)?)?;
            println!("{} joint_remap ({}): {:?}", e.full_name, m.joint_remap.len(), m.joint_remap);
            for me in &m.meshes {
                let n = me.verts.len().max(1);
                let distinct = |v: &idres::md6::DrawVert| {
                    let mut j: Vec<u8> = v.color.to_vec();
                    j.sort();
                    j.dedup();
                    j.len()
                };
                let mut hist = [0usize; 5];
                let (mut sum_n, mut sum_t, mut sum_x) = (0usize, 0usize, 0usize);
                for v in &me.verts {
                    hist[distinct(v)] += 1;
                    if v.normal[3] != 0 { sum_n += 1; }
                    if v.tangent[3] & 0x7f != 0 { sum_t += 1; }
                    if v.extra[..4].iter().any(|&b| b != 0) { sum_x += 1; }
                }
                println!("  {:24} verts={} distinct-joints hist={:?} normal[3]!=0:{} tangent[3]&7f!=0:{} extra[0..4]!=0:{}", me.name, n, &hist[1..], sum_n, sum_t, sum_x);
                for v in me.verts.iter().filter(|v| distinct(v) >= 3).take(dump) {
                    println!("     color={:?} n3={} t3={} extra={:?}", v.color, v.normal[3], v.tangent[3], v.extra);
                }
            }
        }
        Cmd::AnimWebs { filters, verbose } => {
            let (mut ok, mut bad, mut missing) = (0, 0, 0);
            for e in c.live_entries().filter(|e| e.kind == "animWeb" && matches(e, &filters, &None)) {
                let text = String::from_utf8_lossy(&c.read(e)?).into_owned();
                match idres::animweb::AnimWeb::parse(&text) {
                    Ok(w) => {
                        ok += 1;
                        let nodes: usize = w.sub_webs.iter().map(|s| s.nodes.len()).sum();
                        if verbose {
                            println!("{} models={} states={} scalars={} subwebs={} nodes={}", e.full_name, w.model_infos.len(), w.states.len(), w.scalars.len(), w.sub_webs.len(), nodes);
                        }
                        for s in &w.sub_webs {
                            for n in &s.nodes {
                                for t in &n.trees {
                                    for a in &t.anims {
                                        if !a.name.is_empty() && c.get(&idres::animweb::anim_resource(&a.name)).is_none() {
                                            missing += 1;
                                            if verbose || missing <= 10 {
                                                println!("   missing anim {} ({}/{})", a.name, s.name, n.state);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        bad += 1;
                        if bad <= 10 {
                            println!("FAIL {}: {err:#}", e.full_name);
                        }
                    }
                }
            }
            println!("animWeb: {ok} ok, {bad} failed, {missing} missing anim refs");
            let (mut ok, mut bad, mut events) = (0, 0, 0);
            for e in c.live_entries().filter(|e| e.kind == "md6Def" && matches(e, &filters, &None)) {
                let text = String::from_utf8_lossy(&c.read(e)?).into_owned();
                match idres::md6def::Md6DefDecl::parse(&text) {
                    Ok(d) => {
                        ok += 1;
                        events += d.events.values().map(Vec::len).sum::<usize>();
                    }
                    Err(err) => {
                        bad += 1;
                        if bad <= 10 {
                            println!("FAIL {}: {err:#}", e.full_name);
                        }
                    }
                }
            }
            println!("md6Def: {ok} ok, {bad} failed, {events} events");
        }
        Cmd::Anims { filters, verbose } => {
            let (mut ok, mut bad, mut nonunit) = (0, 0, 0);
            for e in c.live_entries().filter(|e| e.full_name.ends_with(".bmd6anim") && matches(e, &filters, &None)) {
                match idres::md6anim::Md6Anim::parse(&c.read(e)?) {
                    Ok(a) => {
                        ok += 1;
                        for ch in &a.rot.keys {
                            for (_, q) in ch {
                                let l = q.iter().map(|v| v * v).sum::<f32>();
                                if (l - 1.0).abs() > 1e-3 { nonunit += 1; }
                            }
                        }
                        if verbose {
                            println!("{} flags={:#06x} frames={} rate={} constR={} constT={} animR={} animT={} animS={} user={}", e.full_name, a.flags, a.num_frames, a.frame_rate, a.const_r.len(), a.const_t.len(), a.rot.joints.len(), a.trans.joints.len(), a.scale.joints.len(), a.user.joints.len());
                            for (ch, j) in a.rot.joints.iter().enumerate() {
                                let k: Vec<String> = a.rot.keys[ch].iter().map(|(f, q)| format!("{f}:[{:.3},{:.3},{:.3},{:.3}]", q[0], q[1], q[2], q[3])).collect();
                                println!("   R j{j}: {}", k.join(" "));
                            }
                            for (ch, j) in a.trans.joints.iter().enumerate() {
                                let k: Vec<String> = a.trans.keys[ch].iter().map(|(f, v)| format!("{f}:[{:.2},{:.2},{:.2}]", v[0], v[1], v[2])).collect();
                                println!("   T j{j}: {}", k.join(" "));
                            }
                        }
                    }
                    Err(err) => {
                        if bad < 6 { println!("FAIL {}: {err:#}", e.full_name); }
                        bad += 1;
                    }
                }
            }
            println!("parsed {ok}, failed {bad}, non-unit quats {nonunit}");
        }
        Cmd::ImgStats => {
            let mut stats: BTreeMap<(u8, u32, String), (usize, String)> = BTreeMap::new();
            let mut bad = 0;
            for e in c.live_entries().filter(|e| e.full_name.ends_with(".bimage") && e.size > 0) {
                let bytes = match c.read(e) {
                    Ok(b) => b,
                    Err(err) => {
                        if bad < 3 { eprintln!("read {}: {err:#}", e.full_name); }
                        bad += 1;
                        continue;
                    }
                };
                match idres::bimage::BImage::parse(&bytes) {
                    Ok(img) => {
                        let m = &img.mips[0];
                        let px = (m.width.max(1) * m.height.max(1)) as f64;
                        let bpp = format!("{:.3}", m.data.len() as f64 / px);
                        let s = stats.entry((img.format, img.texture_type, bpp)).or_insert((0, String::new()));
                        s.0 += 1;
                        if s.1.is_empty() {
                            s.1 = format!("{}x{} tail={:02x?} {}", m.width, m.height, img.tail, e.full_name);
                        }
                    }
                    Err(err) => {
                        if bad < 6 { eprintln!("parse {} ({} bytes): {err:#}", e.full_name, bytes.len()); }
                        bad += 1;
                    }
                }
            }
            for ((fmt, tt, bpp), (n, ex)) in stats {
                println!("fmt {fmt:3} type {tt} bpp {bpp:>7} x{n:5}  {ex}");
            }
            println!("unparsed: {bad}");
        }
        Cmd::Probe { name } => {
            for e in c.entries.iter().filter(|e| e.full_name == name) {
                println!("{e:?}");
                match c.read_raw(e) {
                    Ok(raw) => println!("  raw head: {:02x?}", &raw[..raw.len().min(32)]),
                    Err(err) => println!("  raw read failed: {err:#}"),
                }
                match c.read(e) {
                    Ok(b) => println!("  ok {} bytes: {:?}", b.len(), String::from_utf8_lossy(&b[..b.len().min(80)])),
                    Err(err) => println!("  read failed: {err:#}"),
                }
            }
        }
        Cmd::Extract { filters, kind, out } => {
            let mut n = 0;
            for e in c.live_entries().filter(|e| matches(e, &filters, &kind) && e.size > 0) {
                let path = out.join(e.full_name.replace(':', "_"));
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, payload(&c, e)?)?;
                n += 1;
            }
            println!("extracted {n} resources to {}", out.display());
        }
    }
    Ok(())
}

fn write_png(path: &std::path::Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(f, w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(data)?;
    Ok(())
}

fn vt_page(doom: &std::path::Path, file: &str, slot: Option<usize>, count: usize, opt: idres::vt::HdpOptions, out: &std::path::Path) -> Result<()> {
    use idres::vt::{Mega2, PAGE, decode_page};
    let m = Mega2::open(&doom.join("virtualtextures").join(file))?;
    std::fs::create_dir_all(out)?;
    let slots: Vec<usize> = match slot { Some(s) => vec![s], None => (0..m.slot_count().min(count)).collect() };
    for s in slots {
        let data = m.page(s).context("slot out of range")?;
        match decode_page(data, opt) {
            Ok(p) => {
                println!("slot {s}: {:?}", p.header);
                for (i, img) in p.images.iter().enumerate() {
                    if let Some(img) = img {
                        write_png(&out.join(format!("{s}_img{i}.png")), PAGE as u32, PAGE as u32, png::ColorType::Rgba, img)?;
                    }
                }
                if let Some(l) = &p.lz_plane {
                    write_png(&out.join(format!("{s}_lz.png")), PAGE as u32, PAGE as u32, png::ColorType::Grayscale, l)?;
                }
                if let Some(cv) = &p.cover {
                    write_png(&out.join(format!("{s}_cover.png")), PAGE as u32, PAGE as u32, png::ColorType::Grayscale, cv)?;
                }
            }
            Err(e) => println!("slot {s}: {e:#}"),
        }
    }
    Ok(())
}

fn morton(x: u32, y: u32) -> u64 {
    let mut m = 0u64;
    for b in 0..16 {
        m |= (((x >> b) & 1) as u64) << (2 * b) | (((y >> b) & 1) as u64) << (2 * b + 1);
    }
    m
}
