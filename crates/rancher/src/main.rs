//! dm2016combat: a DOOM (2016) gameplay testbed that reads everything from the user's own install.

mod assets;
mod autotest;
mod camfx;
mod combat;
mod demons;
mod fx;
mod hands_layers;
mod input;
mod map;
mod pickups;
mod post;
mod range;
mod settings;
mod sound;
mod swf_hud;
mod swf_menu;
mod animweb;
mod rig;
mod viewanim;
mod vtmat;

use bevy::camera::visibility::RenderLayers;
use bevy::camera::ClearColorConfig;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PresentMode, PrimaryWindow};
use rancher_sim::cmd::{UserCmd, button};
use rancher_sim::collision::World as SimWorld;
use rancher_sim::physics::flags;
use rancher_sim::player::Player;
use rancher_sim::weapons::Arsenal;

#[derive(Resource)]
pub struct Sim {
    pub player: Player,
    pub world: SimWorld,
    /// The engine's adaptive frame clock: one game frame per rendered frame, length from the last real frame.
    pub clock: rancher_sim::cmd::AdaptiveTick,
    /// Length of the last game frame (whole ms).
    pub msec_last: i32,
    /// Game time (ms) at the end of the last game frame.
    pub game_ms: i32,
    /// The user command of the last game frame.
    pub last_cmd: rancher_sim::cmd::UserCmd,
}

#[derive(Component)]
struct Hud;

#[derive(Component)]
struct MainCamera;

#[derive(Component)]
struct ViewCamera;

#[derive(Resource)]
struct Startup {
    doom: std::path::PathBuf,
    /// The player start (map start or range origin) and its yaw.
    start: rancher_sim::Vec3,
    start_yaw: f32,
    brushes: Vec<range::Brush>,
    /// A real map is loaded (RANCHER_MAP): no range brushes or dummies; the map's lightmap is on.
    map_mode: bool,
    map_lightmap: bool,
    container: std::sync::Arc<idres::Container>,
    defs: Vec<std::sync::Arc<rancher_sim::weapons::WeaponDef>>,
}

