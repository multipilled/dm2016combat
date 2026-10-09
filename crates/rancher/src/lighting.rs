//! DOOM (2016)'s surface shading on the CPU: a line-by-line port of the renderprogs the install ships
//! (decls/renderprogs/includes/lighting.inc COMPUTE_SHADING / COMPUTE_LIGHTING_IMPL / PROCESS_LIGHT /
//! COMPUTE_LIGHT / PROCESS_PROBE / COMPUTE_LIGHT_PROBE, global.inc specBRDF / environmentBRDF /
//! fresnelSchlick / the Approx* and pack helpers, deferred_passes.inc DEFERRED_ENV_PROBES, vmtr.inc
//! APPLY_IRRADIANCE_CONTRIBUTION). `vtmat.wgsl` runs the same math on the GPU; the unit tests pin
//! this port to values derived by hand from those sources.
//!
//! The engine's PC path (no ORBIS / DURANGO, no AMD_VK_EXTENSIONS) is the one ported. Its debug
//! lerps with `$pbrDebugParms.w` are identities: the renderparm default is 1
//! (generated/decls/renderparm/pbrdebugparms.decl `{ Vec 1.0 }`).

use bevy::math::Vec3;

// ---------------------------------------------------------------------------------------------
// global.inc bit tricks (Drobot14 ShaderFastLib). HLSL uint(float) clamps like Rust's `as u32`.

const EXP_BIAS: f32 = 127.0; // g_iExpBias
const MANTISSA: f32 = (1u32 << 23) as f32; // 1 << g_iMantissaBits
/// IEEE_INT_SQRT_CONST_NR0
const SQRT_CONST_NR0: i32 = 0x1FBD1DF5;

/// global.inc ApproxLog2: float( asuint( f ) ) / ( 1 << 23 ) - 127.
pub fn approx_log2(f: f32) -> f32 {
    f.to_bits() as f32 / MANTISSA - EXP_BIAS
}

/// global.inc ApproxExp2: asfloat( uint( ( f + 127 ) * ( 1 << 23 ) ) ).
pub fn approx_exp2(f: f32) -> f32 {
    f32::from_bits(((f + EXP_BIAS) * MANTISSA) as u32)
}

/// global.inc ApproxPow: asfloat( uint( p * float( asuint( b ) ) - ( p - 1 ) * 127 * ( 1 << 23 ) ) ).
pub fn approx_pow(base: f32, power: f32) -> f32 {
    f32::from_bits((power * base.to_bits() as f32 - (power - 1.0) * EXP_BIAS * MANTISSA) as u32)
}

/// global.inc fastSqrtNR0 (sqrtIEEEIntApproximation with IEEE_INT_SQRT_CONST_NR0).
pub fn fast_sqrt_nr0(x: f32) -> f32 {
    f32::from_bits((SQRT_CONST_NR0 + ((x.to_bits() as i32) >> 1)) as u32)
}

/// global.inc SRGBlinear (DeGamma).
pub fn degamma(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { (v / 1.055 + 0.0521327).powf(2.4) }
}

/// global.inc GetLuma (BT-709).
pub fn luma(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.2126, 0.7152, 0.0722))
}

// ---------------------------------------------------------------------------------------------
// global.inc packing (the non-AMD branch): truncating conversions.

pub fn pack_r8g8b8a8(v: [f32; 4]) -> u32 {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0) as u32;
    (q(v[0]) << 24) | (q(v[1]) << 16) | (q(v[2]) << 8) | q(v[3])
}

