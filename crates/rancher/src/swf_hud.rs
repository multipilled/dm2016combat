//! DOOM's single-player HUD drawn from the game's own SWFs through `idswf`: health and armour
//! (hud_bottom_left), ammo and weapon icon (ws_0), the low-health warning (hud_bottom) and the
//! weapon's crosshair (reticle).
//!
//! Every frame `idswf::hud::Hud` applies [`HudState`] to the movies the way the game's HUD widgets
//! do and advances them; their draw lists (`idswf::render::draw_clipped`, clip layers resolved
//! geometrically so no stencil is needed) become one `Mesh2d` per batch on a 2D camera drawn after
//! the world and view-model cameras. The shader below reproduces the engine's GUI stage programs
//! (guiblend, guiblend_coacgy for the Co/A/Cg/Y atlases, SDF font coverage) and each SWF blend mode
//! uses the engine's blend equation. The engine blends GUIs in gamma space; the shader converts
//! each fragment to linear premultiplied colour for Bevy's linear main texture, which only shows
//! where translucent layers overlap.
//!
//! Placement is the game's (`idswf::placement`): every movie is a quad in view space on a tag of the
//! player's helmet model, projected with the HUD's fixed 80-degree field of view (the reticle with the
//! view's). The CPU computes each vertex's clip-space position, so the panels keep their perspective and
//! the textures interpolate perspective-correctly.
//!
//! Loading (container, SWFs, BC7 atlases, SDF fonts, mip chains) runs on a worker thread; the HUD
//! appears when it is done. Everything is read from the user's install at runtime.
//!
//! RANCHER_HUD_VITALS=<health>,<armor>  starting placeholder vitals (for checks; default 100,0)

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};

use anyhow::Context;
use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::camera::{ClearColorConfig, Hdr};
use bevy::camera::visibility::RenderLayers;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, MeshVertexAttribute, MeshVertexBufferLayoutRef, PrimitiveTopology, VertexFormat};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    TextureDimension, TextureFormat,
};
use bevy::shader::{Shader, ShaderRef};
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin};
use idres::decldb::DeclDb;
use idswf::hud::Hud;
use idswf::render::{Blend, DrawList, Shader as Stage, TexRef};
use idswf::texture::Texture;

mod wheel;
pub use wheel::WeaponWheel;

/// `ImageKey::Atlas` index of the weapon wheel movie (past the HUD movies').
const WHEEL_ATLAS: usize = 100;

/// Render layer of the HUD meshes and camera.
pub const HUD_LAYER: usize = 16;

/// What the HUD shows. Fill health and armour from game code (placeholders until the sim has them);
/// the weapon fields (ammo, max ammo, icon, reticle) are refreshed from `Combat` every frame.
#[derive(Resource, Clone, Debug, Deref, DerefMut)]
pub struct HudState(pub idswf::hud::HudState);

impl Default for HudState {
    fn default() -> Self {
        // A fresh campaign start: 100 health, no armour.
        let (health, armor) = std::env::var("RANCHER_HUD_VITALS")
            .ok()
            .and_then(|v| {
                let (h, a) = v.split_once(',')?;
                Some((h.trim().parse().ok()?, a.trim().parse().ok()?))
            })
            .unwrap_or((100.0, 0.0));
        HudState(idswf::hud::HudState { health, max_health: 100.0, armor, max_armor: 50.0, ..default() })
    }
}

pub struct SwfHudPlugin;

impl Plugin for SwfHudPlugin {
    fn build(&self, app: &mut App) {
        app.world_mut().resource_mut::<Assets<Shader>>().insert(SHADER.id(), Shader::from_wgsl(SHADER_SRC, file!())).expect("uuid handle");
        app.add_plugins(Material2dPlugin::<HudMaterial>::default())
            .init_resource::<HudState>()
            .init_resource::<WeaponWheel>()
            .add_systems(Startup, start)
            .add_systems(PostUpdate, (finish_loading, wheel::logic, update).chain());
    }
}

