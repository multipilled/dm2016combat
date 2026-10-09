// Virtual-texture material: samples a material's VT levels the way DOOM (2016) does
// (vtmat_fetch.wgsl: SAMPLE_VMTR / FETCH_PHYSICAL_ARRAY; lighting.inc texture inputs) and shades it
// with the engine's model (lighting.wgsl: COMPUTE_SHADING -> COMPUTE_LIGHTING_IMPL, then the
// deferred environment-probe pass), not Bevy's PBR.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    mesh_view_bindings as view_bindings,
    mesh_view_bindings::view,
    clustered_forward as clustering,
    shadows,
    lighting as bevy_lighting,
    mesh_view_types,
}
#import rancher::vt_fetch::{vt, vt_lightmap, vt_lightmap_sampler, sample_vt, alpha_test, degamma, vt_normal}
#import rancher::lighting as lt

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::main_pass_post_lighting_processing,
}
#endif

// Bevy space -> engine space (Bevy ( x, y, z ) = engine ( -y, z, -x ), range.rs to_bevy)
fn to_engine(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(-v.z, -v.x, v.y);
}

// global.inc SmoothnessEncode / SmoothnessDecode through an 8-bit unorm channel (INTERIM: the
// g-buffer target format is not decoded; RGBA8 assumed).
fn q8(v: f32) -> f32 {
    return round(saturate(v) * 255.0) / 255.0;
}

#ifndef PREPASS_PIPELINE
// Stand-ins for lights that are not the map's engine lights (the range's Bevy lights): the engine's
// COMPUTE_LIGHT with Bevy's light colour converted to the radiance it would give
// (E / pi: Bevy's diffuse is albedo / pi × E). INTERIM.
fn stand_in_lights(s: ptr<function, lt::Shading>, world_pos: vec4<f32>, n_bevy: vec3<f32>, frag_coord: vec4<f32>, is_orthographic: bool) {
    let view_z = dot(vec4<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), world_pos);
    let cluster_index = clustering::view_fragment_cluster_index(frag_coord.xy, view_z, is_orthographic);
    let ranges = clustering::unpack_clusterable_object_index_ranges(cluster_index);
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_reflection_probe_index_offset; i++) {
        let id = clustering::get_clusterable_object_id(i);
        let l = &view_bindings::clustered_lights.data[id];
        // map lights carry -( engine light index + 1 ) in shadow_depth_bias (map.rs spawn_lights)
        if (*l).shadow_depth_bias < 0.0 {
            lt::process_light(s, u32(-(*l).shadow_depth_bias) - 1u);
            continue;
        }
        let to_light = (*l).position_radius.xyz - world_pos.xyz;
        let d2 = dot(to_light, to_light);
        var e = (*l).color_inverse_square_range.rgb * bevy_lighting::getDistanceAttenuation(d2, (*l).color_inverse_square_range.w) / (4.0 * 3.14159265);
        if ((*l).flags & mesh_view_types::POINT_LIGHT_FLAGS_SPOT_LIGHT_BIT) != 0u {
            continue; // INTERIM: Bevy spot stand-ins are not used by the app
        }
        lt::compute_light(s, to_engine((*l).position_radius.xyz), e * view.exposure / 3.14159265, 1.0);
    }
    let n_dir = view_bindings::lights.n_directional_lights;
    for (var i = 0u; i < n_dir; i++) {
        let d = &view_bindings::lights.directional_lights[i];
        var shadow = 1.0;
        if ((*d).flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            shadow = shadows::fetch_directional_shadow(i, world_pos, n_bevy, view_z, frag_coord.xy);
        }
        let pos = (*s).position + to_engine((*d).direction_to_light) * 100000.0;
        lt::compute_light(s, pos, (*d).color.rgb * shadow * view.exposure / 3.14159265, 1.0);
    }
}
#endif

