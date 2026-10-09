//! Weapon firing checks against the user's own install. Skipped when no DOOM install is found.
//! Expected values come from the install's decls plus the decoded routines (gamedata/re/WEAPONS.md).

use std::sync::Arc;

use rancher_sim::install;
use rancher_sim::weapons::{load_arsenal, Arsenal, WeaponDef, WeaponEvent, WeaponInput, WeaponPhase};
use rancher_sim::Vec3;

fn defs() -> Option<Vec<Arc<WeaponDef>>> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).expect("loading install");
    Some(load_arsenal(&inst.decls))
}

fn idx(defs: &[Arc<WeaponDef>], suffix: &str) -> usize {
    defs.iter().position(|d| d.decl.ends_with(suffix)).unwrap_or_else(|| panic!("no {suffix}"))
}

/// An arsenal with `weapon` already raised and one idle frame run (idPlayer::UpdateSpread's base spread
/// reaches GetSpread one frame late, so a cold start would fire the first shot with no base spread).
fn armed(defs: &[Arc<WeaponDef>], suffix: &str) -> Arsenal {
    let mut a = Arsenal::new(defs.to_vec());
    a.current = idx(defs, suffix);
    a.phase = WeaponPhase::Ready;
    a.tick(1, &WeaponInput::default());
    a
}

fn hold(trigger: bool) -> WeaponInput {
    WeaponInput { trigger, ..Default::default() }
}

/// Runs `frames` frames of `ms`, returning the game times of shots.
fn run(a: &mut Arsenal, frames: usize, ms: i32, mut trigger: impl FnMut(i32) -> bool) -> Vec<i32> {
    let mut shots = Vec::new();
    for _ in 0..frames {
        let t = a.time_ms + ms;
        for e in a.tick(ms, &hold(trigger(t))) {
            if let WeaponEvent::Fired(s) = e {
                shots.push(s.time_ms);
            }
        }
    }
    shots
}

#[test]
fn arsenal_loads_decoded_fields() {
    let Some(defs) = defs() else { return };
    assert_eq!(defs.len(), 11, "all SP weapons load");
    for d in &defs {
        println!(
            "{:44} int {:4} tap {} q {} inf {} ammo {:?}/{} x{} spread {:.2}+{:.2}/{:.2} ret {}+{} newkick {} heat {:?} cg {:?}",
            d.decl, d.firing_interval, d.single_tap, d.allow_shot_queueing, d.infinite_ammo, d.ammo_pool, d.ammo_max, d.ammo_per_shot,
            d.spread_params.spread, d.spread_params.addition_per_shot, d.spread_params.addition_max, d.spread_params.return_delay,
            d.spread_params.return_time, d.feedback.use_new_kick_system, d.heat.heat_increment, d.chaingun.map(|c| (c.spin_up_ms, c.firing_interval_min))
        );
    }
    let ssg = &defs[idx(&defs, "/double_barrel")];
    let sg = &defs[idx(&defs, "/shotgun")];
    assert_eq!(ssg.ammo_per_shot, 2, "super shotgun clip ammoPerShot");
    assert_eq!(ssg.ammo_pool, sg.ammo_pool, "both shotguns share the shells pool");
    assert!(ssg.single_tap && !sg.single_tap);
    let cg = defs[idx(&defs, "/chaingun")].chaingun.expect("chaingun weaponData");
    assert!(cg.progressive_firing_interval && cg.firing_interval_min > defs[idx(&defs, "/chaingun")].firing_interval);
    let bfg = &defs[idx(&defs, "/bfg")];
    assert!(!bfg.feedback.use_new_kick_system, "the BFG uses the old kick system");
    let pistol = &defs[idx(&defs, "/pistol")];
    assert!(pistol.infinite_ammo && pistol.single_tap && pistol.allow_shot_queueing);
    // Class defaults show up where the decl is silent (spreadParams ctor values).
    let rl = &defs[idx(&defs, "/rocket_launcher")];
    assert_eq!(rl.spread_params.horizontal_scale, 1.0);
    assert!(rl.projectile.splash.is_some() && !rl.projectile.hitscan);
}

#[test]
fn full_auto_refires_on_the_first_frame_past_the_interval() {
    let Some(defs) = defs() else { return };
    for (suffix, ms) in [("/heavy_rifle_heavy_ar", 1), ("/heavy_rifle_heavy_ar", 16), ("/plasma_rifle", 7), ("/shotgun", 16)] {
        let mut a = armed(&defs, suffix);
        // FinishFire: nextFireTime += (int)(960 * ms * 0.001f) game ticks (HAR 144 -> 138, shotgun 850 -> 816).
        let interval = rancher_sim::weapons::arsenal::ms_to_ticks(a.def().firing_interval);
        let shots = run(&mut a, 3000 / ms as usize, ms, |_| true);
        assert!(shots.len() >= 3, "{suffix}: {shots:?}");
        let quantised = (interval + ms - 1) / ms * ms;
        for w in shots.windows(2) {
            assert_eq!(w[1] - w[0], quantised, "{suffix} at {ms} ms frames: {shots:?}");
        }
    }
}

#[test]
fn single_tap_needs_release_and_pistol_queues() {
    let Some(defs) = defs() else { return };
    // Super shotgun: holding fires once; a fresh pull after the interval fires again.
    let mut a = armed(&defs, "/double_barrel");
    let interval = rancher_sim::weapons::arsenal::ms_to_ticks(a.def().firing_interval);
    let held = run(&mut a, 3000, 1, |_| true);
    assert_eq!(held.len(), 1, "held trigger: {held:?}");
    let shots = run(&mut a, 3000, 1, |t| (t / 100) % 2 == 0);
    assert!(shots.len() >= 1);
    for w in shots.windows(2) {
        assert!(w[1] - w[0] >= interval);
    }
    // Pistol: a tap inside the cooldown is queued and fires as soon as the weapon may fire.
    let mut p = armed(&defs, "/pistol");
    let interval = rancher_sim::weapons::arsenal::ms_to_ticks(p.def().firing_interval);
    let first = run(&mut p, 1, 1, |_| true);
    assert_eq!(first, vec![p.time_ms]);
    let tap_at = first[0] + interval / 2;
    let shots = run(&mut p, interval as usize * 2, 1, |t| t == tap_at);
    assert_eq!(shots, vec![first[0] + interval], "queued pistol shot");
}

