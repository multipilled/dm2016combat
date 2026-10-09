//! Weapon and impact FX from the install: idfx (decls, FX manager, the engine's CPU particle path) drawn
//! with a material that reproduces the `transsortblend` stage program.
//!
//! - First-person weapon FX: the weapon decl's `weaponFX`, triggered with FX_WEAPON_START_FIRE (extra
//!   PRIMARY_FIRE) when the arsenal fires and FX_WEAPON_START_SHELL_EJECT on `ae_ejectShell`; tags are the
//!   view model's joints (`ViewJoints`), drawn on the view-model layer with the view camera.
//! - Lights (FX_LIGHT) become point lights on both layers.
//! - Impacts (projectile impact tables), tracers, and GPU particle stages (idfx::sim gpu_stage, materials
//!   gpuparticle/add|blend).
//! - Shell casings: the weapon's shellEmitter idPieceEmitter (idfx::pieces) emits on `ae_ejectShell` at the
//!   `shell_eject` tag, simulated against the sim's collision world and drawn with the VT material of the
//!   piece model (`generated/discreteanimation/*.dmodel`).
//! - Map FX (RANCHER_MAP): the level's always-on idParticleEmitter / idEntityFx entities (see `load_map_fx`;
//!   RANCHER_MAP_FX=0 disables).
//! - Hitscan tracer ribbons (`tracerInfo.ribbonDecls`: pistol, heavy rifle, gauss beams): idfx::ribbon sets
//!   started on the tracer shot and drawn until tracerLifetime with RibbonMaterial (renderprogs ribbonblend,
//!   ca_ribbonelectricalarc, ribbontracersmoke).
//!
//! The shader follows renderprog transsortblend (decls/renderprog/transsortblend.decl): RG texture
//! (R brightness, G opacity, both through SRGBlinearApprox), cross-faded animation frames, genericParm
//! brightness/opacity boosts, the near-eye fade from the soft-particle alpha scale, the emissive /
//! particleMult switch, premultiplied output blended ONE, ONE_MINUS_SRC_ALPHA, no depth write, two-sided.
//! Debug: RANCHER_FX_TRACE=1 prints per-frame quad counts and tag placement; RANCHER_FX_DEBUG=1 draws
//! particles solid magenta; RANCHER_FX_PARTICLE_MULT=<f> overrides the env's $particleMult; RANCHER_FX_NO_SOFT=1 skips
//! the world camera's depth prepass (no soft-particle fade).
//! $particleMult comes from the level's env decl (worldspawn envSettings; the `default` env sets 0.15, many
//! lit areas 1.0) — transsortblend has no other lighting term for non-emissive particles.
//! The soft-particle depth fade (particle.inc COMPUTE_SOFT_PARTICLE_SCALE) reads the world camera's depth prepass
//! and the billboard depth push moves the quad toward the view (both in the shader below).
//! INTERIM: no fog (the renderer has no fog yet; transsortblend lerps to fogColor * $transparencyLightMult by the
//! vertex fog interpolator, fog.inc); lights are converted like map.rs's idLights (`light_lumens`); stage programs other than
//! transsortblend / gpuparticle (particleshighq, transsortblendflow, liquidhighq, particle/add,
//! softparticleadditiveblend, distortion ...) are drawn with the transsortblend path.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, MeshVertexAttribute, MeshVertexBufferLayoutRef, PrimitiveTopology, VertexFormat};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureDimension,
    TextureFormat,
};
use bevy::shader::{Shader, ShaderRef};
use idfx::Axis;
use idfx::fx::{FxDecl, FxManager, TagSource};
use idfx::material::ParticleMaterial;
use idfx::ribbon::{RibbonDecl, RibbonSet, RibbonVertex};
use idfx::sim::{Frame, Quad, View};
use idres::Container;
use idres::decldb::DeclDb;

use crate::combat::{Combat, VIEW_LAYER};
use crate::viewanim::{HandsEvents, ViewJoints};

type G3 = glam::Vec3;

pub struct FxPlugin;

/// The soft-particle fade reads the opaque depth ($viewDepthMap): the world camera (order 0) gets a depth prepass.
fn depth_prepass_on_world_camera(mut commands: Commands, cams: Query<(Entity, &Camera), (With<Camera3d>, Without<bevy::core_pipeline::prepass::DepthPrepass>)>) {
    if std::env::var("RANCHER_FX_NO_SOFT").is_ok() {
        return;
    }
    for (e, c) in &cams {
        if c.order == 0 {
            commands.entity(e).insert(bevy::core_pipeline::prepass::DepthPrepass);
        }
    }
}

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.world_mut().resource_mut::<Assets<Shader>>().insert(SHADER.id(), Shader::from_wgsl(SHADER_SRC, file!())).expect("uuid handle");
        app.world_mut().resource_mut::<Assets<Shader>>().insert(DECAL_SHADER.id(), Shader::from_wgsl(DECAL_SHADER_SRC, file!())).expect("uuid handle");
        app.world_mut().resource_mut::<Assets<Shader>>().insert(RIBBON_SHADER.id(), Shader::from_wgsl(RIBBON_SHADER_SRC, file!())).expect("uuid handle");
        app.add_plugins(MaterialPlugin::<FxMaterial>::default())
            .add_plugins(MaterialPlugin::<DecalMaterial>::default())
            .add_plugins(MaterialPlugin::<RibbonMaterial>::default())
            .add_message::<crate::combat::ShotImpact>()
            .add_message::<crate::combat::ShotTracer>()
            .add_systems(Startup, load)
            .add_systems(Update, depth_prepass_on_world_camera)
            .add_systems(PostUpdate, run);
    }
}

const SHADER: Handle<Shader> = uuid_handle!("3e7c1a90-5b2d-4c8e-9f41-6a0d2b7c8e15");

const ATTRIBUTE_UV_NEXT: MeshVertexAttribute = MeshVertexAttribute::new("FxUvNext", 0x7a1c_33e0_9b52_4d01, VertexFormat::Float32x2);
const ATTRIBUTE_GENERIC: MeshVertexAttribute = MeshVertexAttribute::new("FxGeneric", 0x7a1c_33e0_9b52_4d02, VertexFormat::Float32x4);
/// (frame cross-fade fraction, soft particle alpha scale)
const ATTRIBUTE_MISC: MeshVertexAttribute = MeshVertexAttribute::new("FxMisc", 0x7a1c_33e0_9b52_4d03, VertexFormat::Float32x2);

/// $emissiveMult renderparm default (emissivemult.decl), used when a material sets none.
const EMISSIVE_MULT_DEFAULT: f32 = 5.0;
/// FX and particle lights in the engine's radiance units, converted like map.rs's idLights (INTERIM there too:
/// a shadowless Bevy point light whose inverse-square diffuse equals albedo x colour x 0.25 at half the radius;
/// the engine's lights are projected falloff volumes): lumens = peak x 0.25 x 4 pi^2 x (radius / 2)^2 / the
/// camera's view.exposure (post::ENGINE_EV100, i.e. 1).
fn light_lumens(peak: f32, radius: f32) -> f32 {
    let half = radius * 0.5;
    let exposure = bevy::camera::Exposure { ev100: crate::post::ENGINE_EV100 }.exposure();
    peak * 0.25 * 4.0 * std::f32::consts::PI * std::f32::consts::PI * half * half / exposure
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct FxMaterial {
    #[texture(0)]
    #[sampler(1)]
    texture: Handle<Image>,
    /// x emissive, y emissiveMult (basicadd: factor), z particleMult, w mode (0 transsortblend, 1 basicadd,
    /// 2 debug solid)
    #[uniform(2)]
    parms: Vec4,
    /// basicadd $color2 (material `color2`), 1 otherwise.
    #[uniform(3)]
    tint: Vec4,
}

impl Material for FxMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, _: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vertex = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(1),
            ATTRIBUTE_UV_NEXT.at_shader_location(2),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(3),
            ATTRIBUTE_GENERIC.at_shader_location(4),
            ATTRIBUTE_MISC.at_shader_location(5),
        ])?;
        descriptor.vertex.buffers = vec![vertex];
        descriptor.primitive.cull_mode = None;
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.first_mut() {
                let c = BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::OneMinusSrcAlpha, operation: BlendOperation::Add };
                target.blend = Some(BlendState { color: c, alpha: c });
            }
        }
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

struct MatEntry {
    handle: Handle<FxMaterial>,
}

/// Which camera layer a set of particles is drawn on.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Layer {
    World,
    View,
}

#[derive(Resource)]
pub struct Fx {
    db: DeclDb,
    /// First-person weapon FX per arsenal slot.
    weapons: Vec<Option<FxManager>>,
    /// Each weapon model's `_info` prop tags (md6Def props).
    weapon_tags: Vec<Vec<idfx::tags::Tag>>,
    materials: HashMap<String, Option<MatEntry>>,
    meshes: HashMap<(String, Layer), (Entity, Handle<Mesh>)>,
    lights: Vec<Entity>,
    last_fire: Option<i32>,
    prev_ms: i32,
    /// Weapon on screen last frame (FX of a holstered weapon keep their last placement).
    shown: Option<usize>,
    impacts: idfx::impact::ImpactCache,
    /// Impact particle systems in the world.
    world: Vec<WorldSystem>,
    /// The game's idRandom used by ImpactEffect (game+0x285be8) for decal and particle picks.
    game_rng: idfx::IdRandom,
    /// INTERIM: seeds for world particle systems (the spawn path's own draw is not decoded).
    world_rng: idfx::IdRandom,
    /// impactEffectTableLimitedBySurfType counters per surface type: (frame, count).
    surf_count: Vec<(i32, i32)>,
    tracer_info: HashMap<String, Option<TracerInfo>>,
    tracers: Vec<TracerRun>,
    /// Per weapon: rounds fired (tracer cadence) and the shot already decided.
    tracer_rounds: HashMap<usize, i32>,
    tracer_shot: Option<(usize, u32, bool)>,
    /// Per arsenal slot: the weapon's shellEmitter pieces (breakable shell casings).
    shells: Vec<Option<ShellEmitter>>,
    /// VT materials for piece models (own streamer; atlases arrive through vtmat::stream_in).
    vtm: Option<crate::vtmat::VtMaterials>,
    /// View yaw (degrees) last frame, for the eject handler's turn-rate term.
    prev_yaw: Option<f32>,
    /// Per projectile decl: its FX decl and model (None when it has neither).
    proj_defs: HashMap<String, Option<ProjDef>>,
    /// Projectiles in flight (and exploded ones whose FX are still playing).
    projs: Vec<ProjFx>,
    /// The level's env renderparms ($particleMult ...): RANCHER_MAP's worldspawn envSettings, else `default`.
    env: idfx::env::EnvParms,
    /// Decal material textures (decalatlas diffuse, _local normal + mask, _s specular) by material name.
    decal_textures: HashMap<String, Option<[Option<Handle<Image>>; 3]>>,
    decals: Vec<LiveDecal>,
    /// Tracer ribbon sets per projectile decl (10 per fire mode on the weapon, +0x2d0 + mode * 0x140).
    ribbon_sets: HashMap<String, Vec<RibbonSet>>,
    /// The ribbon code's own random (global 0x145016e18, zero at start).
    ribbon_rng: idfx::IdRandom,
    ribbon_decls: HashMap<String, Option<Arc<RibbonDecl>>>,
    ribbon_mats: HashMap<String, Option<Handle<RibbonMaterial>>>,
    ribbon_meshes: HashMap<String, (Entity, Handle<Mesh>)>,
    /// 1x1 stand-ins for textures a ribbon material leaves at the renderparm default.
    black: Option<Handle<Image>>,
    /// The level's particle emitters and entity FX.
    map_fx: Vec<MapEmitter>,
}

/// A map-placed FX entity: idParticleEmitter (its particle system) or idEntityFx (its FX decl).
struct MapEmitter {
    name: String,
    origin: G3,
    axis: Axis,
    /// The render entity's Color parm (renderModelInfo.color).
    color: glam::Vec4,
    sys: Option<idfx::sim::System>,
    mgr: Option<FxManager>,
    started: bool,
    /// dormancy.distance when canBecomeDormant (0 = never).
    dormancy: f32,
}

/// The map's always-on (not startOff) idParticleEmitter and idEntityFx entities.
/// idParticleEmitter (ctor 0x140950020: randomizeStartTime 1, fadeIn / fadeOut 0.5, alphaScale 1, translucencyScale
/// 1, distributionScale 1) -> SetParticle 0x140962d20: render parms Color = entity colour, Diversity = game random
/// (game+0x285be8) * 1/32767, TimeStop 0, TimeOffset 0x140957800 = now * 0.001 + (randomizeStartTime and every stage
/// looping forever: game random * 1/32767 * max stage cycleMsec * 0.001), particleScale = distributionScale,
/// alphaScale = 1 / alphaScale. idEntityFx: its fxEffect raised with FX_NONE (the map FX decls' start condition),
/// actions sized by fxSizeScalar.
/// INTERIM: entities are spawned in file order with nothing else drawing from the game random in between; bound
/// emitters stay at their spawn pose (map.rs's movers do not carry them); emitters beyond their dormancy distance
/// or well behind the view are not simulated (CPU budget, not the engine's culling); the FX manager seed, startDelay,
/// fades on trigger show / hide and particleTranslucencyScale are not reproduced.
fn load_map_fx(fx: &mut Fx, map: &str) {
    let path = format!("maps/{map}.entities");
    let Ok(bytes) = fx.db.container().read_by_name(&path) else { return };
    let ents = match idres::entities::parse(&String::from_utf8_lossy(&bytes)) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("fx: {path}: {e:#}");
            return;
        }
    };
    let mut particles: HashMap<String, Option<Arc<idfx::particle::ParticleDecl>>> = HashMap::new();
    let mut fx_decls: HashMap<String, Option<Arc<FxDecl>>> = HashMap::new();
    for e in &ents.entities {
        let class = e.class().unwrap_or("");
        if class != "idParticleEmitter" && class != "idEntityFx" {
            continue;
        }
        let edit = e.edit_merged(Some(&fx.db));
        let flag = |k: &str, d: bool| edit.path(k).and_then(|v| v.as_bool()).unwrap_or(d);
        if flag("startOff", false) {
            continue;
        }
        let a = e.axis();
        let axis = Axis { x: G3::from_array(a[0]), y: G3::from_array(a[1]), z: G3::from_array(a[2]) };
        let c = edit.block("renderModelInfo.color");
        let cf = |k: &str| c.and_then(|c| c.f32(k)).unwrap_or(1.0);
        let dormancy = if flag("flags.canBecomeDormant", false) { edit.f32("dormancy.distance").unwrap_or(0.0) } else { 0.0 };
        let mut em = MapEmitter {
            name: e.name.clone(),
            origin: G3::from_array(e.origin()),
            axis,
            color: glam::Vec4::new(cf("r"), cf("g"), cf("b"), cf("a")),
            sys: None,
            mgr: None,
            started: false,
            dormancy,
        };
        if class == "idParticleEmitter" {
            let Some(name) = edit.str("particleSystem").filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("NULL")) else { continue };
            let decl = particles
                .entry(name.to_string())
                .or_insert_with(|| match idfx::particle::ParticleDecl::load(&fx.db, name) {
                    Ok(d) => Some(d),
                    Err(err) => {
                        eprintln!("fx: {}: {err:#}", e.name);
                        None
                    }
                })
                .clone();
            let Some(decl) = decl else { continue };
            let seed = fx.game_rng.next_int();
            let mut start = 0.0f32;
            if flag("randomizeStartTime", true) && decl.stages.iter().all(|s| s.cycles == 0) {
                let max_cycle = decl.stages.iter().map(|s| s.cycle_msec).fold(0, i32::max);
                start += fx.game_rng.next_int() as f32 * idfx::RANDOM_SCALE * max_cycle as f32 * 0.001;
            }
            let mut sys = idfx::sim::System::new(decl, 0, seed);
            sys.start = start;
            em.sys = Some(sys);
        } else {
            let Some(name) = edit.str("fxEffect").filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("NULL")) else { continue };
            let decl = fx_decls
                .entry(name.to_string())
                .or_insert_with(|| match FxDecl::load(&fx.db, name) {
                    Ok(d) => Some(d),
                    Err(err) => {
                        eprintln!("fx: {}: {err:#}", e.name);
                        None
                    }
                })
                .clone();
            let Some(decl) = decl else { continue };
            let mut m = FxManager::new(decl, fx.game_rng.next_int());
            m.size_scalar = edit.f32("fxSizeScalar").unwrap_or(1.0);
            em.mgr = Some(m);
        }
        fx.map_fx.push(em);
    }
    eprintln!("fx: map {map}: {} always-on particle emitters / entity FX", fx.map_fx.len());
}