const SHADER: Handle<Shader> = uuid_handle!("8a2f4c61-3b7e-4d95-a0c8-5e1f9b6d7a24");

/// Per-vertex additive colour term (the SWF colour transform's add, as the engine packs it).
const ATTRIBUTE_ADD: MeshVertexAttribute = MeshVertexAttribute::new("SwfAdd", 0x5f3a_9c21_7e04_b6d1, VertexFormat::Float32x4);
/// Per-vertex clip-space position from the HUD's own projection (`ATTRIBUTE_POSITION` only feeds culling).
const ATTRIBUTE_CLIP: MeshVertexAttribute = MeshVertexAttribute::new("SwfClip", 0x5f3a_9c21_7e04_b6d2, VertexFormat::Float32x4);

const STAGE_GUI: u8 = 0;
const STAGE_ATLAS: u8 = 1;
const STAGE_SDF: u8 = 2;

/// One SWF batch's texture, stage program and blend mode.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
#[bind_group_data(HudMaterialKey)]
pub struct HudMaterial {
    #[texture(0)]
    #[sampler(1)]
    texture: Handle<Image>,
    stage: u8,
    blend: Blend,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct HudMaterialKey {
    stage: u8,
    blend: u8,
}

impl From<&HudMaterial> for HudMaterialKey {
    fn from(m: &HudMaterial) -> Self {
        HudMaterialKey { stage: m.stage, blend: blend_code(m.blend) }
    }
}

impl Material2d for HudMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    fn specialize(descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, key: Material2dKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vertex = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(1),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(2),
            ATTRIBUTE_ADD.at_shader_location(3),
            ATTRIBUTE_CLIP.at_shader_location(4),
        ])?;
        descriptor.vertex.buffers = vec![vertex];
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push(
                match key.bind_group_data.stage {
                    STAGE_ATLAS => "SWF_ATLAS",
                    STAGE_SDF => "SWF_SDF",
                    _ => "SWF_GUI",
                }
                .into(),
            );
            if let Some(Some(target)) = fragment.targets.first_mut() {
                target.blend = Some(blend_state(key.bind_group_data.blend));
            }
        }
        Ok(())
    }
}

const BLENDS: [Blend; 8] = [Blend::Normal, Blend::Add, Blend::Multiply, Blend::Screen, Blend::Lighten, Blend::Darken, Blend::Subtract, Blend::Overlay];

fn blend_code(b: Blend) -> u8 {
    BLENDS.iter().position(|x| *x == b).unwrap_or(0) as u8
}

/// The engine's blend equations for premultiplied output (render state bits built at 0x14161d5d0).
fn blend_state(code: u8) -> BlendState {
    let c = |src_factor, dst_factor, operation| BlendComponent { src_factor, dst_factor, operation };
    use BlendFactor::{Dst, One, OneMinusSrcAlpha};
    let comp = match BLENDS.get(code as usize).copied().unwrap_or(Blend::Normal) {
        Blend::Normal => c(One, OneMinusSrcAlpha, BlendOperation::Add),
        Blend::Add => c(One, One, BlendOperation::Add),
        Blend::Multiply => c(Dst, OneMinusSrcAlpha, BlendOperation::Add),
        Blend::Screen => c(Dst, One, BlendOperation::ReverseSubtract),
        Blend::Lighten => c(One, One, BlendOperation::Max),
        Blend::Darken => c(One, One, BlendOperation::Min),
        Blend::Subtract => c(One, One, BlendOperation::ReverseSubtract),
        Blend::Overlay => c(Dst, One, BlendOperation::Add),
    };
    BlendState { color: comp, alpha: comp }
}

