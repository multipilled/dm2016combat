//! The attack graph (idDeclAttackGraph, `generated/decls/attackgraph/<name>.decl`): sub graphs of attack nodes,
//! each a list of attacks with an arc, distance ranges and the anim web node that plays them; and the selection
//! the attack component runs for Shared_ShouldAttack (0x1405f1a40 -> 0x140532600 -> 0x140530a70; AI.md 5).

use anyhow::{Context, Result};
use idres::decl::{Block, Value};
use idres::decldb::DeclDb;

use super::def::items;

/// ATTACK_VALIDATOR_*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validator {
    Default,
    /// Checks the attack anim's root-motion delta lands on the target (lunges).
    AnimDelta,
    Other,
}

/// One attackList item (idDeclAttackNode attack).
#[derive(Debug, Clone, PartialEq)]
pub struct Attack {
    pub name: String,
    pub validator: Validator,
    /// Degrees from the body forward axis (positive = left).
    pub arc_direction: f32,
    pub arc_half_length: f32,
    /// 0 = no vertical arc limit.
    pub arc_vertical_half_length: f32,
    pub distance: (f32, f32),
    pub distance_vertical: (f32, f32),
    pub distance_absolute: (f32, f32),
    pub use_2d_distance: bool,
    /// Full web node path ("zion/characters/monsters/zombie/hands_melee/forward").
    pub via_node: String,
    pub dest_node: String,
    pub weight: f32,
    /// timeBetweenAttacks min/max (s).
    pub time_between: (f32, f32),
    pub restrict_with_shared_timer: bool,
    pub shared_timer_interval: (f32, f32),
    pub prediction_time: f32,
    pub disabled: bool,
    pub usable_stopped: bool,
    pub usable_walking: bool,
    pub usable_running: bool,
    pub usable_outside_move_cycle: bool,
    pub flags: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttackNode {
    pub name: String,
    pub enabled: bool,
    pub attacks: Vec<Attack>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubGraph {
    pub name: String,
    pub attacks: Vec<Attack>,
    pub nodes: Vec<AttackNode>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AttackGraph {
    pub name: String,
    pub subgraphs: Vec<SubGraph>,
}

fn rng(b: &Block, key: &str) -> (f32, f32) {
    let lo = b.f32(&format!("{key}.minRange")).or_else(|| b.f32(&format!("{key}.minRange.value"))).unwrap_or(0.0);
    let hi = b.f32(&format!("{key}.maxRange")).or_else(|| b.f32(&format!("{key}.maxRange.value"))).unwrap_or(0.0);
    (lo, hi)
}

fn flag(b: &Block, key: &str, default: bool) -> bool {
    b.path(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

impl Attack {
    fn parse(a: &Block) -> Attack {
        Attack {
            name: a.str("attackName").unwrap_or("").to_string(),
            validator: match a.str("attackValidator").unwrap_or("") {
                "ATTACK_VALIDATOR_DEFAULT" | "" => Validator::Default,
                "ATTACK_VALIDATOR_ANIM_DELTA" => Validator::AnimDelta,
                _ => Validator::Other,
            },
            arc_direction: a.f32("arcDirection.value").unwrap_or(0.0),
            arc_half_length: a.f32("arcHalfLength.value").unwrap_or(0.0),
            arc_vertical_half_length: a.f32("arcVerticalHalfLength.value").unwrap_or(0.0),
            distance: rng(a, "distanceRange"),
            distance_vertical: rng(a, "distanceRange_Vertical"),
            distance_absolute: rng(a, "distanceRange_absolute"),
            use_2d_distance: flag(a, "use2DDistanceChecks", false),
            via_node: a.str("viaNode").unwrap_or("").to_string(),
            dest_node: a.str("destNode").unwrap_or("").to_string(),
            weight: a.f32("weight").unwrap_or(1.0),
            time_between: rng(a, "timeBetweenAttacks"),
            restrict_with_shared_timer: flag(a, "restrictWithSharedTimer", false),
            shared_timer_interval: rng(a, "sharedTimerInterval"),
            prediction_time: a.f32("predictionTime.value").unwrap_or(0.0),
            disabled: flag(a, "disabled", false),
            usable_stopped: flag(a, "usableWhileStopped", true),
            usable_walking: flag(a, "usableWhileWalking", true),
            usable_running: flag(a, "usableWhileRunning", true),
            usable_outside_move_cycle: flag(a, "usableOutsideOfMoveCycle", true),
            flags: a.str("flags").unwrap_or("").to_string(),
        }
    }
}

/// Repeated `key = {..}` children of a block, in decl order.
fn all<'a>(b: &'a Block, key: &str) -> impl Iterator<Item = &'a Block> + 'a {
    let key = key.to_string();
    b.items.iter().filter(move |(k, _)| *k == key).filter_map(|(_, v)| match v {
        Value::Block(b) => Some(b),
        _ => None,
    })
}

impl AttackGraph {
    pub fn load(db: &DeclDb, name: &str) -> Result<AttackGraph> {
        let b = db.raw("attackgraph", name).with_context(|| format!("attackGraph {name}"))?;
        Self::parse(name, &b)
    }

    /// Layout as `doomx cat` prints it: edit.subGraphs.subGraph { object { className, object { name } }, nodes { node
    /// { object { className, object { name, attackList {num, item[i]}, enabled } } } }, links }.
    pub fn parse(name: &str, b: &Block) -> Result<AttackGraph> {
        let sgs = b.block("edit.subGraphs").context("attack graph without subGraphs")?;
        let mut subgraphs = Vec::new();
        for sg in all(sgs, "subGraph") {
            let sg_name = sg.str("object.object.name").unwrap_or("").to_string();
            let mut nodes = Vec::new();
            if let Some(ns) = sg.block("nodes") {
                for n in all(ns, "node") {
                    let Some(o) = n.block("object.object") else { continue };
                    let attacks = o.block("attackList").map(|l| items(l).into_iter().map(Attack::parse).collect()).unwrap_or_default();
                    nodes.push(AttackNode {
                        name: o.str("name").unwrap_or("").to_string(),
                        enabled: flag(o, "enabled", true),
                        attacks,
                    });
                }
            }
            let attacks = nodes.iter().filter(|n| n.enabled).flat_map(|n| n.attacks.iter().cloned()).collect();
            subgraphs.push(SubGraph { name: sg_name, attacks, nodes });
        }
        Ok(AttackGraph { name: name.to_string(), subgraphs })
    }

    pub fn subgraph(&self, name: &str) -> Option<&SubGraph> {
        self.subgraphs.iter().find(|s| s.name == name)
    }
}