fn main() -> anyhow::Result<()> {
    let t_start = std::time::Instant::now();
    let doom = idres::find_install().ok_or_else(|| anyhow::anyhow!("DOOM (2016) install not found. Set DOOM_DIR to your DOOM folder."))?;
    let install = rancher_sim::install::load(&doom)?;
    eprintln!("install loaded in {:.2}s", t_start.elapsed().as_secs_f32());
    // RANCHER_MAP=game/sp/intro/intro loads a campaign map as the arena (map.rs); default is the firing range.
    let map_name = std::env::var("RANCHER_MAP").ok().filter(|m| !m.is_empty());
    let map_load = match &map_name {
        Some(m) => match map::load(&install.decls.container_arc(), Some(&install.decls), &install.movement, m) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("map {m}: {e:#}; using the firing range");
                None
            }
        },
        None => None,
    };
    let (brushes, world, start, start_yaw) = match &map_load {
        Some(l) => (Vec::new(), l.world.clone(), l.start, l.start_yaw),
        None => {
            let b = range::build();
            let w = range::world(&b);
            (b, w, rancher_sim::Vec3::new(0.0, 0.0, 1.0), 0.0)
        }
    };
    let mut player = Player::new(install.movement.clone(), start);
    player.view_angles[1] = start_yaw;
    let clock = rancher_sim::cmd::AdaptiveTick::new(player.cfg.adaptive_tick_min_hz, player.cfg.adaptive_tick_max_hz);
    let defs = rancher_sim::weapons::load_arsenal(&install.decls);
    let mut hands_layers = hands_layers::HandsLayersState::new(
        defs.iter().map(|d| rancher_sim::handlayers::HandsLayerDecl::from_decl(&install.decls, &d.decl).unwrap_or_default()).collect(),
        rancher_sim::handlayers::HandsLayerCvars::from_cvars(&install.cvars, 90.0),
    );
    {
        let speeds = rancher_sim::handlayers::bobcycle::PlayerSpeeds { run: install.movement.run_speed, sprint: install.movement.run_speed, crouch: install.movement.crouch_speed };
        let names: Vec<String> = defs.iter().map(|d| d.decl.clone()).collect();
        match hands_layers::BobCycleRig::load(&install.decls.container_arc(), &install.decls, &install.cvars, &names, &speeds) {
            Ok(rig) => hands_layers.bob_cycle = Some(rig),
            Err(e) => eprintln!("hands bob cycle: {e:#}"),
        }
        let blend_ms = install.cvars.0.get("hands_additiveAnimBlendMS").and_then(|v| v.trim_end_matches('f').parse::<f32>().ok()).unwrap_or(150.0) as i32;
        match hands_layers::AdditiveRig::load(&install.decls.container_arc(), &install.decls, &defs, blend_ms) {
            Ok(rig) => hands_layers.additive = Some(rig),
            Err(e) => eprintln!("hands additive channels: {e:#}"),
        }
    }
    let container = install.decls.container_arc();
    let cam_fx = camfx::CamFx::new(idres::decldb::DeclDb::new(container.clone()), &install.cvars);
    let mut arsenal = Arsenal::new(defs.clone());
    let auto = autotest::AutoTest::from_env();
    if let Some(w) = auto.weapon {
        if w < arsenal.defs.len() {
            arsenal.current = w;
            arsenal.phase = rancher_sim::weapons::WeaponPhase::Ready;
        }
    }
    let mut window = Window { title: "dm2016combat".into(), present_mode: PresentMode::AutoNoVsync, ..default() };
    if auto.shot.is_some() {
        // Background self-test: never take focus, stay off-screen and off the taskbar.
        window.focused = false;
        window.skip_taskbar = true;
        window.position = bevy::window::WindowPosition::At(IVec2::new(-4000, -4000));
        // RANCHER_RES=<w>x<h> sets the self-test resolution (default 1280x720).
        let (w, h) = std::env::var("RANCHER_RES").ok().and_then(|r| {
            let (w, h) = r.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        }).unwrap_or((1280u32, 720u32));
        window.resolution = bevy::window::WindowResolution::new(w, h);
    }

    let map_mode = map_load.is_some();
    // The map's baked lightmap and lights are on unless RANCHER_MAP_LIGHTMAP=0 (map.rs).
    let map_lightmap = map::lightmap_enabled();
    let map_plugin = MapPluginOpt(map_load.as_ref().map(|l| map::MapPlugin::new(doom.clone(), l)));
    // The engine's HDR post chain (post.rs) with the level's env decl (worldspawn edit.envSettings).
    let post_env = map_name.as_deref().and_then(|m| idfx::env::map_env(&container, m));
    let post_plugin = post::PostPlugin::new(&container, &install.cvars, post_env.as_deref());
    // Player settings (cvars the menu changes, saved to rancher.cfg). r_hdrPostProcess: RANCHER_HDR=0 / 1 overrides
    // it for this run, F9 toggles it in game.
    let mut settings = settings::Settings::load(&install.cvars, auto.shot.is_some());
    if let Ok(v) = std::env::var("RANCHER_HDR") {
        settings.set_session("r_hdrPostProcess", if v == "0" { 0 } else { 1 });
    }
    let hdr_on = settings.bool("r_hdrPostProcess");
    // default.cfg's SP binds with the player's rebinds on top.
    let mut actions = input::Actions::load(&container);
    actions.apply_user_binds(settings.binds());
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(window), ..default() }))
        .add_plugins(vtmat::VtMaterialPlugin)
        .add_plugins(post_plugin)
        .add_plugins(swf_hud::SwfHudPlugin)
        .add_plugins(swf_menu::MenuPlugin)
        .add_plugins(fx::FxPlugin)
        .add_plugins(sound::SoundPlugin)
        .add_plugins(map_plugin)
        .add_plugins(demons::DemonsPlugin { doom: doom.clone(), container: container.clone(), cvars: rancher_sim::config::CvarValues(install.cvars.0.clone()), map_mode })
        .add_plugins(pickups::PickupsPlugin { container: container.clone(), cvars: rancher_sim::config::CvarValues(install.cvars.0.clone()), map_mode })
        .insert_resource(auto)
        .insert_resource(actions)
        .insert_resource(hands_layers)
        .insert_resource(cam_fx)
        .insert_resource(HdrMode { on: hdr_on })
        .insert_resource(settings)
        .init_resource::<settings::MenuOpen>()
        .add_message::<settings::MenuAction>()
        .init_resource::<hands_layers::HandsStatus>()
        .init_resource::<combat::ZoomStatus>()
        .init_resource::<viewanim::HandsEvents>()
        .init_resource::<viewanim::ViewJoints>()
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.07)))
        .insert_resource(Sim { player, world, clock, msec_last: 0, game_ms: 0, last_cmd: Default::default() })
        .insert_resource(combat::Combat { arsenal, targets: Vec::new(), projectiles: Vec::new(), last_damage: Vec::new(), shot_total: 0.0 })
        .insert_resource(Startup { doom: doom.clone(), start, start_yaw, brushes, map_mode, map_lightmap, container, defs })
        .add_systems(bevy::app::Startup, setup)
        // Movers advance (and push the player) after the player's think, as in the game frame.
        .configure_sets(Update, map::MapSet::Movers.after(tick).before(place_camera))
        .add_systems(Update, (settings::apply, input::update_actions, grab_cursor, menu_actions, look, tick, player_sounds, combat::weapons_tick, viewanim::animate, place_camera, combat::fx_tick, hud, autotest::autotest).chain())
        .add_systems(Update, hdr_mode)
        .add_systems(Last, settings::save)
        .run();
    Ok(())
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut vt_mats: ResMut<Assets<vtmat::VtMaterial>>,
    startup: Res<Startup>,
    mut sim: ResMut<Sim>,
    mut combat: ResMut<combat::Combat>,
) {
    for b in &startup.brushes {
        commands.spawn((
            Mesh3d(meshes.add(range::mesh(&b.hull))),
            MeshMaterial3d(mats.add(StandardMaterial { base_color: b.color, perceptual_roughness: 0.9, ..default() })),
        ));
    }
    let both = RenderLayers::from_layers(&[0, combat::VIEW_LAYER]);
    // The range's stand-in sun (INTERIM: the range has no level lights). On a lit map there is none: the map's
    // lights (map.rs) light the world and the hands alike, as in the game.
    if !(startup.map_mode && startup.map_lightmap) {
        commands.spawn((
            // Stand-in Bevy light units in engine radiance units (the post chain sets view exposure 1).
            DirectionalLight { illuminance: 6000.0 * post::BEVY_DEFAULT_EXPOSURE, shadow_maps_enabled: !startup.map_mode, ..default() },
            Transform::from_xyz(0.3, 1.0, 0.4).looking_at(Vec3::ZERO, Vec3::Y),
            both.clone(),
        ));
    }
    let cam = commands
        .spawn((
            MainCamera,
            Camera3d::default(),
            Camera { order: 0, ..default() },
            Projection::Perspective(PerspectiveProjection { near: 1.0, far: WORLD_FAR, ..default() }),
            post::hdr_camera(),
            AmbientLight { brightness: 300.0 * post::BEVY_DEFAULT_EXPOSURE, ..default() },
            Transform::default(),
        ))
        .id();
    let view_cam = commands
        .spawn((
            ViewCamera,
            Camera3d::default(),
            Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
            Projection::Perspective(PerspectiveProjection { near: 0.5, far: 1000.0, ..default() }),
            // The hands share the world's HDR buffer; the post chain runs on this camera (it draws last).
            post::hdr_camera(),
            post::DoomPost,
            // INTERIM stand-in ambient (range only, no .ambientsh octree there): same as the world camera so the
            // hands are not lit brighter than the world.
            AmbientLight { brightness: 300.0 * post::BEVY_DEFAULT_EXPOSURE, ..default() },
            RenderLayers::layer(combat::VIEW_LAYER),
            Transform::default(),
        ))
        .id();
    commands.entity(cam).add_child(view_cam);
    let mut vtm = vtmat::VtMaterials::open(&startup.doom);
    match viewanim::build(&startup.container, &mut vtm, &startup.defs, &mut commands, &mut meshes, &mut vt_mats, &mut images, view_cam) {
        Ok((r, web)) => {
            commands.insert_resource(r);
            commands.insert_resource(web);
        }
        Err(e) => eprintln!("view models: {e:#}"),
    }
    if !startup.map_mode {
        combat.targets = combat::spawn_targets(&mut commands, &mut meshes, &mut mats, &mut sim);
    }
    commands.spawn((
        Hud,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(16.0), ..default() },
        Node { position_type: PositionType::Absolute, left: Val::Px(12.0), top: Val::Px(10.0), ..default() },
    ));
    commands.spawn((
        Text::new("+"),
        TextFont { font_size: bevy::text::FontSize::Px(22.0), ..default() },
        Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), margin: UiRect { left: Val::Px(-6.0), top: Val::Px(-14.0), ..default() }, ..default() },
    ));
}