pub fn unpack_r8g8b8a8(v: u32) -> [f32; 4] {
    [((v >> 24) & 0xff) as f32 / 255.0, ((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0]
}

pub fn pack_r10g10b10(v: Vec3) -> u32 {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 1023.0) as u32;
    (q(v.x) << 20) | (q(v.y) << 10) | q(v.z)
}

pub fn unpack_r10g10b10(v: u32) -> Vec3 {
    Vec3::new(((v >> 20) & 0x3ff) as f32, ((v >> 10) & 0x3ff) as f32, (v & 0x3ff) as f32) / 1023.0
}

/// global.inc packRGBE (Ward 1984): shared exponent ceil( ApproxLog2( max ) ). A black input divides
/// 0 by 0 there; saturate( NaN ) is 0 on the GPU, which is what the zero branch gives.
pub fn pack_rgbe(v: Vec3) -> u32 {
    let shared = approx_log2(v.max_element()).ceil();
    let s = approx_exp2(shared);
    let m = if s > 0.0 { v / s } else { Vec3::ZERO };
    pack_r8g8b8a8([m.x, m.y, m.z, (shared + 128.0) / 255.0])
}

/// global.inc unpackRGBE.
pub fn unpack_rgbe(v: u32) -> Vec3 {
    let c = unpack_r8g8b8a8(v);
    Vec3::new(c[0], c[1], c[2]) * approx_exp2(c[3] * 255.0 - 128.0)
}

/// RGBE accumulation as COMPUTE_LIGHT does it: packRGBE( add + unpackRGBE( acc ) ).
pub fn rgbe_add(acc: u32, add: Vec3) -> u32 {
    pack_rgbe(add + unpack_rgbe(acc))
}

// ---------------------------------------------------------------------------------------------
// BRDF (global.inc).

/// fresnelSchlick with the Schuler baked specular occlusion (no reflectance below 2 %).
pub fn fresnel_schlick(f0: Vec3, cos_theta: f32) -> Vec3 {
    let occl = (50.0 * f0.dot(Vec3::splat(0.3333))).clamp(0.0, 1.0);
    (f0 + (Vec3::splat(occl) - f0) * approx_pow((1.0 - cos_theta).clamp(0.0, 1.0), 5.0)).clamp(Vec3::ZERO, Vec3::ONE)
}

/// specBRDF: GGX with m = ( 1 - 0.8 smoothness )^4, Schlick-Smith visibility, fresnelSchlick( L.H ).
pub fn spec_brdf(n: Vec3, v: Vec3, l: Vec3, f0: Vec3, smoothness: f32) -> Vec3 {
    let h = (v + l).normalize();
    let mut m = 1.0 - smoothness * 0.8;
    m *= m;
    m *= m;
    let m2 = m * m;
    let n_dot_h = n.dot(h).clamp(0.0, 1.0);
    let mut spec = (n_dot_h * n_dot_h) * (m2 - 1.0) + 1.0;
    spec = m2 / (spec * spec + 1e-8);
    let gv = n.dot(v).clamp(0.0, 1.0) * (1.0 - m) + m;
    let gl = n.dot(l).clamp(0.0, 1.0) * (1.0 - m) + m;
    spec /= 4.0 * gv * gl + 1e-8;
    fresnel_schlick(f0, l.dot(h)) * spec
}

/// environmentBRDF: Lazarov 2013 analytical fit.
pub fn environment_brdf(n_dot_v: f32, smoothness: f32, f0: Vec3) -> Vec3 {
    let t1 = 0.095 + smoothness * (0.6 + 4.19 * smoothness);
    let t2 = n_dot_v + 0.025;
    let t3 = 9.5 * smoothness * n_dot_v;
    let a0 = t1 * t2 * approx_exp2(1.0 - 14.0 * n_dot_v);
    let a1 = 0.4 + 0.6 * (1.0 - approx_exp2(-t3));
    // lerp( a0, a1, f0 )
    Vec3::splat(a0) + (a1 - a0) * f0
}

/// global.inc ReconstructNormal: tangent-space normal from the two stored channels.
pub fn reconstruct_normal(xy: [f32; 2], front_facing: bool) -> Vec3 {
    let x = xy[0] * 2.0 - 1.0;
    let y = xy[1] * 2.0 - 1.0;
    let z = (1.0 - x * x - y * y).clamp(0.0, 1.0).sqrt();
    Vec3::new(x, y, if front_facing { z } else { -z })
}

// ---------------------------------------------------------------------------------------------
// Lights (lighting.inc PROCESS_LIGHT / COMPUTE_LIGHT).

/// lighting.inc `clip_min`.
pub const CLIP_MIN: f32 = 1.0 / 255.0;

/// One light as PROCESS_LIGHT sees it at a surface point, after the projection lookups.
#[derive(Clone, Copy, Debug)]
pub struct LightSample {
    pub pos: Vec3,
    /// unpackRGBE( colorPacked ): colour × intensity in the engine's radiance units.
    pub color: Vec3,
    pub spec_multiplier: f32,
    /// The falloff image at ( projTC.z, 0.5 ) and the projection image at projTC.xy (lightsAtlasMap .x).
    pub falloff: f32,
    pub proj_filter: f32,
    /// Shadow factor after the shadow fade (1 for lights without LF_CAST_SHADOWS).
    pub shadow: f32,
}

/// The surface inputs of lightingInput_t that the light loops read.
#[derive(Clone, Copy, Debug)]
pub struct Surface {
    pub position: Vec3,
    /// World normal (GetWorldSpaceNormal).
    pub normal: Vec3,
    /// normalize( -( position - view origin ) )
    pub view: Vec3,
    /// unpackR10G10B10( specular_packed )
    pub specular: Vec3,
    /// abs( smoothness )
    pub smoothness: f32,
}

/// Light attenuation of PROCESS_LIGHT: ( falloff² ) × ( projFilter² ); None where it skips the light
/// (attenuation <= clip_min / 256).
pub fn light_attenuation(falloff: f32, proj_filter: f32) -> Option<f32> {
    let a = (falloff * falloff) * (proj_filter * proj_filter);
    (a > CLIP_MIN / 256.0).then_some(a)
}

/// PROCESS_LIGHT's cull of the projected volume: projTC outside ( clip_min, 1 - clip_min ).
pub fn inside_volume(proj_tc: Vec3) -> bool {
    proj_tc.min_element() > CLIP_MIN && proj_tc.max_element() < 1.0 - CLIP_MIN
}

/// PROCESS_LIGHT + COMPUTE_LIGHT for a non-area light: accumulates into the RGBE diffuse and
/// specular words like the engine. Returns false where PROCESS_LIGHT `continue`s.
pub fn add_light(s: &Surface, l: &LightSample, diffuse: &mut u32, specular: &mut u32) -> bool {
    // PROCESS_LIGHT: unnormalised light vector for the back-facing cull
    let n_dot_l0 = s.normal.dot(l.pos - s.position).clamp(0.0, 1.0);
    if n_dot_l0 <= CLIP_MIN {
        return false;
    }
    let Some(att) = light_attenuation(l.falloff, l.proj_filter) else { return false };
    let color = l.color * l.shadow * att;
    // COMPUTE_LIGHT
    let lv = (l.pos - s.position).normalize();
    let n_dot_l = s.normal.dot(lv).clamp(0.0, 1.0);
    let spec = spec_brdf(s.normal, s.view, lv, s.specular, s.smoothness);
    *specular = rgbe_add(*specular, spec * l.spec_multiplier * color * n_dot_l);
    *diffuse = rgbe_add(*diffuse, color * n_dot_l);
    true
}

/// PROCESS_LIGHT's area-light branch (lightParms & LF_AREA_MASK), run after the attenuation:
/// `area` = ( areaPlane, boxMin = U, boxMax = V ) of the light record (`LightDef::area_records`).
/// Returns the moved light position (the reflection ray's hit on the area plane, clamped to the
/// rect / circle) and the colour factor falloff² ( saturate( projToOrigin )² ); None where it
/// `continue`s (the surface is behind the plane). COMPUTE_LIGHT then runs with these; area lights
/// also skip PROCESS_LIGHT's back-facing N.L cull.
pub fn area_light(s: &Surface, light_pos: Vec3, area: &[[f32; 4]; 3], parms: u32) -> Option<(Vec3, f32)> {
    let [pl, pu, pv] = *area;
    let mut plane = Vec3::new(pl[0], pl[1], pl[2]);
    let (u_dir, v_dir) = (Vec3::new(pu[0], pu[1], pu[2]), Vec3::new(pv[0], pv[1], pv[2]));
    let mut proj_to_origin = s.position.dot(plane) + pl[3];
    if proj_to_origin < 0.0 {
        return None;
    }
    let falloff_distance = plane.length();
    let falloff = proj_to_origin.clamp(0.0, 1.0);
    proj_to_origin /= falloff_distance;
    plane /= falloff_distance;
    // HLSL reflect( i, n ) = i - 2 dot( n, i ) n
    let reflect = |i: Vec3, n: Vec3| i - 2.0 * n.dot(i) * n;
    let mut mirror = reflect(-s.view, s.normal);
    // facing away: flip it to face the area light
    if mirror.dot(plane) > 0.0 {
        mirror = reflect(mirror, plane);
    }
    let proj_length = proj_to_origin / mirror.dot(plane);
    let hit_dir = (s.position - mirror * proj_length) - light_pos;
    let (mut u, mut v) = (hit_dir.dot(u_dir), hit_dir.dot(v_dir));
    let len = if parms & LF_AREA_MASK == LF_AREA_CIRCLE { (u * u + v * v).sqrt() } else { u.abs().max(v.abs()) };
    // (LF_AREA_REFLECTOR also writes projTC.xy here, which nothing reads afterwards)
    let len = len.min(1.0) / len;
    u *= len * pu[3];
    v *= len * pv[3];
    Some((light_pos + (u_dir * u + v_dir * v), falloff * falloff))
}

// ---------------------------------------------------------------------------------------------
// Environment probes (lighting.inc PROCESS_PROBE / COMPUTE_LIGHT_PROBE via DEFERRED_ENV_PROBES).

/// PROCESS_PROBE attenuation from the probe's projected coordinates: the unit cube mapped onto the
/// unit sphere, then the inner falloff ramp, squared. (Distance fade and dest alpha come after.)
pub fn probe_attenuation(proj_tc: Vec3, inner_falloff: f32) -> f32 {
    let c = proj_tc * 2.0 - Vec3::ONE;
    let yzx = Vec3::new(c.y, c.z, c.x);
    let zxy = Vec3::new(c.z, c.x, c.y);
    let sphere = c * (Vec3::ONE - yzx * yzx * 0.5 - zxy * zxy * 0.5 + (yzx * yzx * zxy * zxy / 3.0)).max(Vec3::ZERO).map(f32::sqrt);
    let a = (1.0 - (sphere.length() - inner_falloff) / (1.0 - inner_falloff + 1e-6)).clamp(0.0, 1.0);
    a * a
}

/// COMPUTE_LIGHT_PROBE's parallax-corrected lookup direction: the reflection ray clipped against the
/// probe box ( light_min, light_max ), seen from the probe position.
pub fn probe_lookup_dir(s: &Surface, probe_pos: Vec3, box_min: Vec3, box_max: Vec3) -> Vec3 {
    let r = reflect(-s.view, s.normal);
    let bmax = (box_max - s.position) / r;
    let bmin = (box_min - s.position) / r;
    let t = bmax.max(bmin).min_element();
    (s.position + r * t - probe_pos).normalize()
}

/// COMPUTE_LIGHT_PROBE: cube mip = 6 - 6 smoothness.
pub fn probe_mip(smoothness: f32) -> f32 {
    6.0 - 6.0 * smoothness
}

/// HLSL reflect( i, n ).
pub fn reflect(i: Vec3, n: Vec3) -> Vec3 {
    i - 2.0 * n.dot(i) * n
}

// ---------------------------------------------------------------------------------------------
// Irradiance (vmtr.inc APPLY_IRRADIANCE_CONTRIBUTION).

/// Diffuse ambient of dynamic geometry from the 4 RGB coefficients CALC_IRRADIANCE_CONTRIBUTION
/// fetched: ( ( c0 - n.y c1 ) + n.z c2 ) - n.x c3, × envIrradianceScale (renderparm default 1),
/// clamped at 0.
pub fn sh_irradiance(c: [Vec3; 4], n: Vec3, env_irradiance_scale: f32) -> Vec3 {
    let d = ((c[0] - n.y * c[1]) + n.z * c[2]) - n.x * c[3];
    (d * env_irradiance_scale.clamp(0.0, 1.0)).max(Vec3::ZERO)
}

// ---------------------------------------------------------------------------------------------
// The whole opaque pixel (COMPUTE_LIGHTING_IMPL + FP_MRT_OUTPUT + the deferred probe pass).

/// Final opaque colour: ( unpackRGBE( diffuse ) × unpackR10G10B10( albedo ) ) + unpackRGBE( specular ),
/// where diffuse starts from packRGBE( ambient + emissive ) and gathers the lights.
pub fn shade(albedo: Vec3, s: &Surface, ambient: Vec3, emissive: Vec3, lights: &[LightSample]) -> (Vec3, Vec3) {
    let albedo = unpack_r10g10b10(pack_r10g10b10(albedo));
    let mut diffuse = pack_rgbe(ambient + emissive);
    let mut specular = 0u32;
    for l in lights {
        add_light(s, l, &mut diffuse, &mut specular);
    }
    (unpack_rgbe(diffuse) * albedo, unpack_rgbe(specular))
}

// ---------------------------------------------------------------------------------------------
// GPU inputs built from the install: the light image atlas and the environment probe cubes.

/// `_lightimageatlas`: every light material's projection and falloff image sits at the
/// `lightprojatlasscalebias` / `lightfalloffatlasscalebias` its material decl gives (scale = image
/// size / atlas size, bias = position). gauslight (128²) has scale ( 1/32, 1/16 ), so the atlas is
/// 4096 × 2048. The images are BC4 (bimage format 24, generated/lightatlas/<image>.bimage) and their
/// positions are multiples of 16 texels, so the atlas is assembled from their blocks unchanged.
pub const LIGHT_ATLAS_SIZE: [u32; 2] = [4096, 2048];

/// The BC4 light atlas (mip 0 only: PROCESS_LIGHT reads it with tex2Dlod( .., 0 )).
pub struct LightAtlas {
    pub size: [u32; 2],
    pub blocks: Vec<u8>,
    placed: Vec<(String, [f32; 4])>,
}

impl Default for LightAtlas {
    fn default() -> Self {
        Self::empty(LIGHT_ATLAS_SIZE[0], LIGHT_ATLAS_SIZE[1])
    }
}

impl LightAtlas {
    /// A black atlas of `w` × `h` texels.
    pub fn empty(w: u32, h: u32) -> Self {
        LightAtlas { size: [w, h], blocks: vec![0; (w / 4 * h / 4) as usize * 8], placed: Vec::new() }
    }

    /// Places `generated/lightatlas/<image>.bimage` at `scale_bias` (atlas uv = uv * xy + zw) once.
    pub fn place(&mut self, c: &idres::Container, image: &str, scale_bias: [f32; 4]) -> anyhow::Result<()> {
        let key = image.to_ascii_lowercase();
        if self.placed.iter().any(|(k, sb)| *k == key && *sb == scale_bias) {
            return Ok(());
        }
        let bytes = c.read_by_name(&format!("generated/lightatlas/{key}.bimage"))?;
        let img = idres::bimage::BImage::parse(&bytes)?;
        anyhow::ensure!(img.format == 24, "{key}: format {} is not BC4", img.format);
        let mip = img.mips.iter().find(|m| m.level == 0 && m.dest_z == 0).ok_or_else(|| anyhow::anyhow!("{key}: no mip 0"))?;
        let [aw, ah] = self.size;
        let (x0, y0) = ((scale_bias[2] * aw as f32).round() as u32, (scale_bias[3] * ah as f32).round() as u32);
        let (w, h) = (mip.width, mip.height);
        anyhow::ensure!(
            x0 % 4 == 0 && y0 % 4 == 0 && x0 + w <= aw && y0 + h <= ah && ((scale_bias[0] * aw as f32) - w as f32).abs() < 0.5 && ((scale_bias[1] * ah as f32) - h as f32).abs() < 0.5,
            "{key}: {w}x{h} does not fit scale/bias {scale_bias:?}"
        );
        let src = &bytes[mip.data.clone()];
        let (bw, abw) = ((w / 4) as usize, (aw / 4) as usize);
        for by in 0..(h / 4) as usize {
            let row = &src[by * bw * 8..(by + 1) * bw * 8];
            let o = ((y0 / 4) as usize + by) * abw * 8 + (x0 / 4) as usize * 8;
            self.blocks[o..o + row.len()].copy_from_slice(row);
        }
        self.placed.push((key, scale_bias));
        Ok(())
    }
}

/// Size and mip count of the environment probe cubes (maps/<map>/lightprobes/<entity>_compressed.bimage:
/// bimage texture type 2 = cube, 128², 7 mips 128..2, format 22 = BC6H, faces as dest_z 0..5).
pub const PROBE_SIZE: u32 = 128;
pub const PROBE_MIPS: u32 = 7;

/// Bytes of one cube face with its mips (BC6H, 16 bytes per 4×4 block).
pub fn probe_cube_bytes() -> u32 {
    (0..PROBE_MIPS).map(|l| (PROBE_SIZE >> l).max(1).div_ceil(4).pow(2) * 16).sum()
}

/// Appends one probe cube's BC6H blocks in wgpu's layer-major order (face 0 mips 0..6, face 1, ..).
pub fn append_probe_cube(bytes: &[u8], out: &mut Vec<u8>) -> anyhow::Result<()> {
    let img = idres::bimage::BImage::parse(bytes)?;
    anyhow::ensure!(img.format == 22 && img.texture_type == 2 && img.width == PROBE_SIZE, "probe cube: format {} type {} size {}", img.format, img.texture_type, img.width);
    for face in 0..6 {
        for level in 0..PROBE_MIPS {
            let m = img.mips.iter().find(|m| m.dest_z == face && m.level == level).ok_or_else(|| anyhow::anyhow!("probe cube: no face {face} mip {level}"))?;
            let s = (PROBE_SIZE >> level).max(1);
            let need = (s.div_ceil(4) * s.div_ceil(4) * 16) as usize;
            anyhow::ensure!(m.data.len() >= need, "probe cube: short mip");
            out.extend_from_slice(&bytes[m.data.start..m.data.start + need]);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The scene buffer lighting.wgsl reads (all positions and planes in engine space, z up).

/// A light as the clustered lighting reads it ($lights: lightParms1..3, filled by 0x14187b700).
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuLight {
    /// projS, projT, projQ, falloffR planes (dot( p.xyz, x ) + p.w).
    pub planes: [[f32; 4]; 4],
    pub pos: Vec3,
    /// lightParms: LF_* flags; probes: probe id << 8 | ( int )( inner falloff × 255 ) << 16.
    pub parms: u32,
    /// unpackRGBE( colorPacked ) (packed here like the engine: FUN_1418813c0 is packRGBE).
    pub color: Vec3,
    pub spec_multiplier: f32,
    /// scaleBias.xy (falloff) and .zw (projection), R15G15B15A15 words.
    pub scale_bias: [u32; 4],
    /// Probes: boxMin.xyz + distance fade, boxMax.xyz.
    pub box_min: [f32; 4],
    pub box_max: [f32; 3],
    /// Area lights: areaPlane, boxMin (U), boxMax (V) (`LightDef::area_records`).
    pub area: [[f32; 4]; 3],
}

/// Packs `scale_bias` = ( scale.xy, bias.xy ) into two R15G15B15A15 words (unpackR15G15B15A15
/// divides by 32767) as the per-view light gather does (0x14187c440): ( int )( v × 32767 ),
/// truncating, clamped to [-32768, 32767], ( short )a << 16 | ( short )b.
pub fn pack_r15x4(v: [f32; 4]) -> [u32; 2] {
    let q = |x: f32| (((x * 32767.0) as i32).clamp(-0x8000, 0x7fff) as u32) & 0xffff;
    [(q(v[0]) << 16) | q(v[1]), (q(v[2]) << 16) | q(v[3])]
}

/// Everything a map gives the VT materials' lighting bindings.
pub struct SceneData {
    /// Per `build_scene` input: its index among the GPU lights (None: probes, dropped lights).
    pub light_indices: Vec<Option<u32>>,
    pub words: Vec<u32>,
    pub atlas: LightAtlas,
    pub probe_count: u32,
    pub probe_blocks: Vec<u8>,
    /// What the per-frame update needs (colour programs, fades), in GPU order.
    pub anim: SceneAnim,
}

impl Default for SceneData {
    fn default() -> Self {
        SceneData { light_indices: Vec::new(), words: SceneBuffer::pack(&[], &[], None, 1.0), atlas: LightAtlas::empty(4, 4), probe_count: 0, probe_blocks: Vec::new(), anim: SceneAnim::default() }
    }
}

/// vec4 slots per light / probe record (lighting.wgsl LIGHT_STRIDE / PROBE_STRIDE).
pub const LIGHT_STRIDE: usize = 10;
pub const PROBE_STRIDE: usize = 8;

pub struct SceneBuffer;

impl SceneBuffer {
    /// Layout (u32 words, 4 per vec4): [0] ( lights, probes, octree vec4 offset, leaves offset ),
    /// [1] ( records offset, ambient on, envIrradianceScale bits, 0 ), then lights, probes
    /// (smallest first), octree nodes (2 vec4 each), leaves (2 vec4 each), SH records (3 vec4 each,
    /// CALC_IRRADIANCE_CONTRIBUTION's packedSh0..2 order).
    pub fn pack(lights: &[GpuLight], probes: &[GpuLight], ambient: Option<&AmbientOctree>, env_irradiance_scale: f32) -> Vec<u32> {
        let mut w: Vec<u32> = vec![0; 8];
        let f = |x: f32| x.to_bits();
        let push_record = |w: &mut Vec<u32>, l: &GpuLight, probe: bool| {
            for p in &l.planes {
                w.extend(p.iter().map(|&x| f(x)));
            }
            w.extend([f(l.pos.x), f(l.pos.y), f(l.pos.z), l.parms]);
            let c = unpack_rgbe(pack_rgbe(l.color));
            w.extend([f(c.x), f(c.y), f(c.z), f(l.spec_multiplier)]);
            if probe {
                w.extend(l.box_min.iter().map(|&x| f(x)));
                w.extend([f(l.box_max[0]), f(l.box_max[1]), f(l.box_max[2]), 0]);
            } else {
                w.extend(l.scale_bias);
                for a in &l.area {
                    w.extend(a.iter().map(|&x| f(x)));
                }
            }
        };
        for l in lights {
            push_record(&mut w, l, false);
        }
        for p in probes {
            push_record(&mut w, p, true);
        }
        let octree_off = w.len() / 4;
        let (mut leaves_off, mut records_off) = (octree_off, octree_off);
        if let Some(t) = ambient {
            for n in &t.nodes {
                w.extend(n);
            }
            leaves_off = w.len() / 4;
            for l in &t.leaves {
                w.extend(l);
            }
            records_off = w.len() / 4;
            for r in &t.records {
                let c = r.c;
                w.extend([c[0].x, c[0].y, c[0].z, c[1].x, c[1].y, c[1].z, c[2].x, c[2].y, c[2].z, c[3].x, c[3].y, c[3].z].map(f));
            }
        }
        let on = ambient.is_some_and(|t| !t.nodes.is_empty()) as u32;
        w[..8].copy_from_slice(&[lights.len() as u32, probes.len() as u32, octree_off as u32, leaves_off as u32, records_off as u32, on, f(env_irradiance_scale), 0]);
        w
    }
}

// ---------------------------------------------------------------------------------------------
// idLight -> the GPU light (engine space).

/// idRenderLightParms.lightType (committed light +0x60 in 0x14187b700: 3 probe, 4 area).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    Point,
    Spot,
    Parallel,
    Probe,
    Area,
}

/// An idLight's render parms (reflection idRenderLightParms), in engine space.
#[derive(Clone, Debug)]
pub struct LightDef {
    pub name: String,
    pub kind: LightKind,
    pub origin: Vec3,
    /// Rows: the light's axes in world space.
    pub axis: [Vec3; 3],
    pub radius: Vec3,
    pub center: Vec3,
    /// spot / area frustum (relative to origin/axis); parallel: the direction the light comes from.
    pub target: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub start: Vec3,
    pub end: Vec3,
    /// lightColor.rgb × lightIntensity
    pub color: Vec3,
    pub spec_scale: f32,
    pub dynamic_only: bool,
    pub cast_shadows: bool,
    pub probe_inner_falloff: f32,
    /// lightMaterial (None: lights/defaultPointLight or lights/defaultProjectedLight).
    pub material: Option<String>,
    pub fade: VisibilityFade,
}

const LF_SPOT: u32 = 1;
const LF_PARALLEL: u32 = 2;
const LF_PROBE: u32 = 3;
const LF_CAST_SHADOWS: u32 = 1 << 2;
const LF_DINAMIC_ONLY: u32 = 1 << 3;
/// lightArea_t RECT / CIRCLE / REFLECTOR (0, 1, 2) -> LF_AREA_* ( shape + 1 ) << 4 (global.inc).
pub const LF_AREA_RECT: u32 = 1 << 4;
pub const LF_AREA_CIRCLE: u32 = 2 << 4;
pub const LF_AREA_MASK: u32 = 3 << 4;

fn plane(n: Vec3, d: f32) -> [f32; 4] {
    [n.x, n.y, n.z, d]
}

/// idMath::InvSqrt as the exe inlines it: rsqrtss of max( x, FLT_MIN ) and two Newton steps
/// `y = -0.5 y ( x y² - 3 )` (0x141879a70, 0x141a5abe0, 0x141a59fa0). The seed here is the exact
/// 1/sqrt instead of the 12-bit rsqrtss estimate; after two steps they agree to the last bit or two.
pub fn inv_sqrt_nr(x: f32) -> f32 {
    let x = x.max(f32::MIN_POSITIVE);
    let mut y = 1.0 / x.sqrt();
    y = (x * y * y - 3.0) * y * -0.5;
    (x * y * y - 3.0) * y * -0.5
}

/// The spot frustum (0x141a5abe0 / 0x141a5a870, light parms target +0xb0, right +0xbc, up +0xc8,
/// start +0xd4, end +0xe0): n = target / |target|, S = right × 0.5 |target| / |right|² and
/// T = up × -0.5 |target| / |up|², each offset by n × ( 0.5 - n·row ); Q = ( n, 0 ); the falloff row
/// of the second matrix is ( n·p - S ) / ( E - S ) with S = max( n·start, 8 ), E = max( n·end, 16 ).
fn spot_project(l: &LightDef) -> [[f32; 4]; 4] {
    let (t, r, u) = (l.target, l.right, l.up);
    let tl2 = (t.x * t.x + t.y * t.y) + t.z * t.z;
    let inv = inv_sqrt_nr(tl2);
    let n = t * inv;
    let len = inv * tl2;
    let rl2 = (r.x * r.x + r.y * r.y) + r.z * r.z;
    let ul2 = (u.x * u.x + u.y * u.y) + u.z * u.z;
    let s_row = r * (len * 0.5 / rl2);
    let t_row = u * (len * -0.5 / ul2);
    let ds = 0.5 - ((s_row.y * n.y + s_row.x * n.x) + s_row.z * n.z);
    let dt = 0.5 - ((t_row.y * n.y + t_row.x * n.x) + t_row.z * n.z);
    let near = ((n.y * l.start.y + n.x * l.start.x) + n.z * l.start.z).max(8.0);
    let far = ((n.x * l.end.x + n.y * l.end.y) + n.z * l.end.z).max(16.0);
    let k = 1.0 / (far - near);
    let s = n * ds + s_row;
    let tt = n * dt + t_row;
    [[s.x, s.y, s.z, 0.0], [tt.x, tt.y, tt.z, 0.0], [n.x, n.y, n.z, 0.0], [n.x * k, n.y * k, n.z * k, -(k * near)]]
}

/// The light's local projection (S, T, Q, falloff R) as the derive builds it (0x141a5b070 /
/// 0x141a5af50 from 0x141a5b8d0): LIGHT_SPOT gets the frustum, every other type (point, parallel,
/// probe and area) the box S = x·0.5/rx + 0.5, T = y·0.5/ry + 0.5, Q = 1, R = z·0.5/rz + 0.5.
fn local_project(l: &LightDef) -> [[f32; 4]; 4] {
    match l.kind {
        LightKind::Spot => spot_project(l),
        _ => {
            let r = l.radius;
            [[0.5 / r.x, 0.0, 0.0, 0.5], [0.0, 0.5 / r.y, 0.0, 0.5], [0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.5 / r.z, 0.5]]
        }
    }
}

/// A local plane in world space: world point x has local coordinates axis · ( x - origin ).
fn to_global(p: [f32; 4], axis: &[Vec3; 3], origin: Vec3) -> [f32; 4] {
    let n = axis[0] * p[0] + axis[1] * p[1] + axis[2] * p[2];
    [n.x, n.y, n.z, p[3] - n.dot(origin)]
}

impl LightDef {
    /// globalLightOrigin (0x141a59fa0 into committed light +0xab0): spot lights sit at `origin`;
    /// parallel lights 100000 units along lightCenter (world space, not rotated; z = 1 when it has
    /// no length); everything else at origin + axis · lightCenter.
    pub fn global_origin(&self) -> Vec3 {
        let a = &self.axis;
        let c = self.center;
        match self.kind {
            LightKind::Spot => self.origin,
            LightKind::Parallel => {
                let l2 = c.x * c.x + c.y * c.y + c.z * c.z;
                let inv = inv_sqrt_nr(l2);
                let z = if inv * l2 == 0.0 { 1.0 } else { c.z * inv };
                self.origin + Vec3::new(c.x * inv * 100000.0, c.y * inv * 100000.0, z * 100000.0)
            }
            _ => self.origin + a[0] * c.x + a[1] * c.y + a[2] * c.z,
        }
    }

    pub fn planes(&self) -> [[f32; 4]; 4] {
        local_project(self).map(|p| to_global(p, &self.axis, self.origin))
    }

    /// World-space corners of the light volume (for clustering bounds and probe boxes).
    pub fn corners(&self) -> Vec<Vec3> {
        let a = &self.axis;
        let w = |v: Vec3| self.origin + a[0] * v.x + a[1] * v.y + a[2] * v.z;
        match self.kind {
            LightKind::Spot => {
                let mut c = vec![w(Vec3::ZERO), w(self.start), w(self.end)];
                for sr in [-1.0, 1.0] {
                    for su in [-1.0, 1.0] {
                        c.push(w(self.target + self.right * sr + self.up * su));
                    }
                }
                c
            }
            _ => (0..8).map(|k| w(Vec3::new(if k & 1 == 1 { 1.0 } else { -1.0 }, if k & 2 == 2 { 1.0 } else { -1.0 }, if k & 4 == 4 { 1.0 } else { -1.0 }) * self.radius)).collect(),
        }
    }

    /// World bounds of the light volume (committed light +0xa98 / +0xaa4). INTERIM for spots: the
    /// AABB of origin, start, end and the target-plane corners (0x1402c5b80's frustum bounds are not
    /// ported).
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.corners().iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), &p| (a.min(p), b.max(p)))
    }

    /// The area-light records of 0x14187b700 (light parms target +0xb0, right +0xbc, up +0xc8, read
    /// as stored: unrotated): n = target / |target|², areaPlane = ( n, -n·globalLightOrigin ),
    /// boxMin = ( right / max( 1, |right|² ), max( 1, |right|² ) ), boxMax likewise from up; the
    /// second value divides specMultiplier: max( 1, |right| |up| ) (InvSqrt × length², two Newton steps).
    pub fn area_records(&self, origin: Vec3) -> ([[f32; 4]; 3], f32) {
        let (t, r, u) = (self.target, self.right, self.up);
        let inv = 1.0 / ((t.x * t.x + t.y * t.y) + t.z * t.z);
        let n = t * inv;
        let rl2 = (r.x * r.x + r.y * r.y) + r.z * r.z;
        let ul2 = (u.x * u.x + u.y * u.y) + u.z * u.z;
        let spec_div = (((inv_sqrt_nr(ul2) * ul2) * inv_sqrt_nr(rl2)) * rl2).max(1.0);
        let d = (n.x * origin.x + n.y * origin.y) + n.z * origin.z;
        let (rw, uw) = (rl2.max(1.0), ul2.max(1.0));
        let (rs, us) = (r * (1.0 / rw), u * (1.0 / uw));
        ([[n.x, n.y, n.z, -d], [rs.x, rs.y, rs.z, rw], [us.x, us.y, us.z, uw]], spec_div)
    }

    /// The fade centre (+0xac8): the middle of the world bounds (0x141a5b8d0).
    pub fn bounds_center(&self) -> Vec3 {
        let (mn, mx) = self.bounds();
        Vec3::new((mx.x + mn.x) * 0.5, (mx.y + mn.y) * 0.5, (mx.z + mn.z) * 0.5)
    }

    pub fn to_gpu(&self, scale_bias: [u32; 4], probe_id: u32) -> GpuLight {
        let mut parms = match self.kind {
            LightKind::Point => 0,
            // INTERIM: every area light is LIGHTAREA_RECT (the idLight ctor's areaLight shape and the
            // only one the intro uses); map.rs does not read areaLight.shape yet
            LightKind::Area => LF_AREA_RECT,
            LightKind::Spot => LF_SPOT,
            LightKind::Parallel => LF_PARALLEL,
            LightKind::Probe => LF_PROBE,
        };
        if self.dynamic_only {
            parms |= LF_DINAMIC_ONLY;
        }
        if self.cast_shadows {
            parms |= LF_CAST_SHADOWS;
        }
        let mut g = GpuLight { planes: self.planes(), pos: self.global_origin(), parms, color: self.color, spec_multiplier: self.spec_scale, scale_bias, ..Default::default() };
        if self.kind == LightKind::Area {
            let (area, spec_div) = self.area_records(g.pos);
            g.area = area;
            g.spec_multiplier /= spec_div;
        }
        if self.kind == LightKind::Probe {
            // 0x14187b700: probe id << 8, ( int )( lightProbeInnerFalloff × 255 ) << 16
            g.parms |= (probe_id << 8) | ((((self.probe_inner_falloff * 255.0) as i32) & 0xff) as u32) << 16;
            // boxMin / boxMax = the light's world bounds (committed light +0xa98 / +0xaa4, 0x14187b700);
            // boxMin.w = the per-view distance fade (the scene's dynamic buffer overrides this 1)
            let (mn, mx) = self.bounds();
            g.box_min = [mn.x, mn.y, mn.z, 1.0];
            g.box_max = mx.to_array();
        }
        g
    }
}

