//! Weapons in the testbed: view models from the install, firing, projectiles, target dummies.

use bevy::prelude::*;
use rancher_sim::Vec3 as V;
use rancher_sim::weapons::{Arsenal, Hands, HandsInput, HandsState, LandSize, ModSwitch, ModSwitchEvent, SelectInput, SpreadInput, WeaponEvent, WeaponInput, WeaponSelect, Zoom, ZoomInput};

use crate::range::to_bevy;
use crate::Sim;

pub const VIEW_LAYER: usize = 1;

/// One pellet / bullet / projectile hit this frame (idTech world coordinates), for the FX code. Written
/// only when the message type is registered (FxPlugin adds it).
#[derive(Message, Clone, Debug)]
pub struct ShotImpact {
    pub pos: V,
    /// Unit surface normal at the hit.
    pub normal: V,
    /// Surface type of the hit material ("" = default; the testbed's brushes carry none yet).
    pub surface: String,
    /// Projectile decl name (impactEffectTable etc.).
    pub projectile: String,
    /// Shared by every pellet of one Fired event.
    pub shot: u32,
    pub weapon: usize,
    /// Hit a damageable (the dummies).
    pub target: bool,
}

/// One hitscan trace this frame (tracerInfo tracers): fire origin -> end point.
#[derive(Message, Clone, Debug)]
pub struct ShotTracer {
    pub from: V,
    pub to: V,
    pub projectile: String,
    pub shot: u32,
    pub weapon: usize,
}

/// The player's zoom this frame (rancher_sim::weapons::zoom), for the camera and the hands FOV. Written by
/// weapons_tick when the resource exists (main.rs registers it).
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct ZoomStatus {
    /// IsZoomed 0x140e42290.
    pub zoomed: bool,
    /// GetZoomFraction 0x140e416c0 (FovInput.zoom_fraction).
    pub fraction: f32,
    /// The zoom's view FOV (player+0xce74 idInterpolate; g_fov when not zooming). The game's other FOV
    /// offsets (0x140e3b580: player+0xd064, the +0xceac interp) are not included.
    pub fov: f32,
    /// zoomHandsWeaponFovRatio (hands+0x10358, FovInput.zoom_ratio).
    pub hands_ratio: f32,
    /// ironSightZoom.sensitivity_scale_mouse while zoomed, else 1 (how the game applies it is not decoded).
    pub sensitivity_scale_mouse: f32,
    /// hideHandsOnZoom(Delay): the hands are hidden.
    pub hide_hands: bool,
}

#[derive(Resource)]
pub struct Combat {
    pub arsenal: Arsenal,
    pub targets: Vec<Target>,
    pub projectiles: Vec<rancher_sim::weapons::Projectile>,
    pub last_damage: Vec<(f32, f32)>,
    pub shot_total: f32,
}

pub struct Target {
    pub center: V,
    pub half: V,
    pub damage_taken: f32,
}

#[derive(Component)]
pub struct Popup {
    pub ttl: f32,
    pub world: Vec3,
}


pub fn spawn_targets(commands: &mut Commands, meshes: &mut Assets<Mesh>, mats: &mut Assets<StandardMaterial>, sim: &mut Sim) -> Vec<Target> {
    // A row of dummies down the firing lane at falloff-relevant distances (idTech units).
    let mut out = Vec::new();
    let mat = mats.add(StandardMaterial { base_color: Color::srgb(0.55, 0.12, 0.10), ..default() });
    for dist in [64.0, 128.0, 256.0, 384.0, 512.0, 768.0, 1024.0, 1536.0] {
        let half = V::new(16.0, 16.0, 48.0);
        let center = V::new(dist, 0.0, 48.0);
        let mesh = meshes.add(Cuboid::new(half.y * 2.0, half.z * 2.0, half.x * 2.0));
        let e = commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat.clone()), Transform::from_translation(to_bevy(center)))).id();
        let _ = e;
        out.push(Target { center, half, damage_taken: 0.0 });
        let _ = sim;
    }
    out
}

