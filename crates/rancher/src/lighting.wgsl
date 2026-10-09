// DOOM (2016)'s surface shading on the GPU: the same port as lighting.rs (which holds the unit
// tests), from decls/renderprogs includes/lighting.inc, global.inc, vmtr.inc and
// deferred_passes.inc. Engine data come from the VT material's scene bindings (vtmat_fetch.wgsl):
// `vt_scene` (lights, probes, ambient octree), `vt_light_atlas` (_lightimageatlas, BC4) and
// `vt_probes` (envProbesMapArray, BC6H cubes). Everything here works in engine space (z up):
// the caller converts from Bevy space, which is ( -y, z, -x ) of it (range.rs to_bevy).

#define_import_path rancher::lighting

#import rancher::vt_fetch::{vt_scene, vt_light_atlas, vt_light_atlas_sampler, vt_probes, vt_probes_sampler, vt_light_dyn}

const CLIP_MIN: f32 = 1.0 / 255.0;
const MANTISSA: f32 = 8388608.0;
const EXP_BIAS: f32 = 127.0;

// ---- global.inc bit tricks (WGSL f32 -> u32 conversion saturates like the engine's uint()) ----

fn approx_log2(f: f32) -> f32 {
    return f32(bitcast<u32>(f)) / MANTISSA - EXP_BIAS;
}

fn approx_exp2(f: f32) -> f32 {
    return bitcast<f32>(u32((f + EXP_BIAS) * MANTISSA));
}

fn approx_pow(b: f32, p: f32) -> f32 {
    return bitcast<f32>(u32(p * f32(bitcast<u32>(b)) - (p - 1.0) * EXP_BIAS * MANTISSA));
}

// fastSqrtNR0: IEEE_INT_SQRT_CONST_NR0 + ( asint( x ) >> 1 )
fn fast_sqrt_nr0(x: f32) -> f32 {
    return bitcast<f32>(0x1FBD1DF5 + (bitcast<i32>(x) >> 1u));
}

// ---- packing (non-AMD branch: truncating) ----

fn pack_r8g8b8a8(v: vec4<f32>) -> u32 {
    let q = vec4<u32>(saturate(v) * 255.0);
    return (q.x << 24u) | (q.y << 16u) | (q.z << 8u) | q.w;
}

fn unpack_r8g8b8a8(v: u32) -> vec4<f32> {
    return vec4<f32>(f32((v >> 24u) & 0xffu), f32((v >> 16u) & 0xffu), f32((v >> 8u) & 0xffu), f32(v & 0xffu)) / 255.0;
}

fn pack_r10g10b10(v: vec3<f32>) -> u32 {
    let q = vec3<u32>(saturate(v) * 1023.0);
    return (q.x << 20u) | (q.y << 10u) | q.z;
}

fn unpack_r10g10b10(v: u32) -> vec3<f32> {
    return vec3<f32>(f32((v >> 20u) & 0x3ffu), f32((v >> 10u) & 0x3ffu), f32(v & 0x3ffu)) / 1023.0;
}

// packRGBE; 0/0 for black is saturate( NaN ) = 0 on the engine's GPUs, the zero branch here
fn pack_rgbe(v: vec3<f32>) -> u32 {
    let shared_exp = ceil(approx_log2(max(max(v.r, v.g), v.b)));
    let s = approx_exp2(shared_exp);
    var m = vec3<f32>(0.0);
    if s > 0.0 {
        m = v / s;
    }
    return pack_r8g8b8a8(vec4<f32>(m, (shared_exp + 128.0) / 255.0));
}

fn unpack_rgbe(v: u32) -> vec3<f32> {
    let c = unpack_r8g8b8a8(v);
    return c.rgb * approx_exp2(c.a * 255.0 - 128.0);
}

fn rgbe_add(acc: u32, add: vec3<f32>) -> u32 {
    return pack_rgbe(add + unpack_rgbe(acc));
}

// unpackR15G15B15A15
fn unpack_r15x4(v: vec2<u32>) -> vec4<f32> {
    return vec4<f32>(f32((v.x >> 16u) & 0xffffu), f32(v.x & 0xffffu), f32((v.y >> 16u) & 0xffffu), f32(v.y & 0xffffu)) * (1.0 / 32767.0);
}

// ---- BRDF (global.inc) ----

fn fresnel_schlick(f0: vec3<f32>, cos_theta: f32) -> vec3<f32> {
    let occl = saturate(50.0 * dot(f0, vec3<f32>(0.3333)));
    return saturate(f0 + (vec3<f32>(occl) - f0) * approx_pow(saturate(1.0 - cos_theta), 5.0));
}