/// The light material's atlas placement: `lightprojatlasscalebias` / `lightfalloffatlasscalebias`
/// and their images (`lightprojectmap` / `lightfalloffmap`, last word, `.tga` dropped).
pub struct LightMaterial {
    pub proj: (String, [f32; 4]),
    pub falloff: (String, [f32; 4]),
    /// The colour program (flicker / pulse tables), None when the decl has none.
    pub program: Option<std::sync::Arc<ColorProgram>>,
}

impl LightMaterial {
    pub fn parse(decl: &str) -> Option<Self> {
        let line = |k: &str| decl.lines().map(str::trim).find(|l| l.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case(k)));
        let image = |k: &str| -> Option<String> {
            let l = line(k)?;
            let name = l.trim_end_matches('"').split_whitespace().last()?.trim_matches('"');
            Some(name.trim_end_matches(".tga").to_ascii_lowercase())
        };
        let sb = |k: &str| -> Option<[f32; 4]> {
            let l = line(k)?;
            let v: Vec<f32> = l.split(|c: char| c == '{' || c == '}' || c == ',' || c.is_whitespace()).filter_map(|t| t.parse().ok()).collect();
            (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
        };
        let program = Some(ColorProgram::parse(decl)).filter(|p| !p.is_empty()).map(std::sync::Arc::new);
        Some(LightMaterial { proj: (image("lightprojectmap")?, sb("lightprojatlasscalebias")?), falloff: (image("lightfalloffmap")?, sb("lightfalloffatlasscalebias")?), program })
    }

