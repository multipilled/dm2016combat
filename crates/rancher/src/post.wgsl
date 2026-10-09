// DOOM (2016)'s HDR post chain (decls/renderprogs/postprocess.inc templates DOWNSAMPLE, GAUSSIAN_BLUR,
// AUTO_EXPOSURE, POST_PROCESS, VIEW_COLOR_UPSAMPLE; global.inc helpers), one fragment entry point per
// template. Every pass binds the same layout; `p` holds that pass's renderparms (see post.rs `Pass`).

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Params {
    v: array<vec4<f32>, 16>,
}

@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var tex1: texture_2d<f32>;
@group(0) @binding(2) var tex2: texture_2d<f32>;
@group(0) @binding(3) var tex3: texture_2d<f32>;
@group(0) @binding(4) var clamp_sampler: sampler;
@group(0) @binding(5) var wrap_sampler: sampler;
@group(0) @binding(6) var<uniform> p: Params;

fn lod0(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(t, clamp_sampler, uv, 0.0);
}

// global.inc tex2DlodClamped: scalingInfos = (resolution scale, pixel size)
fn lod0_clamped(t: texture_2d<f32>, uv: vec2<f32>, info: vec4<f32>) -> vec4<f32> {
    return lod0(t, clamp(uv, vec2<f32>(0.0), info.xy - info.zw / 2.0));
}

// global.inc GetLuma (BT.709)
fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// global.inc SRGBlinear / linearSRGB
fn srgb_linear1(v: f32) -> f32 {
    if v <= 0.04045 {
        return v / 12.92;
    }
    return pow(v / 1.055 + 0.0521327, 2.4);
}
fn srgb_linear(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_linear1(c.x), srgb_linear1(c.y), srgb_linear1(c.z));
}
fn linear_srgb1(v: f32) -> f32 {
    if v > 0.0031308 {
        return pow(v, 1.0 / 2.4) * 1.055 - 0.055;
    }
    return v * 12.92;
}
fn linear_srgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_srgb1(c.x), linear_srgb1(c.y), linear_srgb1(c.z));
}

// DOWNSAMPLE. v0 = $downsampleType (x: 0 average, 1 luminance, 2 bright pass; y: source uses the
// resolution scale; z: luminance clamp; w: luminance NaN multiplier / bright-pass threshold),
// v1 = $texSize (source size, 1 / source size), v2 = (1 / target size, $resolutionScale.xy).
@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dtype = p.v[0];
    let tex_size = p.v[1];
    let texcoord = in.position.xy * p.v[2].xy;
    var sample_scale = vec2<f32>(1.0);
    if dtype.y != 0.0 {
        sample_scale = p.v[2].zw;
    }
    let st = texcoord * sample_scale;
    // grid rotated 30 degrees
    let c = lod0(tex0, st).xyz;
    let c0 = lod0(tex0, st + sample_scale * tex_size.zw * vec2<f32>(0.86, 0.50)).xyz;
    let c1 = lod0(tex0, st + sample_scale * tex_size.zw * vec2<f32>(-0.50, 0.86)).xyz;
    let c2 = lod0(tex0, st + sample_scale * tex_size.zw * vec2<f32>(-0.86, -0.5)).xyz;
    let c3 = lod0(tex0, st + sample_scale * tex_size.zw * vec2<f32>(0.50, -0.86)).xyz;
    var out = max((c + c0 + c1 + c2 + c3) * 0.2, vec3<f32>(0.0));
    if dtype.x == 1.0 {
        let l = luma(c) + 1e-6;
        let l0 = luma(c0) + 1e-6;
        let l1 = luma(c1) + 1e-6;
        let l2 = luma(c2) + 1e-6;
        let l3 = luma(c3) + 1e-6;
        let lum = min(dtype.z, max((l + l0 + l1 + l2 + l3) * 0.2, 0.0));
        out = vec3<f32>(select(lum, 1.0, lum * dtype.w != 0.0));
    }
    if dtype.x == 2.0 {
        let l = luma(out);
        out *= smoothstep(0.0, 1.0, saturate(l - dtype.w));
    }
    return vec4<f32>(out, 1.0);
}