fn spec_brdf(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, f0: vec3<f32>, smoothness: f32) -> vec3<f32> {
    let h = normalize(v + l);
    var m = 1.0 - smoothness * 0.8;
    m *= m;
    m *= m;
    let m2 = m * m;
    let n_dot_h = saturate(dot(n, h));
    var spec = (n_dot_h * n_dot_h) * (m2 - 1.0) + 1.0;
    spec = m2 / (spec * spec + 1e-8);
    let gv = saturate(dot(n, v)) * (1.0 - m) + m;
    let gl = saturate(dot(n, l)) * (1.0 - m) + m;
    spec /= (4.0 * gv * gl + 1e-8);
    return fresnel_schlick(f0, dot(l, h)) * spec;
}

fn environment_brdf(n_dot_v: f32, smoothness: f32, f0: vec3<f32>) -> vec3<f32> {
    let t1 = 0.095 + smoothness * (0.6 + 4.19 * smoothness);
    let t2 = n_dot_v + 0.025;
    let t3 = 9.5 * smoothness * n_dot_v;
    let a0 = t1 * t2 * approx_exp2(1.0 - 14.0 * n_dot_v);
    let a1 = 0.4 + 0.6 * (1.0 - approx_exp2(-t3));
    return mix(vec3<f32>(a0), vec3<f32>(a1), f0);
}

// ---- the lighting accumulators (lightingInput_t) ----

struct Shading {
    position: vec3<f32>,
    normal: vec3<f32>,
    view: vec3<f32>,
    // unpackR10G10B10( specular_packed )
    specular: vec3<f32>,
    // abs( smoothness )
    smoothness: f32,
    diffuse_packed: u32,
    specular_packed: u32,
    // 1 for static (world) models: skips LF_DINAMIC_ONLY lights ($staticModel.x)
    static_model: bool,
}

const LF_MASK: u32 = 3u;
const LF_PARALLEL: u32 = 2u;
const LF_CAST_SHADOWS: u32 = 4u;
const LF_DINAMIC_ONLY: u32 = 8u;
const LF_AREA_MASK: u32 = 48u;
const LF_AREA_CIRCLE: u32 = 32u;
const LF_AREA_REFLECTOR: u32 = 48u;
const LF_LIGHT_PARTICLES: u32 = 64u;

// vt_scene layout (lighting.rs `SceneBuffer::pack`): [0] = ( lights, probes, octree offset, leaves
// offset ), [1] = ( records offset, ambient on, envIrradianceScale bits, 0 ); lights from
// LIGHT_BASE, LIGHT_STRIDE vec4s each; probes after them, PROBE_STRIDE each.
const LIGHT_BASE: u32 = 2u;
const LIGHT_STRIDE: u32 = 10u;
const PROBE_STRIDE: u32 = 8u;

fn f4(i: u32) -> vec4<f32> {
    return bitcast<vec4<f32>>(vt_scene[i]);
}

fn plane(p: vec4<f32>, x: vec3<f32>) -> f32 {
    // the engine's evaluation order: x * p.x + ( y * p.y + ( z * p.z + p.w ) )
    return x.x * p.x + (x.y * p.y + (x.z * p.z + p.w));
}

fn proj_tc(base: u32, x: vec3<f32>) -> vec3<f32> {
    var tc = vec3<f32>(plane(f4(base), x), plane(f4(base + 1u), x), plane(f4(base + 2u), x));
    tc = vec3<f32>(tc.xy / tc.z, plane(f4(base + 3u), x));
    return tc;
}

fn outside(tc: vec3<f32>) -> bool {
    return min(tc.x, min(tc.y, tc.z)) <= CLIP_MIN || max(tc.x, max(tc.y, tc.z)) >= 1.0 - CLIP_MIN;
}

// COMPUTE_LIGHT (no SSS, no custom light, PC branch)
fn compute_light(s: ptr<function, Shading>, light_position: vec3<f32>, light_color: vec3<f32>, spec_mul: f32) {
    let lv = normalize(light_position - (*s).position);
    let n_dot_l = saturate(dot((*s).normal, lv));
    let spec = spec_brdf((*s).normal, (*s).view, lv, (*s).specular, (*s).smoothness);
    (*s).specular_packed = rgbe_add((*s).specular_packed, spec * spec_mul * light_color * n_dot_l);
    (*s).diffuse_packed = rgbe_add((*s).diffuse_packed, light_color * n_dot_l);
}

