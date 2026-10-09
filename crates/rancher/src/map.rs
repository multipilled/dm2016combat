//! Real DOOM (2016) maps as test arenas: `RANCHER_MAP=game/sp/intro/intro` loads
//! `maps/<map>.entities`, the combined world geometry `maps/<map>/_combo/_world.bmodel` and its
//! collision `maps/<map>/_combo/world.bcm` from the user's install (formats: gamedata/re/MAPS.md).
//!
//! * [`load`] (before the app starts): entities, the initial player start and the collision World.
//! * [`MapPlugin`]: builds render meshes on a worker thread and streams them into the scene. World
//!   surfaces use the map's unique material (`maps/<map>/mega`), whose vertices name their source VT
//!   material per vertex (vmtrTC/vmtrSB); they render with that material through crate::vtmat.
//!   Every `func_static` is already merged into `_world.bmodel` by the map build, so only other
//!   entities with static models (movers, `func_dynamic` megamodels, props) are added.
//!
//!   Surfaces whose material has a `landPageFile` (the map's `mega`/`megatrans`, also used by the
//!   megamodels) get the baked HDR lightmap from the map's unique virtual texture
//!   (`virtualtextures/maps/<map>.pages`, idres::vtex::unique), bound on their VT materials
//!   (`vtmat::set_lightmap`) and sampled with UV1 = `st`, scaled like lighting.inc's
//!   `ambient = lightmap * lightMapScale * envLightMapScale`.
//!
//! The lightmap is on by default (`RANCHER_MAP_LIGHTMAP=0` disables, `RANCHER_MAP_LIGHTMAP_LEVEL`
//! picks the level), scaled by lightMapScale (renderparm default 4) × envLightmapScale in engine
//! radiance units; post.rs's engine auto exposure brings it to screen brightness.
//!
//! With the lightmap, the map's idLights (not static/startOff) feed the engine's shading in the VT
//! materials (vtmat::lighting: projected lights, environment probes, the ambient octree); run-time
//! lights also get a shadowless Bevy point light each, only for Bevy's light clustering
//! (see `spawn_lights`).
//!
//! Static md6 object props (see `md6_prop`) are drawn in their bind pose; surface materials outside
//! the virtual texture get decl-based glass / glow looks (`Placeholder`).
//!
//! Movers (idMover move commands) and everything bound to them are separate entities driven by
//! `movers` (activation test hook `RANCHER_MAP_ACTIVATE`).
//!
//! INTERIM: the lightmap is one whole level (no per-pixel level selection, bilinear only); no auto
//! exposure; no shadow maps; no decals,
//! particles or flares; all layers shown. Alpha-prepass materials (`megatrans`) use the masked VT
//! variant.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context, Result};
use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use idres::entities::{EntitiesFile, Entity};
use idres::{Container, bcm, bmodel};
use rancher_sim::Vec3 as V;
use rancher_sim::collision::{Hull, Mat3, World as SimWorld};
use rancher_sim::config::MoveConfig;

use crate::range::to_bevy;
use crate::vtmat::{self, VtMaterial, VtMaterials};

pub mod movers;
use movers::{MoverSpec, Movers};

/// Systems that must run after the player's game frame (movers push/carry the player). The app
/// orders it: `.configure_sets(Update, map::MapSet::Movers.after(<player tick>))`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MapSet {
    Movers,
}

/// Finest VT level streamed for map materials (0 = full resolution). Level 3 keeps intro's 525
/// materials around 0.4 GB of texture memory (level 2: ~1.6 GB). `RANCHER_MAP_VT_LEVEL` overrides.
const MAP_VT_LEVEL: usize = 3;
/// Meshes spawned per frame while streaming.
const SPAWN_PER_FRAME: usize = 400;
/// Triangles are merged into one mesh per material per cube of this size (world units, by the
/// triangle's first vertex): intro goes from 17k per-surface meshes to ~1.3k.
const MERGE_CELL: f32 = 32768.0;
/// Unique-VT level used as the lightmap (intro: level 3 = 7680² texels, 59 MB of BC6H; level 1 is
/// the finest stored). `RANCHER_MAP_LIGHTMAP_LEVEL` overrides.
const LIGHTMAP_LEVEL: usize = 3;

pub struct MapLoad {
    pub name: String,
    /// Collision for the sim: `world.bcm` plus the entities' collision models (rancher_sim polygon models).
    pub world: SimWorld,
    pub start: V,
    /// Player start yaw in degrees (from the start entity's axis).
    pub start_yaw: f32,
    /// Entities with static render models that `_world.bmodel` does not contain.
    pub props: Vec<Placed<String>>,
    /// lighting.inc's lightmap factor `$lightMapScale.x * $envLightMapScale.x`.
    pub lightmap_scale: f32,
    /// Run-time (non-static) idLights and light probes.
    pub lights: Vec<MapLight>,
    /// The engine lighting inputs built from them (taken by `MapPlugin`).
    pub scene: Arc<Mutex<Option<vtmat::lighting::SceneData>>>,
    /// Movers and the entities bound to them (drawn as their own entities, moved by `movers`).
    pub movers: Vec<MoverSpec>,
    /// World poses of the entities move commands name as `position`.
    pub mover_targets: std::collections::HashMap<String, (V, Mat3)>,
    /// Render models of mover/bound entities (model space; placed by the mover runtime).
    pub dynamic_props: Vec<Placed<String>>,
}

/// An idLight as the renderer gets it: the engine's render parms (vtmat::lighting::LightDef).
pub type MapLight = vtmat::lighting::LightDef;

/// Something placed by an entity: model-to-world p' = origin + axis[0]*p.x + axis[1]*p.y + axis[2]*p.z
/// after scaling (axis rows = spawnOrientation.mat rows, the entity's axes in world space).
#[derive(Clone, Debug)]
pub struct Placed<T> {
    pub entity: String,
    pub item: T,
    pub origin: [f32; 3],
    pub axis: [[f32; 3]; 3],
    pub scale: [f32; 3],
}

fn read(c: &Container, name: &str) -> Result<Vec<u8>> {
    c.read_by_name(name).with_context(|| format!("reading {name}"))
}

/// The player start: `RANCHER_MAP_START` names an idPlayerStart entity or a checkpoint
/// (`checkpointName`, whose first `playerSpawnSpots` entry is used); default is the start marked
/// `initial = true` (the new-game start; intro's is the scripted wake-up on the slab, inside its clip).
fn select_start<'a>(ents: &'a EntitiesFile, want: Option<&str>) -> Result<&'a Entity> {
    let Some(want) = want else {
        return ents.initial_player_start().context("map has no idPlayerStart");
    };
    let by_name = |n: &str| ents.entities.iter().find(|e| e.name == n && e.class() == Some("idPlayerStart"));
    if let Some(e) = by_name(want) {
        return Ok(e);
    }
    for e in &ents.entities {
        let Some(edit) = e.edit() else { continue };
        if edit.str("checkpointName") == Some(want) {
            if let Some(spot) = edit.str("playerSpawnSpots.item[0]").and_then(by_name) {
                return Ok(spot);
            }
        }
    }
    let starts: Vec<&str> = ents.by_class("idPlayerStart").map(|e| e.name.as_str()).collect();
    anyhow::bail!("no player start or checkpoint named {want}; starts: {}", starts.join(", "))
}

/// The player's standing trace model, built like `Player::new` does.
fn player_trm(cfg: &MoveConfig) -> Hull {
    let half = cfg.bbox_width * 0.5;
    Hull::player_trace_model(cfg.collision_style, V::new(-half, -half, 0.0), V::new(half, half, cfg.normal_height), cfg.pencil_collision_angle, cfg.pencil_collision_taper_radius)
}

