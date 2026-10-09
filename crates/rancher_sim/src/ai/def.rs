//! A demon's AI decls from the install: perception (aiEditable.perception over the idDeclAISensorySettings ctor
//! defaults), movement constants, the attack graph and the melee damage events of its attack anims. Addresses and
//! decl paths in gamedata/re/AI.md.

use std::collections::HashMap;

use anyhow::{Context, Result};
use idres::Container;
use idres::decl::Block;
use idres::decldb::DeclDb;
use idres::md6def::Md6DefDecl;

use super::attack::AttackGraph;

/// idAIPerception (0x3c bytes; live copy at idAI2 + 0xbe38). Defaults: idDeclAISensorySettings ctor 0x1406f3680
/// (the struct at decl + 0x70).
#[derive(Debug, Clone, PartialEq)]
pub struct Perception {
    /// Max distance at which the AI perceives other actors.
    pub actor_radius: f32,
    pub obstacle_radius: f32,
    /// -1 = infinite.
    pub actor_refresh_radius: f32,
    /// Radius of the close field of view.
    pub close_radius: f32,
    /// Degrees (full angle).
    pub fov: f32,
    /// Field of view for an actor already sensed.
    pub fov_focused: f32,
    /// Field of view inside `close_radius`.
    pub fov_close: f32,
    /// Seconds to sight a fully exposed enemy.
    pub exposed_sight_time: f32,
    pub hearing_stimulus: f32,
    pub event_radius: f32,
    pub auto_focus_min_ms: i16,
    pub auto_focus_max_ms: i16,
    pub sense_updates_on_non_enemies: bool,
}

impl Default for Perception {
    /// 0x1406f3680: 2048 / 1024 / -1 / 256 / 180 / 360 / 300 / 0.3 / 1.0 / 2048 / 800 / 1200 / true.
    fn default() -> Self {
        Perception {
            actor_radius: 2048.0,
            obstacle_radius: 1024.0,
            actor_refresh_radius: -1.0,
            close_radius: 256.0,
            fov: 180.0,
            fov_focused: 360.0,
            fov_close: 300.0,
            exposed_sight_time: 0.3,
            hearing_stimulus: 1.0,
            event_radius: 2048.0,
            auto_focus_min_ms: 800,
            auto_focus_max_ms: 1200,
            sense_updates_on_non_enemies: true,
        }
    }
}

impl Perception {
    /// Overlays a decl `perception` block (aiEditable.perception / aiSensorySettings edit.data).
    pub fn overlay(&mut self, b: &Block) {
        let f = |k: &str, v: &mut f32| {
            if let Some(x) = b.f32(k) {
                *v = x;
            }
        };
        f("actorPerceptionRadius", &mut self.actor_radius);
        f("obstaclePerceptionRadius", &mut self.obstacle_radius);
        f("actorRefreshRadius", &mut self.actor_refresh_radius);
        f("closePerceptionRadius", &mut self.close_radius);
        f("fieldOfView.value", &mut self.fov);
        f("fieldOfView_focused.value", &mut self.fov_focused);
        f("fieldOfView_close.value", &mut self.fov_close);
        f("exposedSightTime", &mut self.exposed_sight_time);
        f("hearingStimulus", &mut self.hearing_stimulus);
        f("eventPerceptionRadius", &mut self.event_radius);
        if let Some(v) = b.f32("autoFocusMinTime") {
            self.auto_focus_min_ms = v as i16;
        }
        if let Some(v) = b.f32("autoFocusMaxTime") {
            self.auto_focus_max_ms = v as i16;
        }
        if let Some(v) = b.path("senseUpdatesOnNonEnemies").and_then(|v| v.as_bool()) {
            self.sense_updates_on_non_enemies = v;
        }
    }
}

/// A webNodeBadZoneInfoMappings bad zone: while the body / move angle is inside `angle`, turning is limited.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BadZone {
    pub angle: (f32, f32),
    pub max_move_turn_rate: (f32, f32),
    pub max_body_turn_rate: (f32, f32),
}

/// aiConstants.movement.
#[derive(Debug, Clone, PartialEq)]
pub struct Movement {
    /// AAS type the AI paths on ("aas_monster48").
    pub aas_name: String,
    pub decel_rate: f32,
    /// Degrees.
    pub alignment_tolerance: f32,
    pub allow_strafing: bool,
    pub min_departure_distance: f32,
    pub min_arrival_distance: f32,
    /// Web node -> bad zones (hands_combat/walk for the Possessed).
    pub bad_zones: Vec<(String, Vec<BadZone>)>,
}

