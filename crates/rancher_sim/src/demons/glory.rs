//! Glory kills: the player-vs-AI sync melee (idDeclSyncInteraction + idSyncEntity). Notes: gamedata/re/DEMONS.md
//! section 16.
//!
//! Data: the AI's `aiConstants.syncMelee` lists sync entityDefs (`zion/syncmelee/zombie`: an idSyncEntity with its own
//! anim web and model syncentity11.md6, participants: the player on joint `attach00`, the AI on `attach01`) and the
//! `syncInteractions` decls it can be the victim of. One interaction names the input (INPUT_ATTACK2 = melee), a
//! filter (SYNCFILTER_STAGGER_STATE_1: the AI is in its vulnerable stagger), the anim node of the sync web
//! (`killedByPlayer/kill_front_head`), validation limits, the player requirements (the focal damage groups and screen
//! quadrants) and a priority. The sync web has one tree per model of its modelInfos: 0 the sync entity, 1 the
//! player's `player/tp_body.md6` (the arms mesh with the `camera` joint), 2.. the AI models.
//!
//! Choice (player sync component, 0x140ea52c0 builds the candidate list each frame, 0x140ea57f0 picks on the melee
//! press from 0x140da71e0): among the valid candidates the highest priority (+0x2c, the decl's defaultSyncPriority)
//! wins, ties go to the smallest squared distance from the player to the candidate's projected attacker position
//! (+0x64). Placement (idSyncEntity::GetDesiredSyncEntityPosAndAxis 0x140a40ba0, victim-centric): the sync entity is
//! placed so that the victim's attach joint at frame 0 lands on the victim's origin and axis; the attacker's attach
//! joint at frame 0 is where the attacker has to be.

use glam::{Mat3, Mat4, Quat, Vec3};

use idres::decl::{Block, Value};

/// syncFilter_t (enum table 0x143531c28).
pub mod filter {
    pub const HEADSTOMP: u32 = 1;
    pub const STAGGER_STATE_1: u32 = 2;
    pub const STAGGER_STATE_2: u32 = 4;
    pub const KNOCK_DOWN_STATE: u32 = 8;
    pub const DEATH_FROM_ABOVE: u32 = 16;
    pub const ENVIORMENT_LEDGE: u32 = 32;
    pub const ENVIORMENT_WALL: u32 = 64;
    pub const DEATH_FROM_BELOW: u32 = 128;
    pub const FORCE2D_ORIENTATION: u32 = 256;
    pub const CHAINSAW_MINIGAME_START: u32 = 512;
    pub const CHAINSAW_MINIGAME_STAGE: u32 = 1024;

    pub fn parse(s: &str) -> u32 {
        s.split_whitespace()
            .map(|n| match n {
                "SYNCFILTER_HEADSTOMP" => HEADSTOMP,
                "SYNCFILTER_STAGGER_STATE_1" => STAGGER_STATE_1,
                "SYNCFILTER_STAGGER_STATE_2" => STAGGER_STATE_2,
                "SYNCFILTER_KNOCK_DOWN_STATE" => KNOCK_DOWN_STATE,
                "SYNCFILTER_DEATH_FROM_ABOVE" => DEATH_FROM_ABOVE,
                "SYNCFILTER_ENVIORMENT_LEDGE" => ENVIORMENT_LEDGE,
                "SYNCFILTER_ENVIORMENT_WALL" => ENVIORMENT_WALL,
                "SYNCFILTER_DEATH_FROM_BELOW" => DEATH_FROM_BELOW,
                "SYNCFILTER_FORCE2D_ORIENTATION" => FORCE2D_ORIENTATION,
                "SYNCFILTER_CHAINSAW_MINIGAME_START" => CHAINSAW_MINIGAME_START,
                "SYNCFILTER_CHAINSAW_MINIGAME_STAGE" => CHAINSAW_MINIGAME_STAGE,
                _ => 0,
            })
            .fold(0, |a, b| a | b)
    }
}

/// syncParticipantStates_t (enum table 0x143535828).
pub mod participant {
    pub const ON_GROUND: u32 = 1;
    pub const IN_AIR: u32 = 2;
    pub const PERCHED_WALL: u32 = 4;
    pub const PERCHED_CEIL: u32 = 8;
    pub const IN_LEDGE_GRAB: u32 = 16;

