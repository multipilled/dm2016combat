//! Off-screen check of the virtual-texture materials: draws a few materials on quads, waits for the
//! textures to stream in, saves a screenshot and exits.
//! `VT_SHOT=out.png cargo run --release -p rancher --example vt_preview [--masked] [material level]...`
//! (`--masked`: the alpha-tested variants, drawn in front of a lit backdrop).

#[path = "../src/vtmat.rs"]
mod vtmat;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{WindowPosition, WindowResolution};
use vtmat::{VtMaterial, VtMaterialPlugin, VtMaterials};

#[derive(Resource)]
struct Preview {
    mats: Vec<(Handle<VtMaterial>, usize)>,
    start: std::time::Instant,
    coarse_at: Option<f32>,
    shot_at: Option<f32>,
    shot_taken: bool,
}

fn main() {
    let window = Window {
        title: "vt_preview".into(),
        focused: false,
        skip_taskbar: true,
        position: WindowPosition::At(IVec2::new(-4000, -4000)),
        resolution: WindowResolution::new(1280, 720),
        ..default()
    };
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(window), ..default() }))
        .add_plugins(VtMaterialPlugin)
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.07)))
        .add_systems(Startup, setup)
        .add_systems(Update, watch)
        .run();
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<VtMaterial>>) {
    let doom = idres::find_install().expect("DOOM install");
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let masked = args.first().map(String::as_str) == Some("--masked");
    if masked {
        args.remove(0);
    }
    let list: Vec<(String, usize)> = if args.is_empty() {
        vec![("models/weapons/shotgun/shotgun_base".into(), 1), ("models/characters/doommarine_playerhands/doommarine_playerhands".into(), 1)]
    } else {
        args.chunks(2).map(|c| (c[0].clone(), c.get(1).and_then(|l| l.parse().ok()).unwrap_or(1))).collect()
    };
    // `--unique <map> <level>`: show one level of a map's unique HDR lightmap (BC6H) unlit
    if args.first().map(String::as_str) == Some("--unique") {
        let u = idres::vtex::unique::UniqueVt::open(&doom, &args[1]).expect("pages file");
        let t = u.level_texture(args.get(2).and_then(|l| l.parse().ok()).unwrap_or(4));
        eprintln!("unique level {}: {}x{}, {} pages missing", t.level, t.width, t.height, t.missing_pages);
        let mut img = Image::new_uninit(
            bevy::render::render_resource::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
            bevy::render::render_resource::TextureDimension::D2,
            bevy::render::render_resource::TextureFormat::Bc6hRgbUfloat,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        img.data = Some(t.blocks);
        let tex = images.add(img);
        // left: the lightmap unlit; right: a grey lit material with it bound as baked lighting (UV_1)
        let m = vtmat::flat(&mut mats, StandardMaterial { base_color_texture: Some(tex.clone()), unlit: true, ..default() });
        let quad = meshes.add(Mesh::from(Rectangle::new(2.0, 2.0)));
        commands.spawn((Mesh3d(quad), MeshMaterial3d(m), Transform::from_xyz(-1.05, 0.0, 0.0)));
        let mut lit = vtmat::VtMaterial { base: StandardMaterial { base_color: Color::srgb(0.5, 0.5, 0.5), perceptual_roughness: 1.0, ..default() }, extension: default() };
        vtmat::set_lightmap(&mut lit.extension, Some(tex), std::env::var("VT_LIGHTMAP_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(3.2));
        let mut mesh = Mesh::from(Rectangle::new(2.0, 2.0));
        if let Some(bevy::mesh::VertexAttributeValues::Float32x2(uv)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0).cloned() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv);
        }
        commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(mats.add(lit)), Transform::from_xyz(1.05, 0.0, 0.0)));
        commands.spawn((Camera3d::default(), Transform::from_xyz(0.0, 0.0, 4.6).looking_at(Vec3::ZERO, Vec3::Y)));
        commands.insert_resource(Preview { mats: Vec::new(), start: std::time::Instant::now(), coarse_at: None, shot_at: Some(2.0), shot_taken: false });
        return;
    }
    let mut vtm = VtMaterials::open(&doom);
    let quad = meshes.add(Mesh::from(Rectangle::new(2.0, 2.0)).with_generated_tangents().unwrap());
    let mut handles = Vec::new();
    let n = list.len() as f32;
    for (i, (name, level)) in list.iter().enumerate() {
        let got = if masked { vtm.material_masked(name, *level, &mut images, &mut mats) } else { vtm.material(name, *level, &mut images, &mut mats) };
        let Some(m) = got else {
            eprintln!("{name}: not in the virtual texture");
            continue;
        };
        let x = (i as f32 - (n - 1.0) / 2.0) * 2.2;
        commands.spawn((Mesh3d(quad.clone()), MeshMaterial3d(m.clone()), Transform::from_xyz(x, 0.0, 0.0)));
        handles.push((m, *level));
    }
    if masked {
        // a backdrop to see through the holes
        let back = vtmat::flat(&mut mats, StandardMaterial { base_color: Color::srgb(0.8, 0.3, 0.1), ..default() });
        commands.spawn((Mesh3d(meshes.add(Mesh::from(Rectangle::new(20.0, 20.0)))), MeshMaterial3d(back), Transform::from_xyz(0.0, 0.0, -1.0)));
    }
    // masked: shadows too, to check the alpha-tested shadow pass
    commands.spawn((DirectionalLight { illuminance: 6000.0, shadow_maps_enabled: masked, ..default() }, Transform::from_xyz(0.3, 0.6, 1.0).looking_at(Vec3::ZERO, Vec3::Y)));
    commands.spawn((Camera3d::default(), AmbientLight { brightness: 400.0, ..default() }, Transform::from_xyz(0.0, 0.0, 1.35 + 1.1 * n).looking_at(Vec3::ZERO, Vec3::Y)));
    commands.insert_resource(Preview { mats: handles, start: std::time::Instant::now(), coarse_at: None, shot_at: None, shot_taken: false });
}

fn watch(mut commands: Commands, mut p: ResMut<Preview>, mats: Res<Assets<VtMaterial>>, mut exit: MessageWriter<AppExit>) {
    let t = p.start.elapsed().as_secs_f32();
    let state: Vec<(bool, bool)> = p
        .mats
        .iter()
        .map(|(h, level)| mats.get(h).map_or((false, false), |m| (m.extension.params.misc.z > 0.5, m.extension.params.misc.z > 0.5 && m.extension.params.rect.z as usize == *level)))
        .collect();
    if p.coarse_at.is_none() && state.iter().all(|s| s.0) {
        p.coarse_at = Some(t);
        eprintln!("coarse levels in after {t:.2}s");
    }
    if p.shot_at.is_none() && state.iter().all(|s| s.1) {
        eprintln!("all levels in after {t:.2}s");
        p.shot_at = Some(t + 1.0);
    }
    if let Some(at) = p.shot_at {
        if t >= at && !p.shot_taken {
            let path = std::env::var("VT_SHOT").unwrap_or_else(|_| "vt_preview.png".into());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            p.shot_taken = true;
        }
        if t >= at + 1.5 {
            exit.write(AppExit::Success);
        }
    }
    if t > 120.0 {
        eprintln!("timed out: {state:?}");
        exit.write(AppExit::Success);
    }
}