/// Loads the map's entities, player start and collision. `map` is the path under `maps/`
/// (e.g. `game/sp/intro/intro`); `cfg` gives the player's size for the start check.
pub fn load(c: &Container, decls: Option<&idres::decldb::DeclDb>, cfg: &MoveConfig, map: &str) -> Result<MapLoad> {
    let t = std::time::Instant::now();
    let text = read(c, &format!("maps/{map}.entities"))?;
    let entities = idres::entities::parse(&String::from_utf8_lossy(&text)).with_context(|| format!("parsing maps/{map}.entities"))?;
    let chosen = std::env::var("RANCHER_MAP_START").ok().filter(|s| !s.is_empty());
    let start = select_start(&entities, chosen.as_deref())?;

    let cm_name = format!("maps/{map}/_combo/world.bcm");
    let cm = bcm::CollisionModel::parse(&read(c, &cm_name)?).with_context(|| format!("parsing {cm_name}"))?;
    let floor = floor_below(&cm, start.origin());
    let (props, entity_cms) = placements(&entities, c, decls, map);
    eprintln!(
        "map {map}: {} entities, start {} at {:?}, {} collision polygons in {} submodels (floor below start {floor:?}), {} entity collision models, {} props, {:.2}s",
        entities.entities.len(),
        start.name,
        start.origin(),
        cm.polygon_count(),
        cm.submodels.len(),
        entity_cms.len(),
        props.len(),
        t.elapsed().as_secs_f32()
    );
    // The world model is in world coordinates; entity models are placed by their entity (axis rows
    // become the columns: model p -> origin + M·p). All layers' entities are added; the clip mask
    // leaves out triggers and volumes.
    let mut world = SimWorld::default();
    world.add_cm(Arc::new(cm), V::ZERO, Mat3::IDENTITY);
    let mut cm_ids = std::collections::HashMap::new();
    for p in entity_cms {
        let m = Mat3::from_cols(V::from(p.axis[0]), V::from(p.axis[1]), V::from(p.axis[2]));
        cm_ids.insert(p.entity.clone(), world.add_cm(p.item, V::from(p.origin), m));
    }
    let (movers, mover_targets) = mover_specs(&entities, decls, &cm_ids);
    // Movers' collision pushes and carries the player (rancher_sim). INTERIM: default MoverProps
    // (isPusher, no preventPlayerJump / dislodgePlayer) until the physics decl values are read.
    for m in &movers {
        if let Some(cm) = m.cm {
            world.set_cm_mover(cm, rancher_sim::collision::MoverProps::default());
        }
    }
    let dynamic: std::collections::HashSet<&str> = movers.iter().map(|m| m.entity.as_str()).collect();
    let (mut dynamic_props, props): (Vec<_>, Vec<_>) = props.into_iter().partition(|p| dynamic.contains(p.entity.as_str()));
    // Dynamic render models are built in their entity's space (the mover runtime places them):
    // local = axisᵀ·(placement origin − entity origin) (an md6Def offset), identity axis, scale kept.
    for p in &mut dynamic_props {
        if let Some(m) = movers.iter().find(|m| m.entity == p.entity) {
            let d = V::from(p.origin) - m.origin;
            p.origin = (m.axis.transpose() * d).to_array();
            p.axis = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        }
    }

    // Diagnostic only: the game spawns at the start regardless (intro's new-game start is the
    // scripted wake-up inside the slab's player clip; from origin + 5 the step-up puts the player on
    // the slab).
    if !world.position_clear(&player_trm(cfg), spawn_position(start)) {
        eprintln!("map: note: the player's trace model overlaps collision at start {} (the game spawns there too)", start.name);
    }
    let origin = spawn_position(start);
    let axis = start.axis();
    // RANCHER_MAP_YAW overrides the start's facing (degrees; test captures).
    let start_yaw = std::env::var("RANCHER_MAP_YAW").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| axis[0][1].atan2(axis[0][0]).to_degrees());
    eprintln!("map {map}: start {} spawn position {origin:?} yaw {start_yaw:.1}", start.name);
    let lightmap_scale = renderparm_default(c, "lightmapscale").unwrap_or(1.0) * env_lightmap_scale(c, &entities);
    eprintln!("map {map}: lightmap scale {lightmap_scale} (lightMapScale x envLightmapScale)");
    let lights = map_lights(&entities);
    let scene = if lightmap_enabled() { Some(vtmat::lighting::build_scene(c, map, &lights)) } else { None };
    let scene = Arc::new(Mutex::new(scene));
    eprintln!("map {map}: {} movers/bound entities, {} with render models", movers.len(), dynamic_props.len());
    Ok(MapLoad { name: map.to_string(), world, start: origin, start_yaw, props, lightmap_scale, lights, scene, movers, mover_targets, dynamic_props })
}

/// Every idMover plus everything bound to one (transitively through `bindInfo.bindParent`), with the
/// world poses of the entities their move commands target.
fn mover_specs(
    ents: &EntitiesFile,
    decls: Option<&idres::decldb::DeclDb>,
    cm_ids: &std::collections::HashMap<String, usize>,
) -> (Vec<MoverSpec>, std::collections::HashMap<String, (V, Mat3)>) {
    let axis = |e: &Entity| {
        let a = e.axis();
        Mat3::from_cols(V::from(a[0]), V::from(a[1]), V::from(a[2]))
    };
    let edits: Vec<(&Entity, idres::decl::Block)> = ents.entities.iter().map(|e| (e, e.edit_merged(decls))).collect();
    let mut set: std::collections::HashSet<&str> = edits.iter().filter(|(e, _)| e.class() == Some("idMover")).map(|(e, _)| e.name.as_str()).collect();
    loop {
        let before = set.len();
        for (e, edit) in &edits {
            if let Some(b) = Entity::bind(edit) {
                if set.contains(b.parent.as_str()) {
                    set.insert(e.name.as_str());
                }
            }
        }
        if set.len() == before {
            break;
        }
    }
    let mut specs = Vec::new();
    let mut wanted = std::collections::HashSet::new();
    for (e, edit) in &edits {
        if !set.contains(e.name.as_str()) {
            continue;
        }
        let commands = Entity::move_commands(edit);
        wanted.extend(commands.iter().filter(|c| !c.position.is_empty()).map(|c| c.position.clone()));
        specs.push(MoverSpec {
            entity: e.name.clone(),
            origin: V::from(e.origin()),
            axis: axis(e),
            bind: Entity::bind(edit),
            commands,
            cm: cm_ids.get(&e.name).copied(),
        });
    }
    let targets = ents.entities.iter().filter(|e| wanted.contains(&e.name)).map(|e| (e.name.clone(), (V::from(e.origin()), axis(e)))).collect();
    (specs, targets)
}