    pub fn scale_bias(&self) -> [u32; 4] {
        let f = pack_r15x4(self.falloff.1);
        let p = pack_r15x4(self.proj.1);
        [f[0], f[1], p[0], p[1]]
    }
}

// ---------------------------------------------------------------------------------------------
// Per-frame light state: the distance fade and the light material's colour program.

/// The visibility fade of a light: idLight edit maxVisibleRange / fadeVisibilityOver /
/// flipFadeVisibility (+0xda0 / +0xda4 / +0xda8, ctor 0x140944810: 0, 400, false), copied to the
/// render parms +0xfc / +0x100 / +0x104 (idRenderLightParms ctor 0x14119ae10: the same defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibilityFade {
    pub max_range: f32,
    pub fade_over: f32,
    pub flip: bool,
}

impl Default for VisibilityFade {
    fn default() -> Self {
        VisibilityFade { max_range: 0.0, fade_over: 400.0, flip: false }
    }
}

/// r_lightDistanceFadeMultiplier default (gamedata/exe/cvars.tsv: 1.0).
pub const LIGHT_DISTANCE_FADE_MULTIPLIER: f32 = 1.0;
/// Below this the faded light is culled (0x141879a70: FLT_EPSILON).
const FADE_CULL: f32 = 1.192_092_9e-7;

/// The per-view distance fade (0x141879a70): `center` is the light's world-bounds centre (committed
/// light +0xac8, set by the derive 0x141a5b8d0 as the middle of +0xa98..+0xaa4), `view` the view
/// origin (view +0x110). None: culled. A light without maxVisibleRange keeps fade 1.
pub fn distance_fade(f: &VisibilityFade, center: Vec3, view: Vec3, multiplier: f32) -> Option<f32> {
    if f.max_range <= 0.0 {
        return Some(1.0);
    }
    let (dx, dy, dz) = (view.x - center.x, view.y - center.y, view.z - center.z);
    let d2 = (dy * dy + dx * dx) + dz * dz;
    let max_range = f.max_range * multiplier;
    let over = multiplier * f.fade_over;
    let mut fade = if over < max_range {
        let dist = inv_sqrt_nr(d2) * d2;
        ((dist - (max_range - over)) / over).min(1.0).max(0.0)
    } else {
        0.0
    };
    if !f.flip {
        fade = 1.0 - fade;
    }
    (fade >= FADE_CULL).then_some(fade)
}

/// One operand of a light material expression (`generated/decls/material/lights/*.decl` lines
/// `tempN <expr>` / `lightcolor[.mask] <expr>`): a constant, a register with a swizzle, or a decl
/// table looked up at an operand's x.
#[derive(Clone, Debug, PartialEq)]
enum Operand {
    Num(f32),
    Reg(String, [usize; 4]),
    Table(String, Box<Operand>),
}

#[derive(Clone, Debug, PartialEq)]
struct Stmt {
    dst: String,
    mask: Vec<usize>,
    a: Operand,
    op: Option<(char, Operand)>,
}

fn swizzle(sw: &str) -> Option<[usize; 4]> {
    let c: Vec<usize> = sw.chars().map(|ch| "xyzw".find(ch)).collect::<Option<_>>()?;
    match c.len() {
        1 => Some([c[0]; 4]),
        4 => Some([c[0], c[1], c[2], c[3]]),
        _ => None,
    }
}

fn operand(tok: &str) -> Option<Operand> {
    if let Ok(v) = tok.parse::<f32>() {
        return Some(Operand::Num(v));
    }
    if let Some(open) = tok.find('[') {
        let inner = tok[open + 1..].strip_suffix(']')?;
        return Some(Operand::Table(tok[..open].to_ascii_lowercase(), Box::new(operand(inner)?)));
    }
    let (name, sw) = match tok.split_once('.') {
        Some((n, s)) => (n, swizzle(s)?),
        None => (tok, [0, 1, 2, 3]),
    };
    Some(Operand::Reg(name.to_ascii_lowercase(), sw))
}

type Regs<'a> = std::collections::HashMap<&'a str, [f32; 4]>;

/// The renderparm program of a light material: what the renderer evaluates for every visible
/// light each frame (0x14187c440 evaluates the light's material parms with the light's override
/// block, then reads lightColor and lightColorScale). Only the statements that feed those two are
/// kept (temps and `lightcolor*` assignments; `lightrotation` and other parms are dropped).
/// INTERIM: the statement semantics (vec4 registers, scalars broadcast, `dst.mask` takes the
/// same components of the result, a table index is its operand's x, idDeclTable lookup) are read
/// from the exported decl text, not from the exe's expression VM.
#[derive(Clone, Debug, Default)]
pub struct ColorProgram {
    stmts: Vec<Stmt>,
    /// Tables by name, `generated/decls/table/<name>.decl` (idfx::table, idDeclTable lookup).
    pub tables: std::collections::HashMap<String, idfx::table::Table>,
}

