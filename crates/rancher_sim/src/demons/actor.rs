//! One demon as a standing damageable target: idAI2::Damage / Damage_Calculate, the pain / death update
//! (UpdateDeaths_internal 0x1404032b0), StartStaggeringPain (0x140402480) with CheckPainAnim's stationary
//! node choice, stagger timing and death tags (0x1403ff610). The demon's anim web is driven through
//! [`WebRequest`]s the caller applies to its `AnimWebRuntime`.

use std::sync::Arc;

use glam::Vec3;

use super::damage::{AiDamageParms, DamageEvent, DamageResult, location_damage, types};
use super::decl::{DemonDef, GroupKind, PainNode, PainSubGraph, WebTags};
use super::pain::{LeakyBucket, NUM_PAIN_TYPES, PainType, ReactionQuery, find_reaction};
use crate::weapons::GameRng;

/// Game ticks per second (the anim web clock).
pub const TICKS: i32 = 960;

/// Cvar defaults the demon code reads (exe cvar table).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AiCvars {
    /// ai_damageScale.
    pub damage_scale: f32,
    /// ai_unawareDamageScale (the range demons are aware of the player).
    pub unaware_damage_scale: f32,
    /// ai_pain_staggerEntryScale.
    pub stagger_entry_scale: f32,
    /// ai_pain_staggerTimescale.
    pub stagger_timescale: f32,
    /// ai_painBlendFrames.
    pub pain_blend_frames: i32,
    /// g_aiIncomingDamageScale_<difficulty> (all 1.0 by default).
    pub incoming_damage_scale: f32,
    /// ai_noDeath / ai_invulnerable.
    pub no_death: bool,
    pub invulnerable: bool,
    /// ai_allowStaggerPain.
    pub allow_stagger_pain: bool,
}

impl Default for AiCvars {
    fn default() -> Self {
        AiCvars {
            damage_scale: 1.0,
            unaware_damage_scale: 1.0,
            stagger_entry_scale: 1.0,
            stagger_timescale: 1.0,
            pain_blend_frames: 2,
            incoming_damage_scale: 1.0,
            no_death: false,
            invulnerable: false,
            allow_stagger_pain: true,
        }
    }
}

impl AiCvars {
    pub fn from_cvars(c: &crate::config::CvarValues, difficulty: usize) -> Self {
        let d = AiCvars::default();
        let f = |k: &str, def: f32| c.0.get(k).and_then(|v| idres::decl::parse_number(v)).unwrap_or(def);
        let names = ["EASY", "MEDIUM", "HARD", "NIGHTMARE", "NIGHTMARE"];
        AiCvars {
            damage_scale: f("ai_damageScale", d.damage_scale),
            unaware_damage_scale: f("ai_unawareDamageScale", d.unaware_damage_scale),
            stagger_entry_scale: f("ai_pain_staggerEntryScale", d.stagger_entry_scale),
            stagger_timescale: f("ai_pain_staggerTimescale", d.stagger_timescale),
            pain_blend_frames: f("ai_painBlendFrames", d.pain_blend_frames as f32) as i32,
            incoming_damage_scale: f(&format!("g_aiIncomingDamageScale_DIFFICULTY_{}", names[difficulty.min(4)]), 1.0),
            no_death: f("ai_noDeath", 0.0) != 0.0,
            invulnerable: f("ai_invulnerable", 0.0) != 0.0,
            allow_stagger_pain: f("ai_allowStaggerPain", 1.0) != 0.0,
        }
    }
}

