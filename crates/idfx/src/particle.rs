//! idDeclParticle / idParticleStage (`generated/decls/particle/*.decl`).
//!
//! Field defaults are idParticleStage::Default (0x1418e3e20, called by the ctor 0x1418e3a30); the
//! decl text only lists fields that differ. Derived values follow the stage finish 0x1417cdef0.

use std::sync::Arc;

use anyhow::{Context, Result};
use glam::{Vec2, Vec3, Vec4};
use idres::decl::{Block, Value};
use idres::decldb::DeclDb;

use crate::decl::*;
use crate::parm::{Parm, SimpleParm};
use crate::table::Table;
use crate::Axis;

pub const DIST_NAMES: [&str; 7] = ["PDIST_RECT", "PDIST_CYLINDER", "PDIST_SPHERE", "PDIST_RECT_SURFACE", "PDIST_CYLINDER_SURFACE", "PDIST_SPHERE_SURFACE", "PDIST_CORNERS"];
pub const ORIENT_NAMES: [&str; 8] = ["POR_VIEW", "POR_VIEW_Z", "POR_TRAIL", "POR_AIMED", "POR_X", "POR_Y", "POR_Z", "POR_XYZ"];
pub const DIR_NAMES: [&str; 4] = ["PDIR_CONE", "PDIR_OUTWARD", "PDIR_OUTWARDEXPLOSION", "PDIR_SPEED"];
pub const FLIP_NAMES: [&str; 3] = ["PTEXTURE_FLIP_NONE", "PTEXTURE_FLIP_RANDOM", "PTEXTURE_FLIP_ALWAYS"];
pub const ROTATE_NAMES: [&str; 4] = ["PTEXTURE_ROTATE_NONE", "PTEXTURE_ROTATE_90_CW", "PTEXTURE_ROTATE_180", "PTEXTURE_ROTATE_90_CCW"];
pub const SORT_NAMES: [&str; 3] = ["PSORT_TYPE_NONE", "PSORT_TYPE_NEWEST_TO_OLDEST", "PSORT_TYPE_OLDEST_TO_NEWEST"];
pub const ANIM_NAMES: [&str; 3] = ["PANIM_TYPE_CYLE_RATE", "PANIM_TYPE_SINGLE_CYCLE_RATE", "PANIM_TYPE_SINGLE_CYCLE"];
pub const PATH_NAMES: [&str; 5] = ["PPATH_STANDARD", "PPATH_HELIX", "PPATH_FLIES", "PPATH_ORBIT", "PPATH_DRIP"];
pub const GPU_ORIENT_NAMES: [&str; 2] = ["POR_GPU_VIEW", "POR_GPU_TRAIL"];
pub const GPU_DIST_NAMES: [&str; 7] = [
    "PDIST_GPU_RECT_SURFACE",
    "PDIST_GPU_BOX",
    "PDIST_GPU_BOX_SURFACE",
    "PDIST_GPU_SPHERE",
    "PDIST_GPU_SPHERE_SURFACE",
    "PDIST_GPU_CYLINDER",
    "PDIST_GPU_CYLINDER_SURFACE",
];

/// GPU-path stage parameters (prt*GPU_t). Simulated on the CPU here (see sim.rs).
#[derive(Debug, Clone)]
pub struct GpuStage {
    pub material: String,
    pub cycles: i32,
    pub total: i32,
    pub spawn_bunching: f32,
    pub life: SimpleParm,
    pub dead_time: SimpleParm,
    pub time_offset: f32,
    pub emission_time: f32,
    pub initial_color: Vec4,
    pub initial_overbright: f32,
    pub final_color: Vec4,
    pub final_overbright: f32,
    pub fade_color: Vec4,
    pub fade_in: f32,
    pub fade_out: f32,
    pub soft_alpha_scale: f32,
    pub anim_type: usize,
    pub columns: u16,
    pub rows: u16,
    pub anim_rate: SimpleParm,
    pub start_frame: i16,
    pub random_row: bool,
    pub frame_blending: bool,
    pub orientation: usize,
    pub size_initial: Vec2,
    pub size_final: Vec2,
    pub size_variation: Vec2,
    pub dist_type: usize,
    pub dist_scale: Vec3,
    pub dist_offset: Vec3,
    pub vel_type: usize,
    pub vel_scale: Vec3,
    pub vel_offset: Vec3,
    pub acceleration: Vec3,
    pub collision: bool,
    pub restitution: f32,
    pub elasticity: f32,
    pub mass: f32,
    pub mass_variation: f32,
    /// Derived (0x1417cdef0): maximum life / dead time in seconds, bunch time.
    pub max_life: f32,
    pub max_dead: f32,
    pub bunch_time: f32,
}

