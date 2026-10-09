//! sndx: inspect DOOM (2016)'s Wwise data in your own install and write decoded WAVs.
//!
//! ```text
//! sndx [--doom DIR] [--lang LANG|none] [--out DIR] [--set group=value]... <command> [args]
//!   summary                 banks, packages, object counts, parse errors
//!   events [FILTER]         event table entries (name, bus, duration, media count)
//!   tree NAME|ID            an event's play tree
//!   media NAME|ID           media an event can play (with --set: only those switch values)
//!   wav NAME|ID|#MEDIA...   decode media to WAVs in --out (default gamedata/audio)
//!   post NAME|ID [-n N]     simulate posting an event N times
//!   check [--limit N]       decode every media file and sanity-check it; writes check.tsv
//!   map                     player/weapon sound mapping from the decls; writes sound_map.tsv
//!   render OUT.wav ITEM...  mix events offline through idaudio::Engine (never plays aloud).
//!                           ITEM = NAME@SECONDS[@X,Y,Z] (no position = the player emitter, first person;
//!                           listener at the origin facing +X, Z up); --env BUS:GUN_ENV sets an environment
//! ```
//! NAME may be a Wwise event name or any decl sound reference (normalised like the engine).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use idaudio::hirc::kind;
use idaudio::{AudioLibrary, Command, MediaLocation, PlayState, Playable, Switches, TreeNode, sound_event_id, sound_event_name};
use idres::decl::{Block, Value};

struct Args {
    doom: PathBuf,
    lang: Option<String>,
    out: PathBuf,
    sets: Vec<(String, String)>,
    n: usize,
    limit: usize,
    env: Option<String>,
    rest: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        doom: idres::find_install().unwrap_or_default(),
        lang: Some(idaudio::library::DEFAULT_LANGUAGE.to_owned()),
        out: PathBuf::from("gamedata/audio"),
        sets: Vec::new(),
        n: 8,
        limit: usize::MAX,
        env: None,
        rest: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--doom" => a.doom = PathBuf::from(val()?),
            "--lang" => a.lang = Some(val()?).filter(|l| l != "none"),
            "--out" => a.out = PathBuf::from(val()?),
            "--set" => {
                let v = val()?;
                let (g, s) = v.split_once('=').context("--set wants group=value")?;
                a.sets.push((g.to_owned(), s.to_owned()));
            }
            "-n" => a.n = val()?.parse()?,
            "--limit" => a.limit = val()?.parse()?,
            "--env" => a.env = Some(val()?),
            _ => a.rest.push(arg),
        }
    }
    if a.doom.as_os_str().is_empty() {
        bail!("DOOM install not found; pass --doom DIR or set DOOM_DIR");
    }
    Ok(a)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let Some(cmd) = args.rest.first().cloned() else {
        println!("usage: sndx [--doom DIR] [--lang LANG|none] [--out DIR] [--set g=v] summary|events|tree|media|wav|post|check|map ...");
        return Ok(());
    };
    let t = std::time::Instant::now();
    let lib = AudioLibrary::open_with(&args.doom, args.lang.as_deref())?;
    eprintln!("loaded {} banks, {} packages in {:.2}s", lib.banks.len(), lib.packages.len(), t.elapsed().as_secs_f32());
    let rest = &args.rest[1..];
    match cmd.as_str() {
        "summary" => summary(&lib),
        "events" => events(&lib, rest.first().map(String::as_str).unwrap_or("")),
        "tree" => {
            for name in rest {
                let id = event_arg(name);
                println!("== {name} -> {} ({id})", display_event(&lib, id));
                print_tree(&lib, &lib.event_tree(id)?, 0);
            }
            Ok(())
        }
        "media" => {
            let names = media_names(&lib);
            for name in rest {
                let id = event_arg(name);
                println!("== {name} -> {} ({id})", display_event(&lib, id));
                for m in resolve(&lib, id, &args.sets)? {
                    println!("  {}", describe_media(&lib, &names, m.media_id, m.sound_id));
                }
            }
            Ok(())
        }
        "wav" => wav(&lib, &args, rest),
        "post" => post(&lib, &args, rest),
        "check" => check(&lib, &args),
        "map" => map(&lib, &args),
        "render" => render(lib, &args, rest),
        _ => bail!("unknown command {cmd}"),
    }
}