/// SURFTYPE_* of the solid collision polygon under an impact: the map collision models' polygon whose plane
/// passes within 0.5 units of `pos`, facing like `normal`, with `pos` inside its loop. The range's brushes carry
/// no surface type (None).
fn surftype_at(world: &rancher_sim::collision::World, pos: G3, normal: G3) -> Option<u32> {
    let n = idfx::normalize(normal);
    for inst in &world.cms {
        if inst.max.cmplt(pos - G3::ONE).any() || inst.min.cmpgt(pos + G3::ONE).any() {
            continue;
        }
        let inv = inst.axis.transpose();
        let p = inv * (pos - inst.origin);
        let mn = inv * n;
        for sm in &inst.cm.submodels {
            let (lo, hi) = (G3::from_array(sm.bounds[0]) - G3::ONE, G3::from_array(sm.bounds[1]) + G3::ONE);
            if p.cmplt(lo).any() || p.cmpgt(hi).any() {
                continue;
            }
            for poly in &sm.polygons {
                let [blo, bhi] = poly.bounds;
                let (blo, bhi) = (G3::new(blo[0] as f32, blo[1] as f32, blo[2] as f32) - G3::ONE, G3::new(bhi[0] as f32, bhi[1] as f32, bhi[2] as f32) + G3::ONE);
                if p.cmplt(blo).any() || p.cmpgt(bhi).any() {
                    continue;
                }
                let Some(si) = sm.surfaces.get(poly.surface as usize) else { continue };
                if si.contents & idres::bcm::contents::SOLID == 0 {
                    continue;
                }
                let pl = sm.polygon_plane(poly);
                let pn = G3::new(pl[0], pl[1], pl[2]);
                if (pn.dot(p) + pl[3]).abs() > 0.5 || pn.dot(mn) < 0.9 {
                    continue;
                }
                // Inside the loop (counter-clockwise seen along +n).
                let vs: Vec<G3> = sm.polygon_verts(poly).map(|v| G3::from_array(sm.verts[v])).collect();
                let inside = (0..vs.len()).all(|k| {
                    let (a, b) = (vs[k], vs[(k + 1) % vs.len()]);
                    (b - a).cross(p - a).dot(pn) >= -1e-3
                });
                if inside {
                    return Some(si.surface_type);
                }
            }
        }
    }
    None
}

/// One projected impact decal (render decal 0x1415f3e10 parameters).
struct LiveDecal {
    entity: Entity,
    mat: Handle<DecalMaterial>,
    t0: i32,
    lifetime: i32,
    fade_in: i32,
    fade_out: i32,
    emissive_life: i32,
    persistent: bool,
    emissive: Vec4,
}

/// A projectile decl's visuals: `fxDecl` and `notHitscanInfo.entityDef`'s renderModelInfo.
struct ProjDef {
    fx: Option<Arc<FxDecl>>,
    md6: Option<String>,
    scale: G3,
    /// The model's md6Def `_info` prop tags (e.g. `exhaust`).
    tags: Vec<idfx::tags::Tag>,
    meshes: Option<Vec<(Handle<Mesh>, Handle<crate::vtmat::VtMaterial>)>>,
}

struct ProjFx {
    key: (usize, u32),
    def: String,
    mgr: Option<FxManager>,
    pos: G3,
    axis: Axis,
    entities: Vec<Entity>,
    alive: bool,
}

/// Projectile model placement: renderModelInfo.scale and the `_info` tags (parent joints taken at the model
/// origin, INTERIM: the projectile's skeleton is not posed).
struct ProjTags<'a> {
    pos: G3,
    axis: Axis,
    scale: G3,
    tags: &'a [idfx::tags::Tag],
}

impl TagSource for ProjTags<'_> {
    fn tag(&self, name: &str) -> Option<(G3, Axis)> {
        let t = self.tags.iter().find(|t| t.name.eq_ignore_ascii_case(name))?;
        // Same props rotation reading as ViewTags (idQuat::ToMat3 rows).
        let q = glam::Quat::from_xyzw(t.rot[0], t.rot[1], t.rot[2], t.rot[3]).conjugate();
        let local = G3::from_array(t.trans) * self.scale;
        let a = &self.axis;
        Some((self.pos + a.to_parent(local), Axis { x: a.to_parent(q * G3::X), y: a.to_parent(q * G3::Y), z: a.to_parent(q * G3::Z) }))
    }
    fn parent(&self) -> (G3, Axis) {
        (self.pos, self.axis)
    }
}

/// alignToVelocity: idVec3::ToMat3 of the flight direction (x = dir, y = (-dir.y, dir.x, 0) / |dir.xy|, z = x cross y).
fn velocity_axis(v: G3) -> Axis {
    let x = idfx::normalize(v);
    let d = x.x * x.x + x.y * x.y;
    let y = if d == 0.0 { G3::X } else { G3::new(-x.y, x.x, 0.0) / d.sqrt() };
    Axis { x, y, z: x.cross(y) }
}

fn proj_def(fx: &mut Fx, name: &str) -> bool {
    if let Some(d) = fx.proj_defs.get(name) {
        return d.is_some();
    }
    let p = fx.db.get("projectile", name).ok();
    let edit = p.as_deref().and_then(|b| b.block("edit"));
    let fx_decl = edit.and_then(|e| e.str("fxDecl")).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("NULL")).and_then(|n| match FxDecl::load(&fx.db, n) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("fx: projectile {name}: {e:#}");
            None
        }
    });
    let ent = edit.and_then(|e| e.str("notHitscanInfo.entityDef")).and_then(|n| fx.db.get("entitydef", n).ok());
    let rmi = ent.as_deref().and_then(|b| b.block("edit.renderModelInfo"));
    let md6 = rmi.and_then(|r| r.str("model")).filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("NULL")).map(str::to_string);
    let sc = |k: &str| rmi.and_then(|r| r.f32(&format!("scale.{k}"))).unwrap_or(1.0);
    let scale = G3::new(sc("x"), sc("y"), sc("z"));
    let tags = md6.as_deref().and_then(|m| idfx::tags::load(fx.db.container(), m).ok()).and_then(|mut p| p.remove("_info")).unwrap_or_default();
    let def = (fx_decl.is_some() || md6.is_some()).then_some(ProjDef { fx: fx_decl, md6, scale, tags, meshes: None });
    let ok = def.is_some();
    fx.proj_defs.insert(name.to_string(), def);
    ok
}

/// The projectile model's meshes in model space (bind pose, Bevy axes) with their VT materials.
fn proj_meshes(fx: &mut Fx, name: &str, meshes: &mut Assets<Mesh>, images: &mut Assets<Image>, vt_mats: &mut Assets<crate::vtmat::VtMaterial>) -> Vec<(Handle<Mesh>, Handle<crate::vtmat::VtMaterial>)> {
    let Some(Some(d)) = fx.proj_defs.get(name) else { return Vec::new() };
    if let Some(m) = &d.meshes {
        return m.clone();
    }
    let md6 = d.md6.clone();
    let mut out = Vec::new();
    if let Some(md6) = md6 {
        let c = fx.db.container_arc();
        let loaded = crate::assets::read_md6def(&c, &md6).and_then(|def| crate::assets::load_model(&c, &def.mesh).map(|m| (def.offset, m)));
        match loaded {
            Ok((offset, lm)) => {
                if fx.vtm.is_none() {
                    if let Some(doom) = idres::find_install() {
                        fx.vtm = Some(crate::vtmat::VtMaterials::open(&doom));
                    }
                }
                for m in lm.model.meshes.iter().filter(|m| !m.verts.is_empty() && !m.indices.is_empty()) {
                    let pos: Vec<[f32; 3]> = m.verts.iter().map(|v| to_bevy(G3::from_array(v.xyz) + G3::from_array(offset)).to_array()).collect();
                    let nrm: Vec<[f32; 3]> = m.verts.iter().map(|v| to_bevy(G3::from_array(idres::md6::unpack_normal(v.normal))).to_array()).collect();
                    let tan: Vec<[f32; 4]> = m
                        .verts
                        .iter()
                        .map(|v| {
                            let t = to_bevy(G3::from_array(idres::md6::unpack_normal(v.tangent)));
                            [t.x, t.y, t.z, if v.tangent[3] >= 128 { -1.0 } else { 1.0 }]
                        })
                        .collect();
                    let uv: Vec<[f32; 2]> = m.verts.iter().map(|v| v.st).collect();
                    let mut idx: Vec<u32> = Vec::with_capacity(m.indices.len());
                    for t in m.indices.chunks_exact(3) {
                        idx.extend_from_slice(&[t[0] as u32, t[2] as u32, t[1] as u32]);
                    }
                    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, tan)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
                        .with_inserted_indices(Indices::U32(idx));
                    let mat = fx.vtm.as_mut().and_then(|v| v.material(&m.material, SHELL_TEXTURE_LEVEL, images, vt_mats)).unwrap_or_else(|| {
                        crate::vtmat::flat(vt_mats, StandardMaterial { base_color: Color::srgb(0.4, 0.4, 0.4), ..default() })
                    });
                    out.push((meshes.add(mesh), mat));
                }
            }
            Err(e) => eprintln!("fx: projectile model {md6}: {e:#}"),
        }
    }
    if let Some(Some(d)) = fx.proj_defs.get_mut(name) {
        d.meshes = Some(out.clone());
    }
    out
}

/// Bevy transform of a projectile model at idTech `pos` / `axis` with renderModelInfo.scale.
fn proj_transform(pos: G3, axis: &Axis, scale: G3) -> Transform {
    let c0 = to_bevy(-axis.y);
    let c1 = to_bevy(axis.z);
    let c2 = to_bevy(-axis.x);
    let rot = Quat::from_mat3(&Mat3::from_cols(c0, c1, c2));
    // idTech scale (x, y, z) on the Bevy axes (-y, z, -x).
    Transform { translation: to_bevy(pos), rotation: rot, scale: Vec3::new(scale.y, scale.z, scale.x) }
}

/// weapon decl `shellEmitter` (fire mode +0x1230) and its idPieceEmitter.
struct ShellEmitter {
    emitter: idfx::pieces::PieceEmitter,
    base_speed: f32,
    delta_speed: f32,
    delta_angle: f32,
    /// Per render surface: mesh entity, mesh, VT material name.
    meshes: Vec<(Entity, Handle<Mesh>)>,
    material: String,
}

struct WorldSystem {
    sys: idfx::sim::System,
    origin: G3,
    axis: Axis,
}

/// idDeclProjectile::tracerInfo_t (projectile decl +0x3e8).
#[derive(Clone, Debug)]
struct TracerInfo {
    /// tracerMtr (NULL for the gauss beams, which only draw ribbons).
    material: Option<String>,
    /// tracerRibbonInfo_t list: ribbon decl and its start offset.
    ribbons: Vec<(Arc<RibbonDecl>, G3)>,
    tracers: i32,
    /// doRandomTracers (+0x1c) and randomTracerType (+0x24: 0 TRACERRANDOM_ASGROUP, 1 TRACERRANDOM_ONEPERGROUP).
    random: bool,
    random_type: i32,
    speed: f32,
    length: f32,
    height: f32,
    lifetime: i32,
    fade_out: i32,
}

/// Whether a hitscan shot draws its tracer (0x140edbdf0 at 0x140edc231..0x140edc307), once the weapon, a tracer
/// material or ribbon and tracers > 0 are there. doRandomTracers: ASGROUP draws the game LCG and fires when
/// ((seed >> 10) & 0x7fff) % tracers == 0, ONEPERGROUP always fires, other types never; otherwise the ordered cadence
/// round % tracers == 0 (the weapon's round counter, weapon +0x97c).
fn tracer_shot(info: &TracerInfo, round: i32, rng: &mut idfx::IdRandom) -> bool {
    if info.tracers <= 0 {
        return false;
    }
    if !info.random {
        return round % info.tracers == 0;
    }
    match info.random_type {
        0 => rng.next_int() as i32 % info.tracers == 0,
        1 => true,
        _ => false,
    }
}

/// One live tracer (FUN_141858e10 record).
struct TracerRun {
    t0: i32,
    life: i32,
    fade: i32,
    speed: f32,
    start: G3,
    dir: G3,
    length: f32,
    height: f32,
    material: String,
}

/// cg_tracerMuzzleAxisError: min cosine between the shot and the muzzle axis for a tracer.
const TRACER_MUZZLE_AXIS_ERROR: f32 = 0.5;

fn container() -> Option<Arc<Container>> {
    let doom = idres::find_install()?;
    Container::open(&doom.join("base"), "gameresources").ok().map(Arc::new)
}

