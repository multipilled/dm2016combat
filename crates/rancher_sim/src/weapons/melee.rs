//! Player melee: the idDeclWeapon melee data and idMeleeTrace (hands+0x101c8), decoded from DOOMx64.exe
//! (notes: gamedata/re/WEAPONS.md "Melee"; the idHands side is in weapons::hands).
//!
//! - The hands anims start a trace with ae_handsStartJointMeleeTrace (slot "right_hand", joint "melee_impact"
//!   on every SP gun's melee_1 / melee_1_miss, frame 0) and stop it with ae_handsEndMeleeTrace (melee_1_out
//!   frame 3). Start (0x140d55770 -> 0x140d82260 -> 0x140ef8850): projectile = meleeSlashProjectile, else
//!   the directional / sprint / one-hit-kill variant when that melee runs, else meleeProjectile; damage type,
//!   bounds and cap come from the projectile when it overrides them, else from the weapon; the trace's
//!   position is the joint, then 0x140ef83a0 sweeps from the eye to it.
//! - Every frame after UpdateWeapon (0x140d7c980, before the animator, so with last frame's pose): sweep
//!   from the previous position to joint + view forward * meleeTraceOffset (0x140ef8ab0), then from the eye
//!   to that point (0x140ef83a0); g_meleeTracePreviousPositionUpdate 1 keeps the end as the next start.
//!   The sweeps are asynchronous (0x141638b60): a sweep's result is read on the next update.
//! - Hits (0x140ef7b90): MELEE_FIRST_PLUS_ACTOR damages the first non-actor only, an actor once and then
//!   stops; IMPACT stops at the first hit, SEMI_CONTINUOUS at the first actor; damage at most every
//!   meleeTraceDamageIntervalMS (world: ...IntervalWorldMS when >= 0); a positive cap loses the damage decl's
//!   maxDamage per actor hit and stops the trace at 0.

use glam::Vec3;

use super::decl::ProjectileDef;

/// meleeDamage_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeleeDamageType {
    None = -1,
    Impact = 0,
    #[default]
    SemiContinuous = 1,
    FirstPlusActor = 2,
    Continuous = 3,
}

impl MeleeDamageType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "MELEE_NONE" => Self::None,
            "MELEE_IMPACT" => Self::Impact,
            "MELEE_SEMI_CONTINUOUS" => Self::SemiContinuous,
            "MELEE_FIRST_PLUS_ACTOR" => Self::FirstPlusActor,
            "MELEE_CONTINUOUS" => Self::Continuous,
            _ => return None,
        })
    }
}

/// meleeBounds_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeleeBounds {
    None = -1,
    #[default]
    Line = 0,
    B8 = 1,
    B16 = 2,
    B24 = 3,
    B32 = 4,
    B48 = 5,
    B96 = 6,
    PlayerMelee = 7,
    Custom = 8,
}

impl MeleeBounds {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "BOUNDS_NONE" => Self::None,
            "BOUNDS_LINE" => Self::Line,
            "BOUNDS_8x8" => Self::B8,
            "BOUNDS_16x16" => Self::B16,
            "BOUNDS_24x24" => Self::B24,
            "BOUNDS_32x32" => Self::B32,
            "BOUNDS_48x48" => Self::B48,
            "BOUNDS_96x96" => Self::B96,
            "BOUNDS_PLAYER_MELEE" => Self::PlayerMelee,
            "BOUNDS_CUSTOM" => Self::Custom,
            _ => return None,
        })
    }

    /// Half extent of the swept box (0 = a line). INFERRED from the names: the trace models are built at
    /// game start (trace manager gameLocal+0xe0878, slots +0xe0890..+0xe08d8), sizes not read from the exe.
    /// PLAYER_MELEE / CUSTOM are not ported (line).
    pub fn half_extent(self) -> f32 {
        match self {
            Self::B8 => 4.0,
            Self::B16 => 8.0,
            Self::B24 => 12.0,
            Self::B32 => 16.0,
            Self::B48 => 24.0,
            Self::B96 => 48.0,
            _ => 0.0,
        }
    }
}

/// A melee projectile decl (damageDecl + the melee trace overrides).
#[derive(Debug, Clone, Default)]
pub struct MeleeProjectile {
    pub def: ProjectileDef,
    /// +0x214 / +0x218 / +0x21c: override the weapon when not BOUNDS_NONE / MELEE_NONE / >= 0.
    pub bounds: MeleeBounds,
    pub damage_type: MeleeDamageType,
    pub damage_cap: f32,
}

