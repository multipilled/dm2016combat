//! Bevy materials from DOOM's virtual texture, with the texels and sampling the game uses.
//!
//! Each material's VT levels are re-encoded exactly like the engine's transcode job (idres::vtex:
//! BC3 for the three page layers, the stored BC7 colour mask) and packed into one single-mip atlas,
//! like the engine's physical page cache. `vtmat.wgsl` picks the level per pixel like SAMPLE_VMTR
//! (anisotropic LOD from the virtual-coordinate derivatives, the weapons' USE_LOD_HACK bias,
//! 0.25-band trilinear between two levels, frac() wrapping into the material's rect) and decodes the
//! surface like lighting.inc (albedo/specular from the sRGB array, smoothness = DeGamma(power),
//! tangent normal from the two alpha channels). `material_masked` gives the alpha-tested variant
//! (megatrans surfaces: discard where the cover < 0.5, also in depth and shadow passes).
//! Shading is the engine's (lighting.rs / lighting.wgsl), not Bevy's PBR.
//!
//! Loading is in the background: `material` returns at once with a flat grey material; a worker
//! thread builds the coarse levels first, then the requested ones, and `VtMaterialPlugin` swaps
//! the textures in, using `STREAM_THREADS` threads. Transcoded pages are cached on disk under
//! `gamedata/cache` (git-ignored; only data derived from the user's install; `dm2016combat_CACHE`
//! overrides, `dm2016combat_CACHE=off` disables).

/// The engine's shading on the CPU (reference for vtmat.wgsl; reached through vtmat until main.rs
/// declares it).
#[path = "lighting.rs"]
pub mod lighting;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use bevy::asset::{RenderAssetUsages, load_internal_asset, uuid_handle};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
use bevy::shader::{Shader, ShaderRef};
use idres::vtex::{VirtualTexture, VtAtlas, VtRect};

/// The material type: Bevy's StandardMaterial (lighting parameters, placeholder look) extended
/// with the virtual-texture inputs.
pub type VtMaterial = ExtendedMaterial<StandardMaterial, VtExt>;

const VT_SHADER: Handle<Shader> = uuid_handle!("5c0f6a52-8d9e-4b4f-9a0e-2d6b7f1c3e41");
const VT_PREPASS_SHADER: Handle<Shader> = uuid_handle!("b8a4f2d0-3c6e-4e19-9d57-1f0a6c2e8b73");
/// `rancher::vt_fetch`, imported by both.
const VT_FETCH_SHADER: Handle<Shader> = uuid_handle!("e2c9a7b1-5d3f-4a80-b6e4-7c1d9f0a2b56");
/// `rancher::lighting` (lighting.wgsl), the engine's shading.
const LIGHTING_SHADER: Handle<Shader> = uuid_handle!("7a3d91c4-2b6e-4f05-8c1a-9e4b0d2f6a17");
/// The shared engine lighting inputs every VT material binds (`EngineScene` fills them).
pub const SCENE_BUFFER: Handle<ShaderBuffer> = uuid_handle!("c41f0e2a-8b97-4d3c-a6e5-1f2b3c4d5e60");
pub const LIGHT_ATLAS: Handle<Image> = uuid_handle!("d52a1f3b-9ca8-4e4d-b7f6-2a3c4d5e6f71");
pub const PROBE_CUBES: Handle<Image> = uuid_handle!("e63b2a4c-adb9-4f5e-88a7-3b4d5e6f7a82");
/// Per-frame light state (`lighting::SceneAnim::evaluate`: colour × fade per light, fade per probe).
pub const LIGHT_DYN: Handle<ShaderBuffer> = uuid_handle!("f74c3b5d-bec0-4a6f-99b8-4c5e6f7a8b93");
/// The engine's alpha test: clip( cover - 0.5 ) (preZDrawAlphaUnique, shadowVmtrTransUnique).
const ALPHA_TEST: f32 = 0.5;

/// Texels of surrounding virtual texture kept around each level (physical pages have 4).
const MARGIN: u32 = 8;
/// log2(vt_maxAniso = 4), physicalFilterParms.z
const MAX_ANISO_LOG2: f32 = 2.0;
/// The first pass streams in this many levels coarser than the one asked for.
const COARSE_STEP: usize = 2;
/// Transcode threads of the streaming worker (`dm2016combat_VT_THREADS` overrides), kept low so the
/// game and builds keep the CPU.
const STREAM_THREADS: usize = 2;

