//! The idHands driver (weapons::hands) running the real hands anim web (animweb::AnimWebRuntime) on the
//! user's install: shots must come from the anim events at the rates the decls and anims imply.
//! Skipped when DOOM is not found.

use std::sync::Arc;

use idres::animweb::AnimWeb;
use rancher_sim::animweb::{AnimData, AnimWebRuntime};
use rancher_sim::install;
use rancher_sim::weapons::hands::{Hands, HandsInput, HandsWeb};
use rancher_sim::weapons::{load_arsenal, Arsenal, WeaponEvent, WeaponInput};

struct Rig {
    arsenal: Arsenal,
    hands: Hands,
    web: AnimWebRuntime,
}

fn rig(weapon: &str) -> Option<Rig> {
    let doom = idres::find_install()?;
    let inst = install::load(&doom).ok()?;
    let defs = load_arsenal(&inst.decls);
    let c = inst.decls.container_arc();
    let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands.decl").ok()?).into_owned();
    let web = Arc::new(AnimWeb::parse(&text).unwrap());
    let subs: Vec<String> = defs.iter().map(|d| d.hands.subweb.clone()).collect();
    let sub_refs: Vec<&str> = subs.iter().map(String::as_str).collect();
    let data = Arc::new(AnimData::load(&c, &web, &sub_refs));
    let mut arsenal = Arsenal::new(defs.clone());
    arsenal.current = defs.iter().position(|d| d.decl.ends_with(weapon)).expect("weapon");
    let mut web = AnimWebRuntime::new(web, data);
    let mut hands = Hands::new(defs.len());
    hands.start(&mut arsenal, &mut web);
    Some(Rig { arsenal, hands, web })
}

impl Rig {
    /// Runs frames of `ms` until game time `end`; returns shot times and the states entered.
    fn run(&mut self, ms: i32, end: i32, trigger: impl Fn(i32) -> bool) -> (Vec<i32>, Vec<(i32, String)>) {
        let mut shots = Vec::new();
        let mut states = Vec::new();
        let mut last = String::new();
        while self.arsenal.time_ms + ms <= end {
            let t = self.arsenal.time_ms + ms;
            let inp = HandsInput { weapon: WeaponInput { trigger: trigger(t), ..Default::default() }, ..Default::default() };
            for e in self.hands.tick(&mut self.arsenal, &mut self.web, ms, &inp) {
                if let WeaponEvent::Fired(s) = e {
                    shots.push(s.time_ms);
                }
            }
            let now = HandsWeb::current(&self.web).map(|(s, n)| format!("{s}/{n}")).unwrap_or_default();
            if now != last {
                states.push((self.arsenal.time_ms, now.clone()));
                last = now;
            }
        }
        (shots, states)
    }
}

#[test]
fn heavy_ar_full_auto_fires_from_anim_events_at_the_firing_interval() {
    let Some(mut r) = rig("/heavy_rifle_heavy_ar") else { return };
    // Bring-up to idle first.
    let (_, states) = r.run(16, 3000, |_| false);
    assert!(states.last().is_some_and(|s| s.1.ends_with("/idle")), "{states:?}");
    let interval = r.arsenal.def().firing_interval;
    let t0 = r.arsenal.time_ms;
    let (shots, states) = r.run(8, t0 + 1000, |_| true);
    assert!(shots.len() >= 3, "shots {shots:?} states {states:?}");
    for w in shots.windows(2) {
        let gap = w[1] - w[0];
        assert!(gap >= interval, "refire after {gap} ms < firing interval {interval}: {shots:?}");
        assert!(gap <= interval + 60, "refire after {gap} ms (interval {interval}): {shots:?} {states:?}");
    }
}

#[test]
fn shotgun_shot_then_pump_then_idle() {
    let Some(mut r) = rig("/shotgun") else { return };
    let (_, states) = r.run(16, 3000, |_| false);
    assert!(states.last().is_some_and(|s| s.1.ends_with("/idle")), "{states:?}");
    let t0 = r.arsenal.time_ms;
    let (shots, states) = r.run(16, t0 + 2500, |t| t < t0 + 50);
    assert_eq!(shots.len(), 1, "{shots:?} {states:?}");
    let names: Vec<&str> = states.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(names.first(), Some(&"shotgun/shoot"), "{states:?}");
    assert_eq!(names.last(), Some(&"shotgun/idle"), "{states:?}");
}

#[test]
fn plasma_loop_keeps_firing_at_the_firing_interval_until_release() {
    let Some(mut r) = rig("/plasma_rifle") else { return };
    let (_, states) = r.run(16, 3000, |_| false);
    assert!(states.last().is_some_and(|s| s.1.ends_with("/idle")), "{states:?}");
    let interval = r.arsenal.def().firing_interval;
    let t0 = r.arsenal.time_ms;
    let (shots, states) = r.run(16, t0 + 3000, |t| t < t0 + 2000);
    let expected = 2000 / interval;
    assert!(shots.len() as i32 >= expected - 2, "{} shots, expected about {expected}: {shots:?} {states:?}", shots.len());
    for w in shots.windows(2) {
        let gap = w[1] - w[0];
        assert!((gap - interval).abs() <= 20, "gap {gap} vs interval {interval}: {shots:?}");
    }
    assert!(states.iter().any(|s| s.1.ends_with("/shootstate")), "{states:?}");
    assert_eq!(states.last().map(|s| s.1.as_str()), Some("plasma_rifle/idle"), "{states:?}");
}