// GAUSSIAN_BLUR. v0 = $blurStep (xy direction in texels, z weight), v1.xy = 1 / target size.
// tex0 the level, tex1 what the result is added to (the next coarser level, or black).
@fragment
fn blur(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var w = array<f32, 8>(0.027062858, 0.088897429, 0.18941596, 0.26192293, 0.23510250, 0.13697305, 0.051778302, 0.0088469516);
    var off = array<f32, 8>(-6.3269038, -4.3775406, -2.4309988, -0.48611468, 1.4584296, 3.4039848, 5.3518057, 7.00000000);
    let texcoord = in.position.xy * p.v[1].xy;
    let step = p.v[0].xy * p.v[1].xy;
    var sum = vec3<f32>(0.0);
    for (var s = 0; s < 8; s++) {
        sum += lod0(tex0, texcoord + step * off[s]).xyz * w[s];
    }
    return vec4<f32>(sum * p.v[0].z + lod0(tex1, texcoord).xyz, 1.0);
}

// AUTO_EXPOSURE. tex0 = the 2 x 2 luminance, tex1 = last frame's exposure.
// v0 = $autoExposureParms, v1 = $autoExposureMinMaxParms, v2 = $downsampleType.
@fragment
fn auto_exposure(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var luma_curr = lod0(tex0, vec2<f32>(0.25, 0.25)).x + lod0(tex0, vec2<f32>(0.75, 0.25)).x + lod0(tex0, vec2<f32>(0.25, 0.75)).x + lod0(tex0, vec2<f32>(0.75, 0.75)).x;
    luma_curr = luma_curr * 0.25 + 1e-6;
    luma_curr = select(max(0.0, luma_curr), 1.0, luma_curr * p.v[2].w != 0.0);
    let parms = p.v[0];
    let minmax = p.v[1];
    // Krawczyk scene key
    let adjust = mix(parms.x, luma_curr * minmax.w, parms.y);
    let rcp_log2_10 = 1.0 / log2(10.0);
    let scene_key = 1.03 - 2.0 / (2.0 + (log2(adjust + 1.0) * rcp_log2_10));
    let curr = clamp(scene_key / adjust, minmax.x, minmax.y);
    let prev = clamp(lod0(tex1, vec2<f32>(0.5, 0.5)).x, 0.0, 64.0);
    let final_exposure = mix(prev, curr, parms.w);
    return vec4<f32>(vec3<f32>(max(final_exposure, 0.0)), 1.0);
}