fn load(mut commands: Commands, combat: Res<Combat>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<FxMaterial>>, mut ribbon_assets: ResMut<Assets<RibbonMaterial>>) {
    let Some(c) = container() else {
        eprintln!("fx: DOOM install not found");
        return;
    };
    let db = DeclDb::new(c);
    let mut weapons = Vec::new();
    let mut weapon_tags = Vec::new();
    for (i, def) in combat.arsenal.defs.iter().enumerate() {
        let tags = idfx::tags::load(db.container(), &def.hands_md6).ok().and_then(|mut p| p.remove("_info")).unwrap_or_default();
        weapon_tags.push(tags);
        let name = db.get("weapon", &def.decl).ok().and_then(|b| b.str("edit.weaponFX").map(str::to_string));
        let mgr = name.and_then(|n| match FxDecl::load(&db, &n) {
            Ok(d) => Some(FxManager::new(d, 0x1234 + i as u32)),
            Err(e) => {
                eprintln!("fx: {e:#}");
                None
            }
        });
        weapons.push(mgr);
    }
    if std::env::var("RANCHER_FX_TRACE").is_ok() {
        for (i, w) in weapons.iter().enumerate() {
            println!("[fx] weapon {i}: {}", w.as_ref().map(|m| format!("{} ({} actions)", m.decl.name, m.decl.actions.len())).unwrap_or("-".into()));
        }
    }
    let mut fx = Fx {
        db,
        weapons,
        weapon_tags,
        materials: HashMap::new(),
        meshes: HashMap::new(),
        lights: Vec::new(),
        last_fire: None,
        prev_ms: 0,
        shown: None,
        impacts: Default::default(),
        world: Vec::new(),
        game_rng: idfx::IdRandom(0),
        world_rng: idfx::IdRandom(0x5eed),
        surf_count: vec![(i32::MIN, 0); 64],
        tracer_info: HashMap::new(),
        tracers: Vec::new(),
        tracer_rounds: HashMap::new(),
        tracer_shot: None,
        shells: Vec::new(),
        vtm: None,
        prev_yaw: None,
        proj_defs: HashMap::new(),
        projs: Vec::new(),
        env: idfx::env::DEFAULT,
        decal_textures: HashMap::new(),
        decals: Vec::new(),
        ribbon_sets: HashMap::new(),
        ribbon_rng: idfx::IdRandom(0),
        ribbon_decls: HashMap::new(),
        ribbon_mats: HashMap::new(),
        ribbon_meshes: HashMap::new(),
        black: None,
        map_fx: Vec::new(),
    };
    let map_env = std::env::var("RANCHER_MAP").ok().filter(|m| !m.is_empty()).and_then(|m| idfx::env::map_env(fx.db.container(), &m));
    fx.env = idfx::env::load(fx.db.container(), map_env.as_deref());
    // Debug: RANCHER_FX_PARTICLE_MULT=<f> overrides the env's $particleMult (particles and ribbons).
    if let Some(m) = std::env::var("RANCHER_FX_PARTICLE_MULT").ok().and_then(|v| v.parse().ok()) {
        fx.env.particle_mult = m;
    }
    if std::env::var("RANCHER_FX_TRACE").is_ok() {
        println!("[fx] env {map_env:?}: {:?}", fx.env);
    }
    // RANCHER_MAP_FX=0 leaves the level's emitters out.
    if let Some(map) = std::env::var("RANCHER_MAP").ok().filter(|m| !m.is_empty() && std::env::var("RANCHER_MAP_FX").as_deref() != Ok("0")) {
        load_map_fx(&mut fx, &map);
    }
    for def in &combat.arsenal.defs {
        let se = fx.db.get("weapon", &def.decl).ok().and_then(|b| b.block("edit.shellEmitter").cloned());
        let shell = se.and_then(|se| {
            let name = se.str("breakableEmitter").filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("NULL"))?.to_string();
            match idfx::pieces::PieceEmitter::load(&fx.db, &name) {
                Ok(emitter) => {
                    let material = emitter.model.source_surfaces.first().map(|s| s.material.clone()).unwrap_or_default();
                    Some(ShellEmitter {
                        emitter,
                        base_speed: se.f32("baseSpeed").unwrap_or(0.0),
                        delta_speed: se.f32("deltaSpeed").unwrap_or(0.0),
                        delta_angle: se.f32("deltaAngle").unwrap_or(0.0),
                        meshes: Vec::new(),
                        material,
                    })
                }
                Err(e) => {
                    eprintln!("fx: shellEmitter {name}: {e:#}");
                    None
                }
            }
        });
        fx.shells.push(shell);
    }
    if std::env::var("RANCHER_FX_TRACE").is_ok() {
        for (i, se) in fx.shells.iter().enumerate() {
            if let Some(se) = se {
                println!("[fx] weapon {i} shells {} ({} pieces, {}) speed {} +{} angle {}", se.emitter.def.name, se.emitter.pieces.len(), se.material, se.base_speed, se.delta_speed, se.delta_angle);
            }
        }
    }
    if fx.shells.iter().any(Option::is_some) {
        if let Some(doom) = idres::find_install() {
            fx.vtm = Some(crate::vtmat::VtMaterials::open(&doom));
        }
    }
    // Impact tables and tracers of every arsenal projectile.
    for def in &combat.arsenal.defs {
        if !def.projectile.name.is_empty() {
            fx.impacts.projectile(&fx.db, &def.projectile.name);
            tracer_info(&mut fx, &def.projectile.name);
        }
    }
    // Precache every particle material the weapon FX use (textures upload before the first shot).
    let names: Vec<String> = fx
        .weapons
        .iter()
        .flatten()
        .flat_map(|m| m.decl.actions.iter().filter_map(|a| a.particle.clone()))
        .chain(fx.impacts.particles.values().cloned())
        .flat_map(|p| p.stages.iter().map(|s| s.material.clone()).collect::<Vec<_>>())
        .collect();
    let tracer_mats: Vec<String> = fx.tracer_info.values().flatten().filter_map(|t| t.material.clone()).collect();
    for n in tracer_mats {
        tracer_material(&mut fx, &n, &mut images, &mut mats);
    }
    // Quad-damage-only ribbons are never drawn here.
    let ribbon_mats: Vec<String> = fx
        .tracer_info
        .values()
        .flatten()
        .flat_map(|t| t.ribbons.iter().filter(|(d, _)| d.visibility != idfx::ribbon::Visibility::Quad).map(|(d, _)| d.material.clone()))
        .collect();
    for n in ribbon_mats {
        ribbon_material(&mut fx, &n, &mut images, &mut ribbon_assets);
    }
    for n in names {
        material(&mut fx, &n, &mut images, &mut mats);
    }
    commands.insert_resource(fx);
}

/// BIM format byte -> GPU format (gamedata/re/ASSETS.md).
fn texture_format(f: u8) -> Option<(TextureFormat, u32, u32)> {
    // (format, block size in texels, bytes per block)
    Some(match f {
        3 => (TextureFormat::Rgba8Unorm, 1, 4),
        // `$luminancealpha` images: 2 bytes per texel (luminance, alpha); the GPU particle programs read them
        // as RG ("for RG switch!" in gpuparticle/add).
        6 => (TextureFormat::Rg8Unorm, 1, 2),
        10 => (TextureFormat::Bc1RgbaUnorm, 4, 8),
        11 => (TextureFormat::Bc3RgbaUnorm, 4, 16),
        23 => (TextureFormat::Bc7RgbaUnorm, 4, 16),
        24 => (TextureFormat::Bc4RUnorm, 4, 8),
        25 => (TextureFormat::Bc5RgUnorm, 4, 16),
        _ => return None,
    })
}

fn load_image(c: &Container, path: &str) -> Option<Image> {
    let bytes = c.read_by_name(path).ok()?;
    let img = idres::bimage::BImage::parse(&bytes).ok()?;
    let (format, block, bpb) = texture_format(img.format)?;
    let mut mips: Vec<&idres::bimage::Mip> = img.mips.iter().filter(|m| m.dest_z == 0).collect();
    mips.sort_by_key(|m| m.level);
    let mut data = Vec::new();
    let mut levels = 0;
    for m in &mips {
        let w = (img.width >> m.level).max(1);
        let h = (img.height >> m.level).max(1);
        let need = (w.div_ceil(block) * h.div_ceil(block) * bpb) as usize;
        let src = &bytes[m.data.clone()];
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
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    Some(image)
}

fn material(fx: &mut Fx, name: &str, images: &mut Assets<Image>, mats: &mut Assets<FxMaterial>) -> Option<Handle<FxMaterial>> {
    if let Some(e) = fx.materials.get(name) {
        return e.as_ref().map(|e| e.handle.clone());
    }
    let entry = ParticleMaterial::load(fx.db.container(), name).ok().and_then(|m| {
        let img = m.image.as_deref().and_then(|p| load_image(fx.db.container(), p));
        if img.is_none() {
            eprintln!("fx: no image for particle material {name} ({:?})", m.image);
        }
        let img = img?;
        let debug = std::env::var("RANCHER_FX_DEBUG").is_ok();
        if m.is_gpu() {
            // gpuparticle/add (3) or gpuparticle/blend (4): y = $factor.
            let mode = if debug { 2.0 } else if m.is_gpu_add() { 3.0 } else { 4.0 };
            let handle = mats.add(FxMaterial { texture: images.add(img), parms: Vec4::new(0.0, m.factor.unwrap_or(1.0), 0.0, mode), tint: Vec4::ONE });
            return Some(MatEntry { handle });
        }
        let emissive_mult = m.emissive_mult.unwrap_or(EMISSIVE_MULT_DEFAULT);
        let handle = mats.add(FxMaterial { texture: images.add(img), parms: Vec4::new(m.emissive, emissive_mult, fx.env.particle_mult, if debug { 2.0 } else { 0.0 }), tint: Vec4::ONE });
        Some(MatEntry { handle })
    });
    let h = entry.as_ref().map(|e| e.handle.clone());
    fx.materials.insert(name.to_string(), entry);
    h
}

/// A plain material for tracers (stageprogram basicadd: tex * vertex colour * factor, additive).
fn tracer_material(fx: &mut Fx, name: &str, images: &mut Assets<Image>, mats: &mut Assets<FxMaterial>) -> Option<Handle<FxMaterial>> {
    let key = format!("tracer:{name}");
    if let Some(e) = fx.materials.get(&key) {
        return e.as_ref().map(|e| e.handle.clone());
    }
    let path = format!("generated/decls/material/{name}.decl");
    let text = fx.db.container().read_by_name(&path).ok().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut map = None;
    let mut factor = 1.0f32;
    let mut tint = Vec4::ONE;
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("transmap") {
            let m = v.trim().trim_matches('"').to_ascii_lowercase();
            map = Some(m.strip_suffix(".tga").unwrap_or(&m).to_string());
        } else if let Some(v) = l.strip_prefix("factor") {
            factor = v.trim().parse().unwrap_or(1.0);
        } else if let Some(v) = l.strip_prefix("color2") {
            let f: Vec<f32> = v.trim().trim_matches(|c| c == '{' || c == '}').split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if f.len() == 4 {
                tint = Vec4::new(f[0], f[1], f[2], f[3]);
            }
        }
    }
    let mode = if std::env::var("RANCHER_FX_DEBUG").is_ok() { 2.0 } else { 1.0 };
    let entry = map
        .and_then(|m| load_image(fx.db.container(), &format!("generated/image/{m}.bimage")))
        .map(|img| MatEntry { handle: mats.add(FxMaterial { texture: images.add(img), parms: Vec4::new(0.0, factor, 0.0, mode), tint }) });
    if entry.is_none() {
        eprintln!("fx: no image for tracer material {name}");
    }
    let h = entry.as_ref().map(|e| e.handle.clone());
    fx.materials.insert(key, entry);
    h
}

fn tracer_info(fx: &mut Fx, projectile: &str) -> Option<TracerInfo> {
    if let Some(t) = fx.tracer_info.get(projectile) {
        return t.clone();
    }
    let b = fx.db.get("projectile", projectile).ok();
    let t = b.as_deref().and_then(|b| b.block("edit.tracerInfo")).cloned();
    let t = t.and_then(|t| {
        let f = |k: &str, d: f32| t.f32(k).unwrap_or(d);
        // ribbonDecls: the hitscan code only looks at item[0]'s decl to decide on ribbons (0x140edc1ae).
        let mut ribbons = Vec::new();
        if let Some(rd) = t.block("ribbonDecls") {
            for i in 0..rd.f32("num").unwrap_or(0.0) as usize {
                let Some(item) = rd.block(&format!("item[{i}]")) else { continue };
                let Some(name) = item.str("ribbonDecl").filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("NULL")) else {
                    if i == 0 {
                        break;
                    }
                    continue;
                };
                let off = item.block("offset").map_or(G3::ZERO, |o| G3::new(o.f32("x").unwrap_or(0.0), o.f32("y").unwrap_or(0.0), o.f32("z").unwrap_or(0.0)));
                if let Some(d) = ribbon_decl(fx, name) {
                    ribbons.push((d, off));
                }
            }
        }
        let material = t.str("tracerMtr").filter(|m| !m.eq_ignore_ascii_case("NULL")).map(str::to_string);
        if material.is_none() && ribbons.is_empty() {
            return None;
        }
        // tracerInfo_t defaults where the decl is silent: idDeclProjectile ctor 0x1406ec0e0 (0x1406ec5d1..0x1406ec632):
        // tracers 5, doRandomTracers false, ASGROUP, speed 2500, length 48, height 4, tracerZoomedOffset (10, 0, -5)
        // (zoomed starts are not used here), lifetime -1 ("programmatically determined"), fade 0.
        let random_type = match t.str("randomTracerType") {
            Some(v) if v.eq_ignore_ascii_case("TRACERRANDOM_ONEPERGROUP") => 1,
            _ => 0,
        };
        Some(TracerInfo {
            material,
            ribbons,
            tracers: f("tracers", 5.0) as i32,
            random: idfx::decl::bool_or(Some(&t), "doRandomTracers", false),
            random_type,
            speed: f("tracerSpeed", 2500.0),
            length: f("tracerLength", 48.0),
            height: f("tracerHeight", 4.0),
            lifetime: f("tracerLifetime", -1.0) as i32,
            fade_out: f("tracerFadeOutTime", 0.0) as i32,
        })
    });
    fx.tracer_info.insert(projectile.to_string(), t.clone());
    t
}

/// FUN_141858e10: a tracer `length` long moving from `start` toward `end` at `speed`, shown at least 0.05 s
/// (the hitscan code's argument), lifetime capped by tracerLifetime when >= 0.
fn add_tracer(now: i32, start: G3, end: G3, info: &TracerInfo) -> TracerRun {
    let (mut speed, mut length) = (info.speed, info.length);
    let dir = idfx::normalize(end - start);
    let dist = (end - start).length();
    if length < 0.0 {
        length = dist;
    }
    let min_time = 0.05f32;
    let mut travel = dist - length * 0.5;
    if travel < speed * min_time {
        length = (dist + dist) * min_time - (min_time + min_time) * speed;
        if length < 0.0 {
            length = dist * 0.5;
            speed = dist / min_time - (dist * 0.5 * 0.5) / min_time;
        }
        travel = min_time * speed;
    }
    let k = if speed == 0.0 { 0.666_666_7 } else { 1000.0 / speed };
    let mut life = (k * travel).min(3000.0) as i32;
    if info.lifetime >= 0 {
        life = if life < 0 { 0 } else { life.min(info.lifetime) };
    }
    TracerRun { t0: now, life, fade: info.fade_out, speed, start, dir, length, height: info.height, material: info.material.clone().unwrap_or_default() }
}