    pub fn parse(s: &str) -> u32 {
        s.split_whitespace()
            .map(|n| match n {
                "SYNC_PARTICIPANT_ON_GROUND" => ON_GROUND,
                "SYNC_PARTICIPANT_IN_AIR" => IN_AIR,
                "SYNC_PARTICIPANT_PERCHED_WALL" => PERCHED_WALL,
                "SYNC_PARTICIPANT_PERCHED_CEIL" => PERCHED_CEIL,
                "SYNC_PARTICIPANT_IN_LEDGE_GRAB" => IN_LEDGE_GRAB,
                _ => 0,
            })
            .fold(0, |a, b| a | b)
    }
}

/// syncQuadrant_t (enum table 0x143535828).
pub mod quadrant {
    pub const NONE: u32 = 0;
    pub const TOP_LEFT: u32 = 1;
    pub const TOP_RIGHT: u32 = 2;
    pub const BOTTOM_LEFT: u32 = 4;
    pub const BOTTOM_RIGHT: u32 = 8;
    pub const ANY: u32 = 0x7fff_ffff;

    pub fn parse(s: &str) -> u32 {
        s.split_whitespace()
            .map(|n| match n {
                "SYNC_SCREEN_QUADRANT_TOP_LEFT" => TOP_LEFT,
                "SYNC_SCREEN_QUADRANT_TOP_RIGHT" => TOP_RIGHT,
                "SYNC_SCREEN_QUADRANT_BOTTOM_LEFT" => BOTTOM_LEFT,
                "SYNC_SCREEN_QUADRANT_BOTTOM_RIGHT" => BOTTOM_RIGHT,
                "SYNC_SCREEN_QUADRANT_ANY" => ANY,
                _ => 0,
            })
            .fold(0, |a, b| a | b)
    }
}

/// syncFlags_t (enum table 0x143531be0).
pub mod flags {
    pub const INIT_PLAYER: u32 = 1;
    pub const INIT_AI: u32 = 2;
}

/// One idDeclSyncInteraction (reflection size 0x358). Defaults are the decl ctor's (0x1406ee430: priority 0,
/// syncIsKill true, requiresHeightCheck true, markInstigatorOffLimits true) and validationRules_t's (0x1406ee630:
/// every validation on, instigator / target states ON_GROUND, maxRadiusXY 250, maxRadiusZ 96, pitchDeltaMax 45,
/// yawDeltaMax 60, distance errors 32, health ratio 0..1, aasTrace 8); generated decls omit default values.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncInteraction {
    pub name: String,
    /// +0x70 syncClass ("" = idSyncAttackInteraction).
    pub sync_class: String,
    /// +0x78 syncInputs (playerInput_t names, e.g. INPUT_ATTACK2).
    pub inputs: Vec<String>,
    /// +0x80 syncFlags.
    pub flags: u32,
    /// +0x84 syncFilter.
    pub filter: u32,
    /// +0x8c syncIsKill.
    pub is_kill: bool,
    /// +0xf0 instigatorCentric.
    pub instigator_centric: bool,
    /// +0x12c clockwiseDegreeRotation.
    pub clockwise_deg: f32,
    /// +0x130 defaultSyncPriority.
    pub priority: f32,
    /// +0x188 animations.anims: (subwebName, stateName) of the sync web.
    pub anims: Vec<(String, String)>,
    /// validation (+0x1a8): validInstigatorStates +0x8, validTargetStates +0xc.
    pub instigator_states: u32,
    pub target_states: u32,
    /// validation radii (+0x74 .. +0x80) and angle limits (+0x84, +0x88).
    pub min_radius_xy: f32,
    pub max_radius_xy: f32,
    pub min_radius_z: f32,
    pub max_radius_z: f32,
    pub pitch_delta_max: f32,
    pub yaw_delta_max: f32,
    /// validation healthRatioMin / Max (+0xa0, +0xa4).
    pub health_ratio_min: f32,
    pub health_ratio_max: f32,
    /// playerRequirements.damageGroupNames (+0x268).
    pub damage_groups: Vec<String>,
    /// playerRequirements.requiredNonEquipt (+0x288).
    pub required_non_equipt: Vec<String>,
    /// playerRequirements.focalScreenQuadrants (+0x2d0); NONE when the decl leaves it unset.
    pub quadrants: u32,
}

/// `item[N]` / `ptr[N]` children of a list block, in index order.
fn items<'a>(b: Option<&'a Block>, prefix: &str) -> Vec<&'a Value> {
    let Some(b) = b else { return Vec::new() };
    let mut v: Vec<(usize, &Value)> = b
        .items
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix(prefix)?.strip_prefix('[')?.strip_suffix(']')?.parse().ok()?, v)))
        .collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, v)| v).collect()
}