/// Esc opens / closes the pause menu (unless the menu plugin handles Esc itself); the menu frees the cursor and
/// closing it takes the cursor back. A click captures the cursor while no menu is open. Self-tests never capture.
fn grab_cursor(
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<settings::MenuOpen>,
    menu_esc: Option<Res<settings::MenuOwnsEscape>>,
    auto: Res<autotest::AutoTest>,
    mut was_open: Local<bool>,
) {
    if menu_esc.is_none() && keys.just_pressed(KeyCode::Escape) {
        menu.0 = !menu.0;
    }
    let Ok(mut c) = cursor.single_mut() else { return };
    let capture = |c: &mut CursorOptions, on: bool| {
        c.grab_mode = if on { CursorGrabMode::Locked } else { CursorGrabMode::None };
        c.visible = !on;
    };
    if menu.0 {
        if !*was_open {
            capture(&mut c, false);
        }
    } else if auto.shot.is_none() && (mouse.just_pressed(MouseButton::Left) || *was_open) {
        capture(&mut c, true);
    }
    *was_open = menu.0;
}

/// The menu's commands. INTERIM: RestartLevel respawns the player at the start (no full level reload yet).
fn menu_actions(
    mut actions: MessageReader<settings::MenuAction>,
    mut menu: ResMut<settings::MenuOpen>,
    mut sim: ResMut<Sim>,
    startup: Res<Startup>,
    mut exit: MessageWriter<AppExit>,
) {
    for a in actions.read() {
        match a {
            settings::MenuAction::Resume => menu.0 = false,
            settings::MenuAction::Quit => {
                exit.write(AppExit::Success);
            }
            settings::MenuAction::RestartLevel => {
                let cfg = sim.player.cfg.clone();
                sim.player = Player::new(cfg, startup.start);
                sim.player.view_angles[1] = startup.start_yaw;
                menu.0 = false;
            }
        }
    }
}

