//! Map movers: idMover `moverExtender` move commands and the bind chains hanging off them
//! (`bindInfo { bindParent, bindOriented }`), advanced on the game clock. Render side: entities that
//! are movers or bound to one are drawn as their own Bevy entities and follow these transforms.
//! Collision: each moved collision model goes through rancher_sim's `Player::push_mover` (push/carry).
//!
//! INTERIM (motion semantics not decoded from idMover yet): `direction` is a world-space offset,
//! `rotation` degrees about world x/y/z, duration = distance / speed when speed > 0 else `time`
//! seconds, `accel`/`decel` seconds of linear velocity ramp, `delay` seconds before moving.
//! Test hook: `RANCHER_MAP_ACTIVATE=<entity>@<seconds>[,...]` activates movers at game times.

use std::collections::HashMap;

use bevy::prelude::*;
use idres::entities::{BindInfo, MoveCommand};
use rancher_sim::Vec3 as V;
use rancher_sim::collision::Mat3;

use crate::range::to_bevy;

/// A mover or an entity bound (directly or through others) to one, as map::load found it.
#[derive(Clone, Debug)]
pub struct MoverSpec {
    pub entity: String,
    pub origin: V,
    /// Columns = the entity's axes in world space (spawnOrientation rows).
    pub axis: Mat3,
    pub bind: Option<BindInfo>,
    pub commands: Vec<MoveCommand>,
    /// The entity's collision model in the sim World (add_cm index).
    pub cm: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
enum State {
    Idle,
    /// Command `cmd` starts at `at_ms`.
    Delayed { at_ms: i64 },
    Moving { from_o: V, from_r: Mat3, to_o: V, to_r: Mat3, start_ms: i64, dur_ms: i64, accel_ms: i64, decel_ms: i64 },
    /// Arrived; waiting for the next activation before command `cmd`.
    Waiting,
}

struct Node {
    spec: MoverSpec,
    parent: Option<usize>,
    /// Pose relative to the parent at spawn (origin in parent space, rotation), for oriented binds.
    local: (V, Mat3),
    origin: V,
    rot: Mat3,
    cmd: usize,
    state: State,
    render: Option<Entity>,
}

/// Mover runtime state (inserted by MapPlugin).
#[derive(Resource, Default)]
pub struct Movers {
    nodes: Vec<Node>,
    by_name: HashMap<String, usize>,
    /// Parents before children.
    order: Vec<usize>,
    /// World poses of every entity a command may name as `position`.
    targets: HashMap<String, (V, Mat3)>,
    /// (game ms, entity) activations from RANCHER_MAP_ACTIVATE.
    scheduled: Vec<(i64, String)>,
}

impl Movers {
    pub fn new(specs: Vec<MoverSpec>, targets: HashMap<String, (V, Mat3)>) -> Self {
        let mut m = Movers { targets, ..default() };
        for s in specs {
            m.by_name.insert(s.entity.clone(), m.nodes.len());
            m.nodes.push(Node { origin: s.origin, rot: s.axis, local: (V::ZERO, Mat3::IDENTITY), parent: None, cmd: 0, state: State::Idle, render: None, spec: s });
        }
        for i in 0..m.nodes.len() {
            let p = m.nodes[i].spec.bind.as_ref().and_then(|b| m.by_name.get(&b.parent).copied()).filter(|&p| p != i);
            if let Some(p) = p {
                let (po, pr) = (m.nodes[p].origin, m.nodes[p].rot);
                let n = &mut m.nodes[i];
                n.parent = Some(p);
                n.local = (pr.transpose() * (n.origin - po), pr.transpose() * n.rot);
            }
        }
        // Topological order (bind chains are short; guard against cycles).
        let mut depth = vec![0usize; m.nodes.len()];
        for (i, d) in depth.iter_mut().enumerate() {
            let (mut j, mut k) = (i, 0);
            while let Some(p) = m.nodes[j].parent {
                j = p;
                k += 1;
                if k > 64 {
                    break;
                }
            }
            *d = k;
        }
        m.order = (0..m.nodes.len()).collect();
        m.order.sort_by_key(|&i| depth[i]);
        if let Ok(list) = std::env::var("RANCHER_MAP_ACTIVATE") {
            for item in list.split(',').filter(|s| !s.is_empty()) {
                let (name, t) = item.split_once('@').unwrap_or((item, "0"));
                m.scheduled.push(((t.trim().parse::<f32>().unwrap_or(0.0) * 1000.0) as i64, name.trim().to_string()));
            }
        }
        m
    }

    /// The Bevy root entity of a mover/bound entity (spawned by `spawn_roots`).
    pub fn render(&self, entity: &str) -> Option<Entity> {
        self.by_name.get(entity).and_then(|&i| self.nodes[i].render)
    }