/// The map's idLights as render parms: every idLight except `staticLight` (baked only) and
/// `startOff` ones. Missing edit keys keep the idLight constructor defaults (0x140944836): colour
/// (1,1,1), intensity 1, radius (320,320,320), centre 0, spot target (64,0,0) right (0,-64,0) up
/// (0,0,64), dynamicLightSpecularScale 1; lightType defaults to LIGHT_POINT (light/point entityDef).
/// Spot start defaults to 0 and end to the target (idLight -> render parms 0x1409463f0 copies
/// lightEnd only when it is non-zero); fade keys default to 0 / 400 / false (VisibilityFade).
/// Area lights keep radius / centre (box projection) and their `areaLight` frustum (its shading
/// branch is not ported, lighting.wgsl).
fn map_lights(ents: &EntitiesFile) -> Vec<MapLight> {
    use vtmat::lighting::{LightDef, LightKind};
    let mut out = Vec::new();
    for e in ents.by_class("idLight") {
        let Some(edit) = e.edit() else { continue };
        let kind = match edit.str("lightType").unwrap_or("LIGHT_POINT") {
            "LIGHT_SPOT" => LightKind::Spot,
            "LIGHT_PARALLEL" => LightKind::Parallel,
            "LIGHT_PROBE" => LightKind::Probe,
            "LIGHT_AREA" => LightKind::Area,
            _ => LightKind::Point,
        };
        let flag = |k: &str| edit.get(k).and_then(idres::decl::Value::as_bool).unwrap_or(false);
        if flag("staticLight") || flag("startOff") {
            continue;
        }
        let v = |k: &str, d: [f32; 3]| Vec3::from(idres::entities::vec3(edit.block(k), d));
        let col = edit.block("lightColor");
        let c = |k: &str| col.and_then(|b| b.f32(k)).unwrap_or(1.0);
        let intensity = edit.f32("lightIntensity").unwrap_or(1.0);
        let frustum = if kind == LightKind::Area { "areaLight" } else { "spotLight" };
        let target = v(&format!("{frustum}.lightTarget"), [64.0, 0.0, 0.0]);
        let ax = e.axis();
        out.push(LightDef {
            name: e.name.clone(),
            kind,
            origin: Vec3::from(e.origin()),
            axis: [Vec3::from(ax[0]), Vec3::from(ax[1]), Vec3::from(ax[2])],
            radius: v("lightRadius", [320.0; 3]),
            center: v("lightCenter", [0.0; 3]),
            target,
            right: v(&format!("{frustum}.lightRight"), [0.0, -64.0, 0.0]),
            up: v(&format!("{frustum}.lightUp"), [0.0, 0.0, 64.0]),
            start: v(&format!("{frustum}.lightStart"), [0.0; 3]),
            end: v(&format!("{frustum}.lightEnd"), target.to_array()),
            color: Vec3::new(c("r"), c("g"), c("b")) * intensity,
            spec_scale: edit.f32("dynamicLightSpecularScale").unwrap_or(1.0),
            dynamic_only: flag("dynamicOnly"),
            cast_shadows: flag("castShadows"),
            probe_inner_falloff: edit.f32("lightProbeInnerFalloff").unwrap_or(0.0),
            material: edit.str("lightMaterial").map(str::to_string),
            // idLight edit +0xda0 / +0xda4 / +0xda8 (ctor 0x140944810 defaults in VisibilityFade)
            fade: {
                let d = vtmat::lighting::VisibilityFade::default();
                vtmat::lighting::VisibilityFade {
                    max_range: edit.f32("maxVisibleRange").unwrap_or(d.max_range),
                    fade_over: edit.f32("fadeVisibilityOver").unwrap_or(d.fade_over),
                    flip: edit.get("flipFadeVisibility").and_then(idres::decl::Value::as_bool).unwrap_or(d.flip),
                }
            },
        });
    }
    out
}

/// Where a spawn node puts what spawns at it: idSpawnNode (idPlayerStart's base) generates its spawn
/// location as origin + (0, 0, 5) facing its axis (0x140e8fb90, constant 5.0 at 0x14200202c), reached
/// through the node's "gather spawn locations" virtual 0x140e8e670 (team match, isActive, and the
/// `initial` flag when only initial spawns are wanted). The player drops the 5 units onto the floor.
const SPAWN_LOCATION_RAISE: f32 = 5.0;

fn spawn_position(e: &Entity) -> V {
    V::from(e.origin()) + V::Z * SPAWN_LOCATION_RAISE
}

/// The default value of a renderparm decl (`generated/decls/renderparm/<name>.decl`, e.g.
/// `{ Vec 4.0 setFreqLow }`). lightMapScale is never written by engine code (hdp-exact: its only
/// reference is the static parm object 0x143657270 linked by 0x1402195e0), so the default is what
/// the shader sees; a single value applies to all components.
fn renderparm_default(c: &Container, name: &str) -> Option<f32> {
    let text = c.read_by_name(&format!("generated/decls/renderparm/{name}.decl")).ok()?;
    let text = String::from_utf8_lossy(&text);
    let mut it = text.split(|ch: char| ch.is_whitespace() || ch == '{' || ch == '}').filter(|t| !t.is_empty());
    it.find(|t| t.eq_ignore_ascii_case("Vec") || t.eq_ignore_ascii_case("Float"))?;
    it.next()?.parse().ok()
}

/// `renderParms.envLightmapScale` of the env decl the worldspawn names in `edit.envSettings`.
/// Env decls are `key value` text (`renderParms { envLightmapScale 0.800 }`) with `inherit { <env> }`;
/// the value is looked up along that chain (default 1).
fn env_lightmap_scale(c: &Container, ents: &EntitiesFile) -> f32 {
    let mut env = ents.by_class("idWorldspawn").next().and_then(|w| w.edit()).and_then(|e| e.str("envSettings").map(str::to_string));
    for _ in 0..8 {
        let Some(name) = env.take() else { break };
        let Ok(text) = c.read_by_name(&format!("generated/decls/env/{name}.decl")) else { break };
        let text = String::from_utf8_lossy(&text);
        let toks: Vec<&str> = text.split(|ch: char| ch.is_whitespace() || ch == '{' || ch == '}').filter(|t| !t.is_empty()).collect();
        if let Some(i) = toks.iter().position(|t| t.eq_ignore_ascii_case("envLightmapScale")) {
            return toks.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1.0);
        }
        env = toks.iter().position(|t| *t == "inherit").and_then(|i| toks.get(i + 1)).map(|t| t.trim_matches('"').to_string());
    }
    1.0
}

/// Highest upward-facing solid collision polygon directly below `p` (within 512 units).
fn floor_below(cm: &bcm::CollisionModel, p: [f32; 3]) -> Option<f32> {
    let mut best: Option<f32> = None;
    for sm in &cm.submodels {
        if p[0] < sm.bounds[0][0] || p[0] > sm.bounds[1][0] || p[1] < sm.bounds[0][1] || p[1] > sm.bounds[1][1] {
            continue;
        }
        for poly in &sm.polygons {
            let [lo, hi] = poly.bounds;
            if (p[0] as i32) < lo[0] as i32 || (p[0] as i32) > hi[0] as i32 || (p[1] as i32) < lo[1] as i32 || (p[1] as i32) > hi[1] as i32 {
                continue;
            }
            if sm.surfaces.get(poly.surface as usize).is_none_or(|s| s.contents & bcm::contents::SOLID == 0) {
                continue;
            }
            let pl = sm.polygon_plane(poly);
            if pl[2] < 0.7 {
                continue;
            }
            // Point-in-polygon in xy (the loop's winding is CCW seen along +n).
            let vs: Vec<[f32; 3]> = sm.polygon_verts(poly).map(|v| sm.verts[v]).collect();
            let inside = (0..vs.len()).all(|k| {
                let (a, b) = (vs[k], vs[(k + 1) % vs.len()]);
                (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]) >= 0.0
            });
            if !inside {
                continue;
            }
            let z = -(pl[0] * p[0] + pl[1] * p[1] + pl[3]) / pl[2];
            if z <= p[2] + 1.0 && z >= p[2] - 512.0 && best.is_none_or(|b| z > b) {
                best = Some(z);
            }
        }
    }
    best
}

/// Which material a streamed mesh uses.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum MatKey {
    /// A material in the shared virtual texture (`_vmtr*.vmtr`).
    Vt(String),
    /// The same, alpha-tested on the VT cover mask (surfaces of an alpha-prepass material such as
    /// the map's `megatrans`: zPrePassProgram preZDrawAlphaUnique clips at 0.5).
    VtMasked(String),
    /// Not in the virtual texture (emissive sfx, glass, ...): flat placeholder.
    Flat(String),
}