#[derive(Clone, Copy, Debug, Default, ShaderType)]
pub struct VtParams {
    /// Per VT level: atlas uv = fract(material uv) * xy + zw.
    pub xform: [Vec4; 12],
    /// Material size in level-0 texels, first and last level in the atlas.
    pub rect: Vec4,
    /// LOD bias, log2(max aniso), textures loaded, 1 when alpha-tested (`material_masked`).
    pub misc: Vec4,
    /// Baked lightmap (`set_lightmap`): scale, 1 when bound, unused, unused.
    pub lightmap: Vec4,
    /// Engine shading: x 1 for static (world) models ($staticModel.x: LF_DINAMIC_ONLY lights skip
    /// them), y the model's ambient scale ($gpuAmbientParms.y), z/w unused.
    pub shading: Vec4,
    /// Non-lightmapped emissive factor (`lighting::material_emissive`): rgb × mask², w unused.
    pub emissive: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct VtExt {
    #[uniform(100)]
    pub params: VtParams,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub pages: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    pub color_mask: Option<Handle<Image>>,
    /// Optional baked HDR lightmap (e.g. a level of the map's unique VT, BC6H) sampled with UV_1:
    /// on meshes with UV_1 it replaces Bevy's diffuse lighting (ambient and lights) with
    /// albedo * lightmap * scale (lighting.inc USE_LIGHTMAP: ambient = lightmap * $lightMapScale *
    /// $envLightMapScale); specular from Bevy's lights stays.
    #[texture(105)]
    #[sampler(106)]
    pub lightmap: Option<Handle<Image>>,
    /// The map's engine lights, probes and ambient octree (`lighting::SceneBuffer`), shared by all.
    #[storage(107, read_only)]
    pub scene: Handle<ShaderBuffer>,
    /// `_lightimageatlas` (BC4, `lighting::LightAtlas`), shared by all.
    #[texture(108)]
    #[sampler(109)]
    pub light_atlas: Option<Handle<Image>>,
    /// envProbesMapArray (BC6H cubes, `lighting::append_probe_cube`), shared by all.
    #[texture(110, dimension = "cube_array")]
    #[sampler(111)]
    pub probes: Option<Handle<Image>>,
    /// Per-frame light colours and fades (`animate_lights`), shared by all.
    #[storage(112, read_only)]
    pub light_dyn: Handle<ShaderBuffer>,
}

impl Default for VtExt {
    fn default() -> Self {
        VtExt {
            // $gpuAmbientParms (0x1419950f0): x = r_ambientUseGPU (1) unless the entity opts out,
            // y = r_ambientChannelScale (1; min( 1, . ) when r_renderMode != 0) × renderModelInfo
            // ambientScale (idRenderModelInfo +0xac, default 1: fully exported entity decls write
            // `ambientScale = 1;`; player.decl and the hands set none)
            params: VtParams { shading: Vec4::new(0.0, 1.0, 0.0, 0.0), ..default() },
            pages: None,
            color_mask: None,
            lightmap: None,
            scene: SCENE_BUFFER,
            light_atlas: Some(LIGHT_ATLAS),
            probes: Some(PROBE_CUBES),
            light_dyn: LIGHT_DYN,
        }
    }
}

/// Marks a material as a static (world) model: lights flagged dynamicOnly skip it
/// (lighting.inc COMPUTE_LIGHTING: `skipStaticModel = $staticModel.x > 0 ? LF_DINAMIC_ONLY : 0`).
pub fn set_static_model(ext: &mut VtExt, is_static: bool) {
    ext.params.shading.x = if is_static { 1.0 } else { 0.0 };
}

/// Binds (or with None removes) a baked lightmap; `scale` = lightMapScale * envLightMapScale.
pub fn set_lightmap(ext: &mut VtExt, image: Option<Handle<Image>>, scale: f32) {
    ext.params.lightmap = Vec4::new(scale, if image.is_some() { 1.0 } else { 0.0 }, 0.0, 0.0);
    ext.lightmap = image;
}

impl MaterialExtension for VtExt {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(VT_SHADER)
    }
    fn prepass_fragment_shader() -> ShaderRef {
        ShaderRef::Handle(VT_PREPASS_SHADER)
    }
}

/// Registers the material, its shader and the system that streams finished textures in.
pub struct VtMaterialPlugin;