// PROCESS_LIGHT for engine light `index`. Light record (vec4s): 0..3 projS, projT, projQ, falloffR;
// 4 ( globalLightOrigin, lightParms ); 5 ( unpackRGBE( colorPacked ), specMultiplier ); 6 scaleBias
// ( falloff .xy, projection .zw, R15G15B15A15 words ); area lights: 7 areaPlane, 8 boxMin (U),
// 9 boxMax (V) (lighting.rs LightDef::area_records, 0x14187b700).
fn process_light(s: ptr<function, Shading>, index: u32) {
    let base = LIGHT_BASE + index * LIGHT_STRIDE;
    let x = (*s).position;
    let tc = proj_tc(base, x);
    let pos_parms = vt_scene[base + 4u];
    let light_parms = pos_parms.w;
    var light_flags = LF_LIGHT_PARTICLES;
    if (*s).static_model {
        light_flags |= LF_DINAMIC_ONLY;
    }
    // this frame's colour (light material program × distance fade, RGBE-packed); 0 = culled
    let light_color = vt_light_dyn[index].rgb;
    if outside(tc) || (light_parms & light_flags) != 0u || all(light_color == vec3<f32>(0.0)) {
        return;
    }
    var light_position = bitcast<vec3<f32>>(pos_parms.xyz);
    let n_dot_l0 = saturate(dot((*s).normal, light_position - x));
    if n_dot_l0 <= CLIP_MIN && (light_parms & LF_AREA_MASK) == 0u {
        return;
    }
    let sb = vt_scene[base + 6u];
    let falloff_sb = unpack_r15x4(sb.xy);
    let proj_sb = unpack_r15x4(sb.zw);
    let proj_filter = textureSampleLevel(vt_light_atlas, vt_light_atlas_sampler, tc.xy * proj_sb.xy + proj_sb.zw, 0.0).x;
    let falloff = textureSampleLevel(vt_light_atlas, vt_light_atlas_sampler, vec2<f32>(tc.z, 0.5) * falloff_sb.xy + falloff_sb.zw, 0.0).x;
    let att = (falloff * falloff) * (proj_filter * proj_filter);
    if att <= CLIP_MIN / 256.0 {
        return;
    }
    // INTERIM: no shadow maps (LF_CAST_SHADOWS lights are unshadowed)
    var color = light_color * att;
    // area light source: move the light to the reflection ray's hit on the area (lighting.rs area_light)
    if (light_parms & LF_AREA_MASK) != 0u {
        var area_plane = f4(base + 7u);
        let pu = f4(base + 8u);
        let pv = f4(base + 9u);
        var proj_to_origin = dot(x, area_plane.xyz) + area_plane.w;
        if proj_to_origin < 0.0 {
            return;
        }
        let falloff_distance = length(area_plane.xyz);
        let area_falloff = saturate(proj_to_origin);
        color *= area_falloff * area_falloff;
        proj_to_origin /= falloff_distance;
        area_plane /= falloff_distance;
        var mirror = reflect(-(*s).view, (*s).normal);
        // facing away: flip it to face the area light
        if dot(mirror, area_plane.xyz) > 0.0 {
            mirror = reflect(mirror, area_plane.xyz);
        }
        let proj_length = proj_to_origin / dot(mirror, area_plane.xyz);
        let hit_dir = (x - mirror * proj_length) - light_position;
        var u = dot(hit_dir, pu.xyz);
        var v = dot(hit_dir, pv.xyz);
        var len = max(abs(u), abs(v));
        if (light_parms & LF_AREA_MASK) == LF_AREA_CIRCLE {
            len = length(vec2<f32>(u, v));
        }
        // (LF_AREA_REFLECTOR also writes projTC.xy, which nothing reads afterwards)
        len = min(1.0, len) / len;
        u *= len * pu.w;
        v *= len * pv.w;
        light_position += pu.xyz * u + pv.xyz * v;
    }
    let cs = f4(base + 5u);
    compute_light(s, light_position, color, cs.w);
}

// ---- environment probes (DEFERRED_ENV_PROBES -> COMPUTE_LIGHTING -> PROCESS_PROBE) ----

