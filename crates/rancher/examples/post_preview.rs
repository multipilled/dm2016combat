//! Off-screen check of the engine post chain (post.rs): a lit test scene with a bright emitter drawn by a
//! world camera and a second "view model" camera on its own layer (both HDR, sharing the scene buffer),
//! the post chain on the second. Saves a screenshot once the exposure has settled and exits.
//! `POST_SHOT=out.png [POST_ENV=tgarza/intro_crash_int] cargo run --release -p rancher --example post_preview`

#[path = "../src/post.rs"]
mod post;

use bevy::camera::ClearColorConfig;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{WindowPosition, WindowResolution};

#[derive(Resource)]
struct Shot {
    start: std::time::Instant,
    taken: bool,
}

fn main() -> anyhow::Result<()> {
    let doom = idres::find_install().ok_or_else(|| anyhow::anyhow!("DOOM install not found"))?;
    let install = rancher_sim::install::load(&doom)?;
    let env = std::env::var("POST_ENV").ok().filter(|e| !e.is_empty());
    let plugin = post::PostPlugin::new(&install.decls.container_arc(), &install.cvars, env.as_deref());
    let window = Window {
        title: "post_preview".into(),
        focused: false,
        skip_taskbar: true,
        position: WindowPosition::At(IVec2::new(-4000, -4000)),
        resolution: WindowResolution::new(1280, 720),
        ..default()
    };
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(window), ..default() }))
        .add_plugins(plugin)
        .insert_resource(ClearColor(Color::srgb(0.02, 0.02, 0.03)))
        .add_systems(Startup, setup)
        .add_systems(Update, watch)
        .run();
    Ok(())
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    // stand-in lights scaled from Bevy's default exposure to engine units
    let k = post::BEVY_DEFAULT_EXPOSURE;
    let both = RenderLayers::from_layers(&[0, 1]);
    commands.spawn((DirectionalLight { illuminance: 6000.0 * k, ..default() }, Transform::from_xyz(0.3, 1.0, 0.4).looking_at(Vec3::ZERO, Vec3::Y), both));
    let floor = mats.add(StandardMaterial { base_color: Color::srgb(0.4, 0.38, 0.35), perceptual_roughness: 0.9, ..default() });
    commands.spawn((Mesh3d(meshes.add(Plane3d::default().mesh().size(40.0, 40.0))), MeshMaterial3d(floor), Transform::from_xyz(0.0, -1.0, 0.0)));
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    for (i, c) in [Color::srgb(0.7, 0.1, 0.1), Color::srgb(0.1, 0.6, 0.15), Color::srgb(0.15, 0.2, 0.7)].into_iter().enumerate() {
        let m = mats.add(StandardMaterial { base_color: c, perceptual_roughness: 0.6, ..default() });
        commands.spawn((Mesh3d(cube.clone()), MeshMaterial3d(m), Transform::from_xyz(-2.5 + 2.5 * i as f32, -0.5, -3.0)));
    }
    // a bright emitter (engine-unit radiance well above the white point) for bloom and lens dirt
    let hot = mats.add(StandardMaterial { base_color: Color::BLACK, emissive: LinearRgba::rgb(40.0, 30.0, 18.0), ..default() });
    commands.spawn((Mesh3d(meshes.add(Sphere::new(0.35))), MeshMaterial3d(hot), Transform::from_xyz(1.5, 1.2, -5.0)));
    // "view model" on layer 1, drawn by the second camera over the world
    let gun = mats.add(StandardMaterial { base_color: Color::srgb(0.3, 0.3, 0.32), metallic: 0.8, perceptual_roughness: 0.4, ..default() });
    commands.spawn((Mesh3d(meshes.add(Cuboid::new(0.12, 0.12, 0.6))), MeshMaterial3d(gun), Transform::from_xyz(0.25, -0.2, -0.6), RenderLayers::layer(1)));

    let cam = commands
        .spawn((Camera3d::default(), Camera { order: 0, ..default() }, post::hdr_camera(), AmbientLight { brightness: 300.0 * k, ..default() }, Transform::from_xyz(0.0, 0.5, 3.0).looking_at(Vec3::new(0.0, 0.0, -3.0), Vec3::Y)))
        .id();
    let view = commands
        .spawn((
            Camera3d::default(),
            Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
            post::hdr_camera(),
            post::DoomPost,
            AmbientLight { brightness: 400.0 * k, ..default() },
            RenderLayers::layer(1),
            Transform::default(),
        ))
        .id();
    commands.entity(cam).add_child(view);
    // a HUD-like overlay camera after the chain: it must be HDR too, to share the scene's main texture
    // (a non-HDR camera with ClearColorConfig::None gets its own, uncleared, texture)
    commands.spawn((Camera2d, Camera { order: 2, clear_color: ClearColorConfig::None, ..default() }, bevy::camera::Hdr, RenderLayers::layer(2)));
    commands.spawn((Sprite::from_color(Color::srgba(1.0, 1.0, 1.0, 0.5), Vec2::new(160.0, 24.0)), Transform::from_xyz(-500.0, -320.0, 0.0), RenderLayers::layer(2)));
    commands.spawn((Text::new("HUD text"), Node { position_type: PositionType::Absolute, left: Val::Px(12.0), top: Val::Px(10.0), ..default() }));
    commands.insert_resource(Shot { start: std::time::Instant::now(), taken: false });
}

fn watch(mut commands: Commands, mut shot: ResMut<Shot>, mut exit: MessageWriter<AppExit>) {
    let t = shot.start.elapsed().as_secs_f32();
    if t >= 3.0 && !shot.taken {
        let path = std::env::var("POST_SHOT").unwrap_or_else(|_| "post_preview.png".into());
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        shot.taken = true;
    }
    if t >= 4.5 {
        exit.write(AppExit::Success);
    }
}