fn to_id(v: Vec3) -> G3 {
    G3::new(-v.z, -v.x, v.y)
}

fn to_bevy(v: G3) -> Vec3 {
    Vec3::new(-v.y, v.z, -v.x)
}

/// View-model joints placed in the world through the main camera.
struct ViewTags<'a> {
    joints: &'a ViewJoints,
    /// The shown weapon's `_info` tags.
    tags: &'a [idfx::tags::Tag],
    cam: Transform,
}

impl ViewTags<'_> {
    fn place(&self, m: glam::Mat4) -> (G3, Axis) {
        let p = Vec3::from_array(m.transform_point3(glam::Vec3::ZERO).to_array());
        let dir = |v: glam::Vec3| to_id(self.cam.rotation * Vec3::from_array(m.transform_vector3(v).to_array()));
        (to_id(self.cam.transform_point(p)), Axis { x: dir(glam::Vec3::X), y: dir(glam::Vec3::Y), z: dir(glam::Vec3::Z) })
    }
}

impl TagSource for ViewTags<'_> {
    fn tag(&self, name: &str) -> Option<(G3, Axis)> {
        if let Some(t) = self.tags.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
            let parent = self.joints.weapon_joint(&t.parent).or_else(|| self.joints.hands_joint(&t.parent))?;
            // The props `rot` reads as idQuat::ToMat3 rows (the axes of the conjugate rotation in glam's terms).
            // INTERIM inference: every weapon's shell_eject tag sits on the right of the gun and only this reading
            // ejects away from it (shotgun/HAR/chaingun -> forward-right-up); identity tags are unaffected.
            let q = glam::Quat::from_xyzw(t.rot[0], t.rot[1], t.rot[2], t.rot[3]).conjugate();
            let local = glam::Mat4::from_rotation_translation(q, glam::Vec3::from_array(t.trans));
            return Some(self.place(parent * local));
        }
        self.joints.weapon_joint(name).or_else(|| self.joints.hands_joint(name)).map(|m| self.place(m))
    }
    fn parent(&self) -> (G3, Axis) {
        let r = self.cam.rotation;
        (to_id(self.cam.translation), Axis { x: to_id(r * Vec3::NEG_Z), y: to_id(r * Vec3::NEG_X), z: to_id(r * Vec3::Y) })
    }
}