#[test]
fn ammo_use_and_dry_fire() {
    let Some(defs) = defs() else { return };
    let mut a = armed(&defs, "/double_barrel");
    let per = a.def().ammo_per_shot;
    let start = a.ammo[a.current];
    let mut fired = 0;
    let mut dry = 0;
    for i in 0..20000 {
        let trigger = (i / 50) % 2 == 0;
        for e in a.tick(1, &hold(trigger)) {
            match e {
                WeaponEvent::Fired(s) => {
                    fired += 1;
                    assert_eq!(s.ammo_used, per);
                }
                WeaponEvent::DryFire { .. } => dry += 1,
                _ => {}
            }
        }
    }
    assert_eq!(fired, start / per, "shots until the shells run out");
    assert_eq!(a.ammo[a.current], start % per);
    assert!(dry > 0, "dry fire once per pull when empty");
}

#[test]
fn chaingun_spins_up_its_firing_interval() {
    let Some(defs) = defs() else { return };
    let mut a = armed(&defs, "/chaingun");
    let d = a.def().clone();
    let cg = d.chaingun.unwrap();
    let shots = run(&mut a, 4000, 1, |_| true);
    let gaps: Vec<i32> = shots.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps[0] > (cg.firing_interval_min + d.firing_interval) / 2, "first gap near firingIntervalMin: {gaps:?}");
    assert!(gaps.windows(2).all(|w| w[1] <= w[0]), "monotonic spin-up: {gaps:?}");
    assert_eq!(*gaps.last().unwrap(), rancher_sim::weapons::arsenal::ms_to_ticks(d.firing_interval), "fully spun: {gaps:?}");
    // Spin reaches 1 after spinUpTimeMS of holding (1/ms steps).
    let mut b = armed(&defs, "/chaingun");
    run(&mut b, cg.spin_up_ms as usize, 1, |_| true);
    assert!((b.states[b.current].barrel.spin - 1.0).abs() < 1e-3, "spin {}", b.states[b.current].barrel.spin);
    // Releasing decays the spin, and the barrel stops (spin 0) once aligned.
    run(&mut b, 5000, 1, |_| false);
    assert_eq!(b.states[b.current].barrel.spin, 0.0);
}

#[test]
fn shotgun_kick_recoils_then_recovers() {
    let Some(defs) = defs() else { return };
    let mut a = armed(&defs, "/shotgun");
    let k = a.def().feedback;
    assert!(k.use_new_kick_system);
    run(&mut a, 1, 1, |_| true);
    let t0 = a.time_ms;
    let p = k.kick_pitch;
    run(&mut a, p.recoil_ms as usize, 1, |_| false);
    let peak = a.view_kick.pitch;
    assert!(peak <= -p.kick * 0.5 && peak >= -p.max_kick, "KICK_ONLY_POSITIVE pitches up (negative): {peak}");
    assert!(a.kick[0].value > 0.0, "compat value is pitch-up positive");
    run(&mut a, p.recovery_delay_ms as usize, 1, |_| false);
    assert!((a.view_kick.pitch - peak).abs() < 1e-4, "holds through recoveryDelayMS");
    let half = p.recovery_ms / 2;
    run(&mut a, half as usize, 1, |_| false);
    let expect = peak * (1.0 - (std::f32::consts::FRAC_PI_2 * half as f32 / p.recovery_ms as f32).sin());
    assert!((a.view_kick.pitch - expect).abs() < 1e-3, "sine recovery {} vs {expect}", a.view_kick.pitch);
    let end = t0 + p.recoil_ms + p.recovery_delay_ms + p.recovery_ms + 1;
    let left = (end - a.time_ms) as usize;
    run(&mut a, left, 1, |_| false);
    assert_eq!(a.view_kick.pitch, 0.0);
    assert!(a.view_kick.fov == 0.0);
}

#[test]
fn bfg_old_kick_moves_up_and_returns() {
    let Some(defs) = defs() else { return };
    let mut a = armed(&defs, "/bfg");
    let fb = a.def().feedback;
    run(&mut a, 1, 1, |_| true);
    let target = a.kick_old.pitch_target;
    assert!(target <= -fb.pitch_kick_amount + 1e-4 && target >= -fb.pitch_kick_amount - fb.pitch_kick_amount_delta - 1e-4);
    run(&mut a, 200, 1, |_| false);
    assert!(a.view_kick.pitch < 0.0);
    run(&mut a, 5000, 1, |_| false);
    assert_eq!(a.view_kick.pitch, 0.0, "returns to rest");
}

#[test]
fn spread_grows_per_shot_and_returns() {
    let Some(defs) = defs() else { return };
    for suffix in ["/heavy_rifle_heavy_ar", "/pistol", "/rocket_launcher"] {
        let mut a = armed(&defs, suffix);
        let sp = a.def().spread_params;
        let mut spreads = Vec::new();
        for t in 0..3000 {
            for e in a.tick(1, &hold(t % 400 < 300)) {
                if let WeaponEvent::Fired(s) = e {
                    spreads.push(s.spread_deg);
                }
            }
        }
        assert!((spreads[0] - sp.spread).abs() < 1e-5, "{suffix}: first shot sees the base {spreads:?}");
        assert!(spreads[1] > spreads[0], "{suffix}: {spreads:?}");
        assert!(spreads.iter().all(|&s| s >= sp.spread - 1e-5 && s <= sp.spread + sp.addition_max + 1e-4), "{suffix}: {spreads:?}");
        // Release: the addition holds for spreadReturnDelay, then returns linearly over spreadReturnTime.
        run(&mut a, (sp.return_delay + sp.return_time) as usize + 100, 1, |_| false);
        assert!((a.spread.value(a.time_ms) - sp.spread).abs() < 1e-5, "{suffix}");
    }
    // One shot's addition blends in over 67 ms, holds spreadReturnDelay, then returns over spreadReturnTime.
    let mut a = armed(&defs, "/rocket_launcher");
    let sp = a.def().spread_params;
    run(&mut a, 1, 1, |_| true);
    let t0 = a.time_ms;
    let add = |a: &mut Arsenal| a.spread.add.value(a.time_ms);
    run(&mut a, 67, 1, |_| false);
    assert!((add(&mut a) - sp.addition_per_shot.min(sp.addition_max)).abs() < 1e-5);
    run(&mut a, sp.return_delay as usize, 1, |_| false);
    assert!((add(&mut a) - sp.addition_per_shot).abs() < 1e-5, "held through the return delay");
    let half = (sp.return_time / 2.0) as i32;
    run(&mut a, half as usize, 1, |_| false);
    let expect = sp.addition_per_shot * (1.0 - half as f32 / sp.return_time);
    assert!((add(&mut a) - expect).abs() < 1e-4, "{} vs {expect} at {}", add(&mut a), a.time_ms - t0);
}

