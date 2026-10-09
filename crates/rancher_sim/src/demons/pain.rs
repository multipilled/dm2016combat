//! Pain reactions: painType_t, the 14 leaky damage buckets (idLeakyBucket<gameTime_t>, idAI2 +0x8528) and the
//! SDPS reaction search over the threat-management decl (FindReaction 0x1406df2a0, reaction check 0x1406dfd00).

use super::decl::{BucketInfo, Reaction, ThreatDecl};

/// painType_t (enum table 0x14351cad8); also the bucket index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum PainType {
    #[default]
    None = 0,
    Twitch = 1,
    TwitchHeavy = 2,
    FalterLight = 3,
    Interrupt = 4,
    StunLoop = 5,
    Falter = 6,
    Pushback = 7,
    Stun = 8,
    Stagger = 9,
    StaggerVulnerable = 10,
    Knockdown = 11,
    Death = 12,
    FullGib = 13,
}

pub const NUM_PAIN_TYPES: usize = 14;

impl PainType {
    pub const ALL: [PainType; NUM_PAIN_TYPES] = [
        PainType::None,
        PainType::Twitch,
        PainType::TwitchHeavy,
        PainType::FalterLight,
        PainType::Interrupt,
        PainType::StunLoop,
        PainType::Falter,
        PainType::Pushback,
        PainType::Stun,
        PainType::Stagger,
        PainType::StaggerVulnerable,
        PainType::Knockdown,
        PainType::Death,
        PainType::FullGib,
    ];

    pub fn parse(s: &str) -> Self {
        match s {
            "PAIN_TWITCH" => Self::Twitch,
            "PAIN_TWITCH_HEAVY" => Self::TwitchHeavy,
            "PAIN_FALTER_LIGHT" => Self::FalterLight,
            "PAIN_INTERRUPT" => Self::Interrupt,
            "PAIN_STUN_LOOP" => Self::StunLoop,
            "PAIN_FALTER" => Self::Falter,
            "PAIN_PUSHBACK" => Self::Pushback,
            "PAIN_STUN" => Self::Stun,
            "PAIN_STAGGER" => Self::Stagger,
            "PAIN_STAGGER_VULNERABLE" => Self::StaggerVulnerable,
            "PAIN_KNOCKDOWN" => Self::Knockdown,
            "PAIN_DEATH" => Self::Death,
            "PAIN_FULL_GIB" => Self::FullGib,
            _ => Self::None,
        }
    }
    pub fn index(self) -> usize {
        self as usize
    }
    /// STAGGER / STAGGER_VULNERABLE (the "- 9U < 2" tests).
    pub fn is_stagger(self) -> bool {
        matches!(self, Self::Stagger | Self::StaggerVulnerable)
    }
}

/// idLeakyBucket<gameTime_t> (0x18): max, decayRate (per second), value, lastUpdate, decayDelay, delayTimer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LeakyBucket {
    pub max: f32,
    pub decay_rate: f32,
    pub value: f32,
    pub last_update: i32,
    pub decay_delay: i32,
    pub delay_timer: i32,
}

impl LeakyBucket {
    pub fn new(info: BucketInfo) -> Self {
        LeakyBucket { max: info.max, decay_rate: info.decay_rate, decay_delay: info.decay_delay, ..Default::default() }
    }

    fn dt(&mut self, now: i32) -> f32 {
        let dt = if self.last_update > 0 { (now - self.last_update) as f32 * 0.001 } else { 0.0 };
        self.last_update = now;
        dt
    }

    /// AddStimulus (0x1403f3f60, per bucket): the delay restarts first, so the decay step only runs for a
    /// negative decayDelay; the stimulus itself is added unclamped.
    pub fn add(&mut self, now: i32, s: f32) {
        let timer = self.decay_delay.wrapping_add(now);
        self.delay_timer = timer;
        let dt = self.dt(now);
        if dt > 0.0 && now > timer {
            self.value = (dt * self.decay_rate + self.value).min(self.max).max(0.0);
        }
        self.value += s;
    }

    /// Read with decay (0x1403fd620): returns the (decayed, clamped) value.
    pub fn read(&mut self, now: i32) -> f32 {
        let dt = self.dt(now);
        if dt > 0.0 && self.delay_timer < now {
            self.value = (dt * self.decay_rate + self.value).min(self.max).max(0.0);
        }
        self.value
    }

    /// 0x140747f90: value 0, lastUpdate = now.
    pub fn clear(&mut self, now: i32) {
        self.value = 0.0;
        self.last_update = now;
    }
}

/// Inputs of one reaction search.
#[derive(Debug, Clone)]
pub struct ReactionQuery<'a> {
    /// The damage decl that hit (mapping key).
    pub damage_decl: &'a str,
    /// The damage group hit had armor left (SDPSReaction_t.armored must match).
    pub armored_hit: bool,
    /// The AI's current pain reaction.
    pub current: PainType,
    /// Game time since each reaction last played (minRetriggerTime test), by pain type.
    pub since_last: [i32; NUM_PAIN_TYPES],
    /// Bucket values read this frame, by pain type.
    pub buckets: [f32; NUM_PAIN_TYPES],
    pub max_health: f32,
    pub health_fraction: f32,
    /// Splash radius fraction (dist^2 / radius^2), or -1 for direct hits.
    pub splash_fraction: f32,
    /// attacker scale * ai_pain_staggerEntryScale; scales STAGGER/STAGGER_VULNERABLE buckets.
    pub stagger_entry_scale: f32,
    /// cvar SDPS_NeverStagger.
    pub never_stagger: bool,
}

