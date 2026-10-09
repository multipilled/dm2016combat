//! DOOM (2016)'s HDR post chain as a Bevy post-process: the `RenderPostProcess` render job
//! (0x14186a7f0) from the HDR scene to the displayed image, with the parameters the engine computes on
//! the CPU from cvars and the level's env decl renderparms. Decode notes: gamedata/re/POST.md.
//!
//! Passes, per frame (r_renderMode 0):
//! 1. half-resolution copy of the scene (DOWNSAMPLE, 5 taps on a 30-degree grid);
//! 2. luminance: half -> 128^2 (luma, clamp 65535) -> 64 .. 2 (`_autoExposure6..0`), then
//!    AUTO_EXPOSURE (Krawczyk scene key over the 2 x 2 average, clamped to autoExposureMin/Max,
//!    blended with last frame's value);
//! 3. bloom: half -> 1/4 (bright pass, r_hdrBloomThreshold) -> 1/8 .. 1/64, then from the coarsest level
//!    up a separable 15-tap gaussian per level, weighted by bloomWeight1..5 and added to the coarser result;
//! 4. POST_PROCESS: chromatic aberration, bloom + lens dirt composite, exposure, vignette, three-zone colour
//!    correction, Hable filmic curve with white point, exact sRGB, ordered dither, contrast -> 8-bit;
//! 5. VIEW_COLOR_UPSAMPLE: B-spline bicubic unsharp mask (r_sharpening), user gamma, film grain.
//!
//! The view models are drawn into the same HDR scene buffer as the world before this chain (the engine
//! has one view; its hands are ordinary surfaces of it), so they share the exposure and count towards
//! the luminance average. In Bevy: give both cameras [`hdr_camera`] (same HDR main texture, no Bevy
//! tonemapping, scene radiance in engine units) and put [`DoomPost`] on the camera that draws last.
//!
//! Not modelled yet: lens flares ($tex3 black), heat distortion, game screen effects
//! (APPLY_GAME_EFFECTS), env blending between volumes (envBlendTime), TAA / motion blur / DOF before
//! the chain, r_renderMode 1-3, colour-blind modes.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::sync::Mutex;

use bevy::asset::{RenderAssetUsages, load_internal_asset, uuid_handle};
use bevy::camera::{Exposure, Hdr};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping, tonemapping};
use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer_sized};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{GpuImage, TextureCache};
use bevy::render::view::ViewTarget;
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use idres::Container;
use rancher_sim::config::CvarValues;

const POST_SHADER: Handle<Shader> = uuid_handle!("9d3e2c71-4b5a-4f0e-8c21-6a7f1e0b9d55");

/// `Exposure::ev100` that makes Bevy's `view.exposure` 1: scene radiance stays in the engine's units
/// (lightmap x lightMapScale x envLightmapScale x albedo) and the engine's auto exposure does the rest.
pub const ENGINE_EV100: f32 = -0.263_034_4;

/// What `view.exposure` was under Bevy's default camera exposure (`Exposure::BLENDER`, ev100 9.7):
/// multiply stand-in Bevy light intensities (lux, ambient brightness) by this to keep their old look.
pub const BEVY_DEFAULT_EXPOSURE: f32 = 0.000_961_9;

/// Components every camera drawing into the engine's HDR scene needs (world and view models alike, so
/// they share one HDR main texture): HDR, no Bevy tonemapping or deband, engine radiance units.
pub fn hdr_camera() -> impl Bundle {
    (Hdr, Tonemapping::None, DebandDither::Disabled, Exposure { ev100: ENGINE_EV100 })
}

/// Marks the camera whose frame gets the post chain: the one that draws last into the shared HDR
/// scene (the view-model camera), so world and hands are exposed and tonemapped together.
#[derive(Component, Clone, Copy, Default, ExtractComponent)]
pub struct DoomPost;

/// One colour-correction zone (`colorCorrection{Master,Shadows,Midtones,Highlights}*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    pub saturation: f32,
    pub gamma: f32,
    pub gain: f32,
    pub offset: f32,
    pub color: [f32; 3],
}

const NEUTRAL: Zone = Zone { saturation: 1.0, gamma: 1.0, gain: 1.0, offset: 0.0, color: [1.0; 3] };

/// The env decl renderparms the post chain reads (`generated/decls/env/<name>.decl`, `renderParms`).
#[derive(Clone, Debug, PartialEq)]
pub struct PostEnv {
    pub auto_exposure_min: f32,
    pub auto_exposure_max: f32,
    pub filmic_shadows: f32,
    pub filmic_midtones: f32,
    pub filmic_highlights: f32,
    pub filmic_white_point: f32,
    pub hdr_saturation: f32,
    /// HDRColorFilter (an `srgba` renderparm: linearised).
    pub hdr_color_filter: [f32; 3],
    pub chromatic_aberration_scale: f32,
    pub chromatic_aberration_curve: f32,
    pub vignette_scale: f32,
    pub vignette_curve: f32,
    pub vignette_color: [f32; 3],
    /// bloomWeight0..5 (0 scales r_hdrBloomRatio, 1..5 the blur levels).
    pub bloom_weight: [f32; 6],
    /// bloomScale1..5 (index 0 unused): anamorphic stretch of a level's blur.
    pub bloom_scale: [f32; 6],
    pub film_grain: f32,
    pub shadow_midtone_start: f32,
    pub shadow_midtone_end: f32,
    pub midtone_highlight_start: f32,
    pub midtone_highlight_end: f32,
    pub master: Zone,
    pub shadows: Zone,
    pub midtones: Zone,
    pub highlights: Zone,
}