// Probe record: 0..3 projS, projT, projQ, falloffR; 4 ( pos, lightParms: probe id bits 8..15
// ( unpackUintR8G8B8A8.z ), inner falloff bits 16..23 ( unpackR8G8B8A8.y ) ); 5 ( colour,
// specMultiplier ); 6 ( boxMin, distance fade ); 7 ( boxMax, 0 ). Sorted smallest first.
fn process_probes(s: ptr<function, Shading>) {
    let n_lights = vt_scene[0].x;
    let n_probes = vt_scene[0].y;
    var dst_alpha = 1.0;
    let x = (*s).position;
    for (var i = 0u; i < n_probes; i++) {
        let base = LIGHT_BASE + n_lights * LIGHT_STRIDE + i * PROBE_STRIDE;
        let tc = proj_tc(base, x);
        if outside(tc) {
            continue;
        }
        let pos_parms = vt_scene[base + 4u];
        let light_parms = pos_parms.w;
        let inner = unpack_r8g8b8a8(light_parms).y;
        let c = tc * 2.0 - vec3<f32>(1.0);
        let sphere = c * sqrt(vec3<f32>(1.0) - c.yzx * c.yzx * 0.5 - c.zxy * c.zxy * 0.5 + (c.yzx * c.yzx * c.zxy * c.zxy / 3.0));
        var att = saturate(1.0 - (length(sphere) - inner) / (1.0 - inner + 1e-6));
        att *= att;
        let box_min = f4(base + 6u);
        // boxMin.w = this frame's distance fade (vt_light_dyn)
        att *= vt_light_dyn[n_lights + i].w * dst_alpha;
        dst_alpha *= 1.0 - att;
        if att <= CLIP_MIN * 4.0 {
            continue;
        }
        let cs = f4(base + 5u);
        let light_position = bitcast<vec3<f32>>(pos_parms.xyz);
        // COMPUTE_LIGHT_PROBE (no snapmap re-orientation: light_inv_orient is the identity)
        let r = reflect(-(*s).view, (*s).normal);
        let bmax = (f4(base + 7u).xyz - x) / r;
        let bmin = (box_min.xyz - x) / r;
        let bminmax = max(bmax, bmin);
        let dist = min(bminmax.x, min(bminmax.y, bminmax.z));
        let lr = normalize(x + r * dist - light_position);
        let probe_id = (light_parms >> 8u) & 0xffu;
        let mip = 6.0 - 6.0 * (*s).smoothness;
        var spec_env = textureSampleLevel(vt_probes, vt_probes_sampler, lr, i32(probe_id), mip).rgb;
        let n_dot_v = saturate(dot((*s).view, (*s).normal));
        spec_env *= environment_brdf(n_dot_v, (*s).smoothness, (*s).specular) * cs.w;
        (*s).specular_packed = rgbe_add((*s).specular_packed, spec_env * cs.rgb * saturate(att));
        if dst_alpha <= CLIP_MIN * 4.0 {
            break;
        }
    }
}

// ---- ambient for dynamic geometry (vertex.inc CALC_IRRADIANCE_CONTRIBUTION, vmtr.inc
// APPLY_IRRADIANCE_CONTRIBUTION) ----

// The four RGB coefficients at engine-space point `p` from the ambient octree (lighting.rs
// `AmbientOctree`): 10 levels over [-32768, 32768]^3; the cell the descent stops in samples its
// leaf's 8 corner records trilinearly with the position inside that cell (the SH atlas's 2x2x2
// RGBA16F cube read between 0.25 and 0.75). Before × $gpuAmbientParms.y.
fn ambient_sh(p: vec3<f32>) -> array<vec3<f32>, 4> {
    var c: array<vec3<f32>, 4>;
    let h0 = vt_scene[0];
    let h1 = vt_scene[1];
    if h1.y == 0u {
        return c;
    }
    var q = saturate((p + vec3<f32>(32768.0)) / 65536.0);
    var node = 0u;
    var leaf = 0xffffffffu;
    for (var i = 0u; i < 10u; i++) {
        let o = min(vec3<u32>(floor(q * 2.0)), vec3<u32>(1u));
        let child = o.x + o.y * 2u + o.z * 4u;
        let v = vt_scene[h0.z + node * 2u + child / 4u][child % 4u];
        q = fract(q * 2.0);
        if (v & 0x40000000u) != 0u {
            leaf = v & 0x3fffffffu;
            break;
        }
        node = v;
    }
    if leaf == 0xffffffffu {
        return c;
    }
    for (var k = 0u; k < 8u; k++) {
        let r = vt_scene[h0.w + leaf * 2u + k / 4u][k % 4u];
        let w = select(1.0 - q.x, q.x, (k & 1u) != 0u) * select(1.0 - q.y, q.y, (k & 2u) != 0u) * select(1.0 - q.z, q.z, (k & 4u) != 0u);
        let s0 = f4(h1.x + r * 3u);
        let s1 = f4(h1.x + r * 3u + 1u);
        let s2 = f4(h1.x + r * 3u + 2u);
        c[0] += s0.xyz * w;
        c[1] += vec3<f32>(s0.w, s1.x, s1.y) * w;
        c[2] += vec3<f32>(s1.z, s1.w, s2.x) * w;
        c[3] += s2.yzw * w;
    }
    return c;
}

// APPLY_IRRADIANCE_CONTRIBUTION with albedo 1 (lighting.inc): engine-space normal.
fn irradiance(c: array<vec3<f32>, 4>, n: vec3<f32>, ambient_scale: f32) -> vec3<f32> {
    let env_irradiance_scale = bitcast<f32>(vt_scene[1].z);
    let s = ambient_scale;
    let d = ((c[0] * s - n.y * (c[1] * s)) + n.z * (c[2] * s)) - n.x * (c[3] * s);
    return max(d * saturate(env_irradiance_scale), vec3<f32>(0.0));
}

fn ambient_on() -> bool {
    return vt_scene[1].y != 0u;
}