impl Plugin for VtMaterialPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, VT_FETCH_SHADER, "vtmat_fetch.wgsl", Shader::from_wgsl);
        load_internal_asset!(app, LIGHTING_SHADER, "lighting.wgsl", Shader::from_wgsl);
        load_internal_asset!(app, VT_SHADER, "vtmat.wgsl", Shader::from_wgsl);
        load_internal_asset!(app, VT_PREPASS_SHADER, "vtmat_prepass.wgsl", Shader::from_wgsl);
        app.add_plugins(MaterialPlugin::<VtMaterial>::default()).add_systems(Update, (stream_in, apply_scene, animate_lights.after(apply_scene)));
        // Empty engine scene until a map provides one (no lights, probes or ambient octree).
        let world = app.world_mut();
        let mut buffers = world.resource_mut::<Assets<ShaderBuffer>>();
        buffers.insert(&SCENE_BUFFER, scene_buffer(&lighting::SceneData::default().words)).ok();
        buffers.insert(&LIGHT_DYN, dyn_buffer(&[[0.0; 4]])).ok();
        let mut images = world.resource_mut::<Assets<Image>>();
        images.insert(&LIGHT_ATLAS, light_atlas_image(&lighting::LightAtlas::empty(4, 4))).ok();
        images.insert(&PROBE_CUBES, probe_image(1, vec![0; (6 * lighting::probe_cube_bytes()) as usize])).ok();
    }
}

/// A map's engine lighting inputs waiting to be uploaded (`apply_scene`).
#[derive(Resource, Default)]
pub struct PendingScene(pub Mutex<Option<lighting::SceneData>>);

fn scene_buffer(words: &[u32]) -> ShaderBuffer {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderBuffer::new(&bytes, RenderAssetUsages::RENDER_WORLD)
}

fn dyn_buffer(v: &[[f32; 4]]) -> ShaderBuffer {
    let bytes: Vec<u8> = v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect();
    ShaderBuffer::new(&bytes, RenderAssetUsages::RENDER_WORLD)
}

/// The map's per-frame light inputs and the last evaluation (`animate_lights`).
#[derive(Resource)]
pub struct EngineLightAnim {
    pub anim: lighting::SceneAnim,
    out: Vec<[f32; 4]>,
}

fn light_atlas_image(a: &lighting::LightAtlas) -> Image {
    let mut img = Image::new(Extent3d { width: a.size[0], height: a.size[1], depth_or_array_layers: 1 }, TextureDimension::D2, a.blocks.clone(), TextureFormat::Bc4RUnorm, RenderAssetUsages::RENDER_WORLD);
    // tex2Dlod( $lightsAtlasMap, .., 0 ): bilinear, the images carry their own (borderClamp) edges
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}

/// `count` BC6H probe cubes (envProbesMapArray) as one cube-array texture with their 7 mips.
fn probe_image(count: u32, data: Vec<u8>) -> Image {
    let s = lighting::PROBE_SIZE;
    let mut img = Image::new(Extent3d { width: s, height: s, depth_or_array_layers: 6 * count }, TextureDimension::D2, data, TextureFormat::Bc6hRgbUfloat, RenderAssetUsages::RENDER_WORLD);
    img.texture_descriptor.mip_level_count = lighting::PROBE_MIPS;
    img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::CubeArray), ..default() });
    // texCUBEARRAYlod: trilinear between the mips
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}

/// Uploads a pending engine scene into the shared bindings and re-prepares every VT material so
/// their bind groups pick the new resources up.
pub fn apply_scene(mut commands: Commands, pending: Option<Res<PendingScene>>, mut buffers: ResMut<Assets<ShaderBuffer>>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<VtMaterial>>) {
    let Some(pending) = pending else { return };
    let Some(scene) = pending.0.lock().unwrap().take() else { return };
    buffers.insert(&SCENE_BUFFER, scene_buffer(&scene.words)).ok();
    // sized once here; animate_lights then rewrites it in place (same size: the GPU buffer is reused)
    let n = scene.anim.lights.len().max(1);
    buffers.insert(&LIGHT_DYN, dyn_buffer(&vec![[0.0; 4]; n])).ok();
    commands.insert_resource(EngineLightAnim { anim: scene.anim, out: Vec::with_capacity(n) });
    images.insert(&LIGHT_ATLAS, light_atlas_image(&scene.atlas)).ok();
    if scene.probe_count > 0 {
        images.insert(&PROBE_CUBES, probe_image(scene.probe_count, scene.probe_blocks)).ok();
    }
    for (_, m) in mats.iter_mut() {
        m.extension.scene = SCENE_BUFFER;
        m.extension.light_dyn = LIGHT_DYN;
    }
    eprintln!("lighting: engine scene uploaded ({} words, {} probe cubes)", scene.words.len(), scene.probe_count);
}