enum Msg {
    /// A mesh; `true` when it carries lightmap texcoords (UV1).
    Mesh(Mesh, MatKey, bool),
    /// The unique-VT level used as lightmap (BC6H unsigned float blocks).
    Lightmap(idres::vtex::unique::UniqueLevel),
    /// How to draw a surface material that is not in the virtual texture (sent before its meshes).
    Placeholder(String, Placeholder),
    /// A mover/bound entity's render model in model space.
    Dynamic { entity: String, meshes: Vec<(Mesh, MatKey, bool)> },
    Done { meshes: usize, masked: usize, tris: usize, secs: f32 },
    Error(String),
}

#[derive(Resource)]
struct MapStream {
    rx: Mutex<Receiver<Msg>>,
    vtm: VtMaterials,
    level: usize,
    flats: std::collections::HashMap<String, Handle<VtMaterial>>,
    lightmap: Option<Handle<Image>>,
    /// vtmat lightmap scale: the shader adds albedo * lightmap * scale * view exposure (Bevy's
    /// lightmap convention), so this cancels the default camera exposure and applies the engine's scales.
    lightmap_exposure: f32,
    /// Materials the lightmap is already bound to.
    lit_materials: std::collections::HashSet<AssetId<VtMaterial>>,
    /// Materials already flagged as static models.
    static_materials: std::collections::HashSet<AssetId<VtMaterial>>,
    placeholders: std::collections::HashMap<String, Placeholder>,
    std_flats: std::collections::HashMap<String, Handle<StandardMaterial>>,
    /// Masked materials already made two-sided.
    two_sided: std::collections::HashSet<AssetId<VtMaterial>>,
    spawned: usize,
    done: bool,
}

/// Streams the map's render geometry into the scene (see the module docs).
pub struct MapPlugin {
    pub doom: PathBuf,
    pub map: String,
    pub props: Vec<Placed<String>>,
    pub dynamic_props: Vec<Placed<String>>,
    pub lightmap_scale: f32,
    pub lights: Vec<MapLight>,
    pub scene: Arc<Mutex<Option<vtmat::lighting::SceneData>>>,
    pub movers: Vec<MoverSpec>,
    pub mover_targets: std::collections::HashMap<String, (V, Mat3)>,
}

impl MapPlugin {
    pub fn new(doom: PathBuf, load: &MapLoad) -> Self {
        MapPlugin {
            doom,
            map: load.name.clone(),
            props: load.props.clone(),
            dynamic_props: load.dynamic_props.clone(),
            lightmap_scale: load.lightmap_scale,
            lights: load.lights.clone(),
            scene: load.scene.clone(),
            movers: load.movers.clone(),
            mover_targets: load.mover_targets.clone(),
        }
    }
}

fn placed<T>(e: &Entity, edit: &idres::decl::Block, item: T) -> Placed<T> {
    Placed { entity: e.name.clone(), item, origin: e.origin(), axis: e.axis(), scale: Entity::render_scale(edit) }
}

/// Entity classes whose md6 render models are drawn as static props (in the mesh's stored bind
/// pose): interactables, pickups, anim-web set dressing. Not AI, ragdoll corpses (idCorpse:
/// articulatedFigure, posed by physics at run time), cinematics, or doors (idInteractable_Obstacle:
/// opened through their anim web, and their blocking clip is not modelled yet — a closed door you
/// can walk through would mislead).
const MD6_PROP_CLASSES: &[&str] = &["idInteractable", "idInteractable_WeaponModBot", "idProp2", "idAnimated_AnimWeb"];

/// An md6 model as a static prop: `<model>.md6` names an md6Def decl whose `mesh` is the
/// `.bmd6model`; its `offset` is folded into the placement. Only `zion/objects/` models (character
/// bind poses are T-poses).
fn md6_prop(e: &Entity, edit: &idres::decl::Block, model: &str, c: &Container) -> Option<Placed<String>> {
    if !model.ends_with(".md6") || !model.starts_with("zion/objects/") || !e.class().is_some_and(|k| MD6_PROP_CLASSES.contains(&k)) {
        return None;
    }
    let def = crate::assets::read_md6def(c, model).ok()?;
    let res = crate::assets::model_resource(&def.mesh);
    c.get(&res)?;
    let mut p = placed(e, edit, res);
    let ax = p.axis;
    for i in 0..3 {
        p.origin[i] += (0..3).map(|k| ax[k][i] * def.offset[k] * p.scale[k]).sum::<f32>();
    }
    Some(p)
}

/// `models/<path>.<ext>` -> `generated/cm/models/<path>.bcm`.
fn cm_resource_for_model(model: &str) -> String {
    let stem = model.rsplit_once('.').map_or(model, |(s, _)| s);
    format!("generated/cm/{stem}.bcm")
}

/// Render models and collision models of the map's entities.
/// * func_static (idStaticEntity) is skipped: the map build merged its render geometry into
///   `_world.bmodel` (checked against the world's vertices; most of those models are not even shipped)
///   and its collision into `world.bcm`.
/// * render: `renderModelInfo.model` when it names a static model (`.lwo` -> `cooked/model/..bmodel`,
///   `_combo/megamodel_*.bmodel`, both in entity space); md6 models are skipped.
/// * collision: `clipModelInfo.type CLIPMODEL_NONE` -> none; else `clipModelInfo.clipModelName`
///   (`generated/cm/<model>.bcm`), else the entity's brush model `maps/<map>/<entity>.bcm`, else the
///   render model's `generated/cm` model. Triggers and volumes are included (contents tell them apart).
fn placements(ents: &EntitiesFile, c: &Container, decls: Option<&idres::decldb::DeclDb>, map: &str) -> (Vec<Placed<String>>, Vec<Placed<Arc<bcm::CollisionModel>>>) {
    let (mut props, mut cms) = (Vec::new(), Vec::new());
    let mut cm_cache: std::collections::HashMap<String, Option<Arc<bcm::CollisionModel>>> = Default::default();
    for e in &ents.entities {
        if e.class() == Some("idStaticEntity") {
            continue;
        }
        let edit = e.edit_merged(decls);
        let model = Entity::render_model(&edit);
        if let Some(r) = model.and_then(bmodel::resource_for_model).filter(|r| c.get(r).is_some()) {
            props.push(placed(e, &edit, r));
        } else if let Some(p) = model.and_then(|m| md6_prop(e, &edit, m, c)) {
            props.push(p);
        }
        if edit.str("clipModelInfo.type") == Some("CLIPMODEL_NONE") {
            continue;
        }
        let candidates = [
            edit.str("clipModelInfo.clipModelName").filter(|n| !n.is_empty()).map(cm_resource_for_model),
            Some(format!("maps/{map}/{}.bcm", e.name)),
            model.filter(|m| bmodel::resource_for_model(m).is_some() && !m.ends_with(".bmodel")).map(cm_resource_for_model),
        ];
        let Some(res) = candidates.into_iter().flatten().find(|r| c.get(r).is_some()) else { continue };
        let cm = cm_cache
            .entry(res.clone())
            .or_insert_with(|| match c.read_by_name(&res).and_then(|b| bcm::CollisionModel::parse(&b)) {
                Ok(m) => Some(Arc::new(m)),
                Err(err) => {
                    eprintln!("map: {} ({res}): {err:#}", e.name);
                    None
                }
            })
            .clone();
        if let Some(cm) = cm {
            // INTERIM assumption: collision models are not scaled with the render model.
            let mut p = placed(e, &edit, cm);
            p.scale = [1.0; 3];
            cms.push(p);
        }
    }
    (props, cms)
}

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = channel();
        let (doom, map, props, dynamic) = (self.doom.clone(), self.map.clone(), self.props.clone(), self.dynamic_props.clone());
        let spawned = std::thread::Builder::new().name("map-meshes".into()).spawn(move || {
            if let Err(e) = build_meshes(&doom, &map, &props, &dynamic, &tx) {
                let _ = tx.send(Msg::Error(format!("{e:#}")));
            }
        });
        if let Err(e) = spawned {
            eprintln!("map: no mesh thread: {e}");
        }
        let level = std::env::var("RANCHER_MAP_VT_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(MAP_VT_LEVEL);
        // lighting.inc: ambient = lightmap * lightMapScale * envLightMapScale, in the engine's radiance
        // units: the cameras render with view.exposure 1 (post::ENGINE_EV100) and post.rs applies the
        // engine's auto exposure. RANCHER_MAP_LIGHTMAP_SCALE multiplies (experiments; default 1).
        let extra: f32 = std::env::var("RANCHER_MAP_LIGHTMAP_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        let lightmap_exposure = extra * self.lightmap_scale / camera_exposure();
        app.insert_resource(MapStream {
            rx: Mutex::new(rx),
            vtm: VtMaterials::open(&self.doom),
            level,
            flats: Default::default(),
            lightmap: None,
            lightmap_exposure,
            lit_materials: Default::default(),
            static_materials: Default::default(),
            placeholders: Default::default(),
            std_flats: Default::default(),
            two_sided: Default::default(),
            spawned: 0,
            done: false,
        })
        .insert_resource(Movers::new(self.movers.clone(), self.mover_targets.clone()))
        .add_systems(Startup, movers::spawn_roots)
        .add_systems(Update, (stream, movers::tick_movers.in_set(MapSet::Movers)));
        // The map's run-time lights add to the baked lightmap, so they come with it (the app's range
        // sun stands in for all lighting otherwise).
        if lightmap_enabled() {
            let scene = self.scene.lock().unwrap().take();
            let indices = scene.as_ref().map(|s| s.light_indices.clone()).unwrap_or_default();
            let (lights, extra) = (self.lights.clone(), extra);
            app.add_systems(Startup, move |mut commands: Commands| spawn_lights(&mut commands, &lights, &indices, extra));
            app.insert_resource(vtmat::PendingScene(Mutex::new(scene)));
        }
    }
}