/// Stage ctor (FUN_1418e3e20) material defaults: static decl pointers 0x143895598 / 0x1438955b8 (name strings
/// at 0x1428d2160 / 0x1428d2188) copied to systemProperties.material (+0x68) and systemPropertiesGPU.material
/// (+0x560).
pub const DEFAULT_MATERIAL: &str = "textures/effects/particles/default";
pub const DEFAULT_GPU_MATERIAL: &str = "textures/effects/particles/defaultgpu";

#[derive(Debug, Clone)]
pub struct Stage {
    pub name: String,
    pub hidden: bool,
    pub gpu: bool,
    // systemProperties
    pub is_light: bool,
    pub light_scale: f32,
    pub material: String,
    pub total_particles: i16,
    pub cycles: i16,
    pub diversity: i32,
    pub particle_life: Parm,
    pub time_offset: f32,
    pub dead_time: Parm,
    pub spawn_bunching: f32,
    pub emission_time: f32,
    pub flip_s: usize,
    pub flip_t: usize,
    pub tex_rotate: usize,
    pub wind_bias: Parm,
    pub sort: usize,
    pub bounds_expansion: f32,
    pub random_on_cycle: bool,
    pub camera_offset: f32,
    pub draw_near: bool,
    pub curvature: f32,
    // distribution
    pub dist_type: usize,
    pub dist_size: [Parm; 3],
    pub dist_random: bool,
    // orientation
    pub orientation: usize,
    pub num_trails: i16,
    pub segment_length: f32,
    pub aimed_view_fade: f32,
    pub orient_to_vel_only: bool,
    pub orient_to_world_vel: bool,
    pub orient_world: bool,
    pub preserve_aspect: bool,
    // direction
    pub dir_type: usize,
    pub dir_parms: [f32; 4],
    pub cone_axis: Axis,
    pub dir_world: bool,
    pub speed: [Parm; 3],
    // distance / emission scaling
    pub dist_scale_near: f32,
    pub dist_scale_far: f32,
    pub dist_scale_near_scale: f32,
    pub dist_scale_far_scale: f32,
    pub emission_near: f32,
    pub emission_far: f32,
    pub emission_near_scale: f32,
    pub emission_far_scale: f32,
    pub acceleration: [Parm; 3],
    pub accel_world: bool,
    pub gravity: Parm,
    pub gravity_world: bool,
    pub friction: [Parm; 3],
    pub offset: [Parm; 3],
    pub spawn_location: [Parm; 3],
    // colour
    pub base_color: [Parm; 4],
    pub fade_color: Vec4,
    pub fade_in: f32,
    pub fade_out: f32,
    pub fade_index: f32,
    pub soft_alpha_scale: f32,
    pub brightness: Parm,
    pub use_global_shadows: bool,
    pub min_shadow: f32,
    pub entity_color_blend: f32,
    // rotation
    pub rotation: [Parm; 3],
    pub allow_rot_dir_override: bool,
    pub esv_min: f32,
    pub esv_rotation_scale: f32,
    pub esv_fwd: f32,
    pub esv_lf: f32,
    pub esv_up: f32,
    pub esv_lock_to_view: bool,
    pub emit_world_space: bool,
    pub inherit_emitter_velocity: bool,
    pub blend_factor: Parm,
    pub initial_angle: [Parm; 3],
    pub pivot: Vec2,
    pub size: [Parm; 3],
    pub aspect: Parm,
    // texture animation
    pub anim_type: usize,
    pub columns: u16,
    pub rows: u16,
    pub anim_rate: Parm,
    pub start_frame: i16,
    pub random_row: bool,
    pub frame_blending: bool,
    pub path_type: usize,
    pub path_parms: [Parm; 5],
    pub generic: [Parm; 4],
    pub lod_size_scale: f32,
    pub lod_lerp: f32,
    pub lod_total: i16,
    pub gpu_stage: GpuStage,
    // derived
    pub max_particle_life: f32,
    pub max_dead_time: f32,
    pub bunch_time: f32,
    pub cycle_msec: i32,
}