/// Evaluates the map lights for this frame from the first 3D camera's position: light material
/// colour programs at the Time / SysTime renderparms and the distance fade (lighting::SceneAnim).
/// INTERIM: renderView.time is taken as the app's virtual clock in milliseconds and SysTime as the
/// real clock (the exe's sources for view +0x8e0 and 0x14032ab90 are not traced); the
/// r_lightDistanceFadeMultiplier cvar is its default.
fn animate_lights(anim: Option<ResMut<EngineLightAnim>>, time: Res<Time>, real: Res<Time<bevy::time::Real>>, cams: Query<(&Camera, &GlobalTransform), With<Camera3d>>, mut buffers: ResMut<Assets<ShaderBuffer>>) {
    let Some(mut anim) = anim else { return };
    let Some((_, cam)) = cams.iter().filter(|(c, _)| c.is_active).min_by_key(|(c, _)| c.order) else { return };
    let b = cam.translation();
    // Bevy ( x, y, z ) = engine ( -y, z, -x )
    let view = Vec3::new(-b.z, -b.x, b.y);
    let t = lighting::renderparm_time(time.elapsed().as_millis() as u64);
    let st = lighting::renderparm_time(real.elapsed().as_millis() as u64);
    let EngineLightAnim { anim, out } = &mut *anim;
    anim.evaluate(t, st, view, out);
    if out.is_empty() {
        return;
    }
    if let Some(mut buf) = buffers.get_mut(&LIGHT_DYN) {
        buf.data = Some(out.iter().flatten().flat_map(|f| f.to_le_bytes()).collect());
    }
}

/// A material without virtual texture (placeholders).
pub fn flat(mats: &mut Assets<VtMaterial>, base: StandardMaterial) -> Handle<VtMaterial> {
    mats.add(VtMaterial { base, extension: VtExt::default() })
}

struct Job {
    id: AssetId<VtMaterial>,
    name: String,
    rect: VtRect,
    first: usize,
    last: usize,
    coarse: bool,
}

struct Done {
    id: AssetId<VtMaterial>,
    name: String,
    rect: VtRect,
    atlas: VtAtlas,
    lod_bias: f32,
    emissive: Vec3,
}

/// Finished atlases, handed from the worker to `stream_in`.
static DONE: Mutex<Vec<Done>> = Mutex::new(Vec::new());
/// Alpha-tested twins of streamed materials: they get the same textures.
static TWINS: Mutex<Vec<(AssetId<VtMaterial>, AssetId<VtMaterial>)>> = Mutex::new(Vec::new());

pub struct VtMaterials {
    vt: Option<Arc<VirtualTexture>>,
    jobs: Option<Sender<Job>>,
    cache: HashMap<(String, usize), Option<Handle<VtMaterial>>>,
    masked: HashMap<(String, usize), Option<Handle<VtMaterial>>>,
}

fn cache_dir() -> Option<PathBuf> {
    match std::env::var("dm2016combat_CACHE") {
        Ok(v) if v.eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../gamedata/cache")),
    }
}

