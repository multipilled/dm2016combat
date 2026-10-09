//! A demon's decls from the install: entityDef chain, entityDamage groups, md6Def joint groups (damage / pain /
//! death groups and hit-test spheres), AI behaviour damage settings, threat management (SDPS pain reactions)
//! and the pain graph. Defaults that no decl writes come from the decl class constructors where found
//! (DEMONS.md); the rest are marked INFERRED.

use std::sync::Arc;

use anyhow::{Context, Result};
use idres::Container;
use idres::decl::Block;
use idres::decldb::DeclDb;
use idres::ldecl::{self, Arg, Item};

use super::pain::PainType;

/// One leaky bucket's settings (idActor::idActorConstant::painInfo_t / threat overrideBucketInfos).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BucketInfo {
    pub max: f32,
    pub decay_rate: f32,
    /// Game ticks (960/s).
    pub decay_delay: i32,
}

/// damageGroupScalarInfo_t (0x18).
#[derive(Debug, Clone, PartialEq)]
pub struct ScalarInfo {
    pub damage_scale: f32,
    /// Scale of health damage while the location still has armor. INFERRED default 1 (irrelevant without armor).
    pub armored_damage_scale: f32,
    /// Scale of damage to the location's armor. INFERRED default 1.
    pub armor_damage_scale: f32,
    pub critical_hit: bool,
    pub weak_spot: bool,
    pub head_shot: bool,
    /// Damage decl the scales apply to; None = every other decl.
    pub decl: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoreLevel {
    pub level: u32,
    pub causes_injury: bool,
}

/// idDamageGroup (0xa0) of an entityDamage decl.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageGroup {
    pub name: String,
    pub location_armor: f32,
    pub max_health_fraction: f32,
    pub scalars: Vec<ScalarInfo>,
    /// Default true: no AI decl sets it and the AIs take damage.
    pub affects_overall_health: bool,
    pub soft_target: bool,
    pub gore: Vec<GoreLevel>,
}

/// md6Def joint group types (table 0x1436368c0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Damage,
    Pain,
    Twitch,
    Death,
    Limbloss,
    HeadTracking,
    Focus,
    Orientation,
    HitTest,
    Eye,
    Feet,
    Silhouette,
    Trace,
}

impl GroupKind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "damageGroup" => Self::Damage,
            "painGroup" => Self::Pain,
            "twitchGroup" => Self::Twitch,
            "deathGroup" => Self::Death,
            "limblossGroup" => Self::Limbloss,
            "headTrackingGroup" => Self::HeadTracking,
            "focusGroup" => Self::Focus,
            "orientationGroup" => Self::Orientation,
            "hitTestGroup" => Self::HitTest,
            "eyeGroup" => Self::Eye,
            "feetGroup" => Self::Feet,
            "silhouetteGroup" => Self::Silhouette,
            "traceGroup" => Self::Trace,
            _ => return None,
        })
    }
}

/// A joint group: joints in decl order (lowercase); hit-test and trace groups carry one sphere per entry.
#[derive(Debug, Clone, PartialEq)]
pub struct JointGroup {
    pub kind: GroupKind,
    pub name: String,
    pub joints: Vec<String>,
    /// HitTest / Trace entries: (joint, joint-space offset, radius).
    pub spheres: Vec<(String, [f32; 3], f32)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct JointGroups {
    pub groups: Vec<JointGroup>,
}

impl JointGroups {
    /// The first group of `kind` containing `joint` (case-insensitive), as the engine's lookups do.
    pub fn group_of(&self, kind: GroupKind, joint: &str) -> Option<&JointGroup> {
        self.groups.iter().filter(|g| g.kind == kind).find(|g| g.joints.iter().any(|j| j.eq_ignore_ascii_case(joint)))
    }
    pub fn of_kind(&self, kind: GroupKind) -> impl Iterator<Item = &JointGroup> {
        self.groups.iter().filter(move |g| g.kind == kind)
    }