fn strings(b: Option<&Block>, prefix: &str) -> Vec<String> {
    items(b, prefix).into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

impl SyncInteraction {
    pub fn load(db: &idres::decldb::DeclDb, name: &str) -> anyhow::Result<Self> {
        let d = db.get("syncinteractions", name)?;
        let e = d.block("edit").ok_or_else(|| anyhow::anyhow!("{name}: no edit block"))?;
        Ok(Self::from_edit(name, e))
    }

    pub fn from_edit(name: &str, e: &Block) -> Self {
        let f = |k: &str, def: f32| e.f32(k).unwrap_or(def);
        let b = |k: &str, def: bool| e.path(k).and_then(Value::as_bool).unwrap_or(def);
        let anims = items(e.block("animations.anims"), "item")
            .into_iter()
            .filter_map(|v| {
                let a = v.as_block()?;
                Some((a.str("subwebName").unwrap_or("").to_string(), a.str("stateName")?.to_string()))
            })
            .collect();
        SyncInteraction {
            name: name.to_string(),
            sync_class: e.str("syncClass").unwrap_or("").to_string(),
            inputs: strings(e.block("syncInputs.ptr"), "ptr"),
            flags: e.str("syncFlags").map_or(0, |s| s.split_whitespace().map(|n| match n {
                "SYNCFLAG_INIT_PLAYER" => flags::INIT_PLAYER,
                "SYNCFLAG_INIT_AI" => flags::INIT_AI,
                _ => 0,
            }).fold(0, |a, b| a | b)),
            filter: e.str("syncFilter").map_or(0, filter::parse),
            is_kill: b("syncIsKill", true),
            instigator_centric: b("instigatorCentric", false),
            clockwise_deg: f("clockwiseDegreeRotation", 0.0),
            priority: f("defaultSyncPriority", 0.0),
            anims,
            instigator_states: e.str("validation.validInstigatorStates").map_or(participant::ON_GROUND, participant::parse),
            target_states: e.str("validation.validTargetStates").map_or(participant::ON_GROUND, participant::parse),
            min_radius_xy: f("validation.minRadiusXY", 0.0),
            max_radius_xy: f("validation.maxRadiusXY", 250.0),
            min_radius_z: f("validation.minRadiusZ", 0.0),
            max_radius_z: f("validation.maxRadiusZ", 96.0),
            pitch_delta_max: f("validation.pitchDeltaMax", 45.0),
            yaw_delta_max: f("validation.yawDeltaMax", 60.0),
            health_ratio_min: f("validation.healthRatioMin", 0.0),
            health_ratio_max: f("validation.healthRatioMax", 1.0),
            damage_groups: strings(e.block("playerRequirements.damageGroupNames"), "item"),
            required_non_equipt: strings(e.block("playerRequirements.requiredNonEquipt"), "item"),
            quadrants: e.str("playerRequirements.focalScreenQuadrants").map_or(quadrant::NONE, quadrant::parse),
        }
    }
}

/// The AI's sync melee data (aiConstants.syncMelee): the sync entityDefs and the interactions of its sync groups.
#[derive(Debug, Clone, Default)]
pub struct SyncMelee {
    /// msAfterAttackBeforeCanSync.
    pub ms_after_attack: i32,
    pub entity_defs: Vec<String>,
    pub interactions: Vec<SyncInteraction>,
}

impl SyncMelee {
    /// From an AI entityDef; interactions whose decl does not load are skipped.
    pub fn load(db: &idres::decldb::DeclDb, entity: &str) -> anyhow::Result<Self> {
        let d = db.get("entitydef", entity)?;
        let s = d.block("edit.aiConstants.syncMelee");
        let mut out = SyncMelee { ms_after_attack: s.and_then(|s| s.f32("msAfterAttackBeforeCanSync")).unwrap_or(0.0) as i32, ..Default::default() };
        out.entity_defs = strings(s.and_then(|s| s.block("syncMeleeEntityDefs")), "item");
        for g in items(s.and_then(|s| s.block("syncGroups")), "item") {
            for n in strings(g.as_block().and_then(|g| g.block("syncInteractions")), "item") {
                if let Ok(si) = SyncInteraction::load(db, &n) {
                    out.interactions.push(si);
                }
            }
        }
        Ok(out)
    }
}

/// What the eligibility tests look at, for one interaction against one victim.
#[derive(Debug, Clone, PartialEq)]
pub struct Situation {
    /// The input that was pressed (playerInput_t name).
    pub input: &'static str,
    /// The victim is in its vulnerable stagger (STAGGER_VULNERABLE: "stagger state 1").
    pub victim_stagger_1: bool,
    /// Victim health / max health.
    pub victim_health_ratio: f32,
    /// participant::ON_GROUND or IN_AIR.
    pub player_state: u32,
    /// The player's weapon decl.
    pub weapon: String,
    /// Damage group and joint under the player's focus (crosshair) on the victim, lowercase.
    pub focus: Option<(String, String)>,
    /// quadrant::* of the focus point.
    pub focus_quadrant: u32,
    /// Player origin minus victim origin.
    pub to_player: Vec3,
}

/// Whether an interaction may run (the subset of the candidate validation this port follows).
/// INTERIM: the per-frame candidate validation (0x140ea52c0 -> 0x140ea81b0 / 0x140ea25b0 / 0x140eac950: slide-move,
/// position error tolerances, AAS, wall / ledge environment tests, focus history) was not decoded: here the input,
/// INIT_PLAYER, the filter bits the range can satisfy (STAGGER_STATE_1, DEATH_FROM_ABOVE; ENVIORMENT_* and
/// HEADSTOMP never pass), the instigator state, the victim health ratio, requiredNonEquipt, the focal damage group /
/// quadrant and the height window (minRadiusZ..maxRadiusZ of the player above the victim) are tested.
pub fn eligible(si: &SyncInteraction, s: &Situation) -> bool {
    if !si.inputs.iter().any(|i| i == s.input) || si.flags & flags::INIT_PLAYER == 0 || si.anims.is_empty() {
        return false;
    }
    let f = si.filter;
    if f & (filter::HEADSTOMP | filter::STAGGER_STATE_2 | filter::KNOCK_DOWN_STATE | filter::ENVIORMENT_LEDGE | filter::ENVIORMENT_WALL | filter::DEATH_FROM_BELOW | filter::CHAINSAW_MINIGAME_START | filter::CHAINSAW_MINIGAME_STAGE) != 0 {
        return false;
    }
    if f & filter::STAGGER_STATE_1 != 0 && !s.victim_stagger_1 {
        return false;
    }
    if si.instigator_states & s.player_state == 0 {
        return false;
    }
    if f & filter::DEATH_FROM_ABOVE != 0 && s.player_state & participant::IN_AIR == 0 {
        return false;
    }
    if s.victim_health_ratio < si.health_ratio_min || s.victim_health_ratio > si.health_ratio_max {
        return false;
    }
    if si.required_non_equipt.iter().any(|w| w.eq_ignore_ascii_case(&s.weapon)) {
        return false;
    }
    let dz = s.to_player.z;
    if dz < si.min_radius_z || dz > si.max_radius_z {
        return false;
    }
    if !si.damage_groups.is_empty() {
        let Some((group, joint)) = &s.focus else { return false };
        if !si.damage_groups.iter().any(|g| g.eq_ignore_ascii_case(group) || g.eq_ignore_ascii_case(joint)) {
            return false;
        }
    }
    if si.quadrants != quadrant::NONE && si.quadrants & s.focus_quadrant == 0 {
        return false;
    }
    true
}

/// A valid interaction with its projected attacker position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub index: usize,
    pub priority: f32,
    pub attacker: Vec3,
}