#[test]
fn shot_directions_follow_the_patterns() {
    let Some(defs) = defs() else { return };
    // Combat shotgun: Quake 4 rings, spawnCount traces on two rings scaled by h/v spread scale.
    let mut a = armed(&defs, "/shotgun");
    let d = a.def().clone();
    let mut dirs = Vec::new();
    for e in a.tick(1, &hold(true)) {
        if let WeaponEvent::Fired(s) = e {
            dirs = s.dirs;
        }
    }
    assert_eq!(dirs.len(), d.projectile.spawn_count as usize);
    let s = d.spread_params.spread.to_radians();
    let lat = |v: Vec3| (v.y / v.x).abs();
    let up = |v: Vec3| (v.z / v.x).abs();
    // Trace 0: ring 0 (half spread) at 0 degrees -> purely lateral, scaled by horizontalSpreadScale.
    assert!((lat(dirs[0]) - (s * 0.5).sin() * d.spread_params.horizontal_scale).abs() < 1e-4);
    assert!(up(dirs[0]) < 1e-5);
    // Trace 1: ring 0 at 90 degrees -> vertical, scaled by verticalSpreadScale.
    assert!((up(dirs[1]) - (s * 0.5).sin() * d.spread_params.vertical_scale).abs() < 1e-4);
    // Single-trace weapons: gaussian (mean of 3 uniforms) inside the sin(spread) box.
    let mut h = armed(&defs, "/heavy_rifle_heavy_ar");
    let base = h.def().spread_params.spread.to_radians();
    for _ in 0..200 {
        for e in h.tick(1, &hold(true)) {
            if let WeaponEvent::Fired(s) = e {
                let lim = s.spread_deg.to_radians().sin() + 1e-5;
                assert!(lat(s.dirs[0]) <= lim && up(s.dirs[0]) <= lim);
                assert!(s.spread_deg.to_radians() >= base - 1e-6);
            }
        }
    }
}

#[test]
fn weapon_switch_takes_bringdown_plus_bringup() {
    let Some(defs) = defs() else { return };
    let mut a = armed(&defs, "/shotgun");
    let from = a.def().clone();
    let to_i = idx(&defs, "/double_barrel");
    let to = defs[to_i].clone();
    let t_sel = a.time_ms;
    a.select(to_i);
    a.select(idx(&defs, "/chaingun")); // ignored while the change is in progress
    let mut first = None;
    for _ in 0..3000 {
        for e in a.tick(1, &hold(true)) {
            if let WeaponEvent::Fired(s) = e {
                first.get_or_insert(s.time_ms);
            }
        }
    }
    assert_eq!(a.current, to_i);
    let ready_at = t_sel + ((from.bringdown_s + to.bringup_s) * 960.0).round() as i32;
    let f = first.expect("fires after the switch");
    assert!((f - ready_at).abs() <= 2, "first shot at {f}, expected about {ready_at}");
}

#[test]
fn gauss_overheats_after_a_shot() {
    let Some(defs) = defs() else { return };
    let mut a = armed(&defs, "/gauss_rifle");
    let d = a.def().clone();
    run(&mut a, 1, 1, |_| true);
    let mut over = false;
    for e in a.tick(1, &hold(false)) {
        over |= matches!(e, WeaponEvent::Overheated { .. });
    }
    assert!(over, "heatIncrement 1 overheats on the next frame");
    assert_eq!(a.states[a.current].overheat, d.heat.overheat_delay_ms);
    assert!(!a.can_fire(a.current, a.time_ms));
}

#[test]
fn rocket_splash_falls_off_on_squared_distance() {
    let Some(defs) = defs() else { return };
    let rl = &defs[idx(&defs, "/rocket_launcher")];
    let s = rl.projectile.splash.as_ref().unwrap();
    let r = s.splash_radius();
    let full = s.at_distance(0.0);
    assert_eq!(s.splash_at(0.0), full);
    let outer = s.radius_outer_strength;
    assert!((s.splash_at(r) - full * outer).abs() < 1e-3);
    let half = 1.0 - 0.25 * (1.0 - outer);
    assert!((s.splash_at(r * 0.5) - full * half).abs() < 1e-3);
    assert_eq!(s.splash_at(r + 1.0), 0.0);
    // Distance is measured to the target's box.
    let hit = s.splash_on_box(Vec3::ZERO, Vec3::new(r * 0.5, -10.0, -10.0), Vec3::new(r * 0.5 + 32.0, 10.0, 10.0)).unwrap();
    assert!((hit - full * half).abs() < 1e-3);
}

// ---- idHands driver (weapons::hands) against a scripted anim web ----

mod hands_driver {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use rancher_sim::weapons::{Arsenal, Hands, HandsInput, HandsWeb, WeaponDef, WeaponEvent, WeaponInput, WebEvent, WebRequest};

    use super::{defs, idx};

    /// A scripted stand-in for the anim-web runtime (game time in 960 Hz ticks): no blends; a request takes its first edge at the
    /// frame time it is made; a state with a known length leaves at its end (looping states wrap);
    /// frame events fire when their (rate-scaled) offset is reached; node notifications 1, 0, 3, 2 are
    /// reported on every edge. Changing subWeb plays the old subWeb's bringdown first.
    #[derive(Default)]
    pub struct MockWeb {
        pub sub: String,
        pub state: String,
        entered: f32,
        fired: usize,
        path: Vec<(String, String)>,
        jump: bool,
        /// (sub, state) -> length in 30 Hz frames at rate 1.
        pub frames: HashMap<(String, String), i32>,
        /// (model, alias) -> (numFrames, frameRate).
        pub alias: HashMap<(String, String), (i32, i32)>,
        /// state -> scalar that scales its playback rate.
        pub rate_of: HashMap<String, &'static str>,
        /// state -> (30 Hz frame, event name, int).
        pub events: HashMap<String, Vec<(f32, String, Option<i32>)>>,
        pub looping: HashSet<String>,
        pub scalars: HashMap<String, f32>,
        pub visited: Vec<(i32, String, String)>,
    }