impl ColorProgram {
    pub fn parse(decl: &str) -> Self {
        let mut stmts = Vec::new();
        for line in decl.lines() {
            let mut it = line.split_whitespace();
            let Some(key) = it.next() else { continue };
            let rest: Vec<&str> = it.collect();
            let key = key.to_ascii_lowercase();
            let (base, mask) = key.split_once('.').unwrap_or((key.as_str(), "xyzw"));
            let is_temp = base.strip_prefix("temp").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            if !is_temp && base != "lightcolor" && base != "lightcolorscale" {
                continue;
            }
            let Some(mask) = mask.chars().map(|ch| "xyzw".find(ch)).collect::<Option<Vec<_>>>() else { continue };
            let parsed = match rest.as_slice() {
                [a] => operand(a).map(|a| (a, None)),
                [a, op, b] if op.len() == 1 && "+-*/".contains(*op) => {
                    operand(a).zip(operand(b)).map(|(a, b)| (a, Some((op.chars().next().unwrap(), b))))
                }
                _ => None,
            };
            if let Some((a, op)) = parsed {
                stmts.push(Stmt { dst: base.to_string(), mask, a, op });
            }
        }
        ColorProgram { stmts, tables: Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.stmts.is_empty()
    }

    /// Names of the tables the program reads.
    pub fn table_names(&self) -> Vec<String> {
        fn walk(o: &Operand, out: &mut Vec<String>) {
            if let Operand::Table(n, i) = o {
                out.push(n.clone());
                walk(i, out);
            }
        }
        let mut out = Vec::new();
        for st in &self.stmts {
            walk(&st.a, &mut out);
            if let Some((_, b)) = &st.op {
                walk(b, &mut out);
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Runs the program: `time` / `systime` are the Time / SysTime renderparms (seconds,
    /// `renderparm_time`), `color` the light's lightColor override (rgb, w = 1). Returns
    /// lightColor.rgb × lightColorScale.
    pub fn eval(&self, time: f32, systime: f32, color: Vec3) -> Vec3 {
        let mut regs: Regs = Default::default();
        regs.insert("time", [time; 4]);
        regs.insert("systime", [systime; 4]);
        regs.insert("lightcolor", [color.x, color.y, color.z, 1.0]);
        regs.insert("lightcolorscale", [1.0; 4]);
        for st in &self.stmts {
            let a = self.operand(&st.a, &regs);
            let v = match &st.op {
                None => a,
                Some((op, b)) => {
                    let b = self.operand(b, &regs);
                    std::array::from_fn(|i| match op {
                        '+' => a[i] + b[i],
                        '-' => a[i] - b[i],
                        '*' => a[i] * b[i],
                        _ => a[i] / b[i],
                    })
                }
            };
            let dst = regs.entry(st.dst.as_str()).or_insert([0.0; 4]);
            for &c in &st.mask {
                dst[c] = v[c];
            }
        }
        let c = regs["lightcolor"];
        let k = regs["lightcolorscale"][0];
        Vec3::new(c[0] * k, c[1] * k, c[2] * k)
    }

    fn operand(&self, o: &Operand, regs: &Regs) -> [f32; 4] {
        match o {
            Operand::Num(v) => [*v; 4],
            Operand::Reg(n, sw) => {
                let r = regs.get(n.as_str()).copied().unwrap_or([0.0; 4]);
                sw.map(|i| r[i])
            }
            Operand::Table(n, i) => {
                let x = self.operand(i, regs)[0];
                [self.tables.get(n).map_or(0.0, |t| t.lookup(x)); 4]
            }
        }
    }
}

/// The Time / SysTime renderparms (0x1417b5e07): ( float )( ms & 0x7fffff ) × 0.001.
pub fn renderparm_time(ms: u64) -> f32 {
    ((ms & 0x7f_ffff) as i32) as f32 * 0.001
}

/// What the per-frame light update needs for one GPU light or probe (`SceneAnim`).
#[derive(Clone, Debug)]
pub struct LightAnim {
    pub color: Vec3,
    pub program: Option<std::sync::Arc<ColorProgram>>,
    pub fade: VisibilityFade,
    pub center: Vec3,
    pub probe: bool,
}

/// Per-frame inputs of the engine lights, in GPU order (lights, then probes).
#[derive(Clone, Debug, Default)]
pub struct SceneAnim {
    pub lights: Vec<LightAnim>,
}

impl SceneAnim {
    /// One vec4 per GPU light / probe: lights ( unpackRGBE( packRGBE( colour × fade ) ), 0 ) with
    /// colour = the program's lightColor × lightColorScale (0x14187c440, packed by 0x14187b700),
    /// zero when culled; probes ( 0, 0, 0, fade ) (boxMin.w; the probe colour is not faded).
    pub fn evaluate(&self, time: f32, systime: f32, view: Vec3, out: &mut Vec<[f32; 4]>) {
        out.clear();
        for l in &self.lights {
            let fade = distance_fade(&l.fade, l.center, view, LIGHT_DISTANCE_FADE_MULTIPLIER);
            if l.probe {
                out.push([0.0, 0.0, 0.0, fade.unwrap_or(0.0)]);
                continue;
            }
            let Some(fade) = fade else {
                out.push([0.0; 4]);
                continue;
            };
            let c = l.program.as_ref().map_or(l.color, |p| p.eval(time, systime, l.color));
            let faded = c * fade;
            // 0x14187c440: a zero colour, or a faded one with |colour × fade|² < 0.0001, drops the light
            let culled = c == Vec3::ZERO || (fade != 1.0 && faded.length_squared() < 0.0001);
            let c = if culled { Vec3::ZERO } else { unpack_rgbe(pack_rgbe(faded)) };
            out.push([c.x, c.y, c.z, 0.0]);
        }
    }
}

/// Builds a map's engine scene: run-time lights with their light materials placed in the atlas,
/// probes with their cubes (maps/<map>/lightprobes/<entity>_compressed.bimage), the ambient octree
/// (maps/<map>/<map>.ambientsh). Missing pieces are skipped with a log line.
pub fn build_scene(c: &idres::Container, map: &str, defs: &[LightDef]) -> SceneData {
    let mut atlas = LightAtlas::default();
    let mut lights = Vec::new();
    let mut probes: Vec<(f32, LightDef)> = Vec::new();
    let mut mats: std::collections::HashMap<String, Option<LightMaterial>> = Default::default();
    let mut light_indices = Vec::with_capacity(defs.len());
    let mut anim = SceneAnim::default();
    for d in defs {
        light_indices.push(None);
        if d.kind == LightKind::Probe {
            let vol = d.radius.x * d.radius.y * d.radius.z;
            probes.push((vol, d.clone()));
            continue;
        }
        let name = d.material.clone().unwrap_or_else(|| if matches!(d.kind, LightKind::Spot | LightKind::Area) { "lights/defaultprojectedlight" } else { "lights/defaultpointlight" }.into()).to_ascii_lowercase();
        let lm = mats.entry(name.clone()).or_insert_with(|| {
            let text = c.read_by_name(&format!("generated/decls/material/{name}.decl")).ok()?;
            let mut m = LightMaterial::parse(&String::from_utf8_lossy(&text))?;
            if let Some(p) = m.program.as_mut().and_then(std::sync::Arc::get_mut) {
                for t in p.table_names() {
                    let table = c.read_by_name(&format!("generated/decls/table/{t}.decl")).map_err(anyhow::Error::from).and_then(|b| idfx::table::Table::parse(&String::from_utf8_lossy(&b)));
                    match table {
                        Ok(table) => {
                            p.tables.insert(t, table);
                        }
                        Err(e) => eprintln!("lighting: {name}: table {t}: {e:#}"),
                    }
                }
            }
            for (img, sb) in [&m.proj, &m.falloff] {
                if let Err(e) = atlas.place(c, img, *sb) {
                    eprintln!("lighting: light atlas: {e:#}");
                }
            }
            Some(m)
        });
        let Some(lm) = lm else {
            eprintln!("lighting: {}: no light material {name}", d.name);
            continue;
        };
        *light_indices.last_mut().unwrap() = Some(lights.len() as u32);
        lights.push(d.to_gpu(lm.scale_bias(), 0));
        anim.lights.push(LightAnim { color: d.color, program: lm.program.clone(), fade: d.fade, center: d.bounds_center(), probe: false });
    }
    // PROCESS_PROBE: "Probes are sorted from smallest to biggest" (INTERIM key: box volume)
    probes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut probe_blocks = Vec::new();
    let mut gpu_probes = Vec::new();
    for (_, d) in &probes {
        let path = format!("maps/{map}/lightprobes/{}_compressed.bimage", d.name);
        match c.read_by_name(&path).map_err(anyhow::Error::from).and_then(|b| append_probe_cube(&b, &mut probe_blocks)) {
            Ok(()) => {
                let id = gpu_probes.len() as u32;
                gpu_probes.push(d.to_gpu([0; 4], id));
                anim.lights.push(LightAnim { color: d.color, program: None, fade: d.fade, center: d.bounds_center(), probe: true });
            }
            Err(e) => eprintln!("lighting: probe {}: {e:#}", d.name),
        }
    }
    let leaf = map.rsplit('/').next().unwrap_or(map);
    let ambient = c.read_by_name(&format!("maps/{map}/{leaf}.ambientsh")).map_err(anyhow::Error::from).and_then(|b| AmbientOctree::parse(&b));
    let ambient = match ambient {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("lighting: ambient octree: {e:#}");
            None
        }
    };
    if let Some(t) = &ambient {
        eprintln!("lighting: ambient octree {} nodes, {} leaves, {} samples", t.nodes.len(), t.leaves.len(), t.records.len());
    }
    eprintln!("lighting: {} lights, {} probes", lights.len(), gpu_probes.len());
    // envIrradianceScale: renderparm default 1 (generated/decls/renderparm/envirradiancescale.decl)
    let words = SceneBuffer::pack(&lights, &gpu_probes, ambient.as_ref(), 1.0);
    let animated = anim.lights.iter().filter(|l| l.program.is_some()).count();
    let faded = anim.lights.iter().filter(|l| l.fade.max_range > 0.0).count();
    eprintln!("lighting: {animated} lights with colour programs, {faded} with a distance fade");
    SceneData { light_indices, words, atlas, probe_count: gpu_probes.len() as u32, probe_blocks, anim }
}

// ---------------------------------------------------------------------------------------------
// The ambient octree (maps/<map>/<map>.ambientsh), as idAmbientLighting builds its GPU atlases.

/// .ambientsh magic (idAmbientLighting load 0x1418981b0).
const AMBIENTSH_MAGIC: u32 = 0x21155702;
/// AMBIENT_MIN_WORLD / AMBIENT_MAX_WORLD (vertex.inc; root bounds in 0x141898a40).
pub const AMBIENT_WORLD: i32 = 32768;

/// One SH sample of the file: the 4 RGB coefficients the GPU uses, after the load-time processing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShRecord {
    pub c: [Vec3; 4],
    pub pos: [i16; 3],
}

/// half -> float as 0x141896cf0 does it: ( ( h & 0x7fff ) << 13 ) × 2^112, ≥ 65536 -> inf, sign.
pub fn f16_to_f32(h: u16) -> f32 {
    let mut v = f32::from_bits(((h & 0x7fff) as u32) << 13) * f32::from_bits(0x77800000); // 2^112
    if v >= 65536.0 {
        v = f32::from_bits(v.to_bits() | 0x7f800000);
    }
    f32::from_bits(((h as u32 & 0x8000) << 16) | v.to_bits())
}

/// float -> half as 0x141896cf0 does it: truncating, exponent <= 0 flushes to 0, overflow 0x7bff.
pub fn f32_to_f16(f: f32) -> u16 {
    let b = f.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 0x70;
    if e <= 0 {
        0
    } else if e < 0x1f {
        sign | ((b & 0x7fffff) >> 13) as u16 | ((e as u16) << 10)
    } else {
        sign | 0x7bff
    }
}

impl ShRecord {
    /// A 60-byte big-endian record: 9 RGB coefficients as f16 (coefficient-major, only the first
    /// 4 are used), then the sample position as 3 × i16 (byte swap 0x1418989a0).
    fn parse(b: &[u8]) -> Self {
        let h = |i: usize| u16::from_be_bytes([b[i * 2], b[i * 2 + 1]]);
        let mut c = [Vec3::ZERO; 4];
        for (k, v) in c.iter_mut().enumerate() {
            *v = Vec3::new(f16_to_f32(h(k * 3)), f16_to_f32(h(k * 3 + 1)), f16_to_f32(h(k * 3 + 2)));
        }
        // 0x141896cf0: per channel, L1 is clamped to |L0| (length > FLT_MIN), then everything is
        // stored as truncated f16 in the RGBA16F SH atlas.
        for ch in 0..3 {
            let l1 = Vec3::new(c[1][ch], c[2][ch], c[3][ch]);
            let len = l1.length();
            if len > f32::MIN_POSITIVE {
                let s = (c[0][ch].abs() / len).min(1.0);
                c[1][ch] *= s;
                c[2][ch] *= s;
                c[3][ch] *= s;
            }
        }
        for v in &mut c {
            *v = Vec3::new(f16_to_f32(f32_to_f16(v.x)), f16_to_f32(f32_to_f16(v.y)), f16_to_f32(f32_to_f16(v.z)));
        }
        let p = |o: usize| i16::from_be_bytes([b[o], b[o + 1]]);
        ShRecord { c, pos: [p(0x36), p(0x38), p(0x3a)] }
    }
}

/// The octree as the GPU walks it: nodes of 8 children (x + 2y + 4z), each an inner node index or
/// LEAF | leaf index; a leaf holds the SH record of each of its 8 corners (same order), sampled
/// trilinearly with the position inside the cell the descent stopped in.
#[derive(Default)]
pub struct AmbientOctree {
    pub nodes: Vec<[u32; 8]>,
    pub leaves: Vec<[u32; 8]>,
    pub records: Vec<ShRecord>,
}

pub const LEAF: u32 = 0x4000_0000;

impl AmbientOctree {
    pub fn parse(b: &[u8]) -> anyhow::Result<Self> {
        let be = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        anyhow::ensure!(b.len() >= 12 && be(0) == AMBIENTSH_MAGIC, "not an .ambientsh file");
        let (n, m) = (be(4) as usize, be(8) as usize);
        anyhow::ensure!(b.len() >= 12 + n * 32 + m * 60, ".ambientsh: {n} nodes, {m} records do not fit");
        let file_nodes: Vec<[u32; 8]> = (0..n).map(|i| std::array::from_fn(|k| be(12 + i * 32 + k * 4))).collect();
        let records = (0..m).map(|i| ShRecord::parse(&b[12 + n * 32 + i * 60..])).collect();
        let mut t = AmbientOctree { nodes: Vec::new(), leaves: Vec::new(), records };
        if n > 0 {
            let w = AMBIENT_WORLD;
            t.build(&file_nodes, 0, [-w, -w, -w, w, w, w]);
        }
        Ok(t)
    }

    /// 0x141897a70 on a fresh copy of the tree (0x141898560 first unshares nodes referenced twice,
    /// so every reference has its own bounds): returns the output node index.
    fn build(&mut self, file: &[[u32; 8]], node: usize, b: [i32; 6]) -> u32 {
        let out = self.nodes.len();
        self.nodes.push([0; 8]);
        let children = file[node];
        let mid = [(b[3] + b[0]) / 2, (b[4] + b[1]) / 2, (b[5] + b[2]) / 2];
        for i in 0..8 {
            let bit = [i & 1, (i >> 1) & 1, (i >> 2) & 1];
            let mut cb = [0; 6];
            for a in 0..3 {
                cb[a] = if bit[a] == 0 { b[a] } else { mid[a] };
                cb[a + 3] = if bit[a] == 0 { mid[a] } else { b[a + 3] };
            }
            let v = children[i];
            if v & LEAF == 0 {
                let child = self.build(file, v as usize, cb);
                self.nodes[out][i] = child;
                continue;
            }
            if children.iter().all(|c| c & LEAF != 0) {
                // all eight children are samples: one leaf over the whole node, its corners the
                // nearest of the children's samples
                let recs: [u32; 8] = std::array::from_fn(|k| children[k] & !LEAF & 0x7fff_ffff);
                let leaf = self.leaf(&recs, b);
                self.nodes[out] = [LEAF | leaf; 8];
                break;
            }
            let r = v & !LEAF & 0x7fff_ffff;
            let leaf = self.leaf(&[r; 8], cb);
            self.nodes[out][i] = LEAF | leaf;
        }
        out as u32
    }

    /// Corner records of a leaf: candidates are the records up to the first repeat of the previous
    /// one; each corner of `b` takes the nearest candidate position (first wins ties).
    fn leaf(&mut self, recs: &[u32; 8], b: [i32; 6]) -> u32 {
        let mut n = 1;
        while n < 8 && recs[n] != recs[n - 1] {
            n += 1;
        }
        let corners: [u32; 8] = std::array::from_fn(|k| {
            let c = [b[(k & 1) * 3], b[((k >> 1) & 1) * 3 + 1], b[((k >> 2) & 1) * 3 + 2]];
            let d2 = |r: u32| {
                let p = self.records.get(r as usize).map_or([0; 3], |s| s.pos);
                (0..3).map(|a| (p[a] as i64 - c[a] as i64).pow(2)).sum::<i64>()
            };
            let mut best = 0;
            for j in 1..n {
                if d2(recs[j]) < d2(recs[best]) {
                    best = j;
                }
            }
            recs[best]
        });
        self.leaves.push(corners);
        (self.leaves.len() - 1) as u32
    }

    /// The four coefficients at engine-space `p`, as CALC_IRRADIANCE_CONTRIBUTION fetches them
    /// (before × $gpuAmbientParms.y).
    pub fn sample(&self, p: Vec3) -> [Vec3; 4] {
        if self.nodes.is_empty() {
            return [Vec3::ZERO; 4];
        }
        let w = AMBIENT_WORLD as f32;
        let mut q = ((p + Vec3::splat(w)) / (2.0 * w)).clamp(Vec3::ZERO, Vec3::ONE);
        let mut node = 0usize;
        for _ in 0..10 {
            let o = (q * 2.0).floor().min(Vec3::ONE);
            let child = o.x as usize + 2 * o.y as usize + 4 * o.z as usize;
            let v = self.nodes[node][child];
            q = (q * 2.0).fract();
            if v & LEAF != 0 {
                let corners = self.leaves[(v & !LEAF) as usize];
                let mut c = [Vec3::ZERO; 4];
                for (k, &r) in corners.iter().enumerate() {
                    let wx = if k & 1 == 1 { q.x } else { 1.0 - q.x };
                    let wy = if (k >> 1) & 1 == 1 { q.y } else { 1.0 - q.y };
                    let wz = if (k >> 2) & 1 == 1 { q.z } else { 1.0 - q.z };
                    let s = self.records.get(r as usize).map_or([Vec3::ZERO; 4], |s| s.c);
                    for i in 0..4 {
                        c[i] += s[i] * (wx * wy * wz);
                    }
                }
                return c;
            }
            node = v as usize;
        }
        [Vec3::ZERO; 4]
    }
}

// ---------------------------------------------------------------------------------------------
// Emissive of non-lightmapped VT surfaces (dynamic models: hands, weapons, AI, props)

/// The renderparm factor of lighting.inc COMPUTE_SHADING's emissive for models that are not
/// combined world geometry: `emissive = $bloomMaskScale.x × mask² × $colorScale.x × $color.xyz ×
/// $bloomColor.xyz` (mask = page layer 2 red); this returns everything but mask². Values come from
/// the material decl's renderparm lines (`bloommaskscale 500.000000`, `bloomcolor { r, g, b, a }`,
/// `color { .. }`, `colorscale ..`), otherwise the renderparm defaults
/// (generated/decls/renderparm/bloommaskscale.decl `{ Vec 8.0 }`, bloomcolor / color
/// `{ 1, 1, 1, 1 } srgba`, colorscale `{ Vec 1 }`).
/// INTERIM: parm programs (`bloomcolor.xyz temp1 * bloomcolor`) are not evaluated (the constant or
/// default stands); `srgba` parms are linearised with SRGBlinear (the renderparm parser 0x1417561f0
/// tags them, the conversion site is not traced); the entity's renderModelInfo color / colorScale
/// (idRenderModelInfo +0x3c / +0x4c) are taken as their defaults 1.
pub fn material_emissive(decl: &str) -> Vec3 {
    let (mut mask_scale, mut color_scale, mut color, mut bloom) = (8.0f32, 1.0f32, Vec3::ONE, Vec3::ONE);
    for line in decl.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(char::is_whitespace) else { continue };
        let nums: Option<Vec<f32>> = rest.trim().trim_start_matches('{').trim_end_matches('}').split(',').map(|v| v.trim().parse().ok()).collect();
        let Some(nums) = nums.filter(|n| !n.is_empty()) else { continue };
        // a scalar sets every component (idRenderParm Vec with one value)
        let rgb = || Vec3::new(nums[0], *nums.get(1).unwrap_or(&nums[0]), *nums.get(2).unwrap_or(&nums[0]));
        let lin = |c: Vec3| Vec3::new(degamma(c.x), degamma(c.y), degamma(c.z));
        match key.to_ascii_lowercase().as_str() {
            "bloommaskscale" => mask_scale = nums[0],
            "colorscale" => color_scale = nums[0],
            "color" => color = lin(rgb()),
            "bloomcolor" => bloom = lin(rgb()),
            _ => {}
        }
    }
    color * bloom * (mask_scale * color_scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol * b.abs().max(1e-6)
    }

    /// 0x14187b700's area records and lighting.inc's LF_AREA branch: a 128 × 128 rect light 100
    /// units above the floor, facing down.
    #[test]
    fn area_light_rect() {
        let l = LightDef {
            name: "a".into(),
            kind: LightKind::Area,
            origin: Vec3::new(0.0, 0.0, 100.0),
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
            radius: Vec3::splat(320.0),
            center: Vec3::ZERO,
            target: Vec3::new(0.0, 0.0, -100.0),
            right: Vec3::new(0.0, -64.0, 0.0),
            up: Vec3::new(64.0, 0.0, 0.0),
            start: Vec3::ZERO,
            end: Vec3::ZERO,
            color: Vec3::ONE,
            spec_scale: 1.0,
            dynamic_only: false,
            cast_shadows: false,
            probe_inner_falloff: 0.0,
            material: None,
            fade: VisibilityFade::default(),
        };
        let g = l.to_gpu([0; 4], 0);
        assert_eq!(g.parms & LF_AREA_MASK, LF_AREA_RECT);
        // n = target / |target|² = ( 0, 0, -0.01 ), plane w = -n·origin = 1; |right| |up| = 4096
        let pl = g.area[0];
        assert!(pl[0] == 0.0 && pl[1] == 0.0 && close(pl[2], -0.01, 1e-6) && close(pl[3], 1.0, 1e-6));
        assert_eq!(g.area[1], [0.0, -1.0 / 64.0, 0.0, 4096.0]);
        assert_eq!(g.area[2], [1.0 / 64.0, 0.0, 0.0, 4096.0]);
        assert!((g.spec_multiplier - 1.0 / 4096.0).abs() < 1e-9);
        let surf = |p: Vec3| Surface { position: p, normal: Vec3::Z, view: Vec3::Z, specular: Vec3::splat(0.04), smoothness: 0.5 };
        // under the rect: the light moves above the reflection hit; falloff 1 on the target plane
        let (pos, k) = area_light(&surf(Vec3::new(10.0, 20.0, 0.0)), g.pos, &g.area, g.parms).unwrap();
        assert!(pos.distance(Vec3::new(10.0, 20.0, 100.0)) < 1e-3 && k == 1.0);
        // outside it: clamped to the rect edge (64 along up); half way down: falloff 0.5²
        let (pos, k) = area_light(&surf(Vec3::new(100.0, 0.0, 50.0)), g.pos, &g.area, g.parms).unwrap();
        assert!(pos.distance(Vec3::new(64.0, 0.0, 100.0)) < 1e-3 && (k - 0.25).abs() < 1e-6);
        // behind the plane: skipped
        assert!(area_light(&surf(Vec3::new(0.0, 0.0, 150.0)), g.pos, &g.area, g.parms).is_none());
        // circle: clamped by the radius, not the larger axis
        let (pos, _) = area_light(&surf(Vec3::new(64.0, -64.0, 0.0)), g.pos, &g.area, LF_AREA_CIRCLE).unwrap();
        let h = 64.0 / 2f32.sqrt();
        assert!(pos.distance(Vec3::new(h, -h, 100.0)) < 1e-3);
    }

    #[test]
    fn material_emissive_parms() {
        // no renderparm lines: bloomMaskScale 8 × colorScale 1 × color 1 × bloomColor 1
        assert_eq!(material_emissive("{\nambientprogram\toutsidegun\n}"), Vec3::splat(8.0));
        // models/weapons/bfg/mod_singleshot: bloommaskscale 500, color { 0.568627, 1, 0.262745, 1 } (srgba)
        let e = material_emissive("{\nbloommaskscale\t500.000000\ncolor\t{ 0.568627, 1.000000, 0.262745, 1.000000 }\n}");
        assert!(close(e.x, 500.0 * degamma(0.568627), 1e-6) && e.y == 500.0 && close(e.z, 500.0 * degamma(0.262745), 1e-6));
        // scalar bloomcolor, colorscale; a parm program line is skipped (default stays)
        let e = material_emissive("bloomcolor\t0.5\ncolorscale\t2\nbloommaskscale\ttemp1 * bloommaskscale\nbloomcolor.xyz\ttemp2 * bloomcolor");
        assert!(close(e.x, 16.0 * degamma(0.5), 1e-6) && e.x == e.y && e.y == e.z);
    }

    #[test]
    fn approx_helpers() {
        // ApproxPow( 0.5, 5 ): 5 * 0x3f000000 - 4 * 127 * 2^23 = 0x3d000000 = 2^-5 exactly
        assert_eq!(approx_pow(0.5, 5.0), 0.03125);
        // base 0: 5 * 0 - 4 * 127 * 2^23 < 0 -> uint 0 -> 0.0
        assert_eq!(approx_pow(0.0, 5.0), 0.0);
        // ApproxExp2 / ApproxLog2 are exact on powers of two
        assert_eq!(approx_exp2(-13.0), 2f32.powi(-13));
        assert_eq!(approx_log2(8.0), 3.0);
        // ApproxLog2( 1.5 ) = ( 0x3fc00000 / 2^23 ) - 127 = 127.5 - 127
        assert_eq!(approx_log2(1.5), 0.5);
        // fastSqrtNR0( 1 ): 0x1fbd1df5 + ( 0x3f800000 >> 1 ) = 0x3f7d1df5
        assert_eq!(fast_sqrt_nr0(1.0), f32::from_bits(0x3f7d1df5));
    }

    #[test]
    fn rgbe_round_trip() {
        // 3.0: exponent ceil( log2~ 3 ) = ceil( 1.5 ) = 2, mantissa 0.75 -> 191/255, 4 * 191/255
        let v = unpack_rgbe(pack_rgbe(Vec3::new(3.0, 1.0, 0.0)));
        assert!(close(v.x, 4.0 * 191.0 / 255.0, 1e-6), "{v}");
        assert!(close(v.y, 4.0 * 63.0 / 255.0, 1e-6), "{v}");
        assert_eq!(v.z, 0.0);
        assert_eq!(unpack_rgbe(pack_rgbe(Vec3::ZERO)), Vec3::ZERO);
    }

    #[test]
    fn spec_brdf_head_on() {
        // N = V = L: H = N, N.H = 1. smoothness 1: m = 0.2^4 = 0.0016, m2 = 2.56e-6;
        // D = m2 / ( m2^2 + 1e-8 ) = 255.832; Gv = Gl = 1 -> / 4 = 63.958;
        // fresnel at L.H = 1: ApproxPow( 0, 5 ) = 0 -> f0.
        let n = Vec3::Z;
        let s = spec_brdf(n, n, n, Vec3::splat(0.04), 1.0);
        let m2 = 0.0016f32 * 0.0016;
        let want = 0.04 * (m2 / (m2 * m2 + 1e-8)) / (4.0 + 1e-8);
        assert!(close(s.x, want, 1e-5), "{s} vs {want}");
        assert!(close(want, 2.5583, 1e-3));
    }

    #[test]
    fn spec_brdf_rough_grazing() {
        // smoothness 0: m = 1, m2 = 1 -> D = 1 / ( 1 + 1e-8 ), Gv = Gl = 1 -> 1/4;
        // L at 60 deg from N, V = N: H at 30 deg, L.H = cos 30 = 0.8660;
        // ApproxPow( 0.1340, 5 ); f0 = 0.5 -> occlusion 1.
        let n = Vec3::Z;
        let l = Vec3::new(60f32.to_radians().sin(), 0.0, 60f32.to_radians().cos());
        let s = spec_brdf(n, n, l, Vec3::splat(0.5), 0.0);
        let lh = l.dot((n + l).normalize());
        let f = 0.5 + 0.5 * approx_pow(1.0 - lh, 5.0);
        assert!(close(s.x, f * 0.25, 1e-5), "{s}");
        // ApproxPow is within ~10 % of powf here
        assert!(close(approx_pow(1.0 - lh, 5.0), (1.0 - lh).powf(5.0), 0.12));
    }

    #[test]
    fn env_brdf_values() {
        // NdotV 1, smoothness 1: t1 = 0.095 + 4.79 = 4.885, t2 = 1.025, ApproxExp2( -13 ) = 2^-13
        // a0 = 6.1123e-4; t3 = 9.5, ApproxExp2( -9.5 ) = asfloat( uint( 117.5 * 2^23 ) ) = 1.5 * 2^-10
        // a1 = 0.4 + 0.6 * ( 1 - 0.00146484 ) = 0.99912
        let e = environment_brdf(1.0, 1.0, Vec3::new(0.0, 1.0, 0.5));
        let a0 = 4.885f32 * 1.025 * 2f32.powi(-13);
        let a1 = 0.4 + 0.6 * (1.0 - 1.5 * 2f32.powi(-10));
        assert!(close(e.x, a0, 1e-4), "{e}");
        assert!(close(e.y, a1, 1e-5), "{e}");
        assert!(close(e.z, 0.5 * (a0 + a1), 1e-5), "{e}");
    }

    #[test]
    fn probe_falloff() {
        // centre: sphere radius 0 -> ramp 1 + inner / ( 1 - inner ) saturates to 1
        assert_eq!(probe_attenuation(Vec3::splat(0.5), 0.75), 1.0);
        // on a face centre ( 1, 0.5, 0.5 ): cube ( 1, 0, 0 ) -> sphere ( 1, 0, 0 ), length 1 ->
        // ( 1 - 0.25 / ( 0.25 + 1e-6 ) )^2 = ( 4e-6 )^2
        assert!(probe_attenuation(Vec3::new(1.0, 0.5, 0.5), 0.75) < 2e-11);
        // halfway on an axis: length 0.5, inner 0: ( 1 - 0.5 / ( 1 + 1e-6 ) )^2 = 0.25
        assert!(close(probe_attenuation(Vec3::new(0.75, 0.5, 0.5), 0.0), 0.25, 1e-5));
        // a cube corner maps onto the sphere: ( 1, 1, 1 ) -> each 1 - 0.5 - 0.5 + 1/3 -> sqrt( 1/3 ),
        // length 1 -> 0
        assert!(probe_attenuation(Vec3::ONE, 0.0) < 1e-6);
    }

    #[test]
    fn irradiance_and_shading() {
        let c = [Vec3::splat(0.5), Vec3::splat(0.1), Vec3::splat(0.2), Vec3::splat(0.3)];
        // n = +z: 0.5 + 0.2
        assert!(close(sh_irradiance(c, Vec3::Z, 1.0).x, 0.7, 1e-6));
        // n = +x: 0.5 - 0.3; n = -y: 0.5 + 0.1
        assert!(close(sh_irradiance(c, Vec3::X, 1.0).x, 0.2, 1e-6));
        assert!(close(sh_irradiance(c, -Vec3::Y, 1.0).x, 0.6, 1e-6));

        // One white light straight above a grey diffuse floor, no ambient:
        // diffuse = RGBE( 1 * N.L = 1 ) * albedo 10-bit; light colour 2 at falloff 0.5, proj 1 -> att 0.25
        let s = Surface { position: Vec3::ZERO, normal: Vec3::Z, view: Vec3::Z, specular: Vec3::ZERO, smoothness: 0.0 };
        let l = LightSample { pos: Vec3::Z * 100.0, color: Vec3::splat(2.0), spec_multiplier: 1.0, falloff: 0.5, proj_filter: 1.0, shadow: 1.0 };
        let (d, sp) = shade(Vec3::splat(0.5), &s, Vec3::ZERO, Vec3::ZERO, &[l]);
        // 0.5 light: exponent ceil( -1 ) = -1, mantissa 1 -> 255/255 * 0.5; albedo 511/1023
        assert!(close(d.x, 0.5 * 511.0 / 1023.0, 1e-6), "{d}");
        // f0 = 0 -> occlusion 0 -> fresnel 0: no specular
        assert_eq!(sp, Vec3::ZERO);
        // a light behind the surface is culled
        let below = LightSample { pos: -Vec3::Z * 100.0, ..l };
        assert_eq!(shade(Vec3::splat(0.5), &s, Vec3::ZERO, Vec3::ZERO, &[below]).0, Vec3::ZERO);
    }

    /// A one-node .ambientsh: header, 8 child words, `recs` 60-byte records.
    fn ambientsh(children: [u32; 8], recs: &[(f32, [i16; 3])]) -> Vec<u8> {
        let mut b = Vec::new();
        for w in [AMBIENTSH_MAGIC, 1, recs.len() as u32].into_iter().chain(children) {
            b.extend(w.to_be_bytes());
        }
        for (dc, p) in recs {
            let mut r = vec![0u8; 60];
            let h = f32_to_f16(*dc).to_be_bytes();
            r[0..2].copy_from_slice(&h);
            // L1 x (coefficient 3, red) twice the DC: clamped down to |L0|
            r[18..20].copy_from_slice(&f32_to_f16(dc * 2.0).to_be_bytes());
            for (k, v) in p.iter().enumerate() {
                r[0x36 + k * 2..0x38 + k * 2].copy_from_slice(&v.to_be_bytes());
            }
            b.extend(r);
        }
        b
    }

    #[test]
    fn f16_conversions() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        // truncating: 1 + 2^-11 is below half precision -> 1.0
        assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3c00);
        // exponent <= -15 flushes to 0, >= 16 saturates
        assert_eq!(f32_to_f16(2f32.powi(-15)), 0);
        assert_eq!(f32_to_f16(1.0e6), 0x7bff);
    }