fn event_arg(s: &str) -> u32 {
    s.parse().unwrap_or_else(|_| sound_event_id(s))
}

fn display_event(lib: &AudioLibrary, id: u32) -> String {
    lib.event_info(id).map_or_else(|| "(not in event table)".to_owned(), |e| e.name.clone())
}

fn switches(sets: &[(String, String)]) -> Switches {
    let mut s = Switches::default();
    for (g, v) in sets {
        s.set(g, v);
    }
    s
}

fn resolve(lib: &AudioLibrary, id: u32, sets: &[(String, String)]) -> Result<Vec<idaudio::MediaRef>> {
    if sets.is_empty() { lib.resolve_event(id) } else { lib.resolve_event_with(id, &switches(sets)) }
}

fn media_names(lib: &AudioLibrary) -> HashMap<u32, (String, String)> {
    idaudio::names::media_short_names(&lib.dir.join("soundbanksinfo.xml")).unwrap_or_default()
}

fn describe_media(lib: &AudioLibrary, names: &HashMap<u32, (String, String)>, media: u32, sound: u32) -> String {
    let locs: Vec<String> = lib
        .media_locations(media)
        .iter()
        .map(|l| {
            let bytes = lib.location_bytes(l);
            let trunc = idaudio::wem::parse(bytes).is_ok_and(|i| i.is_truncated());
            format!("{}{}", lib.location_name(l), if trunc { "(prefetch)" } else { "" })
        })
        .collect();
    let info = match lib.media_info(media) {
        Ok(i) => format!("{}ch {}Hz mask {:#x} {:.3}s", i.channels, i.sample_rate, i.channel_mask, i.duration_secs()),
        Err(e) => format!("({e:#})"),
    };
    let name = names.get(&media).map_or("", |(_, n)| n.as_str());
    format!("media {media:<10} sound {sound:<10} {info:<32} {name}  [{}]", locs.join(", "))
}

fn summary(lib: &AudioLibrary) -> Result<()> {
    println!("{:<36} {:>10} {:>6} {:>7} {:>7}", "bank", "id", "lang", "media", "objects");
    for b in &lib.banks {
        println!("{:<36} {:>10} {:>6} {:>7} {:>7}", b.name, b.id, b.language, b.media.len(), b.hirc.len());
    }
    println!();
    for p in &lib.packages {
        println!(
            "{:<36} langs {:?} banks {} streams {}",
            p.path.file_name().unwrap_or_default().to_string_lossy(),
            p.languages,
            p.banks.len(),
            p.streams.len()
        );
    }
    let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
    for (_, o) in lib.nodes() {
        *kinds.entry(o.kind()).or_default() += 1;
    }
    println!();
    println!("events in HIRC: {}, in soundbanksinfo.events: {}", lib.event_ids().count(), lib.events_table.events.len());
    for (k, n) in kinds {
        println!("  {:<12} {n}", kind::name(k));
    }
    println!("media ids: {}", lib.media_ids().count());
    println!("conflicting duplicate objects: {}", lib.conflicts.len());
    for (id, a, b) in lib.conflicts.iter().take(10) {
        println!("  {id}: {} vs {}", lib.banks[*a].name, lib.banks[*b].name);
    }
    println!("parse errors: {}", lib.parse_errors.len());
    for (b, id, e) in lib.parse_errors.iter().take(20) {
        println!("  {} {id}: {e}", lib.banks[*b].name);
    }
    let table: HashSet<u32> = lib.events_table.events.iter().map(|e| e.id).collect();
    let missing = lib.events_table.events.iter().filter(|e| lib.event(e.id).is_none()).count();
    let untabled = lib.event_ids().filter(|id| !table.contains(id)).count();
    println!("table events without HIRC event: {missing}; HIRC events not in table: {untabled}");
    println!("switch groups:");
    for g in &lib.events_table.switch_groups {
        println!("  {} = {:?}", g.name, g.values.iter().map(|v| v.0.as_str()).collect::<Vec<_>>());
    }
    Ok(())
}

