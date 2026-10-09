//! The Possessed Soldier in the testbed: its brain (rancher_sim::ai::soldier::SoldierBrain) drives the web as the
//! other demons' (super::apply_ai_output); the shooting anim's ae_aIPullTrigger / ae_aIReleaseTrigger hold the
//! rifle's trigger and the bolts (projectile/zion/ai/hellified_soldier/plasma) leave from the `muzzlereference`
//! joint (the aim tag the anims' ae_setFocusTagName events name; INTERIM: the game asks the weapon's muzzle,
//! 0x140433840), flying through super::imp::fly like the Imp's fireballs.

use glam::Mat4;
use rancher_sim::Vec3 as V;
use rancher_sim::demons::projectile::AiProjectile;

use super::{DemonInst, LiveWorld, ai_inputs, apply_ai_output};

/// The soldier's brain tick.
#[allow(clippy::too_many_arguments)]
pub(super) fn think(inst: &mut DemonInst, sim: &crate::Sim, nav: Option<&rancher_sim::ai::Nav>, player_health: Option<f32>, web: &str, default_blend: f32, now: i32, dt: f32, log: bool) {
    let (body, target, idle) = ai_inputs(inst, sim, player_health);
    let Some(brain) = inst.soldier.as_mut() else { return };
    let ticks = now as i64 * rancher_sim::ai::TICKS_PER_SEC / 1000;
    let out = brain.tick(&LiveWorld { world: &sim.world, nav }, &body, &target, ticks, dt);
    apply_ai_output(inst, &out, &body, target.origin, idle, web, default_blend, now, log);
}

/// ae_aIPullTrigger / ae_aIReleaseTrigger fired by the web this frame.
pub(super) fn trigger_events(inst: &mut DemonInst, fired: &[rancher_sim::animweb::FiredEvent], now: i32, log: bool) {
    let Some(brain) = inst.soldier.as_mut() else { return };
    for f in fired {
        let pulled = match f.event.name.as_str() {
            "ae_aIPullTrigger" => true,
            "ae_aIReleaseTrigger" => false,
            _ => continue,
        };
        brain.set_trigger(pulled, now as i64 * rancher_sim::ai::TICKS_PER_SEC / 1000);
        if log {
            println!("[demon {} {now}] {} ({}) trigger {}", inst.id, f.event.name, f.anim, brain.trigger);
        }
    }
}

/// The shots due this frame from the muzzle joint of this frame's pose, aimed at the player's sight point
/// (INTERIM: the fire control's aim point is not decoded; the clip-bounds centre, pm_normalheight / 2).
pub(super) fn fire(inst: &mut DemonInst, names: &[String], joints: &[Mat4], m2w: Mat4, sim: &crate::Sim, now: i32, log: bool) -> Vec<AiProjectile> {
    let id = inst.id;
    let origin = inst.sim.origin;
    let dead = inst.sim.is_dead();
    let Some(brain) = inst.soldier.as_mut() else { return Vec::new() };
    let n = brain.shots_due(now as i64 * rancher_sim::ai::TICKS_PER_SEC / 1000);
    if n == 0 || dead {
        return Vec::new();
    }
    // muzzlereference, else any *muzzle* joint, else the rifle's thirdPersonAttachTag rightforearm.
    let j = names
        .iter()
        .position(|n| n.eq_ignore_ascii_case("muzzlereference"))
        .or_else(|| names.iter().position(|n| n.to_ascii_lowercase().contains("muzzle")))
        .or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case("rightforearm")));
    let muzzle = j.map_or(origin + V::Z * 50.0, |j| m2w.transform_point3(joints[j].w_axis.truncate()));
    let joint = j.map_or("<none>", |j| names[j].as_str());
    let p = &sim.player;
    let aim = brain.base.sense.last_known_pos + V::Z * (p.cfg.normal_height * 0.5);
    let mut out = Vec::new();
    for _ in 0..n {
        let Some(vel) = brain.shot_velocity(muzzle, aim, p.physics.velocity) else { continue };
        if log {
            println!("[demon {id} {now}] shot {} ({:?}) from {joint} ({:.1} {:.1} {:.1}) vel ({:.1} {:.1} {:.1})", brain.projectile.name, brain.fire_mode(), muzzle.x, muzzle.y, muzzle.z, vel.x, vel.y, vel.z);
        }
        out.push(AiProjectile { def: brain.projectile.clone(), pos: muzzle, vel, age: 0.0, owner: id, launch: muzzle });
    }
    out
}