    impl MockWeb {
        pub fn new(sub: &str) -> Self {
            Self { sub: sub.into(), state: "idle".into(), ..Default::default() }
        }
        pub fn state_frames(&mut self, sub: &str, state: &str, n: i32) {
            self.frames.insert((sub.into(), state.into()), n);
        }
        pub fn event(&mut self, state: &str, frame: f32, name: &str, int: Option<i32>) {
            self.events.entry(state.into()).or_default().push((frame, name.into(), int));
        }
        fn rate(&self, state: &str) -> f32 {
            self.rate_of.get(state).and_then(|s| self.scalars.get(*s)).copied().unwrap_or(1.0)
        }
        fn request(&mut self, sub: &str, steps: &[&str]) {
            self.path.clear();
            if sub != self.sub {
                self.path.push((self.sub.clone(), "bringdown".into()));
            }
            for s in steps {
                self.path.push((sub.into(), (*s).into()));
            }
            self.jump = true;
        }
        fn edge(&mut self, at: f32, out: &mut Vec<WebEvent>) {
            let (sub, state) = self.path.remove(0);
            let old = (std::mem::replace(&mut self.sub, sub), std::mem::replace(&mut self.state, state));
            out.push(WebEvent::Node { kind: 1, sub: old.0.clone(), state: old.1.clone() });
            out.push(WebEvent::Node { kind: 0, sub: self.sub.clone(), state: self.state.clone() });
            out.push(WebEvent::Node { kind: 3, sub: old.0, state: old.1 });
            out.push(WebEvent::Node { kind: 2, sub: self.sub.clone(), state: self.state.clone() });
            self.entered = at;
            self.fired = 0;
            self.visited.push((at as i32, self.sub.clone(), self.state.clone()));
        }
    }

    impl HandsWeb for MockWeb {
        fn current(&self) -> Option<(&str, &str)> {
            Some((&self.sub, &self.state))
        }
        fn is_blending(&self) -> bool {
            false
        }
        fn set_scalar(&mut self, name: &str, value: f32) {
            self.scalars.insert(name.into(), value);
        }
        fn change_state(&mut self, sub: &str, to: &str) {
            self.request(sub, &[to]);
        }
        fn change_state_via(&mut self, sub: &str, to: &str, via: &str) {
            self.request(sub, &[via, to]);
        }
        fn force_state(&mut self, sub: &str, to: &str, _blend_frames: i32) {
            self.request(sub, &[to]);
        }
        fn force_state_via(&mut self, sub: &str, to: &str, via: &str, _blend_frames: i32) {
            self.request(sub, &[to, via]);
        }
        fn alias_anim(&self, model: &str, alias: &str) -> Option<(i32, i32)> {
            self.alias.get(&(model.to_string(), alias.to_string())).copied()
        }
        fn state_anim_frames(&self, sub: &str, state: &str) -> Option<i32> {
            self.frames.get(&(sub.to_string(), state.to_string())).copied()
        }
        fn update(&mut self, now: i32) -> Vec<WebEvent> {
            let now = now as f32;
            let mut out = Vec::new();
            loop {
                if self.jump && !self.path.is_empty() {
                    self.jump = false;
                    self.edge(now, &mut out);
                    continue;
                }
                let rate = self.rate(&self.state);
                let t = now - self.entered;
                if let Some(evs) = self.events.get(&self.state) {
                    while self.fired < evs.len() && evs[self.fired].0 / 30.0 * 960.0 / rate <= t {
                        let (_, n, i) = &evs[self.fired];
                        out.push(WebEvent::Anim { name: n.clone(), int: *i });
                        self.fired += 1;
                    }
                }
                if let Some(&fr) = self.frames.get(&(self.sub.clone(), self.state.clone())) {
                    let len = fr as f32 / 30.0 * 960.0 / rate;
                    if t >= len {
                        if self.looping.contains(&self.state) {
                            self.entered += len;
                            self.fired = 0;
                            continue;
                        }
                        if !self.path.is_empty() {
                            let at = self.entered + len;
                            self.edge(at, &mut out);
                            continue;
                        }
                    }
                }
                break;
            }
            out
        }
    }

    pub fn rig(defs: &[Arc<WeaponDef>], suffix: &str) -> (Arsenal, Hands, MockWeb) {
        let mut a = Arsenal::new(defs.to_vec());
        a.current = idx(defs, suffix);
        let mut web = MockWeb::new(&a.def().hands.subweb);
        for (s, r) in [("bringdown", "bringDownAnimRate"), ("bringup", "bringUpAnimRate"), ("shootstate", "shootAnimRate"), ("shoot_delay", "shootDelayScale")] {
            web.rate_of.insert(s.into(), r);
        }
        let h = Hands::new(defs.len());
        (a, h, web)
    }

    pub fn input(trigger: bool) -> HandsInput {
        HandsInput { weapon: WeaponInput { trigger, ..Default::default() }, ..Default::default() }
    }

    /// Runs frames of `ms` until `end`; returns shot times.
    pub fn run(a: &mut Arsenal, h: &mut Hands, web: &mut MockWeb, ms: i32, end: i32, trigger: impl Fn(i32) -> bool) -> Vec<i32> {
        let mut shots = Vec::new();
        while a.time_ms + ms <= end {
            let t = a.time_ms + ms;
            for e in h.tick(a, web, ms, &input(trigger(t))) {
                if let WeaponEvent::Fired(s) = e {
                    shots.push(s.time_ms);
                }
            }
        }
        shots
    }