impl Default for PostEnv {
    /// generated/decls/env/default.decl.
    fn default() -> Self {
        PostEnv {
            auto_exposure_min: 1.0,
            auto_exposure_max: 16.0,
            filmic_shadows: 1.0,
            filmic_midtones: 1.0,
            filmic_highlights: 1.0,
            filmic_white_point: 1.0,
            hdr_saturation: 1.0,
            hdr_color_filter: [1.0; 3],
            chromatic_aberration_scale: 0.0,
            chromatic_aberration_curve: 0.0,
            vignette_scale: 0.0,
            vignette_curve: 1.0,
            vignette_color: [0.0; 3],
            bloom_weight: [1.0; 6],
            bloom_scale: [0.0; 6],
            film_grain: 0.014,
            shadow_midtone_start: 0.3,
            shadow_midtone_end: 0.4,
            midtone_highlight_start: 0.6,
            midtone_highlight_end: 0.7,
            master: NEUTRAL,
            shadows: NEUTRAL,
            midtones: NEUTRAL,
            highlights: NEUTRAL,
        }
    }
}

/// `renderParms` of one env decl: lower-cased key -> values, and the env it inherits.
fn env_parms(c: &Container, name: &str) -> Option<(HashMap<String, Vec<f32>>, Option<String>)> {
    let text = c.read_by_name(&format!("generated/decls/env/{name}.decl")).ok()?;
    let text = String::from_utf8_lossy(&text);
    let mut toks = Vec::new();
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() || matches!(ch, '{' | '}' | ',') {
            if !word.is_empty() {
                toks.push(std::mem::take(&mut word));
            }
            if matches!(ch, '{' | '}') {
                toks.push(ch.to_string());
            }
        } else {
            word.push(ch);
        }
    }
    let mut parms = HashMap::new();
    let mut inherit = None;
    let mut i = 0;
    while i < toks.len() {
        match toks[i].as_str() {
            "inherit" if toks.get(i + 1).map(String::as_str) == Some("{") => {
                inherit = toks.get(i + 2).filter(|t| *t != "}").map(|t| t.trim_matches('"').to_string());
                i += 3;
            }
            t if t.eq_ignore_ascii_case("renderParms") && toks.get(i + 1).map(String::as_str) == Some("{") => {
                i += 2;
                while i < toks.len() && toks[i] != "}" {
                    let key = toks[i].to_ascii_lowercase();
                    i += 1;
                    let mut vals = Vec::new();
                    if toks.get(i).map(String::as_str) == Some("{") {
                        i += 1;
                        while i < toks.len() && toks[i] != "}" {
                            vals.extend(toks[i].parse::<f32>().ok());
                            i += 1;
                        }
                        i += 1;
                    } else if let Some(v) = toks.get(i).and_then(|t| t.parse::<f32>().ok()) {
                        vals.push(v);
                        i += 1;
                    }
                    parms.insert(key, vals);
                }
            }
            _ => i += 1,
        }
    }
    Some((parms, inherit))
}

fn srgb_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { (v / 1.055 + 0.052_132_7).powf(2.4) }
}

impl PostEnv {
    /// Env `name` (e.g. the worldspawn's `edit.envSettings`, `tgarza/intro_crash_int`) over its inherit
    /// chain, over the `default` env; None gives the `default` env.
    pub fn load(c: &Container, name: Option<&str>) -> PostEnv {
        let mut chain = Vec::new();
        let mut next = name.map(str::to_string);
        while let Some(n) = next.take() {
            if chain.len() >= 8 || n == "default" {
                break;
            }
            let Some((parms, inherit)) = env_parms(c, &n) else { break };
            chain.push(parms);
            next = inherit;
        }
        chain.extend(env_parms(c, "default").map(|e| e.0));
        let mut env = PostEnv::default();
        for parms in chain.iter().rev() {
            for (k, v) in parms {
                env.set(k, v);
            }
        }
        env
    }

    fn set(&mut self, key: &str, v: &[f32]) {
        let Some(&x) = v.first() else { return };
        let rgb = || [x, v.get(1).copied().unwrap_or(x), v.get(2).copied().unwrap_or(x)];
        let zone = |z: &mut Zone, field: &str| match field {
            "saturation" => z.saturation = x,
            "gamma" => z.gamma = x,
            "gain" => z.gain = x,
            "offset" => z.offset = x,
            "color" => z.color = rgb(),
            _ => {}
        };
        match key {
            "autoexposuremin" => self.auto_exposure_min = x,
            "autoexposuremax" => self.auto_exposure_max = x,
            "filmiccurveshadowsscale" => self.filmic_shadows = x,
            "filmiccurvemidtonesscale" => self.filmic_midtones = x,
            "filmiccurvehighlightsscale" => self.filmic_highlights = x,
            "filmiccurvewhitepoint" => self.filmic_white_point = x,
            "hdrsaturation" => self.hdr_saturation = x,
            "hdrcolorfilter" => self.hdr_color_filter = rgb().map(srgb_linear),
            "chromaticaberrationscale" => self.chromatic_aberration_scale = x,
            "chromaticaberrationcurve" => self.chromatic_aberration_curve = x,
            "vignettescale" => self.vignette_scale = x,
            "vignettecurve" => self.vignette_curve = x,
            "vignettecolor" => self.vignette_color = rgb(),
            "filmgrain" => self.film_grain = x,
            "colorcorrectionshadowmidtonestart" => self.shadow_midtone_start = x,
            "colorcorrectionshadowmidtoneend" => self.shadow_midtone_end = x,
            "colorcorrectionmidtonehighlightstart" => self.midtone_highlight_start = x,
            "colorcorrectionmidtonehighlightend" => self.midtone_highlight_end = x,
            _ => {
                if let Some(i) = key.strip_prefix("bloomweight").and_then(|i| i.parse::<usize>().ok()).filter(|&i| i < 6) {
                    self.bloom_weight[i] = x;
                } else if let Some(i) = key.strip_prefix("bloomscale").and_then(|i| i.parse::<usize>().ok()).filter(|&i| (1..6).contains(&i)) {
                    self.bloom_scale[i] = x;
                } else if let Some(rest) = key.strip_prefix("colorcorrection") {
                    for (prefix, z) in [("master", &mut self.master), ("shadows", &mut self.shadows), ("midtones", &mut self.midtones), ("highlights", &mut self.highlights)] {
                        if let Some(field) = rest.strip_prefix(prefix) {
                            zone(z, field);
                        }
                    }
                }
            }
        }
    }
}