// POST_PROCESS (the postprocess renderprog). tex0 = HDR scene, tex1 = bloom, tex2 = exposure,
// tex3 = $bloomDustMap. v0 = (1 / size, $resolutionScale.xy), v1 = $chromaticAberrationVignette,
// v2 = $toneMapParms, v3 = $colorCorrection, v4 = $HDRBloomColorFilter, v5 = ($vignetteColor, $renderMode),
// v6..v11 = $colorCorrectionRange / Saturation / Gamma / ShadowScale / MidtoneScale / HighlightScale,
// v12, v13 = $colorCorrectionCurve0 / 1, v14.x = the game exposure multiplier ($postExposureControl).
// Output: the sRGB-encoded 8-bit view colour.
@fragment
fn post_process(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let res_scale = p.v[0].zw;
    let texcoord = in.position.xy * p.v[0].xy;
    let unscaled = texcoord / res_scale;
    // $postDistortionMap holds no distortion (127 / 255) without heat haze effects

    let cav = p.v[1];
    var texcoord_d = unscaled - vec2<f32>(0.5, 0.5);
    let direction_len = length(texcoord_d);
    let len = max(direction_len, 1e-20);
    texcoord_d *= cav.x * pow(direction_len, cav.y) / len * res_scale;
    let info = vec4<f32>(res_scale, p.v[0].xy);
    var hdr = vec3<f32>(0.5, 0.0, 0.0) * lod0_clamped(tex0, texcoord - texcoord_d, info).xyz
        + vec3<f32>(0.3, 0.25, 0.0) * lod0_clamped(tex0, texcoord - texcoord_d * 0.5, info).xyz
        + vec3<f32>(0.2, 0.5, 0.2) * lod0_clamped(tex0, texcoord, info).xyz
        + vec3<f32>(0.0, 0.25, 0.3) * lod0_clamped(tex0, texcoord + texcoord_d * 0.5, info).xyz
        + vec3<f32>(0.0, 0.0, 0.5) * lod0_clamped(tex0, texcoord + texcoord_d, info).xyz;

    let bloom = lod0(tex1, unscaled).xyz * p.v[4].xyz;
    // $tex3 (lens flares) is black: no flare pass yet
    let flares = vec3<f32>(0.0);
    let vignetting = 1.0 - pow(direction_len, cav.w) / len * cav.z;
    let tm = p.v[2];
    let lens_dirt = textureSampleLevel(tex3, wrap_sampler, 1.4 * (texcoord / res_scale) * vec2<f32>(1.0, tm.z), 0.0).rgb;

    hdr += flares + ((flares + bloom * lens_dirt)) * tm.w;
    let auto_exposure = lod0(tex2, vec2<f32>(0.5, 0.5)).x * p.v[14].x;
    // energy conserving composite
    hdr = auto_exposure * (hdr * tm.x + bloom * tm.y);
    let vc = p.v[5];
    if vc.w == 0.0 {
        hdr = mix(hdr, vc.xyz, 1.0 - vignetting);
    } else {
        hdr *= vignetting;
    }

    // ordered dither
    let idx = vec2<u32>(in.position.xy) & vec2<u32>(3u);
    var bayer = array<u32, 2>(
        (0u << 0u) | (12u << 4u) | (3u << 8u) | (15u << 12u) | (8u << 16u) | (4u << 20u) | (11u << 24u) | (7u << 28u),
        (2u << 0u) | (14u << 4u) | (1u << 8u) | (13u << 12u) | (10u << 16u) | (6u << 20u) | (9u << 24u) | (5u << 28u),
    );
    let dither = f32(((bayer[idx.y >> 1u] >> ((idx.x + (idx.y & 1u) * 4u) * 4u)) & 0xFu) + 1u) / 4335.0;

    // APPLY_GAME_EFFECTS: no powerup / overlay / demon vision active

    let lum = luma(hdr);
    let range = p.v[6];
    let shadow2mid = saturate(lum * range.x + range.y);
    let mid2high = saturate(lum * range.z + range.w);
    let saturation = p.v[7].z * mid2high + p.v[7].y * shadow2mid + p.v[7].x;
    let gamma = p.v[8].z * mid2high + p.v[8].y * shadow2mid + p.v[8].x;
    let color = p.v[11] * mid2high + p.v[10] * shadow2mid + p.v[9];
    hdr = mix(vec3<f32>(lum), hdr, saturation);
    hdr = hdr * color.xyz + vec3<f32>(color.w);

    // filmic response curve (Hable)
    let curve0 = p.v[12];
    let curve1 = p.v[13];
    let sho_stren = curve0.x;
    let lin_stren = curve0.y;
    let c = max(hdr, vec3<f32>(0.0));
    let tone_mapped = ((c * (sho_stren * c + curve1.x) + curve1.y) / (c * (sho_stren * c + lin_stren) + 0.2 * 0.3)) - curve0.w;

    // exact sRGB curve plus the ordered dither. pow of a rounding-negative toe is undefined; clamp it.
    var out = saturate(linear_srgb(pow(max(tone_mapped, vec3<f32>(0.0)), vec3<f32>(gamma)) * curve1.z) + dither);
    // contrast via extrapolation
    out = saturate(mix(vec3<f32>(0.5), out, p.v[3].www));
    if p.v[3].x > 2.0 {
        out = vec3<f32>(saturate(1.0 - shadow2mid), saturate(shadow2mid - mid2high), mid2high);
    }
    return vec4<f32>(out, 1.0);
}