/// One melee sweep (the trace manager's box / line trace): the world (a box `half` wide, or a ray) and the
/// dummies (their boxes grown by `half`), nearest first.
fn melee_sweep(world: &rancher_sim::collision::World, targets: &[Target], demons: Option<&crate::demons::DemonTargets>, a: V, b: V, half: f32) -> Option<rancher_sim::weapons::SweepHit> {
    use rancher_sim::weapons::SweepHit;
    let d = b - a;
    let len = d.length();
    if len < 1e-4 {
        return None;
    }
    let dir = d / len;
    let wall = if 0.0 < half {
        let hull = rancher_sim::collision::Hull::cuboid(V::splat(-half), V::splat(half));
        let tr = world.translate(&hull, a, b);
        tr.hit().then_some((tr.fraction, tr.c.normal))
    } else {
        world.ray(a, dir, len).map(|(t, n, _)| (t / len, n))
    };
    let mut best = wall.map(|(f, n)| SweepHit { fraction: f, pos: a + d * f, normal: n, target: None, demon: None, actor: false });
    for (i, t) in targets.iter().enumerate() {
        if let Some((tt, n)) = ray_box_normal(a, dir, t.center, t.half + V::splat(half)) {
            let f = tt / len;
            if f <= 1.0 && best.as_ref().is_none_or(|h| f < h.fraction) {
                best = Some(SweepHit { fraction: f, pos: a + dir * tt, normal: n, target: Some(i), demon: None, actor: false });
            }
        }
    }
    // Demons: their hit spheres grown by the box half (the box-against-sphere sweep approximated).
    for dm in demons.map(|d| d.demons.as_slice()).unwrap_or_default() {
        for (c, r, joint) in &dm.spheres {
            if let Some(tt) = rancher_sim::demons::hit::ray_sphere(a, dir, len, *c, *r + half) {
                let f = tt / len;
                if best.as_ref().is_none_or(|h| f < h.fraction) {
                    let pos = a + dir * tt;
                    let normal = (pos - *c).normalize_or_zero();
                    best = Some(SweepHit { fraction: f, pos, normal, target: None, demon: Some((dm.id, joint.clone())), actor: true });
                }
            }
        }
    }
    best
}

/// Slab test returning the entry distance and the entered face's outward normal.
fn ray_box_normal(start: V, dir: V, c: V, h: V) -> Option<(f32, V)> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    let mut n = -dir.normalize_or_zero();
    for a in 0..3 {
        let (s, d, lo, hi) = (start[a], dir[a], c[a] - h[a], c[a] + h[a]);
        if d.abs() < 1e-9 {
            if s < lo || s > hi {
                return None;
            }
            continue;
        }
        let (mut ta, mut tb) = ((lo - s) / d, (hi - s) / d);
        let mut face = -1.0;
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
            face = 1.0;
        }
        if ta > t0 {
            t0 = ta;
            n = V::ZERO;
            n[a] = face;
        }
        t1 = t1.min(tb);
        if t0 > t1 {
            return None;
        }
    }
    Some((t0, n))
}

/// idAngles::ToMat3 rows for a roll-free view: forward, left, up.
fn view_axes(angles: [f32; 3]) -> (V, V, V) {
    let fwd = rancher_sim::physics::angles_to_forward(angles);
    let left = V::Z.cross(fwd).normalize_or_zero();
    let up = fwd.cross(left).normalize_or_zero();
    (fwd, left, up)
}

/// The idHands driver (rancher_sim::weapons::hands) once the hands anim web exists. It lives here, not in
/// `Combat`, because main.rs builds `Combat` with a struct literal.
#[derive(Default)]
pub struct HandsDriver {
    pub hands: Option<Hands>,
    pub select: Option<WeaponSelect>,
    /// Fired-event counter (ShotImpact / ShotTracer `shot`).
    pub shot_seq: u32,
    /// Last frame's trigger (fallback path's release edge).
    pub trigger_last: bool,
    pub zoom: Option<Zoom>,
    /// Weapon mods (rancher_sim::weapons::mods): unlocked on the first frame, switched with _reload.
    pub mods_ready: bool,
    pub mod_switch: ModSwitch,
    /// RANCHER_TRACE: last logged (weapon, fire mode, charge state) for the mod trace lines.
    pub trace_last: Option<(usize, usize, rancher_sim::weapons::ChargeState)>,
    /// Projectile ids (the weapon's launched list, rancher_sim::weapons::detonate).
    pub projectile_seq: u32,
}