fn stream(
    mut commands: Commands,
    mut ms: ResMut<MapStream>,
    movers: Res<Movers>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<VtMaterial>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if ms.done {
        return;
    }
    let ms = &mut *ms;
    let msgs: Vec<Msg> = {
        let rx = ms.rx.lock().unwrap();
        (0..SPAWN_PER_FRAME).map_while(|_| rx.try_recv().ok()).collect()
    };
    let mut a = SpawnAssets { meshes: &mut meshes, mats: &mut mats, std_mats: &mut std_mats, images: &mut images };
    for msg in msgs {
        match msg {
            Msg::Lightmap(l) => {
                let mut img = Image::new_uninit(
                    Extent3d { width: l.width, height: l.height, depth_or_array_layers: 1 },
                    TextureDimension::D2,
                    TextureFormat::Bc6hRgbUfloat,
                    RenderAssetUsages::RENDER_WORLD,
                );
                img.data = Some(l.blocks);
                img.sampler = ImageSampler::linear();
                eprintln!("map: lightmap level {} {}x{} ({} pages missing)", l.level, l.width, l.height, l.missing_pages);
                ms.lightmap = Some(a.images.add(img));
            }
            Msg::Placeholder(name, p) => {
                ms.placeholders.insert(name, p);
            }
            Msg::Dynamic { entity, meshes: parts } => {
                let Some(root) = movers.render(&entity) else { continue };
                for (mesh, key, lit) in parts {
                    let child = ms.spawn_mesh(&mut commands, &mut a, mesh, &key, lit, false);
                    commands.entity(root).add_child(child);
                }
            }
            Msg::Mesh(mesh, key, lit) => {
                ms.spawn_mesh(&mut commands, &mut a, mesh, &key, lit, true);
            }
            Msg::Done { meshes, masked, tris, secs } => {
                eprintln!("map: {meshes} meshes ({masked} alpha-tested), {tris} triangles built in {secs:.1}s");
                ms.done = true;
                return;
            }
            Msg::Error(e) => {
                eprintln!("map: {e}");
                ms.done = true;
                return;
            }
        }
    }
}

struct SpawnAssets<'a> {
    meshes: &'a mut Assets<Mesh>,
    mats: &'a mut Assets<VtMaterial>,
    std_mats: &'a mut Assets<StandardMaterial>,
    images: &'a mut Assets<Image>,
}

impl MapStream {
    /// Spawns one mesh with its material: the VT material (with the lightmap bound when the mesh is
    /// lightmapped), else the material's decl-based placeholder, else a flat placeholder.
    /// `is_static`: map geometry that never moves ($staticModel.x; movers pass false).
    fn spawn_mesh(&mut self, commands: &mut Commands, a: &mut SpawnAssets, mut mesh: Mesh, key: &MatKey, lit: bool, is_static: bool) -> bevy::ecs::entity::Entity {
        self.spawned += 1;
        let name = match key {
            MatKey::Vt(n) | MatKey::VtMasked(n) | MatKey::Flat(n) => n.clone(),
        };
        let vt = match key {
            MatKey::Vt(n) => self.vtm.material(n, self.level, a.images, a.mats),
            MatKey::VtMasked(n) => {
                let h = self.vtm.material_masked(n, self.level, a.images, a.mats);
                // shadowVmtrTransUnique is `twosided`: grates and foliage are seen from both sides.
                if let Some(h) = &h {
                    if self.two_sided.insert(h.id()) {
                        if let Some(mut m) = a.mats.get_mut(h) {
                            m.base.cull_mode = None;
                            m.base.double_sided = true;
                        }
                    }
                }
                h
            }
            MatKey::Flat(_) => None,
        };
        if vt.is_none() {
            // Only the VT shading reads the vertex colour (emissive); Bevy's materials would tint
            // their base colour with it.
            mesh.remove_attribute(Mesh::ATTRIBUTE_COLOR);
            // Glass and glows from their decls (plain StandardMaterial: blending / unlit).
            if let Some(p) = self.placeholders.get(&name).cloned() {
                let h = self.std_flats.entry(name).or_insert_with(|| a.std_mats.add(p.material(a.images))).clone();
                return commands.spawn((Mesh3d(a.meshes.add(mesh)), MeshMaterial3d(h))).id();
            }
        }
        let mat = vt.unwrap_or_else(|| self.flats.entry(name.clone()).or_insert_with(|| vtmat::flat(a.mats, placeholder(&name))).clone());
        // The lightmap is a material binding (vtmat); materials are shared per name, and only meshes
        // with UV1 (lit ones) sample it.
        if let (true, Some(lm)) = (lit, &self.lightmap) {
            if self.lit_materials.insert(mat.id()) {
                if let Some(mut m) = a.mats.get_mut(&mat) {
                    vtmat::set_lightmap(&mut m.extension, Some(lm.clone()), self.lightmap_exposure);
                }
            }
        }
        // Static models skip dynamicOnly lights (vtmat.wgsl; lightmapped surfaces always do).
        // Materials are shared per name: INTERIM, a mover sharing a static prop's material is
        // treated as static too.
        if is_static && !lit && self.static_materials.insert(mat.id()) {
            if let Some(mut m) = a.mats.get_mut(&mat) {
                vtmat::set_static_model(&mut m.extension, true);
            }
        }
        commands.spawn((Mesh3d(a.meshes.add(mesh)), MeshMaterial3d(mat))).id()
    }
}