    #[test]
    fn hands_fire_on_the_shoot_event_and_refire_when_interruptible() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/heavy_rifle_heavy_ar");
        let sub = a.def().hands.subweb.clone();
        let interval = a.def().firing_interval;
        assert!(a.def().hands.has_shoot_again_state);
        for st in ["shoot", "shoot_again"] {
            web.state_frames(&sub, st, 9);
            web.event(st, 0.0, "ae_fireWeaponRight", None);
            web.event(st, 2.0, "ae_handsWeaponFireFinished", None);
            web.event(st, 2.0, "ae_setInterruptible", Some(1));
        }
        h.start(&mut a, &mut web);
        run(&mut a, &mut h, &mut web, 16, 16, |_| false);
        assert_eq!(web.state, "idle");
        let shots = run(&mut a, &mut h, &mut web, 16, 16 + 16 * 40, |_| true);
        assert!(shots.len() >= 4, "{shots:?}");
        // Shot 0 fires on the frame of the FIRE request (the shoot state's frame-0 event); afterwards the
        // FIRE is requested once CanFire passes and taken as soon as ae_setInterruptible has run: the
        // HAR's interval is the longer wait here, so gaps are the interval rounded up to frames.
        assert_eq!(shots[0], 32);
        let gap = (interval + 15) / 16 * 16;
        for w in shots.windows(2) {
            assert_eq!(w[1] - w[0], gap, "{shots:?}");
        }
        // shoot and shoot_again alternate.
        let shoots: Vec<&str> = web.visited.iter().filter(|v| v.2.starts_with("shoot")).map(|v| v.2.as_str()).collect();
        assert_eq!(&shoots[..4], &["shoot", "shoot_again", "shoot", "shoot_again"]);
    }

    #[test]
    fn hands_wait_for_the_anim_when_it_is_slower_than_the_interval() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/heavy_rifle_heavy_ar");
        let sub = a.def().hands.subweb.clone();
        // An anim that only becomes interruptible at frame 7 is the slower gate.
        for st in ["shoot", "shoot_again"] {
            web.state_frames(&sub, st, 12);
            web.event(st, 0.0, "ae_fireWeaponRight", None);
            web.event(st, 7.0, "ae_setInterruptible", Some(1));
        }
        h.start(&mut a, &mut web);
        let shots = run(&mut a, &mut h, &mut web, 16, 16 * 50, |_| true);
        assert!(shots.len() >= 3, "{shots:?}");
        // Frame 7 at 30 Hz is 224 game ticks (960/s) after the shot: the event fires on that frame (+224) and
        // the FIRE is taken by the next frame's idHands::Update (+240), well past the 144-tick interval.
        let ev = ((7.0f32 / 30.0 * 960.0) / 16.0).ceil() as i32 * 16;
        for w in shots.windows(2) {
            assert_eq!(w[1] - w[0], ev + 16, "{shots:?}");
        }
    }

    #[test]
    fn hands_looping_shoot_fires_on_loop_events_and_ceases_on_release() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/plasma_rifle");
        let d = a.def().clone();
        let sub = d.hands.subweb.clone();
        assert!(d.hands.has_looping_shoot_state && d.hands.shots_per_looping_shoot_anim == 6);
        web.alias.insert((d.hands_md6.clone(), "shoot".into()), (49, 30));
        web.state_frames(&sub, "shootstate", 48);
        web.looping.insert("shootstate".into());
        for f in [0.0, 8.0, 16.0, 24.0, 32.0, 40.0] {
            web.event("shootstate", f, "ae_fireWeaponRight", None);
        }
        web.state_frames(&sub, "shootstate_recovery", 6);
        h.start(&mut a, &mut web);
        // shootAnimRate 0x140d62670: ((49-1)/30 s * 960 ticks/s) / (6 shots * firingInterval ticks).
        let want = (48.0f32 / 30.0 * 960.0) / (6.0 * d.firing_interval as f32);
        assert!((h.scalars.shoot_anim_rate - want).abs() < 1e-5, "{} vs {want}", h.scalars.shoot_anim_rate);
        let start = a.time_ms;
        let release = start + 7 * 80;
        let shots = run(&mut a, &mut h, &mut web, 7, start + 7 * 100, |t| t <= release);
        // Shots land on the first 7 ms frame at or after each loop event (entry + k * interval), not on
        // "previous shot + interval" (which would drift with the frame rounding).
        let entry = start + 7;
        let mut want_shots = Vec::new();
        let mut k = 0;
        loop {
            let ev = entry as f32 + k as f32 * 8.0 / 30.0 * 960.0 / h.scalars.shoot_anim_rate;
            let t = ((ev - entry as f32 - 1e-3) / 7.0).ceil() as i32 * 7 + entry;
            if t > release {
                break;
            }
            want_shots.push(t);
            k += 1;
        }
        assert_eq!(&shots[..], &want_shots[..], "no shots after the release either");
        assert!(web.visited.iter().any(|v| v.2 == "shootstate_recovery"), "cease fire goes idle via shootstate_recovery");
    }

    #[test]
    fn hands_stop_fire_sound_once_on_release() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/plasma_rifle");
        let sub = a.def().hands.subweb.clone();
        let md6 = a.def().hands_md6.clone();
        web.alias.insert((md6, "shoot".into()), (49, 30));
        web.state_frames(&sub, "shootstate", 48);
        web.looping.insert("shootstate".into());
        for f in [0.0, 8.0, 16.0, 24.0, 32.0, 40.0] {
            web.event("shootstate", f, "ae_fireWeaponRight", None);
        }
        web.state_frames(&sub, "shootstate_recovery", 6);
        h.start(&mut a, &mut web);
        let release = a.time_ms + 7 * 80;
        let (mut fired, mut stops) = (0, Vec::new());
        while a.time_ms + 7 <= release + 7 * 40 {
            let t = a.time_ms + 7;
            for e in h.tick(&mut a, &mut web, 7, &input(t <= release)) {
                match e {
                    WeaponEvent::Fired(_) => fired += 1,
                    WeaponEvent::StopFireSound { .. } => stops.push(t),
                    _ => {}
                }
            }
        }
        assert!(fired > 1);
        // ReleaseTrigger 0x140f1f920 (PULLED -> RELEASED only) -> StopFireSound, on the release frame.
        assert_eq!(stops, vec![release + 7]);
    }

    #[test]
    fn hands_dry_fire_needs_a_new_pull() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/shotgun");
        let sub = a.def().hands.subweb.clone();
        web.state_frames(&sub, "dryfire", 6);
        web.event("dryfire", 0.0, "ae_fireWeaponRight", None);
        let p = a.pools.iter().position(|p| p.key == a.def().ammo_pool).unwrap();
        a.pools[p].count = 0;
        h.start(&mut a, &mut web);
        let mut dry = 0;
        let mut fired = 0;
        for i in 0..40 {
            let trigger = !(20..24).contains(&i);
            for e in h.tick(&mut a, &mut web, 16, &input(trigger)) {
                match e {
                    WeaponEvent::DryFire { .. } => dry += 1,
                    WeaponEvent::Fired(_) => fired += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(fired, 0);
        assert_eq!(dry, 2, "one click per pull");
        assert_eq!(web.visited.iter().filter(|v| v.2 == "dryfire").count(), 2);
    }

    #[test]
    fn hands_switch_weapon_equips_on_the_bringup_event() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/shotgun");
        let sg = a.current;
        let rl = idx(&defs, "/rocket_launcher");
        let (sg_sub, rl_sub) = (defs[sg].hands.subweb.clone(), defs[rl].hands.subweb.clone());
        web.state_frames(&sg_sub, "bringdown", 12);
        web.state_frames(&rl_sub, "bringup", 18);
        web.event("bringup", 0.0, "ae_equipNextWeaponRight", None);
        web.event("bringup", 8.0, "ae_setInterruptible", Some(1));
        web.state_frames(&rl_sub, "shoot", 20);
        web.event("shoot", 0.0, "ae_fireWeaponRight", None);
        h.start(&mut a, &mut web);
        run(&mut a, &mut h, &mut web, 16, 16, |_| false);
        assert!(h.select(&a, &web, rl));
        let t0 = a.time_ms + 16;
        let mut first_rl_shot = None;
        let mut equip_at = None;
        while a.time_ms < t0 + 1500 {
            for e in h.tick(&mut a, &mut web, 16, &input(true)) {
                if let WeaponEvent::Fired(s) = e {
                    first_rl_shot.get_or_insert((s.weapon, s.time_ms));
                }
            }
            if a.current == rl && equip_at.is_none() {
                equip_at = Some(a.time_ms);
            }
            if a.time_ms == t0 {
                assert!(matches!(h.requests.first(), Some(WebRequest::ChangeStateVia { to: "idle", via: "bringup", .. })), "{:?}", h.requests);
            }
        }
        // 0x140d69550: rates make the clips last desiredBring{down,up}DurationSecs.
        assert!((h.scalars.bring_down_anim_rate - (12.0 / 30.0) / defs[sg].bringdown_s).abs() < 1e-5);
        assert!((h.scalars.bring_up_anim_rate - (18.0 / 30.0) / defs[rl].bringup_s).abs() < 1e-5);
        // The shotgun stays current through its bringdown (the decl duration); the rocket launcher is
        // equipped by ae_equipNextWeaponRight on the first frame of its bringup.
        let down_end = t0 as f32 + defs[sg].bringdown_s * 960.0;
        let equip_at = equip_at.expect("equipped");
        assert_eq!(equip_at, ((down_end - 1e-3) / 16.0).ceil() as i32 * 16);
        // Holding fire: the FIRE goes through once the bringup is interruptible (frame 8) and is taken
        // by the next frame's update.
        let (w, t) = first_rl_shot.expect("rocket fired");
        assert_eq!(w, rl);
        let inter = down_end + 8.0 / 30.0 * 960.0 / h.scalars.bring_up_anim_rate;
        assert_eq!(t, ((inter - 1e-3) / 16.0).ceil() as i32 * 16 + 16);
    }

    #[test]
    fn hands_rate_scalars() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/shotgun");
        let d = a.def().clone();
        let sub = d.hands.subweb.clone();
        web.state_frames(&sub, "shoot", 12);
        web.state_frames(&sub, "shoot_delay", 30);
        web.alias.insert((d.hands_md6.clone(), "shoot".into()), (14, 30));
        h.start(&mut a, &mut web);
        // 0x140d66230: shoot_delay plays in what is left of the interval after the shoot clip (weapon
        // model alias frames win over the hands state's).
        let rem = d.firing_interval as f32 / 960.0 - 14.0 / 30.0;
        assert!((h.scalars.shoot_delay_scale - (30.0 / 30.0) / rem).abs() < 1e-4, "{}", h.scalars.shoot_delay_scale);
        assert_eq!(h.scalars.shoot_anim_rate, 1.0, "non-looping shoot anims play at rate 1");
        assert_eq!(web.scalars["shootDelayScale"], h.scalars.shoot_delay_scale, "scalars reach the web");
        // weaponLoadedBlend springs to 1 when the weapon runs dry (k 500, critical damping).
        let p = a.pools.iter().position(|p| p.key == d.ammo_pool).unwrap();
        a.pools[p].count = 0;
        let t = a.time_ms;
        run(&mut a, &mut h, &mut web, 16, t + 16, |_| false);
        assert_eq!(h.scalars.weapon_loaded_select, 1.0);
        let b1 = h.scalars.weapon_loaded_blend;
        let t = a.time_ms;
        run(&mut a, &mut h, &mut web, 16, t + 16 * 30, |_| false);
        assert!(0.0 < b1 && b1 < 0.2 && (h.scalars.weapon_loaded_blend - 1.0).abs() < 1e-3, "{b1} {}", h.scalars.weapon_loaded_blend);
    }
}