/// 0x140ea57f0: the highest priority, ties to the smallest squared distance from `player` to the projected attacker
/// position.
pub fn choose(cands: &[Candidate], player: Vec3) -> Option<Candidate> {
    let mut best: Option<(Candidate, f32)> = None;
    for c in cands {
        let d2 = c.attacker.distance_squared(player);
        let better = match best {
            None => true,
            Some((b, bd2)) => c.priority > b.priority || (c.priority == b.priority && d2 < bd2),
        };
        if better {
            best = Some((*c, d2));
        }
    }
    best.map(|(c, _)| c)
}

/// The sync entity's world transform and the attacker's start position (0x140a40ba0, victim-centric: the victim's
/// attach joint at frame 0, in sync-entity space, lands on the victim's origin and axis, the axis first turned by
/// clockwiseDegreeRotation about up). `victim_attach` / `attacker_attach` are the sync entity's model-space joint
/// transforms at frame 0. INTERIM: the "face the instigator" option (sync entity +0x5221) and instigator-centric
/// interactions are not followed.
pub fn place(victim_origin: Vec3, victim_yaw_deg: f32, clockwise_deg: f32, victim_attach: Mat4, attacker_attach: Mat4) -> (Mat4, Vec3) {
    let yaw = (victim_yaw_deg - clockwise_deg).to_radians();
    let victim = Mat4::from_rotation_translation(Quat::from_rotation_z(yaw), victim_origin);
    let sync = victim * rigid_inverse(victim_attach);
    (sync, (sync * attacker_attach).w_axis.truncate())
}