/// The cvars the chain reads (exe defaults with the shipped configs applied).
#[derive(Clone, Debug, PartialEq)]
pub struct PostCvars {
    pub auto_exposure_base: f32,
    pub auto_exposure_ratio: f32,
    pub auto_exposure_speed: f32,
    pub bloom: bool,
    pub bloom_ratio: f32,
    pub bloom_threshold: f32,
    pub lens_dirt_ratio: f32,
    pub lens_flares_ratio: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub gamma: f32,
    pub chromatic_aberration: i32,
    pub chromatic_aberration_limit: f32,
    pub vignette: bool,
    pub color_correction: i32,
    pub sharpening: f32,
    pub film_grain_ratio: f32,
}

impl Default for PostCvars {
    /// The exe's defaults.
    fn default() -> Self {
        PostCvars {
            auto_exposure_base: 0.01,
            auto_exposure_ratio: 1.0,
            auto_exposure_speed: 1.0,
            bloom: true,
            bloom_ratio: 0.03,
            bloom_threshold: 0.0,
            lens_dirt_ratio: 1.0,
            lens_flares_ratio: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
            chromatic_aberration: 1,
            chromatic_aberration_limit: 1.0,
            vignette: true,
            color_correction: 1,
            sharpening: 2.0,
            film_grain_ratio: 1.0,
        }
    }
}

impl PostCvars {
    pub fn from_cvars(c: &CvarValues) -> Self {
        let d = PostCvars::default();
        let f = |name: &str, def: f32| c.0.get(name).and_then(|v| v.trim_end_matches('f').parse::<f32>().ok()).unwrap_or(def);
        PostCvars {
            auto_exposure_base: f("r_hdrAutoExposureBase", d.auto_exposure_base),
            auto_exposure_ratio: f("r_hdrAutoExposureRatio", d.auto_exposure_ratio),
            auto_exposure_speed: f("r_hdrAutoExposureSpeed", d.auto_exposure_speed),
            bloom: f("r_hdrBloom", 1.0) != 0.0,
            bloom_ratio: f("r_hdrBloomRatio", d.bloom_ratio),
            bloom_threshold: f("r_hdrBloomThreshold", d.bloom_threshold),
            lens_dirt_ratio: f("r_lensDirtRatio", d.lens_dirt_ratio),
            lens_flares_ratio: f("r_lensFlaresRatio", d.lens_flares_ratio),
            contrast: f("r_contrast", d.contrast),
            saturation: f("r_saturation", d.saturation),
            gamma: f("r_gamma", d.gamma),
            chromatic_aberration: f("r_chromaticAberration", 1.0) as i32,
            chromatic_aberration_limit: f("r_chromaticAberrationLimit", d.chromatic_aberration_limit),
            vignette: f("r_vignette", 1.0) != 0.0,
            color_correction: f("r_colorCorrection", 1.0) as i32,
            sharpening: f("r_sharpening", d.sharpening),
            film_grain_ratio: f("r_filmGrainRatio", d.film_grain_ratio),
        }
    }
}

/// Everything the chain reads; change `env` when the level's env changes.
#[derive(Resource, Clone, ExtractResource)]
pub struct PostSettings {
    pub env: PostEnv,
    pub cvars: PostCvars,
    /// $postExposureControl.x (renderparm default 1; the game's exposure fades drive it).
    pub post_exposure_control: f32,
    /// $bloomDustMap (textures/system/lens_dirt.tga); None until loaded, then black if missing.
    pub lens_dirt: Option<Handle<Image>>,
}

/// Frame time for the exposure adaptation and the film grain seed.
#[derive(Resource, Clone, Default, ExtractResource)]
struct PostFrame {
    dt: f32,
    frame: u32,
}

#[derive(Resource)]
struct PendingDirt(Option<Image>);

/// The renderparm bloomDustMap: `{ Tex2D bc7 textures/system/lens_dirt.tga }`.
fn load_lens_dirt(c: &Container) -> Option<Image> {
    let bytes = c.read_by_name("generated/image/textures/system/lens_dirt.tga$bc7.bimage").ok()?;
    let img = idres::bimage::BImage::parse(&bytes).ok()?;
    if img.format != 23 {
        return None;
    }
    let mut mips: Vec<&idres::bimage::Mip> = img.mips.iter().filter(|m| m.dest_z == 0).collect();
    mips.sort_by_key(|m| m.level);
    let mut data = Vec::new();
    let mut levels = 0;
    for m in mips {
        let (w, h) = ((img.width >> m.level).max(1), (img.height >> m.level).max(1));
        let need = (w.div_ceil(4) * h.div_ceil(4) * 16) as usize;
        if m.level != levels || m.data.len() < need {
            break;
        }
        data.extend_from_slice(&bytes[m.data.start..m.data.start + need]);
        levels += 1;
    }
    if levels == 0 {
        return None;
    }
    let mut image = Image::new(Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 }, TextureDimension::D2, data, TextureFormat::Bc7RgbaUnorm, RenderAssetUsages::RENDER_WORLD);
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

/// Adds the post chain. Build it from the install's decls and cvars; `env` is the level's env decl
/// (worldspawn `edit.envSettings`, see `idfx::env::map_env`), None for the `default` env.
pub struct PostPlugin {
    settings: PostSettings,
    dirt: Mutex<Option<Image>>,
}

