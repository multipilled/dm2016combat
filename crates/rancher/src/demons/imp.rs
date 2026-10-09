//! The Imp in the testbed: its brain (rancher_sim::ai::imp::ImpBrain) drives the web as the Possessed's does
//! (super::apply_ai_output), the throw anim's ae_launchItem launches the fireball from the event's joint
//! (idAI2::AnimEvent_LaunchItem 0x1403e9b60), and the fireballs (rancher_sim::demons::projectile) fly through the
//! sim's collision world and hit the player through idPlayer::Damage (live::player_damage).
//! INTERIM: the fireball is drawn as a plain emissive sphere (fx/creatures/imp_fireball is not played).

use bevy::prelude::*;
use rancher_sim::Vec3 as V;
use rancher_sim::ai::imp::ImpBrain;
use rancher_sim::demons::projectile::{AiProjectile, Impact};

use super::{DemonInst, Demons, LiveWorld, ai_inputs, apply_ai_output};

/// The Imp's brain tick (as super::think for the Possessed).
#[allow(clippy::too_many_arguments)]
pub(super) fn think(inst: &mut DemonInst, sim: &crate::Sim, nav: Option<&rancher_sim::ai::Nav>, player_health: Option<f32>, web: &str, default_blend: f32, now: i32, dt: f32, log: bool) {
    let (body, target, idle) = ai_inputs(inst, sim, player_health);
    let Some(brain) = inst.imp.as_mut() else { return };
    let ticks = now as i64 * rancher_sim::ai::TICKS_PER_SEC / 1000;
    let out = brain.tick(&LiveWorld { world: &sim.world, nav }, &body, &target, ticks, dt);
    apply_ai_output(inst, &out, &body, target.origin, idle, web, default_blend, now, log);
}

/// A fireball in flight and its render entity.
pub(super) struct Fireball {
    pub proj: AiProjectile,
    pub entity: Entity,
}

/// ae_launchItem at `hand` (the event joint's world position this frame): the brain's launch velocity toward the
/// player, a new projectile.
pub(super) fn launch(inst: &mut DemonInst, hand: V, sim: &crate::Sim, gravity: f32, log: bool, now: i32) -> Option<AiProjectile> {
    let brain: &mut ImpBrain = inst.imp.as_mut()?;
    // AddTrajectoryTestForQuery 0x140483050, aim type itemSelect AIITEMSELECT_IMP (8): the enemy's last known
    // position + half the offset of its vslot 0x718 point from its origin (INTERIM: the player's eye,
    // pm_normalViewHeight).
    let p = &sim.player;
    let target = brain.base.sense.last_known_pos + V::Z * (p.cfg.normal_view_height * 0.5);
    let vel = brain.launch_velocity(hand, target, p.physics.velocity, gravity)?;
    if log {
        println!("[demon {} {now}] launch {} from ({:.1} {:.1} {:.1}) vel ({:.1} {:.1} {:.1}) |v| {:.1} at player ({:.1} {:.1} {:.1})", inst.id, brain.projectile.name, hand.x, hand.y, hand.z, vel.x, vel.y, vel.z, vel.length(), target.x, target.y, target.z);
    }
    Some(AiProjectile { def: brain.projectile.clone(), pos: hand, vel, age: 0.0, owner: inst.id, launch: hand })
}

/// Spawns the render entity of a new fireball.
pub(super) fn spawn_fireball(commands: &mut Commands, meshes: &mut Assets<Mesh>, mats: &mut Assets<StandardMaterial>, proj: AiProjectile) -> Fireball {
    let r = proj.def.clip_size.max_element() * 0.5;
    let mesh = meshes.add(Sphere::new(r).mesh().ico(2).unwrap_or_else(|_| Sphere::new(r).mesh().build()));
    // INTERIM look: orange for fireballs, red for the soldier's plasma (fx/projectile/plasma/plasma_red not played).
    let (base, glow) = if proj.def.name.contains("plasma") { (Color::srgb(1.0, 0.15, 0.2), LinearRgba::rgb(8.0, 0.8, 1.2)) } else { (Color::srgb(1.0, 0.45, 0.1), LinearRgba::rgb(8.0, 2.5, 0.4)) };
    let mat = mats.add(StandardMaterial { base_color: base, emissive: glow, unlit: true, ..default() });
    let entity = commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(crate::range::to_bevy(proj.pos)))).id();
    Fireball { proj, entity }
}

/// Moves the fireballs one frame; a fireball that reaches the player's clip box damages them (idPlayer::Damage of
/// the projectile's damage decl, damage falloff over the launch -> player distance) and is removed with any that
/// hit the world or outlive the INTERIM lifetime.
pub(super) fn fly(d: &mut Demons, sim: &crate::Sim, hud: &mut Option<ResMut<crate::swf_hud::HudState>>, defense: Option<&crate::pickups::PlayerDefense>, commands: &mut Commands, xf: &mut Query<&mut Transform>, now: i32) {
    let dt = sim.msec_last as f32 * 0.001;
    let p = &sim.player;
    let (w, po) = (p.cfg.bbox_width * 0.5, p.physics.origin);
    let player = (po + V::new(-w, -w, 0.0), po + V::new(w, w, p.cfg.normal_height));
    let mut i = 0;
    while i < d.fireballs.len() {
        let fb = &mut d.fireballs[i];
        let hit = fb.proj.step(&sim.world, d.gravity, player, dt);
        let expired = fb.proj.age > rancher_sim::demons::projectile::MAX_FLIGHT_SECS_INTERIM;
        if hit.is_none() && !expired {
            if let Ok(mut t) = xf.get_mut(fb.entity) {
                t.translation = crate::range::to_bevy(fb.proj.pos);
            }
            i += 1;
            continue;
        }
        let fb = d.fireballs.swap_remove(i);
        commands.entity(fb.entity).despawn();
        match hit {
            Some(Impact::Player { at }) => {
                let parms = match d.parms.get(&d.db, &fb.proj.def.damage_decl) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("demons: {e:#}");
                        continue;
                    }
                };
                let dmg = rancher_sim::demons::live::player_damage(&parms, fb.proj.launch.distance_squared(po), 1.0, &d.player_scales);
                // The suit's damage scale and the armour split (shared player damage path, crate::pickups::hit_player).
                let hit = crate::pickups::hit_player(&mut d.player_health, hud.as_deref_mut(), defense, dmg);
                if d.log {
                    let ((before, after), (a0, a1)) = (hit.health, hit.armor);
                    println!("[demon {} {now}] fireball hit player at ({:.1} {:.1} {:.1}) after {:.2}s: {} damage {dmg:.2} (difficulty x{:.2}) health {before:.1} -> {after:.1} armour {a0:.1} -> {a1:.1}", fb.proj.owner, at.x, at.y, at.z, fb.proj.age, fb.proj.def.damage_decl, d.player_scales.difficulty);
                }
            }
            Some(Impact::World { at, .. }) if d.log => println!("[demon {} {now}] fireball hit world at ({:.1} {:.1} {:.1}) after {:.2}s", fb.proj.owner, at.x, at.y, at.z, fb.proj.age),
            _ => {}
        }
    }
}