/// A manager's FX_SCREEN_SHAKE starts / stops for the one listener here, the player's view at `view` (idTech
/// space); see idfx::fx::screen_shake_start. Stops send the reset 0x1416ef870 gives listener vslot 0x28.
fn shakes_to_view(m: &mut FxManager, view: G3, cam_fx: &mut crate::camfx::CamFx) {
    for ev in std::mem::take(&mut m.shakes) {
        match ev {
            idfx::fx::ShakeEvent::Start { action, origin, action_origin } => {
                if let Some(c) = idfx::fx::screen_shake_start(&m.decl.actions[action], origin, action_origin, view) {
                    if std::env::var("RANCHER_FX_TRACE").is_ok() {
                        println!("[fx shake] {} action {action}: {c:?}", m.decl.name);
                    }
                    cam_fx.fx_shakes.push(c.into());
                }
            }
            idfx::fx::ShakeEvent::Stop { .. } => cam_fx.fx_shakes.push(crate::camfx::FxShake::Camera { magnitude: 0.0, position: [0.0; 3], fade_start: 0.0, fade_end: -1.0 }),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut commands: Commands,
    fx: Option<ResMut<Fx>>,
    combat: Res<Combat>,
    sim: Res<crate::Sim>,
    joints: Res<ViewJoints>,
    events: Res<HandsEvents>,
    cams: Query<(&Camera, &Transform), Without<PointLight>>,
    mut lights: Query<(&mut PointLight, &mut Transform, &mut Visibility), With<FxLight>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<FxMaterial>>,
    mut vis: Query<&mut Visibility, (With<FxMesh>, Without<FxLight>)>,
    mut impacts: MessageReader<crate::combat::ShotImpact>,
    mut shot_tracers: MessageReader<crate::combat::ShotTracer>,
    mut vt_mats: ResMut<Assets<crate::vtmat::VtMaterial>>,
    (mut decal_mats, mut ribbon_assets, mut cam_fx): (ResMut<Assets<DecalMaterial>>, ResMut<Assets<RibbonMaterial>>, ResMut<crate::camfx::CamFx>),
) {
    let Some(mut fx) = fx else { return };
    let fx = &mut *fx;
    let now = sim.game_ms;
    let Some((_, cam)) = cams.iter().find(|(c, _)| c.order == 0) else { return };
    let shown = joints.weapon;
    let no_tags = Vec::new();
    let wtags = shown.and_then(|w| fx.weapon_tags.get(w)).unwrap_or(&no_tags).clone();
    let tags = ViewTags { joints: &joints, tags: &wtags, cam: *cam };

    // Triggers.
    let fired = combat.arsenal.last_fire_ms;
    if fired.is_some() && fired != fx.last_fire {
        let w = combat.arsenal.current;
        if let Some(Some(m)) = fx.weapons.get_mut(w) {
            if let Some(c) = idfx::fx::condition("FX_WEAPON_START_FIRE") {
                m.condition(c, idfx::fx::EXTRA_PRIMARY_FIRE, now, &tags);
            }
        }
    }
    fx.last_fire = fired;
    let yaw = sim.player.view_angles[1];
    let frame_s = sim.msec_last as f32 * 0.001;
    for e in &events.0 {
        if e.event.name.eq_ignore_ascii_case("ae_ejectShell") {
            if let Some(w) = shown {
                // 0x140d5d340: player velocity plus the turn rate times the lever arm to the tag.
                if let (Some(Some(se)), Some((origin, axis))) = (fx.shells.get_mut(w), tags.tag("shell_eject")) {
                    let ph = &sim.player.physics;
                    let mut vel = G3::from_array(ph.velocity.to_array());
                    if frame_s > 0.0 {
                        // INTERIM: the two angle getters (+0xb8 / +0xc0) taken as this / last frame's view yaw;
                        // the difference is wrapped to +-180 (the view yaw here is kept in 0..360).
                        let dyaw = (yaw - fx.prev_yaw.unwrap_or(yaw) + 540.0).rem_euclid(360.0) - 180.0;
                        let w = (dyaw / frame_s) * -0.017_453_292;
                        let r = origin - G3::from_array(ph.origin.to_array());
                        vel.x += w * r.y - r.z * 0.0;
                        vel.y += r.z * 0.0 - w * r.x;
                    }
                    se.emitter.emit(now, &mut fx.game_rng, origin, &axis, vel, se.base_speed, se.delta_speed, se.delta_angle);
                    if std::env::var("RANCHER_FX_TRACE").is_ok() {
                        println!("[fx {now}] eject shell at {origin} axis.x {} vel {vel}", axis.x);
                    }
                }
                if let (Some(Some(m)), Some(c)) = (fx.weapons.get_mut(w), idfx::fx::condition("FX_WEAPON_START_SHELL_EJECT")) {
                    m.condition(c, idfx::fx::EXTRA_PRIMARY_FIRE, now, &tags);
                }
            }
        }
    }
    fx.prev_yaw = Some(yaw);

    // Impacts (idGameLocal::ProjectileImpactEffect -> ImpactEffect).
    let mut exploded: HashMap<(usize, u32), G3> = HashMap::new();
    // The player's absolute clip bounds for the impact view-shake query (INTERIM: the engine's clip model may pad
    // its absBounds; the stance hull's bounds are used as they are).
    let player_bounds = {
        let ph = &sim.player.physics;
        let hull = if ph.ducked() { &ph.crouch_shape } else { &ph.normal_shape };
        let o = G3::from_array(ph.origin.to_array());
        [o + G3::from_array(hull.min.to_array()), o + G3::from_array(hull.max.to_array())]
    };
    for ev in impacts.read() {
        exploded.insert((ev.weapon, ev.shot), G3::from_array(ev.pos.to_array()));
        let p = fx.impacts.projectile(&fx.db, &ev.projectile);
        let pos = G3::from_array(ev.pos.to_array());
        // Surface: the message's surface name (demons' body parts), "flesh" for range targets, else the SURFTYPE
        // of the collision polygon at the hit (the engine's trace material surface type).
        let surftype = if ev.surface.is_empty() && !ev.target { surftype_at(&sim.world, pos, G3::from_array(ev.normal.to_array())) } else { None };
        let surface = if !ev.surface.is_empty() {
            idfx::impact::surface_index(&ev.surface)
        } else if ev.target {
            idfx::impact::SURFACE_FLESH
        } else {
            surftype.map_or(idfx::impact::SURFACE_DEFAULT, idfx::impact::surftype_effect)
        };
        let axis = idfx::impact::normal_axis(G3::from_array(ev.normal.to_array()));
        let mut tables = vec![p.table.clone()];
        if let Some(lim) = &p.limited {
            let c = &mut fx.surf_count[surftype.map_or(surface, |t| t as usize).min(63)];
            if c.0 != now {
                *c = (now, 0);
            }
            if c.1 < p.max_per_surface {
                c.1 += 1;
                tables.push(Some(lim.clone()));
            }
        }
        for t in tables.into_iter().flatten() {
            let e = match surftype {
                Some(st @ (33 | 34)) => t.for_surftype(st, &mut fx.game_rng),
                _ => t.effects.get(surface),
            };
            let Some(e) = e else { continue };
            if !e.decals.is_empty() && !(e.team_based && e.decals.len() >= 2) {
                // Decal pick: rand % count (game idRandom), then the render decal.
                let i = fx.game_rng.next_int() as usize % e.decals.len();
                let name = e.decals[i].clone();
                spawn_decal(fx, &mut commands, &sim.world, &name, e, pos, G3::from_array(ev.normal.to_array()), now, &mut meshes, &mut images, &mut decal_mats);
            }
            if !e.particles.is_empty() {
                let i = fx.game_rng.next_int() as usize % e.particles.len();
                if let Some(d) = &e.particles[i] {
                    let seed = fx.world_rng.next_int();
                    fx.world.push(WorldSystem { sys: idfx::sim::System::new(d.clone(), now, seed), origin: pos, axis });
                }
            }
            if let Some(c) = e.view_shake_call(pos, player_bounds, to_id(cam.translation)) {
                if std::env::var("RANCHER_FX_TRACE").is_ok() {
                    println!("[fx shake] impact {} at {pos}: {c:?}", t.name);
                }
                cam_fx.fx_shakes.push(c.into());
            }
        }
    }
    // Tracers (hitscan fire 0x140edbdf0 -> FUN_141858e10).
    for ev in shot_tracers.read() {
        let Some(info) = tracer_info(fx, &ev.projectile) else {
            if std::env::var("RANCHER_FX_TRACE").is_ok() {
                println!("[fx {now}] tracer {}: no tracerInfo", ev.projectile);
            }
            continue;
        };
        let on = match fx.tracer_shot {
            Some((w, s, on)) if w == ev.weapon && s == ev.shot => on,
            _ => {
                let n = fx.tracer_rounds.entry(ev.weapon).or_insert(0);
                // The LCG draw comes from fx.game_rng, this crate's stand-in for the game random (as the decal picks).
                let on = tracer_shot(&info, *n, &mut fx.game_rng);
                *n += 1;
                fx.tracer_shot = Some((ev.weapon, ev.shot, on));
                on
            }
        };
        if !on {
            continue;
        }
        let to = G3::from_array(ev.to.to_array());
        let (start, muzzle_axis) = tags.tag("muzzle").map(|(o, a)| (o, Some(a.x))).unwrap_or((G3::from_array(ev.from.to_array()), None));
        let dir = idfx::normalize(to - start);
        if muzzle_axis.is_some_and(|ax| dir.dot(ax) < TRACER_MUZZLE_AXIS_ERROR) {
            if std::env::var("RANCHER_FX_TRACE").is_ok() {
                println!("[fx {now}] tracer {} skipped: {start} -> {to} dir {dir} vs muzzle axis {muzzle_axis:?} (from {:?})", ev.projectile, ev.from);
            }
            continue;
        }
        if info.material.is_some() && info.speed > 0.0 {
            fx.tracers.push(add_tracer(now, start, to, &info));
        }
        // 0x140f03060: the weapon's next free ribbon set (vfunc 0x630) restarts at the muzzle and runs to the
        // hit until now + tracerLifetime.
        if !info.ribbons.is_empty() {
            let sets = fx.ribbon_sets.entry(ev.projectile.clone()).or_insert_with(|| (0..10).map(|_| RibbonSet { expire: i32::MIN, ribbons: Vec::new() }).collect());
            let i = RibbonSet::pick(sets);
            sets[i].fire(&info.ribbons, now, start, to, info.lifetime, &mut fx.ribbon_rng);
            if std::env::var("RANCHER_FX_TRACE").is_ok() {
                let n: Vec<i32> = sets[i].ribbons.iter().map(|r| r.num_active).collect();
                println!("[fx {now}] ribbons {} set {i} {start} -> {to} nodes {n:?}", ev.projectile);
            }
        }
    }

    // Projectiles: idProjectile::Launch (0x140f39840) raises FX_PROJECTILE_LAUNCH and FX_PROJECTILE_TRAIL with
    // the projectile's extra flags (0 here, 0x800 with quad); the explode (0x140f34ba0) raises
    // FX_PROJECTILE_EXPLODE (EXPLODE_ALT for manual detonation, not modelled).
    let (c_launch, c_trail, c_explode) = (idfx::fx::condition("FX_PROJECTILE_LAUNCH"), idfx::fx::condition("FX_PROJECTILE_TRAIL"), idfx::fx::condition("FX_PROJECTILE_EXPLODE"));
    let mut seen = vec![false; fx.projs.len()];
    for cp in &combat.projectiles {
        let key = (cp.weapon, cp.shot);
        let pos = G3::from_array(cp.pos.to_array());
        let axis = velocity_axis(G3::from_array(cp.vel.to_array()));
        if let Some(i) = fx.projs.iter().enumerate().position(|(i, p)| !seen[i] && p.alive && p.key == key) {
            seen[i] = true;
            fx.projs[i].pos = pos;
            fx.projs[i].axis = axis;
            continue;
        }
        let name = cp.def.projectile.name.clone();
        if !proj_def(fx, &name) {
            continue;
        }
        let mut mgr = fx.proj_defs[&name].as_ref().and_then(|d| d.fx.clone()).map(|d| FxManager::new(d, 0x9a00 + cp.shot));
        if let (Some(m), Some(Some(d))) = (mgr.as_mut(), fx.proj_defs.get(&name)) {
            let pt = ProjTags { pos, axis, scale: d.scale, tags: &d.tags };
            for c in [c_launch, c_trail].into_iter().flatten() {
                m.condition(c, 0, now, &pt);
            }
        }
        let mut entities = Vec::new();
        let scale = fx.proj_defs[&name].as_ref().map_or(G3::ONE, |d| d.scale);
        for (h, mat) in proj_meshes(fx, &name, &mut meshes, &mut images, &mut vt_mats) {
            entities.push(commands.spawn((Mesh3d(h), MeshMaterial3d(mat), proj_transform(pos, &axis, scale), RenderLayers::layer(0))).id());
        }
        fx.projs.push(ProjFx { key, def: name, mgr, pos, axis, entities, alive: true });
        seen.push(true);
    }
    for (i, p) in fx.projs.iter_mut().enumerate() {
        if !p.alive || seen[i] {
            continue;
        }
        p.alive = false;
        for e in p.entities.drain(..) {
            commands.entity(e).despawn();
        }
        let Some(m) = p.mgr.as_mut() else { continue };
        let def = fx.proj_defs.get(&p.def).and_then(|d| d.as_ref());
        let (scale, tags) = def.map_or((G3::ONE, &[][..]), |d| (d.scale, &d.tags[..]));
        if let Some(at) = exploded.get(&p.key) {
            p.pos = *at;
        }
        let pt = ProjTags { pos: p.pos, axis: p.axis, scale, tags };
        // INTERIM: the trail's looping actions are stopped (with their fade-out) when the projectile goes.
        m.stop_looping(now);
        if let (Some(c), true) = (c_explode, exploded.contains_key(&p.key)) {
            m.condition(c, 0, now, &pt);
        }
    }
    for p in fx.projs.iter_mut().filter(|p| p.alive) {
        if let Some(Some(d)) = fx.proj_defs.get(&p.def) {
            let t = proj_transform(p.pos, &p.axis, d.scale);
            for e in &p.entities {
                commands.entity(*e).insert(t);
            }
        }
    }

    // Simulate.
    let r = cam.rotation;
    let view = View { origin: to_id(cam.translation), right: to_id(r * Vec3::X), up: to_id(r * Vec3::Y) };
    let prev = if fx.prev_ms == 0 { now } else { fx.prev_ms };
    let mut batches: HashMap<(String, Layer), Vec<Quad>> = HashMap::new();
    let mut light_out = Vec::new();
    // Particle lights (isLight stages), at most r_particleMaxParticleLights (256) per frame.
    let mut plights: Vec<idfx::sim::ParticleLight> = Vec::new();
    for (wi, m) in fx.weapons.iter_mut().enumerate() {
        let Some(m) = m else { continue };
        // Only the weapon on screen has joints this frame; a holstered weapon's FX stop drawing.
        if Some(wi) != shown {
            continue;
        }
        m.update(now, &tags);
        shakes_to_view(m, view.origin, &mut cam_fx);
        let layer = Layer::View;
        light_out.extend(m.lights(now));
        for (ai, sys, origin, axis, color) in m.systems_mut() {
            let frame = Frame {
                time_ms: now,
                prev_time_ms: prev,
                origin,
                axis,
                view,
                entity_color: glam::Vec4::from_array(color.to_array()),
                fade: 1.0,
                size_scale: 1.0,
                wind: G3::ZERO,
                shadow: 1.0,
                velocity: G3::ZERO,
            };
            let mut quads = Vec::new();
            sys.generate(&frame, &mut quads);
            sys.lights(&frame, &mut plights);
            let _ = ai;
            for q in quads {
                let mat = &sys.decl.stages[q.stage].material;
                batches.entry((mat.clone(), layer)).or_default().push(q);
            }
        }
    }
    for w in fx.world.iter_mut() {
        let frame = Frame {
            time_ms: now,
            prev_time_ms: prev,
            origin: w.origin,
            axis: w.axis,
            view,
            entity_color: glam::Vec4::ONE,
            fade: 1.0,
            size_scale: 1.0,
            wind: G3::ZERO,
            shadow: 1.0,
            velocity: G3::ZERO,
        };
        let mut quads = Vec::new();
        w.sys.generate(&frame, &mut quads);
        w.sys.lights(&frame, &mut plights);
        for q in quads {
            let mat = &w.sys.decl.stages[q.stage].material;
            batches.entry((mat.clone(), Layer::World)).or_default().push(q);
        }
    }
    fx.world.retain(|w| !w.sys.finished(now));
    let fwd = to_id(cam.rotation * Vec3::NEG_Z);
    for em in fx.map_fx.iter_mut() {
        let rel = em.origin - view.origin;
        if (em.dormancy > 0.0 && rel.length() > em.dormancy) || rel.dot(fwd) < -1024.0 {
            continue;
        }
        if let Some(sys) = em.sys.as_mut() {
            let frame = Frame {
                time_ms: now,
                prev_time_ms: prev,
                origin: em.origin,
                axis: em.axis,
                view,
                entity_color: em.color,
                fade: 1.0,
                size_scale: 1.0,
                wind: G3::ZERO,
                shadow: 1.0,
                velocity: G3::ZERO,
            };
            let mut quads = Vec::new();
            sys.generate(&frame, &mut quads);
            sys.lights(&frame, &mut plights);
            for q in quads {
                let mat = &sys.decl.stages[q.stage].material;
                batches.entry((mat.clone(), Layer::World)).or_default().push(q);
            }
        }
        if let Some(m) = em.mgr.as_mut() {
            let pt = ProjTags { pos: em.origin, axis: em.axis, scale: G3::ONE, tags: &[] };
            if !em.started {
                // FX_NONE (0): the map FX decls' start condition.
                let n = m.condition(0, 0, now, &pt);
                if std::env::var("RANCHER_FX_TRACE").is_ok() {
                    println!("[fx {now}] map fx {} {} started {n} actions at {}", em.name, m.decl.name, em.origin);
                }
                em.started = true;
            }
            m.update(now, &pt);
            shakes_to_view(m, view.origin, &mut cam_fx);
            light_out.extend(m.lights(now));
            for (_, sys, origin, axis, color) in m.systems_mut() {
                let frame = Frame {
                    time_ms: now,
                    prev_time_ms: prev,
                    origin,
                    axis,
                    view,
                    entity_color: glam::Vec4::from_array(color.to_array()),
                    fade: 1.0,
                    size_scale: 1.0,
                    wind: G3::ZERO,
                    shadow: 1.0,
                    velocity: G3::ZERO,
                };
                let mut quads = Vec::new();
                sys.generate(&frame, &mut quads);
                sys.lights(&frame, &mut plights);
                for q in quads {
                    let mat = &sys.decl.stages[q.stage].material;
                    batches.entry((mat.clone(), Layer::World)).or_default().push(q);
                }
            }
        }
    }
    for p in fx.projs.iter_mut() {
        let Some(m) = p.mgr.as_mut() else { continue };
        let def = fx.proj_defs.get(&p.def).and_then(|d| d.as_ref());
        let (scale, tags) = def.map_or((G3::ONE, &[][..]), |d| (d.scale, &d.tags[..]));
        let pt = ProjTags { pos: p.pos, axis: p.axis, scale, tags };
        m.update(now, &pt);
        shakes_to_view(m, view.origin, &mut cam_fx);
        light_out.extend(m.lights(now));
        for (_, sys, origin, axis, color) in m.systems_mut() {
            let frame = Frame {
                time_ms: now,
                prev_time_ms: prev,
                origin,
                axis,
                view,
                entity_color: glam::Vec4::from_array(color.to_array()),
                fade: 1.0,
                size_scale: 1.0,
                wind: G3::ZERO,
                shadow: 1.0,
                velocity: G3::ZERO,
            };
            let mut quads = Vec::new();
            sys.generate(&frame, &mut quads);
            sys.lights(&frame, &mut plights);
            for q in quads {
                let mat = &sys.decl.stages[q.stage].material;
                batches.entry((mat.clone(), Layer::World)).or_default().push(q);
            }
        }
    }
    fx.projs.retain(|p| p.alive || p.mgr.as_ref().is_some_and(|m| m.busy()));
    plights.truncate(PARTICLE_LIGHTS_MAX);
    for l in &plights {
        light_out.push(idfx::fx::LightOut {
            action: usize::MAX,
            origin: l.origin,
            axis: Axis::IDENTITY,
            color: glam::Vec4::new(l.color.x, l.color.y, l.color.z, 1.0),
            intensity: l.intensity,
            radius: G3::splat(l.radius),
            fade: 1.0,
        });
    }
    // Tracers: FUN_1418590d0 (segment advancing at speed, width toward the eye, fade over the last fadeOut ms).
    fx.tracers.retain(|t| now - t.t0 <= t.life);
    for t in &fx.tracers {
        let d = ((now - t.t0) as f32 * t.speed * 0.001).max(0.0);
        let tail = t.start + t.dir * d;
        let head = tail + t.dir * t.length;
        let side = idfx::normalize(t.dir.cross(view.origin - t.start)) * (t.height * 0.5);
        let mut c = 255u8;
        let remaining = t.life - (now - t.t0);
        if t.fade > 0 && remaining < t.fade {
            c = ((remaining * 255) / t.fade).clamp(0, 255) as u8;
        }
        let v = |p: G3, uv: [f32; 2]| idfx::sim::Vertex { pos: p, uv, uv_next: uv, color: [c; 4], generic: [0; 4], frame_frac: 0.0, alpha_scale: 1.0, color_f: None };
        let q = Quad { stage: 0, verts: [v(tail + side, [0.0, 0.0]), v(head + side, [1.0, 0.0]), v(tail - side, [0.0, 1.0]), v(head - side, [1.0, 1.0])] };
        batches.entry((format!("tracer:{}", t.material), Layer::World)).or_default().push(q);
    }
    // Tracer ribbons: 0x140f27e40 draws each live ribbon of a set until the set expires (RIBBON_SHOW_QUAD
    // ribbons only with quad damage, never here) and stops it after; drawing never expires nodes.
    let mut ribbon_batches: HashMap<String, Vec<RibbonVertex>> = HashMap::new();
    let mut segs = Vec::new();
    for sets in fx.ribbon_sets.values_mut() {
        for set in sets.iter_mut() {
            for r in set.ribbons.iter_mut().filter(|r| r.state == 0) {
                if now < set.expire && r.decl.visibility != idfx::ribbon::Visibility::Quad {
                    segs.clear();
                    let mode = r.segments(now, G3::X, G3::ZERO, &mut fx.ribbon_rng, &mut segs);
                    idfx::ribbon::expand(&segs, mode, view.origin, ribbon_batches.entry(r.decl.material.clone()).or_default());
                } else {
                    r.state = 1;
                }
            }
        }
    }
    fx.prev_ms = now;
    fx.shown = shown;
    if std::env::var("RANCHER_FX_TRACE").is_ok() {
        let n: usize = batches.values().map(Vec::len).sum();
        if n > 0 || !light_out.is_empty() {
            let m: Vec<String> = batches.iter().map(|((k, _), v)| format!("{}:{}", k.rsplit('/').next().unwrap_or(k), v.len())).collect();
            let muzzle = tags.tag("muzzle").map(|(o, _)| o);
            if muzzle.is_none() {
                println!("[fx] weapon joints {:?} hands joints {}", joints.weapon_names, joints.hands_names.len());
            }
            println!("[fx {now}] quads {n} [{}] lights {} muzzle {muzzle:?} cam {:?}", m.join(" "), light_out.len(), to_id(cam.translation));
        }
    }

    // Meshes.
    for (_, (e, _)) in fx.meshes.iter() {
        if let Ok(mut v) = vis.get_mut(*e) {
            *v = Visibility::Hidden;
        }
    }
    for ((mat_name, layer), quads) in batches {
        let mat = match mat_name.strip_prefix("tracer:") {
            Some(n) => tracer_material(fx, n, &mut images, &mut mats),
            None => material(fx, &mat_name, &mut images, &mut mats),
        };
        let Some(mat) = mat else { continue };
        let mesh = build_mesh(&quads);
        match fx.meshes.get(&(mat_name.clone(), layer)) {
            Some((e, h)) => {
                if let Some(mut m) = meshes.get_mut(h) {
                    *m = mesh;
                }
                if let Ok(mut v) = vis.get_mut(*e) {
                    *v = Visibility::Visible;
                }
            }
            None => {
                let h = meshes.add(mesh);
                let layers = match layer {
                    Layer::View => RenderLayers::layer(VIEW_LAYER),
                    Layer::World => RenderLayers::layer(0),
                };
                let e = commands.spawn((FxMesh, Mesh3d(h.clone()), MeshMaterial3d(mat), Transform::IDENTITY, NoFrustumCulling, layers)).id();
                fx.meshes.insert((mat_name, layer), (e, h));
            }
        }
    }

    for (e, _) in fx.ribbon_meshes.values() {
        if let Ok(mut v) = vis.get_mut(*e) {
            *v = Visibility::Hidden;
        }
    }
    for (name, verts) in ribbon_batches {
        if verts.is_empty() {
            continue;
        }
        let Some(mat) = ribbon_material(fx, &name, &mut images, &mut ribbon_assets) else { continue };
        if let Some(mut m) = ribbon_assets.get_mut(&mat) {
            m.parms.prog.w = now as f32 * 0.001;
        }
        let mesh = ribbon_mesh(&verts);
        match fx.ribbon_meshes.get(&name) {
            Some((e, h)) => {
                if let Some(mut m) = meshes.get_mut(h) {
                    *m = mesh;
                }
                if let Ok(mut v) = vis.get_mut(*e) {
                    *v = Visibility::Visible;
                }
            }
            None => {
                let h = meshes.add(mesh);
                let e = commands.spawn((FxMesh, Mesh3d(h.clone()), MeshMaterial3d(mat), Transform::IDENTITY, NoFrustumCulling, RenderLayers::layer(0))).id();
                fx.ribbon_meshes.insert(name, (e, h));
            }
        }
    }

    // Decals: fade in/out over their lifetime (r_decalLifetimeMultiplier 1), emissive over decalEmissiveLifetime.
    fx.decals.retain(|d| {
        let age = now - d.t0;
        if !d.persistent && age >= d.lifetime {
            commands.entity(d.entity).despawn();
            return false;
        }
        true
    });
    for d in &fx.decals {
        let age = (now - d.t0) as f32;
        let mut fade = 1.0f32;
        if d.fade_in > 0 {
            fade *= (age / d.fade_in as f32).clamp(0.0, 1.0);
        }
        if !d.persistent && d.fade_out > 0 {
            fade *= ((d.lifetime as f32 - age) / d.fade_out as f32).clamp(0.0, 1.0);
        }
        let blend = if d.emissive_life > 0 { (1.0 - age / d.emissive_life as f32).clamp(0.0, 1.0) } else { 0.0 };
        if let Some(mut m) = decal_mats.get_mut(&d.mat) {
            m.extension.params.tint.w = fade;
            m.extension.params.emissive = Vec4::new(d.emissive.x * blend, d.emissive.y * blend, d.emissive.z * blend, 0.0);
        }
    }

    // Shell casings (idEffectPhysicsPieceEmitter update 0x1417a4530 each game frame).
    let world = SimPieceWorld(&sim.world);
    let frame_ms = sim.msec_last;
    for se in fx.shells.iter_mut().flatten() {
        if se.emitter.in_use > 0 && frame_ms > 0 && prev != now {
            se.emitter.update(now, frame_ms, &world);
        }
        let surfaces = se.emitter.model.surfaces.len();
        while se.meshes.len() < surfaces {
            let h = meshes.add(placeholder_mesh());
            let mat = fx.vtm.as_mut().and_then(|v| v.material(&se.material, SHELL_TEXTURE_LEVEL, &mut images, &mut vt_mats)).unwrap_or_else(|| {
                crate::vtmat::flat(&mut vt_mats, StandardMaterial { base_color: Color::srgb(0.6, 0.15, 0.1), ..default() })
            });
            let e = commands.spawn((FxMesh, Mesh3d(h.clone()), MeshMaterial3d(mat), Transform::IDENTITY, NoFrustumCulling, RenderLayers::layer(0), Visibility::Hidden)).id();
            se.meshes.push((e, h));
        }
        for (si, (e, h)) in se.meshes.iter().enumerate() {
            let built = shell_mesh(&se.emitter, si);
            if let Ok(mut v) = vis.get_mut(*e) {
                *v = if built.is_some() { Visibility::Visible } else { Visibility::Hidden };
            }
            if let (Some(m), Some(mut dst)) = (built, meshes.get_mut(h)) {
                *dst = m;
            }
        }
    }

    // Lights.
    while fx.lights.len() < light_out.len() {
        let e = commands
            .spawn((FxLight, PointLight { intensity: 0.0, range: 250.0, shadow_maps_enabled: false, ..default() }, Transform::IDENTITY, RenderLayers::from_layers(&[0, VIEW_LAYER])))
            .id();
        fx.lights.push(e);
    }
    for (i, e) in fx.lights.iter().enumerate() {
        let Ok((mut pl, mut t, mut v)) = lights.get_mut(*e) else { continue };
        match light_out.get(i) {
            Some(l) => {
                let c = l.color;
                let k = l.intensity * l.fade;
                let peak = c.x.max(c.y).max(c.z).max(1e-6);
                pl.color = Color::linear_rgb(c.x / peak, c.y / peak, c.z / peak);
                pl.range = l.radius.x.max(l.radius.y).max(l.radius.z).max(1.0);
                pl.intensity = light_lumens(peak * k, pl.range);
                t.translation = to_bevy(l.origin);
                *v = Visibility::Visible;
            }
            None => *v = Visibility::Hidden,
        }
    }
}

const DECAL_SHADER: Handle<Shader> = uuid_handle!("8d2f4c61-0b7e-4a35-9c1d-5e6f7a8b9c0d");

/// Decal shading inputs (lighting.inc PROCESS_DECAL, per decal).
#[derive(Clone, Copy, Default, Debug, ShaderType)]
pub struct DecalParams {
    /// xyz decalDiffuseTint, w fade.
    tint: Vec4,
    /// emissive tint * power * emissive blending time.
    emissive: Vec4,
    /// decalOpacityPerChannel (albedo, specular, smoothness, normal).
    opacity: Vec4,
    spec_tint: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct DecalExt {
    #[uniform(100)]
    params: DecalParams,
    #[texture(101)]
    #[sampler(102)]
    albedo: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    normal: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    spec: Option<Handle<Image>>,
}

impl MaterialExtension for DecalExt {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(DECAL_SHADER)
    }
}

pub type DecalMaterial = ExtendedMaterial<StandardMaterial, DecalExt>;

/// Most decals alive at once. INTERIM: the renderer's own decal budget is not decoded; oldest go first.
const MAX_DECALS: usize = 256;
/// Overlay lift off the surface (units) against z-fighting; the engine shades decals inside the surface pass.
const DECAL_LIFT: f32 = 0.05;

/// Decal textures: `decaldiffusemap` / `decalbumpmap` / `decalspecularmap` -> generated/decalatlas/<path>.bimage.
fn decal_textures(fx: &mut Fx, name: &str, images: &mut Assets<Image>) -> Option<[Option<Handle<Image>>; 3]> {
    if let Some(t) = fx.decal_textures.get(name) {
        return t.clone();
    }
    let path = format!("generated/decls/material/{name}.decl");
    let text = fx.db.container().read_by_name(&path).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    let t = text.map(|text| {
        let mut out: [Option<Handle<Image>>; 3] = [None, None, None];
        for line in text.lines() {
            let l = line.trim();
            let Some((k, v)) = l.split_once(char::is_whitespace) else { continue };
            let slot = match k.to_ascii_lowercase().as_str() {
                "decaldiffusemap" => 0,
                "decalbumpmap" => 1,
                "decalspecularmap" => 2,
                _ => continue,
            };
            let src = v.trim().trim_matches('"').to_ascii_lowercase().replace('\\', "/");
            let stem = src.strip_suffix(".tga").unwrap_or(&src).to_string();
            out[slot] = load_image(fx.db.container(), &format!("generated/decalatlas/{stem}.bimage")).map(|mut img| {
                img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::ClampToEdge,
                    address_mode_v: ImageAddressMode::ClampToEdge,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    mipmap_filter: ImageFilterMode::Linear,
                    ..default()
                });
                images.add(img)
            });
        }
        out
    });
    fx.decal_textures.insert(name.to_string(), t.clone());
    t
}