    /// Parses an md6Def's `jointGroups { ... }` block. `painGroup "x" = damageGroup "y"` copies the joints
    /// (and spheres) of an earlier group.
    pub fn parse_md6def(src: &str) -> Result<Self> {
        let root = ldecl::parse(src)?;
        let mut out = JointGroups::default();
        let Some(jg) = root.child("jointGroups") else { return Ok(out) };
        for it in &jg.children {
            let Some(kind) = GroupKind::parse(&it.key) else { continue };
            let Some(name) = it.text().map(str::to_string) else { continue };
            // Alias form: args [name, "=", type, name].
            let texts: Vec<&str> = it.args.iter().filter_map(Arg::text).collect();
            if texts.len() >= 4 && texts[1] == "=" {
                let (k2, n2) = (GroupKind::parse(texts[2]), texts[3]);
                if let Some(src_g) = k2.and_then(|k2| out.groups.iter().find(|g| g.kind == k2 && g.name == n2)).cloned() {
                    out.groups.push(JointGroup { kind, name, ..src_g });
                }
                continue;
            }
            let mut g = JointGroup { kind, name, joints: Vec::new(), spheres: Vec::new() };
            for e in &it.children {
                let j = e.key.to_ascii_lowercase();
                if matches!(kind, GroupKind::HitTest | GroupKind::Trace) {
                    let off = match e.child("offset").and_then(|o| o.arg(0)) {
                        Some(Arg::Tuple(t)) if t.len() == 3 => [t[0], t[1], t[2]],
                        _ => [0.0; 3],
                    };
                    let r = e.child("radius").and_then(Item::f32).unwrap_or(0.0);
                    g.spheres.push((j.clone(), off, r));
                }
                if !g.joints.contains(&j) {
                    g.joints.push(j);
                }
            }
            out.groups.push(g);
        }
        Ok(out)
    }

    /// Child groups replace the parent's group of the same kind and name; new ones are appended.
    fn overlay(&mut self, child: JointGroups) {
        for g in child.groups {
            match self.groups.iter_mut().find(|p| p.kind == g.kind && p.name == g.name) {
                Some(p) => *p = g,
                None => self.groups.push(g),
            }
        }
    }
}

/// SDPSReaction_t (0x48). Defaults INFERRED from which keys the decls omit (ctor not found): totals 0,
/// stimulusScale 1, prerequisiteHealthFraction 1, minRetriggerTime 0, maxTimesUsable -1, NONE states,
/// splash window -1 (off), not armored.
#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    pub total_damage: f32,
    pub total_health_fraction: f32,
    pub stimulus_scale: f32,
    pub prerequisite_health_fraction: f32,
    pub min_retrigger_ms: i32,
    pub max_times_usable: i32,
    pub prerequisite_state: PainType,
    pub reaction: PainType,
    pub tags: Vec<String>,
    pub splash_min: f32,
    pub splash_max: f32,
    pub on_recovery_health_fraction: f32,
    pub clear_this_bucket: bool,
    pub clear_all_buckets: bool,
    pub armored: bool,
}

impl Default for Reaction {
    fn default() -> Self {
        Self {
            total_damage: 0.0,
            total_health_fraction: 0.0,
            stimulus_scale: 1.0,
            prerequisite_health_fraction: 1.0,
            min_retrigger_ms: 0,
            max_times_usable: -1,
            prerequisite_state: PainType::None,
            reaction: PainType::None,
            tags: Vec::new(),
            splash_min: -1.0,
            splash_max: -1.0,
            on_recovery_health_fraction: 0.0,
            clear_this_bucket: false,
            clear_all_buckets: false,
            armored: false,
        }
    }
}

impl Reaction {
    /// The bucket level this reaction needs: totalHealthFractionNeeded * maxHealth when set, else totalDamageNeeded.
    pub fn threshold(&self, max_health: f32) -> f32 {
        if self.total_health_fraction <= 0.0 { self.total_damage } else { self.total_health_fraction * max_health }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    pub decl: Option<String>,
    pub reactions: Vec<Reaction>,
}

/// idDeclAIThreatManagement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreatDecl {
    pub default: Vec<Reaction>,
    pub mappings: Vec<Mapping>,
    pub overrides: Vec<(PainType, BucketInfo)>,
    pub use_full_search: bool,
}

/// idDeclAIBehavior::idDamageBehaviors and the behaviour fields the damage code reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Behaviors {
    pub pain_graph: String,
    pub medium_death_threshold: f32,
    pub heavy_death_threshold: f32,
    pub moving_pain_speed: f32,
    pub moving_death_speed: f32,
    pub slow_moving_death_speed: f32,
    /// idSkillSetting<float> per difficulty. INFERRED default 1 (ctor not decoded; no decl sets it).
    pub difficulty_damage_scale: [f32; 5],
    pub can_take_damage: bool,
    pub has_death_anims: bool,
    pub has_moving_death_anims: bool,
    pub stagger_length_ms: i32,
    pub stagger_vulnerable_ms: i32,
    /// aiMonsterType_t bit (AI_MONSTER_ZOMBIE 2, IMP 4, ...).
    pub monster_type: u32,
}