/// The idDeclWeapon melee fields (ctor 0x1406f0780 defaults in Default).
#[derive(Debug, Clone)]
pub struct MeleeDecl {
    /// +0x510 (fists).
    pub melee_from_fire_input: bool,
    /// +0x511.
    pub fire_from_melee_input: bool,
    /// +0x512 (ctor 1).
    pub melee_from_melee_input: bool,
    /// +0x513.
    pub melee_to_shoot_state: bool,
    pub melee_alternating: bool,
    /// +0x515 (fists): left / right punches.
    pub melee_ltrt: bool,
    /// +0x7bb (ctor 1).
    pub has_sprint_melee: bool,
    /// +0xd08.
    pub has_directional_melee: bool,
    pub has_directional_and_staged_melee: bool,
    /// +0xa18 meleeProjectile, +0xa20 slash, +0xa28 sprint, +0xa30 one-hit-kill, +0xa38 directional.
    pub projectile: Option<MeleeProjectile>,
    pub slash_projectile: Option<MeleeProjectile>,
    pub sprint_projectile: Option<MeleeProjectile>,
    pub one_hit_kill_projectile: Option<MeleeProjectile>,
    pub directional_projectile: Option<MeleeProjectile>,
    /// +0xa40.
    pub trace_offset: f32,
    /// +0xa50 (ctor BOUNDS_LINE).
    pub bounds: MeleeBounds,
    /// +0xa54 (ctor MELEE_SEMI_CONTINUOUS).
    pub damage_type: MeleeDamageType,
    /// +0xa58 (0 = no cap).
    pub damage_cap: f32,
    /// +0xa5c.
    pub damage_interval_ms: i32,
    /// +0xa60 (-1 = use damage_interval_ms).
    pub damage_interval_world_ms: i32,
    /// +0xa64.
    pub immediate_transition: bool,
}

impl Default for MeleeDecl {
    fn default() -> Self {
        Self {
            melee_from_fire_input: false,
            fire_from_melee_input: false,
            melee_from_melee_input: true,
            melee_to_shoot_state: false,
            melee_alternating: false,
            melee_ltrt: false,
            has_sprint_melee: true,
            has_directional_melee: false,
            has_directional_and_staged_melee: false,
            projectile: None,
            slash_projectile: None,
            sprint_projectile: None,
            one_hit_kill_projectile: None,
            directional_projectile: None,
            trace_offset: 0.0,
            bounds: MeleeBounds::Line,
            damage_type: MeleeDamageType::SemiContinuous,
            damage_cap: 0.0,
            damage_interval_ms: 0,
            damage_interval_world_ms: -1,
            immediate_transition: false,
        }
    }
}

/// What a sweep hit (the caller's world / entity trace).
#[derive(Debug, Clone, PartialEq)]
pub struct SweepHit {
    pub fraction: f32,
    pub pos: Vec3,
    pub normal: Vec3,
    /// A target dummy (index into the caller's list); None = the world or a demon.
    pub target: Option<usize>,
    /// A demon (id, hit-sphere joint).
    pub demon: Option<(u32, String)>,
    /// idActor (FUN_14135e0c0): demons are, the testbed's dummies are not.
    pub actor: bool,
}

/// A hit the trace reports (FUN_140ef7b90), with the damage dealt (None while the interval runs).
#[derive(Debug, Clone, PartialEq)]
pub struct MeleeHit {
    pub hit: SweepHit,
    pub damage: Option<f32>,
    pub weapon: usize,
    /// Projectile decl (impactEffectTable for the FX).
    pub projectile: String,
    /// Its damage decl.
    pub damage_decl: String,
    /// Normalised sweep direction.
    pub dir: Vec3,
}

/// idMeleeTrace (hands+0x101c8).
#[derive(Debug, Clone, Default)]
pub struct MeleeTrace {
    /// +0x78: remaining sweeps (-1 = until stopped, 0 = off).
    pub count: i32,
    /// +0x7c.
    pub cap: f32,
    /// +0x80 / +0x84.
    pub interval: i32,
    pub interval_world: i32,
    /// +0x8c: next time damage may be dealt.
    pub next_damage: i32,
    /// +0x90 / +0x91.
    pub damage_type: MeleeDamageType,
    pub bounds: MeleeBounds,
    /// +0x77: something was hit.
    pub hit_any: bool,
    /// Hands model joint followed (+0x50).
    pub joint: String,
    /// +0x94.
    pub pos: Vec3,
    pub weapon: usize,
    pub projectile: String,
    pub damage_decl: String,
    /// The damage decl's maxDamage (decl +0x140 = damageParms +0xd0).
    pub damage: f32,
    pub offset: f32,
    /// The trace was started by this frame's anim events: its first update sets the position.
    pub starting: bool,
    /// meleeImmediateTransition (decl +0xa64).
    pub immediate: bool,
    /// Sweeps issued last update (start, end, eye sweep), read on this one.
    pending: Vec<(Vec3, Vec3, bool)>,
}