/// Clips a convex polygon against the decal box (|s|<=hx, |t|<=hy, |r|<=hd around c).
fn clip_to_box(mut poly: Vec<G3>, c: G3, axes: [(G3, f32); 3]) -> Vec<G3> {
    for (ax, h) in axes {
        for sign in [1.0f32, -1.0] {
            let n = ax * sign;
            let d = |p: G3| h - n.dot(p - c);
            let mut out = Vec::with_capacity(poly.len() + 2);
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let (da, db) = (d(a), d(b));
                if da >= 0.0 {
                    out.push(a);
                }
                if (da >= 0.0) != (db >= 0.0) {
                    out.push(a + (b - a) * (da / (da - db)));
                }
            }
            poly = out;
            if poly.len() < 3 {
                return poly;
            }
        }
    }
    poly
}

/// Overlay mesh of the world polygons inside the decal box: uv = projection S/T, colour r = projection R
/// (box depth 0..1), g = angle weight saturate(dot(surface normal, R)), tangent = S.
fn decal_mesh(world: &rancher_sim::collision::World, c: G3, axes: [(G3, f32); 3]) -> Option<Mesh> {
    let (s, hx) = axes[0];
    let (t, hy) = axes[1];
    let (r, hd) = axes[2];
    let reach = hx.abs() + hy.abs() + hd.abs();
    let (lo, hi) = (c - G3::splat(reach), c + G3::splat(reach));
    let mut polys: Vec<(Vec<G3>, G3)> = Vec::new();
    for b in &world.brushes {
        if b.max.cmplt(lo).any() || b.min.cmpgt(hi).any() {
            continue;
        }
        for (n, lp) in &b.faces {
            polys.push((lp.iter().map(|&i| b.verts[i]).collect(), *n));
        }
    }
    for inst in &world.cms {
        if inst.max.cmplt(lo).any() || inst.min.cmpgt(hi).any() {
            continue;
        }
        let to_world = |p: [f32; 3]| inst.origin + inst.axis * G3::from_array(p);
        // Decal box bounds in model space for the submodel / polygon rejects.
        let inv = inst.axis.transpose();
        let corners = (0..8).map(|i| inv * (G3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }) - inst.origin));
        let (mlo, mhi) = corners.fold((G3::splat(f32::MAX), G3::splat(f32::MIN)), |(a, b), p| (a.min(p), b.max(p)));
        for sm in &inst.cm.submodels {
            let (blo, bhi) = (G3::from_array(sm.bounds[0]), G3::from_array(sm.bounds[1]));
            if bhi.cmplt(mlo).any() || blo.cmpgt(mhi).any() {
                continue;
            }
            for poly in &sm.polygons {
                let [plo, phi] = poly.bounds;
                let (plo, phi) = (G3::new(plo[0] as f32, plo[1] as f32, plo[2] as f32), G3::new(phi[0] as f32, phi[1] as f32, phi[2] as f32));
                if phi.cmplt(mlo).any() || plo.cmpgt(mhi).any() {
                    continue;
                }
                // INTERIM: render surfaces stand in as the solid collision polygons.
                if sm.surfaces.get(poly.surface as usize).is_none_or(|si| si.contents & idres::bcm::contents::SOLID == 0) {
                    continue;
                }
                let pl = sm.polygon_plane(poly);
                let n = inst.axis * G3::new(pl[0], pl[1], pl[2]);
                polys.push((sm.polygon_verts(poly).map(|v| to_world(sm.verts[v])).collect(), n));
            }
        }
    }
    let (mut pos, mut nrm, mut tan, mut uv, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (poly, n) in polys {
        let aw = n.dot(r);
        if aw <= 0.0 {
            continue;
        }
        let clipped = clip_to_box(poly, c, axes);
        if clipped.len() < 3 {
            continue;
        }
        let base = pos.len() as u32;
        for p in &clipped {
            let d = *p - c;
            pos.push(to_bevy(*p + n * DECAL_LIFT).to_array());
            nrm.push(to_bevy(n).to_array());
            let sb = to_bevy(s);
            tan.push([sb.x, sb.y, sb.z, 1.0]);
            uv.push([d.dot(s) / (2.0 * hx) + 0.5, d.dot(t) / (2.0 * hy) + 0.5]);
            col.push([d.dot(r) / (2.0 * hd) + 0.5, aw.min(1.0), 0.0, 1.0]);
        }
        for k in 1..clipped.len() as u32 - 1 {
            // Loops are counter-clockwise seen along +n (idTech); Bevy's front faces are CCW in its axes.
            idx.extend_from_slice(&[base, base + k, base + k + 1]);
        }
    }
    if idx.is_empty() {
        return None;
    }
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
            .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, tan)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
            .with_inserted_indices(Indices::U32(idx)),
    )
}

/// ImpactEffect's decal (0x140ee70e0 -> render decal 0x1415f3e10): a box decalSize x decalSize x decalDepth
/// around the hit, R along the surface normal, S/T rotated by decalAngle (0 = random). INTERIM: the random
/// angle's source and the render side's packing (tint bytes, opacity scales from the material, threshold
/// blending, emissive power encoding) are not decoded; see FX.md.
#[allow(clippy::too_many_arguments)]
fn spawn_decal(
    fx: &mut Fx,
    commands: &mut Commands,
    world: &rancher_sim::collision::World,
    name: &str,
    e: &idfx::impact::Effect,
    at: G3,
    normal: G3,
    now: i32,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    decal_mats: &mut Assets<DecalMaterial>,
) {
    let Some(tex) = decal_textures(fx, name, images) else { return };
    let r = idfx::normalize(normal);
    let base = idfx::impact::normal_axis(r);
    let ang = if e.decal_angle != 0.0 { e.decal_angle.to_radians() } else { fx.world_rng.random_float() * idfx::TWO_PI };
    let (sn, cs) = ang.sin_cos();
    let s = base.x * cs + base.y * sn;
    let t = r.cross(s);
    let axes = [(s, e.decal_size * 0.5), (t, e.decal_size * 0.5), (r, e.decal_depth * 0.5)];
    let Some(mesh) = decal_mesh(world, at, axes) else { return };
    let params = DecalParams {
        tint: Vec4::new(e.decal_diffuse_tint.x, e.decal_diffuse_tint.y, e.decal_diffuse_tint.z, 1.0),
        emissive: Vec4::ZERO,
        opacity: Vec4::from_array(e.decal_opacity.to_array()),
        spec_tint: Vec4::from_array(e.decal_specular_tint.to_array()),
    };
    let [albedo, normal_tex, spec] = tex;
    let mat = decal_mats.add(DecalMaterial {
        base: StandardMaterial { alpha_mode: AlphaMode::Blend, depth_bias: 8.0, cull_mode: None, ..default() },
        extension: DecalExt { params, albedo, normal: normal_tex, spec },
    });
    let entity = commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(mat.clone()), Transform::IDENTITY, RenderLayers::layer(0))).id();
    // decalEmmissiveTint xyz colour, w power (shader: tint * w * 16 from a byte; INTERIM: power used directly).
    let em = e.decal_emissive_tint;
    fx.decals.push(LiveDecal {
        entity,
        mat,
        t0: now,
        lifetime: e.decal_lifetime,
        fade_in: e.decal_fade_in,
        fade_out: e.decal_fade_out,
        emissive_life: e.decal_emissive_lifetime,
        persistent: e.persistent,
        emissive: Vec4::new(em.x * em.w, em.y * em.w, em.z * em.w, 0.0),
    });
    if fx.decals.len() > MAX_DECALS {
        let d = fx.decals.remove(0);
        commands.entity(d.entity).despawn();
    }
}

const DECAL_SHADER_SRC: &str = r"
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::calculate_tbn_mikktspace,
}
#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#endif