/// A non-VT surface material, from its decl (INTERIM looks, not the engine's stage programs).
#[derive(Clone)]
enum Placeholder {
    /// `stageprogram glass*`: translucent.
    Glass,
    /// `emissive { x .. }` with a `transmap` image (ca_fakelight and similar): additive unlit glow.
    Glow { image: Option<Image>, strength: f32 },
}

impl Placeholder {
    /// Reads `generated/decls/material/<name>.decl` (`key value` lines).
    fn from_decl(c: &Container, name: &str) -> Option<Placeholder> {
        let res = if name.ends_with(".decl") { name.to_string() } else { format!("generated/decls/material/{name}.decl") };
        let text = String::from_utf8_lossy(&c.read_by_name(&res).ok()?).into_owned();
        let value = |key: &str| {
            text.lines().map(str::trim).find_map(|l| {
                let (k, v) = l.split_once(char::is_whitespace)?;
                k.eq_ignore_ascii_case(key).then(|| v.trim().to_string())
            })
        };
        if value("stageprogram").is_some_and(|p| p.starts_with("glass")) {
            return Some(Placeholder::Glass);
        }
        let strength = value("emissive").and_then(|v| v.trim_matches(|ch| ch == '{' || ch == '}' || ch == ' ').split(',').next()?.trim().parse::<f32>().ok())?;
        let tm = value("transmap");
        // The image's full name drops the source extension (`textures/x.tga` -> generated/image/textures/x.bimage).
        let image = tm.as_deref().and_then(|m| {
            let stem = m.rsplit_once('.').map_or(m, |(s, _)| s);
            load_bimage(c, &format!("generated/image/{stem}.bimage")).or_else(|| load_bimage(c, &format!("generated/image/{m}.bimage")))
        });
        if std::env::var_os("RANCHER_MAP_TRACE").is_some() {
            eprintln!("map: glow {name}: emissive {strength}, transmap {tm:?} {}", if image.is_some() { "loaded" } else { "MISSING" });
        }
        Some(Placeholder::Glow { image, strength })
    }

    fn material(&self, images: &mut Assets<Image>) -> StandardMaterial {
        match self {
            Placeholder::Glass => StandardMaterial { base_color: Color::srgba(0.8, 0.85, 0.9, 0.15), perceptual_roughness: 0.08, reflectance: 0.5, alpha_mode: AlphaMode::Blend, ..default() },
            Placeholder::Glow { image, strength } => StandardMaterial {
                base_color: Color::linear_rgb(*strength, *strength, *strength),
                base_color_texture: image.clone().map(|i| images.add(i)),
                unlit: true,
                alpha_mode: AlphaMode::Add,
                ..default()
            },
        }
    }
}

/// A `.bimage` (BC1/BC3/BC7/BC4/BC5 formats) as a Bevy image with its mips (like fx.rs's loader).
fn load_bimage(c: &Container, path: &str) -> Option<Image> {
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSamplerDescriptor};
    let bytes = c.read_by_name(path).ok()?;
    let img = idres::bimage::BImage::parse(&bytes).ok()?;
    // ASSETS.md: 10 BC1, 11 BC3, 23 BC7, 24 BC4, 25 BC5 (sRGB colour for the first three).
    let (format, bpb) = match img.format {
        10 => (TextureFormat::Bc1RgbaUnormSrgb, 8),
        11 => (TextureFormat::Bc3RgbaUnormSrgb, 16),
        23 => (TextureFormat::Bc7RgbaUnormSrgb, 16),
        24 => (TextureFormat::Bc4RUnorm, 8),
        25 => (TextureFormat::Bc5RgUnorm, 16),
        _ => return None,
    };
    let mut mips: Vec<&idres::bimage::Mip> = img.mips.iter().filter(|m| m.dest_z == 0).collect();
    mips.sort_by_key(|m| m.level);
    let (mut data, mut levels) = (Vec::new(), 0);
    for m in mips {
        let (w, h) = ((img.width >> m.level).max(1), (img.height >> m.level).max(1));
        let need = (w.div_ceil(4) * h.div_ceil(4) * bpb) as usize;
        let src = bytes.get(m.data.clone())?;
        if src.len() < need {
            break;
        }
        data.extend_from_slice(&src[..need]);
        levels += 1;
    }
    if levels == 0 {
        return None;
    }
    let mut image = Image::new(Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 }, TextureDimension::D2, data, format, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    Some(image)
}

/// The baked lightmap and the map's run-time lights are on unless `RANCHER_MAP_LIGHTMAP=0` (the app
/// uses this too, to keep its range sun off lightmapped worlds).
pub fn lightmap_enabled() -> bool {
    std::env::var("RANCHER_MAP_LIGHTMAP").map_or(true, |v| v != "0")
}

/// `view.exposure` of the app's cameras (post::hdr_camera: ENGINE_EV100, i.e. 1).
fn camera_exposure() -> f32 {
    bevy::camera::Exposure { ev100: crate::post::ENGINE_EV100 }.exposure()
}

/// One shadowless Bevy point light per run-time engine light, covering its volume, so Bevy's
/// clusters list it for the pixels it can reach; `shadow_depth_bias` = -( engine light index + 1 )
/// tells vtmat.wgsl to evaluate it with the engine's PROCESS_LIGHT (indices follow
/// lighting::build_scene's light order). Their Bevy intensity only lights StandardMaterial
/// stand-ins (INTERIM: inverse-square diffuse = albedo × colour × 0.25 at half the radius, × `extra`).
fn spawn_lights(commands: &mut Commands, lights: &[MapLight], indices: &[Option<u32>], extra: f32) {
    use vtmat::lighting::LightKind;
    let exposure = camera_exposure();
    // The engine lights view models with the same lights as the world: world layer + hands layer.
    let layers = bevy::camera::visibility::RenderLayers::from_layers(&[0, crate::combat::VIEW_LAYER]);
    let mut n = 0;
    for (l, index) in lights.iter().zip(indices) {
        let Some(index) = *index else { continue };
        let corners = l.corners();
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        let (at, range) = if l.kind == LightKind::Parallel {
            (l.global_origin(), 1.0e6)
        } else {
            (center, corners.iter().map(|p| p.distance(center)).fold(0.0, f32::max))
        };
        let peak = l.color.max_element();
        let color = if peak > 0.0 { Color::linear_rgb(l.color.x / peak, l.color.y / peak, l.color.z / peak) } else { Color::BLACK };
        let half = l.radius.max_element() * 0.5;
        let lumens = peak.max(0.0) * 0.25 * 4.0 * std::f32::consts::PI * std::f32::consts::PI * half * half * extra / exposure;
        commands.spawn((
            PointLight { color, intensity: lumens, range, radius: 0.0, shadow_maps_enabled: false, shadow_depth_bias: -((index + 1) as f32), ..default() },
            Transform::from_translation(to_bevy(V::from(at.to_array()))),
            Name::new(l.name.clone()),
            layers.clone(),
        ));
        n += 1;
    }
    eprintln!("map: {n} run-time lights (engine shading; Bevy point lights for clustering)");
}

/// INTERIM look for materials outside the virtual texture: lights and glow sfx glow, the rest is grey.
fn placeholder(name: &str) -> StandardMaterial {
    let glow = name.contains("/sfx/") || name.contains("light") || name.contains("glow");
    if glow {
        StandardMaterial { base_color: Color::srgb(0.9, 0.9, 0.85), emissive: LinearRgba::rgb(2.0, 2.0, 1.8), ..default() }
    } else {
        StandardMaterial { base_color: Color::srgb(0.35, 0.35, 0.37), perceptual_roughness: 0.9, ..default() }
    }
}

