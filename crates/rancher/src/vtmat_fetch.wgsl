// Virtual-texture sampling shared by the VT material's main-pass and prepass shaders: the material
// bindings and SAMPLE_VMTR's level selection (decls/renderprogs/includes vmtr.inc SAMPLE_VMTR,
// virtualtexture.inc FETCH_PHYSICAL_ARRAY).
//
// Textures: `vt_pages` is a 3-layer sRGB BC3 array (layer 0 albedo + normal x, 1 specular +
// normal y, 2 emissive mask / SSS mask / cover + power), `vt_mask` the BC7 colour mask, both
// holding every VT level of the material side by side (single mip, like the physical page cache).

#define_import_path rancher::vt_fetch

struct VtParams {
    // per VT level n: atlas uv = fract(material uv) * xform[n].xy + xform[n].zw
    xform: array<vec4<f32>, 12>,
    // x, y: material size in level-0 texels (virtualMapping.xy * widthInTexels); z, w: first and
    // last level present in the atlas
    rect: vec4<f32>,
    // x: LOD bias (USE_LOD_HACK: -1 for outsidegun programs); y: log2(vt_maxAniso);
    // z: 1 once the textures are in; w: 1 for alpha-tested surfaces (megatrans: clip(cover - 0.5))
    misc: vec4<f32>,
    // baked lightmap: x scale, y 1 when bound
    lightmap: vec4<f32>,
    // engine shading: x 1 for static (world) models ($staticModel.x: LF_DINAMIC_ONLY lights skip
    // them), y model ambient scale ($gpuAmbientParms.y), zw unused
    shading: vec4<f32>,
    // non-lightmapped emissive: rgb = $bloomMaskScale.x × $colorScale.x × $color × $bloomColor
    // (lighting::material_emissive), × mask²; w unused
    emissive: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> vt: VtParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var vt_pages: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var vt_pages_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var vt_mask: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var vt_mask_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var vt_lightmap: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var vt_lightmap_sampler: sampler;
// The map's engine lighting data, shared by every VT material (lighting.wgsl reads them).
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var<storage, read> vt_scene: array<vec4<u32>>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var vt_light_atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var vt_light_atlas_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var vt_probes: texture_cube_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(111) var vt_probes_sampler: sampler;
// Per-frame light state (vtmat.rs animate_lights): lights ( colour × fade, 0 ), probes ( 0, fade ).
@group(#{MATERIAL_BIND_GROUP}) @binding(112) var<storage, read> vt_light_dyn: array<vec4<f32>>;

struct Fetch {
    diffuse: vec4<f32>,
    specular: vec4<f32>,
    lightmap: vec4<f32>,
    color_mask: vec4<f32>,
}

// The two VT levels a pixel samples and the blend towards the second.
struct Levels {
    lo: f32,
    hi: f32,
    // 0: lo only, 1: hi only, between: blend
    t: f32,
}

// COMPUTE_ANISO_LOD in level-0 texels from the derivatives of texCoordsLOD = virtCoords *
// virtualMapping.xy (unwrapped), the USE_LOD_HACK bias, then SAMPLE_SETUP_TRI with a 0.25 band:
// the page table is read at lod -/+ 0.5 with nearest mip selection, i.e. levels floor(lod) and
// floor(lod) + 1.
fn vt_levels(ddx_uv: vec2<f32>, ddy_uv: vec2<f32>) -> Levels {
    let dx = ddx_uv * vt.rect.xy;
    let dy = ddy_uv * vt.rect.xy;
    let px = dot(dx, dx);
    let py = dot(dy, dy);
    let max_lod = 0.5 * log2(max(px, py));
    let min_lod = 0.5 * log2(min(px, py));
    var lod = max_lod - min(vt.misc.y, max_lod - min_lod);
    lod += vt.misc.x;
    let frac_lod = fract(lod);
    let n = floor(lod);
    var l: Levels;
    l.lo = clamp(n, vt.rect.z, vt.rect.w);
    l.hi = clamp(n + 1.0, vt.rect.z, vt.rect.w);
    l.t = saturate((frac_lod - 0.375) * 4.0);
    return l;
}

// FETCH_PHYSICAL_ARRAY on one level: SampleGrad with the virtual-coordinate derivatives scaled to
// the physical (atlas) texture.
fn fetch_level(level: f32, uv: vec2<f32>, ddx_uv: vec2<f32>, ddy_uv: vec2<f32>) -> Fetch {
    let x = vt.xform[u32(level)];
    let p = uv * x.xy + x.zw;
    let gx = ddx_uv * x.xy;
    let gy = ddy_uv * x.xy;
    var f: Fetch;
    f.diffuse = textureSampleGrad(vt_pages, vt_pages_sampler, p, 0, gx, gy);
    f.specular = textureSampleGrad(vt_pages, vt_pages_sampler, p, 1, gx, gy);
    f.lightmap = textureSampleGrad(vt_pages, vt_pages_sampler, p, 2, gx, gy);
    f.color_mask = textureSampleGrad(vt_mask, vt_mask_sampler, p, gx, gy);
    return f;
}

fn lerp_fetch(a: Fetch, b: Fetch, t: f32) -> Fetch {
    var f: Fetch;
    f.diffuse = mix(a.diffuse, b.diffuse, t);
    f.specular = mix(a.specular, b.specular, t);
    f.lightmap = mix(a.lightmap, b.lightmap, t);
    f.color_mask = mix(a.color_mask, b.color_mask, t);
    return f;
}

// SAMPLE_VMTR at material uv `uv_lod` (unwrapped; texCoords = frac(virtCoords)).
fn sample_vt(uv_lod: vec2<f32>, ddx_uv: vec2<f32>, ddy_uv: vec2<f32>) -> Fetch {
    let l = vt_levels(ddx_uv, ddy_uv);
    let uv = fract(uv_lod);
    if l.t <= 0.0 {
        return fetch_level(l.lo, uv, ddx_uv, ddy_uv);
    }
    if l.t >= 1.0 {
        return fetch_level(l.hi, uv, ddx_uv, ddy_uv);
    }
    return lerp_fetch(fetch_level(l.lo, uv, ddx_uv, ddy_uv), fetch_level(l.hi, uv, ddx_uv, ddy_uv), l.t);
}

// The cover alone (layer 2 blue, sRGB-decoded like the engine's DXT5 sRGB array): what
// FETCH_PHYSICAL_ARRAY moves into sampleSpecular.w.
fn sample_cover(uv_lod: vec2<f32>, ddx_uv: vec2<f32>, ddy_uv: vec2<f32>) -> f32 {
    let l = vt_levels(ddx_uv, ddy_uv);
    let uv = fract(uv_lod);
    let a = vt.xform[u32(l.lo)];
    let b = vt.xform[u32(l.hi)];
    let ca = textureSampleGrad(vt_pages, vt_pages_sampler, uv * a.xy + a.zw, 2, ddx_uv * a.xy, ddy_uv * a.xy).z;
    let cb = textureSampleGrad(vt_pages, vt_pages_sampler, uv * b.xy + b.zw, 2, ddx_uv * b.xy, ddy_uv * b.xy).z;
    return mix(ca, cb, l.t);
}

// preZDrawAlphaUnique / shadowVmtrTransUnique: clip( sampleSpecular.w - 0.5 ) on alpha-tested
// (megatrans) surfaces.
fn alpha_test(cover: f32) {
    if vt.misc.w > 0.5 && cover < 0.5 {
        discard;
    }
}

// World normal from the VT tangent-space normal (sampleNormal.wyz = diffuse.a, specular.a):
// lighting.inc CONSTRUCT_INV_TS (normalised normal / tangent, bitangent = normalize( cross( N, T )
// × sign )), global.inc ReconstructNormal and TransformNormal (normalised). Bevy-space vectors.
fn vt_normal(f: Fetch, world_normal: vec3<f32>, world_tangent: vec4<f32>) -> vec3<f32> {
    var nt = vec3<f32>(f.diffuse.a * 2.0 - 1.0, f.specular.a * 2.0 - 1.0, 0.0);
    nt.z = sqrt(saturate(1.0 - nt.x * nt.x - nt.y * nt.y));
    let n = normalize(world_normal);
    let t = normalize(world_tangent.xyz);
    let b = normalize(cross(n, t) * world_tangent.w);
    return normalize(nt.x * t + nt.y * b + nt.z * n);
}

// global.inc SRGBlinear (DeGamma)
fn degamma(v: f32) -> f32 {
    if v <= 0.04045 {
        return v / 12.92;
    }
    return pow(v / 1.055 + 0.0521327, 2.4);
}
