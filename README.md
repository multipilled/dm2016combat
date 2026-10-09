# dm2016combat

A testbed that recreates DOOM (2016) single-player gameplay in Rust + Bevy, built to match the original
exactly. It ships **no game data**: tuning values, decls, models, animations, textures, sounds, HUD and
effects are all read at runtime from your own installed copy of DOOM (2016). Behaviour is recovered from the
game's own data and executable (notes with addresses under the git-ignored `gamedata/re/`).

## Status
About 76% of the full goal (DOOM 2016 single-player combat 1:1: movement, every weapon and mod, hands,
camera, HUD and menus, FX, audio, the maps' look and the demons with their AI). Recently merged: glory kills, the
Possessed and Possessed Soldier, rocket launcher lock-on, the Imp, pickups, drops and powerups, player upgrades, HUD
mod indicators.

| Milestone | Weight | Done |
|---|---|---|
| Formats and readers (containers, decls, md6, virtual textures, SWF, Wwise, maps) | 8 | 100% |
| Player movement 1:1 (physics, collision, ledge grab, movers) | 10 | 90% |
| Weapons 1:1 (firing, spread, kick, projectiles, zoom, melee; mods, charge, alt-fire) | 15 | 95% |
| First-person presentation (hands animation, layers, camera effects, weapon FX, sound) | 12 | 88% |
| HUD, menus, settings | 8 | 95% |
| Rendering: maps, virtual-texture materials, lighting, post chain | 12 | 80% |
| Damageable demons (damage, pain, death, animation, IK) | 10 | 70% |
| Demon AI and combat (navigation, attacks, every single-player demon) | 20 | 38% |
| Glory kills, chainsaw, gore, pickups, upgrades and runes | 5 | 55% |
| **Overall** | 100 | **76%** |

Want to help? See [CONTRIBUTING.md](CONTRIBUTING.md). License: MIT (`LICENSE`).

## Fan project disclaimer
This is a non-commercial fan project for research and learning. It is not affiliated with, endorsed or sponsored by
id Software, Bethesda Softworks, ZeniMax Media or Microsoft; DOOM and all related names are their trademarks. You need your own legal copy of the game: this
repository contains no game files, no extracted assets and no decompiled code (game data is read from your install
into git-ignored folders). Comments may cite function addresses in the retail executable as research notes. If you
represent a rights holder and want something changed or removed, open an issue and it will be done promptly.


## Game files
You need your own copy of DOOM (2016). No game files are in this repository: assets are read from your install at
run time or extracted into git-ignored folders.

## Requirements

- DOOM (2016) installed (Steam). Found automatically at the usual Steam library paths, or set
  `DOOM_DIR` to the folder that contains `DOOMx64.exe` and `base/`.
- Rust 1.95+.

## Run

```bash
cargo run -p rancher --release
```

Click to capture the mouse; Esc pauses the game and frees the mouse (the options menu opens there once it
lands). Controls are the game's own default SP binds, read from the
install's `default.cfg`: WASD move, Space jump (again in the air to double jump), C crouch, Shift walk,
Mouse1 fire, Mouse2 zoom/alt-fire, F melee, 1-8 weapon groups (repeat to cycle a group), mouse wheel / X / Z
next / previous weapon, Q last weapon. F9 switches HDR rendering (the engine's post chain:
auto exposure, bloom, tonemap; `r_hdrPostProcess`) on or off, F10 toggles the developer telemetry overlay.

Settings and key rebinds are cvars / `bind` lines in `<Saved Games>/dm2016combat/rancher.cfg` (game syntax,
e.g. `seta g_fov "110"`); the game's own config is never written.

### Real maps

```bash
RANCHER_MAP=game/sp/intro/intro cargo run -p rancher --release
```

loads a campaign map (geometry, virtual textures, collision, player start) instead of the firing range.
`RANCHER_MAP_LIGHTMAP=1` turns on the baked lighting (`RANCHER_MAP_LIGHTMAP_SCALE=16` stands in for the
engine's auto exposure until that is ported); `RANCHER_MAP_START=<idPlayerStart or checkpoint>` picks a start.

## What works

| area | status |
|---|---|
| Movement | the game's player physics: friction, acceleration, jumps / double jump, crouch, the 8-sided collision trace model and SlideMove, step springs, ledge grab, adaptive tick |
| Weapons | all campaign weapons: firing intervals, spread, kick, ammo, projectiles and splash, chaingun spin-up, gauss heat; fire happens on the hands animation's fire event via the ported idHands controller; weapon selection by group / next / previous / last like the game |
| First-person hands | the game's hands anim web (states, edges, blends, events) on the game clock, arm IK rig, 4-weight skinning, weapon lag pendulum, procedural weapon bob, animated bob cycle (fists), additive shoot / offset channels, hands FOV |
| Camera | the engine's FOV (Hor+ from g_fov at 16:9), weapon view kick incl. roll, zoom, double-jump view pitch, the hands rig's animated camera joint (landing dip), screen shakes from hand animation events |
| Rendering | the game's virtual textures transcoded bit-exactly (BC3/BC7) with the engine's sampling; real maps with baked lightmaps |
| Audio | Wwise banks and WEM media decoded from the install; weapon / anim / footstep sounds |
| HUD | the game's SWF HUD: health / armour, ammo and weapon icon, per-weapon reticles |
| FX | weapon fx decls and the particle system (muzzle flashes and lights, impacts, tracers, GPU particle stages run on the CPU) |
| Not yet | demons, damage to the player, weapon mods, glory kills, auto exposure / the engine's lighting model |

## Layout

| crate | what it does |
|---|---|
| `idres` | Reads the game's archives, decrypts `.bfile`s, parses decls (both decl dialects), md6 models / skeletons / anims, anim webs, md6Def events, virtual textures, maps (`.bmodel`, `.bcm`, `.entities`), and scans the exe for cvar defaults. |
| `jxr_sys` | Patched jxrlib for the virtual textures' headerless HD Photo pages. |
| `idaudio`, `idswf`, `idfx` | Wwise audio, SWF HUD, FX / particles. |
| `rancher_sim` | Headless, deterministic simulation: movement, weapons and the idHands driver, the anim web runtime, hand layers. Tests run against your install: `cargo test -p rancher_sim --release`. |
| `rancher` | The Bevy game. |
| `doomx` | CLI over the install: `doomx stats`, `list <words>`, `cat <resource>`, `decl`, `anims`, `models`, `anim-webs`, `extract`, `cvars`, ... |

Parallel sessions work on this repo: `COORDINATION.md` says who owns which files and the shared rules (CPU slots
via `tools/with_cpu.sh`, `tools/with_lock.sh` around rancher builds and runs). `UNVERIFIED.md` lists every
stand-in still in the code (regenerate its table with `py -I tools/unverified.py`); the commit guard in
`.githooks/` blocks game data.

`tools/` has the offline analysis scripts used to recover behaviour from the exe. They write only to the
git-ignored `gamedata/` folder.

## Self-tests (no window focus, no audio)

Environment variables drive off-screen runs: `RANCHER_SHOT=<png>` (+ `RANCHER_SHOTS=t1,t2,...` or
`RANCHER_SHOT_AT`), `RANCHER_WEAPON=<index>`, `RANCHER_FIRE=<a>-<b>`, `RANCHER_PRESS=<t>:<action>`,
`RANCHER_HOLD=<a>-<b>:<action>`, `RANCHER_TURN=<a>-<b>:<deg/s>`, `RANCHER_TRACE=1` (anim web states, events,
layer alphas, camera effects, timings), `RANCHER_MENU=<t>` (pause at t), `RANCHER_RES=<w>x<h>`,
`RANCHER_HDR=0|1`, `RANCHER_CFG=<file>` (self-tests otherwise read and write no settings). The window opens off-screen and sound stays silent.

Only distribute the code. Never commit anything extracted from the game.