/// Mesh builder for one merged mesh: copies the referenced vertices of each surface added to it,
/// converts to Bevy space and reverses the winding (idTech triangles are clockwise seen from the front).
struct Builder {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    tan: Vec<[f32; 4]>,
    uv: Vec<[f32; 2]>,
    /// Lightmap texcoords (`st`) for surfaces in the unique virtual texture.
    uv1: Option<Vec<[f32; 2]>>,
    /// Vertex colour of lightmapped surfaces, raw u8x4 / 255 (vertex.inc DECODE_COLOR: the packed
    /// unpackR8G8B8A8(..).wzyx gives bytes 0..3 as r, g, b, a); vtmat.wgsl applies `rgb * a * 16`.
    color: Option<Vec<[f32; 4]>>,
    idx: Vec<u32>,
    /// Surface vertex -> mesh vertex, for the surface `remap_for`.
    remap: Vec<u32>,
    remap_for: usize,
}

impl Builder {
    fn new(lightmapped: bool) -> Self {
        Builder { pos: Vec::new(), nrm: Vec::new(), tan: Vec::new(), uv: Vec::new(), uv1: lightmapped.then(Vec::new), color: lightmapped.then(Vec::new), idx: Vec::new(), remap: Vec::new(), remap_for: usize::MAX }
    }

    /// Starts taking triangles of surface number `serial` (with `nv` vertices).
    fn surface(&mut self, serial: usize, nv: usize) -> &mut Self {
        if self.remap_for != serial {
            self.remap_for = serial;
            self.remap.clear();
            self.remap.resize(nv, u32::MAX);
        }
        self
    }

    fn vert(&mut self, s: &bmodel::Surface, i: usize, xf: &Xform, use_vmtr: bool) -> u32 {
        if self.remap[i] == u32::MAX {
            let v = &s.verts[i];
            self.remap[i] = self.pos.len() as u32;
            self.pos.push(to_bevy(xf.point(V::from(v.xyz))).to_array());
            self.nrm.push(to_bevy(xf.dir(V::from(v.normal_f32())).normalize_or_zero()).to_array());
            let t = v.tangent_f32();
            let tb = to_bevy(xf.vec(V::new(t[0], t[1], t[2])).normalize_or_zero());
            // Same bitangent-sign convention as the view models (viewanim.rs TANGENT_SIGN).
            let w = if v.tangent[3] >= 128 { -1.0 } else { 1.0 } * xf.handedness;
            self.tan.push([tb.x, tb.y, tb.z, w]);
            self.uv.push(if use_vmtr { v.vmtr_tc } else { v.st });
            if let Some(uv1) = &mut self.uv1 {
                uv1.push(v.st);
            }
            if let Some(color) = &mut self.color {
                color.push(v.color.map(|c| c as f32 / 255.0));
            }
        }
        self.remap[i]
    }

    fn tri(&mut self, s: &bmodel::Surface, t: [usize; 3], xf: &Xform, use_vmtr: bool) {
        let a = self.vert(s, t[0], xf, use_vmtr);
        let b = self.vert(s, t[1], xf, use_vmtr);
        let c = self.vert(s, t[2], xf, use_vmtr);
        if xf.handedness > 0.0 {
            self.idx.extend_from_slice(&[a, c, b]);
        } else {
            self.idx.extend_from_slice(&[a, b, c]);
        }
    }

    fn finish(self) -> Option<(Mesh, bool)> {
        if self.idx.is_empty() {
            return None;
        }
        let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.nrm)
            .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, self.tan)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uv)
            .with_inserted_indices(Indices::U32(self.idx));
        let lit = self.uv1.is_some();
        if let Some(uv1) = self.uv1 {
            m.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1);
        }
        if let Some(color) = self.color {
            m.insert_attribute(Mesh::ATTRIBUTE_COLOR, color);
        }
        Some((m, lit))
    }
}

/// Entity placement: p' = origin + axisᵀ·(p·scale) (idTech row-vector axis).
struct Xform {
    origin: V,
    axis: [V; 3],
    scale: V,
    handedness: f32,
}

impl Xform {
    fn identity() -> Self {
        Xform { origin: V::ZERO, axis: [V::X, V::Y, V::Z], scale: V::ONE, handedness: 1.0 }
    }
    fn new(origin: [f32; 3], axis: [[f32; 3]; 3], scale: [f32; 3]) -> Self {
        let axis = [V::from(axis[0]), V::from(axis[1]), V::from(axis[2])];
        let det = axis[0].dot(axis[1].cross(axis[2])) * scale[0] * scale[1] * scale[2];
        Xform { origin: V::from(origin), axis, scale: V::from(scale), handedness: if det < 0.0 { -1.0 } else { 1.0 } }
    }
    fn point(&self, p: V) -> V {
        let p = p * self.scale;
        self.origin + self.axis[0] * p.x + self.axis[1] * p.y + self.axis[2] * p.z
    }
    /// Normals: inverse-transpose of the placement (axis rows are orthonormal).
    fn dir(&self, d: V) -> V {
        let d = d / self.scale;
        self.axis[0] * d.x + self.axis[1] * d.y + self.axis[2] * d.z
    }
    /// Tangents: like positions without the translation.
    fn vec(&self, d: V) -> V {
        let d = d * self.scale;
        self.axis[0] * d.x + self.axis[1] * d.y + self.axis[2] * d.z
    }
}

/// Merged meshes under construction, keyed by (cell, material, lightmapped).
#[derive(Default)]
struct Batches {
    builders: std::collections::HashMap<([i32; 3], MatKey, bool), Builder>,
    surfaces: usize,
    tris: usize,
}

impl Batches {
    /// Adds a model's surfaces. World surfaces are split by their per-vertex VT material
    /// (triangles never mix materials in the shipped files); others use the surface material.
    fn add_model(&mut self, bytes: &[u8], xf: &Xform, unique: &mut UniqueMaterials) -> Result<()> {
        let mut rd = bmodel::SurfaceReader::new(bytes)?;
        while let Some(s) = rd.next_surface()? {
            self.add_surface(&s, xf, unique);
        }
        Ok(())
    }

    /// An md6 model's meshes in their stored (bind) pose, as static surfaces.
    fn add_md6(&mut self, bytes: &[u8], xf: &Xform, unique: &mut UniqueMaterials) -> Result<()> {
        let m = idres::md6::Md6Model::parse(bytes)?;
        for mesh in m.meshes {
            let verts = mesh
                .verts
                .iter()
                .map(|v| bmodel::StaticVert { xyz: v.xyz, st: v.st, normal: v.normal, tangent: v.tangent, color: [255; 4], vmtr_tc: [0.0; 2], vmtr_sb: [0xffff, 0, 0, 0] })
                .collect();
            let s = bmodel::Surface {
                material: mesh.material,
                material_check: 0,
                unknown_b: 0,
                unknown_c: 0,
                vmtrs: Vec::new(),
                format: bmodel::FORMAT_DRAWVERT,
                dequant: [1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0],
                verts,
                indices: mesh.indices,
                bounds: mesh.bounds,
                unknown_tail: 0,
            };
            self.add_surface(&s, xf, unique);
        }
        Ok(())
    }