@fragment
fn fragment(
    vertex_output: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var in = vertex_output;
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Surfaces without a virtual texture (placeholders): Bevy material colour, dielectric f0 0.04,
    // smoothness 0 (INTERIM stand-ins).
    var albedo = pbr_input.material.base_color.rgb;
    var specular = vec3<f32>(0.04);
    var smoothness = 0.0;
    var mask_emissive = 0.0;
#ifdef VERTEX_UVS_A
    // texCoordsLOD = virtCoords * virtualMapping.xy (unwrapped) drives the LOD and the gradients
    let uv_lod = in.uv;
    let ddx_uv = dpdx(uv_lod);
    let ddy_uv = dpdy(uv_lod);
    if vt.misc.z > 0.5 {
        let f = sample_vt(uv_lod, ddx_uv, ddy_uv);
        // megatrans surfaces: the engine clips in its depth prepass (preZDrawAlphaUnique) and draws
        // colour with depth EQUAL, which is this discard
        alpha_test(f.lightmap.z);

        // COMPUTE_SHADING: albedo / specular as sampled (the array is sRGB, so already linear),
        // smoothness = DeGamma( power ) (FETCH_PHYSICAL_ARRAY), normal = sampleNormal.wyz
        albedo = f.diffuse.rgb;
        specular = f.specular.rgb;
        smoothness = degamma(f.lightmap.a);
        mask_emissive = f.lightmap.x;
#ifdef VERTEX_TANGENTS
        pbr_input.N = vt_normal(f, pbr_input.world_normal, in.world_tangent);
#endif
    }
#endif

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    var s: lt::Shading;
    s.position = to_engine(in.world_position.xyz);
    s.normal = to_engine(pbr_input.N);
    s.view = to_engine(pbr_input.V);
    let albedo_packed = lt::pack_r10g10b10(albedo);
    s.specular = lt::unpack_r10g10b10(lt::pack_r10g10b10(specular));
    s.smoothness = smoothness;

    // COMPUTE_LIGHTING_IMPL: ambient = lightmap × $lightMapScale × $envLightMapScale (USE_LIGHTMAP)
    // or the irradiance octree (USE_IRRADIANCE_PROBES), + emissive
    var ambient = vec3<f32>(0.0);
    var emissive = vec3<f32>(0.0);
    var lightmapped = false;
#ifdef VERTEX_UVS_B
    if vt.lightmap.y > 0.5 {
        lightmapped = true;
        // vt.lightmap.x = lightMapScale × envLightMapScale (map.rs; ÷ the camera exposure, 1)
        ambient = textureSample(vt_lightmap, vt_lightmap_sampler, in.uv_b).rgb * vt.lightmap.x * view.exposure;
#ifdef VERTEX_COLORS
        // world geometry: emissive = mask² × vertex colour rgb × a × 16 (vertex.inc DECODE_COLOR)
        emissive = vec3<f32>(mask_emissive * mask_emissive) * in.color.rgb * in.color.a * 16.0;
#endif
    }
#endif
    if !lightmapped {
        // models that are not combined world geometry (hands, weapons, AI, props): emissive =
        // $bloomMaskScale.x × mask² × $colorScale.x × $color.xyz × $bloomColor.xyz (COMPUTE_SHADING)
        emissive = vec3<f32>(mask_emissive * mask_emissive) * vt.emissive.rgb;
        if lt::ambient_on() {
            ambient = lt::irradiance(lt::ambient_sh(s.position), s.normal, vt.shading.y);
        } else {
            // INTERIM: no ambient octree (the range): Bevy's AmbientLight stands in
            ambient = view_bindings::lights.ambient_color.rgb * view.exposure;
        }
    }
    // $staticModel.x: world surfaces skip lights flagged dynamicOnly
    s.static_model = lightmapped || vt.shading.x > 0.5;
    s.diffuse_packed = lt::pack_rgbe(ambient + emissive);
    s.specular_packed = 0u;

    stand_in_lights(&s, in.world_position, pbr_input.N, in.position, pbr_input.is_orthographic);

    let diffuse = lt::unpack_rgbe(s.diffuse_packed) * lt::unpack_r10g10b10(albedo_packed);
    let direct_spec = lt::unpack_rgbe(s.specular_packed);

    // FP_MRT_OUTPUT -> DEFERRED_ENV_PROBES: the probe pass reads specular = ( fastSqrtNR0( f0 ) )²
    // and SmoothnessDecode( SmoothnessEncode( s ) ) back from the g-buffer
    var g = s;
    let enc = vec3<f32>(lt::fast_sqrt_nr0(specular.x), lt::fast_sqrt_nr0(specular.y), lt::fast_sqrt_nr0(specular.z));
    let enc8 = vec3<f32>(q8(enc.x), q8(enc.y), q8(enc.z));
    g.specular = lt::unpack_r10g10b10(lt::pack_r10g10b10(enc8 * enc8));
    let sm = q8(lt::fast_sqrt_nr0(abs(smoothness)) * 0.5 + 0.5) * 2.0 - 1.0;
    g.smoothness = sm * sm;
    g.specular_packed = 0u;
    if dot(enc8, vec3<f32>(1.0)) > 0.0 {
        lt::process_probes(&g);
    }
    let probe_spec = lt::unpack_rgbe(g.specular_packed);

    // DEFERRED_COMPOSITE without SSDO / SSR (INTERIM: those passes are not ported)
    var color = diffuse + direct_spec + probe_spec;
    // Bevy emissive of placeholder materials (not engine data)
    color += pbr_input.material.emissive.rgb * view.exposure;
    out.color = vec4<f32>(color, pbr_input.material.base_color.a);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif
    return out;
}