/// Inverse of a rotation + translation matrix.
pub fn rigid_inverse(m: Mat4) -> Mat4 {
    let r = Mat3::from_mat4(m).transpose();
    let t = -(r * m.w_axis.truncate());
    Mat4::from_cols(r.x_axis.extend(0.0), r.y_axis.extend(0.0), r.z_axis.extend(0.0), t.extend(1.0))
}

/// The yaw (degrees) of a transform's x axis.
pub fn yaw_of(m: Mat4) -> f32 {
    let x = m.x_axis;
    x.y.atan2(x.x).to_degrees()
}

/// Delta correction of a participant (sync entity events ae_animSyncStartDeltaCorrection at frame `start` ..
/// ae_animSyncDeltaCorrectionEndPos at frame `end`, joint attach00 / attach01): the participant's offset from its
/// attach joint when the correction starts fades out by the end frame. INTERIM: linear in anim time (the correction's
/// curve, and what its two bools select, were not decoded).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeltaCorrection {
    pub start: f32,
    pub end: f32,
    pub offset: Vec3,
    pub yaw_offset: f32,
}

impl DeltaCorrection {
    /// Weight of the offset at anim frame `frame` (1 up to `start`, 0 from `end`).
    pub fn weight(&self, frame: f32) -> f32 {
        if frame <= self.start {
            1.0
        } else if frame >= self.end || self.end <= self.start {
            0.0
        } else {
            1.0 - (frame - self.start) / (self.end - self.start)
        }
    }
}

/// Retargets an anim authored for skeleton `from` onto skeleton `to` by joint name; channels of joints `to` lacks
/// are dropped. The player body's sync anims are authored for marine.md6skl while tp_body.md6's mesh (arms.md6mesh)
/// uses arms.md6skl, so their joint indices differ. INTERIM: the engine's retarget path for a skeleton mismatch was
/// not traced; matching by name follows the md6 joint naming.
pub fn retarget(anim: &idres::md6anim::Md6Anim, from: &idres::md6::Md6Skel, to: &idres::md6::Md6Skel) -> idres::md6anim::Md6Anim {
    let map: Vec<Option<u16>> = from.names.iter().map(|n| to.names.iter().position(|m| m.eq_ignore_ascii_case(n)).map(|i| i as u16)).collect();
    let f = |j: u16| map.get(j as usize).copied().flatten();
    fn chans<T: Clone>(c: &idres::md6anim::Channels<T>, f: &dyn Fn(u16) -> Option<u16>) -> idres::md6anim::Channels<T> {
        let mut out = idres::md6anim::Channels::default();
        for (j, k) in c.joints.iter().zip(c.keys.iter()) {
            if let Some(t) = f(*j) {
                out.joints.push(t);
                out.keys.push(k.clone());
            }
        }
        out
    }
    let mut a = anim.clone();
    a.const_r = anim.const_r.iter().filter_map(|&(j, v)| f(j).map(|k| (k, v))).collect();
    a.const_s = anim.const_s.iter().filter_map(|&(j, v)| f(j).map(|k| (k, v))).collect();
    a.const_t = anim.const_t.iter().filter_map(|&(j, v)| f(j).map(|k| (k, v))).collect();
    a.rot = chans(&anim.rot, &f);
    a.scale = chans(&anim.scale, &f);
    a.trans = chans(&anim.trans, &f);
    a
}