/// A request for the demon's anim web.
#[derive(Debug, Clone, PartialEq)]
pub enum WebRequest {
    /// Put the web straight into a state (spawn).
    Set { sub: String, state: String },
    /// ForceState with a blend of `blend_frames` (30 Hz frames) and an immediate window.
    Force { sub: String, state: String, blend_frames: i32 },
    /// ChangeState (interruptPath 1, interruptBlend 1), optionally via a node.
    Change { sub: String, state: String, via: Option<(String, String)> },
    Scalar { name: String, value: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DemonPhase {
    Idle,
    /// A falter-type pain playing its node; back to Idle when the web reaches `idle`.
    Pain { reaction: PainType, sub: String, state: String },
    /// STAGGER / STAGGER_VULNERABLE: into, loop until `until`, then out.
    Stagger { vulnerable: bool, until: i32, recovering: bool, recovery_fraction: f32 },
    /// Dead at `at` (game ticks); the corpse fades from `at + removeTime` over fadeOutTime.
    Dead { at: i32 },
    Removed,
}

/// What happened this frame (for HUD / FX / logs).
#[derive(Debug, Clone, PartialEq)]
pub enum DemonEvent {
    Damaged(DamageResult),
    Pain { reaction: PainType, sub: String, state: String },
    Death { tags: Vec<String> },
    StaggerEnd,
    Removed,
}

/// One damage info of the frame (idAIDamageInfo entry) kept for the pain update.
#[derive(Debug, Clone)]
struct FrameDamage {
    parms: Arc<AiDamageParms>,
    result: DamageResult,
    dir: Vec3,
    splash_fraction: f32,
    armored: bool,
}

pub struct Demon {
    pub def: Arc<DemonDef>,
    pub tags: Arc<WebTags>,
    pub origin: Vec3,
    /// Facing yaw in degrees (idTech: x forward, y left).
    pub yaw: f32,
    pub health: f32,
    pub shield: f32,
    pub buckets: [LeakyBucket; NUM_PAIN_TYPES],
    pub current_pain: PainType,
    pub last_reaction_times: [i32; NUM_PAIN_TYPES],
    pub phase: DemonPhase,
    pub injured_state: usize,
    /// Difficulty index 0..4 (DIFFICULTY_EASY .. NIGHTMARE).
    pub difficulty: usize,
    pub cvars: AiCvars,
    /// The game's LCG (gameLocal+0x285be8): pain anim picks.
    pub rng: GameRng,
    pub requests: Vec<WebRequest>,
    /// A request issued once the web has entered `(sub, state)` (the AI asks for the next node only after
    /// the forced pain edge was taken; one stored request at a time in the web).
    pub follow: Option<((String, String), WebRequest)>,
    frame: Vec<FrameDamage>,
    mitigation: Option<(i32, f32, f32, bool, f32)>,
    /// The sub-web whose idle the demon starts in and returns to after pains / staggers (AISUBWEB_COMBAT:
    /// hands_combat; rifle_combat for the Possessed Soldier). INTERIM: set by the caller (the aiSubWeb_t -> name
    /// mapping is not decoded).
    pub combat_sub: String,
}

/// DAMAGEDIR from a damage direction (0x140749f50): `fwd` / `left` are the demon's axis, `n` the node's
/// numDamageDirs. 1 front 2 back 3 left 4 right (8-way: 5 frontleft 6 frontright 7 backleft 8 backright).
pub fn damage_dir(fwd: Vec3, left: Vec3, d: Vec3, n: u32) -> u32 {
    let f = d.dot(fwd);
    let l = d.dot(left);
    match n {
        2 => (f > 0.0) as u32 + 1,
        4 => {
            if f >= 0.707_106_77 {
                2
            } else if f > -0.707_106_77 {
                (l > -0.707_106_77) as u32 + 3
            } else {
                1
            }
        }
        8 => {
            if l >= 0.0 {
                if f < -0.92387 {
                    1
                } else if f < -0.38263 {
                    6
                } else if f < 0.38263 {
                    4
                } else if f < 0.92387 {
                    8
                } else {
                    2
                }
            } else if f < -0.92387 {
                1
            } else if f < -0.38263 {
                5
            } else if f < 0.38263 {
                3
            } else if f < 0.92387 {
                7
            } else {
                2
            }
        }
        _ => 1,
    }
}

/// Direction names (atomic strings 0x144414b00).
pub const DIR_NAMES: [&str; 9] = ["none", "front", "back", "left", "right", "frontleft", "frontright", "backleft", "backright"];
/// Intensity names (0x144414b48).
pub const INTENSITY_NAMES: [&str; 4] = ["none", "light", "medium", "heavy"];

impl Demon {
    pub fn new(def: Arc<DemonDef>, tags: Arc<WebTags>, origin: Vec3, yaw: f32, difficulty: usize, cvars: AiCvars, seed: u32) -> Self {
        let mut buckets = [LeakyBucket::default(); NUM_PAIN_TYPES];
        for t in PainType::ALL {
            buckets[t.index()] = LeakyBucket::new(def.bucket_info(t));
        }
        let health = def.health;
        let shield = def.shield;
        let injured_state = def.initial_injured_state;
        let mut d = Demon {
            def,
            tags,
            origin,
            yaw,
            health,
            shield,
            buckets,
            current_pain: PainType::None,
            last_reaction_times: [i32::MIN / 2; NUM_PAIN_TYPES],
            phase: DemonPhase::Idle,
            injured_state,
            difficulty,
            cvars,
            rng: GameRng(seed),
            requests: Vec::new(),
            follow: None,
            frame: Vec::new(),
            mitigation: None,
            combat_sub: "hands_combat".into(),
        };
        d.requests.push(WebRequest::Set { sub: "hands_combat".into(), state: "idle".into() });
        d
    }

    /// Plays AISUBWEB_COMBAT on `sub` (see `combat_sub`): the start request and the pain / stagger returns.
    pub fn set_combat_sub(&mut self, sub: &str) {
        self.combat_sub = sub.to_string();
        for r in &mut self.requests {
            if let WebRequest::Set { sub: s, .. } = r
                && s == "hands_combat"
            {
                *s = sub.to_string();
            }
        }
    }

    pub fn max_health(&self) -> f32 {
        self.def.health + self.def.shield
    }
    pub fn health_fraction(&self) -> f32 {
        let m = self.max_health();
        if m > 0.0 { (self.health + self.shield) / m } else { 0.0 }
    }
    /// In the vulnerable stagger (PAIN_STAGGER_VULNERABLE): the glory-kill filter SYNCFILTER_STAGGER_STATE_1.
    pub fn stagger_vulnerable(&self) -> bool {
        matches!(self.phase, DemonPhase::Stagger { vulnerable: true, .. })
    }

    /// The glory kill's `ae_kill` (victim anim): health to 0 and dead now, without the death anim request (the
    /// sync entity's web drives the pose to its end). Clears pending requests. INTERIM: the handler of ae_kill was
    /// not decoded (no damage event, no death tags).
    pub fn sync_kill(&mut self, now: i32) {
        self.health = 0.0;
        self.requests.clear();
        self.follow = None;
        self.current_pain = PainType::Death;
        self.phase = DemonPhase::Dead { at: now };
    }

    pub fn is_dead(&self) -> bool {
        matches!(self.phase, DemonPhase::Dead { .. } | DemonPhase::Removed)
    }
    /// Forward and left axis.
    pub fn axis(&self) -> (Vec3, Vec3) {
        let (s, c) = self.yaw.to_radians().sin_cos();
        (Vec3::new(c, s, 0.0), Vec3::new(-s, c, 0.0))
    }

    /// idAI2::Damage -> Damage_Calculate (0x1403fa6b0) for one call. `now` in game ticks.
    pub fn damage(&mut self, ev: &DamageEvent, parms: Arc<AiDamageParms>, now: i32) -> Option<DamageResult> {
        if self.is_dead() || !self.def.behaviors.can_take_damage || self.cvars.invulnerable {
            return None;
        }
        // Base: GetDamage over the attacker -> self ORIGIN distance, times the call's scale.
        let base = match ev.attacker_origin {
            Some(a) => parms.at_distance_sq(a.distance_squared(self.origin)) * ev.scale,
            None => parms.max * ev.scale,
        };
        let mut scaled = base;
        // Difficulty: gamedifficulty decl scale (1.0) * behaviors difficultyDamageScale[difficulty] * cvar.
        let di = self.difficulty.min(4);
        scaled *= self.cvars.incoming_damage_scale * self.def.behaviors.difficulty_damage_scale[di];
        // Aware of the attacker: no ai_unawareDamageScale. Component / perk / multi-attacker scales are 1.
        scaled *= self.cvars.damage_scale;
        // aiDamageMitigation (0x1403ff340): first entry for this monster type, per-frame max cap.
        if let Some(m) = parms.mitigation.iter().find(|m| m.monster_types & self.def.behaviors.monster_type != 0) {
            let cap = if m.per_frame { self.mitigation.filter(|x| x.0 == now).map(|x| x.4).unwrap_or(0.0) } else { 0.0 };
            self.mitigation = Some((now, m.max_damage, m.scalar, m.per_frame, cap));
            scaled *= m.scalar;
            if m.max_damage != -1.0 {
                let left = m.max_damage - cap;
                if left <= 0.0 {
                    scaled = 0.0;
                } else if m.max_damage < cap + scaled {
                    scaled = left;
                }
            }
        }
        let mut res = DamageResult { base, scaled, ..Default::default() };
        // Per trace: the joint's damage group, damage split evenly over the traces.
        let n = ev.traces.len().max(1);
        let f = 1.0 / n as f32;
        let traces: Vec<Option<String>> = if ev.traces.is_empty() { vec![None] } else { ev.traces.iter().map(|t| t.joint.clone()).collect() };
        let mut armored = false;
        for (i, j) in traces.iter().enumerate() {
            let group = j.as_deref().and_then(|j| self.def.damage_group_of_joint(j));
            let parts: Vec<([f32; 4], bool, bool)> = match group {
                Some(g) => {
                    let armor = self.def.groups[g].location_armor;
                    let (o, head) = location_damage(&self.def, &parms, g, armor, f * scaled);
                    vec![(o, head, armor > 0.0)]
                }
                // No group: spread over every group (0x14082fab0 with index -1).
                None => {
                    let ng = self.def.groups.len().max(1);
                    (0..self.def.groups.len())
                        .map(|g| {
                            let armor = self.def.groups[g].location_armor;
                            let (o, head) = location_damage(&self.def, &parms, g, armor, f * scaled / ng as f32);
                            (o, head, armor > 0.0)
                        })
                        .collect()
                }
            };
            for (o, head, arm) in parts {
                res.health += o[0];
                res.armor += o[1];
                res.head_shot |= head;
                armored |= arm;
            }
            if i == 0 {
                res.joint = j.clone();
                res.group = group;
            }
        }
        // Pain stimulus: aiStimulusScale * health damage * threat stimulusScale (1.0 in every decl).
        let stim = parms.ai_stimulus_scale * res.health;
        for b in &mut self.buckets {
            b.add(now, stim);
        }
        // Kill rule: damage without DAMAGETYPE_HEALTH cannot kill; noDeath leaves 1 hp.
        let hp = self.health + self.shield;
        if res.health >= hp && parms.damage_types & types::HEALTH == 0 {
            res.health = res.health.min(hp - 0.1);
        }
        if (parms.no_death || self.cvars.no_death) && res.health >= hp {
            res.health = res.health.min(hp - 1.0);
        }
        // idShieldHealthT: the shield component takes damage first, the rest goes to hitpoints.
        let mut d = res.health;
        let s = d.min(self.shield);
        self.shield -= s;
        d -= s;
        self.health -= d;
        res.killed = self.health <= 0.0;
        self.frame.push(FrameDamage { parms, result: res.clone(), dir: ev.dir, splash_fraction: ev.splash_fraction, armored });
        Some(res)
    }

    /// The per-frame pain / death update. `now` in game ticks.
    pub fn update(&mut self, now: i32, current: Option<(&str, &str)>) -> Vec<DemonEvent> {
        let mut ev: Vec<DemonEvent> = self.frame.iter().map(|f| DemonEvent::Damaged(f.result.clone())).collect();
        if let Some(((s, n), _)) = &self.follow
            && current == Some((s.as_str(), n.as_str()))
        {
            let (_, r) = self.follow.take().unwrap();
            self.requests.push(r);
        }
        let frame = std::mem::take(&mut self.frame);
        match self.phase.clone() {
            DemonPhase::Removed => return ev,
            DemonPhase::Dead { at } => {
                if self.def.remove_time_ms > 0 && now - at >= self.remove_ticks() + (self.def.fade_out_s * TICKS as f32) as i32 {
                    self.phase = DemonPhase::Removed;
                    ev.push(DemonEvent::Removed);
                }
                return ev;
            }
            _ => {}
        }
        if self.health <= 0.0 {
            let tags = self.start_death(&frame, now);
            ev.push(DemonEvent::Death { tags });
            return ev;
        }
        // Strongest reaction over the frame's damage infos.
        let mut best: Option<(super::decl::Reaction, usize)> = None;
        for (i, fd) in frame.iter().enumerate() {
            if !fd.parms.causes_pain {
                continue;
            }
            let mut buckets = [0.0f32; NUM_PAIN_TYPES];
            for (k, b) in self.buckets.iter_mut().enumerate() {
                buckets[k] = b.read(now);
            }
            let since = self.last_reaction_times.map(|t| now.saturating_sub(t));
            let q = ReactionQuery {
                damage_decl: &fd.parms.name,
                armored_hit: fd.armored,
                current: self.current_pain,
                since_last: since,
                buckets,
                max_health: self.max_health(),
                health_fraction: self.health_fraction(),
                splash_fraction: fd.splash_fraction,
                stagger_entry_scale: self.cvars.stagger_entry_scale,
                never_stagger: false,
            };
            if let Some(r) = find_reaction(&self.def.threat, &q)
                && best.as_ref().is_none_or(|(b, _)| b.reaction < r.reaction)
            {
                best = Some((r.clone(), i));
            }
        }
        // TWITCH / TWITCH_HEAVY are additive twitch pains (declTwitchPain): not ported.
        if let Some((r, i)) = best
            && r.reaction >= PainType::FalterLight
            && let Some(e) = self.start_pain(&r, &frame[i], now)
        {
            ev.push(e);
        }
        // Phase bookkeeping.
        match self.phase.clone() {
            DemonPhase::Pain { .. } => {
                if current == Some((self.combat_sub.as_str(), "idle")) {
                    self.phase = DemonPhase::Idle;
                    self.current_pain = PainType::None;
                }
            }
            DemonPhase::Stagger { vulnerable, until, recovering, recovery_fraction } => {
                if !recovering && now >= until {
                    // Recovery: health := max(health, onRecovery_healthFraction * max), then out of the loop.
                    if recovery_fraction > 0.0 {
                        let min = recovery_fraction * self.max_health();
                        if self.health + self.shield < min {
                            self.health = min - self.shield;
                        }
                    }
                    self.requests.push(WebRequest::Change { sub: self.combat_sub.clone(), state: "idle".into(), via: Some(("stagger".into(), "out_stagger".into())) });
                    self.phase = DemonPhase::Stagger { vulnerable, until, recovering: true, recovery_fraction };
                    ev.push(DemonEvent::StaggerEnd);
                } else if recovering && current == Some((self.combat_sub.as_str(), "idle")) {
                    self.phase = DemonPhase::Idle;
                    self.current_pain = PainType::None;
                }
            }
            _ => {}
        }
        ev
    }

    fn remove_ticks(&self) -> i32 {
        self.def.remove_time_ms * TICKS / 1000
    }

    /// Corpse opacity: 1 until removeTime, then a linear fade over fadeOutTime. INFERRED curve.
    pub fn fade(&self, now: i32) -> f32 {
        match self.phase {
            DemonPhase::Dead { at } => {
                let t = now - at - self.remove_ticks();
                if t <= 0 || self.def.fade_out_s <= 0.0 { 1.0 } else { (1.0 - t as f32 / (self.def.fade_out_s * TICKS as f32)).max(0.0) }
            }
            DemonPhase::Removed => 0.0,
            _ => 1.0,
        }
    }

    fn web_node(&self, path: &str) -> Option<(String, String)> {
        let rest = path.strip_prefix(&self.def.anim_web).map(|r| r.trim_start_matches('/')).unwrap_or(path);
        let (sub, state) = rest.rsplit_once('/')?;
        Some((sub.to_string(), state.to_string()))
    }

    /// CheckPainAnim's entry node for a standing demon: a single node; else an enabled, non-proxy,
    /// non-fallback node with no motion dir; else the subgraph's proxy, which leads to another subgraph.
    fn pain_node<'a>(&'a self, mut sg: &'a PainSubGraph) -> Option<&'a PainNode> {
        for _ in 0..4 {
            if sg.nodes.len() == 1 && !sg.nodes[0].subgraph_proxy {
                return sg.nodes.first();
            }
            if let Some(n) = sg.nodes.iter().find(|n| n.enabled && !n.subgraph_proxy && !n.fallback_only && !n.internal_only && n.motion_dir.is_empty() && n.pain_flags & 2 == 0) {
                return Some(n);
            }
            // Proxy: follow its link to a proxy of another subgraph, then choose there.
            let proxy = sg.nodes.iter().find(|n| n.subgraph_proxy)?;
            let link = sg.links.iter().find(|l| l.start == proxy.name)?;
            sg = self.def.pain_graph.subgraphs.iter().find(|s| s.nodes.iter().any(|n| n.name == link.end))?;
        }
        None
    }

    /// StartStaggeringPain (0x140402480) for the chosen reaction.
    fn start_pain(&mut self, r: &super::decl::Reaction, fd: &FrameDamage, now: i32) -> Option<DemonEvent> {
        if r.reaction.is_stagger() && !self.cvars.allow_stagger_pain {
            return None;
        }
        // cur and last both STAGGER / STAGGER_VULNERABLE: no restart.
        if self.current_pain == PainType::Stagger && r.reaction == PainType::Stagger {
            return None;
        }
        if self.current_pain == PainType::StaggerVulnerable && r.reaction.is_stagger() {
            return None;
        }
        let sg = self.def.pain_graph.subgraph(r.reaction)?;
        let node = self.pain_node(sg)?.clone();
        let (fwd, left) = self.axis();
        let mut dir = damage_dir(fwd, left, fd.dir, node.num_damage_dirs);
        if node.pain_flags & 8 != 0 {
            dir = 1;
        }
        let anim = node.anims.iter().find(|a| a.damage_dir == dir).or_else(|| node.anims.first())?;
        let (sub, state) = self.web_node(&anim.web_node)?;
        // painIndex: aliases whose tags contain the hit joint's pain group and the injured state
        // (0x1403fc730 -> 0x1405218f0, non-exact), random pick; PAINFLAG_IGNORE_TAGS skips it.
        if node.pain_flags & 0x10 == 0 {
            let mut req: Vec<String> = Vec::new();
            if let Some(j) = &fd.result.joint
                && let Some(g) = self.def.joint_groups.group_of(GroupKind::Pain, j)
            {
                req.push(g.name.clone());
            }
            if let Some(s) = self.def.injured.get(self.injured_state) {
                req.push(s.name.clone());
            }
            if let Some(aliases) = self.tags.aliases(&sub, &state) {
                let cands: Vec<usize> = aliases.iter().enumerate().filter(|(_, t)| req.iter().all(|r| t.iter().any(|x| x == r))).map(|(i, _)| i).collect();
                if !cands.is_empty() {
                    let k = self.rng.next15() as usize % cands.len();
                    self.requests.push(WebRequest::Scalar { name: "painIndex".into(), value: cands[k] as f32 });
                }
            }
        }
        // Bucket clearing, reaction bookkeeping.
        if r.clear_all_buckets {
            for b in &mut self.buckets {
                b.clear(now);
            }
        } else if r.clear_this_bucket {
            self.buckets[r.reaction.index()].clear(now);
        }
        self.current_pain = r.reaction;
        self.last_reaction_times[r.reaction.index()] = now;
        self.requests.push(WebRequest::Force { sub: sub.clone(), state: state.clone(), blend_frames: self.cvars.pain_blend_frames });
        if r.reaction.is_stagger() && node.pain_flags & 1 != 0 {
            // staggerLength (behaviors +0xec / +0xf0) * damage staggerTimeScale (other factors 1).
            let base = if r.reaction == PainType::Stagger { self.def.behaviors.stagger_length_ms } else { self.def.behaviors.stagger_vulnerable_ms };
            let len = (base as f32 * fd.parms.stagger_time_scale) as i32 * TICKS / 1000;
            self.follow = Some(((sub.clone(), state.clone()), WebRequest::Change { sub: sub.clone(), state: "loop_stagger".into(), via: None }));
            self.phase = DemonPhase::Stagger { vulnerable: r.reaction == PainType::StaggerVulnerable, until: now + len, recovering: false, recovery_fraction: r.on_recovery_health_fraction };
        } else {
            self.follow = Some(((sub.clone(), state.clone()), WebRequest::Change { sub: self.combat_sub.clone(), state: "idle".into(), via: None }));
            self.phase = DemonPhase::Pain { reaction: r.reaction, sub: sub.clone(), state: state.clone() };
        }
        Some(DemonEvent::Pain { reaction: r.reaction, sub, state })
    }

    /// Death (0x1403ff610): tags on the web's tag scalars, then the death node.
    fn start_death(&mut self, frame: &[FrameDamage], now: i32) -> Vec<String> {
        let last = frame.last();
        let mut tags: Vec<String> = Vec::new();
        // moving / movingSlow: the range demons stand.
        let dmg: f32 = frame.iter().map(|f| f.result.health).sum();
        let intensity = match last.map(|f| f.parms.intensity).unwrap_or(0) {
            0 => {
                let frac = if self.max_health() > 0.0 { dmg / self.max_health() } else { 1.0 };
                if frac < 0.26 {
                    1
                } else if frac < 0.66 {
                    2
                } else {
                    3
                }
            }
            i => i as usize,
        };
        match intensity {
            1 => tags.push("light".into()),
            2 => {
                tags.push("light".into());
                tags.push("medium".into());
            }
            _ => tags.push("heavy".into()),
        }
        let (fwd, left) = self.axis();
        let dir = last.map(|f| damage_dir(fwd, left, f.dir, 4)).unwrap_or(1);
        tags.push(DIR_NAMES[dir as usize].into());
        if let Some(j) = last.and_then(|f| f.result.joint.as_deref())
            && let Some(g) = self.def.joint_groups.group_of(GroupKind::Death, j)
        {
            tags.push(g.name.clone());
        }
        match self.def.injured.get(self.injured_state) {
            Some(s) => tags.push(s.name.clone()),
            None => tags.push("notinjured".into()),
        }
        let src = last.map(|f| f.parms.damage_source).unwrap_or(0);
        if src == 2 || src == 4 {
            tags.push("src_melee".into());
        } else if src == 0x10000 {
            tags.push("src_push_back".into());
        }
        // Every tag scalar of the death tree to 0, then the death tags to 1.
        for t in self.tags.tag_names("death", "death") {
            self.requests.push(WebRequest::Scalar { name: t, value: 0.0 });
        }
        for t in &tags {
            self.requests.push(WebRequest::Scalar { name: t.clone(), value: 1.0 });
        }
        // INFERRED: the death request (anim request type 3, blend -1) uses the web's default blend.
        self.follow = None;
        self.requests.push(WebRequest::Force { sub: "death".into(), state: "death".into(), blend_frames: -1 });
        self.current_pain = PainType::Death;
        self.phase = DemonPhase::Dead { at: now };
        tags
    }
}