/// One melee damage window of an attack anim: md6Def ae_startSphereModelTrace(WithAutoStop) .. ae_endSphereModelTrace.
#[derive(Debug, Clone, PartialEq)]
pub struct MeleeWindow {
    pub anim: String,
    /// Anim frames (the anim's own frame rate).
    pub start_frame: f32,
    pub end_frame: f32,
    /// md6Def joint group whose spheres sweep (e.g. "left_arm").
    pub joint_group: String,
    pub damage_decl: String,
}

/// damage decl damageParms the melee hit needs.
#[derive(Debug, Clone, PartialEq)]
pub struct MeleeDamage {
    pub decl: String,
    pub min_damage: f32,
    pub max_damage: f32,
    /// Scale applied when the victim is the player.
    pub player_damage_scale: f32,
}

#[derive(Debug, Clone)]
pub struct AiDef {
    pub entity: String,
    pub perception: Perception,
    /// actorConstants.perception.eyeOffset.z.
    pub eye_height: f32,
    /// Max x / y half extent of clipModelInfo.size (bounds radius 0x140287600).
    pub clip_half_width: f32,
    pub movement: Movement,
    pub anim_web: String,
    pub attacks: AttackGraph,
    /// Anim path -> melee windows, for every anim under the attack graph's via nodes.
    pub melee_windows: HashMap<String, Vec<MeleeWindow>>,
    pub damage: HashMap<String, MeleeDamage>,
}

fn range(b: &Block, key: &str) -> (f32, f32) {
    (b.f32(&format!("{key}.minRange")).unwrap_or(0.0), b.f32(&format!("{key}.maxRange")).unwrap_or(0.0))
}

/// `item[N]` children of a list block in index order.
pub fn items(b: &Block) -> Vec<&Block> {
    let mut v: Vec<(usize, &Block)> = b
        .items
        .iter()
        .filter_map(|(k, v)| {
            let i = k.strip_prefix("item[")?.strip_suffix(']')?.parse().ok()?;
            Some((i, v.as_block()?))
        })
        .collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, b)| b).collect()
}

impl AiDef {
    pub fn load(db: &DeclDb, entity: &str) -> Result<AiDef> {
        let e = db.get("entitydef", entity).with_context(|| format!("entityDef {entity}"))?;
        let edit = e.block("edit").context("entityDef without edit")?;
        let mut perception = Perception::default();
        if let Some(p) = edit.block("aiEditable.perception") {
            perception.overlay(p);
        }
        let eye_height = edit.f32("actorConstants.perception.eyeOffset.z").unwrap_or(0.0);
        let clip_half_width = edit.f32("clipModelInfo.size.x").unwrap_or(0.0).max(edit.f32("clipModelInfo.size.y").unwrap_or(0.0)) * 0.5;
        let mv = edit.block("aiConstants.movement");
        let mut bad_zones = Vec::new();
        if let Some(m) = mv.and_then(|m| m.block("webNodeBadZoneInfoMappings")) {
            for it in items(m) {
                let zones: Vec<BadZone> = it
                    .block("badZones")
                    .map(|z| {
                        items(z)
                            .into_iter()
                            .map(|z| BadZone {
                                angle: range(z, "angleRange"),
                                max_move_turn_rate: range(z, "maxMoveTurnRates"),
                                max_body_turn_rate: range(z, "maxBodyTurnRates"),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for n in it.block("webNodes").map(items_str).unwrap_or_default() {
                    bad_zones.push((n, zones.clone()));
                }
            }
        }
        let movement = Movement {
            aas_name: mv.and_then(|m| m.str("aasName")).unwrap_or("").to_string(),
            decel_rate: mv.and_then(|m| m.f32("decelRate")).unwrap_or(0.0),
            alignment_tolerance: mv.and_then(|m| m.f32("alignmentTolerance.value")).unwrap_or(0.0),
            allow_strafing: mv.and_then(|m| m.path("allowStrafing")).and_then(|v| v.as_bool()).unwrap_or(false),
            min_departure_distance: mv.and_then(|m| m.f32("minDepartureDistance")).unwrap_or(0.0),
            min_arrival_distance: mv.and_then(|m| m.f32("minArrivalDistance")).unwrap_or(0.0),
            bad_zones,
        };
        let anim_web = edit.str("aiConstants.animation.animWebs.animWebs[0]").unwrap_or("").to_string();
        let graph = edit.str("aiEditable.behaviors.attackGraph").context("no attackGraph")?;
        let attacks = AttackGraph::load(db, graph)?;
        let model = edit.str("renderModelInfo.model").context("no model")?;
        let md6 = load_md6def(db.container(), model)?;
        let web_src = db.container().read_by_name(&format!("generated/decls/animweb/{anim_web}.decl"))?;
        let web = idres::animweb::AnimWeb::parse(&String::from_utf8_lossy(&web_src))?;
        let mut melee_windows = HashMap::new();
        let mut damage = HashMap::new();
        for a in attacks.subgraphs.iter().flat_map(|s| s.attacks.iter()) {
            let Some((sub, state)) = a.via_node.strip_prefix(&format!("{anim_web}/")).and_then(|r| r.split_once('/')) else {
                continue;
            };
            let Some(node) = web.sub_web(sub).and_then(|s| s.node(state)) else { continue };
            for alias in node.trees.iter().flat_map(|t| t.anims.iter()) {
                let w = melee_windows_of(&md6, &alias.name);
                for mw in &w {
                    if !damage.contains_key(&mw.damage_decl) {
                        damage.insert(mw.damage_decl.clone(), MeleeDamage::load(db, &mw.damage_decl)?);
                    }
                }
                if !w.is_empty() {
                    melee_windows.insert(alias.name.clone(), w);
                }
            }
        }
        Ok(AiDef {
            entity: entity.to_string(),
            perception,
            eye_height,
            clip_half_width,
            movement,
            anim_web,
            attacks,
            melee_windows,
            damage,
        })
    }
}

fn items_str(b: &Block) -> Vec<String> {
    let mut v: Vec<(usize, String)> = b
        .items
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("item[")?.strip_suffix(']')?.parse().ok()?, v.as_str()?.to_string())))
        .collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, s)| s).collect()
}

