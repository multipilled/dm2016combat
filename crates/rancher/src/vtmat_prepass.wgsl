// Prepass / shadow fragment shader of the virtual-texture material. Only alpha-tested (megatrans)
// materials need a fragment shader here: the engine's preZDrawAlphaUnique and shadowVmtrTransUnique
// clip( sampleSpecular.w - 0.5 ), the cover. Otherwise this mirrors Bevy's default prepass outputs.

#import bevy_pbr::{prepass_io, pbr_prepass_functions}
#import rancher::vt_fetch::{vt, sample_cover, sample_vt, alpha_test, vt_normal}

fn vt_discard(in: prepass_io::VertexOutput) {
#ifdef VERTEX_UVS_A
    if vt.misc.z > 0.5 && vt.misc.w > 0.5 {
        alpha_test(sample_cover(in.uv, dpdx(in.uv), dpdy(in.uv)));
    }
#endif
}

#ifdef PREPASS_FRAGMENT
@fragment
fn fragment(in: prepass_io::VertexOutput, @builtin(front_facing) is_front: bool) -> prepass_io::FragmentOutput {
    vt_discard(in);
    var out: prepass_io::FragmentOutput;
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif
#ifdef NORMAL_PREPASS
    // the VT tangent-space normal in world space, like the colour pass (lighting.inc
    // GetWorldSpaceNormal)
    var n = normalize(in.world_normal);
#ifdef VERTEX_UVS_A
#ifdef VERTEX_TANGENTS
    if vt.misc.z > 0.5 {
        let f = sample_vt(in.uv, dpdx(in.uv), dpdy(in.uv));
        n = vt_normal(f, n, in.world_tangent);
    }
#endif
#endif
    out.normal = vec4<f32>(n * 0.5 + vec3<f32>(0.5), 1.0);
#endif
#ifdef MOTION_VECTOR_PREPASS
    out.motion_vector = pbr_prepass_functions::calculate_motion_vector(in.world_position, in.previous_world_position);
#endif
    return out;
}
#else
@fragment
fn fragment(in: prepass_io::VertexOutput) {
    vt_discard(in);
}
#endif