/// One idDeclAIPainNode anim entry.
#[derive(Debug, Clone, PartialEq)]
pub struct PainAnim {
    /// `zion/characters/monsters/zombie/<subweb>/<state>`.
    pub web_node: String,
    /// DAMAGEDIR_ value (1 front 2 back 3 left 4 right).
    pub damage_dir: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PainNode {
    pub name: String,
    pub anims: Vec<PainAnim>,
    pub motion_dir: String,
    /// PAINFLAG_ bits: 1 USE_LOOP_STAGGER, 2 MOVING, 4 NO_ORIENT, 8 FORCE_FRONT, 0x10 IGNORE_TAGS.
    pub pain_flags: u32,
    /// DAMAGE_DIR_ count (2 front/back, 4 front/back/left/right, 8 all).
    pub num_damage_dirs: u32,
    pub enabled: bool,
    pub internal_only: bool,
    pub subgraph_proxy: bool,
    pub fallback_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PainLink {
    pub start: String,
    pub end: String,
    /// PAINREJECT_ bits: 2 TAGS, 4 TRANSLATION, 8 NO_NODE.
    pub reject: u32,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PainSubGraph {
    pub name: String,
    pub pain_type: PainType,
    pub nodes: Vec<PainNode>,
    pub links: Vec<PainLink>,
}

/// idDeclAIPainGraph.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PainGraph {
    pub subgraphs: Vec<PainSubGraph>,
}

impl PainGraph {
    pub fn subgraph(&self, t: PainType) -> Option<&PainSubGraph> {
        self.subgraphs.iter().find(|s| s.pain_type == t)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InjuredState {
    pub name: String,
}

/// Everything the damage, pain and death code needs for one demon type.
#[derive(Debug, Clone)]
pub struct DemonDef {
    pub entity: String,
    pub md6: String,
    pub anim_web: String,
    pub health: f32,
    pub shield: f32,
    pub mass: f32,
    pub pain_info: BucketInfo,
    pub groups: Vec<DamageGroup>,
    pub joint_groups: JointGroups,
    pub behaviors: Behaviors,
    pub threat: ThreatDecl,
    pub pain_graph: PainGraph,
    pub injured: Vec<InjuredState>,
    pub initial_injured_state: usize,
    /// aiEditable.death.removeTime (ms) / fadeOutTime (s).
    pub remove_time_ms: i32,
    pub fade_out_s: f32,
    /// goreComponent.goreGraph.
    pub gore_graph: String,
    /// The gore graph's wound decls: (short name, mesh kits). Mesh kits are model meshes hidden until the wound
    /// is enabled (gore, or ae_goreEnableByName "<short name>" anim events).
    pub wounds: Vec<(String, Vec<String>)>,
}

/// Alias tags and tag groups of an anim web's nodes (model 0 tree), read straight from the decl text: the
/// pain anim choice (painIndex) and the death tags need them. Tag group values are the decl's DEFAULT bits.
#[derive(Debug, Clone, Default)]
pub struct WebTags {
    nodes: std::collections::HashMap<(String, String), NodeTags>,
}

#[derive(Debug, Clone, Default)]
pub struct NodeTags {
    /// Per alias, its `tags` list.
    pub aliases: Vec<Vec<String>>,
    /// Tag groups in decl order: (group, [(tag, default)]).
    pub groups: Vec<(String, Vec<(String, bool)>)>,
}

impl WebTags {
    pub fn load(c: &Container, web: &str) -> Result<Arc<Self>> {
        let src = String::from_utf8_lossy(&c.read_by_name(&format!("generated/decls/animweb/{web}.decl")).with_context(|| format!("animWeb {web}"))?).into_owned();
        Ok(Arc::new(Self::parse(&src)?))
    }

    pub fn parse(src: &str) -> Result<Self> {
        let root = ldecl::parse(src)?;
        let mut out = WebTags::default();
        let Some(sws) = root.child("subWebs") else { return Ok(out) };
        for sw in sws.children_named("subWeb") {
            let Some(sub) = sw.text() else { continue };
            for n in sw.children_named("node") {
                let Some(state) = n.text() else { continue };
                let Some(tree) = n.child("blendTrees").and_then(|b| b.children_named("tree").find(|t| t.child("modelIndex").and_then(Item::f32).unwrap_or(0.0) == 0.0)) else { continue };
                let groups = tree
                    .children_named("tagGroup")
                    .filter_map(|g| Some((g.text()?.to_string(), g.children_named("tag").filter_map(|t| Some((t.text()?.to_string(), t.arg(1).and_then(Arg::f32).unwrap_or(0.0) != 0.0))).collect())))
                    .collect();
                let aliases = tree
                    .child("anims")
                    .map(|a| a.children_named("alias").map(|al| al.child("tags").map(|t| t.args.iter().filter_map(Arg::text).map(str::to_string).collect()).unwrap_or_default()).collect())
                    .unwrap_or_default();
                out.nodes.insert((sub.to_string(), state.to_string()), NodeTags { aliases, groups });
            }
        }
        Ok(out)
    }

    pub fn node(&self, sub: &str, state: &str) -> Option<&NodeTags> {
        self.nodes.get(&(sub.to_string(), state.to_string()))
    }
    pub fn aliases(&self, sub: &str, state: &str) -> Option<&[Vec<String>]> {
        self.node(sub, state).map(|n| n.aliases.as_slice())
    }
    /// Every tag of the node's tag groups (each is also a web scalar).
    pub fn tag_names(&self, sub: &str, state: &str) -> Vec<String> {
        self.node(sub, state).map(|n| n.groups.iter().flat_map(|(_, t)| t.iter().map(|(n, _)| n.clone())).collect()).unwrap_or_default()
    }
}

/// `num` + `item[i]` lists (child `num` truncates inherited items; missing items keep the struct defaults).
pub(crate) fn list(b: Option<&Block>) -> Vec<Option<&Block>> {
    let Some(b) = b else { return Vec::new() };
    let n = match b.f32("num") {
        Some(n) => n.max(0.0) as usize,
        None => b.items.iter().filter_map(|(k, _)| k.strip_prefix("item[")?.strip_suffix(']')?.parse::<usize>().ok()).map(|i| i + 1).max().unwrap_or(0),
    };
    (0..n).map(|i| b.block(&format!("item[{i}]"))).collect()
}

/// `num` + `item[i] = "text"` string lists.
pub(crate) fn str_list(b: Option<&Block>) -> Vec<String> {
    let Some(b) = b else { return Vec::new() };
    let n = match b.f32("num") {
        Some(n) => n.max(0.0) as usize,
        None => b.items.iter().filter_map(|(k, _)| k.strip_prefix("item[")?.strip_suffix(']')?.parse::<usize>().ok()).map(|i| i + 1).max().unwrap_or(0),
    };
    (0..n).filter_map(|i| b.str(&format!("item[{i}]")).map(str::to_string)).collect()
}

fn flag(b: Option<&Block>, k: &str, d: bool) -> bool {
    b.and_then(|b| b.path(k)).and_then(|v| v.as_bool()).unwrap_or(d)
}
fn num(b: Option<&Block>, k: &str, d: f32) -> f32 {
    b.and_then(|b| b.f32(k)).unwrap_or(d)
}
fn text<'a>(b: Option<&'a Block>, k: &str) -> Option<&'a str> {
    b.and_then(|b| b.str(k)).filter(|s| !s.is_empty())
}

/// aiMonsterType_t values (enum table 0x14351f220).
pub fn monster_type(s: &str) -> u32 {
    match s {
        "AI_MONSTER_GENERIC" => 1,
        "AI_MONSTER_ZOMBIE" => 2,
        "AI_MONSTER_IMP" => 4,
        "AI_MONSTER_HELLIFIED_SOLDIER" => 8,
        "AI_MONSTER_LASER_SOLDIER" => 0x10,
        "AI_MONSTER_HELLIFIED_SHOTGUNNER" => 0x20,
        "AI_MONSTER_LOSTSOUL" => 0x40,
        "AI_MONSTER_ARCHVILE" => 0x100,
        "AI_MONSTER_HELLKNIGHT" => 0x200,
        "AI_MONSTER_MANCUBUS" => 0x400,
        "AI_MONSTER_PINKY" => 0x800,
        "AI_MONSTER_PINKY_SPECTRE" => 0x1000,
        "AI_MONSTER_NPC" => 0x2000,
        "AI_MONSTER_CACODEMON" => 0x4000,
        "AI_MONSTER_REVENANT" => 0x8000,
        "AI_MONSTER_BARON" => 0x10000,
        "AI_MONSTER_CYBERDEMON" => 0x40000,
        "AI_MONSTER_OLIVIASGUARD" => 0x80000,
        "AI_MONSTER_TALISMANGUARD" => 0x100000,
        "AI_MONSTER_CYBERDEMON_HELL" => 0x200000,
        "AI_MONSTER_SPIDER_MASTERMIND" => 0x400000,
        _ => 0,
    }
}

fn bits(s: &str, table: &[(&str, u32)]) -> u32 {
    s.split_whitespace().filter_map(|w| table.iter().find(|(n, _)| *n == w).map(|(_, v)| *v)).fold(0, |a, b| a | b)
}

fn damage_dir(s: &str) -> u32 {
    match s {
        "DAMAGEDIR_FRONT" => 1,
        "DAMAGEDIR_BACK" => 2,
        "DAMAGEDIR_LEFT" => 3,
        "DAMAGEDIR_RIGHT" => 4,
        "DAMAGEDIR_FRONTLEFT" => 5,
        "DAMAGEDIR_FRONTRIGHT" => 6,
        "DAMAGEDIR_BACKLEFT" => 7,
        "DAMAGEDIR_BACKRIGHT" => 8,
        _ => 0,
    }
}

fn reaction(b: Option<&Block>) -> Reaction {
    let d = Reaction::default();
    let Some(b) = b else { return d };
    let s = Some(b);
    Reaction {
        total_damage: num(s, "totalDamageNeeded", d.total_damage),
        total_health_fraction: num(s, "totalHealthFractionNeeded", d.total_health_fraction),
        stimulus_scale: num(s, "stimulusScale", d.stimulus_scale),
        prerequisite_health_fraction: num(s, "prerequisiteHealthFraction", d.prerequisite_health_fraction),
        min_retrigger_ms: num(s, "minRetriggerTime.value", 0.0) as i32,
        max_times_usable: num(s, "maxTimesUsable", -1.0) as i32,
        prerequisite_state: text(s, "prerequisiteState").map(PainType::parse).unwrap_or(PainType::None),
        reaction: text(s, "reaction").map(PainType::parse).unwrap_or(PainType::None),
        tags: str_list(b.block("tags")),
        splash_min: num(s, "splashRadiusPercMin", d.splash_min),
        splash_max: num(s, "splashRadiusPercMax", d.splash_max),
        on_recovery_health_fraction: num(s, "onRecovery_healthFraction", d.on_recovery_health_fraction),
        clear_this_bucket: flag(s, "clearThisBucket", false),
        clear_all_buckets: flag(s, "clearAllBuckets", false),
        armored: flag(s, "armored", false),
    }
}

fn reactions(table: Option<&Block>) -> Vec<Reaction> {
    list(table.and_then(|t| t.block("reactions"))).into_iter().map(reaction).collect()
}

impl ThreatDecl {
    pub fn from_block(b: &Block) -> Self {
        let e = b.block("edit");
        ThreatDecl {
            default: reactions(e.and_then(|e| e.block("defaultMapping.table"))),
            mappings: list(e.and_then(|e| e.block("mappings")))
                .into_iter()
                .map(|m| Mapping { decl: text(m, "damageDecl").map(str::to_string), reactions: reactions(m.and_then(|m| m.block("table"))) })
                .collect(),
            overrides: list(e.and_then(|e| e.block("overrideBucketInfos")))
                .into_iter()
                .flatten()
                .map(|o| {
                    let o = Some(o);
                    (
                        text(o, "painType").map(PainType::parse).unwrap_or(PainType::None),
                        BucketInfo { max: num(o, "bucketMaxValue", 0.0), decay_rate: num(o, "decayRate", 0.0), decay_delay: num(o, "decayDelay", 0.0) as i32 },
                    )
                })
                .collect(),
            use_full_search: flag(e, "useFullSearch", false),
        }
    }
}

impl PainGraph {
    /// idDeclAIPainGraph text: `subGraphs { subGraph = { object = { object = {...} } nodes = { node = ... } links = {...} } ... }`.
    pub fn from_block(b: &Block) -> Self {
        let mut out = PainGraph::default();
        let Some(sgs) = b.block("edit.subGraphs") else { return out };
        for (k, v) in &sgs.items {
            if k != "subGraph" {
                continue;
            }
            let Some(sg) = v.as_block() else { continue };
            let o = sg.block("object.object");
            let mut g = PainSubGraph {
                name: text(o, "name").unwrap_or("").to_string(),
                pain_type: text(o, "painType").map(PainType::parse).unwrap_or(PainType::None),
                nodes: Vec::new(),
                links: Vec::new(),
            };
            if !flag(o, "enabled", true) {
                continue;
            }
            if let Some(nodes) = sg.block("nodes") {
                for (nk, nv) in &nodes.items {
                    if nk != "node" {
                        continue;
                    }
                    let n = nv.as_block().and_then(|n| n.block("object.object"));
                    g.nodes.push(PainNode {
                        name: text(n, "name").unwrap_or("").to_string(),
                        anims: list(n.and_then(|n| n.block("anims")))
                            .into_iter()
                            .map(|a| PainAnim { web_node: text(a, "webNode").unwrap_or("").to_string(), damage_dir: text(a, "damageDir").map(damage_dir).unwrap_or(0) })
                            .collect(),
                        motion_dir: text(n, "motionDir").unwrap_or("").to_string(),
                        pain_flags: text(n, "painFlags")
                            .map(|s| bits(s, &[("PAINFLAG_USE_LOOP_STAGGER", 1), ("PAINFLAG_MOVING", 2), ("PAINFLAG_NO_ORIENT", 4), ("PAINFLAG_FORCE_FRONT", 8), ("PAINFLAG_IGNORE_TAGS", 0x10)]))
                            .unwrap_or(0),
                        num_damage_dirs: text(n, "numDamageDirs")
                            .map(|s| match s {
                                "DAMAGE_DIR_FRONT_BACK" => 2,
                                "DAMAGE_DIR_FRONT_BACK_LEFT_RIGHT" => 4,
                                "DAMAGE_DIR_ALL" => 8,
                                _ => 1,
                            })
                            .unwrap_or(1),
                        enabled: flag(n, "enabled", true),
                        internal_only: flag(n, "internalOnly", false),
                        subgraph_proxy: flag(n, "subgraphProxy", false),
                        fallback_only: flag(n, "fallbackOnly", false),
                    });
                }
            }
            // `links = { <startNode> = { link = { object = { object = { name rejectReason enabled } }
            // startNode = ".."; endNode = ".."; } link = ... } ... }`.
            if let Some(links) = sg.block("links") {
                for (_, lv) in &links.items {
                    let Some(lb) = lv.as_block() else { continue };
                    for (lk, l) in &lb.items {
                        let Some(l) = l.as_block().filter(|_| lk == "link") else { continue };
                        let lo = l.block("object.object");
                        g.links.push(PainLink {
                            start: l.str("startNode").unwrap_or("").to_string(),
                            end: l.str("endNode").unwrap_or("").to_string(),
                            reject: text(lo, "rejectReason").map(|s| bits(s, &[("PAINREJECT_TAGS", 2), ("PAINREJECT_TRANSLATION", 4), ("PAINREJECT_NO_NODE", 8)])).unwrap_or(0),
                            enabled: flag(lo, "enabled", true),
                        });
                    }
                }
            }
            out.subgraphs.push(g);
        }
        out
    }
}

fn md6def_text(c: &Container, md6: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&c.read_by_name(&format!("generated/decls/md6def/{md6}.decl")).with_context(|| format!("md6Def {md6}"))?).into_owned())
}