const SHADER_SRC: &str = r"
#import bevy_render::color_operations::srgb_to_linear

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var swf_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var swf_sampler: sampler;

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) add: vec4<f32>,
    @location(4) clip: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) add: vec4<f32>,
};

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
    out.clip = v.clip;
    out.uv = v.uv;
    out.color = v.color;
    out.add = v.add;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = textureSample(swf_texture, swf_sampler, in.uv);
#ifdef SWF_ATLAS
    // guiblend_coacgy: R = Co, G = alpha, B = Cg, A = Y, chroma biased by 132/255.
    let co = t.r - 132.0 / 255.0;
    let cg = t.b - 132.0 / 255.0;
    let rgba = vec4<f32>(t.a + co - cg, t.a + cg, t.a - co - cg, t.g);
    let c = saturate(rgba * in.color + in.add);
#else
#ifdef SWF_SDF
    // Distance in the atlas alpha; edge width from the screen-space texel rate.
    let pd = clamp((abs(dpdx(in.uv.x)) + abs(dpdy(in.uv.y))) * 32.0, 0.0001, 0.5);
    let coverage = smoothstep(0.5 - pd, 0.5 + pd, t.a);
    let c = saturate(vec4<f32>(in.color.rgb, coverage * in.color.a));
#else
    // guiblend
    let c = saturate(t * (in.color + in.add));
#endif
#endif
    return vec4<f32>(srgb_to_linear(c.rgb) * c.a, c.a);
}
";

/// Textures the draw lists refer to (`TexRef::Atlas` is per movie).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum ImageKey {
    White,
    Atlas(usize),
    Font(String),
    Material(String),
}

/// An RGBA8 texture with its mip chain, built off the main thread.
pub(crate) struct Mips {
    width: u32,
    height: u32,
    levels: u32,
    data: Vec<u8>,
}

struct Loaded {
    hud: Hud,
    wheel: Option<idswf::wheel::Movie>,
    assets: idswf::Assets,
    images: Vec<(ImageKey, Mips)>,
}

#[derive(Resource)]
struct Loading(Mutex<Receiver<anyhow::Result<Loaded>>>);

#[derive(Resource)]
struct HudRuntime {
    hud: Hud,
    /// The weapon wheel movie (weapon_select4).
    wheel: Option<idswf::wheel::Movie>,
    assets: idswf::Assets,
    decls: DeclDb,
    /// Weapon decl -> (icon material, weaponReticle decl, the ammo decl's lowAmmoWarningCount).
    visuals: HashMap<String, (Option<String>, Option<String>, i32)>,
    /// Base perk decl -> its HUD view (iconMaterial, displayName, shouldHideOnHud).
    perks: HashMap<String, Option<idswf::hud::HudMod>>,
    /// RANCHER_TRACE=1: last logged (weapon, mod, other owned, charge tenths, lock tenths).
    trace: Option<(Option<usize>, Option<String>, bool, i32, i32)>,
    batcher: Batcher,
}

struct Slot {
    entity: Entity,
    mesh: Handle<Mesh>,
    material: Option<Handle<HudMaterial>>,
    hash: u64,
}

/// What a [`Batcher`] spawns meshes and materials with.
pub(crate) struct BatchCx<'a, 'w, 's> {
    pub commands: &'a mut Commands<'w, 's>,
    pub meshes: &'a mut Assets<Mesh>,
    pub materials: &'a mut Assets<HudMaterial>,
    pub images: &'a mut Assets<Image>,
}

/// Turns SWF draw lists into one `Mesh2d` per batch slot on the HUD camera (shared by the HUD and the menu). Each
/// frame: [`Batcher::begin`], [`Batcher::push`] per movie in drawing order, then [`Batcher::finish`].
pub(crate) struct Batcher {
    images: HashMap<ImageKey, Option<Handle<Image>>>,
    materials: HashMap<(ImageKey, u8, u8), Handle<HudMaterial>>,
    /// One mesh entity per batch slot, with the hash of what it holds.
    slots: Vec<Slot>,
    used: usize,
    /// z of the first slot; later slots draw on top (transparent 2D items are sorted by z).
    z0: f32,
}