struct DecalParams {
    tint: vec4<f32>,
    emissive: vec4<f32>,
    opacity: vec4<f32>,
    spec_tint: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> decal: DecalParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var decal_albedo: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var decal_albedo_smp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var decal_normal: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var decal_normal_smp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var decal_spec: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var decal_spec_smp: sampler;

// lighting.inc PROCESS_DECAL, applied as a lit overlay: the normal map's alpha is the mask, albedo and
// specular get the 'cheap degamma' (x*x), the box depth fades the mask (z_soft_scale 1), the angle weight
// and per-channel opacities scale the blend, emissive = albedo * tint * mask * blending time.
@fragment
fn fragment(vertex_output: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var in = vertex_output;
    var pbr_input = pbr_input_from_standard_material(in, is_front);
#ifdef VERTEX_UVS_A
    let uv = in.uv;
#else
    let uv = vec2<f32>(0.5, 0.5);
#endif
    let nm = textureSample(decal_normal, decal_normal_smp, uv);
    var alpha = nm.a;
    if alpha <= 4.0 / 255.0 {
        discard;
    }
#ifdef VERTEX_COLORS
    let z = in.color.r;
    let angle_w = in.color.g;
#else
    let z = 0.5;
    let angle_w = 1.0;
#endif
    alpha = alpha * saturate(1.0 - abs(z * 2.0 - 1.0));
    var a = textureSample(decal_albedo, decal_albedo_smp, uv);
    a = a * a;
    let albedo = a.rgb * decal.tint.xyz;
    let emissive = angle_w * albedo * decal.emissive.xyz * a.a;
    var sp = textureSample(decal_spec, decal_spec_smp, uv);
    sp = sp * sp;
    let op = decal.opacity * (alpha * angle_w * decal.tint.w);
    pbr_input.material.base_color = vec4<f32>(albedo, saturate(op.x));
    pbr_input.material.metallic = 0.0;
    pbr_input.material.reflectance = sqrt(sp.rgb * decal.spec_tint.xyz / 0.16);
    let rough = 1.0 - 0.8 * sp.a;
    pbr_input.material.perceptual_roughness = rough * rough;
    pbr_input.material.emissive = vec4<f32>(emissive, 1.0);
#ifdef VERTEX_TANGENTS
    let nt = nm.xyz * 2.0 - 1.0;
    let tbn = calculate_tbn_mikktspace(pbr_input.world_normal, in.world_tangent);
    let dn = normalize(nt.x * tbn[0] + nt.y * tbn[1] + nt.z * tbn[2]);
    pbr_input.N = normalize(mix(pbr_input.N, dn, saturate(op.w)));
#endif
#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif
    return out;
}
";

/// r_particleMaxParticleLights default (registration 0x1402161ef).
const PARTICLE_LIGHTS_MAX: usize = 256;

/// VT level the shell material streams from (like the view models, crate::viewanim).
const SHELL_TEXTURE_LEVEL: usize = 1;

/// Point traces for simplePointCollision pieces against the sim's collision world. INTERIM:
/// rancher_sim's trace-model sweep needs polygons, so this uses `World::ray` and backs the end off the
/// surface by CLIP_EPSILON like the collision model's translation (fraction = (d1 - eps) / (d1 - d2)).
struct SimPieceWorld<'a>(&'a rancher_sim::collision::World);

impl idfx::pieces::PieceWorld for SimPieceWorld<'_> {
    fn trace_point(&self, start: G3, end: G3, _clip_mask: u32) -> idfx::pieces::PieceTrace {
        let d = end - start;
        let len = d.length();
        let miss = idfx::pieces::PieceTrace { fraction: 1.0, endpos: end, point: end, normal: G3::Z, surface: 0 };
        if len <= 0.0 {
            return miss;
        }
        let dir = d / len;
        let s = rancher_sim::Vec3::from_array(start.to_array());
        let Some((t, n, _)) = self.0.ray(s, rancher_sim::Vec3::from_array(dir.to_array()), len) else { return miss };
        let n = G3::from_array(n.to_array());
        let ndir = n.dot(dir);
        if ndir >= 0.0 {
            return miss;
        }
        let d1 = -t * ndir;
        let fraction = ((d1 - rancher_sim::collision::CLIP_EPSILON) / (len * -ndir)).clamp(0.0, 1.0);
        idfx::pieces::PieceTrace { fraction, endpos: start + d * fraction, point: start + dir * t, normal: n, surface: 0 }
    }
}

/// A one-vertex degenerate triangle with `shell_mesh`'s attributes: Bevy's mesh allocator mishandles meshes
/// with an empty vertex buffer (copy of an unallocated slab key), so placeholders are never empty.
fn placeholder_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0f32, 0.0, 0.0, 1.0]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]])
        .with_inserted_indices(Indices::U32(vec![0, 0, 0]))
}

/// The live pieces of one render surface, skinned by their rigid-body poses (None when none is live).
fn shell_mesh(e: &idfx::pieces::PieceEmitter, si: usize) -> Option<Mesh> {
    let s = e.model.surfaces.get(si)?;
    if !e.pieces.iter().any(|p| p.body.active) {
        return None;
    }
    let (mut pos, mut nrm, mut tan, mut uv) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut remap = vec![u32::MAX; s.verts.len()];
    for (vi, v) in s.verts.iter().enumerate() {
        let pi = v.piece();
        let Some(p) = e.pieces.get(pi) else { continue };
        if !p.body.active {
            continue;
        }
        remap[vi] = pos.len() as u32;
        let r = &p.rest;
        let local_dir = |d: G3| r.rows[0] * d.x + r.rows[1] * d.y + r.rows[2] * d.z;
        let n = p.body.orientation.to_parent(local_dir(v.normal_f32())).normalize_or_zero();
        let t = p.body.orientation.to_parent(local_dir(v.tangent_f32())).normalize_or_zero();
        pos.push(to_bevy(e.piece_point(pi, v.xyz)).to_array());
        nrm.push(to_bevy(n).to_array());
        let tb = to_bevy(t);
        tan.push([tb.x, tb.y, tb.z, if v.tangent[3] >= 128 { -1.0 } else { 1.0 }]);
        uv.push(v.st.to_array());
    }
    let mut idx = Vec::with_capacity(s.indexes.len());
    for tri in s.indexes.chunks_exact(3) {
        let m = [remap[tri[0] as usize], remap[tri[1] as usize], remap[tri[2] as usize]];
        if m.iter().all(|&i| i != u32::MAX) {
            // idTech winding (clockwise front faces) to Bevy's counter-clockwise.
            idx.extend_from_slice(&[m[0], m[2], m[1]]);
        }
    }
    if idx.is_empty() {
        return None;
    }
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
            .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, tan)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_indices(Indices::U32(idx)),
    )
}

#[derive(Component)]
struct FxMesh;

#[derive(Component)]
struct FxLight;

fn build_mesh(quads: &[Quad]) -> Mesh {
    let n = quads.len() * 4;
    let mut pos = Vec::with_capacity(n);
    let mut uv = Vec::with_capacity(n);
    let mut uvn = Vec::with_capacity(n);
    let mut col = Vec::with_capacity(n);
    let mut gp = Vec::with_capacity(n);
    let mut misc = Vec::with_capacity(n);
    let mut idx = Vec::with_capacity(quads.len() * 6);
    for (qi, q) in quads.iter().enumerate() {
        for v in &q.verts {
            pos.push(to_bevy(v.pos).to_array());
            uv.push(v.uv);
            uvn.push(v.uv_next);
            col.push(v.color_f.unwrap_or(v.color.map(|b| b as f32 / 255.0)));
            gp.push(v.generic.map(|b| b as f32 / 255.0));
            misc.push([v.frame_frac, v.alpha_scale]);
        }
        let b = (qi * 4) as u32;
        idx.extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 1, b + 3]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_attribute(ATTRIBUTE_UV_NEXT, uvn)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_attribute(ATTRIBUTE_GENERIC, gp)
        .with_inserted_attribute(ATTRIBUTE_MISC, misc)
        .with_inserted_indices(Indices::U32(idx))
}

fn ribbon_decl(fx: &mut Fx, name: &str) -> Option<Arc<RibbonDecl>> {
    if let Some(d) = fx.ribbon_decls.get(name) {
        return d.clone();
    }
    let d = match RibbonDecl::load(&fx.db, name) {
        Ok(d) => Some(Arc::new(d)),
        Err(e) => {
            eprintln!("fx: {e:#}");
            None
        }
    };
    fx.ribbon_decls.insert(name.to_string(), d.clone());
    d
}

const RIBBON_SHADER: Handle<Shader> = uuid_handle!("5b9e2d17-3c4a-4f60-8e21-9d7c6b5a4f30");

/// Ribbon stage programs: x program (1 ribbonblend, 2 ca_ribbonelectricalarc, 3 ribbontracersmoke), y $factor,
/// z $emissive.x, w $time (s); $color, $color2, $color3, $tiling, $tiling2; tracer (x $tracerSize, y
/// $tracerSpeed, z $bloomThreshold, w $particleMult); misc (x $particleFade).
#[derive(Clone, Copy, Default, ShaderType)]
pub struct RibbonParms {
    prog: Vec4,
    color: Vec4,
    color2: Vec4,
    color3: Vec4,
    tiling: Vec4,
    tiling2: Vec4,
    tracer: Vec4,
    misc: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct RibbonMaterial {
    /// $transMap
    #[texture(0)]
    #[sampler(1)]
    map: Handle<Image>,
    /// $ribbonMask (ribbonblend) / $transMap1
    #[texture(2)]
    #[sampler(3)]
    map1: Handle<Image>,
    /// $transMap2
    #[texture(4)]
    #[sampler(5)]
    map2: Handle<Image>,
    #[uniform(6)]
    parms: RibbonParms,
}

impl Material for RibbonMaterial {
    fn vertex_shader() -> ShaderRef {
        RIBBON_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        RIBBON_SHADER.into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, _: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vertex = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(1),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(2),
            Mesh::ATTRIBUTE_TANGENT.at_shader_location(3),
        ])?;
        descriptor.vertex.buffers = vec![vertex];
        // Two-sided: view-oriented ribbons always face the eye; ca_ribbonelectricalarc is `twosided`.
        descriptor.primitive.cull_mode = None;
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.first_mut() {
                // Every program's blend is expressed as premultiplied ONE, ONE_MINUS_SRC_ALPHA output (additive
                // programs write alpha 0).
                let c = BlendComponent { src_factor: BlendFactor::One, dst_factor: BlendFactor::OneMinusSrcAlpha, operation: BlendOperation::Add };
                target.blend = Some(BlendState { color: c, alpha: c });
            }
        }
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Material decl image key -> `generated/image` resource (`.tga` dropped unless the name carries `$` options).
fn decl_image_path(v: &str) -> String {
    let src = v.trim().trim_matches('"').to_ascii_lowercase();
    let stem = if src.contains('$') { src.as_str() } else { src.strip_suffix(".tga").unwrap_or(&src) };
    format!("generated/image/{stem}.bimage")
}

fn decl_vec4(v: &str) -> Option<Vec4> {
    let f: Vec<f32> = v.trim().trim_matches(|c| c == '{' || c == '}' || c == ' ').split(',').filter_map(|x| x.trim().parse().ok()).collect();
    match f.len() {
        1 => Some(Vec4::splat(f[0])),
        4 => Some(Vec4::new(f[0], f[1], f[2], f[3])),
        _ => None,
    }
}

/// A ribbon material from its decl: stage program, maps and parms over the renderparm defaults (tiling /
/// tiling2 (1,1,0,0), colours 1, factor 1, emissive 0, tracerSize 1, tracerSpeed 30, bloomThreshold 1 (0 in
/// ribbontracersmoke's parms), particleFade 0.009, ribbonMask textures/effects/misc/tracer_mask, maps black).
fn ribbon_material(fx: &mut Fx, name: &str, images: &mut Assets<Image>, assets: &mut Assets<RibbonMaterial>) -> Option<Handle<RibbonMaterial>> {
    if let Some(h) = fx.ribbon_mats.get(name) {
        return h.clone();
    }
    let path = format!("generated/decls/material/{name}.decl");
    let text = fx.db.container().read_by_name(&path).ok().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut prog = String::new();
    let mut maps: [Option<String>; 3] = [None, None, None];
    let mut mask = None;
    let mut parms = RibbonParms {
        prog: Vec4::new(0.0, 1.0, 0.0, 0.0),
        color: Vec4::ONE,
        color2: Vec4::ONE,
        color3: Vec4::ONE,
        tiling: Vec4::new(1.0, 1.0, 0.0, 0.0),
        tiling2: Vec4::new(1.0, 1.0, 0.0, 0.0),
        tracer: Vec4::new(1.0, 30.0, 1.0, fx.env.particle_mult),
        misc: Vec4::new(0.009, 0.0, 0.0, 0.0),
    };
    for line in text.lines() {
        let Some((k, v)) = line.trim().split_once(char::is_whitespace) else { continue };
        let v4 = decl_vec4(v);
        match k.to_ascii_lowercase().as_str() {
            "stageprogram" => prog = v.trim().to_ascii_lowercase(),
            "transmap" => maps[0] = Some(decl_image_path(v)),
            "transmap1" => maps[1] = Some(decl_image_path(v)),
            "transmap2" => maps[2] = Some(decl_image_path(v)),
            "ribbonmask" => mask = Some(decl_image_path(v)),
            "color" => parms.color = v4.unwrap_or(parms.color),
            "color2" => parms.color2 = v4.unwrap_or(parms.color2),
            "color3" => parms.color3 = v4.unwrap_or(parms.color3),
            "tiling" => parms.tiling = v4.unwrap_or(parms.tiling),
            "tiling2" => parms.tiling2 = v4.unwrap_or(parms.tiling2),
            "factor" => parms.prog.y = v4.map_or(parms.prog.y, |f| f.x),
            "emissive" => parms.prog.z = v4.map_or(parms.prog.z, |f| f.x),
            "tracersize" => parms.tracer.x = v4.map_or(parms.tracer.x, |f| f.x),
            "tracerspeed" => parms.tracer.y = v4.map_or(parms.tracer.y, |f| f.x),
            _ => {}
        }
    }
    parms.prog.x = match prog.as_str() {
        "ribbonblend" => 1.0,
        // addprogram ribbonelectricalarcglare adds max(0, colour - 1), which is 0 for unorm vertex colours and
        // colour2 <= 1: not drawn.
        "ca_ribbonelectricalarc" => 2.0,
        "ribbontracersmoke" => {
            parms.tracer.z = 0.0;
            3.0
        }
        _ => {
            eprintln!("fx: ribbon material {name}: stage program {prog:?} not reproduced");
            fx.ribbon_mats.insert(name.to_string(), None);
            return None;
        }
    };
    let c = fx.db.container_arc();
    let black = fx
        .black
        .get_or_insert_with(|| {
            let img = load_image(&c, "generated/image/textures/system/constant_color/black_noalpha.bimage")
                .unwrap_or_else(|| Image::new(Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, vec![0, 0, 0, 255], TextureFormat::Rgba8Unorm, RenderAssetUsages::RENDER_WORLD));
            images.add(img)
        })
        .clone();
    let mut tex = |path: Option<&str>, repeat: bool| -> Handle<Image> {
        let Some(mut img) = path.and_then(|p| load_image(&c, p)) else {
            if let Some(p) = path {
                eprintln!("fx: ribbon material {name}: no image {p}");
            }
            return black.clone();
        };
        if repeat {
            if let ImageSampler::Descriptor(d) = &mut img.sampler {
                d.address_mode_u = ImageAddressMode::Repeat;
                d.address_mode_v = ImageAddressMode::Repeat;
            }
        }
        images.add(img)
    };
    // ribbonblend samples $ribbonMask in the transMap1 slot.
    let mask_path = if parms.prog.x == 1.0 { Some(mask.unwrap_or_else(|| decl_image_path("textures/effects/misc/tracer_mask"))) } else { maps[1].clone() };
    let map = tex(maps[0].as_deref(), true);
    let map1 = tex(mask_path.as_deref(), false);
    let map2 = tex(maps[2].as_deref(), true);
    let h = assets.add(RibbonMaterial { map, map1, map2, parms });
    fx.ribbon_mats.insert(name.to_string(), Some(h.clone()));
    Some(h)
}