// ---- idPlayer weapon selection (weapons::select) ----

mod selection {
    use rancher_sim::weapons::{Arsenal, SelectInput, WeaponSelect};

    use super::{defs, idx};

    /// Runs one 16 ms frame with `inp`; an issued switch is applied at once. Returns the issued weapon.
    fn frame(a: &mut Arsenal, s: &mut WeaponSelect, inp: SelectInput) -> Option<usize> {
        a.time_ms += 16;
        let w = s.update(a, a.time_ms, &inp);
        if let Some(w) = w {
            a.current = w;
        }
        w
    }

    fn group(g: usize) -> SelectInput {
        let mut i = SelectInput::default();
        i.weap[g] = true;
        i
    }

    #[test]
    fn group_keys_cycle_within_the_group_in_inventory_order() {
        let Some(defs) = defs() else { return };
        let mut a = Arsenal::new(defs.to_vec());
        let (pistol, chainsaw, plasma, har) = (idx(&defs, "/pistol"), idx(&defs, "/chainsaw"), idx(&defs, "/plasma_rifle"), idx(&defs, "/heavy_rifle_heavy_ar"));
        assert_eq!((defs[pistol].selection_group_index, defs[chainsaw].selection_group_index), (0, 0));
        assert_eq!((defs[plasma].selection_group_index, defs[har].selection_group_index), (2, 3), "_weap2 is the plasma rifle, _weap3 the HAR");
        let mut s = WeaponSelect::new(&a);
        // Group 0 from the shotgun: the first selectable member in inventory order (pistol), issued on the
        // same frame (group buttons are handled before UpdateWeapon's deferred switch).
        assert_eq!(frame(&mut a, &mut s, group(0)), Some(pistol));
        // Again after g_weaponChangeMinIntervalMS: the current one rotates to the front, the next member wins.
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        assert_eq!(frame(&mut a, &mut s, group(0)), Some(chainsaw));
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        assert_eq!(frame(&mut a, &mut s, group(0)), Some(pistol));
        // A group with only the current weapon does nothing.
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        assert_eq!(frame(&mut a, &mut s, group(1)), Some(idx(&defs, "/shotgun")));
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        assert_eq!(frame(&mut a, &mut s, group(1)), None);
    }