impl Batcher {
    pub(crate) fn new(images: HashMap<ImageKey, Option<Handle<Image>>>, z0: f32) -> Self {
        Batcher { images, materials: HashMap::new(), slots: Vec::new(), used: 0, z0 }
    }

    pub(crate) fn begin(&mut self) {
        self.used = 0;
    }

    /// Adds one movie's draw list. `placed` is the movie's panel and projection (tag-placed HUD movies), else it
    /// fills the `win` window; `frame` is the movie's stage size.
    pub(crate) fn push(&mut self, cx: &mut BatchCx, assets: &idswf::Assets, list: &DrawList, atlas: &ImageKey, placed: Option<(idswf::placement::Panel, idswf::placement::Projection)>, frame: (f32, f32), win: (f32, f32), alpha: f32) {
        let ((fw, fh), (w, h)) = (frame, win);
        for batch in &list.batches {
            let key = match &batch.texture {
                TexRef::White => ImageKey::White,
                TexRef::Atlas => atlas.clone(),
                TexRef::Font(f) => ImageKey::Font(idswf::font::face_dir(f)),
                TexRef::Material(m) => ImageKey::Material(m.to_string()),
            };
            let texture = self
                .images
                .entry(key.clone())
                .or_insert_with(|| match &key {
                    ImageKey::Material(m) => assets.material_texture(m).map(|t| cx.images.add(image(mips(&t)))),
                    _ => None,
                })
                .clone();
            let Some(texture) = texture else { continue };
            let stage = match batch.shader {
                Stage::Gui => STAGE_GUI,
                Stage::Atlas => STAGE_ATLAS,
                Stage::Sdf => STAGE_SDF,
            };
            let material = self
                .materials
                .entry((key, stage, blend_code(batch.blend)))
                .or_insert_with(|| cx.materials.add(HudMaterial { texture, stage, blend: batch.blend }))
                .clone();

            // Clip-space position (depth = distance ahead of the eye) and the matching window point.
            let clip = |v: [f32; 2]| match placed {
                Some((panel, proj)) => {
                    let n = proj.ndc(panel.stage_to_view(v, fw, fh));
                    [n[0] * n[2], n[1] * n[2], 0.5 * n[2], n[2]]
                }
                None => [v[0] / w * 2.0 - 1.0, 1.0 - v[1] / h * 2.0, 0.5, 1.0],
            };
            let to_world = |c: [f32; 4]| [c[0] / c[3] * w * 0.5, c[1] / c[3] * h * 0.5, 0.0];
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (w.to_bits(), h.to_bits(), alpha.to_bits(), batch.indices.as_slice()).hash(&mut hasher);
            if let Some((panel, proj)) = placed {
                for f in panel.origin.iter().chain(panel.axis.iter().flatten()).chain([&panel.scale, &proj.tan_x, &proj.tan_y]) {
                    f.to_bits().hash(&mut hasher);
                }
            }
            for v in &batch.verts {
                for f in v.pos.iter().chain(&v.uv).chain(&v.color).chain(&v.add) {
                    f.to_bits().hash(&mut hasher);
                }
            }
            let hash = hasher.finish();
            let build = || {
                let clips: Vec<[f32; 4]> = batch.verts.iter().map(|v| clip(v.pos)).collect();
                Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, clips.iter().map(|&c| to_world(c)).collect::<Vec<_>>())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, batch.verts.iter().map(|v| v.uv).collect::<Vec<_>>())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, batch.verts.iter().map(|v| [v.color[0], v.color[1], v.color[2], v.color[3] * alpha]).collect::<Vec<_>>())
                    .with_inserted_attribute(ATTRIBUTE_ADD, batch.verts.iter().map(|v| v.add).collect::<Vec<_>>())
                    .with_inserted_attribute(ATTRIBUTE_CLIP, clips)
                    .with_inserted_indices(Indices::U32(batch.indices.clone()))
            };
            let slot = self.used;
            if slot == self.slots.len() {
                // Meshes are only ever created with data (Bevy's mesh allocator mishandles empty vertex buffers).
                let mesh = cx.meshes.add(build());
                let entity = cx
                    .commands
                    .spawn((Mesh2d(mesh.clone()), Transform::from_xyz(0.0, 0.0, self.z0 + slot as f32 * 0.01), Visibility::Hidden, RenderLayers::layer(HUD_LAYER), bevy::camera::visibility::NoFrustumCulling))
                    .id();
                self.slots.push(Slot { entity, mesh, material: None, hash });
            }
            let s = &mut self.slots[slot];
            self.used += 1;
            if hash != s.hash {
                s.hash = hash;
                let _ = cx.meshes.insert(&s.mesh, build());
            }
            if s.material.as_ref() != Some(&material) {
                cx.commands.entity(s.entity).insert(MeshMaterial2d(material.clone()));
                s.material = Some(material);
            }
        }
    }

    /// Hides the slots this frame did not use and shows the used ones.
    pub(crate) fn finish(&mut self, vis: &mut Query<&mut Visibility>) {
        for (i, s) in self.slots.iter().enumerate() {
            if let Ok(mut v) = vis.get_mut(s.entity) {
                v.set_if_neq(if i < self.used { Visibility::Inherited } else { Visibility::Hidden });
            }
        }
    }
}

