//! Self-test hooks for automated checks (no effect unless the env vars are set):
//! RANCHER_SHOT=<png path>   save a screenshot after RANCHER_SHOT_AT seconds (default 4) and exit
//! RANCHER_SHOTS=<t1,t2,..>  instead take several screenshots (<path stem>_<i>.png) at these times
//! RANCHER_WEAPON=<index>    select a weapon at start
//! RANCHER_FIRE=1            hold the trigger; RANCHER_FIRE=<a>-<b> hold it from a to b seconds
//! RANCHER_SWITCH=<t>:<index>  select a weapon at time t (bypasses the game's selection logic)
//! RANCHER_PRESS=<t>:<action>[,<t>:<action>...]  press a game action (e.g. _weap3, _weapnext) at time t
//! RANCHER_HOLD=<a>-<b>:<action>[,...]  hold a game action (e.g. _moveforward) from a to b seconds
//! RANCHER_TURN=<a>-<b>:<deg per s>     turn the view (yaw) from a to b seconds
//! RANCHER_MENU=<t>          open the pause / options menu at t seconds
//! RANCHER_RES=<w>x<h>       self-test window resolution (default 1280x720)
//! RANCHER_TRACE=1           log hands anim web state changes and anim events to stdout
//! RANCHER_PITCH / RANCHER_YAW  initial view angles in degrees

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

#[derive(Resource, Default)]
pub struct AutoTest {
    pub shot: Option<String>,
    pub shot_at: f32,
    /// Extra screenshot times (RANCHER_SHOTS); empty = the single RANCHER_SHOT_AT capture.
    pub shots: Vec<f32>,
    pub taken: usize,
    /// Trigger held this frame (combat reads it).
    pub fire: bool,
    pub fire_window: Option<(f32, f32)>,
    pub switch: Option<(f32, usize)>,
    pub presses: Vec<(f32, String)>,
    pub holds: Vec<(f32, f32, String)>,
    pub turn: Option<(f32, f32, f32)>,
    pub trace: bool,
    pub done: bool,
    pub elapsed: f32,
    pub weapon: Option<usize>,
    /// RANCHER_MENU: open the menu at this time.
    pub menu: Option<f32>,
}

impl AutoTest {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok();
        let fire = get("RANCHER_FIRE");
        let fire_window = fire.as_deref().and_then(|f| f.split_once('-')).and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));
        Self {
            shot: get("RANCHER_SHOT"),
            menu: get("RANCHER_MENU").and_then(|v| v.parse().ok()),
            shot_at: get("RANCHER_SHOT_AT").and_then(|v| v.parse().ok()).unwrap_or(4.0),
            shots: get("RANCHER_SHOTS").map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect()).unwrap_or_default(),
            fire: fire.is_some() && fire_window.is_none(),
            fire_window,
            switch: get("RANCHER_SWITCH").and_then(|v| {
                let (t, i) = v.split_once(':')?;
                Some((t.parse().ok()?, i.parse().ok()?))
            }),
            trace: get("RANCHER_TRACE").is_some(),
            holds: get("RANCHER_HOLD")
                .map(|v| {
                    v.split(',')
                        .filter_map(|p| {
                            let (w, a) = p.split_once(':')?;
                            let (t0, t1) = w.split_once('-')?;
                            Some((t0.trim().parse().ok()?, t1.trim().parse().ok()?, a.trim().to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            turn: get("RANCHER_TURN").and_then(|v| {
                let (w, r) = v.split_once(':')?;
                let (t0, t1) = w.split_once('-')?;
                Some((t0.trim().parse().ok()?, t1.trim().parse().ok()?, r.trim().parse().ok()?))
            }),
            presses: get("RANCHER_PRESS")
                .map(|v| v.split(',').filter_map(|p| p.split_once(':')).filter_map(|(t, a)| Some((t.trim().parse().ok()?, a.trim().to_string()))).collect())
                .unwrap_or_default(),
            weapon: get("RANCHER_WEAPON").and_then(|v| v.parse().ok()),
            ..default()
        }
    }

    fn shot_path(&self, i: usize) -> String {
        let p = self.shot.clone().unwrap_or_default();
        if self.shots.is_empty() {
            return p;
        }
        match p.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}_{i}.{ext}"),
            None => format!("{p}_{i}"),
        }
    }
}

pub fn autotest(
    mut commands: Commands,
    mut at: ResMut<AutoTest>,
    time: Res<Time>,
    mut combat: ResMut<crate::combat::Combat>,
    mut actions: ResMut<crate::input::Actions>,
    mut sim: ResMut<crate::Sim>,
    mut exit: MessageWriter<AppExit>,
    mut menu: ResMut<crate::settings::MenuOpen>,
) {
    if at.shot.is_none() && !at.trace {
        return;
    }
    at.elapsed += time.delta_secs();
    let t = at.elapsed;
    if at.menu.is_some_and(|m| t >= m) {
        at.menu = None;
        menu.0 = true;
    }
    if let Some((a, b)) = at.fire_window {
        at.fire = t >= a && t < b;
    }
    let due: Vec<String> = at.presses.iter().filter(|(when, _)| t >= *when).map(|(_, a)| a.clone()).collect();
    at.presses.retain(|(when, _)| t < *when);
    for a in due {
        actions.queue_press(&a);
    }
    for (t0, t1, a) in &at.holds {
        if t >= *t0 && t < *t1 {
            actions.queue_hold(a);
        }
    }
    if let Some((t0, t1, rate)) = at.turn {
        if t >= t0 && t < t1 {
            let yaw = sim.player.view_angles[1] + rate * time.delta_secs();
            sim.player.view_angles[1] = yaw.rem_euclid(360.0);
        }
    }
    if let Some((when, idx)) = at.switch {
        if t >= when {
            if idx < combat.arsenal.defs.len() {
                combat.arsenal.select(idx);
            }
            at.switch = None;
        }
    }
    if at.shot.is_none() {
        return;
    }
    let times = if at.shots.is_empty() { vec![at.shot_at] } else { at.shots.clone() };
    if at.taken < times.len() && t >= times[at.taken] {
        let path = at.shot_path(at.taken);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        at.taken += 1;
        at.done = at.taken == times.len();
    }
    if at.done && t >= times.last().copied().unwrap_or(0.0) + 1.5 {
        exit.write(AppExit::Success);
    }
}