impl PostPlugin {
    pub fn new(c: &Container, cvars: &CvarValues, env: Option<&str>) -> Self {
        let settings = PostSettings { env: PostEnv::load(c, env), cvars: PostCvars::from_cvars(cvars), post_exposure_control: 1.0, lens_dirt: None };
        PostPlugin { settings, dirt: Mutex::new(load_lens_dirt(c)) }
    }
}

impl Plugin for PostPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, POST_SHADER, "post.wgsl", Shader::from_wgsl);
        app.insert_resource(self.settings.clone())
            .insert_resource(PendingDirt(self.dirt.lock().unwrap().take()))
            .init_resource::<PostFrame>()
            .add_plugins((ExtractComponentPlugin::<DoomPost>::default(), ExtractResourcePlugin::<PostSettings>::default(), ExtractResourcePlugin::<PostFrame>::default()))
            .add_systems(Startup, add_lens_dirt)
            .add_systems(First, tick_frame);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<PostStates>()
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(Render, prepare.in_set(RenderSystems::PrepareResources))
            .add_systems(Core3d, doom_post.before(tonemapping).in_set(Core3dSystems::PostProcess));
    }
}

fn add_lens_dirt(mut pending: ResMut<PendingDirt>, mut images: ResMut<Assets<Image>>, mut settings: ResMut<PostSettings>) {
    match pending.0.take() {
        Some(img) => settings.lens_dirt = Some(images.add(img)),
        None => eprintln!("post: lens dirt texture not found; no lens dirt"),
    }
}

fn tick_frame(time: Res<Time>, mut frame: ResMut<PostFrame>) {
    frame.dt = time.delta_secs();
    frame.frame = frame.frame.wrapping_add(1);
}

// ---------------------------------------------------------------------------------------------------
// CPU side of the chain: the renderparm values the engine sets (0x1418682c0 auto exposure,
// 0x141868660 bloom, 0x141869350 downsample, 0x14186ae50 blur, 0x14186c7b0 post process,
// 0x14186f5da upsample), r_renderMode 0.

const FLT_EPSILON: f32 = 1.192_092_9e-7;

/// The POST_PROCESS renderparms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneParms {
    pub tone_map: [f32; 4],
    pub chromatic_aberration_vignette: [f32; 4],
    pub color_correction: [f32; 4],
    pub range: [f32; 4],
    pub saturation: [f32; 4],
    pub gamma: [f32; 4],
    pub shadow_scale: [f32; 4],
    pub midtone_scale: [f32; 4],
    pub highlight_scale: [f32; 4],
    pub curve0: [f32; 4],
    pub curve1: [f32; 4],
}

/// Hable's curve with the engine's shoulder (A), linear (B) and toe (E) scales; C 0.1, D 0.2, F 0.3.
fn hable(x: f32, a: f32, b: f32, e: f32) -> f32 {
    (x * (a * x + b * 0.1) + 0.2 * e) / (x * (a * x + b) + 0.2 * 0.3) - e / 0.3
}

/// 0x14186c7b0 for a `width` x `height` view.
pub fn tone_parms(s: &PostSettings, width: u32, height: u32) -> ToneParms {
    let (env, cv) = (&s.env, &s.cvars);
    // bloom: bloomWeight0 (0x141868660's return) * r_hdrBloomRatio, energy conserving split
    let b = if cv.bloom { (env.bloom_weight[0] * cv.bloom_ratio).clamp(0.0, 1.0) } else { 0.0 };
    let tone_map = [1.0 / (1.0 + b), b / (1.0 + b), height as f32 / width as f32, cv.lens_dirt_ratio * b];

    let ca = if cv.chromatic_aberration == 1 { env.chromatic_aberration_scale } else { 0.0 };
    let ca = cv.chromatic_aberration_limit.min(ca);
    let vignette = if cv.vignette { env.vignette_scale } else { 0.0 };
    let chromatic_aberration_vignette = [
        (1.0 - (1.0 - ca).powf(2.0)) * 100.0 / width as f32,
        1.0 / FLT_EPSILON.max(1.0 - env.chromatic_aberration_curve),
        vignette,
        1.0 / FLT_EPSILON.max(1.0 - env.vignette_curve),
    ];
    let color_correction = [(cv.color_correction * 2 - 1) as f32, 0.0, cv.lens_flares_ratio, cv.contrast];

    // zone boundaries, each at least FLT_EPSILON above the previous
    let sms = env.shadow_midtone_start;
    let sme = env.shadow_midtone_end.max(sms + FLT_EPSILON);
    let mhs = env.midtone_highlight_start.max(sme + FLT_EPSILON);
    let mhe = env.midtone_highlight_end.max(mhs + FLT_EPSILON);
    let range = [1.0 / (sme - sms), -(sms / (sme - sms)), 1.0 / (mhe - mhs), -(mhs / (mhe - mhs))];

    let m = &env.master;
    let sat = env.hdr_saturation * cv.saturation * m.saturation;
    let base: [f32; 3] = std::array::from_fn(|i| env.hdr_color_filter[i] * m.color[i]);
    // per zone: saturation, 1 / gamma, gain * colour, offset
    let zone = |z: &Zone| -> (f32, f32, [f32; 3], f32) {
        let gain = z.gain * m.gain;
        (z.saturation * sat, 1.0 / (z.gamma * m.gamma), std::array::from_fn(|i| gain * base[i] * z.color[i]), z.offset + m.offset)
    };
    let (s_sat, s_gamma, s_col, s_off) = zone(&env.shadows);
    let (m_sat, m_gamma, m_col, m_off) = zone(&env.midtones);
    let (h_sat, h_gamma, h_col, h_off) = zone(&env.highlights);
    let shadow_scale = [s_col[0], s_col[1], s_col[2], s_off];
    let midtone_scale = [m_col[0] - s_col[0], m_col[1] - s_col[1], m_col[2] - s_col[2], m_off - s_off];
    let highlight_scale = [h_col[0] - m_col[0], h_col[1] - m_col[1], h_col[2] - m_col[2], h_off - m_off];
    let saturation = [s_sat, m_sat - s_sat, h_sat - m_sat, 0.0];
    let gamma = [s_gamma, m_gamma - s_gamma, h_gamma - m_gamma, 0.0];

    // filmic curve: A = 0.22 * highlights, B = 0.3 * midtones, E = 0.01 * shadows; white point scale
    let (a, bb, e) = (0.22 * env.filmic_highlights, 0.3 * env.filmic_midtones, 0.01 * env.filmic_shadows);
    let w = env.filmic_white_point;
    let curve0 = [a, bb, e, e / 0.3];
    let curve1 = [bb * 0.1, 0.2 * e, 1.0 / hable(w, a, bb, e), 0.0];
    ToneParms { tone_map, chromatic_aberration_vignette, color_correction, range, saturation, gamma, shadow_scale, midtone_scale, highlight_scale, curve0, curve1 }
}