#[derive(Component)]
pub struct HudCamera;

fn start(mut commands: Commands) {
    commands.spawn((
        HudCamera,
        Camera2d,
        // Shares the 3D cameras' HDR main texture (post.rs tonemaps on the last 3D camera).
        Hdr,
        Camera { order: 2, clear_color: ClearColorConfig::None, ..default() },
        RenderLayers::layer(HUD_LAYER),
    ));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(load());
    });
    commands.insert_resource(Loading(Mutex::new(rx)));
}

fn load() -> anyhow::Result<Loaded> {
    let doom = idres::find_install().context("DOOM (2016) install not found")?;
    let assets = idswf::Assets::open(&doom)?;
    let hud = Hud::load(&assets)?;
    let mut images = vec![(ImageKey::White, mips(&Texture::white()))];
    for (i, (_, p)) in hud.players().iter().enumerate() {
        player_images(&mut images, i, p);
    }
    let wheel = idswf::wheel::Movie::load(&assets).map_err(|e| eprintln!("weapon wheel: {e:#}")).ok();
    if let Some(w) = &wheel {
        player_images(&mut images, WHEEL_ATLAS, &w.player);
    }
    Ok(Loaded { hud, wheel, assets, images })
}

/// The movie's atlas (as `ImageKey::Atlas(atlas)`) and fonts, mip-mapped, for the textures its draw lists use.
pub(crate) fn player_images(images: &mut Vec<(ImageKey, Mips)>, atlas: usize, p: &idswf::Player) {
    if let Some(a) = &p.atlas {
        images.push((ImageKey::Atlas(atlas), mips(a)));
    }
    for (face, f) in &p.fonts {
        if !images.iter().any(|(k, _)| *k == ImageKey::Font(face.clone())) {
            images.push((ImageKey::Font(face.clone()), mips(&f.atlas)));
        }
    }
}