    #[test]
    fn octree_corners_and_trilinear() {
        // all eight children are samples: one leaf over the root, corner k = nearest sample to
        // root corner k (x + 2y + 4z); samples sit near the corners in child order
        let w = 30000i16;
        let recs: Vec<(f32, [i16; 3])> = (0..8).map(|k| ((k + 1) as f32, [if k & 1 == 1 { w } else { -w }, if k & 2 == 2 { w } else { -w }, if k & 4 == 4 { w } else { -w }])).collect();
        let t = AmbientOctree::parse(&ambientsh(std::array::from_fn(|k| LEAF | k as u32), &recs)).unwrap();
        assert_eq!(t.leaves, vec![[0, 1, 2, 3, 4, 5, 6, 7]]);
        assert_eq!(t.nodes, vec![[LEAF; 8]]);
        // L1 clamped to |L0|: red x coefficient 2 dc -> dc
        assert_eq!(t.records[3].c[3].x, 4.0);
        // the descent stops in the root's child cell; the position inside that cell weights the
        // corners: the middle of child 0 ( -16384 each axis ) is q = 0.5 -> the average 4.5
        let c = t.sample(Vec3::splat(-16384.0));
        assert!((c[0].x - 4.5).abs() < 1e-5, "{:?}", c[0]);
        // near the +x low corner of a cell -> corner 1 (record 2.0)
        let c = t.sample(Vec3::new(32767.0, 1.0, 1.0));
        assert!((c[0].x - 2.0).abs() < 1e-3, "{:?}", c[0]);

        let file = ambientsh([LEAF | 1, LEAF | 5, LEAF | 1, LEAF | 1, LEAF | 1, LEAF | 1, LEAF | 1, LEAF | 1], &recs);
        let t = AmbientOctree::parse(&file).unwrap();
        // all eight are samples here too, but runs stop at the first repeat: candidates 1, 5
        assert_eq!(t.leaves.len(), 1);
        assert!(t.leaves[0].iter().all(|&r| r == 1 || r == 5));
    }