/// Ribbon quads (start -, start +, end -, end +) as triangles; vertex colour / tangent are the engine's unorm bytes.
fn ribbon_mesh(verts: &[RibbonVertex]) -> Mesh {
    let mut pos = Vec::with_capacity(verts.len());
    let mut uv = Vec::with_capacity(verts.len());
    let mut col = Vec::with_capacity(verts.len());
    let mut tan = Vec::with_capacity(verts.len());
    for v in verts {
        pos.push(to_bevy(v.pos).to_array());
        uv.push(v.st);
        col.push(v.color.map(|c| c as f32 / 255.0));
        tan.push([v.tangent[0] as f32 / 255.0, v.tangent[1] as f32 / 255.0, v.tangent[2] as f32 / 255.0, 0.0]);
    }
    let mut idx = Vec::with_capacity(verts.len() / 4 * 6);
    for q in 0..(verts.len() / 4) as u32 {
        let b = q * 4;
        idx.extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 1, b + 3]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, tan)
        .with_inserted_indices(Indices::U32(idx))
}

// Renderprogs ribbonblend, ca_ribbonelectricalarc and ribbontracersmoke (decls/renderprog/*.decl). No fog.
const RIBBON_SHADER_SRC: &str = r"
#import bevy_pbr::mesh_functions
#import bevy_pbr::view_transformations::position_world_to_clip

struct RibbonParms {
    prog: vec4<f32>,
    color: vec4<f32>,
    color2: vec4<f32>,
    color3: vec4<f32>,
    tiling: vec4<f32>,
    tiling2: vec4<f32>,
    tracer: vec4<f32>,
    misc: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var map0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var samp0: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var map1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var samp1: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var map2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var samp2: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var<uniform> rp: RibbonParms;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) tangent: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tc1: vec4<f32>,
    @location(2) tc2: vec4<f32>,
    @location(3) color: vec4<f32>,
};

// global.inc SRGBlinear (DeGamma).
fn srgb_linear(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(v.position, 1.0));
    out.clip = position_world_to_clip(world.xyz);
    // Fade ribbon based on distance to near clip.
    let fade = clamp(abs(out.clip.w) * rp.misc.x, 0.0, 1.0);
    out.uv = v.uv;
    out.tc2 = vec4<f32>(0.0);
    if (rp.prog.x > 1.5 && rp.prog.x < 2.5) {
        // ca_ribbonelectricalarc: flip-book frames of $transMap ($tiling.xy cells, $tiling.z frames/s) with
        // $tiling2 scrolling, cross-faded; per-ribbon random time offset from tangent.z.
        let t = rp.tiling;
        let t2 = rp.tiling2;
        let my_time = rp.prog.w * (v.tangent.z * 0.3 + 1.0) + v.tangent.z * 73.0;
        var fx = fract(floor(my_time * t.z) / t.x);
        var fy = fract(floor((my_time * t.z) / t.x) / t.y);
        out.tc1.x = (v.uv.x * t2.x + my_time * t2.z) * (1.0 / t.x) + fx;
        out.tc1.y = (v.uv.y * t2.y + my_time * t2.w) * (1.0 / t.y) + fy;
        fx = fract(floor(my_time * t.z + 1.0) / t.x);
        fy = fract(floor((my_time * t.z + 1.0) / t.x) / t.y);
        out.tc1.z = (v.uv.x * t2.x + my_time * t2.z) * (1.0 / t.x) + fx;
        out.tc1.w = (v.uv.y * t2.y + my_time * t2.w) * (1.0 / t.y) + fy;
        out.tc2 = vec4<f32>(fract(my_time * t.z), v.tangent.x, 0.0, 0.0);
        out.color = v.color * rp.color;
        out.color.w = out.color.w * fade;
        return out;
    }
    out.tc1 = vec4<f32>(v.tangent.xyz, 0.0);
    out.color = vec4<f32>(srgb_linear(v.color.x), srgb_linear(v.color.y), srgb_linear(v.color.z), srgb_linear(v.color.w));
    if (rp.prog.x < 1.5) {
        // ribbonblend
        out.color = out.color * fade * rp.color;
    } else {
        // ribbontracersmoke
        out.color.w = out.color.w * fade;
    }
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    if (rp.prog.x < 1.5) {
        // ribbonblend: blend ONE, ONE_MINUS_SRC_ALPHA after PREMULTIPLY_ALPHA_BLENDED.
        let my_time = rp.prog.w + in.tc1.z * 73.0;
        var f = textureSample(map0, samp0, in.uv * rp.tiling.xy + vec2<f32>(my_time) * rp.tiling.zw);
        f = f * in.color;
        f = f * rp.color2 * rp.prog.y;
        f = f * textureSample(map1, samp1, in.tc1.xy);
        let a = clamp(f.w, 0.0, 1.0);
        return vec4<f32>(f.xyz * a, a);
    }
    if (rp.prog.x < 2.5) {
        // ca_ribbonelectricalarc: blend SRC_ALPHA, ONE ($transAtlasScaleBias at its default (1, 1, 0, 0)).
        let ta = textureSampleLevel(map0, samp0, fract(in.tc1.xy), 0.0);
        let tb = textureSampleLevel(map0, samp0, fract(in.tc1.zw), 0.0);
        let mask = textureSample(map1, samp1, vec2<f32>(in.tc2.y, 0.5));
        var f = mix(ta, tb, vec4<f32>(in.tc2.x));
        f = f * in.color * (rp.color2 * rp.prog.z);
        f = vec4<f32>(f.xyz * rp.color2.w, f.w);
        f = f * mask;
        return vec4<f32>(f.xyz * f.w, 0.0);
    }
    // ribbontracersmoke: blend SRC_ALPHA, ONE_MINUS_SRC_ALPHA.
    let my_time = rp.prog.w + in.tc1.z * 73.0;
    var tc = in.uv;
    tc.x = (tc.x * rp.tracer.x) - (1.0 - in.color.w - 0.1) * rp.tracer.y;
    tc.x = clamp(1.0 - tc.x, 0.0, 1.0);
    tc.x = tc.x * 0.9999 + 0.0001;
    let tracer = textureSampleLevel(map0, samp0, tc, 0.0);
    let mask = textureSample(map1, samp1, in.tc1.xy);
    let smoke = textureSample(map2, samp2, in.uv * rp.tiling.xy + vec2<f32>(my_time) * rp.tiling.zw);
    var f = smoke * rp.color3;
    f.w = f.w * tracer.w;
    f = f * in.color;
    f = f + vec4<f32>(tracer.x) * rp.color2 * rp.prog.y;
    f = f * mask;
    let rgb = max(f.xyz - vec3<f32>(rp.tracer.z), vec3<f32>(0.0)) * rp.tracer.w;
    let a = clamp(f.w, 0.0, 1.0);
    return vec4<f32>(rgb * a, a);
}
";

const SHADER_SRC: &str = r"
#import bevy_pbr::mesh_functions
#import bevy_pbr::view_transformations::position_world_to_clip
#import bevy_pbr::mesh_view_bindings::view
#ifdef DEPTH_PREPASS
#import bevy_pbr::prepass_utils
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var fx_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var fx_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var<uniform> fx_parms: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var<uniform> fx_tint: vec4<f32>;

// r_znear (cvar default 3): the engine's near plane, for the soft-particle depth difference and the billboard push.
const R_ZNEAR: f32 = 3.0;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) uv_next: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) generic: vec4<f32>,
    @location(5) misc: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) uv_next: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) generic: vec4<f32>,
    @location(4) frame_frac: f32,
    @location(5) alpha_scale: f32,
};

// global.inc SRGBlinear (exact) and SRGBlinearApprox.
fn srgb_linear(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn srgb_linear_approx(v: f32) -> f32 {
    return v * (v * (v * 0.305306011 + 0.682171111) + 0.012522878);
}

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(v.position, 1.0));
    out.clip = position_world_to_clip(world.xyz);
    let eye_dist = out.clip.w;
    let alpha_scale = v.misc.y;
    // Fade by distance to the eye over the fake thickness (1 - alphascale) * 127.
    let thickness = (1.0 - alpha_scale) * 127.0;
    var particle_fade = 1.0;
    if (thickness > 0.0) {
        particle_fade = clamp(abs(eye_dist) / thickness, 0.0, 1.0);
    }
    out.uv = v.uv;
    out.uv_next = v.uv_next;
    out.frame_frac = v.misc.x;
    out.alpha_scale = alpha_scale;
    out.generic = vec4<f32>(v.generic.x * 10.0 + 1.0, v.generic.y * 10.0 + 1.0, v.generic.z * 10.0 + 1.0, v.generic.w);
    if (fx_parms.w > 2.5) {
        // gpuparticlerender: result.color is the linear lerped colour, no culling or near fade.
        out.color = v.color;
        return out;
    }
    if (fx_parms.w > 0.5 && fx_parms.w < 1.5) {
        // basicBlend vertex colour: no DeGamma (vertex.color.w = 1 -> overbright 1).
        out.color = v.color;
        return out;
    }
    // Billboard depth push: position.z -= ((1 - alphascale) * 127 / eyeDist) * texcoord2.z (pushes the billboard
    // forward towards the player view, per its comment). Read with the engine's clip z = z - znear (non-reversed, infinite), that is
    // a view depth z * znear / (znear + push), i.e. Bevy's reversed depth near / z scaled by 1 + push / znear.
    // INTERIM: the engine's clip-z convention is inferred from the renderprog comment.
    if (eye_dist > 0.0) {
        let push = thickness / eye_dist * out.generic.z;
        out.clip.z = out.clip.z * (1.0 + push / R_ZNEAR);
    }
    out.color = vec4<f32>(srgb_linear(v.color.x), srgb_linear(v.color.y), srgb_linear(v.color.z), srgb_linear(v.color.w));
    out.color.w = out.color.w * particle_fade;
    // PARTICLE_ALPHA_CULLING
    let opacity_min = 1.0 / 255.0;
    if (out.color.w < opacity_min) {
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    out.color.w = (out.color.w - opacity_min) * (1.0 / (1.0 - opacity_min));
    return out;
}

// particle.inc COMPUTE_SOFT_PARTICLE_SCALE: saturate((1 / (1 - depth) - 1 / (1 - interpolatedDepth)) * softness),
// softness = the vertex alpha scale. With the infinite projection that is (z_scene - z_frag) / znear: Bevy's reversed
// infinite depth is near / z, so 1 / depth * near gives view z here, divided by the engine's r_znear (3).
// INTERIM: the engine's projection is taken as infinite far with r_znear; the $gunParticle lerp (x500) is not applied
// because the view-layer camera has no depth prepass (those particles keep scale 1, as without a depth map).
fn soft_particle_scale(frag: vec4<f32>, softness: f32) -> f32 {
#ifdef DEPTH_PREPASS
    let near = view.clip_from_view[3][2];
    let scene = prepass_utils::prepass_depth(frag, 0u);
    if (scene <= 0.0 || frag.z <= 0.0) { return 1.0; }
    return clamp((near / scene - near / frag.z) / R_ZNEAR * softness, 0.0, 1.0);
#else
    return 1.0;
#endif
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    if (fx_parms.w > 2.5) {
        // gpuparticle/add|blend: COMPUTE_ANIM_CROSSFADE_COLOR_SAMPLER (no DeGamma), a = g, rgb = r (* $factor for
        // add). Soft-particle alphaScale and $coverage are 1 here.
        let c = textureSample(fx_texture, fx_sampler, in.uv) * (1.0 - in.frame_frac) + textureSample(fx_texture, fx_sampler, in.uv_next) * in.frame_frac;
        if (fx_parms.w < 3.5) {
            // PREMULTIPLY_ALPHA_ADDITIVE: alpha 0 -> ONE / ONE_MINUS_SRC_ALPHA adds.
            return vec4<f32>(vec3<f32>(c.x * fx_parms.y) * in.color.xyz, 0.0);
        }
        // PREMULTIPLY_ALPHA_BLENDED
        let a = clamp(c.y * in.color.w, 0.0, 1.0);
        return vec4<f32>(vec3<f32>(c.x) * in.color.xyz * a, a);
    }
    if (fx_parms.w > 1.5) { return vec4<f32>(1.0, 0.0, 1.0, 1.0); }
    if (fx_parms.w > 0.5) {
        // basicadd: tex2DDeGammaApprox(transMap) * colour * factor, blended ONE ONE (alpha 0 here).
        let t = textureSample(fx_texture, fx_sampler, in.uv);
        let c = vec3<f32>(srgb_linear_approx(t.x), srgb_linear_approx(t.y), srgb_linear_approx(t.z)) * in.color.xyz * fx_tint.xyz * fx_parms.y;
        return vec4<f32>(c, 0.0);
    }
    let t0 = textureSample(fx_texture, fx_sampler, in.uv);
    let t1 = textureSample(fx_texture, fx_sampler, in.uv_next);
    let r = srgb_linear_approx(t0.x) * (1.0 - in.frame_frac) + srgb_linear_approx(t1.x) * in.frame_frac;
    let g = srgb_linear_approx(t0.y) * (1.0 - in.frame_frac) + srgb_linear_approx(t1.y) * in.frame_frac;
    var a = g;
    var rgb = vec3<f32>(r);
    if (a - 1e-6 < 0.0) { discard; }
    rgb = rgb * in.generic.x;
    a = a * soft_particle_scale(in.clip, in.alpha_scale);
    rgb = rgb * in.color.xyz;
    a = mix(a - (1.0 - pow(max(in.color.w, 0.0), 0.5)), a * in.color.w, in.generic.w);
    a = a * in.generic.y;
    rgb = mix(rgb * fx_parms.y, rgb * fx_parms.z, 1.0 - fx_parms.x);
    a = clamp(a, 0.0, 1.0);
    return vec4<f32>(rgb * a, a);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    fn info(tracers: i32, random: bool, random_type: i32) -> TracerInfo {
        TracerInfo { material: None, ribbons: Vec::new(), tracers, random, random_type, speed: 2500.0, length: 48.0, height: 4.0, lifetime: -1, fade_out: 0 }
    }

    #[test]
    fn tracer_gate() {
        let mut rng = idfx::IdRandom(0);
        // Ordered cadence: round % tracers == 0, no draw.
        let ordered: Vec<bool> = (0..6).map(|r| tracer_shot(&info(3, false, 0), r, &mut rng)).collect();
        assert_eq!(ordered, [true, false, false, true, false, false]);
        assert_eq!(rng.0, 0);
        // ONEPERGROUP (SP chaingun): every shot, no draw; tracers 0 never.
        assert!(tracer_shot(&info(5, true, 1), 7, &mut rng) && rng.0 == 0);
        assert!(!tracer_shot(&info(0, true, 1), 0, &mut rng));
        // ASGROUP: one LCG draw per shot, ((seed >> 10) & 0x7fff) % tracers == 0.
        let mut expect = idfx::IdRandom(0);
        for r in 0..20 {
            let v = expect.next_int() as i32;
            assert_eq!(tracer_shot(&info(5, true, 0), r, &mut rng), v % 5 == 0);
        }
    }
}