fn look(
    mut sim: ResMut<Sim>,
    wheel: Res<swf_hud::WeaponWheel>,
    motion: Res<AccumulatedMouseMotion>,
    zoom: Res<combat::ZoomStatus>,
    settings: Res<settings::Settings>,
    menu: Res<settings::MenuOpen>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
) {
    if !menu.0 && !wheel.open && cursor.single().is_ok_and(|c| c.grab_mode != CursorGrabMode::None) {
        // in_invertLook flips the mouse pitch.
        let dy = if settings.bool("in_invertLook") { -motion.delta.y } else { motion.delta.y };
        // INTERIM: ironSightZoom.sensitivity_scale_mouse scales the mouse delta while zoomed (how the game applies
        // it is not decoded; 1 when not zoomed).
        let k = if zoom.sensitivity_scale_mouse > 0.0 { zoom.sensitivity_scale_mouse } else { 1.0 };
        sim.player.look(motion.delta.x * k, dy * k);
    }
}

/// Builds the user command from the game actions (default.cfg SP bindset).
fn user_cmd(act: &input::Actions) -> UserCmd {
    let axis = |pos: &str, neg: &str| -> i8 { 127 * act.held(pos) as i8 - 127 * act.held(neg) as i8 };
    let mut cmd = UserCmd { forward: axis("_moveforward", "_moveback"), right: axis("_moveright", "_moveleft"), ..default() };
    if act.held("_jump") {
        cmd.up = 127;
    }
    if act.held("_crouch") {
        cmd.up = -127;
        cmd.buttons |= button::CROUCH;
    }
    if act.held("_walk") {
        cmd.buttons |= button::WALK;
    }
    cmd
}

/// Runs one game frame per rendered frame in whole milliseconds, within the engine's adaptive tick range
/// (com_adaptiveTickMinHz 30 .. MaxHz 200). INTERIM until the adaptive tick is decoded.
fn tick(mut sim: ResMut<Sim>, wheel: Res<swf_hud::WeaponWheel>, time: Res<Time>, actions: Res<input::Actions>, menu: Res<settings::MenuOpen>, sync: Option<Res<demons::SyncView>>) {
    if menu.0 {
        // The pause menu stops the game clock (no game frame this frame).
        sim.msec_last = 0;
        return;
    }
    let cmd = user_cmd(&actions);
    // com_fixedTic 1 / com_adaptiveTick 1: exactly one game frame per rendered frame.
    let msec = (sim.clock.next(time.delta().as_micros() as u64) as f32 * wheel.time_scale) as i32;
    sim.msec_last = msec;
    sim.game_ms += msec;
    sim.last_cmd = cmd;
    // A glory kill holds the player at its sync attach joint (demons/glory.rs; sync_forceBypassPlayerPhysics 1).
    if sync.is_some_and(|s| s.active) {
        return;
    }
    let Sim { player, world, .. } = &mut *sim;
    player.think(world, cmd, msec);
}