    /// The point-light projection (0x141a5b070's box): the volume edges map to 0 / 1.
    #[test]
    fn point_light_planes() {
        let l = LightDef {
            name: "t".into(),
            kind: LightKind::Point,
            origin: Vec3::new(100.0, 0.0, 0.0),
            axis: [Vec3::Y, -Vec3::X, Vec3::Z],
            radius: Vec3::new(10.0, 20.0, 40.0),
            center: Vec3::ZERO,
            target: Vec3::ZERO,
            right: Vec3::ZERO,
            up: Vec3::ZERO,
            start: Vec3::ZERO,
            end: Vec3::ZERO,
            color: Vec3::ONE,
            spec_scale: 1.0,
            dynamic_only: false,
            cast_shadows: false,
            probe_inner_falloff: 0.0,
            material: None,
            fade: VisibilityFade::default(),
        };
        let p = l.planes();
        let ev = |pl: [f32; 4], x: Vec3| pl[0] * x.x + pl[1] * x.y + pl[2] * x.z + pl[3];
        // local +x is world +y: 10 units along it is the S edge
        assert!((ev(p[0], Vec3::new(100.0, 10.0, 0.0)) - 1.0).abs() < 1e-6);
        assert!((ev(p[0], Vec3::new(100.0, 0.0, 0.0)) - 0.5).abs() < 1e-6);
        // local +y is world -x (radius 20); Q = 1; falloff along z (radius 40)
        assert!((ev(p[1], Vec3::new(80.0, 0.0, 0.0)) - 1.0).abs() < 1e-6);
        assert_eq!(ev(p[2], Vec3::new(3.0, 4.0, 5.0)), 1.0);
        assert!((ev(p[3], Vec3::new(100.0, 0.0, -40.0))).abs() < 1e-6);
    }

