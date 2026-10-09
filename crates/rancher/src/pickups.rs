//! Pickups in the testbed (BOARD row 19): placed health / armour / ammo props picked up by touch, and the
//! Possessed's death drops (rules in `rancher_sim::pickups`; gamedata/re/pickups/PICKUPS.md).
//!
//! RANCHER_PICKUPS=1                   place the test row ahead of the range start (vial, +25, +50, mega, armour
//!                                     shard, +25, +50, shells, bullets, cells, rockets)
//! RANCHER_PICKUPS=<def>@x,y[,z];...   place these entityDefs instead (e.g. `prop/health/health_vial@96,0`)
//! RANCHER_AMMO=<0..1>                 start every ammo pool at this fraction of its max (to test ammo pickups)
//! RANCHER_TRACE=1                     log pickups, drops and removals
//! RANCHER_UPGRADES=<item>[,<item>...]  give player upgrades at start (BOARD row 23): `health:N`, `armor:N`,
//!                                     `ammo:N` (argent cells), or a perk under perk/zion/player/sp/enviroment_suit/
//!                                     (runes and Praetor suit upgrades, e.g. `increase_drop_radius`), `!` = mastered
//! The difficulty is RANCHER_DIFFICULTY, as for the demons (default MEDIUM).
//!
//! Health and armour live where the damage code keeps them: `Demons::player_health` (when demons run) and the
//! HUD's `HudState` health / armour; ammo in the arsenal's pools.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use idres::Container;
use idres::decldb::DeclDb;
use rancher_sim::Vec3 as V;
use rancher_sim::pickups::loot::{Holding, IdRandom, KillKind, LootDropComponent};
use rancher_sim::pickups::powerups::{Applied, EffectEvent, Focus, Powerups, SavingThrow};
use rancher_sim::pickups::{self, PickupDef, PlayerHit, ScaleType, Useable, Vitals};
use rancher_sim::upgrades::{self, Perk, Suit, WeaponPool};

use crate::combat::Combat;
use crate::demons::{DemonTargets, Demons};
use crate::range::to_bevy;
use crate::sound::Sound;
use crate::swf_hud::HudState;

/// The default test row: entityDefs the campaign maps place most (counts over every SP map's .entities), in an
/// order that also walks over a vial at full health and a shard at full armour (both refused), mega health last.
const TEST_ROW: [&str; 13] = [
    "prop/health/health_vial",
    "prop/health/health_pickup50",
    "prop/health/health_pickup25",
    "prop/health/health_vial",
    "prop/armor/armor_shard",
    "prop/armor/armor_pickup25",
    "prop/armor/armor_pickup50",
    "prop/armor/armor_shard",
    "prop/ammo/shotgun_ammo",
    "prop/ammo/heavy_rifle_ammo",
    "prop/ammo/plasma_rifle_ammo",
    "prop/ammo/rocket_launcher",
    "prop/powerup/megahealth",
];

pub struct PickupsPlugin {
    pub container: Arc<Container>,
    pub cvars: rancher_sim::config::CvarValues,
    /// True on a real map (no test row unless RANCHER_PICKUPS lists props).
    pub map_mode: bool,
}

struct Prop {
    id: u32,
    def: Arc<PickupDef>,
    origin: V,
    /// Dropped items: launch velocity until they land.
    vel: Option<V>,
    /// When the attraction to the player may start (idMovementThinkComponent movementDelayTime), seconds.
    attract_at: Option<f32>,
    spawned: f32,
    /// item_removalTimeMS of a drop (seconds), from `spawned`.
    lifetime: Option<f32>,
    /// What the item counts for in loot tracking while it lies in the world.
    tracked: f32,
    entity: Option<Entity>,
    /// Touched while unusable (logged once per touch).
    refused: bool,
}

/// A drop waiting for its dropEntity event.
struct Pending {
    at: f32,
    def: String,
    origin: V,
    lifetime: f32,
    per_item: i32,
}

#[derive(Resource)]
struct Pickups {
    db: DeclDb,
    container: Arc<Container>,
    defs: HashMap<String, Arc<PickupDef>>,
    props: Vec<Prop>,
    next_id: u32,
    difficulty: usize,
    log: bool,
    elapsed: f32,
    /// Component maxima / drain limits / starting values (player decl `playerHealth`).
    vitals: Vitals,
    /// The Possessed's lootDropComponent (each death drops from a fresh copy).
    loot: Option<LootDropComponent>,
    rng: IdRandom,
    /// Demons alive last frame (id -> origin, bounds centre).
    alive: HashMap<u32, (V, V)>,
    pending: Vec<Pending>,
    /// loot_tracking (cvar default 1): entityDef -> amount lying in the world in dropped items.
    tracking: HashMap<String, f32>,
    loot_enable: bool,
    gravity: f32,
    /// Meshes per model.
    meshes: HashMap<String, Option<Handle<Mesh>>>,
    ammo_fraction: Option<f32>,
    placed: Vec<(String, V)>,
    /// propMoveable pinata/default attactMovement (attractSpeed, attractRadius, attractDelayMS) and the cvar
    /// g_lootDropMovementDelayAddition.
    attract: (f32, f32, i32),
    attract_add: i32,
    /// The player's idEnvironmentSuit (upgrades) and the RANCHER_UPGRADES grants still to apply.
    suit: Suit,
    grants: Vec<(String, bool)>,
    /// Rich Get Richer: the infinite-ammo flag and the pool counts it holds.
    infinite: bool,
    held: HashMap<String, i32>,
    /// Powerups (status effects) and the Saving Throw focus.
    powerups: Powerups,
    focus: Option<Focus>,
    focus_uses: i32,
    /// statusEffect_lifetimeSec (cvar, 0 = the decls' lifetimes).
    lifetime_cvar: f32,
    /// Player death: when it happened (s), and the range's "checkpoint" (vitals and pools after the start-up grants)
    /// restored on respawn.
    dead_at: Option<f32>,
    checkpoint: Option<(Vitals, Vec<rancher_sim::weapons::arsenal::AmmoPool>)>,
    /// g_invulnerabilityOnRespawnDurationSeconds after a respawn (s, game time of the end).
    respawn_invulnerable_until: f32,
    respawn_invulnerable_sec: f32,
}