fn parms<const N: usize>(b: Option<&Block>, name: &str, d: [Parm; N]) -> [Parm; N] {
    let mut out = d;
    for (i, p) in out.iter_mut().enumerate() {
        *p = Parm::read(elem(b, name, i), d[i]);
    }
    out
}

fn sparm(b: Option<&Block>, key: &str, d: SimpleParm) -> SimpleParm {
    SimpleParm::read(b.and_then(|b| b.get(key)), d)
}

impl Stage {
    pub fn read(s: Option<&Block>, tables: &[Table]) -> Stage {
        let sp = block(s, "systemProperties");
        let dist = block(s, "distribution");
        let ori = block(s, "orientation");
        let dir = block(s, "direction");
        let ds = block(s, "distanceScale");
        let es = block(s, "emissionRateScale");
        let acc = block(s, "acceleration");
        let grav = block(s, "gravity");
        let col = block(s, "colorAttributes");
        let rot = block(s, "rotation");
        let esv = block(s, "emitterSpaceVelocity");
        let ems = block(s, "emissionSpace");
        let size = block(s, "size");
        let ta = block(s, "texAnimation");
        let path = block(s, "customPath");
        let lod = block(s, "lodParms");
        let dir_parms = {
            let p = block(dir, "parms");
            [f32_or(p, "parms[0]", 90.0), f32_or(p, "parms[1]", 0.0), f32_or(p, "parms[2]", 0.0), f32_or(p, "parms[3]", 0.0)]
        };
        let base_d = [Parm::generic(1.0); 4];
        let mut st = Stage {
            name: str_of(s, "stageName").unwrap_or("").to_string(),
            hidden: bool_or(s, "hidden", false),
            gpu: bool_or(s, "gpuStage", false),
            is_light: bool_or(sp, "isLight", false),
            light_scale: f32_or(sp, "particleLightScale", 1.0),
            material: str_of(sp, "material").unwrap_or(DEFAULT_MATERIAL).to_string(),
            total_particles: i32_or(sp, "totalParticles", 20) as i16,
            cycles: i32_or(sp, "cycles", 0) as i16,
            diversity: i32_or(sp, "diversity", 0),
            particle_life: Parm::read(sp.and_then(|b| b.get("particleLife")), Parm::generic(1.5)),
            time_offset: f32_or(sp, "timeOffset", 0.0),
            dead_time: Parm::read(sp.and_then(|b| b.get("deadTime")), Parm::generic(0.0)),
            spawn_bunching: f32_or(sp, "spawnBunching", 1.0),
            emission_time: f32_or(sp, "emissionTime", 0.0),
            flip_s: enum_or(sp, "textureFlipS", &FLIP_NAMES, 0),
            flip_t: enum_or(sp, "textureFlipT", &FLIP_NAMES, 0),
            tex_rotate: enum_or(sp, "textureRotate", &ROTATE_NAMES, 0),
            wind_bias: Parm::read(sp.and_then(|b| b.get("windBias")), Parm::generic(0.0)),
            sort: enum_or(sp, "sortType", &SORT_NAMES, 0),
            bounds_expansion: f32_or(sp, "boundsExpansion", 0.0),
            random_on_cycle: bool_or(sp, "randomOnCycle", true),
            camera_offset: f32_or(sp, "cameraOffset", 0.0),
            draw_near: bool_or(sp, "drawNear", false),
            curvature: f32_or(sp, "curvature", f32::from_bits(0x3f59_999a)),
            dist_type: enum_or(dist, "type", &DIST_NAMES, 0),
            dist_size: parms(block(dist, "size"), "size", [Parm::generic(0.0); 3]),
            dist_random: bool_or(dist, "random", true),
            orientation: enum_or(ori, "type", &ORIENT_NAMES, 0),
            num_trails: i32_or(ori, "numTrails", 0) as i16,
            segment_length: f32_or(ori, "segmentLength", 0.0),
            aimed_view_fade: f32_or(ori, "aimedViewFade", 1.0),
            orient_to_vel_only: bool_or(ori, "orientToVelOnly", false),
            orient_to_world_vel: bool_or(ori, "orientToWorldVel", false),
            orient_world: bool_or(ori, "world", false),
            preserve_aspect: bool_or(ori, "preserveAspectRatio", false),
            dir_type: enum_or(dir, "type", &DIR_NAMES, 0),
            dir_parms,
            cone_axis: Axis::IDENTITY,
            dir_world: bool_or(dir, "world", false),
            speed: parms(block(s, "speed").and_then(|b| b.block("speed")), "speed", [Parm::integrate(0.0, 0.0); 3]),
            dist_scale_near: f32_or(ds, "nearDist", 0.0),
            dist_scale_far: f32_or(ds, "farDist", 0.0),
            dist_scale_near_scale: f32_or(ds, "nearScale", 1.0),
            dist_scale_far_scale: f32_or(ds, "farScale", 1.0),
            emission_near: f32_or(es, "nearDist", 0.0),
            emission_far: f32_or(es, "farDist", 0.0),
            emission_near_scale: f32_or(es, "nearEmissionScale", 1.0),
            emission_far_scale: f32_or(es, "farEmissionScale", 1.0),
            acceleration: parms(block(acc, "acceleration"), "acceleration", [Parm::generic(0.0); 3]),
            accel_world: bool_or(acc, "world", false),
            gravity: Parm::read(grav.and_then(|b| b.get("gravity")), Parm::generic(0.0)),
            gravity_world: bool_or(grav, "world", false),
            friction: parms(block(s, "friction").and_then(|b| b.block("friction")), "friction", [Parm::generic(0.0); 3]),
            offset: parms(block(s, "offset").and_then(|b| b.block("offset")), "offset", [Parm::generic(0.0); 3]),
            spawn_location: parms(block(s, "spawnLocation").and_then(|b| b.block("spawnLocation")), "spawnLocation", [Parm::generic(0.0); 3]),
            base_color: parms(block(col, "baseColor"), "baseColor", base_d),
            fade_color: vec4_or(col, "fadeColor", Vec4::ZERO),
            fade_in: f32_or(col, "fadeInFraction", 0.1),
            fade_out: f32_or(col, "fadeOutFraction", 0.25),
            fade_index: f32_or(col, "fadeIndexFraction", 0.0),
            soft_alpha_scale: f32_or(col, "softParticleAlphaScale", 1.0),
            brightness: Parm::read(col.and_then(|b| b.get("brightness")), Parm::generic(1.0)),
            use_global_shadows: bool_or(col, "useGlobalShadows", false),
            min_shadow: f32_or(col, "minShadowVal", 0.2),
            entity_color_blend: f32_or(col, "entityColorBlendVal", 1.0),
            rotation: parms(block(rot, "rotation"), "rotation", [Parm::integrate(0.0, 0.0); 3]),
            allow_rot_dir_override: bool_or(rot, "allowRotDirOverride", true),
            esv_min: f32_or(esv, "min", 1.0),
            esv_rotation_scale: f32_or(esv, "rotationScale", 100.0),
            esv_fwd: f32_or(esv, "fwdScale", 1.0),
            esv_lf: f32_or(esv, "lfScale", 1.0),
            esv_up: f32_or(esv, "upScale", 1.0),
            esv_lock_to_view: bool_or(esv, "lockToView", true),
            emit_world_space: bool_or(ems, "emitWorldSpace", false),
            inherit_emitter_velocity: bool_or(ems, "inheritEmitterVelocity", true),
            blend_factor: Parm::read(ems.and_then(|b| b.get("blendFactor")), Parm::generic(1.0)),
            initial_angle: parms(block(s, "initialRotation").and_then(|b| b.block("initialAngle")), "initialAngle", [Parm::range(-360.0, 360.0); 3]),
            pivot: vec2_or(block(s, "pivot"), "pivotOffset", Vec2::ZERO),
            size: parms(block(size, "size"), "size", [Parm::eval(4.0, 4.0); 3]),
            aspect: Parm::read(size.and_then(|b| b.get("aspectRatio")), Parm::eval(1.0, 1.0)),
            anim_type: enum_or(ta, "type", &ANIM_NAMES, 2),
            columns: i32_or(ta, "numColumns", 1) as u16,
            rows: i32_or(ta, "numRows", 1) as u16,
            anim_rate: Parm::read(ta.and_then(|b| b.get("rate")), Parm::generic(0.0)),
            start_frame: i32_or(ta, "startFrame", 0) as i16,
            random_row: bool_or(ta, "useRandomRow", false),
            frame_blending: bool_or(ta, "useFrameBlending", true),
            path_type: enum_or(path, "type", &PATH_NAMES, 0),
            path_parms: parms(block(path, "parms"), "parms", [Parm::generic(0.0); 5]),
            generic: parms(block(s, "genericParm").and_then(|b| b.block("genericParm")), "genericParm", [Parm::generic(0.0); 4]),
            lod_size_scale: f32_or(lod, "sizeScale", 1.0),
            lod_lerp: f32_or(lod, "lerpAmount", 0.0),
            lod_total: i32_or(lod, "totalParticles", 1) as i16,
            gpu_stage: GpuStage::read(s),
            max_particle_life: 0.0,
            max_dead_time: 0.0,
            bunch_time: 0.0,
            cycle_msec: 0,
        };
        // INTERIM: the render-side pick between the two material pointers is not decoded; GPU stages are drawn
        // with systemPropertiesGPU.material (shipped stages without one rely on its defaultgpu default, and the
        // ones naming defaultgpu in systemProperties agree).
        if st.gpu {
            st.material = st.gpu_stage.material.clone();
        }
        // Derived cone axis: angles (parms[1..3]) in degrees.
        st.cone_axis = Axis::from_angles(st.dir_parms[1], st.dir_parms[2], st.dir_parms[3]);
        st.derive(tables);
        st
    }