// global.inc cubic B-spline weights
fn w0(a: f32) -> f32 { return (1.0 / 6.0) * (a * (a * (-a + 3.0) - 3.0) + 1.0); }
fn w1(a: f32) -> f32 { return (1.0 / 6.0) * (a * a * (3.0 * a - 6.0) + 4.0); }
fn w2(a: f32) -> f32 { return (1.0 / 6.0) * (a * (a * (-3.0 * a + 3.0) + 3.0) + 1.0); }
fn w3(a: f32) -> f32 { return (1.0 / 6.0) * (a * a * a); }
fn g0(a: f32) -> f32 { return w0(a) + w1(a); }
fn g1(a: f32) -> f32 { return w2(a) + w3(a); }
fn h0(a: f32) -> f32 { return -1.0 + w1(a) / (w0(a) + w1(a)); }
fn h1(a: f32) -> f32 { return 1.0 + w3(a) / (w2(a) + w3(a)); }

// global.inc tex2DBicubicClamped
fn bicubic_clamped(t: texture_2d<f32>, uv_in: vec2<f32>, res: vec2<f32>, info: vec4<f32>) -> vec4<f32> {
    let uv = uv_in * res + 0.5;
    let iuv = floor(uv);
    let fuv = fract(uv);
    let g0x = g0(fuv.x);
    let g1x = g1(fuv.x);
    let h0x = h0(fuv.x);
    let h1x = h1(fuv.x);
    let h0y = h0(fuv.y);
    let h1y = h1(fuv.y);
    let p0 = (vec2<f32>(iuv.x + h0x, iuv.y + h0y) - 0.5) / res;
    let p1 = (vec2<f32>(iuv.x + h1x, iuv.y + h0y) - 0.5) / res;
    let p2 = (vec2<f32>(iuv.x + h0x, iuv.y + h1y) - 0.5) / res;
    let p3 = (vec2<f32>(iuv.x + h1x, iuv.y + h1y) - 0.5) / res;
    let t0 = lod0_clamped(t, p0, info);
    let t1 = lod0_clamped(t, p1, info);
    let t2 = lod0_clamped(t, p2, info);
    let t3 = lod0_clamped(t, p3, info);
    return g0(fuv.y) * (g0x * t0 + g1x * t1) + g1(fuv.y) * (g0x * t2 + g1x * t3);
}

// VIEW_COLOR_UPSAMPLE (r_renderMode 0, no GUI layer). tex0 = the 8-bit view colour.
// v0 = $resolutionScale (w: sharpening), v1 = $positionToViewTexture (size, 1 / size),
// v2 = $upsampleParms (w: film grain), v3 = ($randomInt.xy, $gamma.x).
// Bevy's sRGB swapchain re-encodes, so the engine's display value is written linearised.
@fragment
fn upsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let rs = p.v[0];
    let ptv = p.v[1];
    let texcoord = in.position.xy * ptv.zw;
    let info = vec4<f32>(rs.xy, 1.0 / ptv.xy);
    let low_pass = bicubic_clamped(tex0, texcoord, ptv.xy, info).rgb;
    let linear_color = srgb_linear(saturate(mix(low_pass, lod0_clamped(tex0, texcoord, info).rgb, vec3<f32>(rs.w))));
    var out = linear_srgb(linear_color);
    // user gamma
    if p.v[3].z != 1.0 {
        out = pow(out, vec3<f32>(p.v[3].z));
    }
    // APPLY_FILM_GRAIN
    let up = p.v[2];
    if up.w > 0.0 {
        let grain_tc = texcoord + vec2<f32>(vec2<i32>(p.v[3].xy) & vec2<i32>(1023)) / 1023.0;
        let grain = fract(sin(grain_tc.x + grain_tc.y * 521.0) * 493013.0) * 2.0 - 1.0;
        out = saturate(out + saturate(1.0 - luma(out)) * vec3<f32>(grain) * up.w);
    }
    // APPLY_COLOR_BLIND_MODE: r_colorBlindMode 0
    return vec4<f32>(srgb_linear(out), 1.0);
}