    #[test]
    fn switches_wait_for_the_min_interval() {
        let Some(defs) = defs() else { return };
        let mut a = Arsenal::new(defs.to_vec());
        let mut s = WeaponSelect::new(&a);
        let t0 = a.time_ms + 16;
        assert!(frame(&mut a, &mut s, group(2)).is_some());
        // A second selection right away is marked now but issued at t0 + 375.
        assert_eq!(frame(&mut a, &mut s, group(3)), None);
        let mut at = None;
        for _ in 0..40 {
            if frame(&mut a, &mut s, SelectInput::default()).is_some() {
                at = Some(a.time_ms);
                break;
            }
        }
        assert_eq!(at, Some(t0 + 375 + (16 - 375 % 16) % 16), "first frame at or after the interval");
        assert_eq!(a.current, idx(&defs, "/heavy_rifle_heavy_ar"));
    }

    #[test]
    fn next_prev_skip_the_chainsaw_and_empty_weapons() {
        let Some(defs) = defs() else { return };
        let mut a = Arsenal::new(defs.to_vec());
        let mut s = WeaponSelect::new(&a);
        let next = SelectInput { next: true, ..Default::default() };
        let prev = SelectInput { prev: true, ..Default::default() };
        // Next from the shotgun is the HAR; empty the bullets and it is skipped (and the chaingun too).
        let p = a.pools.iter().position(|p| p.key == defs[idx(&defs, "/heavy_rifle_heavy_ar")].ammo_pool).unwrap();
        a.pools[p].count = 0;
        frame(&mut a, &mut s, next);
        let w = frame(&mut a, &mut s, SelectInput::default());
        assert_eq!(w, Some(idx(&defs, "/plasma_rifle")), "next/prev selections are issued on the following frame");
        // Prev from the pistol wraps past the chainsaw and the (unselectable) fists to the BFG.
        a.current = idx(&defs, "/pistol");
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        frame(&mut a, &mut s, prev);
        assert_eq!(frame(&mut a, &mut s, SelectInput::default()), Some(idx(&defs, "/bfg")));
    }

    #[test]
    fn change_weapon_tap_swaps_to_the_last_weapon() {
        let Some(defs) = defs() else { return };
        let mut a = Arsenal::new(defs.to_vec());
        let sg = a.current;
        let mut s = WeaponSelect::new(&a);
        assert_eq!(frame(&mut a, &mut s, group(0)), Some(idx(&defs, "/pistol")));
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        let hold = SelectInput { change: true, ..Default::default() };
        // A hold past weapon_SelectLastWeaponDelay (180 ms) is the weapon wheel: no swap.
        for _ in 0..13 {
            frame(&mut a, &mut s, hold);
        }
        frame(&mut a, &mut s, SelectInput::default());
        for _ in 0..3 {
            assert_eq!(frame(&mut a, &mut s, SelectInput::default()), None);
        }
        // A tap swaps back to the shotgun (the quick-swap reserve) on the next frame.
        frame(&mut a, &mut s, hold);
        frame(&mut a, &mut s, SelectInput::default());
        assert_eq!(frame(&mut a, &mut s, SelectInput::default()), Some(sg));
        // The BFG is never the reserve (quickSwapRemember 0): shotgun -> BFG, tap -> shotgun, and a second tap
        // finds no reserve (the game then auto-selects via 0x140e40390, not ported).
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        assert_eq!(frame(&mut a, &mut s, group(8)), Some(idx(&defs, "/bfg")));
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        frame(&mut a, &mut s, hold);
        frame(&mut a, &mut s, SelectInput::default());
        assert_eq!(frame(&mut a, &mut s, SelectInput::default()), Some(sg));
        for _ in 0..30 {
            frame(&mut a, &mut s, SelectInput::default());
        }
        frame(&mut a, &mut s, hold);
        frame(&mut a, &mut s, SelectInput::default());
        assert_eq!(frame(&mut a, &mut s, SelectInput::default()), None);
    }
}

mod zoom {
    use rancher_sim::weapons::{Arsenal, Zoom, ZoomInput, ZoomMode};

    use super::{defs, idx};

    fn inp(button: bool) -> ZoomInput {
        ZoomInput { button, base_fov: 90.0, moving: false, jumped: false, double_jumped: false, on_ground: true, hands_state_ok: true, hands_flags_ok: true, hands_hidden: false }
    }

    #[test]
    fn ssg_zooms_in_after_the_blended_delay_and_back_out() {
        let Some(defs) = defs() else { return };
        let mut a = Arsenal::new(defs.to_vec());
        a.current = idx(&defs, "/double_barrel");
        let d = a.def().clone();
        assert_eq!(d.zoom_mode, ZoomMode::WeaponNoHandAnim);
        assert!(d.zoom.has_blended_zoom && d.zoom.zoom_time == 198 && d.zoom.zoomed_fov == 75.0);
        let mut z = Zoom::new(defs.len(), 90.0);
        let t0 = 1000;
        let sounds = z.update(&a, t0, &inp(true));
        assert!(z.zoomed && z.wanted);
        assert_eq!(sounds, vec![d.zoom_in_sound.clone()]);
        // FOV lerp starts after zoomDelay 0 + g_blendedZoom_fovDelay 132 and takes zoomTime 198.
        assert_eq!(z.view_fov(t0 + 132), 90.0);
        assert!((z.view_fov(t0 + 132 + 99) - 82.5).abs() < 1e-4);
        assert_eq!(z.view_fov(t0 + 132 + 198), 75.0);
        assert_eq!(z.fraction(&d, t0 + 400, 90.0), 1.0);
        // Hands FOV ratio: zoomedHandsFOV / zoomedFOV on the same timing.
        assert!((z.hands_ratio.value(t0 + 400) - 40.0 / 75.0).abs() < 1e-6);
        // No hand anim for ZOOM_WEAPON_NO_HANDANIM: zoomPCT stays 0.
        assert_eq!(z.zoom_pct, 0.0);
        // Release: out from the current FOV over zoomTime * (90 - cur) / (90 - 75).
        let t1 = t0 + 400;
        let sounds = z.update(&a, t1, &inp(false));
        assert!(!z.zoomed && !z.wanted);
        assert_eq!(sounds, vec![d.zoom_out_sound.clone()]);
        assert_eq!(z.fov.start, t1);
        assert_eq!(z.fov.duration, 198);
        assert_eq!(z.view_fov(t1 + 198), 90.0);
    }

    #[test]
    fn zoom_scales_with_g_fov_and_needs_a_zoom_mode() {
        let Some(defs) = defs() else { return };
        let mut a = Arsenal::new(defs.to_vec());
        a.current = idx(&defs, "/double_barrel");
        // 0x140f14ff0: zoomedFOV * g_fov / atof("90").
        assert!((Zoom::zoomed_fov(a.def(), 110.0) - 75.0 * 110.0 / 90.0).abs() < 1e-4);
        a.current = idx(&defs, "/shotgun");
        assert_eq!(a.def().zoom_mode, ZoomMode::None);
        let mut z = Zoom::new(defs.len(), 90.0);
        z.update(&a, 1000, &inp(true));
        assert!(!z.zoomed && !z.wanted, "ZOOM_NONE weapons do not zoom without mods");
    }