    /// Stage finish (0x1417cdef0).
    fn derive(&mut self, tables: &[Table]) {
        if !self.gpu {
            self.max_particle_life = self.particle_life.max(tables);
            self.max_dead_time = self.dead_time.max(tables);
            self.bunch_time = if self.emission_time <= 0.0 { self.max_particle_life } else { self.emission_time };
        } else {
            self.max_particle_life = self.gpu_stage.max_life;
            self.max_dead_time = self.gpu_stage.max_dead;
            self.bunch_time = self.gpu_stage.bunch_time;
        }
        self.cycle_msec = ((self.max_dead_time + self.max_particle_life) * 1000.0) as i32;
        if !self.gpu {
            // Vertex budget clamp: 0x2aa verts max per stage in 0x30-byte verts.
            let verts = if self.is_light { 0 } else if self.orientation == 2 { (self.num_trails as i32 + 1) * 4 } else { 4 };
            if verts > 0 && self.total_particles as i32 * verts > 0x2aa {
                self.total_particles = (0x8000 / (verts * 0x30)) as i16;
            }
            if verts > 0 && self.lod_total as i32 * verts > 0x2aa {
                self.lod_total = (0x8000 / (verts * 0x30)) as i16;
            }
        } else if self.gpu_stage.total > 0x8000 {
            self.gpu_stage.total = 0x8000;
        }
        self.num_trails = self.num_trails.min(0x20);
    }
}