/// What the player damage path needs: the suit, and the scale powerups / focus / respawn protection put on hits
/// (0 while invulnerable). Kept current by the pickups plugin.
#[derive(Resource, Clone)]
pub struct PlayerDefense {
    pub suit: Suit,
    pub taken_scale: f32,
    /// Saving Throw: the focus health while focus is on and past its invulnerable time, and what death blows took
    /// from it since the pickups plugin last read it (f32 bits; the damage sites only read this resource).
    pub focus_health: Option<f32>,
    pub focus_taken: Arc<AtomicU32>,
}

impl Default for PlayerDefense {
    fn default() -> Self {
        PlayerDefense { suit: Suit::default(), taken_scale: 1.0, focus_health: None, focus_taken: Arc::new(AtomicU32::new(0)) }
    }
}

/// The player's damage-dealt scale (Quad Damage x the suit's 0x140b85200 scale), read by the weapons' damage events.
#[derive(Resource, Clone, Copy)]
pub struct DamageDealt {
    pub scale: f32,
}

impl Default for DamageDealt {
    fn default() -> Self {
        DamageDealt { scale: 1.0 }
    }
}

/// shared: one hit on the player, for every damage site (demon melee, projectiles): idPlayer::Damage's suit scale
/// (0x140b829d0, `Suit::damage_taken_scale`) then the health component's armour split (`Vitals::hit`). Reads and
/// writes the health the damage code keeps (`Demons::player_health`, else the HUD) and the HUD's armour.
/// During Saving Throw focus (past its invulnerable time) the same damage path sends a death blow to the focus
/// health in full (the unscaled damage) and the hit does nothing while that health stays above 0; once it is
/// gone the hit goes through. INTERIM: "death blow" = the hit would leave health at 0 after the armour split
/// (the path compares the scaled damage with a value before the split that was not traced).
pub fn hit_player(player_health: &mut Option<f32>, hud: Option<&mut HudState>, defense: Option<&PlayerDefense>, damage: f32) -> PlayerHit {
    let suit = defense.map(|d| &d.suit);
    let (health, armor, max_h, max_a) = match &hud {
        Some(h) => (player_health.unwrap_or(h.health), h.armor, h.max_health, h.max_armor),
        None => (player_health.unwrap_or(100.0), 0.0, 100.0, 50.0),
    };
    let comp = |cur: f32, max: f32, absorption: f32| pickups::HealthComponent { cur, max, drain_limit: max, starting: max, absorption };
    let mut v = Vitals { health: comp(health, max_h, 1.0), armor: comp(armor, max_a, suit.and_then(|s| s.armor_absorption).unwrap_or(1.0)) };
    let scale = suit.map_or(1.0, |s| s.damage_taken_scale(armor)) * defense.map_or(1.0, |d| d.taken_scale);
    if let Some(d) = defense
        && let Some(focus_health) = d.focus_health
        && scale > 0.0
        && { v }.hit(damage * scale).health.1 <= 0.0
    {
        let taken = f32::from_bits(d.focus_taken.load(Ordering::Relaxed)) + damage;
        d.focus_taken.store(taken.to_bits(), Ordering::Relaxed);
        if focus_health - taken > 0.0 {
            return PlayerHit { amount: 0.0, health: (health, health), armor: (armor, armor) };
        }
    }
    let hit = v.hit(damage * scale);
    *player_health = Some(v.health.cur);
    if let Some(h) = hud {
        h.health = v.health.cur;
        h.armor = v.armor.cur;
    }
    hit
}