impl MeleeDamage {
    pub fn load(db: &DeclDb, decl: &str) -> Result<MeleeDamage> {
        let b = db.get("damage", decl).with_context(|| format!("damage {decl}"))?;
        let p = b.block("edit.damageParms").context("damage decl without damageParms")?;
        Ok(MeleeDamage {
            decl: decl.to_string(),
            min_damage: p.f32("minDamage").unwrap_or(0.0),
            max_damage: p.f32("maxDamage").unwrap_or(0.0),
            player_damage_scale: p.f32("playerDamageScale").unwrap_or(1.0),
        })
    }
}

/// The md6Def of `model` with its inherit chain merged.
pub fn load_md6def(c: &Container, model: &str) -> Result<Md6DefDecl> {
    let mut chain = Vec::new();
    let mut name = Some(model.to_string());
    while let Some(n) = name.take() {
        let text = String::from_utf8_lossy(&c.read_by_name(&format!("generated/decls/md6def/{n}.decl"))?).into_owned();
        let d = Md6DefDecl::parse(&text).with_context(|| format!("md6Def {n}"))?;
        name = d.inherit.clone().filter(|s| !s.is_empty() && chain.len() < 8);
        chain.push(d);
    }
    let mut out = chain.pop().unwrap_or_default();
    while let Some(child) = chain.pop() {
        out = out.merged_with(&child);
    }
    Ok(out)
}

/// ae_startSphereModelTrace* { string group, damage decl, ... } .. ae_endSphereModelTrace pairs of one anim.
fn melee_windows_of(md6: &Md6DefDecl, anim: &str) -> Vec<MeleeWindow> {
    let Some(ev) = md6.events.get(anim) else { return Vec::new() };
    let mut out = Vec::new();
    for e in ev.iter().filter(|e| e.name.starts_with("ae_startSphereModelTrace")) {
        let group = e.param("string").and_then(|a| a.text()).unwrap_or("").to_string();
        let Some(decl) = e.param("damage").and_then(|a| a.text()) else { continue };
        let end = ev
            .iter()
            .filter(|x| x.name == "ae_endSphereModelTrace" && x.frame >= e.frame)
            .map(|x| x.frame)
            .fold(f32::INFINITY, f32::min);
        out.push(MeleeWindow {
            anim: anim.to_string(),
            start_frame: e.frame,
            end_frame: end,
            joint_group: group,
            damage_decl: decl.to_string(),
        });
    }
    out
}