/// md6Def joint groups with the `init { inherit ... }` chain applied.
pub fn joint_groups(c: &Container, md6: &str) -> Result<JointGroups> {
    let mut chain = Vec::new();
    let mut name = md6.to_string();
    for _ in 0..16 {
        let src = md6def_text(c, &name)?;
        let parent = ldecl::parse(&src).ok().and_then(|r| r.path("init.inherit").and_then(Item::text).map(str::to_string));
        chain.push(src);
        match parent {
            Some(p) if !p.is_empty() => name = p,
            _ => break,
        }
    }
    let mut out = JointGroups::default();
    for src in chain.iter().rev() {
        out.overlay(JointGroups::parse_md6def(src)?);
    }
    Ok(out)
}

/// Every `*WoundDecl = "..."` of a gore graph, with each wound decl's `visuals.meshKits`.
fn wounds(db: &DeclDb, graph: &str) -> Vec<(String, Vec<String>)> {
    fn collect(b: &Block, out: &mut Vec<String>) {
        for (k, v) in &b.items {
            match v {
                idres::decl::Value::Block(c) => collect(c, out),
                v if k.ends_with("WoundDecl") => {
                    if let Some(s) = v.as_str().filter(|s| !s.is_empty() && *s != "NULL") {
                        if !out.iter().any(|o| o == s) {
                            out.push(s.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let Ok(g) = db.get("goregraph", graph) else { return Vec::new() };
    let mut names = Vec::new();
    collect(&g, &mut names);
    names
        .into_iter()
        .filter_map(|n| {
            let w = db.get("gorewounds", &n).ok()?;
            let kits = str_list(w.block("edit.visuals.meshKits"));
            Some((n.rsplit('/').next().unwrap_or(&n).to_string(), kits))
        })
        .collect()
}

impl DemonDef {
    /// Mesh kits of every wound: the model meshes that start hidden.
    pub fn wound_meshes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.wounds.iter().flat_map(|(_, k)| k.iter().cloned()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Loads an AI entityDef (e.g. [`super::POSSESSED`]) and everything its damage, pain and death code reads.
    pub fn load(db: &DeclDb, entity: &str) -> Result<Arc<Self>> {
        let ent = db.get("entitydef", entity).with_context(|| format!("entityDef {entity}"))?;
        let e = ent.block("edit").context("entityDef without edit")?;
        let model = e.str("renderModelInfo.model").context("no renderModelInfo.model")?.to_string();
        let anim_web = e.str("aiConstants.animation.animWebs.animWebs[0]").unwrap_or("").to_string();
        let comps = e.block("aiHealth.components");
        let health = num(comps, "components[0].starting", num(comps, "components[0].max", 100.0));
        let shield = num(comps, "components[1].starting", num(comps, "components[1].max", 0.0));
        let pi = e.block("actorConstants.painInfo");
        // painInfo_t: no default bucket size is written by ai/default; bucketMaxValue 0 means no bucket.
        let pain_info = BucketInfo { max: num(pi, "bucketMaxValue", 0.0), decay_rate: num(pi, "decayRate", 0.0), decay_delay: num(pi, "decayDelay", 0.0) as i32 };

        let ed_name = e.str("actorEditable.entityDamageComponent.entityDamage").context("no entityDamage")?;
        let ed = db.get("entitydamage", ed_name).with_context(|| format!("entityDamage {ed_name}"))?;
        let groups = list(ed.block("edit.damageGroups"))
            .into_iter()
            .map(|g| DamageGroup {
                name: text(g, "groupName").unwrap_or("").to_string(),
                location_armor: num(g, "locationArmor", 0.0),
                max_health_fraction: num(g, "maxHealthFraction", 0.0),
                scalars: list(g.and_then(|g| g.block("damageGroupScalarInfo")))
                    .into_iter()
                    .map(|s| ScalarInfo {
                        damage_scale: num(s, "damageScale", 1.0),
                        armored_damage_scale: num(s, "armoredDamageScale", 1.0),
                        armor_damage_scale: num(s, "armorDamageScale", 1.0),
                        critical_hit: flag(s, "damageFlags.criticalHit", false),
                        weak_spot: flag(s, "damageFlags.demonWeakSpot", false),
                        head_shot: flag(s, "damageFlags.headShot", false),
                        decl: text(s, "damageDecl").map(str::to_string),
                    })
                    .collect(),
                affects_overall_health: flag(g, "affectsOverallHealth", true),
                soft_target: flag(g, "softTarget", false),
                gore: list(g.and_then(|g| g.block("goreLevelInfo")))
                    .into_iter()
                    .map(|l| GoreLevel {
                        level: match text(l, "goreLevel").unwrap_or("") {
                            "GORELEVEL_BLOOD" => 1,
                            "GORELEVEL_LIGHT_DAMAGE" => 2,
                            "GORELEVEL_HEAVY_DAMAGE" => 3,
                            "GORELEVEL_TATTERED" => 4,
                            "GORELEVEL_DISMEMBERED" => 5,
                            _ => 0,
                        },
                        causes_injury: flag(l, "causesInjury", true),
                    })
                    .collect(),
            })
            .collect();

        let beh_name = e.str("aiEditable.behaviors.decl").context("no behaviors decl")?;
        let beh = db.get("aibehavior", beh_name).with_context(|| format!("behaviors {beh_name}"))?;
        let be = beh.block("edit");
        let db_ = be.and_then(|b| b.block("damageBehaviors"));
        let mut dds = [1.0f32; 5];
        if let Some(s) = db_.and_then(|d| d.block("difficultyDamageScale")) {
            for (i, v) in dds.iter_mut().enumerate() {
                if let Some(x) = s.f32(&format!("value[{i}]")) {
                    *v = x;
                }
            }
        }
        let behaviors = Behaviors {
            pain_graph: text(db_, "painGraph").unwrap_or("").to_string(),
            medium_death_threshold: num(db_, "mediumDeathThreshold", 0.0),
            heavy_death_threshold: num(db_, "heavyDeathThreshold", 0.0),
            moving_pain_speed: num(db_, "movingPainSpeed", 0.0),
            moving_death_speed: num(db_, "movingDeathSpeed", 0.0),
            slow_moving_death_speed: num(db_, "slowMovingDeathSpeed", 0.0),
            difficulty_damage_scale: dds,
            can_take_damage: flag(db_, "canTakeDamage", true),
            has_death_anims: flag(db_, "hasDeathAnims", false),
            has_moving_death_anims: flag(db_, "hasMovingDeathAnims", false),
            stagger_length_ms: num(db_, "staggerLength", 0.0) as i32,
            stagger_vulnerable_ms: num(db_, "staggerLength_Vulnerable", 0.0) as i32,
            monster_type: text(be, "monsterType").map(monster_type).unwrap_or(0),
        };

        let threat = match e.str("aiConstants.threatManagementDecls.item[0].decl") {
            Some(t) => ThreatDecl::from_block(&*db.get("aithreatmanagement", t).with_context(|| format!("threat {t}"))?),
            None => ThreatDecl::default(),
        };
        let pain_graph = if behaviors.pain_graph.is_empty() {
            PainGraph::default()
        } else {
            PainGraph::from_block(&*db.get("aipaingraph", &behaviors.pain_graph).with_context(|| format!("pain graph {}", behaviors.pain_graph))?)
        };
        let injured = list(e.block("actorEditable.injuredStates")).into_iter().map(|s| InjuredState { name: text(s, "name").unwrap_or("").to_string() }).collect();
        let joint_groups = joint_groups(db.container(), &model)?;
        let gore_graph = e.str("goreComponent.goreGraph").unwrap_or("").to_string();
        let wounds = if gore_graph.is_empty() { Vec::new() } else { wounds(db, &gore_graph) };
        let death = e.block("aiEditable.death");
        Ok(Arc::new(DemonDef {
            entity: entity.to_string(),
            md6: model,
            anim_web,
            health,
            shield,
            mass: num(Some(e), "mass", 0.0),
            pain_info,
            groups,
            joint_groups,
            behaviors,
            threat,
            pain_graph,
            injured,
            initial_injured_state: num(Some(e), "actorEditable.initialInjuredState", 0.0) as usize,
            remove_time_ms: num(death, "removeTime", 0.0) as i32,
            fade_out_s: num(death, "fadeOutTime", 0.0),
            gore_graph,
            wounds,
        }))
    }

    /// The entityDamage group whose name equals the md6Def damageGroup containing `joint` (0x140830900).
    pub fn damage_group_of_joint(&self, joint: &str) -> Option<usize> {
        let jg = self.joint_groups.group_of(GroupKind::Damage, joint)?;
        self.groups.iter().position(|g| g.name.eq_ignore_ascii_case(&jg.name))
    }

    /// Bucket settings for pain type `t`: painInfo, unless the threat decl overrides that bucket.
    pub fn bucket_info(&self, t: PainType) -> BucketInfo {
        self.threat.overrides.iter().rev().find(|(p, _)| *p == t).map(|(_, b)| *b).unwrap_or(self.pain_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint_group_syntax() {
        let src = "{\n init {\n mesh \"m.md6mesh\"\n }\n jointGroups {\n damageGroup \"head\" {\n neck\n head\n }\n painGroup \"head\" = damageGroup \"head\"\n hitTestGroup \"head\" {\n head {\n offset ( 6 0 2 )\n radius 6.000000\n }\n }\n hitTestGroup \"crotch\" {\n hips {\n radius 8.000000\n }\n }\n }\n}\n";
        let g = JointGroups::parse_md6def(src).unwrap();
        assert_eq!(g.group_of(GroupKind::Damage, "Head").map(|g| g.name.as_str()), Some("head"));
        assert_eq!(g.group_of(GroupKind::Pain, "neck").map(|g| g.joints.len()), Some(2));
        let ht: Vec<_> = g.of_kind(GroupKind::HitTest).collect();
        assert_eq!(ht[0].spheres, vec![("head".to_string(), [6.0, 0.0, 2.0], 6.0)]);
        assert_eq!(ht[1].spheres, vec![("hips".to_string(), [0.0, 0.0, 0.0], 8.0)]);
    }
}
