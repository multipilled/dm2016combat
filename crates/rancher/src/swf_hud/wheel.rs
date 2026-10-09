//! The weapon wheel (weapon_select4.bswf through `idswf::wheel`), opened and closed as idPlayer does it
//! (UpdateWeapon 0x140e45e50) and driven like the weapon select HUD element (notes in gamedata/re/SWF.md,
//! "Weapon wheel"):
//! - `_changeWeapon` (Q) held for weapon_OpenWeaponWheelDelay (180 game ms) opens it; a shorter tap is the
//!   quick swap to the last weapon (rancher_sim weapons::select).
//! - While it is open the mouse drives the GUI cursor, not the view: outside weapon_wheel_deadZone of the wheel's
//!   centre the cursor is clamped to the wheel, its direction picks one of eight sectors (focus moves only to a
//!   slot holding a selectable weapon) and turns the dial.
//! - swf_weaponSelect_slowTimeDelayMS (192) after opening, the game timeline blends to
//!   swf_weaponSelect_slowTimeScale (0.14) over 0.5 s; closing blends it back to 1 over 0.5 s.
//! - Releasing `_changeWeapon` closes it and selects the focused weapon (SelectWeaponByDecl): [`WeaponWheel::pick`]
//!   is taken by the weapon code.
//!
//! RANCHER_WHEEL_MOUSE=<a>-<b>:<dx>/<dy>[,...]  self-test: move the wheel cursor by (dx, dy) GUI pixels per
//! second from a to b seconds after the wheel opened (hold it with RANCHER_HOLD=<a>-<b>:_changeweapon).
//! RANCHER_WHEEL_TRACE=1  log open / slow-down / close / pick and the time scale to stdout.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use idswf::wheel::{self, Item, Ramp};

use super::HudRuntime;

/// The wheel's state for the rest of the game.
#[derive(Resource, Debug, Clone)]
pub struct WeaponWheel {
    /// The wheel is up: the mouse belongs to it (no view look).
    pub open: bool,
    /// Scale of the game timeline (game msec = int(frame msec * time_scale), as idGameTimeManagerLocal's timeline
    /// 1, which player Move and UpdateWeapon run on).
    pub time_scale: f32,
    /// Weapon (arsenal def index) chosen on release, for SelectWeaponByDecl; the weapon code takes it.
    pub pick: Option<usize>,
    /// Game ms when `_changeWeapon` went down (player+0xcdfc).
    pressed_at: i32,
    held_last: bool,
    /// When the slow-down starts (game ms, element +0x25c) and whether it is applied (+0x258).
    slow_at: Option<i32>,
    slowed: bool,
    ramp: Ramp,
    /// The ramp's clock: unscaled milliseconds (INTERIM: the engine counts timeline 0 ticks).
    clock_ms: f64,
    /// GUI cursor (wheel stage units).
    cursor: [f32; 2],
    /// Slot weapons (arsenal def index) as last shown.
    slots: Vec<Option<usize>>,
    /// The player's masterWeaponList (loaded once).
    master: Option<Vec<String>>,
    /// RANCHER_WHEEL_MOUSE windows.
    auto_mouse: Vec<(f32, f32, [f32; 2])>,
    elapsed: f32,
    /// `elapsed` when the wheel last opened.
    opened_at: f32,
    /// RANCHER_WHEEL_TRACE.
    trace: bool,
    traced_focus: Option<usize>,
}

impl Default for WeaponWheel {
    fn default() -> Self {
        let auto_mouse = std::env::var("RANCHER_WHEEL_MOUSE")
            .ok()
            .map(|v| {
                v.split(',')
                    .filter_map(|p| {
                        let (win, d) = p.split_once(':')?;
                        let (a, b) = win.split_once('-')?;
                        let (x, y) = d.split_once('/')?;
                        Some((a.parse().ok()?, b.parse().ok()?, [x.parse().ok()?, y.parse().ok()?]))
                    })
                    .collect()
            })
            .unwrap_or_default();
        WeaponWheel {
            open: false,
            time_scale: 1.0,
            pick: None,
            pressed_at: 0,
            held_last: false,
            slow_at: None,
            slowed: false,
            ramp: Ramp::constant(1.0),
            clock_ms: 0.0,
            cursor: [0.0; 2],
            slots: Vec::new(),
            master: None,
            auto_mouse,
            elapsed: 0.0,
            opened_at: 0.0,
            trace: std::env::var("RANCHER_WHEEL_TRACE").is_ok(),
            traced_focus: None,
        }
    }
}