    /// Activates a mover: starts its current command, or the next one when it waits for activation.
    pub fn activate(&mut self, entity: &str, now_ms: i64) {
        let Some(&i) = self.by_name.get(entity) else {
            eprintln!("map: activate {entity}: not a mover");
            return;
        };
        let n = &mut self.nodes[i];
        if n.spec.commands.is_empty() {
            return;
        }
        match n.state {
            State::Idle | State::Waiting => {
                if n.cmd >= n.spec.commands.len() {
                    n.cmd = 0;
                }
                let delay = (n.spec.commands[n.cmd].delay * 1000.0) as i64;
                n.state = State::Delayed { at_ms: now_ms + delay };
            }
            _ => {}
        }
    }

    fn start(&mut self, i: usize, now_ms: i64) -> Vec<String> {
        let n = &self.nodes[i];
        let c = &n.spec.commands[n.cmd];
        let (from_o, from_r) = (n.origin, n.rot);
        let (mut to_o, mut to_r) = (from_o + V::from(c.direction), from_r);
        if !c.position.is_empty() {
            if let Some(&(o, r)) = self.targets.get(&c.position) {
                to_o = o;
                if c.align_to_position {
                    to_r = r;
                }
            }
        }
        let rot = c.rotation;
        if rot != [0.0; 3] {
            let r = Mat3::from_rotation_z(rot[2].to_radians()) * Mat3::from_rotation_y(rot[1].to_radians()) * Mat3::from_rotation_x(rot[0].to_radians());
            to_r = r * to_r;
        }
        let dist = (to_o - from_o).length();
        let secs = if c.speed > 0.0 { dist / c.speed } else { c.time };
        let dur_ms = ((secs * 1000.0) as i64).max(1);
        let accel_ms = ((c.accel * 1000.0) as i64).clamp(0, dur_ms);
        let decel_ms = ((c.decel * 1000.0) as i64).clamp(0, dur_ms - accel_ms);
        let fire = c.activate_on_start.clone();
        self.nodes[i].state = State::Moving { from_o, from_r, to_o, to_r, start_ms: now_ms, dur_ms, accel_ms, decel_ms };
        if fire.is_empty() { Vec::new() } else { vec![fire] }
    }

    /// Advances every mover to `now_ms`; returns the entities whose pose changed with their previous pose.
    pub fn tick(&mut self, now_ms: i64) -> Vec<(usize, V, Mat3)> {
        let due: Vec<String> = self.scheduled.iter().filter(|(t, _)| *t <= now_ms).map(|(_, n)| n.clone()).collect();
        self.scheduled.retain(|(t, _)| *t > now_ms);
        let mut fire = due;
        let mut guard = 0;
        while let Some(name) = fire.pop() {
            self.activate(&name, now_ms);
            guard += 1;
            if guard > 256 {
                break;
            }
        }
        let mut changed = Vec::new();
        for k in 0..self.order.len() {
            let i = self.order[k];
            let mut fired = Vec::new();
            if let State::Delayed { at_ms } = self.nodes[i].state {
                if now_ms >= at_ms {
                    fired.extend(self.start(i, now_ms));
                }
            }
            let before = (self.nodes[i].origin, self.nodes[i].rot);
            if let State::Moving { from_o, from_r, to_o, to_r, start_ms, dur_ms, accel_ms, decel_ms } = self.nodes[i].state {
                let t = ((now_ms - start_ms) as f32).clamp(0.0, dur_ms as f32);
                let f = ramp(t, dur_ms as f32, accel_ms as f32, decel_ms as f32);
                let n = &mut self.nodes[i];
                n.origin = from_o.lerp(to_o, f);
                n.rot = slerp(from_r, to_r, f);
                if now_ms - start_ms >= dur_ms {
                    let c = &n.spec.commands[n.cmd];
                    if !c.activate_on_arrival.is_empty() {
                        fired.push(c.activate_on_arrival.clone());
                    }
                    let wait = c.wait_for_next_activate;
                    n.cmd += 1;
                    n.state = if n.cmd < n.spec.commands.len() {
                        if wait { State::Waiting } else { State::Delayed { at_ms: now_ms + (n.spec.commands[n.cmd].delay * 1000.0) as i64 } }
                    } else {
                        State::Idle
                    };
                }
            } else if let Some(p) = self.nodes[i].parent {
                let (po, pr, po0) = (self.nodes[p].origin, self.nodes[p].rot, self.nodes[p].spec.origin);
                let n = &mut self.nodes[i];
                if n.spec.bind.as_ref().is_some_and(|b| b.oriented) {
                    n.origin = po + pr * n.local.0;
                    n.rot = pr * n.local.1;
                } else {
                    // Not oriented: follows the parent's translation only.
                    n.origin = n.spec.origin + (po - po0);
                }
            }
            if (self.nodes[i].origin, self.nodes[i].rot) != before {
                changed.push((i, before.0, before.1));
            }
            for name in fired {
                self.activate(&name, now_ms);
            }
        }
        changed
    }