    fn add_surface(&mut self, s: &bmodel::Surface, xf: &Xform, unique: &mut UniqueMaterials) {
        {
            let serial = self.surfaces;
            self.surfaces += 1;
            let lit = unique.is_unique(&s.material);
            let masked = unique.alpha_tested(&s.material);
            for t in s.indices.chunks_exact(3) {
                let t = [t[0] as usize, t[1] as usize, t[2] as usize];
                let (key, use_vmtr) = match s.verts[t[0]].vmtr().filter(|&k| k < s.vmtrs.len()) {
                    Some(k) if masked => (MatKey::VtMasked(s.vmtrs[k].clone()), true),
                    Some(k) => (MatKey::Vt(s.vmtrs[k].clone()), true),
                    None if s.vmtrs.is_empty() && masked => (MatKey::VtMasked(s.material.clone()), false),
                    None if s.vmtrs.is_empty() => (MatKey::Vt(s.material.clone()), false),
                    None => (MatKey::Flat(s.material.clone()), false),
                };
                let p = xf.point(V::from(s.verts[t[0]].xyz)) / MERGE_CELL;
                let cell = [p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32];
                self.builders.entry((cell, key, lit)).or_insert_with(|| Builder::new(lit)).surface(serial, s.verts.len()).tri(s, t, xf, use_vmtr);
                self.tris += 1;
            }
        }
    }
}

/// Which surface materials live in the map's unique virtual texture (decl has `landPageFile`).
struct UniqueMaterials<'a> {
    c: &'a Container,
    known: std::collections::HashMap<String, Option<String>>,
    /// zPrePassProgram of each material decl looked at.
    prepass: std::collections::HashMap<String, Option<String>>,
    /// Lightmap off: no surface gets lightmap texcoords.
    disabled: bool,
}

impl UniqueMaterials<'_> {
    /// The material's `landPageFile`, if any. Map materials are named by their decl path
    /// (`maps/<map>/mega.decl`); others are `generated/decls/material/<name>.decl`.
    fn land_page_file(&mut self, material: &str) -> Option<String> {
        let c = self.c;
        self.known
            .entry(material.to_string())
            .or_insert_with(|| {
                let res = if material.ends_with(".decl") { material.to_string() } else { format!("generated/decls/material/{material}.decl") };
                // Material decls are `key value` lines (no `=`): take the token after `landPageFile`.
                let text = c.read_by_name(&res).ok()?;
                let text = String::from_utf8_lossy(&text);
                let mut it = text.split_whitespace();
                it.find(|t| t.eq_ignore_ascii_case("landPageFile"))?;
                it.next().map(|v| v.trim_matches('"').to_string())
            })
            .clone()
    }
    fn is_unique(&mut self, material: &str) -> bool {
        !self.disabled && self.land_page_file(material).is_some()
    }

    /// Materials drawn with an alpha-tested depth prepass (`zPrePassProgram preZDrawAlpha*`).
    fn alpha_tested(&mut self, material: &str) -> bool {
        let c = self.c;
        self.prepass
            .entry(material.to_string())
            .or_insert_with(|| {
                let res = if material.ends_with(".decl") { material.to_string() } else { format!("generated/decls/material/{material}.decl") };
                let text = c.read_by_name(&res).ok()?;
                let text = String::from_utf8_lossy(&text);
                let mut it = text.split_whitespace();
                it.find(|t| t.eq_ignore_ascii_case("zPrePassProgram"))?;
                it.next().map(|v| v.trim_matches('"').to_string())
            })
            .as_deref()
            .is_some_and(|p| p.to_ascii_lowercase().starts_with("prezdrawalpha"))
    }
}

fn build_meshes(doom: &std::path::Path, map: &str, props: &[Placed<String>], dynamic: &[Placed<String>], tx: &Sender<Msg>) -> Result<()> {
    let t = std::time::Instant::now();
    let c = Container::open(&doom.join("base"), "gameresources")?;
    let mut unique = UniqueMaterials { c: &c, known: Default::default(), prepass: Default::default(), disabled: false };
    // The lightmap first, so lit meshes find it when they are spawned.
    let land = unique.land_page_file(&format!("maps/{map}/mega.decl")).unwrap_or_else(|| format!("maps/{map}"));
    let level = std::env::var("RANCHER_MAP_LIGHTMAP_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(LIGHTMAP_LEVEL);
    // Opt-in (see the module docs).
    let enabled = lightmap_enabled();
    if !enabled {
        unique.disabled = true;
    }
    match idres::vtex::unique::UniqueVt::open(doom, &land).and_then(|u| if enabled { Ok(u) } else { anyhow::bail!("off (RANCHER_MAP_LIGHTMAP=1 enables)") }) {
        Ok(u) => {
            let level = level.clamp(1, u.levels.saturating_sub(1).max(1));
            let _ = tx.send(Msg::Lightmap(u.level_texture(level)));
        }
        Err(e) => eprintln!("map: no unique lightmap ({land}): {e:#}"),
    }
    let mut batches = Batches::default();
    let world = read(&c, &format!("maps/{map}/_combo/_world.bmodel"))?;
    batches.add_model(&world, &Xform::identity(), &mut unique).context("world model")?;
    drop(world);
    let add = |batches: &mut Batches, p: &Placed<String>, unique: &mut UniqueMaterials| {
        let xf = Xform::new(p.origin, p.axis, p.scale);
        let r = read(&c, &p.item).and_then(|b| if p.item.ends_with(".bmd6model") { batches.add_md6(&b, &xf, unique) } else { batches.add_model(&b, &xf, unique) });
        if let Err(e) = r {
            eprintln!("map: {} ({}): {e:#}", p.entity, p.item);
        }
    };
    for p in props {
        add(&mut batches, p, &mut unique);
    }
    // Movers and bound entities: one batch set per entity, in model space.
    let mut dyn_batches: Vec<(String, Batches)> = Vec::new();
    for p in dynamic {
        match dyn_batches.iter_mut().find(|(e, _)| *e == p.entity) {
            Some((_, b)) => add(b, p, &mut unique),
            None => {
                let mut b = Batches::default();
                add(&mut b, p, &mut unique);
                dyn_batches.push((p.entity.clone(), b));
            }
        }
    }
    let (mut meshes, tris) = (0, batches.tris);
    let masked = batches.builders.keys().chain(dyn_batches.iter().flat_map(|(_, b)| b.builders.keys())).filter(|(_, k, _)| matches!(k, MatKey::VtMasked(_))).count();
    // Decl-based looks for surface materials outside the virtual texture (whole surfaces only; the
    // per-vertex VT materials are all in it).
    let mut seen = std::collections::HashSet::new();
    for (_, key, _) in batches.builders.keys().chain(dyn_batches.iter().flat_map(|(_, b)| b.builders.keys())) {
        let (MatKey::Vt(n) | MatKey::VtMasked(n) | MatKey::Flat(n)) = key;
        if seen.insert(n.clone()) && !n.ends_with(".decl") {
            if let Some(p) = Placeholder::from_decl(&c, n) {
                let _ = tx.send(Msg::Placeholder(n.clone(), p));
            }
        }
    }
    for (entity, mut b) in dyn_batches {
        let parts: Vec<(Mesh, MatKey, bool)> = b.builders.drain().filter_map(|((_, key, _), bl)| bl.finish().map(|(m, lit)| (m, key, lit))).collect();
        meshes += parts.len();
        if tx.send(Msg::Dynamic { entity, meshes: parts }).is_err() {
            return Ok(());
        }
    }
    for ((_, key, _), b) in batches.builders.drain() {
        let Some((m, lit)) = b.finish() else { continue };
        meshes += 1;
        if tx.send(Msg::Mesh(m, key, lit)).is_err() {
            return Ok(());
        }
    }
    let _ = tx.send(Msg::Done { meshes, masked, tris, secs: t.elapsed().as_secs_f32() });
    Ok(())
}