    #[test]
    fn no_zoom_while_moving_in_the_air_without_can_zoom_while_jumping() {
        let Some(mut defs) = defs() else { return };
        // Both zoomable SP weapons (pistol, SSG) set canZoomWhileJumping; test the gate on a copy without it.
        let ssg = idx(&defs, "/double_barrel");
        assert!(defs[ssg].zoom.can_zoom_while_jumping);
        std::sync::Arc::make_mut(&mut defs[ssg]).zoom.can_zoom_while_jumping = false;
        let mut a = Arsenal::new(defs.to_vec());
        a.current = ssg;
        let mut z = Zoom::new(defs.len(), 90.0);
        let air = ZoomInput { moving: true, on_ground: false, ..inp(true) };
        z.update(&a, 1000, &air);
        assert!(!z.zoomed);
        z.update(&a, 1016, &inp(true));
        assert!(z.zoomed, "zooms once on the ground");
        z.update(&a, 1032, &air);
        assert!(!z.zoomed, "leaving the ground unzooms");
    }
}

mod melee {
    use rancher_sim::weapons::{HandsInput, MeleeBounds, MeleeDamageType, SweepHit, WebEvent, WebRequest};
    use rancher_sim::Vec3;

    use super::defs;
    use super::hands_driver::{input, rig};

    #[test]
    fn shotgun_melee_forces_melee_into_then_misses_without_a_target() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/shotgun");
        let sub = a.def().hands.subweb.clone();
        let md = a.def().melee.clone();
        assert!(md.has_directional_melee && md.melee_from_melee_input);
        assert_eq!((md.bounds, md.damage_type), (MeleeBounds::B48, MeleeDamageType::FirstPlusActor));
        h.start(&mut a, &mut web);
        for _ in 0..40 {
            h.tick(&mut a, &mut web, 16, &input(false));
        }
        assert_eq!(web.state, "idle");
        h.tick(&mut a, &mut web, 16, &HandsInput { melee: true, ..input(false) });
        assert!(h.requests.contains(&WebRequest::ForceState { line: 0x1000, sub: sub.clone(), to: "melee_into", blend_frames: 0 }), "{:?}", h.requests);
        assert_eq!(h.flags.0[2] & 0x0c, 0x0c, "0x10376 |= 0x2c (0x20 is per frame)");
        h.tick(&mut a, &mut web, 16, &input(false));
        assert!(h.requests.contains(&WebRequest::ChangeStateVia { line: 0x209f, sub: sub.clone(), to: "idle", via: "melee_miss" }), "{:?}", h.requests);
        assert!(h.registrations().iter().any(|r| r.0 == 1 && r.2 == "melee_out"));
    }

    #[test]
    fn joint_trace_damages_the_first_non_actor_once() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/shotgun");
        h.start(&mut a, &mut web);
        let inp = input(false);
        let start = WebEvent::AnimArgs { name: "ae_handsStartJointMeleeTrace".into(), int: None, strings: vec!["right_hand".into(), "melee_impact".into()] };
        h.dispatch(&mut a, &mut web, &start, &inp);
        assert!(h.melee.active() && h.melee.joint == "melee_impact");
        assert_eq!(h.melee.damage, 50.0, "damage/zion/player/sp/directional_melee maxDamage");
        let mut sweep = |_a: Vec3, b: Vec3, half: f32| {
            assert_eq!(half, 24.0);
            Some(SweepHit { fraction: 0.5, pos: b, normal: Vec3::X, target: Some(0), demon: None, actor: false })
        };
        let (eye, fwd) = (Vec3::ZERO, Vec3::X);
        // Start: the trace takes the joint and sweeps eye -> joint, read on the next update.
        assert!(h.melee_update(100, Vec3::new(20.0, 0.0, 0.0), eye, fwd, &mut sweep).is_empty());
        let hits = h.melee_update(116, Vec3::new(30.0, 0.0, 0.0), eye, fwd, &mut sweep);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].damage, Some(50.0));
        // MELEE_FIRST_PLUS_ACTOR: later non-actor hits do nothing.
        assert!(h.melee_update(132, Vec3::new(40.0, 0.0, 0.0), eye, fwd, &mut sweep).is_empty());
        let end = WebEvent::Anim { name: "ae_handsEndMeleeTrace".into(), int: None };
        h.dispatch(&mut a, &mut web, &end, &inp);
        assert!(!h.melee.active());
    }
}

mod fists {
    use rancher_sim::weapons::{HandsInput, WebEvent, WebRequest};

    use super::defs;
    use super::hands_driver::{input, rig};

    #[test]
    fn fists_punch_right_then_left_alternating() {
        let Some(defs) = defs() else { return };
        let (mut a, mut h, mut web) = rig(&defs, "/fists");
        let sub = a.def().hands.subweb.clone();
        assert_eq!(a.def().hands.trigger_mode, 0xd);
        assert!(a.def().melee.melee_ltrt && a.def().melee.melee_from_fire_input);
        h.start(&mut a, &mut web);
        for _ in 0..40 {
            h.tick(&mut a, &mut web, 16, &input(false));
        }
        assert_eq!(web.state, "idle");
        // BUTTON_ATTACK1: MELEE_RIGHT; hands+0x10390 toggles first, so the first punch is melee_r2.
        h.tick(&mut a, &mut web, 16, &input(true));
        assert!(h.requests.contains(&WebRequest::ChangeStateVia { line: 0xfdf, sub: sub.clone(), to: "idle", via: "melee_r2" }), "{:?}", h.requests);
        h.tick(&mut a, &mut web, 16, &input(false));
        // ae_setInterruptible opens the transition; BUTTON_ALTFIRE (trigger mode 0xd) punches left.
        let inp = input(false);
        h.dispatch(&mut a, &mut web, &WebEvent::Anim { name: "ae_setInterruptible".into(), int: Some(1) }, &inp);
        h.tick(&mut a, &mut web, 16, &HandsInput { altfire: true, ..input(false) });
        assert!(h.requests.contains(&WebRequest::ChangeStateVia { line: 0x2105, sub: sub.clone(), to: "idle", via: "melee_l1" }), "{:?}", h.requests);
    }
}
