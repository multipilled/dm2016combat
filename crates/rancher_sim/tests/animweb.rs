//! The hands anim web runtime against the user's install (skipped when DOOM is not found).

use std::sync::Arc;

use idres::animweb::AnimWeb;
use rancher_sim::animweb::{AnimData, AnimWebRuntime};

fn runtime(subs: &[&str]) -> Option<AnimWebRuntime> {
    let doom = idres::find_install()?;
    let c = idres::Container::open(&doom.join("base"), "gameresources").ok()?;
    let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands.decl").ok()?).into_owned();
    let web = Arc::new(AnimWeb::parse(&text).unwrap());
    let data = Arc::new(AnimData::load(&c, &web, subs));
    Some(AnimWebRuntime::new(web, data))
}

/// Steps in 16 ms frames until `until` ms, returning (time, state) changes and (time, event) hits.
fn run(rt: &mut AnimWebRuntime, from: i32, until: i32) -> (Vec<(i32, String)>, Vec<(i32, String)>) {
    let mut states = Vec::new();
    let mut events = Vec::new();
    let mut last = rt.current().map(|(_, s)| s.to_string()).unwrap_or_default();
    let mut t = from;
    while t < until {
        t += 16;
        for e in rt.update(t) {
            events.push((t, e.event.name.clone()));
        }
        let now = rt.current().map(|(_, s)| s.to_string()).unwrap_or_default();
        if now != last {
            states.push((t, now.clone()));
            last = now;
        }
    }
    (states, events)
}

#[test]
fn shotgun_shot_routes_through_pump() {
    let Some(mut rt) = runtime(&["shotgun"]) else { return };
    assert!(rt.set_state("shotgun", "idle"));
    rt.update(0);
    assert!(rt.change_state_via(None, "idle", Some("shoot")));
    let (states, events) = run(&mut rt, 0, 2000);
    let names: Vec<&str> = states.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(names.first(), Some(&"shoot"), "{states:?}");
    assert!(names.contains(&"shoot_delay") || names.contains(&"shoot_delay_2"), "{states:?}");
    assert_eq!(names.last(), Some(&"idle"), "{states:?}");
    let fire: Vec<_> = events.iter().filter(|(_, e)| e == "ae_fireWeaponRight").collect();
    assert_eq!(fire.len(), 1, "{events:?}");
    assert_eq!(fire[0].0, states[0].0, "fires on entering shoot");
    assert!(events.iter().any(|(_, e)| e == "ae_ejectShell"), "{events:?}");
}

#[test]
fn heavy_ar_shot_returns_to_idle_and_switch_routes_cross_subweb() {
    let Some(mut rt) = runtime(&["assault_rifle_heavy_ar", "shotgun"]) else { return };
    assert!(rt.set_state("assault_rifle_heavy_ar", "idle"));
    rt.update(0);
    assert!(rt.change_state_via(None, "idle", Some("shoot")));
    let (states, _) = run(&mut rt, 0, 1500);
    let names: Vec<&str> = states.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(names, vec!["shoot", "idle"], "{states:?}");
    // Weapon switch (idHands BRINGDOWN, new sub-web): new idle via new bringup; FindPath routes through the
    // old sub-web's bringdown and its toSubWeb edge.
    assert!(rt.change_state_via(Some("shotgun"), "idle", Some("bringup")));
    let (states, _) = run(&mut rt, 1500, 5000);
    let names: Vec<&str> = states.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(names, vec!["bringdown", "bringup", "idle"], "{states:?}");
    assert_eq!(rt.current().map(|(s, _)| s), Some("shotgun"), "{states:?}");
    assert_eq!(states.last().map(|s| s.1.as_str()), Some("idle"), "{states:?}");
}