    /// Moves the changed entities' collision with the sim, one `push_team` per bind team (master
    /// first, as the engine runs a team's physics). A team the player is pinned against does not move
    /// (idPhysics_Parametric::Evaluate 0x1417843d0 restores the pre-move state): its parts go back to
    /// their previous pose and its root's command timer is held for the frame (INTERIM; how the mover
    /// entity reacts to being blocked is not decoded).
    pub fn push(&mut self, changed: &mut [(usize, V, Mat3)], player: &mut rancher_sim::player::Player, world: &mut rancher_sim::collision::World, msec: i64) {
        let root = |nodes: &[Node], mut r: usize| {
            while let Some(p) = nodes[r].parent {
                r = p;
            }
            r
        };
        // `changed` follows `order` (parents first), so each team's list starts with its master.
        let mut teams: Vec<(usize, Vec<(usize, V, Mat3)>)> = Vec::new();
        for &(i, _, _) in changed.iter() {
            let Some(cm) = self.nodes[i].spec.cm else { continue };
            let r = root(&self.nodes, i);
            let part = (cm, self.nodes[i].origin, self.nodes[i].rot);
            match teams.iter_mut().find(|(t, _)| *t == r) {
                Some((_, parts)) => parts.push(part),
                None => teams.push((r, vec![part])),
            }
        }
        let blocked: Vec<usize> = teams.into_iter().filter(|(_, parts)| !player.push_team(world, parts, 0)).map(|(r, _)| r).collect();
        if blocked.is_empty() {
            return;
        }
        for &r in &blocked {
            if let State::Moving { ref mut start_ms, .. } = self.nodes[r].state {
                *start_ms += msec;
            }
        }
        for &(i, o, rot) in changed.iter() {
            if blocked.contains(&root(&self.nodes, i)) {
                self.nodes[i].origin = o;
                self.nodes[i].rot = rot;
            }
        }
    }

    /// Copies poses to the render entities.
    pub fn sync_render(&self, changed: &[(usize, V, Mat3)], transforms: &mut Query<&mut Transform>) {
        for &(i, _, _) in changed {
            let n = &self.nodes[i];
            if let Some(e) = n.render {
                if let Ok(mut t) = transforms.get_mut(e) {
                    *t = transform(n.origin, n.rot);
                }
            }
        }
    }

}

/// Fraction of the way at time t (ms) with linear velocity ramps of a / d ms at the ends.
fn ramp(t: f32, dur: f32, a: f32, d: f32) -> f32 {
    if dur <= 0.0 {
        return 1.0;
    }
    let cruise = dur - a - d;
    // distance units: v_max * (a/2 + cruise + d/2) = 1
    let v = 1.0 / (a * 0.5 + cruise + d * 0.5).max(1e-6);
    let s = if t < a {
        0.5 * v * t * t / a.max(1e-6)
    } else if t < a + cruise {
        v * (a * 0.5 + (t - a))
    } else {
        let u = (t - a - cruise).min(d);
        v * (a * 0.5 + cruise + u - 0.5 * u * u / d.max(1e-6))
    };
    s.clamp(0.0, 1.0)
}

fn slerp(a: Mat3, b: Mat3, f: f32) -> Mat3 {
    if a == b {
        return a;
    }
    let (qa, qb) = (glam::Quat::from_mat3(&a), glam::Quat::from_mat3(&b));
    Mat3::from_quat(qa.slerp(qb, f))
}

/// Bevy transform of an idTech pose: translation to_bevy(o), rotation C·R·Cᵀ.
pub fn transform(o: V, r: Mat3) -> Transform {
    let from_bevy = |b: Vec3| V::new(-b.z, -b.x, b.y);
    let col = |b: Vec3| to_bevy(r * from_bevy(b));
    let m = bevy::math::Mat3::from_cols(col(Vec3::X), col(Vec3::Y), col(Vec3::Z));
    Transform { translation: to_bevy(o), rotation: Quat::from_mat3(&m), scale: Vec3::ONE }
}

/// Spawns a root entity (world pose, no mesh) for every mover node; render meshes and bound FX are
/// attached to these.
pub fn spawn_roots(mut commands: Commands, mut movers: ResMut<Movers>) {
    for n in &mut movers.nodes {
        n.render = Some(commands.spawn((transform(n.origin, n.rot), Visibility::default(), Name::new(n.spec.entity.clone()))).id());
    }
}

/// Advances movers on the game clock, pushes/carries the player with their collision and moves
/// their render entities. Runs after the player's game frame (`map::MapSet::Movers`).
pub fn tick_movers(mut sim: ResMut<crate::Sim>, mut movers: ResMut<Movers>, mut transforms: Query<&mut Transform>) {
    let mut changed = movers.tick(sim.game_ms as i64);
    if changed.is_empty() {
        return;
    }
    let crate::Sim { player, world, msec_last, .. } = &mut *sim;
    movers.push(&mut changed, player, world, *msec_last as i64);
    movers.sync_render(&changed, &mut transforms);
}
