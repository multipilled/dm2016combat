//! `swfx`: inspect and render DOOM (2016) binary SWF GUIs from the user's own install.
//! Output goes to the git-ignored gamedata/swf folder.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use idres::Container;
use idswf::as2;
use idswf::bswf::{DictEntry, Sprite, Swf};
use idswf::tags::{self, Tag};

#[derive(Parser)]
struct Cli {
    /// DOOM install folder (defaults to $DOOM_DIR or known Steam paths)
    #[arg(long, global = true)]
    doom: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse every generated/swf/*.bswf and report failures
    Check,
    /// Summarise one SWF (e.g. `dump hud`)
    Dump {
        name: String,
        #[arg(long)]
        verbose: bool,
    },
    /// Print every sprite's timeline (place/remove/actions per frame)
    Tree { name: String },
    /// Disassemble every DoAction / DoInitAction / clip action in a SWF
    Scripts { name: String },
    /// Count AS2 opcodes and PlaceObject features over every SWF (or the named ones)
    Ops { names: Vec<String> },
    /// Print an SDF font's metrics and some glyphs, and write its atlas to gamedata/swf/
    Font { face: String, #[arg(default_value = "0123456789!AW")] chars: String },
    /// Write a SWF's texture atlas to gamedata/swf/<name>_atlas.png
    Atlas { name: String },
    /// Write a GUI material's decoded image to gamedata/swf/mat_<name>.png
    Material { name: String },
    /// Render a SWF to gamedata/swf/<name>.png after running it for some time
    Render {
        name: String,
        /// Seconds of playback before capturing
        #[arg(long, default_value_t = 0.0)]
        time: f64,
        /// Output scale relative to the SWF stage
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Supersampling factor
        #[arg(long, default_value_t = 1)]
        ss: usize,
        /// Background grey level 0..1
        #[arg(long, default_value_t = 0.15)]
        bg: f32,
        /// Script statements to run first: path=frame (gotoAndStop), path.text=..., call:path
        #[arg(long)]
        set: Vec<String>,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print the live instance tree
        #[arg(long)]
        tree: bool,
        /// Stage region to render: x0,y0,x1,y1 (defaults to the SWF frame)
        #[arg(long)]
        view: Option<String>,
        /// Resolve clip layers geometrically (the Bevy path) instead of with the stencil
        #[arg(long)]
        clip: bool,
    },
    /// Composite the SP HUD (vitals, ammo, warning, reticle) through the HUD controller into one screen PNG
    Hud {
        #[arg(long, default_value_t = 100.0)]
        health: f32,
        #[arg(long, default_value_t = 100.0)]
        max_health: f32,
        #[arg(long, default_value_t = 50.0)]
        armor: f32,
        #[arg(long, default_value_t = 50.0)]
        max_armor: f32,
        /// Weapon decl (icon and reticle come from it)
        #[arg(long, default_value = "weapon/zion/player/sp/shotgun")]
        weapon: String,
        #[arg(long, default_value_t = 30)]
        ammo: i32,
        /// 0 = the weapon uses no ammo
        #[arg(long, default_value_t = 50)]
        max_ammo: i32,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 1.5)]
        time: f64,
        /// View field of view (g_fov) for the reticle
        #[arg(long, default_value_t = 90.0)]
        view_fov: f32,
        /// The ammo decl's lowAmmoWarningCount
        #[arg(long, default_value_t = 6)]
        low_ammo_count: i32,
        /// Reticle spread (idPlayer::GetSpread / tan(fov / 2))
        #[arg(long, default_value_t = 0.0)]
        spread: f32,
        /// Active weapon mod: its base perk decl (e.g. perk/zion/player/sp/weapons/shotgun/pop_rocket)
        #[arg(long)]
        perk: Option<String>,
        /// The weapon's other mod (shown as owned)
        #[arg(long)]
        other_perk: Option<String>,
        /// weaponReticle decl instead of the weapon's (e.g. a mod's weaponreticle/sp/rocket_launcher_lockon_mod)
        #[arg(long)]
        reticle: Option<String>,
        /// Reticle charge (0..1) and lock fraction
        #[arg(long, default_value_t = 0.0)]
        charge: f32,
        #[arg(long, default_value_t = 0.0)]
        lock: f32,
        /// Print the live instance tree of this HUD movie
        #[arg(long)]
        tree: Option<String>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print localized strings (#str_ ids)
    Lang { ids: Vec<String> },
    /// Drive the in-game pause / settings menu with inputs and render it to a PNG
    Menu {
        /// Inputs in order: up, down, left, right, accept, back, wheel:<n>, at:<x>,<y> (pointer), press, release,
        /// wait:<seconds>
        #[arg(long, value_delimiter = ' ')]
        keys: Vec<String>,
        /// Cvar values: name=value
        #[arg(long)]
        cvar: Vec<String>,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Seconds to run after the last input
        #[arg(long, default_value_t = 1.0)]
        time: f64,
        /// Background grey level 0..1
        #[arg(long, default_value_t = 0.1)]
        bg: f32,
        #[arg(long)]
        tree: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// Cvars and key binds for `swfx menu` (binds start as default.cfg's bindset 0, sorted by key name).
struct MapCvars {
    cvars: BTreeMap<String, String>,
    binds: BTreeMap<String, Vec<String>>,
    defaults: BTreeMap<String, Vec<String>>,
}

impl idswf::menu::CvarStore for MapCvars {
    fn cvar(&self, name: &str) -> Option<String> {
        self.cvars.get(&name.to_ascii_lowercase()).cloned()
    }
    fn set_cvar(&mut self, name: &str, value: &str) {
        println!("set {name} = {value}");
        self.cvars.insert(name.to_ascii_lowercase(), value.to_string());
    }
    fn reset_cvar(&mut self, name: &str) {
        println!("reset {name}");
        self.cvars.remove(&name.to_ascii_lowercase());
    }
    fn binds(&self) -> Vec<(String, Vec<String>)> {
        self.binds.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
    fn set_bind(&mut self, key: &str, actions: &str) {
        println!("bind {key} \"{actions}\"");
        let list: Vec<String> = actions.split_whitespace().map(|a| a.to_ascii_lowercase()).collect();
        if list.is_empty() {
            self.binds.remove(key);
        } else {
            self.binds.insert(key.to_string(), list);
        }
    }
    fn reset_binds(&mut self) {
        println!("reset binds");
        self.binds = self.defaults.clone();
    }
}

/// `bind "<key>" "<actions>"` lines of default.cfg's bindset 0.
fn default_binds(container: &Container) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Some(e) = container.get("generated/binaryfile/default.bfile") else { return out };
    let Some(text) = container.read(e).ok().and_then(|raw| idres::crypt::decrypt(&raw, &e.short_name)) else { return out };
    let mut set = None;
    for line in String::from_utf8_lossy(&text).lines() {
        let line = line.trim();
        if let Some(n) = line.strip_prefix("bindset ") {
            set = n.trim().parse::<u32>().ok();
        } else if set == Some(0) {
            if let Some(rest) = line.strip_prefix("bind ") {
                let parts: Vec<&str> = rest.split('"').map(str::trim).filter(|a| !a.is_empty()).collect();
                if let [key, actions] = parts.as_slice() {
                    // Gamepad inputs are another list in the game; the side-less modifiers are the left keys.
                    let key = match key.to_ascii_uppercase().as_str() {
                        k if k.starts_with("JOY") => continue,
                        "SHIFT" => "LSHIFT".to_string(),
                        "CTRL" => "LCTRL".to_string(),
                        "ALT" => "LALT".to_string(),
                        k => k.to_string(),
                    };
                    let list: Vec<String> = actions.split_whitespace().filter(|a| a.starts_with('_')).map(|a| a.to_ascii_lowercase()).collect();
                    if !list.is_empty() {
                        out.insert(key, list);
                    }
                }
            }
        }
    }
    out
}

/// Rasterizes one player's draw list onto `canvas` (window-sized GUI).
fn raster_player(assets: &idswf::Assets, p: &idswf::Player, canvas: &mut idswf::raster::Canvas) {
    let list = idswf::render::draw_clipped(p, canvas.width as f32, canvas.height as f32);
    let mut materials = std::collections::HashMap::new();
    for b in &list.batches {
        if let idswf::render::TexRef::Material(m) = &b.texture {
            materials.entry(m.to_string()).or_insert_with(|| assets.material_texture(m));
        }
    }
    let atlas = p.atlas.clone();
    let fonts = p.fonts.clone();
    let lookup = |t: &idswf::render::TexRef| -> Option<&idswf::texture::Texture> {
        match t {
            idswf::render::TexRef::Atlas => atlas.as_deref(),
            idswf::render::TexRef::Font(f) => fonts.get(&idswf::font::face_dir(f)).map(|f| &f.atlas),
            idswf::render::TexRef::Material(m) => materials.get(m.as_ref()).and_then(|t| t.as_deref()),
            idswf::render::TexRef::White => None,
        }
    };
    idswf::raster::rasterize(&list, canvas, [1.0, 1.0], [0.0, 0.0], &lookup);
}

fn swf_path(name: &str) -> String {
    if name.starts_with("generated/") { name.to_string() } else { format!("generated/swf/{name}.bswf") }
}

fn sprites(swf: &Swf) -> Vec<(String, &Sprite)> {
    let mut v = vec![("main".to_string(), &swf.main)];
    for (i, e) in swf.dict.iter().enumerate() {
        if let DictEntry::Sprite(s) = e {
            v.push((format!("sprite #{i}"), s));
        }
    }
    v
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let doom = cli.doom.or_else(idres::find_install).context("DOOM install not found; pass --doom or set DOOM_DIR")?;
    let container = Container::open(&doom.join("base"), "gameresources")?;
    let all_swfs = || {
        let mut v: Vec<_> = container.live_entries().filter(|e| e.full_name.ends_with(".bswf")).map(|e| e.full_name.clone()).collect();
        v.sort();
        v
    };
    match cli.cmd {
        Cmd::Check => {
            let mut tags = BTreeMap::<u32, usize>::new();
            let (mut ok, mut bad) = (0, 0);
            for n in &all_swfs() {
                match Swf::parse(&container.read_by_name(n)?) {
                    Ok(swf) => {
                        ok += 1;
                        for (_, s) in sprites(&swf) {
                            s.commands.iter().for_each(|c| *tags.entry(c.tag).or_default() += 1);
                        }
                    }
                    Err(e) => {
                        bad += 1;
                        println!("FAIL {n}: {e:#}");
                    }
                }
            }
            println!("{ok} parsed, {bad} failed");
            println!("command tags: {tags:?}");
        }
        Cmd::Dump { name, verbose } => {
            let swf = Swf::parse(&container.read_by_name(&swf_path(&name))?)?;
            println!(
                "{name}: {}x{} @ {} fps, atlas {}x{}, {} dict entries, main: {} frames {} commands",
                swf.frame_width,
                swf.frame_height,
                swf.frames_per_second(),
                swf.atlas_width,
                swf.atlas_height,
                swf.dict.len(),
                swf.main.frame_count,
                swf.main.commands.len()
            );
            let mut kinds = BTreeMap::<&str, usize>::new();
            for e in &swf.dict {
                *kinds.entry(e.type_name()).or_default() += 1;
            }
            println!("dictionary: {kinds:?}");
            for (i, e) in swf.dict.iter().enumerate() {
                match e {
                    DictEntry::Image(im) => println!("  #{i} image {:?} size {:?} at {:?} scale {:?}", im.material, im.size, im.atlas_offset, im.channel_scale),
                    DictEntry::Font(f) => println!("  #{i} font '{}' asc {} desc {} lead {} glyphs {}", f.name, f.ascent, f.descent, f.leading, f.glyphs.len()),
                    DictEntry::EditText(t) => println!(
                        "  #{i} edittext font #{} h {} color {:?} flags {:#x} align {} margins {} {} indent {} leading {} max {} var '{}' text {:?} bounds {:?}",
                        t.font_id, t.font_height, t.color, t.flags, t.align, t.left_margin, t.right_margin, t.indent, t.leading, t.max_length, t.variable, t.initial_text, t.bounds
                    ),
                    DictEntry::Sprite(s) if verbose => println!(
                        "  #{i} sprite {} frames {} cmds labels {:?} init {}",
                        s.frame_count,
                        s.commands.len(),
                        s.frame_labels,
                        s.do_init_actions.len()
                    ),
                    DictEntry::Shape(s) | DictEntry::Morph(s) if verbose => println!(
                        "  #{i} {} fills {:?} lines {} bounds {:?}",
                        e.type_name(),
                        s.fills.iter().map(|f| (f.style.kind, f.style.sub_type, f.style.bitmap_id, f.indices.len() / 3)).collect::<Vec<_>>(),
                        s.lines.len(),
                        s.start_bounds
                    ),
                    DictEntry::Text(t) if verbose => println!("  #{i} text records {:?}", t.records),
                    _ => {}
                }
            }
            println!("main labels: {:?}", swf.main.frame_labels);
        }
        Cmd::Tree { name } => {
            let swf = Swf::parse(&container.read_by_name(&swf_path(&name))?)?;
            for (label, s) in sprites(&swf) {
                print_timeline(&swf, &label, s);
            }
        }
        Cmd::Scripts { name } => {
            let swf = Swf::parse(&container.read_by_name(&swf_path(&name))?)?;
            for (label, s) in sprites(&swf) {
                for (k, init) in s.do_init_actions.iter().enumerate() {
                    let mut o = String::new();
                    as2::disassemble(init, &mut o);
                    println!("== {label} initAction {k}\n{o}");
                }
                for f in 0..s.frame_count as usize {
                    for c in s.frame_commands(f) {
                        match tags::decode(c) {
                            Ok(Tag::DoAction(code)) => {
                                let mut o = String::new();
                                as2::disassemble(&code, &mut o);
                                println!("== {label} frame {f} DoAction\n{o}");
                            }
                            Ok(Tag::Place(p)) => {
                                for ca in &p.clip_actions {
                                    let mut o = String::new();
                                    as2::disassemble(&ca.actions, &mut o);
                                    println!("== {label} frame {f} clipAction depth {} events {:#x}\n{o}", p.depth, ca.events);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Cmd::Font { face, chars } => {
            let f = idswf::font::SdfFont::load(&container, &face)?;
            println!(
                "{}: point {} asc {} desc {} pad {} glyphs {} atlas {}x{}",
                f.name, f.point_size, f.ascender, f.descender, f.padding, f.glyphs.len(), f.atlas.width, f.atlas.height
            );
            for c in chars.chars() {
                println!("  {c:?} {:?}", f.glyph(c as u32));
            }
            let out = PathBuf::from(format!("gamedata/swf/font_{}.png", idswf::font::face_dir(&face)));
            idswf::write_png(&out, f.atlas.width, f.atlas.height, &f.atlas.rgba)?;
            println!("wrote {}", out.display());
        }
        Cmd::Atlas { name } => {
            let bytes = container.read_by_name(&format!("generated/swf/{name}.bimage"))?;
            let t = idswf::texture::decode_bimage(&bytes)?;
            let out = PathBuf::from(format!("gamedata/swf/{}_atlas.png", name.replace('/', "_")));
            idswf::write_png(&out, t.width, t.height, &t.rgba)?;
            println!("wrote {} ({}x{})", out.display(), t.width, t.height);
        }
        Cmd::Material { name } => {
            let assets = idswf::Assets::new(std::sync::Arc::new(container));
            let t = assets.material_texture(&name).with_context(|| format!("material {name}"))?;
            let out = PathBuf::from(format!("gamedata/swf/mat_{}.png", name.replace('/', "_")));
            idswf::write_png(&out, t.width, t.height, &t.rgba)?;
            println!("wrote {} ({}x{})", out.display(), t.width, t.height);
        }
        Cmd::Render { name, time, scale, ss, bg, set, out, tree, view, clip } => {
            let assets = idswf::Assets::new(std::sync::Arc::new(container));
            let mut p = assets.player(&name)?;
            apply_sets(&mut p, &set);
            let mut t = 0.0;
            while t < time {
                p.update(1.0 / 60.0);
                t += 1.0 / 60.0;
            }
            apply_sets(&mut p, &set);
            if tree {
                print!("{}", p.dump_tree());
            }
            for l in p.log.drain(..).take(40) {
                println!("log: {l}");
            }
            let (w, h) = (p.swf.frame_width, p.swf.frame_height);
            let list = if clip { idswf::render::draw_clipped(&p, w, h) } else { idswf::render::draw(&p, w, h) };
            let v: Vec<f32> = view.as_deref().map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default();
            let (x0, y0, x1, y1) = if v.len() == 4 { (v[0], v[1], v[2], v[3]) } else { (0.0, 0.0, w, h) };
            let (cw, ch) = (((x1 - x0) * scale) as usize * ss, ((y1 - y0) * scale) as usize * ss);
            let mut canvas = idswf::raster::Canvas::new(cw, ch);
            let k = scale * ss as f32;
            let atlas = p.atlas.clone();
            let fonts = p.fonts.clone();
            let mut materials = std::collections::HashMap::new();
            for b in &list.batches {
                if let idswf::render::TexRef::Material(m) = &b.texture {
                    if !materials.contains_key(m.as_ref()) {
                        let t = assets.material_texture(m);
                        if t.is_none() {
                            println!("material {m}: not found");
                        }
                        materials.insert(m.to_string(), t);
                    }
                }
            }
            let lookup = |t: &idswf::render::TexRef| -> Option<&idswf::texture::Texture> {
                match t {
                    idswf::render::TexRef::Atlas => atlas.as_deref(),
                    idswf::render::TexRef::Font(f) => fonts.get(&idswf::font::face_dir(f)).map(|f| &f.atlas),
                    idswf::render::TexRef::Material(m) => materials.get(m.as_ref()).and_then(|t| t.as_deref()),
                    idswf::render::TexRef::White => None,
                }
            };
            let t0 = std::time::Instant::now();
            idswf::raster::rasterize(&list, &mut canvas, [k, k], [-x0 * k, -y0 * k], &lookup);
            println!("rasterized in {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0);
            let t1 = std::time::Instant::now();
            let _ = idswf::render::draw(&p, w, h);
            println!("draw list in {:.3} ms", t1.elapsed().as_secs_f64() * 1000.0);
            let canvas = canvas.downsample(ss);
            let out = out.unwrap_or_else(|| PathBuf::from(format!("gamedata/swf/{}.png", name.replace('/', "_"))));
            idswf::write_png(&out, canvas.width as u32, canvas.height as u32, &canvas.over([bg, bg, bg]))?;
            let tris: usize = list.batches.iter().map(|b| b.indices.len() / 3).sum();
            println!("wrote {} ({}x{}, {} batches, {} triangles)", out.display(), canvas.width, canvas.height, list.batches.len(), tris);
        }
        Cmd::Hud { health, max_health, armor, max_armor, weapon, ammo, max_ammo, width, height, time, view_fov, low_ammo_count, spread, perk, other_perk, reticle: reticle_decl, charge, lock, tree, out } => {
            let assets = idswf::Assets::new(std::sync::Arc::new(container));
            let db = idres::decldb::DeclDb::new(assets.container.clone());
            let (weapon_icon, reticle) = idswf::hud::weapon_visuals(&db, &weapon);
            let reticle = reticle_decl.or(reticle);
            let state = idswf::hud::HudState {
                visible: true,
                health,
                max_health,
                armor,
                max_armor,
                ammo,
                max_ammo,
                weapon_icon,
                reticle,
                bfg_ammo: 0,
                chainsaw_fuel: 0,
                reticle_spread: spread,
                reticle_zoomed: false,
                low_ammo_count,
                ammo_per_shot: 1,
                weapon_id: Some(0),
                weapon_mod: perk.as_deref().and_then(|p| idswf::hud::perk_visuals(&db, p)),
                other_mod: other_perk.as_deref().and_then(|p| idswf::hud::perk_visuals(&db, p)),
                other_mod_owned: other_perk.is_some(),
                mod_slot: true,
                reticle_charge: charge,
                reticle_discharge: 0.0,
                reticle_lock: lock,
            };
            println!("icon {:?} reticle {:?} mod {:?}", state.weapon_icon, state.reticle, state.weapon_mod);
            let mut hud = idswf::hud::Hud::load(&assets)?;
            let mut t = 0.0;
            while t < time {
                hud.update(&state, 1.0 / 60.0);
                t += 1.0 / 60.0;
            }
            println!("reticle sprite {:?}", hud.current_reticle());
            let t0 = std::time::Instant::now();
            let mut batches = 0;
            for _ in 0..100 {
                hud.update(&state, 1.0 / 60.0);
                for (_, p) in hud.players() {
                    batches += idswf::render::draw_clipped(p, p.swf.frame_width, p.swf.frame_height).batches.len();
                }
            }
            println!("update + draw lists: {:.3} ms/frame, {} batches", t0.elapsed().as_secs_f64() * 10.0, batches / 100);
            let (w, h) = (width as f32, height as f32);
            let layout = hud.layout(w, h, view_fov);
            for (name, panel, proj) in &layout.panels {
                let c = proj.to_screen(panel.origin, w, h);
                println!("{name}: centre at ({:.1}, {:.1}) px, scale {}", c[0], c[1], panel.scale);
            }
            let mut canvas = idswf::raster::Canvas::new(width as usize, height as usize);
            let mut materials = std::collections::HashMap::new();
            for (name, p) in hud.players() {
                if tree.as_deref() == Some(name) {
                    print!("{}", p.dump_tree());
                }
                let (fw, fh) = (p.swf.frame_width, p.swf.frame_height);
                // The fullscreen movie is drawn 2D at window size; the others on their panels.
                let placed = layout.get(name);
                if placed.is_none() && name != idswf::hud::SCREEN {
                    continue;
                }
                let mut list = if placed.is_some() { idswf::render::draw_clipped(p, fw, fh) } else { idswf::render::draw_clipped(p, w, h) };
                for b in &mut list.batches {
                    if let idswf::render::TexRef::Material(m) = &b.texture {
                        materials.entry(m.to_string()).or_insert_with(|| assets.material_texture(m));
                    }
                    if tree.as_deref() == Some("stage") {
                        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                        for v in &b.verts {
                            for k in 0..2 {
                                lo[k] = lo[k].min(v.pos[k]);
                                hi[k] = hi[k].max(v.pos[k]);
                            }
                        }
                        if lo[1] < 0.0 || hi[1] > fh || lo[0] < 0.0 || hi[0] > fw {
                            println!("{name}: outside stage {:?} ({:.0},{:.0})..({:.0},{:.0})", b.texture, lo[0], lo[1], hi[0], hi[1]);
                        }
                    }
                    if let Some((panel, proj)) = placed {
                        for v in &mut b.verts {
                            v.pos = proj.to_screen(panel.stage_to_view(v.pos, fw, fh), w, h);
                        }
                    }
                }
                let atlas = p.atlas.clone();
                let fonts = p.fonts.clone();
                let lookup = |t: &idswf::render::TexRef| -> Option<&idswf::texture::Texture> {
                    match t {
                        idswf::render::TexRef::Atlas => atlas.as_deref(),
                        idswf::render::TexRef::Font(f) => fonts.get(&idswf::font::face_dir(f)).map(|f| &f.atlas),
                        idswf::render::TexRef::Material(m) => materials.get(m.as_ref()).and_then(|t| t.as_deref()),
                        idswf::render::TexRef::White => None,
                    }
                };
                idswf::raster::rasterize(&list, &mut canvas, [1.0, 1.0], [0.0, 0.0], &lookup);
                for b in &list.batches {
                    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                    for v in &b.verts {
                        for k in 0..2 {
                            lo[k] = lo[k].min(v.pos[k]);
                            hi[k] = hi[k].max(v.pos[k]);
                        }
                    }
                    if tree.as_deref() == Some("bounds") {
                        println!("{name}: batch {:?} {:?} {} verts ({:.0},{:.0})..({:.0},{:.0})", b.texture, b.shader, b.verts.len(), lo[0], lo[1], hi[0], hi[1]);
                    }
                }
                for l in p.log.iter().take(5) {
                    println!("{name}: {l}");
                }
            }
            let out = out.unwrap_or_else(|| PathBuf::from("gamedata/swf/hud_composite.png"));
            idswf::write_png(&out, width, height, &canvas.over([0.18, 0.16, 0.15]))?;
            println!("wrote {}", out.display());
        }
        Cmd::Lang { ids } => {
            let strings = idswf::player::load_strings(&container)?;
            for id in ids {
                // `~text` lists every string whose key contains `text`.
                if let Some(sub) = id.strip_prefix('~') {
                    let sub = sub.to_ascii_lowercase();
                    let mut hits: Vec<_> = strings.iter().filter(|(k, _)| k.to_ascii_lowercase().contains(&sub)).collect();
                    hits.sort();
                    for (k, v) in hits {
                        println!("{k} = {v:?}");
                    }
                    continue;
                }
                println!("{id} = {:?}", strings.get(&id.to_ascii_lowercase()).or_else(|| strings.get(&id)));
            }
        }
        Cmd::Menu { keys, cvar, width, height, time, bg, tree, out } => {
            use idswf::menu::{Menu, MenuInput};
            let defaults = default_binds(&container);
            let assets = idswf::Assets::new(std::sync::Arc::new(container));
            let mut cvars = MapCvars { cvars: BTreeMap::new(), binds: defaults.clone(), defaults };
            for c in &cvar {
                if let Some((k, v)) = c.split_once('=') {
                    cvars.cvars.insert(k.to_ascii_lowercase(), v.to_string());
                }
            }
            let (w, h) = (width as f32, height as f32);
            let mut menu = Menu::load(&assets)?;
            menu.open(&cvars);
            let step = |menu: &mut Menu, secs: f64| {
                let mut t = 0.0;
                while t < secs {
                    menu.update(w, h, 1.0 / 60.0);
                    t += 1.0 / 60.0;
                }
            };
            step(&mut menu, 0.5);
            for k in &keys {
                let ev = match k.as_str() {
                    "up" => Some(MenuInput::Up),
                    "down" => Some(MenuInput::Down),
                    "left" => Some(MenuInput::Left),
                    "right" => Some(MenuInput::Right),
                    "accept" => Some(MenuInput::Accept),
                    "back" => Some(MenuInput::Back),
                    "press" => Some(MenuInput::Press),
                    "release" => Some(MenuInput::Release),
                    "defaults" => Some(MenuInput::Defaults),
                    k if k.starts_with("key:") => Some(MenuInput::Key(k[4..].to_ascii_uppercase())),
                    k if k.starts_with("wheel:") => k[6..].parse().ok().map(MenuInput::Wheel),
                    k if k.starts_with("at:") => {
                        let v: Vec<f32> = k[3..].split(',').filter_map(|x| x.parse().ok()).collect();
                        (v.len() == 2).then(|| MenuInput::Pointer(v[0], v[1]))
                    }
                    k if k.starts_with("wait:") => {
                        step(&mut menu, k[5..].parse().unwrap_or(0.0));
                        None
                    }
                    "" => None,
                    k => {
                        println!("unknown input '{k}'");
                        None
                    }
                };
                if let Some(ev) = ev {
                    if let Some(e) = menu.input(ev, &mut cvars) {
                        println!("{k}: event {e:?}");
                    }
                    step(&mut menu, 0.1);
                }
            }
            step(&mut menu, time);
            if tree {
                print!("{}", menu.player.dump_tree());
            }
            for l in menu.player.log.iter().take(20) {
                println!("log: {l}");
            }
            let mut canvas = idswf::raster::Canvas::new(width as usize, height as usize);
            for p in menu.players() {
                raster_player(&assets, p, &mut canvas);
            }
            let out = out.unwrap_or_else(|| PathBuf::from("gamedata/swf/menu.png"));
            idswf::write_png(&out, width, height, &canvas.over([bg, bg, bg]))?;
            println!("wrote {}", out.display());
        }
        Cmd::Ops { names } => {
            let list: Vec<String> = if names.is_empty() { all_swfs() } else { names.iter().map(|n| swf_path(n)).collect() };
            let mut ops = BTreeMap::<String, usize>::new();
            let mut place = BTreeMap::<String, usize>::new();
            for n in &list {
                let swf = Swf::parse(&container.read_by_name(n)?)?;
                for (_, s) in sprites(&swf) {
                    let mut codes: Vec<Vec<u8>> = s.do_init_actions.clone();
                    for c in &s.commands {
                        match tags::decode(c)? {
                            Tag::DoAction(code) => codes.push(code),
                            Tag::Place(p) => {
                                let mut bump = |k: String| *place.entry(k).or_default() += 1;
                                if let Some(b) = p.blend_mode {
                                    bump(format!("blend {b}"));
                                }
                                if p.clip_depth.is_some() {
                                    bump("clipDepth".into());
                                }
                                if let Some(v) = p.visible {
                                    bump(format!("visible {v}"));
                                }
                                if p.has_filters {
                                    bump("filters".into());
                                }
                                if p.ratio.is_some() {
                                    bump("ratio".into());
                                }
                                if !p.clip_actions.is_empty() {
                                    bump("clipActions".into());
                                }
                                if p.class_name.is_some() {
                                    bump("className".into());
                                }
                                if p.cache_as_bitmap.is_some() {
                                    bump("cacheAsBitmap".into());
                                }
                                codes.extend(p.clip_actions.into_iter().map(|ca| ca.actions));
                            }
                            _ => {}
                        }
                    }
                    for code in codes {
                        let mut pc = 0;
                        while pc < code.len() {
                            let Ok((a, next)) = as2::decode(&code, pc) else {
                                *ops.entry("<decode error>".into()).or_default() += 1;
                                break;
                            };
                            let key = match &a {
                                as2::Action::Simple(op) => as2::simple_name(*op).to_string(),
                                as2::Action::Unknown(op, _) => format!("unknown {op:#x}"),
                                other => format!("{other:?}").split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_string(),
                            };
                            *ops.entry(key).or_default() += 1;
                            pc = next;
                        }
                    }
                }
            }
            for (k, v) in &ops {
                println!("{v:8} {k}");
            }
            for (k, v) in &place {
                println!("place {v:8} {k}");
            }
        }
    }
    Ok(())
}

/// `path=frame` gotoAndStop, `path.member=value` set, `call:path(args)` call.
fn apply_sets(p: &mut idswf::Player, sets: &[String]) {
    for s in sets {
        if let Some(c) = s.strip_prefix("call:") {
            let (path, args) = match c.split_once('(') {
                Some((p, a)) => (p, a.trim_end_matches(')')),
                None => (c, ""),
            };
            let args: Vec<idswf::Value> = args.split(',').filter(|a| !a.is_empty()).map(parse_value).collect();
            p.call(path, &args);
            continue;
        }
        let Some((lhs, rhs)) = s.split_once('=') else { continue };
        if let Some((path, member)) = lhs.rsplit_once('.') {
            if member.starts_with('_') || member == "text" || member == "textColor" || member == "material" {
                p.set(path, member, parse_value(rhs));
                continue;
            }
        }
        p.goto_and_stop(lhs, parse_value(rhs));
    }
}

fn parse_value(s: &str) -> idswf::Value {
    let s = s.trim();
    if let Ok(n) = s.parse::<f64>() {
        idswf::Value::Num(n)
    } else if s == "true" || s == "false" {
        idswf::Value::Bool(s == "true")
    } else {
        idswf::Value::str(s.trim_matches('"'))
    }
}

fn print_timeline(swf: &Swf, label: &str, s: &Sprite) {
    println!("{label}: {} frames, labels {:?}", s.frame_count, s.frame_labels);
    for f in 0..s.frame_count as usize {
        for c in s.frame_commands(f) {
            match tags::decode(c) {
                Ok(Tag::Place(p)) => {
                    let kind = p.character.and_then(|id| swf.dict.get(id as usize)).map(|e| e.type_name()).unwrap_or("");
                    let m = p.matrix.map(|m| format!(" m[{:.3} {:.3} {:.3} {:.3} {:.1} {:.1}]", m.xx, m.yy, m.xy, m.yx, m.tx, m.ty)).unwrap_or_default();
                    let cx = p.cxform.map(|c| format!(" cx{:?}+{:?}", c.mul, c.add)).unwrap_or_default();
                    println!(
                        "  f{f} {}{} d{} {}{}{}{}{}{}{}",
                        if p.is_move { "move" } else { "place" },
                        p.version,
                        p.depth,
                        p.character.map(|c| format!("#{c} {kind} ")).unwrap_or_default(),
                        p.name.as_ref().map(|n| format!("'{n}'")).unwrap_or_default(),
                        m,
                        cx,
                        p.clip_depth.map(|d| format!(" clip{d}")).unwrap_or_default(),
                        p.blend_mode.map(|b| format!(" blend{b}")).unwrap_or_default(),
                        p.visible.map(|v| format!(" vis{v}")).unwrap_or_default(),
                    );
                }
                Ok(Tag::Remove { depth }) => println!("  f{f} remove d{depth}"),
                Ok(Tag::DoAction(code)) => println!("  f{f} doAction {} bytes", code.len()),
                Ok(Tag::Other(t)) => println!("  f{f} tag {t}"),
                Err(e) => println!("  f{f} tag {} ERROR {e}", c.tag),
            }
        }
    }
}