    /// 0x141a5abe0 / 0x141a5a870: target plane maps to S/Q = T/Q = 0.5 at the target, 1 at
    /// target + right; the falloff runs from max( n·start, 8 ) to max( n·end, 16 ) along the target.
    #[test]
    fn spot_light_planes() {
        let l = LightDef {
            name: "s".into(),
            kind: LightKind::Spot,
            origin: Vec3::ZERO,
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
            radius: Vec3::ZERO,
            center: Vec3::new(5.0, 5.0, 5.0),
            target: Vec3::new(100.0, 0.0, 0.0),
            right: Vec3::new(0.0, -50.0, 0.0),
            up: Vec3::new(0.0, 0.0, 50.0),
            start: Vec3::ZERO,
            end: Vec3::new(100.0, 0.0, 0.0),
            color: Vec3::ONE,
            spec_scale: 1.0,
            dynamic_only: false,
            cast_shadows: false,
            probe_inner_falloff: 0.0,
            material: None,
            fade: VisibilityFade::default(),
        };
        let p = l.planes();
        let ev = |pl: [f32; 4], x: Vec3| pl[0] * x.x + pl[1] * x.y + pl[2] * x.z + pl[3];
        let st = |x: Vec3| (ev(p[0], x) / ev(p[2], x), ev(p[1], x) / ev(p[2], x));
        let (s0, t0) = st(l.target);
        assert!((s0 - 0.5).abs() < 1e-6 && (t0 - 0.5).abs() < 1e-6);
        assert!((st(l.target + l.right).0 - 1.0).abs() < 1e-6);
        // up runs toward T = 0 (the -0.5 scale)
        assert!(st(l.target + l.up).1.abs() < 1e-6);
        assert!((ev(p[3], Vec3::new(8.0, 0.0, 0.0))).abs() < 1e-6);
        assert!((ev(p[3], Vec3::new(54.0, 3.0, 0.0)) - 0.5).abs() < 1e-6);
        assert!((ev(p[3], l.end) - 1.0).abs() < 1e-6);
        // spot lights sit at their origin (0x141a59fa0 ignores lightCenter for them)
        assert_eq!(l.global_origin(), Vec3::ZERO);
    }

    /// 0x141a59fa0: parallel lights 100000 units along the unrotated lightCenter.
    #[test]
    fn parallel_origin() {
        let mut l = LightDef {
            name: "p".into(),
            kind: LightKind::Parallel,
            origin: Vec3::new(1.0, 2.0, 3.0),
            axis: [Vec3::Y, -Vec3::X, Vec3::Z],
            radius: Vec3::splat(10.0),
            center: Vec3::new(0.0, 3.0, 4.0),
            target: Vec3::ZERO,
            right: Vec3::ZERO,
            up: Vec3::ZERO,
            start: Vec3::ZERO,
            end: Vec3::ZERO,
            color: Vec3::ONE,
            spec_scale: 1.0,
            dynamic_only: false,
            cast_shadows: false,
            probe_inner_falloff: 0.0,
            material: None,
            fade: VisibilityFade::default(),
        };
        let o = l.global_origin();
        assert!((o - Vec3::new(1.0, 60002.0, 80003.0)).length() < 0.02);
        l.center = Vec3::ZERO;
        assert_eq!(l.global_origin(), Vec3::new(1.0, 2.0, 100003.0));
    }

    /// 0x141879a70 with the idLight defaults (fadeVisibilityOver 400).
    #[test]
    fn distance_fade_ramp() {
        let f = VisibilityFade { max_range: 1000.0, ..Default::default() };
        let at = |d: f32, f: &VisibilityFade| distance_fade(f, Vec3::ZERO, Vec3::new(0.0, d, 0.0), 1.0);
        assert_eq!(at(500.0, &f), Some(1.0));
        assert!((at(700.0, &f).unwrap() - 0.75).abs() < 1e-5);
        // exactly at maxVisibleRange the rounded distance leaves ~2e-7 (> FLT_EPSILON), as in the exe
        assert!(at(1000.0, &f).unwrap() < 1e-6);
        assert_eq!(at(1001.0, &f), None);
        assert_eq!(at(5000.0, &f), None);
        let flip = VisibilityFade { flip: true, ..f };
        assert!((at(700.0, &flip).unwrap() - 0.25).abs() < 1e-5);
        assert_eq!(at(100.0, &flip), None);
        // no maxVisibleRange: never faded
        assert_eq!(at(1e6, &VisibilityFade::default()), Some(1.0));
        // fadeVisibilityOver >= maxVisibleRange: ramp 0, so full colour until the cull range
        let wide = VisibilityFade { max_range: 300.0, fade_over: 400.0, flip: false };
        assert_eq!(at(10.0, &wide), Some(1.0));
    }

    /// A light material colour program (lights/biground1_flicker's shape) over a wrapping table.
    #[test]
    fn colour_program() {
        let decl = "{
lightprojectmap	\"borderClamp lights/x\"
temp2	0.500000
temp3	time * temp2
temp4	flick[temp3]
lightcolor.x	temp4 * lightcolor
temp6	2.000000
temp7	lightcolor * temp6
lightcolor.y	temp4 * temp7
lightrotation	time * temp2
}";
        let mut p = ColorProgram::parse(decl);
        assert_eq!(p.table_names(), vec!["flick".to_string()]);
        p.tables.insert("flick".into(), idfx::table::Table::parse("{ { 0, 1 } }").unwrap());
        let c = Vec3::new(2.0, 3.0, 4.0);
        // time 0.5 -> table index 0.25 (wrapping table { 0, 1 })
        let v = p.eval(0.5, 0.0, c);
        let k = p.tables["flick"].lookup(0.25);
        assert!(k > 0.0 && k < 1.0);
        assert!((v.x - 2.0 * k).abs() < 1e-6);
        assert!((v.y - 3.0 * 2.0 * k).abs() < 1e-6);
        assert_eq!(v.z, 4.0);
        // swizzled source (lights/smm_grenade_light): lightcolor.xyz = t * lightcolor.xyzx
        let p2 = ColorProgram::parse("temp2	0.25
temp3	lightcolor.xyzx
lightcolor.xyz	temp2 * temp3
");
        assert_eq!(p2.eval(0.0, 0.0, c), c * 0.25);
    }

    #[test]
    fn renderparm_time_wraps() {
        assert_eq!(renderparm_time(1500), 1500.0f32 * 0.001);
        assert!((renderparm_time(0x80_0000 + 5) - 0.005).abs() < 1e-7);
    }

    /// 0x14187c440's R15 packing truncates.
    #[test]
    fn r15_truncates() {
        let w = pack_r15x4([0.03125, 0.0625, 0.40625, 1.0]);
        assert_eq!(w, [(1023 << 16) | 2047, (13311 << 16) | 32767]);
    }

    /// SceneAnim: faded colours go through RGBE, culled lights are zero, probes carry the fade.
    #[test]
    fn scene_anim_evaluate() {
        let fade = VisibilityFade { max_range: 1000.0, ..Default::default() };
        let a = SceneAnim {
            lights: vec![
                LightAnim { color: Vec3::new(1.0, 0.5, 0.25), program: None, fade, center: Vec3::ZERO, probe: false },
                LightAnim { color: Vec3::ONE, program: None, fade, center: Vec3::ZERO, probe: true },
                LightAnim { color: Vec3::splat(0.001), program: None, fade, center: Vec3::ZERO, probe: false },
            ],
        };
        let mut out = Vec::new();
        a.evaluate(0.0, 0.0, Vec3::new(700.0, 0.0, 0.0), &mut out);
        let want = unpack_rgbe(pack_rgbe(Vec3::new(1.0, 0.5, 0.25) * 0.75));
        assert_eq!(out[0], [want.x, want.y, want.z, 0.0]);
        assert!((out[1][3] - 0.75).abs() < 1e-5);
        assert_eq!(out[2], [0.0; 4]);
        a.evaluate(0.0, 0.0, Vec3::new(2000.0, 0.0, 0.0), &mut out);
        assert_eq!(out, vec![[0.0; 4]; 3]);
    }
}