/// The reaction validity test 0x1406dfd00. `best_threshold` is the threshold of the last accepted reaction.
pub fn reaction_valid(r: &Reaction, q: &ReactionQuery, bucket: f32, health_fraction: f32, best_threshold: i32) -> bool {
    if r.armored != q.armored_hit || (q.never_stagger && r.reaction.is_stagger()) {
        return false;
    }
    let t = r.threshold(q.max_health);
    if !(t < bucket && ((best_threshold as f32) < t || r.reaction.is_stagger() || t == -1.0)) {
        return false;
    }
    let prereq_state_ok = r.prerequisite_state == PainType::None || r.prerequisite_state == q.current;
    let health_ok = health_fraction <= r.prerequisite_health_fraction || r.prerequisite_state != PainType::None;
    let splash_ok = q.splash_fraction <= 0.0 || r.splash_min < 0.0 || r.splash_max < 0.0 || (r.splash_min <= q.splash_fraction && q.splash_fraction <= r.splash_max);
    if !(health_ok && prereq_state_ok && splash_ok) {
        return false;
    }
    if !(q.current <= r.reaction || r.reaction == PainType::Stun) {
        return false;
    }
    r.min_retrigger_ms <= q.since_last[r.reaction.index()]
}

fn check(r: &Reaction, q: &ReactionQuery, best_threshold: i32) -> bool {
    let mut bucket = q.buckets[r.reaction.index()];
    let mut hf = q.health_fraction;
    if r.reaction.is_stagger() {
        bucket *= q.stagger_entry_scale;
        hf *= 1.0 / q.stagger_entry_scale;
    }
    reaction_valid(r, q, bucket, hf, best_threshold)
}

fn threshold_int(r: &Reaction, max_health: f32) -> i32 {
    if r.total_health_fraction <= 0.0 { r.total_damage as i32 } else { (r.total_health_fraction * max_health) as i32 }
}

/// FindReaction (0x1406df2a0) for a player attacker (the AI-vs-AI table is skipped).
pub fn find_reaction<'a>(t: &'a ThreatDecl, q: &ReactionQuery) -> Option<&'a Reaction> {
    // Specific table: reactions in order, each accepted one sets the threshold the next must beat.
    let mut specific: Option<&Reaction> = None;
    let mut had_mapping = false;
    for m in &t.mappings {
        if m.decl.as_deref() != Some(q.damage_decl) {
            continue;
        }
        had_mapping = true;
        let mut best_t = -1;
        for r in &m.reactions {
            if check(r, q, best_t) {
                specific = Some(r);
                best_t = threshold_int(r, q.max_health);
            }
        }
        if specific.is_some() {
            if !t.use_full_search {
                return specific;
            }
            break;
        }
    }
    if had_mapping && specific.is_none() && !t.use_full_search {
        return None;
    }
    // Default table: only pain types above the best so far; the highest valid type wins.
    let mut default: Option<&Reaction> = None;
    let mut best_type = PainType::None;
    let mut best_t = -1;
    for r in &t.default {
        if best_type < r.reaction && check(r, q, best_t) {
            best_type = r.reaction;
            default = Some(r);
            best_t = threshold_int(r, q.max_health);
        }
    }
    if !t.use_full_search {
        return default;
    }
    match (specific, default) {
        (Some(s), Some(d)) => Some(if s.reaction < d.reaction && s.reaction != PainType::Stun { d } else { s }),
        (s, d) => s.or(d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demons::decl::Mapping;

    fn zombie_threat() -> ThreatDecl {
        ThreatDecl {
            default: vec![
                Reaction { reaction: PainType::StaggerVulnerable, prerequisite_health_fraction: 0.6, on_recovery_health_fraction: 0.5, clear_this_bucket: true, clear_all_buckets: true, ..Default::default() },
                Reaction { reaction: PainType::Falter, total_health_fraction: 0.3, clear_this_bucket: true, ..Default::default() },
            ],
            mappings: vec![Mapping { decl: Some("damage/zion/firearm/sp/chaingun".into()), reactions: vec![Reaction { reaction: PainType::Falter, min_retrigger_ms: 500, clear_this_bucket: true, ..Default::default() }] }],
            overrides: vec![],
            use_full_search: true,
        }
    }

    fn query(bucket: f32, hf: f32) -> ReactionQuery<'static> {
        ReactionQuery {
            damage_decl: "damage/zion/firearm/sp/pistol",
            armored_hit: false,
            current: PainType::None,
            since_last: [i32::MAX; NUM_PAIN_TYPES],
            buckets: [bucket; NUM_PAIN_TYPES],
            max_health: 150.0,
            health_fraction: hf,
            splash_fraction: -1.0,
            stagger_entry_scale: 1.0,
            never_stagger: false,
        }
    }

    #[test]
    fn zombie_pistol_reactions() {
        let t = zombie_threat();
        // 2 body shots: 40 in the bucket, 110/150 health -> nothing.
        assert!(find_reaction(&t, &query(40.0, 110.0 / 150.0)).is_none());
        // Bucket past 45 -> falter.
        assert_eq!(find_reaction(&t, &query(60.0, 0.65)).map(|r| r.reaction), Some(PainType::Falter));
        // At 60% health the stagger-vulnerable reaction wins.
        assert_eq!(find_reaction(&t, &query(60.0, 0.6)).map(|r| r.reaction), Some(PainType::StaggerVulnerable));
    }

    #[test]
    fn bucket_clamps_on_read() {
        let mut b = LeakyBucket::new(BucketInfo { max: 150.0, decay_rate: 0.0, decay_delay: 0 });
        b.add(1000, 100.0);
        b.add(1100, 100.0);
        assert_eq!(b.value, 200.0);
        assert_eq!(b.read(1200), 150.0);
    }
}