/// The player's jump / landing sounds from this frame's PlayerEvents (idPlayer Think and the jump callback):
/// take-off plays sndJump; a double jump also plays the jump boots' doubleJumpSound; a landing plays the
/// landing table (heavy for large landings). INTERIM: surface "default" until surfaces are traced; the
/// falling-large / fatal / no-damage landing sounds are not played yet.
fn player_sounds(sim: Res<Sim>, sound: Option<ResMut<sound::Sound>>) {
    let Some(mut sound) = sound else { return };
    let ev = &sim.player.events;
    if ev.double_jumped {
        sound.double_jump();
    }
    if ev.jumped || ev.double_jumped {
        sound.jump();
    }
    if let Some(l) = &ev.landed {
        match l.sound {
            rancher_sim::player::LandSound::Normal => sound.landing(false, "default"),
            rancher_sim::player::LandSound::Heavy => sound.landing(true, "default"),
            _ => {}
        }
    }
}

/// Culling distance of the world camera. The projection itself is infinite (reverse Z), as in the engine; this
/// only bounds the frustum, large enough for map vistas.
const WORLD_FAR: f32 = 262_144.0;

fn place_camera(
    sim: Res<Sim>,
    mut combat: ResMut<combat::Combat>,
    layers: Res<hands_layers::HandsLayersState>,
    zoom: Res<combat::ZoomStatus>,
    mut cam_fx: ResMut<camfx::CamFx>,
    events: Res<viewanim::HandsEvents>,
    joints: Res<viewanim::ViewJoints>,
    auto: Res<autotest::AutoTest>,
    mut traced_joint: Local<bool>,
    mut cams: Query<(&mut Transform, &mut Projection, Option<&ViewCamera>), Or<(With<MainCamera>, With<ViewCamera>)>>,
    mut hands_root: Query<(&mut Transform, &mut Visibility), (With<viewanim::HandsRoot>, Without<MainCamera>, Without<ViewCamera>)>,
    windows: Query<&Window, With<PrimaryWindow>>,
    sync: Option<Res<demons::SyncView>>,
) {
    let p = &sim.player;
    let sync = sync.as_deref().cloned().unwrap_or_default();
    let now = sim.game_ms;
    let aspect = windows.single().map(|w| w.width() / w.height().max(1.0)).unwrap_or(16.0 / 9.0);
    let combat = &mut *combat;
    let kick = combat.arsenal.view_kick;
    let fx = &mut *cam_fx;
    fx.hands_events(&events.0, now, &mut combat.arsenal.rng);
    if p.events.double_jumped {
        fx.last_dj = Some(p.physics.last_double_jump_time);
    }
    // The player FOV (0x140e3b580: the zoom interp, g_fov when not zooming; 0 before the weapon code first
    // writes it) plus the FOV kick, turned into the camera's fov_x / fov_y (0x140e6bc00).
    // A glory kill forces its FOV (sync_autoFOV 1, sync_autoFOVValue; demons/glory.rs).
    let view_fov = sync.fov.unwrap_or(if zoom.fov > 0.0 { zoom.fov } else { p.cfg.fov });
    let (fov_x, fov_y) = hands_layers::view_fov(view_fov + kick.fov, aspect);

    // The first-person view (VIEWFX.md): view angles + weapon kick (pitch, yaw, roll) + the double-jump pitch,
    // then the hands rig's animated camera joint (p_applyAnimatedCamera). The hands are placed at this view.
    let dj = rancher_sim::viewfx::double_jump_pitch(fx.last_dj, p.physics.time, fx.dj_change, fx.dj_duration);
    // The eye, or the ledge-grab spring camera while a grab drives it.
    let eye = p.view_origin();
    let angles = [p.view_angles[0] + kick.pitch + dj, p.view_angles[1] + kick.yaw, p.view_angles[2] + kick.roll];
    let mut view = rancher_sim::viewfx::View { origin: [eye.x, eye.y, eye.z], axis: rancher_sim::viewfx::angles_to_mat3(angles) };
    if let Some((t, r)) = fx.cam {
        rancher_sim::viewfx::apply_animated_camera(&mut view, t, &r);
        if auto.trace && t.iter().any(|v| v.abs() > 0.01) {
            let a = rancher_sim::viewfx::mat3_to_angles(&r);
            println!("[camera {:7.3}] joint t ({:.2} {:.2} {:.2}) angles ({:.2} {:.2} {:.2})", auto.elapsed, t[0], t[1], t[2], a[0], a[1], a[2]);
        }
    }
    if auto.trace && (!view.origin.iter().all(|v| v.is_finite()) || !view.axis.iter().flatten().all(|v| v.is_finite())) {
        println!("[camera {:7.3}] view origin {:?} eye {:?} angles {:?}", auto.elapsed, view.origin, p.physics.origin, angles);
    }
    if auto.trace && dj != 0.0 {
        println!("[camera {:7.3}] double-jump pitch {dj:.3}", auto.elapsed);
    }
    // A glory kill's camera: the player body's `camera` joint (ae_attachCamera; demons/glory.rs).
    if let Some((o, axis)) = sync.view {
        view = rancher_sim::viewfx::View { origin: o, axis };
    }
    let first_person = view;
    // idView::RenderView: the render view alone gets the screen shakes (damage view kick: no damage yet).
    fx.fx.apply_shakes(&mut view, now, &mut combat.arsenal.rng);
    if auto.trace && view != first_person {
        let (a, b) = (rancher_sim::viewfx::mat3_to_angles(&first_person.axis), rancher_sim::viewfx::mat3_to_angles(&view.axis));
        let d = |i: usize| view.origin[i] - first_person.origin[i];
        println!("[camera {:7.3}] shake angles ({:.3} {:.3} {:.3}) origin ({:.2} {:.2} {:.2})", auto.elapsed, b[0] - a[0], b[1] - a[1], b[2] - a[2], d(0), d(1), d(2));
    }
    // The engine reads the camera joint of the last built hands frame: keep this frame's for the next view.
    fx.capture_camera_joint(&joints);
    if auto.trace && !*traced_joint && !joints.hands_names.is_empty() {
        *traced_joint = true;
        println!("[camera {:7.3}] hands camera joint {:?}", auto.elapsed, fx.cam);
    }

    let view_rot = camfx::axis_to_bevy(&view.axis);
    let view_pos = camfx::pos_to_bevy(view.origin);
    for (mut t, mut proj, view_cam) in &mut cams {
        if view_cam.is_some() {
            // Child of the main camera: inherits position and view. Hands projection: GetHandsFovScale on the
            // horizontal FOV, vertical with hands_fovVerticalScaleHack (independent x / y scales).
            let (fx, fy) = layers.projection_fov(combat.arsenal.current, fov_x, fov_y, Some((zoom.fraction, zoom.hands_ratio)));
            *proj = Projection::custom(hands_layers::HandsProjection::from_fov(fx, fy, 0.5, 1000.0));
            continue;
        }
        t.translation = view_pos;
        t.rotation = view_rot;
        // Independent x / y scales: beyond 3:1 the engine's aspect clamp no longer matches the window.
        *proj = Projection::custom(hands_layers::HandsProjection::from_fov(fov_x, fov_y, 1.0, WORLD_FAR));
    }
    // Hands placement (0x140d7ccd0): the first-person view origin plus (hands spring - view spring) along world z,
    // with the first-person axis; relative to the (shaken) render camera they hang off.
    let dz = p.hands_offset() - p.view_spring.pos;
    let hands_pos = camfx::pos_to_bevy(first_person.origin) + Vec3::new(0.0, dz, 0.0);
    let inv = view_rot.inverse();
    for (mut t, mut vis) in &mut hands_root {
        t.translation = inv * (hands_pos - view_pos);
        t.rotation = inv * camfx::axis_to_bevy(&first_person.axis);
        // hideHandsOnZoom(Delay).
        *vis = if zoom.hide_hands || sync.active { Visibility::Hidden } else { Visibility::Inherited };
    }
}