impl VtMaterials {
    pub fn open(doom: &Path) -> Self {
        let vt = match VirtualTexture::open(doom) {
            Ok(mut v) => {
                v.set_cache_dir(cache_dir());
                v.set_threads(std::env::var("dm2016combat_VT_THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(STREAM_THREADS));
                Arc::new(v)
            }
            Err(e) => {
                eprintln!("virtual textures unavailable: {e:#}");
                return VtMaterials { vt: None, jobs: None, cache: HashMap::new(), masked: HashMap::new() };
            }
        };
        let (tx, rx) = channel();
        let (wvt, wdoom) = (vt.clone(), doom.to_path_buf());
        let spawned = std::thread::Builder::new().name("vt-stream".into()).spawn(move || worker(wvt, wdoom, rx));
        if let Err(e) = spawned {
            eprintln!("virtual textures: no worker thread: {e}");
        }
        VtMaterials { vt: Some(vt), jobs: Some(tx), cache: HashMap::new(), masked: HashMap::new() }
    }

    /// The material for `name` with VT levels from `level` (0 = full resolution) down, or None when
    /// it is not in the virtual texture. Returns at once; textures stream in (coarse first).
    pub fn material(&mut self, name: &str, level: usize, _images: &mut Assets<Image>, mats: &mut Assets<VtMaterial>) -> Option<Handle<VtMaterial>> {
        let key = (name.to_string(), level);
        if let Some(h) = self.cache.get(&key) {
            return h.clone();
        }
        let built = self.start(name, level, mats);
        self.cache.insert(key, built.clone());
        built
    }

    /// The alpha-tested variant of `material(name, level)` for surfaces drawn with megatrans
    /// (preZDrawAlphaUnique / shadowVmtrTransUnique / outsideUniqueTrans): same textures, discards
    /// where the cover (page layer 2 blue) < 0.5 in the colour, depth and shadow passes.
    pub fn material_masked(&mut self, name: &str, level: usize, images: &mut Assets<Image>, mats: &mut Assets<VtMaterial>) -> Option<Handle<VtMaterial>> {
        let key = (name.to_string(), level);
        if let Some(h) = self.masked.get(&key) {
            return h.clone();
        }
        let built = self.material(name, level, images, mats).and_then(|base| {
            let mut m = mats.get(&base)?.clone();
            m.base.alpha_mode = AlphaMode::Mask(ALPHA_TEST);
            m.extension.params.misc.w = 1.0;
            let twin = mats.add(m);
            TWINS.lock().unwrap().push((base.id(), twin.id()));
            Some(twin)
        });
        self.masked.insert(key, built.clone());
        built
    }

    fn start(&self, name: &str, level: usize, mats: &mut Assets<VtMaterial>) -> Option<Handle<VtMaterial>> {
        let vt = self.vt.as_ref()?;
        let rect = vt.rect(name)?;
        let last = vt.level_count().saturating_sub(1).max(level);
        let handle = flat(mats, StandardMaterial { base_color: Color::srgb(0.5, 0.5, 0.5), perceptual_roughness: 0.8, ..default() });
        if let Some(tx) = &self.jobs {
            let id = handle.id();
            let coarse = (level + COARSE_STEP).min(last);
            if coarse > level {
                let _ = tx.send(Job { id, name: name.to_string(), rect, first: coarse, last, coarse: true });
            }
            let _ = tx.send(Job { id, name: name.to_string(), rect, first: level, last, coarse: false });
        }
        Some(handle)
    }
}

fn worker(vt: Arc<VirtualTexture>, doom: PathBuf, rx: Receiver<Job>) {
    let (mut coarse, mut fine) = (VecDeque::new(), VecDeque::new());
    let mut decls = Decls::new(doom);
    let push = |j: Job, coarse: &mut VecDeque<Job>, fine: &mut VecDeque<Job>| if j.coarse { coarse.push_back(j) } else { fine.push_back(j) };
    loop {
        if coarse.is_empty() && fine.is_empty() {
            match rx.recv() {
                Ok(j) => push(j, &mut coarse, &mut fine),
                Err(_) => return,
            }
        }
        while let Ok(j) = rx.try_recv() {
            push(j, &mut coarse, &mut fine);
        }
        let Some(job) = coarse.pop_front().or_else(|| fine.pop_front()) else { continue };
        let (lod_bias, emissive) = decls.material_info(&job.name);
        let t = std::time::Instant::now();
        match vt.atlas(job.rect, job.first, job.last, MARGIN) {
            Ok(atlas) => {
                if atlas.missing_pages > 0 {
                    eprintln!("{}: {} virtual-texture pages missing", job.name, atlas.missing_pages);
                }
                if std::env::var_os("dm2016combat_VT_LOG").is_some() {
                    eprintln!("vt {} levels {}..={} {}x{} in {:.2}s", job.name, job.first, job.last, atlas.width, atlas.height, t.elapsed().as_secs_f32());
                }
                DONE.lock().unwrap().push(Done { id: job.id, name: job.name, rect: job.rect, atlas, lod_bias, emissive });
            }
            Err(e) => eprintln!("{}: {e:#}", job.name),
        }
    }
}

/// Material and render-program decls: weapons use programs with `#define USE_LOD_HACK 1`
/// (outsidegun*), which sample one level finer (SAMPLE_VMTR); the material's renderparms give the
/// non-lightmapped emissive (`lighting::material_emissive`).
struct Decls {
    doom: PathBuf,
    container: Option<Option<idres::Container>>,
    programs: HashMap<String, f32>,
}

impl Decls {
    fn new(doom: PathBuf) -> Self {
        Decls { doom, container: None, programs: HashMap::new() }
    }

    /// (LOD bias, emissive factor) of a material.
    fn material_info(&mut self, material: &str) -> (f32, Vec3) {
        let doom = self.doom.clone();
        let defaults = (0.0, lighting::material_emissive(""));
        let Some(c) = self.container.get_or_insert_with(|| idres::Container::open(&doom.join("base"), "gameresources").ok()).as_ref() else { return defaults };
        let read = |name: String| c.read_by_name(&name).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
        let Some(text) = read(format!("generated/decls/material/{material}.decl")) else { return defaults };
        let emissive = lighting::material_emissive(&text);
        let program = text.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some("ambientprogram")).then(|| it.next().map(|p| p.trim_matches('"').to_string())).flatten()
        });
        let Some(program) = program else { return (0.0, emissive) };
        if let Some(&b) = self.programs.get(&program) {
            return (b, emissive);
        }
        let hack = read(format!("generated/decls/renderprog/{program}.decl")).is_some_and(|t| t.lines().any(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            w.len() >= 3 && w[0] == "#define" && w[1] == "USE_LOD_HACK" && w[2] != "0"
        }));
        let b = if hack { -1.0 } else { 0.0 };
        self.programs.insert(program, b);
        (b, emissive)
    }
}