fn finish_loading(mut commands: Commands, loading: Option<Res<Loading>>, mut images: ResMut<Assets<Image>>) {
    let Some(loading) = loading else { return };
    let result = match loading.0.lock().unwrap().try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("loader thread died")),
    };
    commands.remove_resource::<Loading>();
    match result {
        Ok(l) => {
            let decls = DeclDb::new(l.assets.container.clone());
            let handles = l.images.into_iter().map(|(k, m)| (k, Some(images.add(image(m))))).collect();
            commands.insert_resource(HudRuntime {
                hud: l.hud,
                wheel: l.wheel,
                assets: l.assets,
                decls,
                visuals: HashMap::new(),
                perks: HashMap::new(),
                trace: std::env::var("RANCHER_TRACE").is_ok_and(|v| v == "1").then_some((None, None, false, -1, -1)),
                batcher: Batcher::new(handles, 0.0),
            });
        }
        Err(e) => eprintln!("swf hud: {e:#}"),
    }
}

/// Feeds the HUD, advances its movies and rebuilds the batch meshes that changed.
#[allow(clippy::too_many_arguments)]
fn update(
    mut commands: Commands,
    rt: Option<ResMut<HudRuntime>>,
    mut state: ResMut<HudState>,
    combat: Option<Res<crate::combat::Combat>>,
    zoom: Option<Res<crate::combat::ZoomStatus>>,
    sim: Option<Res<crate::Sim>>,
    menu: Option<Res<crate::settings::MenuOpen>>,
    settings: Option<Res<crate::settings::Settings>>,
    time: Res<Time>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HudMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut vis: Query<&mut Visibility>,
) {
    let Some(mut rt) = rt else { return };
    let rt = &mut *rt;
    let Ok(window) = window.single() else { return };
    let (w, h) = (window.width(), window.height());
    // The view's field of view (the zoom's when zooming, else g_fov): the reticle's projection and spread.
    let view_fov = match (zoom.as_deref(), sim.as_deref()) {
        (Some(z), _) if z.fov > 0.0 => z.fov,
        (_, Some(s)) => s.player.cfg.fov,
        _ => 90.0,
    };
    if let Some(c) = combat {
        weapon_from_combat(&mut state, &c, &rt.decls, &mut rt.visuals);
        mods_from_combat(&mut state, &c, &rt.decls, &mut rt.perks);
        if let Some(last) = rt.trace.as_mut() {
            let now = (state.weapon_id, state.weapon_mod.as_ref().map(|m| m.perk.clone()), state.other_mod_owned, (state.reticle_charge * 10.0) as i32, (state.reticle_lock * 10.0) as i32);
            if (&now.0, &now.1, now.2) != (&last.0, &last.1, last.2) {
                let other = state.other_mod.as_ref().map_or("none", |m| m.perk.as_str());
                println!("[hud] weapon {:?} mod {} other {other} owned {} slot {}", now.0, now.1.as_deref().unwrap_or("none"), now.2, state.mod_slot);
            }
            if (now.3, now.4) != (last.3, last.4) {
                println!("[hud] charge {:.3} lock {:.3} reticle {}", state.reticle_charge, state.reticle_lock, state.reticle.as_deref().unwrap_or("none"));
            }
            *last = now;
        }
        // idHudReticleInfo::spread = GetSpread / tan(fov / 2) (0x140e04f3b); zoomed, the reticle widget gets 0
        // (view_ReticalAllowZoomSpread 0, 0x140c307d0).
        let zoomed = zoom.as_deref().is_some_and(|z| z.zoomed);
        let mut spread = c.arsenal.spread;
        let value = spread.value(c.arsenal.time_ms) / (view_fov.to_radians() * 0.5).tan();
        state.reticle_spread = if zoomed { 0.0 } else { value };
        state.reticle_zoomed = zoomed;
    }
    rt.hud.update(&state, time.delta_secs_f64());
    let layout = rt.hud.layout(w, h, view_fov);

    let mut cx = BatchCx { commands: &mut commands, meshes: &mut meshes, materials: &mut materials, images: &mut images };
    // The HUD hides while the pause menu is up (INTERIM: not decoded) and with the "Enable HUD" setting off
    // (g_setting_hud_show, the GAME page's toggle).
    let shown = state.visible && !menu.as_ref().is_some_and(|m| m.0) && settings.as_ref().is_none_or(|s| s.get("g_setting_hud_show").is_none() || s.bool("g_setting_hud_show"));
    // The "UI Opacity" setting (hud_globalAlpha, ADVANCED page): the alpha of every HUD element.
    let ui_alpha = settings.as_ref().and_then(|s| s.f32("hud_globalAlpha")).unwrap_or(1.0).clamp(0.0, 1.0);
    rt.batcher.begin();
    for (index, (name, p)) in rt.hud.players().into_iter().enumerate() {
        if !shown {
            break;
        }
        let (fw, fh) = (p.swf.frame_width, p.swf.frame_height);
        // Tag-placed movies are drawn at their frame size onto their panel; the fullscreen one at window size.
        let placed = layout.get(name);
        if placed.is_none() && name != idswf::hud::SCREEN {
            continue;
        }
        let list: DrawList = if placed.is_some() { idswf::render::draw_clipped(p, fw, fh) } else { idswf::render::draw_clipped(p, w, h) };
        rt.batcher.push(&mut cx, &rt.assets, &list, &ImageKey::Atlas(index), placed, (fw, fh), (w, h), ui_alpha);
    }
    if let Some(wm) = rt.wheel.as_mut() {
        wm.update(time.delta_secs_f64());
        if wm.visible() && !menu.as_ref().is_some_and(|m| m.0) {
            let (fw, fh) = (wm.player.swf.frame_width, wm.player.swf.frame_height);
            if let Some(placed) = wheel::panel(&rt.hud, w, h) {
                let list = idswf::render::draw_clipped(&wm.player, fw, fh);
                rt.batcher.push(&mut cx, &rt.assets, &list, &ImageKey::Atlas(WHEEL_ATLAS), Some(placed), (fw, fh), (w, h), ui_alpha);
            }
        }
    }
    rt.batcher.finish(&mut vis);
}