fn events(lib: &AudioLibrary, filter: &str) -> Result<()> {
    let f = filter.to_lowercase();
    for e in &lib.events_table.events {
        if !e.name.to_lowercase().contains(&f) && !e.path.to_lowercase().contains(&f) {
            continue;
        }
        let n = lib.resolve_event(e.id).map_or(0, |m| m.len());
        println!(
            "{:<10} {:<56} {:<22} {:>7.3}-{:<7.3} media {:>3}  {}",
            e.id, e.name, e.bus, e.duration_min, e.duration_max, n, e.path
        );
    }
    Ok(())
}

fn print_tree(lib: &AudioLibrary, n: &TreeNode, depth: usize) {
    let sw = n.switch_value.map(|v| format!("[{}] ", lib.events_table.group_name(v).unwrap_or_else(|| v.to_string()))).unwrap_or_default();
    println!("{}{sw}{} {} {}", "  ".repeat(depth), kind::name(n.kind), n.id, n.label);
    for c in &n.children {
        print_tree(lib, c, depth + 1);
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' }).collect()
}

struct Stats {
    frames: usize,
    peak: i32,
    rms_db: f64,
    clipped: usize,
    dc: f64,
}

fn stats(d: &idaudio::Decoded) -> Stats {
    let (mut peak, mut sum2, mut sum, mut clipped) = (0i32, 0f64, 0f64, 0usize);
    for &s in &d.samples {
        let v = i32::from(s);
        peak = peak.max(v.abs());
        sum += f64::from(v);
        sum2 += f64::from(v) * f64::from(v);
        if s == i16::MAX || s == i16::MIN {
            clipped += 1;
        }
    }
    let n = d.samples.len().max(1) as f64;
    let rms = (sum2 / n).sqrt();
    Stats { frames: d.frames(), peak, rms_db: 20.0 * (rms.max(1e-9) / 32768.0).log10(), clipped, dc: sum / n }
}

fn fmt_stats(d: &idaudio::Decoded, s: &Stats) -> String {
    format!(
        "{}ch {}Hz {:>8} frames {:.3}s peak {:>6.1}dBFS rms {:>6.1}dBFS clipped {} dc {:.1}{}",
        d.channels,
        d.rate,
        s.frames,
        d.duration_secs(),
        20.0 * (f64::from(s.peak).max(1.0) / 32768.0).log10(),
        s.rms_db,
        s.clipped,
        s.dc,
        if d.truncated { " TRUNCATED" } else { "" }
    )
}

fn wav(lib: &AudioLibrary, args: &Args, items: &[String]) -> Result<()> {
    let names = media_names(lib);
    for item in items {
        let (dir, medias): (PathBuf, Vec<u32>) = if let Some(m) = item.strip_prefix('#') {
            (args.out.join("media"), vec![m.parse()?])
        } else {
            let id = event_arg(item);
            let ename = lib.event_info(id).map_or_else(|| sound_event_name(item), |e| e.name.clone());
            (args.out.join(sanitize(&ename)), resolve(lib, id, &args.sets)?.into_iter().map(|m| m.media_id).collect())
        };
        std::fs::create_dir_all(&dir)?;
        println!("== {item} -> {} ({} media)", dir.display(), medias.len());
        for m in medias {
            if lib.media_locations(m).is_empty() {
                println!("  {m}: no media (plugin source?)");
                continue;
            }
            let d = match lib.decode_media(m) {
                Ok(d) => d,
                Err(e) => {
                    println!("  {m}: {e:#}");
                    continue;
                }
            };
            let short = names.get(&m).map(|(_, n)| {
                let file = n.rsplit(['\\', '/']).next().unwrap_or(n);
                format!("_{}", sanitize(file.trim_end_matches(".wav")))
            });
            let path = dir.join(format!("{m}{}.wav", short.unwrap_or_default()));
            idaudio::wav::write(&path, d.rate, d.channels, d.channel_mask, &d.samples)?;
            let s = stats(&d);
            println!("  {}  {}", path.file_name().unwrap_or_default().to_string_lossy(), fmt_stats(&d, &s));
        }
    }
    Ok(())
}

fn describe_playable(lib: &AudioLibrary, p: &Playable, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match p {
        Playable::Voice(v) => {
            let _ = writeln!(
                out,
                "{pad}voice media {} vol {:+.1}dB pitch {:+.0}c lpf {:.0} hpf {:.0} delay {:.3}s loop {} bus {} {} {}({})",
                v.media,
                v.volume_db,
                v.pitch_cents,
                v.lpf,
                v.hpf,
                v.delay_s,
                v.loop_count,
                v.output_bus,
                lib.bus_name(v.output_bus).unwrap_or("?"),
                v.silence_s.map_or_else(String::new, |s| format!("silence {s:.3}s ")),
                lib.media_info(v.media).map_or_else(|_| "no media".into(), |i| format!("{:.3}s", i.duration_secs()))
            );
        }
        Playable::Together(v) => {
            let _ = writeln!(out, "{pad}together:");
            v.iter().for_each(|c| describe_playable(lib, c, depth + 1, out));
        }
        Playable::Sequence { container, loops, items, .. } => {
            let _ = writeln!(out, "{pad}sequence {container} loops {loops}:");
            items.iter().for_each(|c| describe_playable(lib, c, depth + 1, out));
        }
        Playable::Unsupported { id, kind: k } => {
            let _ = writeln!(out, "{pad}unsupported {} {id}", kind::name(*k));
        }
        Playable::Nothing => {
            let _ = writeln!(out, "{pad}nothing");
        }
    }
}

fn post(lib: &AudioLibrary, args: &Args, items: &[String]) -> Result<()> {
    let mut st = PlayState::new(0x5eed);
    for (g, v) in &args.sets {
        st.set_switch(g, v);
    }
    for item in items {
        let id = event_arg(item);
        println!("== {item} -> {} ({id})", display_event(lib, id));
        for i in 0..args.n {
            let mut s = String::new();
            for c in st.post_event(lib, id)? {
                match c {
                    Command::Play { delay_s, what, .. } => {
                        let _ = writeln!(s, "  play (action delay {delay_s:.3}s):");
                        describe_playable(lib, &what, 2, &mut s);
                    }
                    other => {
                        let _ = writeln!(s, "  {other:?}");
                    }
                }
            }
            print!("#{i}\n{s}");
        }
    }
    Ok(())
}

fn check(lib: &AudioLibrary, args: &Args) -> Result<()> {
    std::fs::create_dir_all(&args.out)?;
    let mut tsv = String::from("media\tlocation\tchannels\trate\tframes\theader_frames\tseconds\tpeak\trms_db\tclipped\tdc\tflags\n");
    let mut ids: Vec<u32> = lib.media_ids().collect();
    ids.sort_unstable();
    let (mut ok, mut bad, mut flagged) = (0usize, 0usize, 0usize);
    let mut codecs: BTreeMap<String, usize> = BTreeMap::new();
    let mut frame_mismatch = 0usize;
    let t = std::time::Instant::now();
    for &m in ids.iter().take(args.limit) {
        let bytes = lib.media_bytes(m)?;
        let loc = lib.media_locations(m).iter().find(|l| matches!(l, MediaLocation::Package { .. })).unwrap_or(&lib.media_locations(m)[0]);
        let info = match idaudio::wem::parse(bytes) {
            Ok(i) => i,
            Err(e) => {
                *codecs.entry(format!("not RIFF ({} bytes)", bytes.len())).or_default() += 1;
                let _ = writeln!(tsv, "{m}\t{}\t\t\t\t\t\t\t\t\t\tnot-riff: {e}", lib.location_name(loc));
                bad += 1;
                continue;
            }
        };
        *codecs.entry(format!("{:?} {}ch", info.codec, info.channels)).or_default() += 1;
        match idaudio::decode(bytes) {
            Ok(d) => {
                let s = stats(&d);
                let mut flags = Vec::new();
                if d.truncated {
                    flags.push("truncated");
                }
                if s.frames as u64 != info.total_frames() {
                    flags.push("frame-count");
                    frame_mismatch += 1;
                }
                if s.clipped * 1000 > d.samples.len() {
                    flags.push("clip>0.1%");
                }
                if s.dc.abs() > 1000.0 {
                    flags.push("dc");
                }
                if s.peak == 0 {
                    flags.push("silent");
                }
                if !flags.is_empty() {
                    flagged += 1;
                }
                let _ = writeln!(
                    tsv,
                    "{m}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t{}\t{:.1}\t{}\t{:.1}\t{}",
                    lib.location_name(loc),
                    d.channels,
                    d.rate,
                    s.frames,
                    info.total_frames(),
                    d.duration_secs(),
                    s.peak,
                    s.rms_db,
                    s.clipped,
                    s.dc,
                    flags.join(",")
                );
                ok += 1;
            }
            Err(e) => {
                let _ = writeln!(tsv, "{m}\t{}\t\t\t\t\t\t\t\t\t\tdecode: {e:#}", lib.location_name(loc));
                bad += 1;
            }
        }
    }
    let path = args.out.join("check.tsv");
    std::fs::write(&path, tsv)?;
    println!("decoded {ok}, failed {bad}, flagged {flagged}, frame-count mismatches {frame_mismatch} in {:.1}s -> {}", t.elapsed().as_secs_f32(), path.display());
    for (c, n) in &codecs {
        println!("  {c:<28} {n}");
    }

    // Event durations: one-shot events with a single, unrandomised voice vs the engine's table
    // (which Wwise computed from the source files, including pitch, delays and loop counts).
    let (mut compared, mut within, mut worst, mut over) = (0usize, 0usize, 0f64, Vec::new());
    let mut st = PlayState::new(1);
    for e in &lib.events_table.events {
        if e.duration_min <= 0.0 || (e.duration_max - e.duration_min).abs() > 1e-4 {
            continue;
        }
        let Some(ev) = lib.event(e.id) else { continue };
        let Ok(cmds) = st.post_event(lib, e.id) else { continue };
        let plays: Vec<(f32, &Playable)> =
            cmds.iter().filter_map(|c| if let Command::Play { delay_s, what, .. } = c { Some((*delay_s, what)) } else { None }).collect();
        if plays.len() != 1 || ev.actions.iter().any(|a| lib.action(*a).is_some_and(|a| !a.ranged.is_empty())) {
            continue;
        }
        let voices = plays[0].1.voices();
        if voices.len() != 1 || !matches!(plays[0].1, Playable::Voice(_)) {
            continue;
        }
        let v = voices[0];
        let randomised = v.nodes.iter().any(|n| {
            lib.node(*n).and_then(|o| o.node()).is_some_and(|b| b.ranged.iter().any(|r| r.0 == idaudio::hirc::prop::PITCH || r.0 == idaudio::hirc::prop::INITIAL_DELAY))
        });
        if randomised || v.loop_count == 0 {
            continue;
        }
        let Ok(info) = lib.media_info(v.media) else { continue };
        let expected = info.duration_secs() * 2f64.powf(-f64::from(v.pitch_cents) / 1200.0) * f64::from(v.loop_count)
            + f64::from(plays[0].0)
            + f64::from(v.delay_s);
        let diff = expected - f64::from(e.duration_min);
        compared += 1;
        worst = worst.max(diff.abs());
        if diff.abs() * f64::from(info.sample_rate) <= 64.0 {
            within += 1;
        } else {
            over.push((e.name.clone(), expected, e.duration_min, v.pitch_cents, v.delay_s, v.loop_count));
        }
    }
    println!(
        "single-voice one-shot events vs soundbanksinfo durations: {compared} compared, {within} within one 64-sample ADPCM block, worst |diff| {worst:.4}s"
    );
    let mut out = String::from("event	expected	table	pitch_cents	delay	loops
");
    for (n, a, b, p, d, l) in &over {
        let _ = writeln!(out, "{n}	{a:.4}	{b:.4}	{p}	{d}	{l}");
    }
    std::fs::write(args.out.join("duration_outliers.tsv"), out)?;
    for (n, a, b, p, d, l) in over.iter().filter(|o| o.3 == 0.0).take(12) {
        println!("  {n}: expected {a:.4}s vs table {b:.4}s (pitch {p:+.0}c delay {d:.3}s loops {l})");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Decl-driven mapping

struct Decls {
    container: Arc<idres::Container>,
    db: idres::decldb::DeclDb,
}

impl Decls {
    fn open(doom: &Path) -> Result<Self> {
        let container = Arc::new(idres::Container::open(&doom.join("base"), "gameresources")?);
        Ok(Self { db: idres::decldb::DeclDb::new(container.clone()), container })
    }
    fn names(&self, prefix: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .container
            .live_entries()
            .filter_map(|e| e.full_name.strip_prefix(prefix).and_then(|n| n.strip_suffix(".decl")).map(str::to_owned))
            .collect();
        v.sort();
        v
    }
}

/// (dotted key path, value) for every string value whose key mentions a sound.
fn sound_keys(b: &Block, prefix: &str, out: &mut Vec<(String, String)>) {
    for (k, v) in &b.items {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            Value::Block(inner) => sound_keys(inner, &path, out),
            Value::Str(s) | Value::Atom(s) => {
                let lk = k.to_lowercase();
                if (lk.contains("sound") || lk.starts_with("snd")) && !s.is_empty() && s != "NULL" && !lk.ends_with("ms") {
                    out.push((path, s.clone()));
                }
            }
        }
    }
}

struct Row {
    category: String,
    item: String,
    trigger: String,
    sound: String,
}

fn map(lib: &AudioLibrary, args: &Args) -> Result<()> {
    let decls = Decls::open(&args.doom)?;
    let mut rows: Vec<Row> = Vec::new();
    let push = |rows: &mut Vec<Row>, c: &str, i: &str, t: &str, s: &str| {
        rows.push(Row { category: c.into(), item: i.into(), trigger: t.into(), sound: s.into() })
    };

    // Weapons: every SP weapon decl, its ammo -> projectile, and its hands model's anim events.
    let mut md6_done = HashSet::new();
    for w in decls.names("generated/decls/weapon/weapon/zion/player/sp/") {
        let name = format!("weapon/zion/player/sp/{w}");
        let Ok(decl) = decls.db.get("weapon", &name) else { continue };
        let mut keys = Vec::new();
        sound_keys(&decl, "", &mut keys);
        for (k, s) in keys {
            push(&mut rows, "weapon", &w, &format!("weapon.{}", k.trim_start_matches("edit.")), &s);
        }
        let mut ammo: Vec<String> = decl.str("edit.initialAmmoDecl").map(str::to_owned).into_iter().collect();
        if let Some(clips) = decl.block("edit.validAmmoClips") {
            for (k, v) in &clips.items {
                if let (true, Some(a)) = (k.starts_with("item["), v.as_block().and_then(|b| b.str("validAmmoDecl"))) {
                    if !ammo.iter().any(|x| x == a) {
                        ammo.push(a.to_owned());
                    }
                }
            }
        }
        for a in ammo {
            let Some(proj) = decls.db.get("ammo", &a).ok().and_then(|d| d.str("edit.projectileDecl").map(str::to_owned)) else { continue };
            let Ok(p) = decls.db.get("projectile", &proj) else { continue };
            let mut keys = Vec::new();
            sound_keys(&p, "", &mut keys);
            for (k, s) in keys {
                push(&mut rows, "weapon", &w, &format!("{proj}.{}", k.trim_start_matches("edit.")), &s);
            }
        }
        if let Some(md6) = decl.str("edit.handsModelMD6") {
            if md6_done.insert(md6.to_owned()) {
                let path = format!("generated/decls/md6def/{md6}.decl");
                if let Ok(bytes) = decls.container.read_by_name(&path) {
                    for e in idaudio::anim::md6def_sounds(&String::from_utf8_lossy(&bytes)) {
                        let anim = e.anim.rsplit('/').next().unwrap_or(&e.anim).trim_end_matches(".md6anim").to_owned();
                        push(&mut rows, "weapon-anim", md6, &format!("{anim} f{} {}", e.frame, e.event), &e.sound);
                    }
                }
            }
        }
    }

    // Player: entityDef keys, footstep and landing tables.
    if let Ok(p) = decls.db.get("entitydef", "player") {
        let mut keys = Vec::new();
        sound_keys(&p, "", &mut keys);
        for (k, s) in keys {
            push(&mut rows, "player", "entitydef/player", k.trim_start_matches("edit."), &s);
        }
        for key in [
            "footstepEffectTable",
            "footstepEffectTable_Sprint",
            "footstepEffectTable_SlowWalk",
            "footstepEffectTable_CrouchWalk",
            "footstepEffectTable_Landing",
            "footstepEffectTable_HeavyLanding",
        ] {
            let Some(table) = find_key(&p, key) else { continue };
            let Ok(t) = decls.db.get("projectileimpacteffect", &table) else { continue };
            let mut keys = Vec::new();
            sound_keys(&t, "", &mut keys);
            for (k, s) in keys {
                push(&mut rows, "player-surface", &format!("{key} ({table})"), k.trim_start_matches("edit."), &s);
            }
        }
    }

    // Pickups.
    for (prefix, kindname) in [
        ("generated/decls/prophealth/", "prophealth"),
        ("generated/decls/propitem/ammo/sp/", "propitem"),
        ("generated/decls/propitem/weapon/sp/", "propitem"),
    ] {
        for n in decls.names(prefix) {
            let full = format!("{}{}", prefix.trim_start_matches(&format!("generated/decls/{kindname}/")), n);
            if full.contains("mp_") || full.contains("coop") {
                continue;
            }
            let Ok(d) = decls.db.get(kindname, &full) else { continue };
            let mut keys = Vec::new();
            sound_keys(&d, "", &mut keys);
            for (k, s) in keys {
                push(&mut rows, "pickup", &format!("{kindname}/{full}"), k.trim_start_matches("edit."), &s);
            }
        }
    }

    // Resolve and write.
    let mut tsv = String::from("category\titem\ttrigger\tsound\tevent\tevent_id\tin_table\tmedia_all\tmedia_first_person\tduration\n");
    let mut fp = Switches::default();
    fp.set("locality", "first_person");
    let mut unresolved = 0;
    for r in &rows {
        let ev = sound_event_name(&r.sound);
        let id = sound_event_id(&r.sound);
        let info = lib.event_info(id);
        let all = lib.resolve_event(id).map_or(0, |m| m.len());
        let first = lib.resolve_event_with(id, &fp).map_or(0, |m| m.len());
        if lib.event(id).is_none() {
            unresolved += 1;
        }
        let _ = writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.category,
            r.item,
            r.trigger,
            r.sound,
            info.map_or(ev.as_str(), |e| e.name.as_str()),
            id,
            info.is_some(),
            all,
            first,
            info.map_or(String::new(), |e| format!("{:.3}-{:.3}", e.duration_min, e.duration_max))
        );
    }
    std::fs::create_dir_all(&args.out)?;
    let path = args.out.join("sound_map.tsv");
    std::fs::write(&path, &tsv)?;
    println!("{} sound references, {unresolved} with no Wwise event -> {}", rows.len(), path.display());
    Ok(())
}

fn find_key(b: &Block, key: &str) -> Option<String> {
    for (k, v) in &b.items {
        if k == key {
            if let Some(s) = v.as_str() {
                return Some(s.to_owned());
            }
        }
        if let Some(inner) = v.as_block() {
            if let Some(s) = find_key(inner, key) {
                return Some(s);
            }
        }
    }
    None
}

fn render(lib: AudioLibrary, args: &Args, items: &[String]) -> Result<()> {
    let (out, items) = items.split_first().context("render OUT.wav ITEM...")?;
    let rate = 48000u32;
    let mixer = Arc::new(std::sync::Mutex::new(idaudio::Mixer::new(rate)));
    let mut eng = idaudio::Engine::new(Arc::new(lib), mixer.clone(), 1);
    for (g, v) in &args.sets {
        eng.set_switch(None, g, v);
    }
    if let Some(e) = &args.env {
        let (bus, gun) = e.split_once(':').unwrap_or((e.as_str(), "int_med"));
        eng.set_environment(Some(idaudio::Environment {
            aux_bus: bus.to_owned(),
            gun_env: gun.to_owned(),
            aux_send_level: 1.0,
            dry_gain: 1.0,
            reverb: idaudio::ReverbParams::default(),
        }));
    }
    let mut events: Vec<(f32, String, Option<[f32; 3]>)> = Vec::new();
    for it in items {
        let mut parts = it.split('@');
        let name = parts.next().unwrap_or_default().to_owned();
        let t: f32 = parts.next().unwrap_or("0").parse()?;
        let pos = parts.next().map(|p| {
            let v: Vec<f32> = p.split(',').filter_map(|x| x.parse().ok()).collect();
            [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0)]
        });
        events.push((t, name, pos));
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let end = events.last().map_or(0.0, |e| e.0) + 4.0;
    let block = 480usize; // 10 ms "frames"
    let mut pcm: Vec<f32> = Vec::new();
    let mut buf = vec![0.0f32; block * 2];
    let mut next = 0;
    let mut emitter = 100u64;
    let mut t = 0.0f32;
    while t < end {
        while next < events.len() && events[next].0 <= t {
            let (_, name, pos) = &events[next];
            let id = match pos {
                None => eng.player_emitter(),
                Some(p) => {
                    emitter += 1;
                    eng.set_emitter(emitter, *p);
                    emitter
                }
            };
            match eng.post_name(name, id) {
                Ok(pid) => println!("{t:7.3}s post {name} -> playing {pid}"),
                Err(e) => println!("{t:7.3}s post {name}: {e:#}"),
            }
            next += 1;
        }
        eng.update();
        mixer.lock().unwrap().render(&mut buf);
        pcm.extend_from_slice(&buf);
        t += block as f32 / rate as f32;
    }
    let peak = pcm.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let rms = (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
    let clipped = pcm.iter().filter(|x| x.abs() >= 1.0).count();
    let samples: Vec<i16> = pcm.iter().map(|x| (x.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
    let path = PathBuf::from(out);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    idaudio::wav::write(&path, rate, 2, 3, &samples)?;
    println!(
        "{} : {:.2}s stereo, peak {:.1} dBFS, rms {:.1} dBFS, {} samples >= full scale, voices left {}",
        path.display(),
        pcm.len() as f32 / 2.0 / rate as f32,
        20.0 * peak.max(1e-9).log10(),
        20.0 * rms.max(1e-9).log10(),
        clipped,
        eng.voice_count()
    );
    Ok(())
}