impl Plugin for PickupsPlugin {
    fn build(&self, app: &mut App) {
        let db = DeclDb::new(self.container.clone());
        let difficulty = std::env::var("RANCHER_DIFFICULTY").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).min(4);
        let placed = placements(self.map_mode);
        let loot = match LootDropComponent::load(&db, rancher_sim::demons::POSSESSED) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("pickups: {e:#}");
                None
            }
        };
        let cv = |k: &str| self.cvars.0.get(k).and_then(|v| idres::decl::parse_number(v));
        let vitals = pickups::player_vitals(&db);
        app.insert_resource(Pickups {
            container: self.container.clone(),
            defs: HashMap::new(),
            props: Vec::new(),
            next_id: 0,
            difficulty,
            log: std::env::var("RANCHER_TRACE").is_ok_and(|v| v != "0"),
            elapsed: 0.0,
            vitals,
            loot,
            // INTERIM: gameLocal's random seed at map start was not traced.
            rng: IdRandom(0),
            alive: HashMap::new(),
            pending: Vec::new(),
            tracking: HashMap::new(),
            loot_enable: cv("loot_enable").unwrap_or(1.0) != 0.0,
            gravity: cv("g_gravity").unwrap_or(0.0),
            meshes: HashMap::new(),
            ammo_fraction: std::env::var("RANCHER_AMMO").ok().and_then(|v| v.parse().ok()),
            attract: attraction(&db),
            attract_add: cv("g_lootDropMovementDelayAddition").unwrap_or(0.0) as i32,
            placed,
            db,
            suit: Suit::default(),
            grants: upgrade_grants(),
            infinite: false,
            held: HashMap::new(),
            powerups: Powerups::default(),
            focus: None,
            focus_uses: 0,
            lifetime_cvar: cv("statusEffect_lifetimeSec").unwrap_or(0.0),
            dead_at: None,
            checkpoint: None,
            respawn_invulnerable_until: 0.0,
            respawn_invulnerable_sec: cv("g_invulnerabilityOnRespawnDurationSeconds").unwrap_or(0.0),
        })
        .init_resource::<PlayerDefense>()
        .init_resource::<DamageDealt>()
        .add_systems(Update, (setup_once, death_drops, spawn_drops, move_drops, touch).chain())
        .add_systems(Update, infinite_ammo.after(crate::combat::weapons_tick))
        .add_systems(Update, player_state.after(touch));
    }
}

/// RANCHER_UPGRADES: (perk name, mastered) per item; argent cells expand to one entry per cell.
fn upgrade_grants() -> Vec<(String, bool)> {
    let Ok(v) = std::env::var("RANCHER_UPGRADES") else { return Vec::new() };
    let mut out = Vec::new();
    for item in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((stat, n)) = item.split_once(':') {
            for _ in 0..n.trim().parse::<usize>().unwrap_or(1) {
                out.push((format!("argent:{}", stat.trim()), false));
            }
            continue;
        }
        let (name, mastered) = match item.strip_suffix('!') {
            Some(n) => (n, true),
            None => (item, false),
        };
        let name = if name.contains('/') { name.to_string() } else { format!("perk/zion/player/sp/enviroment_suit/{name}") };
        out.push((name, mastered));
    }
    out
}

/// RANCHER_PICKUPS: the test row (`1`) or `<def>@x,y[,z];...`.
fn placements(map_mode: bool) -> Vec<(String, V)> {
    let Ok(v) = std::env::var("RANCHER_PICKUPS") else { return Vec::new() };
    if v == "1" {
        if map_mode {
            return Vec::new();
        }
        // A row along +x from the range start (0, 0), 64 units apart, starting 96 ahead.
        return TEST_ROW.iter().enumerate().map(|(i, d)| (d.to_string(), V::new(96.0 + 64.0 * i as f32, 0.0, 0.0))).collect();
    }
    v.split(';')
        .filter_map(|p| {
            let (d, at) = p.split_once('@')?;
            let n: Vec<f32> = at.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (n.len() >= 2).then(|| (d.trim().to_string(), V::new(n[0], n[1], n.get(2).copied().unwrap_or(0.0))))
        })
        .collect()
}

impl Pickups {
    fn def(&mut self, name: &str) -> Option<Arc<PickupDef>> {
        if let Some(d) = self.defs.get(name) {
            return Some(d.clone());
        }
        match PickupDef::load(&self.db, name) {
            Ok(d) => {
                let d = Arc::new(d);
                self.defs.insert(name.to_string(), d.clone());
                Some(d)
            }
            Err(e) => {
                eprintln!("pickups: {name}: {e:#}");
                None
            }
        }
    }

    fn mesh(&mut self, model: &str, meshes: &mut Assets<Mesh>) -> Option<Handle<Mesh>> {
        if let Some(m) = self.meshes.get(model) {
            return m.clone();
        }
        let m = model_mesh(&self.container, model).map(|m| meshes.add(m));
        if m.is_none() {
            eprintln!("pickups: no mesh for {model}");
        }
        self.meshes.insert(model.to_string(), m.clone());
        m
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(&mut self, commands: &mut Commands, meshes: &mut Assets<Mesh>, mats: &mut Assets<StandardMaterial>, def: Arc<PickupDef>, origin: V, vel: Option<V>, lifetime: Option<f32>, tracked: f32) -> u32 {
        let entity = def.model.clone().and_then(|m| self.mesh(&m, meshes)).map(|mesh| {
            // Untextured: a flat colour per kind (the props' materials are not loaded here).
            let color = match &def.useable {
                Useable::Health(h) if h.items.iter().all(|i| i.component == pickups::Component::Armor) => Color::srgb(0.2, 0.8, 0.3),
                Useable::Health(_) => Color::srgb(0.3, 0.5, 1.0),
                Useable::Ammo(_) => Color::srgb(0.9, 0.7, 0.2),
                Useable::StatusEffect(_) => Color::srgb(0.9, 0.2, 0.9),
            };
            let s = def.model_scale;
            commands
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(mats.add(StandardMaterial { base_color: color, perceptual_roughness: 0.6, ..default() })),
                    Transform::from_translation(to_bevy(origin)).with_scale(Vec3::new(s.y, s.z, s.x)),
                ))
                .id()
        });
        let id = self.next_id;
        self.next_id += 1;
        if tracked > 0.0 {
            *self.tracking.entry(def.name.clone()).or_default() += tracked;
        }
        // idMovementThinkComponent init (0x1409b5d00): movementDelayTime = now + attractDelayMS + a random
        // 0..=g_lootDropMovementDelayAddition ms (gameLocal idRandom).
        let attract_at = vel.map(|_| {
            let add = if self.attract_add > 0 { (self.rng.random_int() % (self.attract_add as u32 + 1)) as i32 } else { 0 };
            self.elapsed + (self.attract.2 + add) as f32 / 1000.0
        });
        self.props.push(Prop { id, def, origin, vel, attract_at, spawned: self.elapsed, lifetime, tracked, entity, refused: false });
        id
    }

    fn untrack(&mut self, p: &Prop) {
        if p.tracked > 0.0
            && let Some(t) = self.tracking.get_mut(&p.def.name)
        {
            *t = (*t - p.tracked).max(0.0);
        }
    }
}