/// Ammo, icon and reticle of the current weapon (the weapon info widget reads the same pool counts).
fn weapon_from_combat(state: &mut HudState, c: &crate::combat::Combat, decls: &DeclDb, visuals: &mut HashMap<String, (Option<String>, Option<String>, i32)>) {
    let a = &c.arsenal;
    let def = a.def();
    let (icon, reticle, low_ammo_count) = visuals
        .entry(def.decl.clone())
        .or_insert_with(|| {
            let (icon, reticle) = idswf::hud::weapon_visuals(decls, &def.decl);
            let low = decls.get("ammo", &def.ammo_decl).ok().and_then(|b| b.f32("edit.lowAmmoWarningCount")).unwrap_or(0.0) as i32;
            (icon, reticle, low)
        })
        .clone();
    // idHudReticleInfo::decl = idWeapon GetReticleDecl 0x140f12c70 (the fire mode's decl: lockedReticle once locked,
    // reticleWhenZoomed, the RETICLE_DECL mod override, else its reticle).
    let reticle = Some(a.reticle_decl(a.current)).filter(|r| !r.is_empty()).or(reticle);
    let pool = a.pools.iter().find(|p| p.key == def.ammo_pool);
    let (ammo, max_ammo) = match (a.ammo_for(a.current), pool) {
        (Some(n), Some(p)) if !def.infinite_ammo => (n, if p.max > 0 { p.max } else { def.ammo_max }),
        (Some(n), None) if !def.infinite_ammo => (n, def.ammo_max),
        _ => (0, 0),
    };
    if state.ammo != ammo || state.max_ammo != max_ammo || state.weapon_icon != icon || state.reticle != reticle {
        state.ammo = ammo;
        state.max_ammo = max_ammo;
        state.weapon_icon = icon;
        state.reticle = reticle;
    }
    state.low_ammo_count = low_ammo_count;
    state.ammo_per_shot = def.ammo_per_shot;
}