impl GpuStage {
    fn read(s: Option<&Block>) -> GpuStage {
        let sp = block(s, "systemPropertiesGPU");
        let col = block(s, "colorGPU");
        let ta = block(s, "texAnimationGPU");
        let geo = block(s, "geometryGPU");
        let dist = block(s, "distributionGPU");
        let vel = block(s, "velocityGPU");
        let acc = block(s, "accelerationGPU");
        let phys = block(s, "physicsGPU");
        let mut g = GpuStage {
            material: str_of(sp, "material").unwrap_or(DEFAULT_GPU_MATERIAL).to_string(),
            cycles: i32_or(sp, "cycles", 0),
            total: i32_or(sp, "totalGpuParticles", 10),
            spawn_bunching: f32_or(sp, "spawnBunching", 1.0),
            life: sparm(sp, "particleLife", SimpleParm::constant(1.5)),
            dead_time: sparm(sp, "deadTime", SimpleParm::constant(0.0)),
            time_offset: f32_or(sp, "timeOffset", 0.0),
            emission_time: f32_or(sp, "emissionTime", 0.0),
            initial_color: vec4_or(col, "initialColor", Vec4::ONE),
            initial_overbright: f32_or(col, "initialOverbright", 1.0),
            final_color: vec4_or(col, "finalColor", Vec4::ONE),
            final_overbright: f32_or(col, "finalOverbright", 1.0),
            fade_color: vec4_or(col, "fadeColor", Vec4::ZERO),
            fade_in: f32_or(col, "fadeInFraction", 0.1),
            fade_out: f32_or(col, "fadeOutFraction", 0.25),
            soft_alpha_scale: f32_or(col, "softParticleAlphaScale", 1.0),
            anim_type: enum_or(ta, "type", &ANIM_NAMES, 2),
            columns: i32_or(ta, "numColumns", 1) as u16,
            rows: i32_or(ta, "numRows", 1) as u16,
            anim_rate: sparm(ta, "rate", SimpleParm::constant(0.0)),
            start_frame: i32_or(ta, "startFrame", 0) as i16,
            random_row: bool_or(ta, "useRandomRow", false),
            frame_blending: bool_or(ta, "useFrameBlending", true),
            orientation: enum_or(geo, "orientationType", &GPU_ORIENT_NAMES, 0),
            size_initial: vec2_or(geo, "sizeInitial", Vec2::ONE),
            size_final: vec2_or(geo, "sizeFinal", Vec2::ONE),
            size_variation: vec2_or(geo, "sizeVariation", Vec2::ZERO),
            dist_type: enum_or(dist, "type", &GPU_DIST_NAMES, 3),
            dist_scale: vec3_or(dist, "scale", Vec3::ONE),
            dist_offset: vec3_or(dist, "offset", Vec3::ZERO),
            vel_type: enum_or(vel, "type", &GPU_DIST_NAMES, 3),
            vel_scale: vec3_or(vel, "scale", Vec3::ONE),
            vel_offset: vec3_or(vel, "offset", Vec3::ZERO),
            acceleration: vec3_or(acc, "acceleration", Vec3::ZERO),
            collision: bool_or(phys, "useCollisionDetecion", true),
            restitution: f32_or(phys, "restitution", 0.5),
            elasticity: f32_or(phys, "elasticity", 0.5),
            mass: f32_or(phys, "mass", 1.0),
            mass_variation: f32_or(phys, "massVariation", 0.0),
            max_life: 0.0,
            max_dead: 0.0,
            bunch_time: 0.0,
        };
        g.max_life = g.life.max();
        g.max_dead = g.dead_time.max();
        g.bunch_time = if g.emission_time <= 0.0 { g.max_life } else { g.emission_time };
        if g.total > 0x8000 {
            g.total = 0x8000;
        }
        g
    }
}