/// HDR rendering on / off (r_hdrPostProcess). On: the world, hands and HUD cameras share one HDR buffer and the
/// engine's post chain (post.rs: auto exposure, bloom, tonemap) runs on the hands camera. Off: all of them render to
/// the LDR target with no post chain (INTERIM: what the engine draws with r_hdrPostProcess 0 is not decoded).
/// All three cameras switch together: a camera whose HDR setting differs from the others' composites its own
/// uncleared buffer over the frame (a black screen).
#[derive(Resource)]
struct HdrMode {
    on: bool,
}

fn hdr_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<settings::Settings>,
    mode: Res<HdrMode>,
    mut commands: Commands,
    cams: Query<(Entity, Has<bevy::camera::Hdr>, Has<ViewCamera>, Has<post::DoomPost>), Or<(With<MainCamera>, With<ViewCamera>, With<swf_hud::HudCamera>)>>,
) {
    if keys.just_pressed(KeyCode::F9) {
        // Through the settings, so the choice is saved; settings::apply sets the mode next frame.
        settings.set("r_hdrPostProcess", if mode.on { 0 } else { 1 });
        eprintln!("HDR post process {}", if mode.on { "off" } else { "on" });
    }
    for (e, hdr, view, doom_post) in &cams {
        let mut ec = commands.entity(e);
        if hdr != mode.on {
            if mode.on { ec.insert(bevy::camera::Hdr); } else { ec.remove::<bevy::camera::Hdr>(); }
        }
        if view && doom_post != mode.on {
            if mode.on { ec.insert(post::DoomPost); } else { ec.remove::<post::DoomPost>(); }
        }
    }
}