/// The idHudInfo weapon mod fields (builder 0x140e023d0) and the idHudReticleInfo charge values (0x140e04520).
fn mods_from_combat(state: &mut HudState, c: &crate::combat::Combat, decls: &DeclDb, perks: &mut HashMap<String, Option<idswf::hud::HudMod>>) {
    let a = &c.arsenal;
    let w = a.current;
    let mut view = |perk: &str| perks.entry(perk.to_string()).or_insert_with(|| idswf::hud::perk_visuals(decls, perk)).clone();
    let (mut active, mut other, mut owned, mut slot) = (None, None, false, false);
    if let (Some(m), Some(l)) = (a.defs[w].mods.as_ref(), a.loadouts.get(w)) {
        // +0xe48 (0x140b8e1d0): the active family's base perk (the testbed's active family is always owned).
        active = l.active.filter(|f| l.owned.get(*f).copied().unwrap_or(false)).and_then(|f| view(&m.families[f].base.name));
        // 0x140b8e440: the first family (the weapon's first perk group) whose base perk is not active; +0xe50 owned.
        if let Some(f) = (0..m.families.len()).find(|f| Some(*f) != l.active) {
            other = view(&m.families[f].base.name);
            owned = l.owned.get(f).copied().unwrap_or(false);
        }
        // +0xe51: INTERIM: the exe also needs a per-player unlock (0x140dd18c0 vslot 0xe0 (4)) and excludes some
        // weapons (player +0x55c40, an item range check 0x140e41f50, player +0xb530); approximated as "the weapon
        // has a perk group".
        slot = true;
    }
    state.weapon_id = Some(w);
    state.weapon_mod = active;
    state.other_mod = other;
    state.other_mod_owned = owned;
    state.mod_slot = slot;
    // charging (+0x16c) = CanCharge (vslot 0x488) ? GetChargePercent (vslot 0x470) : 0; idRailGun overrides both.
    let now = a.time_ms;
    let charging = if a.railgun_def(w).is_some() {
        if a.railgun_can_charge(w) { a.railgun_charge_percent(w, now) } else { 0.0 }
    } else if a.can_charge(w, now) {
        a.mstate[w].charge.percent
    } else {
        0.0
    };
    state.reticle_charge = charging;
    // discharging (+0x170) = vslot 0x480, 0.0 for every weapon class.
    state.reticle_discharge = 0.0;
    // lockFraction (+0x158) = vslot 0x358 (0x140c25e40).
    state.reticle_lock = a.lock_percent(w);
}

/// Box-filtered mip chain (at most 5 levels, so atlas neighbours bleed only under heavy minification).
pub(crate) fn mips(t: &Texture) -> Mips {
    let (mut w, mut h) = (t.width.max(1) as usize, t.height.max(1) as usize);
    let mut level = t.rgba.clone();
    let mut data = level.clone();
    let mut levels = 1;
    while (w > 1 || h > 1) && levels < 5 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                for ch in 0..4 {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (sx, sy) = ((x * 2 + dx).min(w - 1), (y * 2 + dy).min(h - 1));
                        sum += level[(sy * w + sx) * 4 + ch] as u32;
                    }
                    next[(y * nw + x) * 4 + ch] = ((sum + 2) / 4) as u8;
                }
            }
        }
        data.extend_from_slice(&next);
        level = next;
        (w, h) = (nw, nh);
        levels += 1;
    }
    Mips { width: t.width.max(1), height: t.height.max(1), levels, data }
}

/// Linear (not sRGB) RGBA8: the stage programs work on the raw texel values like the engine.
pub(crate) fn image(m: Mips) -> Image {
    let size = Extent3d { width: m.width, height: m.height, depth_or_array_layers: 1 };
    let mut img = Image::new_uninit(size, TextureDimension::D2, TextureFormat::Rgba8Unorm, RenderAssetUsages::RENDER_WORLD);
    img.texture_descriptor.mip_level_count = m.levels;
    img.data = Some(m.data);
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}