#[derive(Debug, Clone)]
pub struct ParticleDecl {
    pub name: String,
    pub stages: Vec<Stage>,
    pub tables: Vec<Table>,
    pub max_system_duration: i32,
}

impl ParticleDecl {
    pub fn load(db: &DeclDb, name: &str) -> Result<Arc<ParticleDecl>> {
        let b = db.get("particle", name).with_context(|| format!("particle decl {name}"))?;
        let edit = block(Some(&b), "edit");
        let mut tables = Vec::new();
        for t in list(edit, "tableDecls") {
            let Some(tn) = t.as_str() else { continue };
            let path = format!("generated/decls/table/{tn}.decl");
            let table = match db.container().read_by_name(&path) {
                Ok(bytes) => Table::parse(&String::from_utf8_lossy(&bytes)).with_context(|| path.clone())?,
                Err(_) => Table::default(),
            };
            tables.push(table);
        }
        let stages = list(edit, "stages").into_iter().map(|v| Stage::read(v.as_block(), &tables)).collect();
        Ok(Arc::new(ParticleDecl { name: name.to_string(), stages, tables, max_system_duration: i32_or(edit, "maxSystemDuration", 0) }))
    }
}

/// Unused-value guard so `Value` stays imported for readers above.
#[allow(dead_code)]
fn _v(_: &Value) {}