/// $autoExposureParms for a frame of `dt` seconds: (base, ratio, 0, adaptation). The adaptation is
/// 1 - exp(-r_hdrAutoExposureSpeed * frame time), the frame time in 60 Hz frames ("1.0 = 16.5 ms").
/// INFERRED: the engine's frame-time operand (post job +0x1a4) was not traced to its writer.
pub fn auto_exposure_parms(s: &PostSettings, dt: f32) -> [f32; 4] {
    let cv = &s.cvars;
    [cv.auto_exposure_base, cv.auto_exposure_ratio, 0.0, 1.0 - (-(cv.auto_exposure_speed * dt * 60.0)).exp()]
}

// ---------------------------------------------------------------------------------------------------
// Render world.

/// Uniform block of one pass: 16 vec4s, one 256-byte dynamic-offset slot.
type Pass = [[f32; 4]; 16];
const PASS_BYTES: u64 = 256;
const LUM_LEVELS: usize = 7;
const BLOOM_LEVELS: usize = 5;
/// Clamp of the luminance downsample ($downsampleType.z, r_renderMode 0).
const MAX_LUMA: f32 = 65535.0;
const BLOOM_FORMAT: TextureFormat = TextureFormat::Rg11b10Ufloat;
const LUMA_FORMAT: TextureFormat = TextureFormat::R16Float;
const VIEW_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;
/// Bevy's HDR main texture (`Hdr` cameras).
const HDR_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

// pass slots in the uniform buffer
const P_HALF: usize = 0;
const P_LUM: usize = 1; // .. P_LUM + LUM_LEVELS - 1: half -> lum6, lum6 -> lum5 .. lum1 -> lum0
const P_EXPOSURE: usize = P_LUM + LUM_LEVELS;
const P_BLOOM_DOWN: usize = P_EXPOSURE + 1; // half -> L1, L1 -> L2 .. L4 -> L5
const P_BLUR: usize = P_BLOOM_DOWN + BLOOM_LEVELS; // per level 5..1: horizontal, vertical
const P_POST: usize = P_BLUR + 2 * BLOOM_LEVELS;
const P_UPSAMPLE: usize = P_POST + 1;
const PASSES: usize = P_UPSAMPLE + 1;

#[derive(Resource)]
struct PostPipelines {
    layout: BindGroupLayoutDescriptor,
    clamp: Sampler,
    wrap: Sampler,
    black: TextureView,
    downsample_bloom: CachedRenderPipelineId,
    downsample_luma: CachedRenderPipelineId,
    exposure: CachedRenderPipelineId,
    blur: CachedRenderPipelineId,
    post: CachedRenderPipelineId,
    upsample: CachedRenderPipelineId,
}

fn init_pipelines(mut commands: Commands, device: Res<RenderDevice>, queue: Res<RenderQueue>, fullscreen: Res<FullscreenShader>, cache: Res<PipelineCache>) {
    let tex = || texture_2d(TextureSampleType::Float { filterable: true });
    let layout = BindGroupLayoutDescriptor::new(
        "doom_post_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (tex(), tex(), tex(), tex(), sampler(SamplerBindingType::Filtering), sampler(SamplerBindingType::Filtering), uniform_buffer_sized(true, NonZeroU64::new(PASS_BYTES))),
        ),
    );
    let linear = |mode: AddressMode| SamplerDescriptor {
        label: Some("doom_post_sampler"),
        address_mode_u: mode,
        address_mode_v: mode,
        address_mode_w: mode,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..default()
    };
    let clamp = device.create_sampler(&linear(AddressMode::ClampToEdge));
    let wrap = device.create_sampler(&linear(AddressMode::Repeat));
    let black = device
        .create_texture_with_data(
            &queue,
            &TextureDescriptor {
                label: Some("doom_post_black"),
                size: Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            TextureDataOrder::LayerMajor,
            &[0, 0, 0, 0],
        )
        .create_view(&TextureViewDescriptor::default());
    let pipeline = |entry: &'static str, format: TextureFormat| {
        cache.queue_render_pipeline(RenderPipelineDescriptor {
            label: Some(format!("doom_post_{entry}").into()),
            layout: vec![layout.clone()],
            vertex: fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: POST_SHADER,
                shader_defs: Vec::new(),
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
            }),
            ..default()
        })
    };
    commands.insert_resource(PostPipelines {
        downsample_bloom: pipeline("downsample", BLOOM_FORMAT),
        downsample_luma: pipeline("downsample", LUMA_FORMAT),
        exposure: pipeline("auto_exposure", LUMA_FORMAT),
        blur: pipeline("blur", BLOOM_FORMAT),
        post: pipeline("post_process", VIEW_FORMAT),
        upsample: pipeline("upsample", HDR_FORMAT),
        layout,
        clamp,
        wrap,
        black,
    });
}