/// Ticks per second of the ramp's clock (ms here).
const CLOCK_RATE: i32 = 1000;

/// The player entityDef's `masterWeaponList` (player+0x4ff0, at most 16 entries, 0x140e02932).
fn master_weapon_list(decls: &idres::decldb::DeclDb) -> Vec<String> {
    let Ok(b) = decls.get("entitydef", "player") else { return Vec::new() };
    let key = if b.path("edit.masterWeaponList").is_some() { "edit.masterWeaponList" } else { "masterWeaponList" };
    (0..16).map_while(|i| b.str(&format!("{key}.item[{i}]")).map(str::to_string)).collect()
}

/// Opens / closes the wheel, moves its cursor, picks, and runs the time-scale blend. Runs after the game frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn logic(
    mut ww: ResMut<WeaponWheel>,
    rt: Option<ResMut<HudRuntime>>,
    actions: Option<Res<crate::input::Actions>>,
    sim: Option<Res<crate::Sim>>,
    combat: Option<Res<crate::combat::Combat>>,
    menu: Option<Res<crate::settings::MenuOpen>>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time<Real>>,
) {
    let ww = &mut *ww;
    let dt = time.delta_secs();
    ww.elapsed += dt;
    ww.clock_ms += time.delta_secs_f64() * 1000.0;
    let clock = ww.clock_ms as i32;
    let (Some(mut rt), Some(actions), Some(sim), Some(combat)) = (rt, actions, sim, combat) else { return };
    let rt = &mut *rt;
    let Some(movie) = rt.wheel.as_mut() else { return };
    let now = sim.game_ms;
    let held = actions.held("_changeweapon");
    if held && !ww.held_last {
        ww.pressed_at = now;
    }
    ww.held_last = held;
    let paused = menu.is_some_and(|m| m.0);

    // INTERIM: of UpdateWeapon's open conditions only the hold time and weapon_allowWeaponSwitchWheel are modelled
    // (not 0x140e1f6e0(player, 2), HUD state 0xe, the forced-close weapon_Wheel_Activation_delay); no DOF
    // (weapon_wheel_dof_*), no open sound, no gamepad stick path (0x140c5a010 / 0x140c5b4f0).
    if !ww.open && held && !paused && wheel::ALLOW_WHEEL && now - ww.pressed_at >= wheel::OPEN_DELAY_MS {
        // Show 0x140c5b800 with the slots of the player's wheel list (0x140e02932): the entityDef's masterWeaponList
        // entries whose decl is selectable (0x63d), in order; a slot holds the weapon when the player owns it.
        let a = &combat.arsenal;
        let master = ww.master.get_or_insert_with(|| master_weapon_list(&rt.decls)).clone();
        let mut slots = vec![None; wheel::SLOTS];
        let mut items = vec![Item { name: "Empty".into(), ..default() }; wheel::SLOTS];
        let listed = master.iter().filter_map(|decl| a.defs.iter().position(|d| &d.decl == decl)).filter(|&w| a.defs[w].selectable);
        for (s, w) in listed.take(wheel::SLOTS).enumerate() {
            let d = &a.defs[w];
            slots[s] = Some(w);
            // Slot +0x2d0 (0x140e02a78): the ammo count, -1 without an ammo item.
            let count = if d.infinite_ammo { -1 } else { a.ammo_for(w).unwrap_or(0) };
            items[s] = Item {
                weapon: Some(d.decl.clone()),
                icon: rt.decls.get("weapon", &d.decl).ok().and_then(|b| b.str("edit.icon").map(str::to_string)),
                name: movie.player.localize(&d.display_name).into_owned(),
                count,
                current: w == a.current,
                // Slot flag +0x1b (0x2eb): the player owns it (every arsenal weapon here).
                selectable: true,
                empty: !d.infinite_ammo && count <= 0,
            };
        }
        if ww.trace {
            println!("wheel: open at game {now} ms (pressed {}), t={:.3}s, centre/half {:?}, slots {:?}", ww.pressed_at, ww.elapsed, movie.centre_and_half(), items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>());
        }
        ww.slots = slots;
        movie.show(items);
        ww.open = true;
        ww.opened_at = ww.elapsed;
        ww.cursor = movie.centre_and_half().0;
        // The slow-down waits swf_weaponSelect_slowTimeDelayMS (0x140c5bb18).
        if wheel::SLOW_TIME && !ww.slowed {
            ww.slow_at = Some(now + wheel::SLOW_TIME_DELAY_MS);
        }
    } else if ww.open && (!held || paused) {
        // Release: HUD state 0xd -> Hide 0x140c5a630 -> SelectWeaponByDecl(focused weapon).
        let chosen = movie.focus.and_then(|i| ww.slots.get(i).copied().flatten());
        movie.hide();
        ww.open = false;
        ww.slow_at = None;
        ww.pick = chosen;
        if ww.trace {
            let name = chosen.map(|w| combat.arsenal.defs[w].decl.clone());
            println!("wheel: close at game {now} ms, t={:.3}s, pick {name:?}, time scale {:.3}", ww.elapsed, ww.ramp.value(clock));
        }
        if ww.slowed {
            ww.slowed = false;
            ww.ramp.blend_to(clock, 1.0, wheel::SLOW_RAMP_OUT_S, CLOCK_RATE);
        }
    }

    if ww.open {
        // Update 0x140c5bcc0: the slow-down once its delay has passed.
        if ww.slow_at.is_some_and(|t| t <= now) && !ww.slowed {
            ww.slowed = true;
            ww.slow_at = None;
            ww.ramp.blend_to(clock, wheel::SLOW_TIME_SCALE, wheel::SLOW_RAMP_IN_S, CLOCK_RATE);
            if ww.trace {
                println!("wheel: slow-down starts at game {now} ms, t={:.3}s", ww.elapsed);
            }
        }
        // Mouse path 0x140c5a200. INTERIM: the game reads the OS cursor (with pointer ballistics); this moves a
        // virtual cursor by the raw mouse motion.
        let mut d = [motion.delta.x, motion.delta.y];
        for &(a, b, v) in &ww.auto_mouse {
            if (a..b).contains(&(ww.elapsed - ww.opened_at)) {
                d[0] += v[0] * dt;
                d[1] += v[1] * dt;
            }
        }
        ww.cursor[0] += d[0];
        ww.cursor[1] += d[1];
        let (centre, half) = movie.centre_and_half();
        if let Some((c, dir)) = wheel::mouse_direction(ww.cursor, centre, half) {
            ww.cursor = c;
            movie.point(dir);
        }
        if ww.trace && movie.focus != ww.traced_focus {
            ww.traced_focus = movie.focus;
            println!("wheel: focus {:?} ({:?}), dial {:?}, t={:.3}s", movie.focus, movie.focus.map(|i| movie.items[i].name.clone()), movie.dial, ww.elapsed);
        }
    }
    let scale = ww.ramp.value(clock);
    if ww.trace && (scale - ww.time_scale).abs() > 0.0 && ((scale * 20.0) as i32 != (ww.time_scale * 20.0) as i32 || scale == 1.0 || scale == wheel::SLOW_TIME_SCALE) {
        println!("wheel: time scale {scale:.3}, game msec {} at t={:.3}s", sim.msec_last, ww.elapsed);
    }
    ww.time_scale = scale;
}

/// The wheel's quad (0x140beef90): on the helmet tag `idswf::wheel::TAG` ("weapon_select_vs"), turned
/// by idAngles (0.5, 0.5, 0) (swf_dossier_UseNewWeaponSelect 1), scale swf_dossier_WeaponSelectScale (0x140beed01),
/// projected with the HUD's 80-degree field of view (CalcFov(80) there too).
pub(super) fn panel(hud: &idswf::hud::Hud, width: f32, height: f32) -> Option<(idswf::placement::Panel, idswf::placement::Projection)> {
    use idswf::placement::{HUD_FOV, Panel, Projection};
    let tag = hud.tags().get(wheel::TAG)?;
    Some((Panel::on_tag_angles(tag, wheel::ANGLES, wheel::SCALE), Projection::from_fov(HUD_FOV, width, height)))
}