impl MeleeTrace {
    pub fn active(&self) -> bool {
        self.count != 0
    }

    /// 0x140ef7b80.
    pub fn stop(&mut self) {
        self.count = 0;
        self.starting = false;
        self.pending.clear();
    }

    /// 0x140d82260 -> 0x140ef8850: start following `joint` with the weapon's melee data.
    pub fn start(&mut self, weapon: usize, melee: &MeleeDecl, proj: &MeleeProjectile, joint: &str) {
        let damage_type = if proj.damage_type != MeleeDamageType::None { proj.damage_type } else { melee.damage_type };
        let bounds = if proj.bounds != MeleeBounds::None { proj.bounds } else { melee.bounds };
        let cap = if proj.damage_cap >= 0.0 { proj.damage_cap } else { melee.damage_cap };
        *self = MeleeTrace {
            count: -1,
            cap,
            interval: melee.damage_interval_ms,
            interval_world: melee.damage_interval_world_ms,
            next_damage: 0,
            damage_type,
            bounds,
            hit_any: false,
            joint: joint.to_string(),
            pos: Vec3::ZERO,
            weapon,
            projectile: proj.def.name.clone(),
            damage_decl: proj.def.damage.name.clone(),
            damage: proj.def.damage.max,
            offset: melee.trace_offset,
            starting: true,
            immediate: melee.immediate_transition,
            pending: Vec::new(),
        };
    }

    /// One update (0x140d7c980): read last update's sweeps, then issue this one's. `joint` is the followed
    /// joint's world position (last frame's pose), `eye` / `forward` the view. `sweep(start, end, half)`
    /// returns the first hit.
    pub fn update(&mut self, now: i32, joint: Vec3, eye: Vec3, forward: Vec3, sweep: &mut dyn FnMut(Vec3, Vec3, f32) -> Option<SweepHit>) -> Vec<MeleeHit> {
        let mut out = Vec::new();
        if !self.active() {
            return out;
        }
        let half = self.bounds.half_extent();
        for (a, b, eye_sweep) in std::mem::take(&mut self.pending) {
            if !self.active() {
                break;
            }
            if let Some(h) = sweep(a, b, half) {
                if let Some(m) = self.on_hit(now, h, a, b, eye_sweep) {
                    out.push(m);
                }
            }
        }
        if !self.active() {
            return out;
        }
        let p = joint + forward.normalize_or_zero() * self.offset;
        if self.starting {
            // 0x140ef8850 places the trace at the joint; 0x140ef83a0 sweeps eye -> joint.
            self.starting = false;
            self.pos = p;
            self.issue(eye, p, true);
            return out;
        }
        // 0x140ef8ab0: previous -> joint; 0x140ef83a0: eye -> joint.
        let prev = self.pos;
        self.issue(prev, p, false);
        self.pos = p;
        self.issue(eye, p, true);
        out
    }

    /// FUN_140ef8c30's issue: positive counts drop per sweep.
    fn issue(&mut self, a: Vec3, b: Vec3, eye_sweep: bool) {
        if 0 < self.count {
            self.count -= 1;
        }
        self.pending.push((a, b, eye_sweep));
    }

    /// FUN_140ef7b90 (damage when the trace allows it).
    /// An actor already passed over by a joint sweep is skipped unless the eye sweep finds it (the
    /// FUN_1414d6d00 exception is not ported).
    fn on_hit(&mut self, now: i32, h: SweepHit, start: Vec3, end: Vec3, eye_sweep: bool) -> Option<MeleeHit> {
        match self.damage_type {
            MeleeDamageType::FirstPlusActor => {
                if !h.actor {
                    if self.hit_any {
                        return None;
                    }
                } else if self.hit_any && !eye_sweep {
                    return None;
                } else {
                    self.count = 0;
                }
            }
            MeleeDamageType::SemiContinuous if h.actor => self.count = 0,
            MeleeDamageType::Impact => self.count = 0,
            _ => {}
        }
        self.hit_any = true;
        let dir = (end - start).normalize_or_zero();
        let mut damage = None;
        if self.next_damage <= now {
            let world = h.target.is_none() && h.demon.is_none();
            let interval = if world && 0 <= self.interval_world { self.interval_world } else { self.interval };
            self.next_damage = now + interval;
            damage = Some(self.damage);
            if 0.0 < self.cap {
                let cost = if h.actor { self.damage } else { 0.0 };
                self.cap -= cost;
                if self.cap <= 0.0 {
                    self.count = 0;
                }
            }
        }
        Some(MeleeHit { hit: h, damage, weapon: self.weapon, projectile: self.projectile.clone(), damage_decl: self.damage_decl.clone(), dir })
    }
}