/// The developer telemetry overlay (top left): the setting `rancher_debugOverlay` (the menu's DEBUG OVERLAY row),
/// toggled by F10 (unbound in the game's SP config). Off by default; on in self-tests and with RANCHER_DEBUG_HUD=1
/// (for that run only).
fn hud(
    sim: Res<Sim>,
    combat: Res<combat::Combat>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<settings::Settings>,
    mut init: Local<bool>,
    mut text: Query<(&mut Text, &mut Visibility), With<Hud>>,
) {
    let Ok((mut t, mut vis)) = text.single_mut() else { return };
    if !std::mem::replace(&mut *init, true) && (std::env::var("RANCHER_DEBUG_HUD").is_ok_and(|v| v == "1") || std::env::var("RANCHER_SHOT").is_ok()) {
        settings.set_session("rancher_debugOverlay", 1);
    }
    if keys.just_pressed(KeyCode::F10) {
        let on = settings.bool("rancher_debugOverlay");
        settings.set("rancher_debugOverlay", if on { 0 } else { 1 });
    }
    let on = settings.bool("rancher_debugOverlay");
    *vis = if on { Visibility::Inherited } else { Visibility::Hidden };
    if !on {
        return;
    }
    let ph = &sim.player.physics;
    let v = ph.velocity;
    let hs = (v.x * v.x + v.y * v.y).sqrt();
    let f = ph.flags;
    let a = &combat.arsenal;
    let def = a.def();
    let short = def.decl.rsplit('/').next().unwrap_or(&def.decl);
    let ammo = if def.ammo_decl.is_empty() { "-".to_string() } else { format!("{}/{}", a.ammo[a.current], def.ammo_max) };
    let dummies: Vec<String> = combat.targets.iter().map(|tg| format!("{:.0}u:{:.0}", tg.center.x, tg.damage_taken)).collect();
    **t = format!(
        "speed {hs:6.1}  vz {:7.1}  z {:7.2}  {}{}{} jumps {}  frame {} ms  fps {:.0}\n\
         weapon [{}] {short}  ammo {ammo}  {:?}\n\
         last shot {:.0}   dummies (distance:damage) {}\n\
         click: capture mouse  esc: release  WASD move  space jump  C crouch  shift walk  LMB fire  1-0 / wheel switch",
        v.z,
        ph.origin.z,
        if ph.walking { "WALKING " } else { "AIR " },
        if f & flags::DUCKED != 0 { "DUCKED " } else { "" },
        if f & flags::JUMP_HELD != 0 { "JUMP_HELD " } else { "" },
        ph.jump_count,
        sim.msec_last,
        1.0 / time.delta_secs().max(1e-6),
        a.current,
        a.phase,
        combat.shot_total,
        dummies.join("  "),
    );
}

/// Adds the map streaming plugin only when a map was loaded.
struct MapPluginOpt(Option<map::MapPlugin>);

impl Plugin for MapPluginOpt {
    fn build(&self, app: &mut App) {
        if let Some(p) = &self.0 {
            p.build(app);
        }
    }
}