fn sampler() -> ImageSampler {
    // physical pages: bilinear, anisotropic up to vt_maxAniso, no mips
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 4,
        ..default()
    })
}

fn image(w: u32, h: u32, layers: u32, format: TextureFormat, data: Vec<u8>) -> Image {
    let mut img = Image::new_uninit(Extent3d { width: w, height: h, depth_or_array_layers: layers }, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    img.data = Some(data);
    img.sampler = sampler();
    if layers > 1 {
        img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::D2Array), ..default() });
    }
    img
}

fn params(rect: VtRect, atlas: &VtAtlas, lod_bias: f32, emissive: Vec3) -> VtParams {
    let mut p = VtParams::default();
    let (aw, ah) = (atlas.width as f64, atlas.height as f64);
    for l in &atlas.levels {
        let s = (1u64 << l.level) as f64;
        p.xform[l.level] = Vec4::new(
            (rect.w as f64 / s / aw) as f32,
            (rect.h as f64 / s / ah) as f32,
            ((rect.x as f64 / s - l.x0 as f64 + l.ax as f64) / aw) as f32,
            ((rect.y as f64 / s - l.y0 as f64 + l.ay as f64) / ah) as f32,
        );
    }
    let (first, last) = (atlas.levels.first().map_or(0, |l| l.level), atlas.levels.last().map_or(0, |l| l.level));
    p.rect = Vec4::new(rect.w as f32, rect.h as f32, first as f32, last as f32);
    p.misc = Vec4::new(lod_bias, MAX_ANISO_LOG2, 1.0, 0.0);
    p.emissive = emissive.extend(0.0);
    p
}

/// Swaps finished atlases into their materials and alpha-tested twins (finer levels replace coarser
/// ones).
pub fn stream_in(mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<VtMaterial>>) {
    let done: Vec<Done> = std::mem::take(&mut *DONE.lock().unwrap());
    if done.is_empty() {
        return;
    }
    let twins = TWINS.lock().unwrap().clone();
    for d in done {
        let first = d.atlas.levels.first().map_or(usize::MAX, |l| l.level);
        let p = params(d.rect, &d.atlas, d.lod_bias, d.emissive);
        let VtAtlas { width, height, pages, color_mask, .. } = d.atlas;
        let mut textures = None;
        let mut pages = Some(pages);
        let mut color_mask = Some(color_mask);
        for id in std::iter::once(d.id).chain(twins.iter().filter(|t| t.0 == d.id).map(|t| t.1)) {
            let Some(mut m) = mats.get_mut(id) else { continue };
            if m.extension.params.misc.z > 0.5 && m.extension.params.rect.z as usize <= first {
                continue; // a finer atlas is already in
            }
            let (pages_h, mask_h) = textures
                .get_or_insert_with(|| {
                    (
                        images.add(image(width, height, 3, TextureFormat::Bc3RgbaUnormSrgb, pages.take().unwrap_or_default())),
                        images.add(image(width, height, 1, TextureFormat::Bc7RgbaUnorm, color_mask.take().unwrap_or_default())),
                    )
                })
                .clone();
            let mut mp = p;
            mp.lightmap = m.extension.params.lightmap;
            mp.misc.w = m.extension.params.misc.w;
            mp.shading = m.extension.params.shading;
            m.extension.pages = Some(pages_h);
            m.extension.color_mask = Some(mask_h);
            m.extension.params = mp;
        }
        let _ = d.name;
    }
}