/// Per-view state kept across frames: the exposure (last frame's and this frame's, 1 x 1) and the
/// pass uniforms.
struct ViewState {
    exposure: [TextureView; 2],
    current: usize,
    params: Buffer,
}

#[derive(Resource, Default)]
struct PostStates(HashMap<Entity, ViewState>);

/// This frame's textures and uniforms of a view.
#[derive(Component)]
struct PostViewData {
    half: TextureView,
    lum: Vec<TextureView>,
    /// bloom levels 1..5 (index 0 = level 1) and their blur temporaries
    bloom: Vec<TextureView>,
    bloom_tmp: Vec<TextureView>,
    view: TextureView,
    exposure_prev: TextureView,
    exposure: TextureView,
    params: Buffer,
}

fn transient(cache: &mut TextureCache, device: &RenderDevice, label: &'static str, size: UVec2, format: TextureFormat) -> TextureView {
    cache
        .get(
            device,
            TextureDescriptor {
                label: Some(label),
                size: Extent3d { width: size.x.max(1), height: size.y.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
        .default_view
}

fn size4(s: UVec2) -> [f32; 4] {
    let s = s.max(UVec2::ONE).as_vec2();
    [s.x, s.y, 1.0 / s.x, 1.0 / s.y]
}

fn downsample_pass(dtype: [f32; 4], src: UVec2, dst: UVec2) -> Pass {
    let mut p = [[0.0; 4]; 16];
    p[0] = dtype;
    p[1] = size4(src);
    let d = size4(dst);
    p[2] = [d[2], d[3], 1.0, 1.0];
    p
}

fn blur_pass(d: f32, weight: f32, dst: UVec2) -> Pass {
    let mut p = [[0.0; 4]; 16];
    p[0] = [d.max(0.0), (-d).max(0.0), weight, 0.0];
    let s = size4(dst);
    p[1] = [s[2], s[3], 0.0, 0.0];
    p
}

fn shr(s: UVec2, n: u32) -> UVec2 {
    UVec2::new(s.x >> n, s.y >> n).max(UVec2::ONE)
}

/// All pass uniforms of a frame.
fn passes(s: &PostSettings, f: &PostFrame, size: UVec2) -> Vec<Pass> {
    let (env, cv) = (&s.env, &s.cvars);
    let half = shr(size, 1);
    let lum_size = |i: usize| UVec2::splat(2 << i);
    let bloom_size = |level: usize| shr(size, level as u32 + 1);
    let mut out = vec![[[0.0f32; 4]; 16]; PASSES];
    // RenderPostProcess: scene -> _viewColorScaled01 (both resolution-scaled)
    out[P_HALF] = downsample_pass([0.0, 1.0, MAX_LUMA, 0.0], size, half);
    // 0x1418682c0: luminance of the half-resolution scene, then down to 2 x 2
    out[P_LUM] = downsample_pass([1.0, 1.0, MAX_LUMA, 0.0], half, lum_size(LUM_LEVELS - 1));
    for k in 1..LUM_LEVELS {
        let i = LUM_LEVELS - 1 - k;
        out[P_LUM + k] = downsample_pass([0.0, 0.0, MAX_LUMA, 0.0], lum_size(i + 1), lum_size(i));
    }
    let mut e = [[0.0; 4]; 16];
    e[0] = auto_exposure_parms(s, f.dt);
    e[1] = [env.auto_exposure_min, env.auto_exposure_max, 1.0, 1.0];
    e[2] = [0.0, 0.0, MAX_LUMA, 0.0];
    out[P_EXPOSURE] = e;
    // 0x141868660: bright pass into level 1, then down to level 5
    out[P_BLOOM_DOWN] = downsample_pass([2.0, 1.0, MAX_LUMA, cv.bloom_threshold], half, bloom_size(1));
    for level in 1..BLOOM_LEVELS {
        out[P_BLOOM_DOWN + level] = downsample_pass([0.0, 0.0, MAX_LUMA, 0.0], bloom_size(level), bloom_size(level + 1));
    }
    for (k, level) in (1..=BLOOM_LEVELS).rev().enumerate() {
        let scale = env.bloom_scale[level];
        out[P_BLUR + 2 * k] = blur_pass(scale + 1.0, 1.0, bloom_size(level));
        out[P_BLUR + 2 * k + 1] = blur_pass(scale - 1.0, env.bloom_weight[level], bloom_size(level));
    }
    // 0x14186c7b0
    let t = tone_parms(s, size.x, size.y);
    let sz = size4(size);
    let mut p = [[0.0; 4]; 16];
    p[0] = [sz[2], sz[3], 1.0, 1.0];
    p[1] = t.chromatic_aberration_vignette;
    p[2] = t.tone_map;
    p[3] = t.color_correction;
    p[4] = [1.0; 4];
    p[5] = [env.vignette_color[0], env.vignette_color[1], env.vignette_color[2], 0.0];
    p[6] = t.range;
    p[7] = t.saturation;
    p[8] = t.gamma;
    p[9] = t.shadow_scale;
    p[10] = t.midtone_scale;
    p[11] = t.highlight_scale;
    p[12] = t.curve0;
    p[13] = t.curve1;
    // the postprocess vertex program: exposure *= max(0, $postExposureControl.x * 8 - 7)
    p[14] = [(s.post_exposure_control * 8.0 - 7.0).max(0.0), 0.0, 0.0, 0.0];
    out[P_POST] = p;
    // 0x14186f5da: resolutionScale.w = clamp(r_sharpening, 0, 4), film grain = filmGrain * min(4, r_filmGrainRatio)
    let mut u = [[0.0; 4]; 16];
    u[0] = [1.0, 1.0, 1.0, cv.sharpening.clamp(0.0, 4.0)];
    u[1] = sz;
    u[2] = [0.0, 0.0, 0.0, env.film_grain * cv.film_grain_ratio.min(4.0)];
    let r = f.frame.wrapping_mul(0x9E37_79B9) ^ 0x5bd1_e995;
    u[3] = [(r & 0xffff) as f32, (r >> 16) as f32, 1.0 / cv.gamma, 0.0];
    out[P_UPSAMPLE] = u;
    out
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    mut commands: Commands,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut cache: ResMut<TextureCache>,
    mut states: ResMut<PostStates>,
    settings: Option<Res<PostSettings>>,
    frame: Option<Res<PostFrame>>,
    views: Query<(Entity, &ExtractedCamera), With<DoomPost>>,
) {
    let (Some(settings), Some(frame)) = (settings, frame) else { return };
    let mut seen = HashSet::new();
    for (entity, camera) in &views {
        let Some(size) = camera.physical_target_size.filter(|s| s.x > 0 && s.y > 0) else { continue };
        if !camera.hdr {
            continue;
        }
        seen.insert(entity);
        let state = states.0.entry(entity).or_insert_with(|| {
            let exposure = std::array::from_fn(|_| {
                device
                    .create_texture(&TextureDescriptor {
                        label: Some("doom_post_exposure"),
                        size: Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: TextureDimension::D2,
                        format: LUMA_FORMAT,
                        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&TextureViewDescriptor::default())
            });
            let params = device.create_buffer(&BufferDescriptor {
                label: Some("doom_post_params"),
                size: PASS_BYTES * PASSES as u64,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            ViewState { exposure, current: 0, params }
        });
        state.current ^= 1;
        let bytes: Vec<u8> = passes(&settings, &frame, size).iter().flat_map(|p| p.iter().flatten().flat_map(|v| v.to_le_bytes())).collect();
        queue.write_buffer(&state.params, 0, &bytes);

        let half = shr(size, 1);
        let lum = (0..LUM_LEVELS).map(|i| transient(&mut cache, &device, "doom_post_luminance", UVec2::splat(2 << i), LUMA_FORMAT)).collect();
        let bloom = (1..=BLOOM_LEVELS).map(|l| transient(&mut cache, &device, "doom_post_bloom", shr(size, l as u32 + 1), BLOOM_FORMAT)).collect();
        let bloom_tmp = (1..=BLOOM_LEVELS).map(|l| transient(&mut cache, &device, "doom_post_bloom_tmp", shr(size, l as u32 + 1), BLOOM_FORMAT)).collect();
        commands.entity(entity).insert(PostViewData {
            half: transient(&mut cache, &device, "doom_post_half", half, BLOOM_FORMAT),
            lum,
            bloom,
            bloom_tmp,
            view: transient(&mut cache, &device, "doom_post_view", size, VIEW_FORMAT),
            exposure_prev: state.exposure[state.current ^ 1].clone(),
            exposure: state.exposure[state.current].clone(),
            params: state.params.clone(),
        });
    }
    states.0.retain(|e, _| seen.contains(e));
}

fn doom_post(
    view: ViewQuery<(&ViewTarget, &PostViewData)>,
    pipes: Option<Res<PostPipelines>>,
    cache: Res<PipelineCache>,
    settings: Option<Res<PostSettings>>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (Some(pipes), Some(settings)) = (pipes, settings) else { return };
    let (target, data) = view.into_inner();
    if target.main_texture_format() != HDR_FORMAT {
        return;
    }
    let ids = [pipes.downsample_bloom, pipes.downsample_luma, pipes.exposure, pipes.blur, pipes.post, pipes.upsample];
    let Some(p) = ids.iter().map(|&id| cache.get_render_pipeline(id)).collect::<Option<Vec<_>>>() else { return };
    let (down_bloom, down_luma, exposure, blur, post_pipe, upsample) = (p[0], p[1], p[2], p[3], p[4], p[5]);
    let dirt = settings.lens_dirt.as_ref().and_then(|h| images.get(h)).map_or(&pipes.black, |g| &g.texture_view);
    let layout = cache.get_bind_group_layout(&pipes.layout);
    let post = target.post_process_write();
    let params = BufferBinding { buffer: &data.params, offset: 0, size: NonZeroU64::new(PASS_BYTES) };

    let encoder_pass = |ctx: &mut RenderContext, label: &'static str, pipeline: &RenderPipeline, dst: &TextureView, slot: usize, tex: [&TextureView; 4]| {
        let bind_group = ctx.render_device().create_bind_group(
            "doom_post_bind_group",
            &layout,
            &BindGroupEntries::sequential((tex[0], tex[1], tex[2], tex[3], &pipes.clamp, &pipes.wrap, params.clone())),
        );
        let mut pass = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(RenderPassColorAttachment { view: dst, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[(slot as u64 * PASS_BYTES) as u32]);
        pass.draw(0..3, 0..1);
    };
    let black = &pipes.black;
    ctx.command_encoder().push_debug_group("doom_post");

    encoder_pass(&mut ctx, "doom_post_half", down_bloom, &data.half, P_HALF, [post.source, black, black, black]);
    // luminance and auto exposure
    encoder_pass(&mut ctx, "doom_post_luminance", down_luma, &data.lum[LUM_LEVELS - 1], P_LUM, [&data.half, black, black, black]);
    for k in 1..LUM_LEVELS {
        let i = LUM_LEVELS - 1 - k;
        encoder_pass(&mut ctx, "doom_post_luminance", down_luma, &data.lum[i], P_LUM + k, [&data.lum[i + 1], black, black, black]);
    }
    encoder_pass(&mut ctx, "doom_post_auto_exposure", exposure, &data.exposure, P_EXPOSURE, [&data.lum[0], &data.exposure_prev, black, black]);
    // bloom
    encoder_pass(&mut ctx, "doom_post_bloom_down", down_bloom, &data.bloom[0], P_BLOOM_DOWN, [&data.half, black, black, black]);
    for level in 1..BLOOM_LEVELS {
        encoder_pass(&mut ctx, "doom_post_bloom_down", down_bloom, &data.bloom[level], P_BLOOM_DOWN + level, [&data.bloom[level - 1], black, black, black]);
    }
    for (k, i) in (0..BLOOM_LEVELS).rev().enumerate() {
        encoder_pass(&mut ctx, "doom_post_bloom_blur_h", blur, &data.bloom_tmp[i], P_BLUR + 2 * k, [&data.bloom[i], black, black, black]);
        let coarser = data.bloom.get(i + 1).unwrap_or(black);
        encoder_pass(&mut ctx, "doom_post_bloom_blur_v", blur, &data.bloom[i], P_BLUR + 2 * k + 1, [&data.bloom_tmp[i], coarser, black, black]);
    }
    // tonemap to the 8-bit view colour, then the final upsample into the view target
    encoder_pass(&mut ctx, "doom_post_process", post_pipe, &data.view, P_POST, [post.source, &data.bloom[0], &data.exposure, dirt]);
    encoder_pass(&mut ctx, "doom_post_upsample", upsample, post.destination, P_UPSAMPLE, [&data.view, black, black, black]);
    ctx.command_encoder().pop_debug_group();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(env: PostEnv) -> PostSettings {
        PostSettings { env, cvars: PostCvars::default(), post_exposure_control: 1.0, lens_dirt: None }
    }

    #[test]
    fn default_tone_parms() {
        let t = tone_parms(&settings(PostEnv::default()), 1920, 1080);
        // bloom 1 * 0.03
        assert!((t.tone_map[0] - 1.0 / 1.03).abs() < 1e-6 && (t.tone_map[1] - 0.03 / 1.03).abs() < 1e-6);
        assert!((t.tone_map[2] - 0.5625).abs() < 1e-6 && (t.tone_map[3] - 0.03).abs() < 1e-6);
        // Hable white point 1: 1 / ((0.252 / 0.58) - 0.01 / 0.3)
        let white = 1.0 / (0.252f32 / 0.58 - 0.01 / 0.3);
        assert!((t.curve1[2] - white).abs() < 1e-4, "{}", t.curve1[2]);
        assert_eq!(t.curve0, [0.22, 0.3, 0.01, 0.01 / 0.3]);
        // neutral colour correction: identity scales, unit saturation and gamma
        assert_eq!(t.shadow_scale, [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(t.midtone_scale, [0.0; 4]);
        assert_eq!(t.saturation, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(t.gamma, [1.0, 0.0, 0.0, 0.0]);
        // no chromatic aberration, no vignette
        assert_eq!(t.chromatic_aberration_vignette[0], 0.0);
        assert_eq!(t.chromatic_aberration_vignette[2], 0.0);
        // the curve maps 0 to 0 and the white point to 1
        let (a, b, e) = (t.curve0[0], t.curve0[1], t.curve0[2]);
        assert!(hable(0.0, a, b, e).abs() < 1e-6);
        assert!((hable(1.0, a, b, e) * t.curve1[2] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn auto_exposure_adaptation() {
        let s = settings(PostEnv::default());
        let p = auto_exposure_parms(&s, 1.0 / 60.0);
        assert_eq!((p[0], p[1], p[2]), (0.01, 1.0, 0.0));
        assert!((p[3] - (1.0 - (-1.0f32).exp())).abs() < 1e-6);
    }

    #[test]
    fn pass_layout() {
        let s = settings(PostEnv::default());
        let f = PostFrame { dt: 1.0 / 60.0, frame: 1 };
        let p = passes(&s, &f, UVec2::new(1280, 720));
        assert_eq!(p.len(), PASSES);
        // half: source 1280 x 720, target 640 x 360
        assert_eq!(p[P_HALF][1][0], 1280.0);
        assert_eq!(p[P_HALF][2][0], 1.0 / 640.0);
        // luminance: half -> 128, last 4 -> 2
        assert_eq!(p[P_LUM][2][0], 1.0 / 128.0);
        assert_eq!(p[P_LUM + LUM_LEVELS - 1][1][0], 4.0);
        // blur of level 5 first: horizontal step 1 texel weight 1, vertical weight bloomWeight5
        assert_eq!(p[P_BLUR][0], [1.0, 0.0, 1.0, 0.0]);
        assert_eq!(p[P_BLUR + 1][0], [0.0, 1.0, 1.0, 0.0]);
        assert_eq!(p[P_BLUR][1][0], 1.0 / 20.0);
        assert_eq!(p[P_UPSAMPLE][0][3], 2.0);
    }

    /// `cargo test -p rancher --release -- --ignored post_env --nocapture` (needs the install).
    #[test]
    #[ignore]
    fn post_env() {
        let doom = idres::find_install().expect("install");
        let c = Container::open(&doom.join("base"), "gameresources").unwrap();
        let d = PostEnv::load(&c, None);
        assert_eq!(d, PostEnv::default());
        let e = PostEnv::load(&c, Some("tgarza/intro_crash_int"));
        assert_eq!((e.auto_exposure_min, e.filmic_highlights, e.chromatic_aberration_scale), (2.0, 4.0, 0.014));
        assert_eq!((e.shadow_midtone_end, e.midtone_highlight_end), (0.04, 1.607));
        assert_eq!((e.shadows.saturation, e.shadows.gain), (0.7, 1.25));
        assert_eq!(e.shadows.color, [0.973515, 0.983507, 1.0]);
        let dirt = load_lens_dirt(&c).expect("lens dirt");
        eprintln!("lens dirt {:?} {} mips", dirt.texture_descriptor.size, dirt.texture_descriptor.mip_level_count);
        eprintln!("intro env tone parms {:?}", tone_parms(&PostSettings { env: e, cvars: PostCvars::default(), post_exposure_control: 1.0, lens_dirt: None }, 1920, 1080));
    }
}