/// Testbed mod unlock (no perk economy here): every mod owned at RANCHER_MOD_LEVEL (0 = base mod, 1..3 =
/// upgrades bought in decl order, 4 = + mastery; default 4 = fully upgraded) with family RANCHER_MOD active on
/// every weapon (1 = the first mod, 2 = the second, 0 = none; default 1). The game starts a weapon with no mod
/// active; _reload (R) cycles the owned mods through the perk switcher.
pub fn unlock_mods(arsenal: &mut Arsenal) -> (u8, Option<usize>) {
    let level = std::env::var("RANCHER_MOD_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(4u8).min(4);
    let active = match std::env::var("RANCHER_MOD").ok().and_then(|v| v.parse::<usize>().ok()) {
        Some(0) => None,
        Some(n) => Some(n - 1),
        None => Some(0),
    };
    arsenal.unlock_all_mods(level, active);
    (level, active)
}

#[allow(clippy::too_many_arguments)]
pub fn weapons_tick(
    mut commands: Commands,
    mut combat: ResMut<Combat>,
    sim: Res<Sim>,
    actions: Res<crate::input::Actions>,
    time: Res<Time>,
    auto: Res<crate::autotest::AutoTest>,
    web: Option<ResMut<crate::viewanim::HandsWebRuntime>>,
    mut driver: Local<HandsDriver>,
    mut wheel: Option<ResMut<crate::swf_hud::WeaponWheel>>,
    (mut impacts, mut tracers): (Option<ResMut<Messages<ShotImpact>>>, Option<ResMut<Messages<ShotTracer>>>),
    mut status: ResMut<crate::hands_layers::HandsStatus>,
    mut sound: Option<ResMut<crate::sound::Sound>>,
    zoom_status: Option<ResMut<ZoomStatus>>,
    view_joints: Option<Res<crate::viewanim::ViewJoints>>,
    (demon_targets, mut demon_damage, sync_view, dealt): (Option<Res<crate::demons::DemonTargets>>, Option<ResMut<Messages<crate::demons::DemonDamage>>>, Option<Res<crate::demons::SyncView>>, Option<Res<crate::pickups::DamageDealt>>),
) {
    use rancher_sim::demons::{DamageEvent, TraceHit};
    let attacker_origin = sim.player.physics.origin;
    // The player's damage-dealt scale (Quad Damage, suit mods; crate::pickups) on the damage scale argument.
    let dealt = dealt.map_or(1.0, |d| d.scale);
    // One idAI2::Damage call; the demon computes the amount from the decl itself (Damage_Calculate).
    let mut send_demon = |id: u32, decl: &str, traces: Vec<TraceHit>, scale: f32, dir: V, splash_fraction: f32| {
        if let Some(m) = demon_damage.as_mut() {
            m.write(crate::demons::DemonDamage { id, event: DamageEvent { decl: decl.to_string(), traces, attacker_origin: Some(attacker_origin), scale: scale * dealt, dir, splash_fraction } });
        }
    };
    let ms = time.delta_secs() * 1000.0;
    let mut web = web;
    let driver = &mut *driver;
    // Glory kills (crates/rancher/src/demons/glory.rs): the sync holds the player, so no weapon update while it runs
    // (sync_forceBypassPlayerPhysics 1); a melee press that starts one is not a hands melee (hands_syncMeleeInterruptsAll
    // 1: the sync melee 0x140d6b110 is tried first).
    let sync = sync_view.as_deref().cloned().unwrap_or_default();
    if sync.active {
        return;
    }
    if !driver.mods_ready {
        driver.mods_ready = true;
        let (level, active) = unlock_mods(&mut combat.arsenal);
        if auto.trace {
            println!("[mods] unlocked: level {level} active family {active:?}");
        }
    }
    // The arsenal's clock is the game clock (the hands web is advanced to it): after this frame's tick
    // it must read sim.game_ms.
    combat.arsenal.time_ms = sim.game_ms - sim.msec_last;
    // With the hands anim web: idHands owns the weapon (fire on ae_fireWeaponRight, switches on the
    // bring-up events). On the first frame the hands start (SetWeapon + ResetAnimWeb) and take the web
    // over from the renderer's INTERIM controller.
    if let Some(w) = web.as_mut() {
        if driver.hands.is_none() {
            let mut h = Hands::new(combat.arsenal.defs.len());
            h.start(&mut combat.arsenal, &mut w.rt);
            driver.hands = Some(h);
            w.driven = true;
        }
    }
    // Weapon selection as the game does it (rancher_sim::weapons::select): _weap0.._weap9 pick
    // weaponSelectionGroup N (repeat presses cycle the group), _weapnext / _weapprev walk the inventory
    // skipping the chainsaw and empty weapons, a _changeWeapon tap swaps to the last weapon; switches
    // are issued once g_weaponChangeMinIntervalMS allows (idHands::SelectWeapon, or the sim-timed
    // fallback's switch_to).
    let sel = driver.select.get_or_insert_with(|| WeaponSelect::new(&combat.arsenal));
    let mut sel_in = SelectInput { next: actions.pressed("_weapnext"), prev: actions.pressed("_weapprev"), change: actions.held("_changeweapon"), ..Default::default() };
    for (g, p) in sel_in.weap.iter_mut().enumerate() {
        *p = actions.pressed(&format!("_weap{g}"));
    }
    let now = combat.arsenal.time_ms + sim.msec_last;
    if let Some(w) = sel.update(&combat.arsenal, now, &sel_in) {
        match (driver.hands.as_mut(), web.as_mut()) {
            (Some(h), Some(rt)) => {
                h.select(&combat.arsenal, &rt.rt, w);
            }
            _ => combat.arsenal.switch_to(w),
        }
    }
    if let Some(w) = wheel.as_mut().and_then(|wh| wh.pick.take()) {
        sel.select_by_decl(&combat.arsenal, now, w);
    }
    let player = &sim.player;
    let eye = player.physics.origin + V::new(0.0, 0.0, player.eye_height());
    // The game adds the kick pitch to the view while it builds the view/fire axis (0x140e3f1a0).
    let mut angles = player.view_angles;
    angles[0] += combat.arsenal.view_kick.pitch;
    angles[1] += combat.arsenal.view_kick.yaw;
    let (fwd, left, up) = view_axes(angles);

    // Zoom (rancher_sim::weapons::zoom): BUTTON_ZOOM is hold-to-zoom; UpdateZoom runs before the weapon.
    let base_fov = player.cfg.fov;
    let zoom = driver.zoom.get_or_insert_with(|| Zoom::new(combat.arsenal.defs.len(), base_fov));
    let (state_ok, flags_ok, hidden) = match (driver.hands.as_ref(), web.as_ref()) {
        (Some(h), Some(rt)) => (h.zoom_state_ok(&combat.arsenal, &rt.rt), h.zoom_flags_ok(&rt.rt), h.hidden(&rt.rt)),
        _ => (true, true, false),
    };
    use rancher_sim::physics::flags as pmf;
    let zin = ZoomInput {
        button: actions.held("_zoom"),
        base_fov,
        moving: player.physics.velocity != V::ZERO,
        jumped: player.physics.flags & pmf::JUMPED != 0,
        double_jumped: player.physics.flags & pmf::DOUBLE_JUMPED != 0,
        on_ground: player.physics.walking,
        hands_state_ok: state_ok,
        hands_flags_ok: flags_ok,
        hands_hidden: hidden || zoom.hide_hands,
    };
    for snd in zoom.update(&combat.arsenal, now, &zin) {
        if let Some(s) = sound.as_mut() {
            s.post(&snd, None, None);
        }
    }
    if let Some(h) = driver.hands.as_mut() {
        h.set_zoom_pct(zoom.zoom_pct);
    }
    let zoomed = zoom.zoomed;
    if let Some(mut zs) = zoom_status {
        let def = combat.arsenal.def();
        *zs = ZoomStatus {
            zoomed,
            fraction: zoom.fraction(def, now, base_fov),
            fov: zoom.view_fov(now),
            hands_ratio: zoom.hands_ratio.value(now),
            sensitivity_scale_mouse: zoom.sensitivity_scale_mouse(def),
            hide_hands: zoom.hide_hands,
        };
    }

    let input = WeaponInput {
        trigger: actions.held("_attack1") || auto.fire,
        spread: SpreadInput { speed: player.physics.velocity.length(), run_speed: player.cfg.run_speed, crouched: player.physics.ducked(), zoomed, view_forward: fwd },
        left,
        up,
        gaussian_spread: true,
        altfire: actions.held("_altfire"),
    };
    // Perk switcher (rancher_sim::weapons::modswitch, player think 0x140e456a0): _reload cycles the mods.
    if let Some(h) = driver.hands.as_mut() {
        let busy = h.new_weapon.is_some();
        match driver.mod_switch.update(&mut combat.arsenal, h, now, actions.pressed("_reload"), busy) {
            Some(ModSwitchEvent::Switched { weapon, family }) if auto.trace => {
                let m = combat.arsenal.defs[weapon].mods.clone();
                let name = m.as_ref().and_then(|m| m.families.get(family)).map(|f| f.name().to_string()).unwrap_or_default();
                println!("[mods] t={now} switched {} -> {name} (upgrades {:?})", combat.arsenal.defs[weapon].decl, combat.arsenal.applied[weapon].upgrades);
            }
            Some(ModSwitchEvent::Hide { weapon }) if auto.trace => println!("[mods] t={now} hide for mod switch ({})", combat.arsenal.defs[weapon].decl),
            _ => {}
        }
    }
    // The lock-on's world (rancher_sim::weapons::targeting, read by UpdateTargeting at the top of
    // UpdateWeapon_Default): the live demons with their target points and clip boxes; LOS = world-only rays from
    // the eye (validation: to the origin, the clip query: to AIMPOINT_CENTER). INTERIM: other entities do not
    // block the clip query.
    combat.arsenal.target_world = lock_world(&sim.world, eye, fwd, demon_targets.as_deref());
    let events = match (driver.hands.as_mut(), web.as_mut()) {
        (Some(h), Some(rt)) => {
            // Player side (rancher_sim::player): falling = 0x140d63260; JUMP from the physics jump callback
            // 0x140e2fbe0 (jump and double jump); LAND_SM / MED / LG from the landing test 0x140e33110.
            // BUTTON_ATTACK2 is the melee button; BUTTON_ALTFIRE picks the fists' left punch.
            let ev = &player.events;
            let hin = HandsInput {
                weapon: input,
                can_use_weapon: true,
                weapon_enabled: true,
                falling: player.falling(),
                jumped: ev.jumped || ev.double_jumped,
                landed: ev.landed.and_then(|l| l.hands_action()).map(|a| match a {
                    18 => LandSize::Small,
                    19 => LandSize::Medium,
                    _ => LandSize::Large,
                }),
                melee: actions.pressed("_attack2") && !sync.available,
                altfire: actions.held("_altfire"),
            };
            let ev = h.tick(&mut combat.arsenal, &mut rt.rt, sim.msec_last, &hin);
            // For the additive channel drivers: destHandsState (hands +0x3b28) looping, handsFlags +0x10375.
            *status = crate::hands_layers::HandsStatus {
                dest_looping: matches!(h.target, HandsState::LoopingShoot | HandsState::ChargeLoopingShoot),
                flags1: h.flags.0[1],
                zoomed,
            };
            ev
        }
        _ => {
            *status = crate::hands_layers::HandsStatus::default();
            let released = driver.trigger_last && !input.trigger;
            let mut ev = combat.arsenal.tick(sim.msec_last, &input);
            // Sim-timed fallback: ReleaseTrigger on the release edge stops looping fire sounds.
            if released {
                ev.push(WeaponEvent::StopFireSound { weapon: combat.arsenal.current });
            }
            ev
        }
    };
    driver.trigger_last = input.trigger;

    // Melee trace (rancher_sim::weapons::melee, 0x140d7c980): the followed hands joint from last frame's
    // pose (ViewJoints, camera-local Bevy space) placed at this frame's eye and view axes.
    let mut melee_hits = Vec::new();
    if let (Some(h), Some(vj)) = (driver.hands.as_mut(), view_joints.as_ref()) {
        if h.melee.active() {
            if let Some(m) = vj.hands_joint(&h.melee.joint) {
                let p = m.w_axis;
                let joint = eye + fwd * -p.z + left * -p.x + up * p.y;
                let world = &sim.world;
                let targets = &combat.targets;
                let demons = demon_targets.as_deref();
                let mut sweep = |a: V, b: V, half: f32| melee_sweep(world, targets, demons, a, b, half);
                melee_hits = h.melee_update(now, joint, eye, fwd, &mut sweep);
            }
        }
    }
    for hit in melee_hits {
        let Some(dmg) = hit.damage else { continue };
        driver.shot_seq = driver.shot_seq.wrapping_add(1);
        if let Some(i) = hit.hit.target {
            combat.targets[i].damage_taken += dmg;
            spawn_popup(&mut commands, hit.hit.pos + V::new(0.0, 0.0, 24.0), dmg);
        }
        if let Some((id, joint)) = &hit.hit.demon {
            send_demon(*id, &hit.damage_decl, vec![TraceHit { joint: Some(joint.clone()), point: hit.hit.pos }], 1.0, hit.dir, -1.0);
        }
        let target = hit.hit.target.is_some() || hit.hit.demon.is_some();
        if let Some(m) = impacts.as_mut() {
            m.write(ShotImpact { pos: hit.hit.pos, normal: hit.hit.normal, surface: if hit.hit.demon.is_some() { "flesh".into() } else { String::new() }, projectile: hit.projectile.clone(), shot: driver.shot_seq, weapon: hit.weapon, target });
        }
    }

    if auto.trace {
        let w = combat.arsenal.current;
        let ms_ = &combat.arsenal.mstate[w];
        let key = (w, ms_.fire_mode, ms_.charge.state);
        if driver.trace_last != Some(key) {
            driver.trace_last = Some(key);
            let c = &ms_.charge;
            println!(
                "[mods] t={now} weapon {w} mode {} charge {:?} pct {:.3} can_charge_time {} burst_count {}",
                ms_.fire_mode, c.state, c.percent, c.can_charge_time, ms_.burst_count
            );
        }
    }
    for e in std::mem::take(&mut combat.arsenal.target_events) {
        match e {
            rancher_sim::weapons::TargetEvent::Sound(snd) => {
                if auto.trace {
                    println!("[lock] t={now} sound {snd}");
                }
                if let Some(s) = sound.as_mut() {
                    s.post(&snd, None, None);
                }
            }
            rancher_sim::weapons::TargetEvent::State { slot, from, to, target } if auto.trace => {
                let w = combat.arsenal.current;
                let sl = &combat.arsenal.targeting[w].slots[slot];
                println!("[lock] t={now} slot {slot} {from:?} -> {to:?} target {target:?} lock_percent {:.3} clear_after {}", sl.lock_percent, sl.clear_after_num_shots);
            }
            _ => {}
        }
    }
    let mut detonations: Vec<u32> = Vec::new();
    for ev in events {
        let shot = match ev {
            WeaponEvent::Fired(shot) => shot,
            // Remote detonate (rancher_sim::weapons::detonate): explode the listed rockets where they are.
            WeaponEvent::Detonate { projectiles, sound: snd, .. } => {
                if auto.trace {
                    println!("[detonate] t={now} explode {projectiles:?} sound {snd}");
                }
                if let (Some(s), false) = (sound.as_mut(), snd.is_empty()) {
                    s.post(&snd, None, None);
                }
                detonations.extend(projectiles);
                continue;
            }
            WeaponEvent::Sound { sound: snd, .. } => {
                if auto.trace {
                    println!("[detonate] t={now} sound {snd}");
                }
                if let Some(s) = sound.as_mut() {
                    s.post(&snd, None, None);
                }
                continue;
            }
            // idWeapon::StopFireSound 0x140f23a80 (looping fire sounds: plasma).
            WeaponEvent::StopFireSound { weapon } => {
                if let Some(s) = sound.as_mut() {
                    s.weapon_stopped_firing(&combat.arsenal.wdef(weapon));
                }
                continue;
            }
            // Charge feedback (rancher_sim::weapons::charge): start / interval / fully charged sounds.
            WeaponEvent::ChargeSound { sound: snd, .. } => {
                if auto.trace {
                    println!("[mods] t={now} charge sound {snd}");
                }
                if let Some(s) = sound.as_mut() {
                    s.post(&snd, None, None);
                }
                continue;
            }
            WeaponEvent::FireMode { weapon, mode } => {
                if auto.trace {
                    let d = combat.arsenal.wdef(weapon);
                    println!("[mods] t={now} fire mode {mode} ({}) trigger {}", d.decl, combat.arsenal.secondary_trigger_mode(weapon));
                }
                continue;
            }
            _ => continue,
        };
        driver.shot_seq = driver.shot_seq.wrapping_add(1);
        let shot_id = driver.shot_seq;
        // The shot's fire-mode decl (mod decls, DECL_AMMO projectiles).
        let def = shot.def.0.clone();
        if auto.trace {
            println!(
                "[mods] t={} shot weapon {} mode {} decl {} projectile {} pellets {} ammo {} burst_left {} charge {:.3} damage_scale {:.2} next {} target {:?}",
                shot.time_ms,
                shot.weapon,
                shot.mode,
                def.decl,
                def.projectile.name,
                shot.dirs.len(),
                shot.ammo_used,
                shot.burst_left,
                shot.charge,
                shot.damage_scale,
                shot.next_fire_ms,
                shot.target
            );
        }
        // idWeapon::Fire -> PlayFireSound 0x140f1def0: loopingFireSound (started once) else fireSound.
        if let Some(s) = sound.as_mut() {
            s.weapon_fired(&def);
        }
        let proj = &def.projectile;
        combat.shot_total = 0.0;
        combat.arsenal.last_shot_dirs = shot.dirs.clone();
        if proj.hitscan {
            for &dir in &shot.dirs {
                let range = proj.max_range.min(def.max_range).max(1.0);
                let wall_hit = sim.world.ray(eye, dir, range);
                let wall = wall_hit.map(|h| h.0).unwrap_or(range);
                let mut best: Option<(f32, usize, V)> = None;
                for (i, t) in combat.targets.iter().enumerate() {
                    if let Some((d, n)) = ray_box_normal(eye, dir, t.center, t.half) {
                        if d < wall && best.is_none_or(|b| d < b.0) {
                            best = Some((d, i, n));
                        }
                    }
                }
                // Demons (crate::demons: the md6Def hitTestGroup spheres, last frame's pose) nearer than the
                // wall and the dummies take the pellet, one idAI2::Damage call per trace (whether the game
                // batches a shot's pellets per entity is not decoded, WEAPONS.md).
                let near = best.map(|b| b.0).unwrap_or(wall);
                let demon = demon_targets.as_deref().and_then(|dt| dt.trace(eye, dir, range)).filter(|h| h.dist < near);
                if demon.is_some() {
                    best = None;
                }
                let end_d = demon.as_ref().map(|h| h.dist).unwrap_or(near);
                let end = eye + dir * end_d;
                let from = eye - up * 6.0 + fwd * 16.0;
                if let Some(m) = tracers.as_mut() {
                    m.write(ShotTracer { from, to: end, projectile: proj.name.clone(), shot: shot_id, weapon: shot.weapon });
                }
                let impact = match (&demon, best, wall_hit) {
                    (Some(h), _, _) => Some((h.normal, true)),
                    (None, Some((_, _, n)), _) => Some((n, true)),
                    (None, None, Some((_, n, _))) => Some((n, false)),
                    _ => None,
                };
                if let Some(h) = &demon {
                    send_demon(h.id, &proj.damage.name, vec![TraceHit { joint: Some(h.joint.clone()), point: h.point }], shot.damage_scale, dir, -1.0);
                }
                if let (Some((normal, target)), Some(m)) = (impact, impacts.as_mut()) {
                    m.write(ShotImpact { pos: end, normal, surface: if demon.is_some() { "flesh".into() } else { String::new() }, projectile: proj.name.clone(), shot: shot_id, weapon: shot.weapon, target });
                }
                if let Some((d, i, _)) = best {
                    let dmg = proj.damage.at_distance(d) * shot.damage_scale;
                    combat.targets[i].damage_taken += dmg;
                    combat.shot_total += dmg;
                    combat.last_damage.push((d, dmg));
                }
            }
            if combat.shot_total > 0.0 {
                let tgt = combat.targets.iter().min_by(|a, b| (a.center - eye).length().total_cmp(&(b.center - eye).length())).map(|t| t.center);
                if let Some(c) = tgt {
                    spawn_popup(&mut commands, c + V::new(0.0, 0.0, 56.0), combat.shot_total);
                }
            }
        } else {
            let speed = if proj.speed > 0.0 { proj.speed } else { 1000.0 };
            for &dir in &shot.dirs {
                driver.projectile_seq = driver.projectile_seq.wrapping_add(1);
                let id = driver.projectile_seq;
                // The shot's lock target (InitForFire) becomes the rocket's seek target (SetSeekTarget 0x140f3e610,
                // centred on the target's clip box). INTERIM: the launch call that hands the fire-params target to
                // SetSeekTarget is not traced, and the startSpeed / acceleration ramp (lock-on rockets 1300 -> 2500 at
                // 1000 u/s^2, idProjectile +0x46b0) is not ported: projectiles fly at notHitscanInfo.speed.
                let center = shot.target.and_then(|t| demon_targets.as_deref().and_then(|dt| dt.demons.iter().find(|d| d.id == t))).map(|d| (d.clip_bounds.0 + d.clip_bounds.1) * 0.5);
                let seek = match (shot.target, center) {
                    (Some(t), Some(c)) => rancher_sim::weapons::seek::Seeker::start(&proj.seek, t, c, shot.time_ms, &mut combat.arsenal.rng),
                    _ => None,
                };
                if auto.trace && seek.is_some() {
                    println!("[seek] t={} projectile {id} target {:?} state {:?}", shot.time_ms, shot.target, seek.as_ref().map(|s| s.state));
                }
                combat.arsenal.add_launched(shot.weapon, &def, id, shot.time_ms);
                combat.projectiles.push(rancher_sim::weapons::Projectile { def: def.clone(), pos: eye + fwd * 16.0, vel: dir * speed, age_ms: 0.0, weapon: shot.weapon, shot: shot_id, damage_scale: shot.damage_scale, id, seek });
            }
        }
    }

    // Projectiles: straight-line flight (gravity if the decl asks), impact, splash.
    let dt = ms * 0.001;
    let mut i = 0;
    while i < combat.projectiles.len() {
        // UpdateSeek (rancher_sim::weapons::seek) before the move: the target's AIMPOINT_CENTER.
        let now_g = sim.game_ms;
        let cm = &mut *combat;
        let pr = &mut cm.projectiles[i];
        if let Some(sk) = pr.seek.as_mut() {
            let tgt = sk.target.and_then(|t| demon_targets.as_deref().and_then(|dt| dt.demons.iter().find(|d| d.id == t))).map(|d| rancher_sim::weapons::seek::SeekTarget { aim_point: d.aim_points[rancher_sim::weapons::targeting::AIMPOINT_CENTER], velocity: V::ZERO, alive: true });
            let before = sk.state;
            if let Some(v) = sk.update(&pr.def.projectile.seek, dt, now_g, pr.pos, pr.vel, tgt, &mut cm.arsenal.rng) {
                pr.vel = v;
            }
            if auto.trace && before != sk.state {
                println!("[seek] t={now_g} projectile {} {:?} -> {:?} at ({:.0} {:.0} {:.0})", pr.id, before, sk.state, pr.pos.x, pr.pos.y, pr.pos.z);
            }
        }
        let p = combat.projectiles[i].clone();
        // Remote detonation: explode in place (no direct hit), splash only.
        let detonated = detonations.contains(&p.id);
        let mut vel = p.vel;
        if p.def.projectile.gravity {
            vel += V::new(0.0, 0.0, -sim.player.cfg.gravity) * dt;
        }
        let step = vel * dt;
        let len = step.length();
        let dir = step / len.max(1e-6);
        let wall = sim.world.ray(p.pos, dir, len).map(|h| (h.0, h.1));
        let tgt = combat.targets.iter().enumerate().filter_map(|(ti, t)| ray_box_normal(p.pos, dir, t.center, t.half).filter(|h| h.0 <= len).map(|(d, n)| (d, ti, n))).min_by(|a, b| a.0.total_cmp(&b.0));
        let mut hit = match (wall, tgt) {
            (Some((w, wn)), Some((t, ti, tn))) => Some(if t < w { (t, Some(ti), tn) } else { (w, None, wn) }),
            (Some((w, wn)), None) => Some((w, None, wn)),
            (None, Some((t, ti, tn))) => Some((t, Some(ti), tn)),
            (None, None) => None,
        };
        let demon = demon_targets.as_deref().and_then(|dt| dt.trace(p.pos, dir, len)).filter(|h| hit.is_none_or(|x| h.dist < x.0));
        if let Some(h) = &demon {
            hit = Some((h.dist, None, h.normal));
        }
        if detonated {
            hit = Some((0.0, None, -dir));
        }
        if let Some((d, direct, normal)) = hit.filter(|_| p.age_ms < 10000.0 || detonated) {
            let at = p.pos + dir * d;
            if auto.trace && (detonated || p.seek.is_some()) {
                println!("[projectile] t={} {} {} at ({:.0} {:.0} {:.0}) demon {:?}", sim.game_ms, p.id, if detonated { "detonated" } else { "impact" }, at.x, at.y, at.z, demon.as_ref().map(|h| h.id));
            }
            if let Some(m) = impacts.as_mut() {
                m.write(ShotImpact { pos: at, normal, surface: if demon.is_some() { "flesh".into() } else { String::new() }, projectile: p.def.projectile.name.clone(), shot: p.shot, weapon: p.weapon, target: direct.is_some() || demon.is_some() });
            }
            if let Some(h) = &demon {
                send_demon(h.id, &p.def.projectile.damage.name, vec![TraceHit { joint: Some(h.joint.clone()), point: h.point }], p.damage_scale, dir, -1.0);
            }
            let mut total = 0.0;
            if let Some(ti) = direct {
                let dmg = p.def.projectile.damage.max * p.damage_scale;
                combat.targets[ti].damage_taken += dmg;
                total += dmg;
            }
            if let Some(s) = &p.def.projectile.splash {
                // Radius damage against each target's bounds (0x1403861e0); the expanding shockwave delay
                // (expansionSpeed) is not modelled here, damage lands on impact.
                for t in combat.targets.iter_mut() {
                    if let Some(dmg) = s.splash_on_box(at, t.center - t.half, t.center + t.half) {
                        t.damage_taken += dmg * p.damage_scale;
                        total += dmg * p.damage_scale;
                    }
                }
                // Demons: the radius scale (before the decl damage) and dist^2 / radius^2 for the SDPS
                // window; like the dummies, a directly hit demon also gets the splash.
                if let Some(dt) = demon_targets.as_deref() {
                    let r = s.splash_radius();
                    for (id, lo, hi) in dt.in_radius(at, r) {
                        let Some(d2) = s.splash_dist_sq(at, lo, hi) else { continue };
                        if let Some(scale) = s.splash_scale(d2) {
                            send_demon(id, &s.name, Vec::new(), scale * p.damage_scale, dir, d2 / (r * r));
                        }
                    }
                }
            }
            if total > 0.0 {
                spawn_popup(&mut commands, at + V::new(0.0, 0.0, 24.0), total);
            }
            combat.arsenal.launched_state(p.id, rancher_sim::weapons::detonate::PROJECTILE_EXPLODED);
            combat.projectiles.swap_remove(i);
            continue;
        }
        if p.age_ms > 10000.0 {
            combat.arsenal.launched_state(p.id, rancher_sim::weapons::detonate::PROJECTILE_EXPLODED);
            combat.projectiles.swap_remove(i);
            continue;
        }
        let pr = &mut combat.projectiles[i];
        pr.pos += step;
        pr.vel = vel;
        pr.age_ms += ms;
        i += 1;
    }
}

/// The lock-on targets this frame (rancher_sim::weapons::targeting::LockTarget) from the demons' last pose.
fn lock_world(world: &rancher_sim::collision::World, eye: V, fwd: V, demons: Option<&crate::demons::DemonTargets>) -> rancher_sim::weapons::TargetWorld {
    use rancher_sim::weapons::targeting::{LockTarget, AIMPOINT_CENTER};
    let clear = |to: V| {
        let d = to - eye;
        let len = d.length();
        len <= 0.0 || world.ray(eye, d / len, len).is_none()
    };
    let targets = demons
        .map(|dt| {
            dt.demons
                .iter()
                .map(|d| LockTarget { id: d.id, origin: d.origin, aim_points: d.aim_points, bounds: d.clip_bounds, eligible: true, origin_visible: clear(d.origin), center_visible: clear(d.aim_points[AIMPOINT_CENTER]) })
                .collect()
        })
        .unwrap_or_default();
    rancher_sim::weapons::TargetWorld { eye, forward: fwd, targets }
}

fn spawn_popup(commands: &mut Commands, at: V, amount: f32) {
    commands.spawn((
        Text::new(format!("{amount:.0}")),
        TextFont { font_size: bevy::text::FontSize::Px(22.0), ..default() },
        TextColor(Color::srgb(1.0, 0.85, 0.2)),
        Node { position_type: PositionType::Absolute, ..default() },
        Popup { ttl: 1.2, world: to_bevy(at) },
    ));
}

pub fn fx_tick(mut commands: Commands, time: Res<Time>, mut popups: Query<(Entity, &mut Popup, &mut Node)>, cam: Query<(&Camera, &GlobalTransform)>) {
    let dt = time.delta_secs();
    let main_cam = cam.iter().find(|(c, _)| c.order == 0);
    for (e, mut p, mut node) in &mut popups {
        p.ttl -= dt;
        p.world.y += dt * 24.0;
        if p.ttl <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        if let Some((c, gt)) = main_cam {
            if let Ok(s) = c.world_to_viewport(gt, p.world) {
                node.left = Val::Px(s.x);
                node.top = Val::Px(s.y);
            }
        }
    }
}