/// Wraps an angle difference to -180..180 degrees.
pub fn wrap_deg(a: f32) -> f32 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn si(name: &str, prio: f32, groups: &[&str], quads: u32) -> SyncInteraction {
        SyncInteraction {
            name: name.into(),
            sync_class: String::new(),
            inputs: vec!["INPUT_ATTACK2".into()],
            flags: flags::INIT_PLAYER,
            filter: filter::STAGGER_STATE_1,
            is_kill: true,
            instigator_centric: false,
            clockwise_deg: 0.0,
            priority: prio,
            anims: vec![("killedByPlayer".into(), format!("kill_{name}"))],
            instigator_states: participant::ON_GROUND | participant::IN_AIR,
            target_states: participant::ON_GROUND,
            min_radius_xy: 0.0,
            max_radius_xy: 250.0,
            min_radius_z: 0.0,
            max_radius_z: 500.0,
            pitch_delta_max: 45.0,
            yaw_delta_max: 60.0,
            health_ratio_min: 0.0,
            health_ratio_max: 1.0,
            damage_groups: groups.iter().map(|s| s.to_string()).collect(),
            required_non_equipt: vec!["weapon/zion/player/sp/chainsaw".into()],
            quadrants: quads,
        }
    }

    fn sit() -> Situation {
        Situation {
            input: "INPUT_ATTACK2",
            victim_stagger_1: true,
            victim_health_ratio: 0.4,
            player_state: participant::ON_GROUND,
            weapon: "weapon/zion/player/sp/shotgun".into(),
            focus: Some(("head".into(), "head".into())),
            focus_quadrant: quadrant::TOP_LEFT,
            to_player: Vec3::new(60.0, 0.0, 0.0),
        }
    }

    #[test]
    fn eligibility_filters() {
        let head = si("front_head", 0.0, &["head"], quadrant::TOP_LEFT | quadrant::TOP_RIGHT);
        let s = sit();
        assert!(eligible(&head, &s));
        assert!(!eligible(&head, &Situation { victim_stagger_1: false, ..s.clone() }));
        assert!(!eligible(&head, &Situation { input: "INPUT_ATTACK1", ..s.clone() }));
        assert!(!eligible(&head, &Situation { focus: Some(("chest".into(), "spine3".into())), ..s.clone() }));
        assert!(!eligible(&head, &Situation { focus: None, ..s.clone() }));
        assert!(!eligible(&head, &Situation { focus_quadrant: quadrant::BOTTOM_LEFT, ..s.clone() }));
        assert!(!eligible(&head, &Situation { weapon: "weapon/zion/player/sp/chainsaw".into(), ..s.clone() }));
        // No damage-group requirement: any focus; above kills need the air and the height window.
        let mut above = si("above_front", 2.0, &[], quadrant::NONE);
        above.filter |= filter::DEATH_FROM_ABOVE;
        above.instigator_states = participant::IN_AIR;
        above.min_radius_z = 69.0;
        assert!(!eligible(&above, &s));
        let air = Situation { player_state: participant::IN_AIR, to_player: Vec3::new(40.0, 0.0, 80.0), ..s.clone() };
        assert!(eligible(&above, &air));
        assert!(!eligible(&above, &Situation { to_player: Vec3::new(40.0, 0.0, 60.0), ..air }));
        let mut wall = si("back_headwallsmash", 1.0, &[], quadrant::NONE);
        wall.filter |= filter::ENVIORMENT_WALL;
        assert!(!eligible(&wall, &s));
    }

    #[test]
    fn choice_by_priority_then_distance() {
        let p = Vec3::new(60.0, 0.0, 0.0);
        let front = Candidate { index: 0, priority: 0.0, attacker: Vec3::new(50.0, 0.0, 0.0) };
        let back = Candidate { index: 1, priority: 0.0, attacker: Vec3::new(-50.0, 0.0, 0.0) };
        assert_eq!(choose(&[back, front], p).unwrap().index, 0);
        assert_eq!(choose(&[back, front], -p).unwrap().index, 1);
        let above = Candidate { index: 2, priority: 2.0, attacker: Vec3::new(0.0, 0.0, 300.0) };
        assert_eq!(choose(&[back, front, above], p).unwrap().index, 2);
        assert!(choose(&[], p).is_none());
    }

    #[test]
    fn placement_lands_the_victim_joint_on_the_victim() {
        // Victim joint 10 forward of the sync origin facing back (yaw 180), attacker joint at the origin facing +x.
        let vj = Mat4::from_rotation_translation(Quat::from_rotation_z(std::f32::consts::PI), Vec3::new(10.0, 0.0, 0.0));
        let aj = Mat4::IDENTITY;
        let (sync, attacker) = place(Vec3::new(100.0, 50.0, 0.0), 90.0, 0.0, vj, aj);
        let v = sync * vj;
        assert!((v.w_axis.truncate() - Vec3::new(100.0, 50.0, 0.0)).length() < 1e-4);
        assert!(wrap_deg(yaw_of(v) - 90.0).abs() < 1e-3);
        // The attacker stands 10 in front of the victim (along the victim's facing), facing it.
        assert!((attacker - Vec3::new(100.0, 60.0, 0.0)).length() < 1e-4, "{attacker}");
        assert!(wrap_deg(yaw_of(sync * aj) + 90.0).abs() < 1e-3);
        // clockwiseDegreeRotation turns the whole scene clockwise about the victim.
        let (_, a2) = place(Vec3::new(100.0, 50.0, 0.0), 90.0, 90.0, vj, aj);
        assert!((a2 - Vec3::new(110.0, 50.0, 0.0)).length() < 1e-4, "{a2}");
    }

    #[test]
    fn delta_correction_weight() {
        let c = DeltaCorrection { start: 1.0, end: 4.0, offset: Vec3::X, yaw_offset: 10.0 };
        assert_eq!((c.weight(0.0), c.weight(1.0), c.weight(2.5), c.weight(4.0), c.weight(9.0)), (1.0, 1.0, 0.5, 0.0, 0.0));
        assert_eq!(wrap_deg(350.0), -10.0);
    }

    /// Model-space joint transforms of a skeleton under a sampled pose (parents before children).
    fn model_space(skel: &idres::md6::Md6Skel, pose: &idres::md6anim::Pose) -> Vec<Mat4> {
        let n = skel.names.len();
        let (mut r, mut t, mut s) = (skel.rotations.clone(), skel.translations.clone(), skel.scales.clone());
        for &(j, q) in &pose.rot {
            if (j as usize) < n {
                r[j as usize] = q;
            }
        }
        for &(j, v) in &pose.trans {
            if (j as usize) < n {
                t[j as usize] = v;
            }
        }
        for &(j, v) in &pose.scale {
            if (j as usize) < n {
                s[j as usize] = v;
            }
        }
        let mut out = vec![Mat4::IDENTITY; n];
        for j in 0..n {
            let l = Mat4::from_scale_rotation_translation(Vec3::from(s[j]), Quat::from_xyzw(r[j][0], r[j][1], r[j][2], r[j][3]).normalize(), Vec3::from(t[j]));
            let p = skel.parents[j];
            out[j] = if p < 0 { l } else { out[p as usize] * l };
        }
        out
    }

    /// syncentity11's attach joints at frame 0 of the zombie kills (install-backed): the victim (attach01) and the
    /// attacker (attach00) a melee step apart; a front kill puts the attacker in front of the victim.
    #[test]
    fn possessed_kill_attach_joints() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let c = inst.decls.container();
        let model = idres::md6::Md6Model::parse(&c.read_by_name("generated/basemodel/md6/objects/syncentities/syncentity11/assets/mesh/syncentity11.bmd6model").unwrap()).unwrap();
        let skel = idres::md6::Md6Skel::parse(&c.read_by_name(&format!("generated/skeleton/{}.bmd6skl", model.skeleton.trim_end_matches(".md6skl"))).unwrap()).unwrap();
        let j = |n: &str| skel.names.iter().position(|x| x.eq_ignore_ascii_case(n)).unwrap();
        let (a0, a1) = (j("attach00"), j("attach01"));
        for kill in ["kill_front_head", "kill_back_upper", "kill_left_upper"] {
            let anim = format!("md6/objects/syncentities/syncentity11/motion/from/player__human__base__mechanics__syncmelee__kill__zombie/{kill}.md6anim");
            let a = idres::md6anim::Md6Anim::parse(&c.read_by_name(&idres::animweb::anim_resource(&anim)).unwrap()).unwrap();
            let m = model_space(&skel, &a.sample(0.0));
            let (p, v) = (m[a0], m[a1]);
            eprintln!("{kill}: attach00 {:?} x {:?} | attach01 {:?} x {:?}", p.w_axis.truncate(), p.x_axis.truncate(), v.w_axis.truncate(), v.x_axis.truncate());
            // The attacker relative to the victim, in the victim's frame (its facing = x).
            let rel = rigid_inverse(v).transform_point3(p.w_axis.truncate());
            eprintln!("{kill}: attacker in victim frame {rel:?}, dist {:.1}", rel.truncate().length());
        }
        // The player body's anim against the arms mesh's skeleton.
        let body = idres::md6::Md6Model::parse(&c.read_by_name("generated/basemodel/md6/player/human/base/assets/mesh/arms.bmd6model").unwrap()).unwrap();
        let ba = idres::md6anim::Md6Anim::parse(&c.read_by_name(&idres::animweb::anim_resource("md6/player/human/base/motion/mechanics/syncmelee/kill/zombie/kill_front_head.md6anim")).unwrap()).unwrap();
        let bs = idres::md6::Md6Skel::parse(&c.read_by_name(&format!("generated/skeleton/{}.bmd6skl", body.skeleton.trim_end_matches(".md6skl"))).unwrap()).unwrap();
        let max_j = ba.rot.joints.iter().chain(ba.trans.joints.iter()).max().copied().unwrap_or(0);
        eprintln!("body mesh skeleton {} ({} joints) anim skeleton {} checksum {} max channel joint {max_j} rot channels {} frames {}", body.skeleton, bs.names.len(), ba.skeleton, ba.skel_checksum, ba.rot.joints.len(), ba.num_frames);
        let hand = bs.names.iter().position(|n| n.eq_ignore_ascii_case("righthand")).unwrap();
        let marine = idres::md6::Md6Skel::parse(&c.read_by_name(&format!("generated/skeleton/{}.bmd6skl", ba.skeleton.trim_end_matches(".md6skl"))).unwrap()).unwrap();
        let rt = retarget(&ba, &marine, &bs);
        let (m0, m20) = (model_space(&bs, &rt.sample(0.0)), model_space(&bs, &rt.sample(20.0)));
        let (h0, h20) = (m0[hand].w_axis.truncate(), m20[hand].w_axis.truncate());
        eprintln!("retargeted righthand frame 0 {h0:?} frame 20 {h20:?} (marine skeleton {} joints)", marine.names.len());
        // The punch: the right hand goes forward by frame 20 (ae_gloryKillShockwave).
        assert!(h20.x - h0.x > 20.0, "{h0} -> {h20}");
        let cam = bs.names.iter().position(|n| n.eq_ignore_ascii_case("camera")).unwrap();
        for f in [0.0, 10.0, 20.0, 30.0] {
            let m = model_space(&bs, &rt.sample(f));
            let rel = rigid_inverse(m[cam]).transform_point3(m[hand].w_axis.truncate());
            eprintln!("frame {f}: camera {:?} hand in camera {rel:?}; camera parent {}", m[cam].w_axis.truncate(), bs.parents[cam]);
        }
        // The Praetor suit mesh (the player's hands model) on the same anims.
        let pm = idres::md6::Md6Model::parse(&c.read_by_name("generated/basemodel/md6/player/human/base/assets/mesh/praetor.bmd6model").unwrap()).unwrap();
        let ps = idres::md6::Md6Skel::parse(&c.read_by_name(&format!("generated/skeleton/{}.bmd6skl", pm.skeleton.trim_end_matches(".md6skl"))).unwrap()).unwrap();
        let same = ps.names.iter().filter(|n| marine.names.iter().any(|m| m.eq_ignore_ascii_case(n))).count();
        eprintln!("praetor skeleton {} ({} joints, {same} names shared with marine, order equal {})", pm.skeleton, ps.names.len(), ps.names == marine.names);
        let prt = retarget(&ba, &marine, &ps);
        let (ph, pc) = (ps.names.iter().position(|n| n.eq_ignore_ascii_case("righthand")).unwrap(), ps.names.iter().position(|n| n.eq_ignore_ascii_case("camera")).unwrap());
        for f in [0.0, 20.0] {
            let m = model_space(&ps, &prt.sample(f));
            eprintln!("praetor frame {f}: hand in camera {:?}", rigid_inverse(m[pc]).transform_point3(m[ph].w_axis.truncate()));
        }
    }

    /// The Possessed's interactions from the install.
    #[test]
    fn possessed_sync_melee() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("loading install");
        let sm = SyncMelee::load(&inst.decls, crate::demons::POSSESSED).unwrap();
        assert_eq!(sm.ms_after_attack, 250);
        assert_eq!(sm.entity_defs[0], "zion/syncmelee/zombie");
        let fh = sm.interactions.iter().find(|i| i.name == "syncdeath/playervsai/zombie/front_head").unwrap();
        assert_eq!(fh.anims, [("killedByPlayer".to_string(), "kill_front_head".to_string())]);
        assert_eq!((fh.filter, fh.flags, fh.priority, fh.max_radius_z), (filter::STAGGER_STATE_1, flags::INIT_PLAYER, 0.0, 500.0));
        assert_eq!(fh.damage_groups, ["head"]);
        assert_eq!(fh.quadrants, quadrant::TOP_LEFT | quadrant::TOP_RIGHT);
        assert_eq!(fh.instigator_states, participant::ON_GROUND | participant::IN_AIR);
        let af = sm.interactions.iter().find(|i| i.name == "syncdeath/playervsai/zombie/above_front").unwrap();
        assert_eq!((af.priority, af.min_radius_z, af.filter), (2.0, 69.0, filter::STAGGER_STATE_1 | filter::DEATH_FROM_ABOVE));
        // The player-vs-AI kills a marine can trigger with the melee button.
        let n = sm.interactions.iter().filter(|i| i.inputs.iter().any(|x| x == "INPUT_ATTACK2") && i.flags & flags::INIT_PLAYER != 0).count();
        assert!(n >= 15, "{n}");
    }
}