/// A static model's surfaces as one untextured mesh (idTech -> Bevy axes, winding flipped).
fn model_mesh(c: &Container, model: &str) -> Option<Mesh> {
    let res = idres::bmodel::resource_for_model(model)?;
    let bytes = c.read_by_name(&res).ok()?;
    let mut rd = idres::bmodel::SurfaceReader::new(&bytes).ok()?;
    let (mut pos, mut nrm, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    while let Ok(Some(s)) = rd.next_surface() {
        let base = pos.len() as u32;
        for v in &s.verts {
            pos.push(to_bevy(V::from(v.xyz)).to_array());
            nrm.push(to_bevy(V::from(v.normal_f32())).to_array());
        }
        for t in s.indices.chunks_exact(3) {
            idx.extend_from_slice(&[base + t[0] as u32, base + t[2] as u32, base + t[1] as u32]);
        }
    }
    (!pos.is_empty()).then(|| {
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
            .with_inserted_indices(Indices::U32(idx))
    })
}

/// First frame: the placed props, the HUD's maxima, RANCHER_AMMO.
#[allow(clippy::too_many_arguments)]
fn setup_once(
    mut p: ResMut<Pickups>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut hud: Option<ResMut<HudState>>,
    mut combat: Option<ResMut<Combat>>,
    mut defense: ResMut<PlayerDefense>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    *done = true;
    let p = &mut *p;
    grant_upgrades(p, &mut hud, &mut combat);
    defense.suit = p.suit.clone();
    if let Some(h) = hud.as_mut() {
        h.max_health = p.vitals.health.max;
        h.max_armor = p.vitals.armor.max;
    }
    if let (Some(f), Some(c)) = (p.ammo_fraction, combat.as_mut()) {
        for pool in &mut c.arsenal.pools {
            pool.count = (pool.max as f32 * f) as i32;
        }
    }
    for (name, at) in std::mem::take(&mut p.placed) {
        if let Some(def) = p.def(&name) {
            let id = p.spawn(&mut commands, &mut meshes, &mut mats, def, at, None, None, 0.0);
            if p.log {
                println!("[pickup {id}] placed {name} at {at}");
            }
        }
    }
}

/// A weapon inventory decl as the upgrade packs name it.
fn weapon_key(decl: &str) -> String {
    let d = decl.to_lowercase();
    if d.starts_with("weapon/") { d } else { format!("weapon/{d}") }
}

/// RANCHER_UPGRADES: gives each perk to the suit (argent cells: the five per-difficulty perks of the stat, of which
/// the current difficulty's applies), applying capacity to the HUD's health / armour and the arsenal's pools.
fn grant_upgrades(p: &mut Pickups, hud: &mut Option<ResMut<HudState>>, combat: &mut Option<ResMut<Combat>>) {
    if p.grants.is_empty() {
        return;
    }
    let weapon_pools: Vec<WeaponPool> = combat
        .as_ref()
        .map(|c| {
            c.arsenal
                .defs
                .iter()
                .filter_map(|d| {
                    let pool = c.arsenal.pools.iter().find(|pl| pl.key == d.ammo_pool)?;
                    Some(WeaponPool { weapon: weapon_key(&d.decl), pool: pool.key.clone(), base_max: pool.max })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut vitals = read_vitals(&p.vitals, None, hud.as_ref().map(|h| (h.health, h.armor)));
    let mut pools = combat.as_ref().map(|c| c.arsenal.pools.clone()).unwrap_or_default();
    for (name, mastered) in std::mem::take(&mut p.grants) {
        let perks: Vec<String> = match name.strip_prefix("argent:") {
            Some(stat) => upgrades::argent_perks(stat).to_vec(),
            None => vec![name.clone()],
        };
        for perk in perks {
            let pk = match Perk::load(&p.db, &perk) {
                Ok(pk) => pk,
                Err(e) => {
                    eprintln!("upgrades: {perk}: {e:#}");
                    continue;
                }
            };
            let got = p.suit.grant(&pk, p.difficulty, &mut vitals, &mut pools, &weapon_pools);
            if mastered {
                for e in pk.upgrades.iter().flat_map(|u| &u.equip) {
                    p.suit.mastered.push(e.kind.clone());
                }
            }
            if p.log && (!got.is_empty() || pk.difficulty.is_none()) {
                println!("[upgrade] {perk} ({:?}{}): {got:?}", pk.kind, if mastered { ", mastered" } else { "" });
            }
        }
    }
    p.vitals.health.max = vitals.health.max;
    p.vitals.armor.max = vitals.armor.max;
    if let Some(h) = hud.as_mut() {
        h.health = vitals.health.cur;
        h.armor = vitals.armor.cur;
    }
    if let Some(c) = combat.as_mut() {
        c.arsenal.pools = pools;
    }
    if p.log {
        println!("[upgrade] health {:.0}/{:.0}, armour {:.0}/{:.0}, abilities not applied here: {:?}", vitals.health.cur, vitals.health.max, vitals.armor.cur, vitals.armor.max, p.suit.abilities);
    }
}

/// INTERIM: seconds from death to the range's checkpoint reload (the campaign shows GUI_SCREEN_CAMPAIGN_DEATH and
/// reloads the checkpoint; that screen's timing was not decoded). RANCHER_RESPAWN_SEC overrides.
const RESPAWN_SEC: f32 = 3.0;

/// Per frame (BOARD row 26): powerup timers and their effects (movement speed, damage taken / dealt), the Saving Throw
/// focus, and player death -> range checkpoint reload (the menu's RestartLevel, plus the vitals / pools the range
/// started with).
#[allow(clippy::too_many_arguments)]
fn player_state(
    mut p: ResMut<Pickups>,
    mut sim: ResMut<crate::Sim>,
    mut demons: Option<ResMut<Demons>>,
    mut hud: Option<ResMut<HudState>>,
    mut combat: Option<ResMut<Combat>>,
    mut sound: Option<ResMut<Sound>>,
    (mut defense, mut dealt): (ResMut<PlayerDefense>, ResMut<DamageDealt>),
    mut restart: MessageWriter<crate::settings::MenuAction>,
    time: Res<Time>,
) {
    let p = &mut *p;
    let now = p.elapsed;
    let now_ms = (now * 1000.0) as i32;
    let mut vitals = read_vitals(&p.vitals, demons.as_ref().and_then(|d| d.player_health), hud.as_ref().map(|h| (h.health, h.armor)));
    if p.checkpoint.is_none() && hud.is_some() {
        p.checkpoint = Some((vitals, combat.as_ref().map(|c| c.arsenal.pools.clone()).unwrap_or_default()));
    }
    let before = vitals;
    let mut post = |name: &Option<String>| {
        if let (Some(s), Some(n)) = (sound.as_mut(), name) {
            s.post(n, None, None);
        }
    };
    if p.dead_at.is_none() {
        for e in p.powerups.tick((time.delta_secs() * 1000.0) as i32) {
            match &e {
                EffectEvent::RunningOut { sound, .. } => post(sound),
                EffectEvent::Ended { sound, .. } => post(sound),
            }
            if p.log {
                println!("[powerup] {e:?}");
            }
        }
    }
    // Saving Throw focus: death blows went to its health (hit_player); it ends at its time, once that health is
    // gone (the last blow went through), or once the player's health reaches focusDisablesAtHealth.
    let taken = f32::from_bits(defense.focus_taken.swap(0, Ordering::Relaxed));
    if let Some(f) = p.focus.as_mut() {
        if taken > 0.0 {
            f.health -= taken;
            if p.log {
                println!("[focus] death blow {taken:.1} to the focus health, now {:.1}", f.health);
            }
        }
        if now_ms >= f.ends_ms || f.health <= 0.0 || vitals.health.cur >= f.disables_at_health {
            if p.log {
                println!("[focus] ends (health {:.0})", vitals.health.cur);
            }
            p.focus = None;
        }
    }
    if p.dead_at.is_none() && vitals.health.cur <= 0.0 {
        match SavingThrow::from_suit(&p.suit) {
            Some(st) if p.focus.is_none() && p.focus_uses < st.max_uses => {
                p.focus = Some(st.enter(now_ms));
                p.focus_uses += 1;
                vitals.health.cur = st.left_over_health;
                if p.log {
                    println!("[focus] Saving Throw: health -> {:.0}, focus health {:.0} for {} ms, invulnerable {} ms", vitals.health.cur, st.max_health, st.duration_ms, st.invulnerable_ms);
                }
            }
            _ => {
                p.dead_at = Some(now);
                p.focus = None;
                for e in p.powerups.clear() {
                    if let EffectEvent::Ended { sound, .. } = &e {
                        post(sound);
                    }
                }
                if p.log {
                    println!("[player] died (armour {:.0}); range checkpoint reload in {:.1} s", vitals.armor.cur, respawn_sec());
                }
            }
        }
    }
    if let Some(t) = p.dead_at
        && now - t >= respawn_sec()
    {
        restart.write(crate::settings::MenuAction::RestartLevel);
        p.dead_at = None;
        p.focus_uses = 0;
        p.respawn_invulnerable_until = now + p.respawn_invulnerable_sec;
        if let Some((v, pools)) = p.checkpoint.clone() {
            vitals = v;
            if let Some(c) = combat.as_mut() {
                c.arsenal.pools = pools;
            }
        }
        if p.log {
            println!("[player] respawned at the range start: health {:.0}, armour {:.0}, invulnerable {:.1} s", vitals.health.cur, vitals.armor.cur, p.respawn_invulnerable_sec);
        }
    }
    // Effects: movement (dead: none), damage taken / dealt.
    let dead = p.dead_at.is_some();
    let speed = if dead { 0.0 } else { p.powerups.move_speed_scale() };
    if p.log && speed != sim.player.physics.speed_scale {
        println!("[powerup] movement speed scale {:.2} -> {speed:.2}", sim.player.physics.speed_scale);
    }
    sim.player.physics.speed_scale = speed;
    if dead {
        // INTERIM: PM_DEAD stands in as no movement or jumping until the reload recreates the player.
        sim.player.physics.jump_scale = 0.0;
    }
    let focus_invulnerable = p.focus.is_some_and(|f| now_ms < f.invulnerable_until_ms);
    defense.focus_health = p.focus.filter(|f| !focus_invulnerable && f.health > 0.0).map(|f| f.health);
    defense.taken_scale = if focus_invulnerable || now < p.respawn_invulnerable_until { 0.0 } else { p.powerups.damage_taken_scale() };
    let scale = p.powerups.damage_dealt_scale() * p.suit.damage_dealt_scale(&vitals, false);
    if p.log && scale != dealt.scale {
        println!("[powerup] damage dealt scale {:.2} -> {scale:.2}, taken scale {:.2}", dealt.scale, defense.taken_scale);
    }
    dealt.scale = scale;
    if vitals != before {
        write_vitals(&vitals, &mut demons, &mut hud);
    }
}

fn respawn_sec() -> f32 {
    std::env::var("RANCHER_RESPAWN_SEC").ok().and_then(|v| v.parse().ok()).unwrap_or(RESPAWN_SEC)
}

/// Rich Get Richer: while the suit's infinite-ammo condition holds (0x140b86f50), idWeapon::UseAmmo (0x140f29cf0)
/// spends nothing; here the pools are put back to what they held when the flag came on (after the weapons' tick).
fn infinite_ammo(mut p: ResMut<Pickups>, demons: Option<Res<Demons>>, hud: Option<Res<HudState>>, combat: Option<ResMut<Combat>>) {
    let p = &mut *p;
    let Some(mut combat) = combat else { return };
    let vitals = read_vitals(&p.vitals, demons.as_ref().and_then(|d| d.player_health), hud.as_ref().map(|h| (h.health, h.armor)));
    // INTERIM: no combat level in the testbed (only IAM_COMBAT_LEVEL reads it).
    let on = p.suit.infinite_ammo(&vitals, 0.0, 0.0);
    if on != p.infinite && p.log {
        println!("[upgrade] infinite ammo {} (armour {:.0}, health {:.0})", if on { "on" } else { "off" }, vitals.armor.cur, vitals.health.cur);
    }
    p.infinite = on;
    for pool in &mut combat.arsenal.pools {
        match p.held.get(&pool.key) {
            Some(&held) if on && pool.count < held => pool.count = held,
            _ => {
                p.held.insert(pool.key.clone(), pool.count);
            }
        }
    }
}

/// The player's vitals as the damage code (`player_health`) and the HUD (health, armour) hold them.
fn read_vitals(base: &Vitals, player_health: Option<f32>, hud: Option<(f32, f32)>) -> Vitals {
    let mut v = *base;
    v.health.cur = player_health.or(hud.map(|h| h.0)).unwrap_or(v.health.starting);
    v.armor.cur = hud.map(|h| h.1).unwrap_or(v.armor.starting);
    v
}

fn write_vitals(v: &Vitals, demons: &mut Option<ResMut<Demons>>, hud: &mut Option<ResMut<HudState>>) {
    if let Some(d) = demons.as_mut() {
        d.player_health = Some(v.health.cur);
    }
    if let Some(h) = hud.as_mut() {
        h.health = v.health.cur;
        h.armor = v.armor.cur;
    }
}

/// A demon that was alive last frame and has no hit volumes now died: idLootDropComponent::DropLoot for a normal
/// kill, or a glory kill (demons::Demons::kill_kind; chainsaw kills are not in the testbed yet).
fn death_drops(mut p: ResMut<Pickups>, targets: Option<Res<DemonTargets>>, demons: Option<Res<Demons>>, hud: Option<Res<HudState>>, combat: Option<Res<Combat>>, time: Res<Time>) {
    let p = &mut *p;
    p.elapsed += time.delta_secs();
    let Some(targets) = targets else { return };
    let now: HashMap<u32, (V, V)> = targets.demons.iter().map(|t| (t.id, (t.origin, (t.bounds.0 + t.bounds.1) * 0.5))).collect();
    let dead: Vec<(u32, (V, V))> = p.alive.iter().filter(|(id, _)| !now.contains_key(id)).map(|(id, v)| (*id, *v)).collect();
    p.alive = now;
    if !p.loot_enable {
        return;
    }
    let vitals = read_vitals(&p.vitals, demons.as_ref().and_then(|d| d.player_health), hud.as_ref().map(|h| (h.health, h.armor)));
    let pools = combat.as_ref().map(|c| c.arsenal.pools.clone()).unwrap_or_default();
    for (id, (origin, centre)) in dead {
        let Some(mut comp) = p.loot.clone() else { continue };
        let mut rng = p.rng;
        let tracking = p.tracking.clone();
        let suit = p.suit.clone();
        // The kill kind for dropRestriction: a glory kill when the demon died from its sync's ae_kill.
        let kind = demons.as_ref().map_or(KillKind::Normal, |dm| dm.kill_kind(id));
        let drops = comp.drop_loot(kind, &suit, &mut rng, |d| {
            let pending = if d.entity_def.is_empty() { 0.0 } else { tracking.get(&d.entity_def).copied().unwrap_or(0.0) };
            let h = rancher_sim::pickups::loot::holding(d, &vitals, &pools, pending, &suit);
            if p.log {
                match h {
                    Some((per, Holding { cur, max, max_drop })) => println!("[loot {id}] {}: per item {per}, have {cur:.0} / {max:.0}, maxDrop {max_drop}", d.name),
                    None => println!("[loot {id}] {}: not for this player", d.name),
                }
            }
            h
        });
        p.rng = rng;
        if p.log {
            println!("[loot {id}] died at {origin}: {} drop(s)", drops.len());
        }
        let at = p.elapsed;
        for d in drops {
            if p.log {
                println!("[loot {id}] drop {} ({} per item) in {} ms, removed after {} ms", d.entity_def, d.per_item, d.delay_ms, d.removal_ms);
            }
            // INTERIM: the dropEntity handler (spawn point, launch) was not found; items start at the dead demon's hit
            // volume centre plus explicitSpawnOffset.
            let o = V::from(d.spawn_offset);
            p.pending.push(Pending { at: at + d.delay_ms as f32 / 1000.0, def: d.entity_def, origin: V::new(origin.x, origin.y, centre.z) + o, lifetime: d.removal_ms as f32 / 1000.0, per_item: d.per_item });
        }
    }
}

/// Fires due dropEntity events: the item spawns with the pinata launch (propAttribs base/pinata/aidrop).
fn spawn_drops(mut p: ResMut<Pickups>, mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let p = &mut *p;
    let now = p.elapsed;
    let due: Vec<Pending> = {
        let (due, rest) = std::mem::take(&mut p.pending).into_iter().partition(|d| d.at <= now);
        p.pending = rest;
        due
    };
    for d in due {
        let Some(def) = p.def(&d.def) else { continue };
        let vel = pinata_velocity(&p.db, &mut p.rng);
        let id = p.spawn(&mut commands, &mut meshes, &mut mats, def, d.origin, Some(vel), Some(d.lifetime), d.per_item as f32);
        if p.log {
            println!("[pickup {id}] dropped {} at {} vel {vel}", d.def, d.origin);
        }
    }
}

/// INTERIM launch: propAttribs base/pinata/aidrop randomVelocity (randomVelocityMin..Max) along a direction drawn
/// per axis in randomVelocityDirMin..Max and normalized; the moveable code that applies them was not decoded.
fn pinata_velocity(db: &DeclDb, rng: &mut IdRandom) -> V {
    let a = db.get("propattribs", "base/pinata/aidrop").ok();
    let f = |k: &str, d: f32| a.as_ref().and_then(|b| b.f32(&format!("edit.{k}"))).unwrap_or(d);
    let mut r = |lo: f32, hi: f32| lo + (hi - lo) * (rng.random_int() as f32 / 32767.0);
    let dir = V::new(
        r(f("randomVelocityDirMin.x", 0.0), f("randomVelocityDirMax.x", 0.0)),
        r(f("randomVelocityDirMin.y", 0.0), f("randomVelocityDirMax.y", 0.0)),
        r(f("randomVelocityDirMin.z", 0.0), f("randomVelocityDirMax.z", 0.0)),
    );
    dir.normalize_or_zero() * r(f("randomVelocityMin", 0.0), f("randomVelocityMax", 0.0))
}

/// The dropped items' idMovementThinkComponent data: propMoveable pinata/default `attactMovement` over the
/// idDeclProp_MovementComponent ctor defaults (0x1406f4c60: attraction on, speed 100, radius 150, delay 2000 ms).
fn attraction(db: &DeclDb) -> (f32, f32, i32) {
    let a = db.get("propmoveable", "pinata/default").ok();
    let f = |k: &str, d: f32| a.as_ref().and_then(|b| b.f32(&format!("edit.attactMovement.{k}"))).unwrap_or(d);
    (f("attractSpeed", 100.0), f("attractRadius", 150.0), f("attractDelayMS", 2000.0) as i32)
}

/// Dropped items fly under gravity until they land; from their movementDelayTime on they move toward the player at
/// attractSpeed while the player is within attractRadius. INTERIM: no bounce, and the attraction is a straight line
/// at that speed (the think 0x1409ba390 with its orbit / swarm angles is not decoded).
fn move_drops(mut p: ResMut<Pickups>, sim: Res<crate::Sim>, time: Res<Time>, mut tf: Query<&mut Transform>) {
    let p = &mut *p;
    let dt = time.delta_secs();
    let g = p.gravity;
    // Vacuum (EQUIP_INCREASE_DROP_RADIUS) scales the radius and speed (popup 0x1409bcde0).
    let (radius, speed) = p.suit.drop_attraction(p.attract.1, p.attract.0);
    let target = sim.player.physics.origin + V::Z * (sim.player.cfg.normal_height * 0.5);
    let now = p.elapsed;
    for prop in p.props.iter_mut() {
        let Some(mut v) = prop.vel else { continue };
        if prop.attract_at.is_some_and(|t| now >= t) && prop.origin.distance(target) <= radius && speed > 0.0 {
            let to = target - prop.origin;
            let step = (speed * dt).min(to.length());
            prop.origin += to.normalize_or_zero() * step;
        } else if v != V::ZERO {
            v.z -= g * dt;
            let d = v * dt;
            match sim.world.ray(prop.origin, d.normalize_or_zero(), d.length() + 0.25) {
                Some((dist, _, _)) => {
                    prop.origin += d.normalize_or_zero() * (dist - 0.25).max(0.0);
                    v = V::ZERO;
                }
                None => prop.origin += d,
            }
            prop.vel = Some(v);
        }
        if let Some(e) = prop.entity
            && let Ok(mut t) = tf.get_mut(e)
        {
            t.translation = to_bevy(prop.origin);
        }
    }
}

/// Touch: the player's clip box against each prop's trigger; a usable prop is used and removed. Drops past their
/// removal time disappear.
#[allow(clippy::too_many_arguments)]
fn touch(mut p: ResMut<Pickups>, mut commands: Commands, sim: Res<crate::Sim>, mut demons: Option<ResMut<Demons>>, mut hud: Option<ResMut<HudState>>, mut combat: Option<ResMut<Combat>>, mut sound: Option<ResMut<Sound>>) {
    let p = &mut *p;
    let pl = &sim.player;
    let (w, po) = (pl.cfg.bbox_width * 0.5, pl.physics.origin);
    let (lo, hi) = (po + V::new(-w, -w, 0.0), po + V::new(w, w, pl.cfg.normal_height));
    let now = p.elapsed;
    let mut vitals = read_vitals(&p.vitals, demons.as_ref().and_then(|d| d.player_health), hud.as_ref().map(|h| (h.health, h.armor)));
    let mut removed = Vec::new();
    // Touching but unusable (CanUse false or nothing changed): logged once until the player steps off.
    let (mut refused, mut stepped_off) = (Vec::new(), Vec::new());
    for (k, prop) in p.props.iter().enumerate() {
        if prop.lifetime.is_some_and(|l| now - prop.spawned >= l) {
            removed.push((k, None));
            continue;
        }
        if !prop.def.trigger.touches(prop.origin, lo, hi) {
            stepped_off.push(k);
            continue;
        }
        let msg = match &prop.def.useable {
            Useable::Health(h) => {
                let changes = if h.can_use(&vitals) { h.apply(&mut vitals, prop.def.scale, p.difficulty) } else { Vec::new() };
                if changes.is_empty() {
                    refused.push(k);
                    continue;
                }
                let parts: Vec<String> = changes.iter().map(|c| format!("{:?} {:.0} -> {:.0}", c.component, c.before, c.after)).collect();
                (h.sound.clone(), h.effect.clone(), parts.join(", "))
            }
            Useable::Ammo(a) => {
                let Some(c) = combat.as_mut() else { continue };
                let suit_scale = match prop.def.scale {
                    ScaleType::Dropped => p.suit.ammo_drop_scale(),
                    ScaleType::Pickup => p.suit.ammo_pickup_scale(),
                    _ => 1.0,
                };
                let Some((before, after)) = a.apply(&mut c.arsenal.pools, prop.def.scale, p.difficulty, suit_scale) else {
                    refused.push(k);
                    continue;
                };
                (a.sound.clone(), None, format!("{} {before} -> {after}", a.pool))
            }
            Useable::StatusEffect(ps) => {
                if p.dead_at.is_some() {
                    continue;
                }
                let applied = p.powerups.apply(&ps.effect, &p.suit, p.lifetime_cvar);
                if applied == Applied::Rejected {
                    refused.push(k);
                    continue;
                }
                // Praetor suit: a powerup fills health / armour on entering it (ABILITY_MOD_POWERUP_HEALTH / ARMOR).
                let mut fill = String::new();
                if ps.effect.is_powerup {
                    for (m, c) in [(&p.suit.powerup_health, &mut vitals.health), (&p.suit.powerup_armor, &mut vitals.armor)] {
                        if let Some(m) = m {
                            let (value, on_enter, _) = if m.kind.ends_with("HEALTH") { m.health } else { m.armor };
                            if on_enter {
                                let v = if value < 0 { c.max } else { value as f32 };
                                fill.push_str(&format!(" {} {:.0} -> {:.0}", m.kind, c.cur, c.cur.max(v)));
                                c.cur = c.cur.max(v);
                            }
                        }
                    }
                }
                (ps.pickup_sound.clone().or(ps.effect.start_sound.clone()), ps.effect.start_fx.clone(), format!("{} {applied:?}{fill}", ps.effect.name))
            }
        };
        removed.push((k, Some(msg)));
    }
    for k in stepped_off {
        p.props[k].refused = false;
    }
    for k in refused {
        let prop = &mut p.props[k];
        if !prop.refused && p.log {
            println!("[pickup {}] {} touched, not usable (health {:.0}/{:.0}, armour {:.0}/{:.0})", prop.id, prop.def.name, vitals.health.cur, vitals.health.max, vitals.armor.cur, vitals.armor.max);
        }
        prop.refused = true;
    }
    if removed.is_empty() {
        return;
    }
    write_vitals(&vitals, &mut demons, &mut hud);
    for (k, msg) in removed.into_iter().rev() {
        let prop = p.props.remove(k);
        p.untrack(&prop);
        if let Some(e) = prop.entity {
            commands.entity(e).despawn();
        }
        match msg {
            Some((snd, fx, what)) => {
                if let (Some(s), Some(name)) = (sound.as_mut(), snd.as_ref()) {
                    s.post(name, None, None);
                }
                if p.log {
                    println!("[pickup {}] {} picked up: {what}; sound {snd:?} fx {fx:?}", prop.id, prop.def.name);
                }
            }
            None if p.log => println!("[pickup {}] {} removed (timeout)", prop.id, prop.def.name),
            None => {}
        }
    }
}
