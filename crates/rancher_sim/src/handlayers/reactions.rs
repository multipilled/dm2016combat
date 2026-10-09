//! idHandsHitReactions (idHands +0x46d8): the additive hit-reaction web `player/fp_hands_hit_reactions`, from the
//! user's own DOOMx64.exe (build 13954591). Notes: gamedata/re/HANDSLAYERS.md section 7.
//!
//! The web runs on its own animator (an idAnimWebHandsDirectionalAnimator) merged onto the hands pose with
//! BOP_ADD_RIGHT (layer parms {op 4, alpha 0.5}, added after the bob cycle layer and before the environmental
//! reactions, the additive channels and the weapon lag). Every trigger snaps that layer's merge alpha to the chosen
//! reaction's strength ([`HitReactions::alpha`]). Per frame, as the engine:
//! 1. [`HitReactions::set_weapon`] (0x140d87d90, run at the start of Update and of every play);
//! 2. [`HitReactions::update`] (0x140d889d0): the yaw spring while a reaction plays and the push movement graph;
//! 3. damage calls [`HitReactions::trigger`] (0x140d88080, from idPlayer's damage feedback 0x140cd2480 through
//!    idPlayerAdapter vslot 0xa0 and the hands wrapper 0x140d64800);
//! 4. the caller steps the [`AnimWebRuntime`], then [`HitReactions::web_events`] fires the one-shot "reaction done"
//!    callback (0x140d87a30) when the web edges back into the default state.

use anyhow::{bail, Context, Result};
use glam::Vec3;
use idres::animweb::BlendParms;
use idres::decl::Block;
use idres::decldb::DeclDb;

use super::additive::{blend_frames, ChannelCmd};
use super::inv_sqrt;
use crate::animweb::AnimWebRuntime;
use crate::player::Spring;
use crate::weapons::hands::HandsWeb;
use crate::weapons::GameRng;

/// idHandsHitReactionType_t (script constants table 0x143530628).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HitReactionType {
    None = 0,
    /// idDamageParms ctor (0x1406e42a0) default, and the descriptor default (the exported decls write every other type).
    #[default]
    Generic = 1,
    Melee = 2,
    Explosive = 3,
    Fire = 4,
    Pushed = 5,
    Lunge = 6,
    ShotgunnerKnockback = 7,
}

impl HitReactionType {
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "HANDSHITREACTION_NONE" => Self::None,
            "HANDSHITREACTION_GENERIC" => Self::Generic,
            "HANDSHITREACTION_MELEE" => Self::Melee,
            "HANDSHITREACTION_EXPLOSIVE" => Self::Explosive,
            "HANDSHITREACTION_FIRE" => Self::Fire,
            "HANDSHITREACTION_PUSHED" => Self::Pushed,
            "HANDSHITREACTION_LUNGE" => Self::Lunge,
            "HANDSHITREACTION_SHOTGUNNER_KNOCKBACK" => Self::ShotgunnerKnockback,
            _ => return None,
        })
    }
}

/// An idDeclTable as the hit reactions read it: the input is remapped by the decl's left/right
/// ((x - left) / (right - left), 0x1417f6e20), then looked up in its idLookupTable (0x140284580):
/// `min + (max - min) * curve`. Only clamped tables are supported (linear, snapped or Catmull-Rom spline;
/// parser 0x1417f6ee0 / finish 0x140284100 as decoded for idfx::table, which rancher_sim does not depend on).
#[derive(Debug, Clone, PartialEq)]
pub struct DeclTable {
    pub left: f32,
    pub right: f32,
    /// idLookupTable min (+4) / max (+8), after the parser's renormalisation.
    pub min: f32,
    pub max: f32,
    pub snap: bool,
    pub spline: bool,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
}

impl DeclTable {
    /// `{ clamp [snap] [min m] [max M] [left l] [right r] { v, .. | t:v, .. } }`.
    pub fn parse(src: &str) -> Result<Self> {
        let mut toks = Vec::new();
        let mut cur = String::new();
        for c in src.chars() {
            if matches!(c, '{' | '}' | ',' | ':') || c.is_whitespace() {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                if !c.is_whitespace() {
                    toks.push(c.to_string());
                }
            } else {
                cur.push(c);
            }
        }
        let num = |s: Option<&String>| -> Result<f32> {
            let s = s.map(String::as_str).unwrap_or("");
            s.trim_end_matches('f').parse::<f32>().map_err(|_| anyhow::anyhow!("bad table number {s:?}"))
        };
        let mut t = Self { left: 0.0, right: 1.0, min: 0.0, max: 1.0, snap: false, spline: false, times: Vec::new(), values: Vec::new() };
        let (mut clamp, mut i) = (false, toks.iter().position(|s| s == "{").map_or(toks.len(), |p| p + 1));
        let (mut lo, mut hi) = (1e30f32, -1e30f32);
        while i < toks.len() {
            let tok = toks[i].to_ascii_lowercase();
            i += 1;
            match tok.as_str() {
                "}" => break,
                "clamp" => clamp = true,
                "snap" => t.snap = !t.spline,
                "spline" => {
                    t.spline = true;
                    t.snap = false;
                }
                "min" | "max" | "left" | "right" => {
                    let v = num(toks.get(i))?;
                    i += 1;
                    match tok.as_str() {
                        "min" => t.min = v,
                        "max" => t.max = v,
                        "left" => t.left = v,
                        _ => t.right = v,
                    }
                }
                "{" => {
                    let mut index = 0.0f32;
                    while toks.get(i).is_some_and(|s| s != "}") {
                        let mut v = num(toks.get(i))?;
                        i += 1;
                        if toks.get(i).map(String::as_str) == Some(":") {
                            index = v;
                            v = num(toks.get(i + 1))?;
                            i += 2;
                        }
                        lo = lo.min(v);
                        hi = hi.max(v);
                        t.times.push(index);
                        t.values.push(v);
                        index += 1.0;
                        if toks.get(i).map(String::as_str) == Some(",") {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                other => bail!("unknown table keyword {other:?}"),
            }
        }
        if !clamp {
            bail!("unclamped (wrapping) tables are not used by the hands");
        }
        if t.values.is_empty() {
            bail!("empty table");
        }
        // Values outside [0, 1] are renormalised, keeping the output via min/max.
        if lo < 0.0 || 1.0 < hi {
            let range = hi - lo;
            for v in &mut t.values {
                *v = (*v - lo) / range;
            }
            t.min = t.min * range + lo;
            t.max = t.max * range + lo;
        }
        // 0x140284100: sort; times past 1 end at (n - 1) / n (clamped).
        let mut idx: Vec<usize> = (0..t.times.len()).collect();
        idx.sort_by(|&a, &b| t.times[a].total_cmp(&t.times[b]));
        t.times = idx.iter().map(|&k| t.times[k]).collect();
        t.values = idx.iter().map(|&k| t.values[k]).collect();
        let n = t.times.len();
        if n > 1 && t.times[n - 1] > 1.0 {
            let s = ((n - 1) as f32 / n as f32) / t.times[n - 1];
            for x in &mut t.times {
                *x *= s;
            }
        }
        Ok(t)
    }

    /// `name` as a decl references it, e.g. "player/handsanimation/hitdirection/explosive".
    pub fn load(db: &DeclDb, name: &str) -> Result<Self> {
        let path = format!("generated/decls/table/{name}.decl");
        let bytes = db.container().read_by_name(&path).with_context(|| path.clone())?;
        Self::parse(&String::from_utf8_lossy(&bytes)).with_context(|| path)
    }

    /// Clamped knot time / value (idCurve boundary 1).
    fn time(&self, i: i32) -> f32 {
        let n = self.times.len() as i32;
        if i < 0 {
            return self.times[0] + (self.times[1.min(n as usize - 1)] - self.times[0]) * i as f32;
        }
        if i > n - 1 {
            let (last, prev) = (self.times[(n - 1) as usize], self.times[(n - 2).max(0) as usize]);
            return last + (last - prev) * (i - (n - 1)) as f32;
        }
        self.times[i as usize]
    }

    fn value(&self, i: i32) -> f32 {
        self.values[i.clamp(0, self.values.len() as i32 - 1) as usize]
    }

    /// The curve in [0, 1] at a remapped input (0x140284600).
    pub fn curve(&self, x: f32) -> f32 {
        let n = self.times.len();
        if n == 1 {
            return self.values[0];
        }
        if self.times[0] > x {
            return self.values[0];
        }
        if x >= self.times[n - 1] {
            return self.values[n - 1];
        }
        let i = self.times.iter().position(|&t| t >= x).unwrap_or(n) as i32;
        if self.spline {
            // idCurve_CatmullRomSpline basis (0x140283f40) over knots i-2 .. i+1.
            let t0 = self.time(i - 1);
            let s = (x - t0) / (self.time(i) - t0);
            let b = [
                ((2.0 - s) * s - 1.0) * s * 0.5,
                ((s * 3.0 - 5.0) * s * s + 2.0) * 0.5,
                ((4.0 - s * 3.0) * s + 1.0) * s * 0.5,
                (s - 1.0) * s * s * 0.5,
            ];
            return (0..4).map(|k| self.value(i - 2 + k) * b[k as usize]).sum();
        }
        if self.snap {
            return self.value(i - 1);
        }
        let (ta, tb) = (self.time(i - 1), self.time(i));
        if ta == tb {
            return 0.0;
        }
        let f = (x - ta) / (tb - ta);
        (1.0 - f) * self.value(i - 1) + f * self.value(i)
    }

    pub fn lookup(&self, x: f32) -> f32 {
        let c = self.curve((x - self.left) / (self.right - self.left));
        let tiny = |v: f32| if v.abs() <= 1e-18 { 0.0 } else { v };
        (1.0 - c) * tiny(self.min) + c * tiny(self.max)
    }
}

/// idHandsHitReactionDescriptor_t (+0x20 in the reaction). Defaults: the exported player decl writes only
/// non-default values, and generic_1 (no type) must be GENERIC, every other reaction spells its type and
/// allowWeaponAlphaOverride = false. blendStrength and weaponSpecific are never read by the trigger path.
#[derive(Debug, Clone, PartialEq)]
pub struct HitReactionDescriptor {
    pub enable: bool,
    pub kind: HitReactionType,
    pub angle_perturb_degs: f32,
    /// Strength multiplier by damage amount.
    pub strength_table: Option<DeclTable>,
    /// Use the weapon's handsAdditiveAnimData target alpha as the strength.
    pub allow_weapon_alpha_override: bool,
    /// Player push over time (ms) for PUSHED / LUNGE / SHOTGUNNER_KNOCKBACK.
    pub movement_graph: Option<DeclTable>,
    pub scale_movement_with_damage: bool,
}

impl Default for HitReactionDescriptor {
    fn default() -> Self {
        Self {
            enable: true,
            kind: HitReactionType::Generic,
            angle_perturb_degs: 0.0,
            strength_table: None,
            allow_weapon_alpha_override: true,
            movement_graph: None,
            scale_movement_with_damage: false,
        }
    }
}

/// idHandsHitReaction_t (0x50).
#[derive(Debug, Clone, PartialEq)]
pub struct HitReaction {
    pub name: String,
    /// The web state; None for shotgunner_knockback (movement only).
    pub state: Option<String>,
    /// Reaction to switch to when the push hits a wall.
    pub impact: Option<String>,
    pub desc: HitReactionDescriptor,
}

/// idHandsHitReactionData_t (idPlayer +0xb560, entityDef "player" handsHitReactionData).
#[derive(Debug, Clone, PartialEq)]
pub struct HitReactionData {
    pub anim_web: String,
    pub default_sub_web: String,
    pub default_state: String,
    pub reactions: Vec<HitReaction>,
}

impl HitReactionData {
    pub fn from_decl(db: &DeclDb) -> Result<Self> {
        let b = db.get("entitydef", "player").context("entityDef player")?;
        let e = b.block("edit").and_then(|e| e.block("handsHitReactionData")).context("handsHitReactionData")?;
        Self::from_edit(db, e)
    }

    pub fn from_edit(db: &DeclDb, e: &Block) -> Result<Self> {
        let s = |b: &Block, k: &str| b.str(k).filter(|v| !v.is_empty()).map(str::to_string);
        let mut reactions = Vec::new();
        if let Some(list) = e.block("reactions") {
            let num = list.f32("num").unwrap_or(0.0) as usize;
            for i in 0..num {
                let Some(it) = list.block(&format!("item[{i}]")) else { continue };
                let mut desc = HitReactionDescriptor::default();
                if let Some(d) = it.block("descriptor") {
                    if let Some(v) = d.path("enable").and_then(|v| v.as_bool()) {
                        desc.enable = v;
                    }
                    if let Some(t) = d.str("type") {
                        desc.kind = HitReactionType::from_name(t).with_context(|| format!("hit reaction type {t}"))?;
                    }
                    desc.angle_perturb_degs = d.f32("anglePerturbDegs").unwrap_or(desc.angle_perturb_degs);
                    if let Some(v) = d.path("allowWeaponAlphaOverride").and_then(|v| v.as_bool()) {
                        desc.allow_weapon_alpha_override = v;
                    }
                    if let Some(v) = d.path("scaleMovementWithDamage").and_then(|v| v.as_bool()) {
                        desc.scale_movement_with_damage = v;
                    }
                    desc.strength_table = s(d, "strengthTable").map(|t| DeclTable::load(db, &t)).transpose()?;
                    desc.movement_graph = s(d, "movementGraph").map(|t| DeclTable::load(db, &t)).transpose()?;
                }
                reactions.push(HitReaction {
                    name: s(it, "name").unwrap_or_default(),
                    state: s(it, "animWebState"),
                    impact: s(it, "impactReaction"),
                    desc,
                });
            }
        }
        Ok(Self {
            anim_web: s(e, "animWebDecl").unwrap_or_default(),
            default_sub_web: s(e, "defaultAnimWebSubWeb").unwrap_or_default(),
            default_state: s(e, "defaultAnimWebState").unwrap_or_default(),
            reactions,
        })
    }

    /// 0x140d87a50: the first reaction with this name (idStr::Icmp).
    pub fn find(&self, name: &str) -> Option<usize> {
        self.reactions.iter().position(|r| r.name.eq_ignore_ascii_case(name))
    }
}

/// hands_hitReactions* cvars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitReactionsCvars {
    pub enable: bool,
    pub timeout_ms: i32,
    pub yaw_smooth_spring_k: f32,
}

impl Default for HitReactionsCvars {
    fn default() -> Self {
        Self { enable: true, timeout_ms: 100, yaw_smooth_spring_k: 150.0 }
    }
}

impl HitReactionsCvars {
    pub fn from_cvars(cv: &crate::config::CvarValues) -> Self {
        let d = Self::default();
        let g = |n: &str, def: f32| cv.0.get(n).and_then(|v| v.trim_end_matches('f').parse().ok()).unwrap_or(def);
        Self {
            enable: g("hands_hitReactionsEnable", 1.0) != 0.0,
            timeout_ms: g("hands_hitReactionsTimeoutMs", d.timeout_ms as f32) as i32,
            yaw_smooth_spring_k: g("hands_hitReactionsYawSmoothSpringK", d.yaw_smooth_spring_k),
        }
    }
}

/// The webVars of idAnimWebHandsDirectionalAnimator (+0x7d8, names bound by RegisterScalars 0x140ccb600; static
/// init 0x140125c90). The tenth, "strength" (+0x7fc), is only written by the environmental reactions.
pub const DIR_SCALAR_NAMES: [&str; 9] = [
    "dir_2_index_0",
    "dir_2_index_1",
    "dir_2_index_blend",
    "dir_4_index_0",
    "dir_4_index_1",
    "dir_4_index_blend",
    "dir_8_index_0",
    "dir_8_index_1",
    "dir_8_index_blend",
];

/// 0x140ccb790: a reaction yaw (degrees, 0 = the hit travels along the player's forward, i.e. it came from behind)
/// to the dir_2/4/8 index/blend scalars. a = 180 - yaw wrapped into [0, 360); index k covers [k*span, (k+1)*span)
/// and blends toward k+1 (wrapping to 0). The web's 4-way anims are ordered n, e, s, w (index 0 = hit from the front).
pub fn dir_scalars(yaw: f32) -> [f32; 9] {
    let mut a = 180.0 - yaw;
    if 360.0 <= a || a < 0.0 {
        a -= (a * 0.002_777_777_8).floor() * 360.0;
    }
    let mut o = [0.0f32; 9];
    // dir_2
    if 180.0 <= a {
        if a <= 360.0 {
            o[0] = 1.0;
            o[1] = 0.0;
            o[2] = (a - 180.0) / 180.0;
        }
    } else {
        o[0] = 0.0;
        o[1] = 1.0;
        o[2] = a / 180.0;
    }
    // dir_4
    let four = if a < 90.0 {
        Some((0.0, 1.0, a))
    } else if a < 180.0 {
        Some((1.0, 2.0, a - 90.0))
    } else if a < 270.0 {
        Some((2.0, 3.0, a - 180.0))
    } else if a <= 360.0 {
        Some((3.0, 0.0, a - 270.0))
    } else {
        None
    };
    if let Some((i0, i1, f)) = four {
        o[3] = i0;
        o[4] = i1;
        o[5] = f / 90.0;
    }
    // dir_8: the exe's comparison chain on 45, 90, .., 315; a > 360 only warns (unreachable after the wrap).
    let bounds = [45.0f32, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0];
    if a <= 360.0 {
        let k = bounds.iter().position(|&b| a < b).unwrap_or(7);
        o[6] = k as f32;
        o[7] = ((k + 1) % 8) as f32;
        o[8] = if k == 0 { a / 45.0 } else { (a - 45.0 * k as f32) / 45.0 };
    }
    o
}

/// idVec3::ToYaw (0x1402ee3e0): atan2(y, x) in degrees, wrapped to [0, 360); 0 when |x| and |y| are both tiny.
pub fn to_yaw(v: Vec3) -> f32 {
    const TINY: f32 = 1.175_494_4e-38;
    if v.x.abs() <= TINY && v.y.abs() <= TINY {
        return 0.0;
    }
    let y = v.y.atan2(v.x) * 57.295_776;
    if y < 0.0 {
        y + 360.0
    } else {
        y
    }
}

/// The hit direction idPlayer's damage feedback (0x140cd2480) passes to the hit reaction when the damage has an
/// attacker entity: the damage direction (`world_dir`, attacker -> player) shaped by the damage decl's knockBack /
/// knockUp (idDamageParms +0x1dc / +0x1e0), expressed in the frame of the player's view yaw (the player's
/// vslot 0x170 axis flattened; when its forward row is vertical the up row's x/y are used). Only its yaw matters to
/// the reaction; knockUp > 0 with knockBack <= 0 gives straight up, i.e. yaw 0. (Not oracle-checked; the yaw is
/// perturbed by up to anglePerturbDegs at random anyway.)
pub fn local_hit_direction(world_dir: Vec3, knock_back: i32, knock_up: i32, view_forward: Vec3, view_up: Vec3) -> Vec3 {
    const TINY: f32 = 1.175_494_4e-38;
    let mut v = world_dir;
    if knock_up < 1 {
        if knock_up == 0 {
            let l2 = v.y * v.y + v.x * v.x + 0.0;
            let s = inv_sqrt(l2);
            v = Vec3::new(v.x * s, v.y * s, s * 0.0);
        }
    } else {
        let (kb, ku) = (knock_back as f32, knock_up as f32);
        let s = inv_sqrt(kb * kb + 0.0 + ku * ku);
        let (kbn, kun) = (kb * s, ku * s);
        if knock_back < 1 || kbn.abs() < TINY {
            let s = inv_sqrt(ku * ku + 0.0);
            v = Vec3::new(s * 0.0, s * 0.0, ku * s);
        } else {
            let s = inv_sqrt(v.y * v.y + v.x * v.x + v.z * v.z);
            v *= s;
            if v.x.abs() < TINY && v.y.abs() < TINY {
                v = view_forward;
            }
            let (kun_pos, kun_neg) = (kun, -kun);
            if kun_pos < v.z || v.z < kun_neg {
                let t = 1.0 - v.z * v.z;
                let r = inv_sqrt(t) * t;
                let k = kbn / r;
                v = Vec3::new(v.x * k, v.y * k, if kun_pos < v.z { kun_pos } else { kun_neg });
            }
        }
    }
    let mut f = (view_forward.x, view_forward.y);
    if !(TINY <= f.0.abs() || TINY <= f.1.abs()) {
        f = (view_up.x, view_up.y);
    }
    let s = inv_sqrt(f.1 * f.1 + f.0 * f.0 + 0.0);
    let (fx, fy) = (f.0 * s, f.1 * s);
    // idVec3::ToMat3 of the flat forward: rows forward, left = (-fy, fx, 0), up = forward x left.
    let d = inv_sqrt(fx * fx + fy * fy);
    let (lx, ly) = (-fy * d, fx * d);
    let uz = fx * ly - fy * lx;
    Vec3::new(fy * v.y + fx * v.x, ly * v.y + lx * v.x, uz * v.z)
}

/// What a damage event asks of the hit reactions (0x140cd2480): PUSHED with the pushback distance when the damage
/// decl's playerPushbackDistMin/Max (+0xa8 / +0xac) lerped by where the damage falls in [minDamage, maxDamage] is
/// positive, else the decl's handsHitReactionType with the damage amount.
pub fn reaction_for_damage(damage: f32, kind: HitReactionType, pushback_min: f32, pushback_max: f32, min_damage: f32, max_damage: f32) -> (HitReactionType, f32) {
    let f = if min_damage < max_damage { (damage - min_damage) / (max_damage - min_damage) } else { 0.0 };
    let tiny = |v: f32| if v.abs() <= 1e-18 { 0.0 } else { v };
    let push = (1.0 - f) * tiny(pushback_min) + tiny(pushback_max) * f;
    if push <= 0.0 {
        (kind, damage)
    } else {
        (HitReactionType::Pushed, push)
    }
}

/// One damage event for [`HitReactions::trigger`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitInput {
    /// The hit direction in the player's view-yaw frame (x forward, y left): [`local_hit_direction`].
    pub dir: Vec3,
    pub kind: HitReactionType,
    /// Damage amount (strength tables), or the pushback distance for PUSHED ([`reaction_for_damage`]).
    pub amount: f32,
    /// The player is zoomed (0x140e42290).
    pub zoomed: bool,
    /// The current weapon decl's handsAdditiveAnimData (targetAlphaDefault +0xc18, targetAlphaZoomed +0xc20;
    /// ctor 0x1406f0780 defaults 0.5 / 0.15, no campaign weapon overrides them). None without a weapon.
    pub weapon_alpha: Option<(f32, f32)>,
    /// The player's view yaw (degrees; vslot 0x128 angles[1]) that turns `dir` back into the world push direction.
    pub view_yaw: f32,
    /// Game time, ms.
    pub now: i32,
}

/// idHandsHitReactions state (0x8b0; reflection names).
#[derive(Debug, Clone)]
pub struct HitReactions {
    pub last_time_ms: i32,
    pub last_dist: f32,
    pub last: Option<usize>,
    /// smoothedYaw (+0x848): target = the newest hit yaw, pos = what the dir scalars use.
    pub yaw: Spring,
    pub snap_to_yaw: bool,
    pub world_hit_direction: Vec3,
    pub move_distance: f32,
    pub playing: bool,
    pub collided: bool,
    /// curWeaponSubWebIndex (None = the weapon's handsHitReactionSubweb is not in the web: use the default).
    pub weapon_sub: Option<String>,
    weapon: Option<String>,
    /// The layer's merge alpha (BOP_ADD_RIGHT): 0.5 until the first reaction, then each reaction's strength.
    pub alpha: f32,
    /// The registered "reaction done" callback (slot type 0: edge into (sub-web, default state)).
    done_slot: Option<(String, String)>,
}

impl HitReactions {
    /// Ctor 0x140d878d0: lastHitReactionTimeMs -1, snapToYaw, the yaw spring at K 1 / damping 2 / mass 1 then
    /// SetK(hands_hitReactionsYawSmoothSpringK, critical).
    pub fn new(cv: &HitReactionsCvars) -> Self {
        let mut yaw = Spring::default();
        set_k_critical(&mut yaw, cv.yaw_smooth_spring_k);
        Self {
            last_time_ms: -1,
            last_dist: 0.0,
            last: None,
            yaw,
            snap_to_yaw: true,
            world_hit_direction: Vec3::ZERO,
            move_distance: 0.0,
            playing: false,
            collided: false,
            weapon_sub: None,
            weapon: None,
            alpha: 0.5,
            done_slot: None,
        }
    }

    /// Init 0x140d87ae0: the web starts in the default state of the default sub-web.
    pub fn init(&mut self, web: &mut AnimWebRuntime, d: &HitReactionData) -> bool {
        web.set_state(&d.default_sub_web, &d.default_state)
    }

    /// 0x140d87d90: on a weapon change, use its handsHitReactionSubweb (when the web has it) and snap that sub-web's
    /// default state (ForceState, destDuration 0). `weapon` is the decl name, `hit_sub_web` its handsHitReactionSubweb.
    pub fn set_weapon(&mut self, web: &mut AnimWebRuntime, d: &HitReactionData, weapon: &str, hit_sub_web: &str) {
        if self.weapon.as_deref() == Some(weapon) {
            return;
        }
        self.weapon = Some(weapon.to_string());
        self.weapon_sub = web.web.sub_webs.iter().any(|s| s.name == hit_sub_web).then(|| hit_sub_web.to_string());
        if let Some(sub) = &self.weapon_sub {
            let parms = BlendParms { source_duration: 0x7fff as f32, dest_duration: 0.0, ..Default::default() };
            web.force_state(Some(sub), &d.default_state, parms);
        }
    }

    fn sub_web<'a>(&'a self, d: &'a HitReactionData) -> &'a str {
        self.weapon_sub.as_deref().unwrap_or(&d.default_sub_web)
    }

    /// 0x140d88c30: does reaction `r` answer this hit, and at what strength.
    fn strength(r: &HitReaction, inp: &HitInput) -> Option<f32> {
        if !r.desc.enable || r.desc.kind != inp.kind {
            return None;
        }
        let mut s = 1.0f32;
        if r.desc.allow_weapon_alpha_override {
            s = if inp.zoomed { 0.15 } else { 0.5 };
            if let Some((def, zoomed)) = inp.weapon_alpha {
                s = if inp.zoomed { zoomed } else { def };
            }
        }
        if let Some(t) = &r.desc.strength_table {
            s *= t.lookup(inp.amount);
        }
        Some(s)
    }

    /// 0x140d88500: a random reaction among those answering the hit, never the last one when there is a choice.
    fn select(&self, d: &HitReactionData, inp: &HitInput, rng: &mut GameRng) -> Option<(usize, f32)> {
        let mut cands: Vec<(usize, f32)> = d.reactions.iter().enumerate().filter_map(|(i, r)| Self::strength(r, inp).map(|s| (i, s))).collect();
        if cands.len() > 1
            && let Some(last) = self.last
            && let Some(p) = cands.iter().position(|c| c.0 == last)
        {
            cands.remove(p);
        }
        if cands.is_empty() {
            return None;
        }
        let k = (rng.next15() as i32 % cands.len() as i32) as usize;
        Some(cands[k])
    }

    /// TriggerHitReaction 0x140d88080. Returns the reaction index when one started. After every call (started or
    /// not) the hands wrapper 0x140d64800 also jolts the weapon: weaponJoltData "hit" (0x140d637e0(hands, 3, 1.0)).
    pub fn trigger(&mut self, web: &mut AnimWebRuntime, d: &HitReactionData, cv: &HitReactionsCvars, rng: &mut GameRng, inp: &HitInput) -> Option<usize> {
        if inp.kind == HitReactionType::None || !cv.enable || inp.now < self.last_time_ms + cv.timeout_ms {
            return None;
        }
        let yaw0 = to_yaw(inp.dir);
        let (idx, strength) = self.select(d, inp, rng)?;
        let p = d.reactions[idx].desc.angle_perturb_degs;
        let lo = -p;
        let yaw = yaw0 + (rng.next01() * (p - lo) + lo);
        if self.snap_to_yaw {
            self.yaw.pos = yaw;
            self.yaw.target = yaw;
            self.snap_to_yaw = false;
        } else {
            self.yaw.target = yaw;
            let t = self.yaw.target;
            while 180.0 < self.yaw.pos - t {
                self.yaw.pos -= 360.0;
            }
            while 180.0 < t - self.yaw.pos {
                self.yaw.pos += 360.0;
            }
        }
        if d.reactions[idx].desc.movement_graph.is_some() {
            self.move_distance = inp.amount;
        }
        self.play(web, d, idx, strength, inp.now);
        let (s, c) = (inp.view_yaw.to_radians().sin(), inp.view_yaw.to_radians().cos());
        self.world_hit_direction = Vec3::new(inp.dir.x * c - inp.dir.y * s, inp.dir.x * s + inp.dir.y * c, inp.dir.z);
        Some(idx)
    }

    /// 0x140d882d0 / 0x140d88280: start reaction `idx`: the dir scalars from the smoothed yaw, the merge alpha, and a
    /// forced edge into its state (1 frame) that then paths back to the default state, whose arrival ends it.
    fn play(&mut self, web: &mut AnimWebRuntime, d: &HitReactionData, idx: usize, strength: f32, now: i32) {
        self.last_time_ms = now;
        self.last_dist = 0.0;
        self.last = Some(idx);
        for (n, v) in DIR_SCALAR_NAMES.iter().zip(dir_scalars(self.yaw.pos)) {
            web.set_scalar(n, v);
        }
        // 0x140ccb710(alpha, 0): clamp to [0, 1] and snap the merge alpha.
        self.alpha = strength.clamp(0.0, 1.0);
        let sub = self.sub_web(d).to_string();
        if let Some(state) = &d.reactions[idx].state {
            // 0x141704f60(sub, default, sub, state) = 0x1417046b0: forced edge into `state` with blend parms
            // {sourceDuration 0x7fff, destDuration 1}, then a path to the default state (interruptPath 1,
            // interruptBlend 2).
            HandsWeb::force_state_via(web, &sub, state, &d.default_state, 1);
            self.done_slot = Some((sub, d.default_state.clone()));
        }
        self.playing = true;
        self.collided = false;
    }

    /// Update 0x140d889d0 (`dt` = frame seconds). While a reaction plays: the yaw spring, and for reactions with a
    /// movement graph the push: returns this frame's world displacement of the player (game units) for the caller's
    /// physics to apply (0x140e32950), or switches to the impact reaction when the push hit a wall.
    pub fn update(&mut self, web: &mut AnimWebRuntime, d: &HitReactionData, cv: &HitReactionsCvars, dt: f32, now: i32) -> Option<Vec3> {
        if !cv.enable || !self.playing {
            return None;
        }
        self.yaw.update(dt);
        let r = &d.reactions[self.last?];
        let graph = r.desc.movement_graph.as_ref()?;
        if graph.max == 0.0 {
            return None;
        }
        let elapsed = now - self.last_time_ms;
        let scale = if r.desc.scale_movement_with_damage { self.move_distance / graph.max } else { 1.0 };
        if graph.right < elapsed as f32 {
            return None;
        }
        if let Some(impact) = &r.impact
            && 0 < elapsed
            && self.collided
        {
            self.collided = false;
            if let Some(i) = d.find(impact) {
                self.play(web, d, i, 1.0, now);
            }
            return None;
        }
        let dist = graph.lookup(elapsed as f32) * scale;
        let delta = dist - self.last_dist;
        self.last_dist = dist;
        Some(self.world_hit_direction * delta)
    }

    /// Player physics collision callback (0x140d87f20): while a reaction plays, hitting something that is not floor
    /// (up . normal <= the physics' min floor cosine) marks the push as collided.
    pub fn on_collision(&mut self, normal: Vec3, up: Vec3, min_floor_cosine: f32) {
        if !self.playing {
            return;
        }
        let s = inv_sqrt(normal.y * normal.y + normal.x * normal.x + normal.z * normal.z);
        if up.y * normal.y * s + up.x * normal.x * s + up.z * normal.z * s <= min_floor_cosine {
            self.collided = true;
        }
    }

    /// After the web's update: the one-shot "reaction done" callback (0x140d87a30: snapToYaw, not playing) fires
    /// when the web edges into the default state of the sub-web the reaction played in (node event kind 0).
    pub fn web_events(&mut self, web: &AnimWebRuntime) {
        for (kind, sub, state) in web.node_events() {
            if kind == 0 && self.done_slot.as_ref().is_some_and(|(s, st)| s == sub && st == state) {
                self.done_slot = None;
                self.snap_to_yaw = true;
                self.playing = false;
            }
        }
    }
}

/// idDeclWeapon weaponJoltData (idWeaponJoltData_t, +0xbb0; ctor 0x1406f0780 defaults). In the campaign only the
/// chaingun decls set additiveAnim ("additiveJolt", an alias of the WEAPON model's md6Def).
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponJoltData {
    pub additive_anim: String,
    pub footstep: (f32, f32),
    pub land: (f32, f32),
    pub hit: (f32, f32),
}

impl Default for WeaponJoltData {
    fn default() -> Self {
        Self { additive_anim: String::new(), footstep: (0.2, 0.35), land: (0.75, 1.0), hit: (0.1, 0.2) }
    }
}

impl WeaponJoltData {
    /// From a weapon decl's `edit` block.
    pub fn from_edit(e: &Block) -> Self {
        let mut d = Self::default();
        if let Some(j) = e.block("weaponJoltData") {
            d.additive_anim = j.str("additiveAnim").unwrap_or_default().to_string();
            let pair = |lo: &str, hi: &str, def: (f32, f32)| (j.f32(lo).unwrap_or(def.0), j.f32(hi).unwrap_or(def.1));
            d.footstep = pair("footstepMinStrength", "footstepMaxStrength", d.footstep);
            d.land = pair("landMinStrength", "landMaxStrength", d.land);
            d.hit = pair("hitMinStrength", "hitMaxStrength", d.hit);
        }
        d
    }
}

/// The weapon jolt the hands wrapper 0x140d64800 fires after EVERY hit-reaction trigger call (started or not):
/// 0x140d637e0(hands, 3, 1.0) -> idWeapon 0x140f1e570 (type 3 = hit; types 1 footstep / 2 land have no callers in
/// this build). Without an additiveAnim nothing happens (and no random draw); else the strength is
/// `rand * (hitMax - hitMin) + hitMin` (one draw of the game LCG) and the weapon's own additive channel
/// (idWeapon +0x738, BOP_ADD_RIGHT on the weapon model's anim stack, layer alpha 1.0) plays the alias from frame 0 at
/// rate 1 with a weapon_additiveAnimBlendMS (150) blend, its merge alpha snapped to the strength (0x140f1d3f0 ->
/// 0x140f19170).
pub fn weapon_jolt_hit(jd: &WeaponJoltData, rng: &mut GameRng, additive_anim_blend_ms: i32) -> Option<ChannelCmd> {
    if jd.additive_anim.is_empty() {
        return None;
    }
    let (lo, hi) = jd.hit;
    let s = rng.next01() * (hi - lo) + lo;
    Some(ChannelCmd::Play { alias: jd.additive_anim.clone(), rate: 1.0, blend_frames: blend_frames(additive_anim_blend_ms), blend_type: 0, alpha: Some(s) })
}

/// idSpring<idVec1>::SetK(k, -1) (0x1409430e0): K capped at 10000 and critical damping 2*sqrt(K*mass) with the
/// exe's InvSqrt.
fn set_k_critical(s: &mut Spring, k: f32) {
    s.k = if k <= 10000.0 { k } else { 10000.0 };
    let km = s.k * s.mass;
    let root = inv_sqrt(km) * km;
    s.damping = root + root;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::large_const_arrays)]
    mod golden {
        include!("reactions_golden.in");
    }

    #[test]
    fn dir_scalars_match_engine() {
        for (yaw, want) in golden::DIR_SCALARS {
            let got = dir_scalars(f32::from_bits(yaw));
            assert_eq!(got.map(f32::to_bits), want, "yaw {}", f32::from_bits(yaw));
        }
    }

    #[test]
    fn to_yaw_matches_engine() {
        for (v, want) in golden::TO_YAW {
            let v = Vec3::new(f32::from_bits(v[0]), f32::from_bits(v[1]), f32::from_bits(v[2]));
            assert_eq!(to_yaw(v).to_bits(), want, "{v:?}");
        }
    }

    #[test]
    fn decl_table_lookup() {
        let t = DeclTable::parse("{ clamp min 0 max 1 left 0 right 1 {0:0.00625002, 0.0589302:0.35, 0.157752:0.5125, 1:0} }").unwrap();
        assert!((t.lookup(0.0) - 0.006_250_02).abs() < 1e-6);
        assert!((t.lookup(0.0589302) - 0.35).abs() < 1e-5);
        assert_eq!(t.lookup(2.0), 0.0);
        // Values 0..4 renormalised to 0..1 with min 0 / max 4; bare values take times 0, 1, 2 -> 0, 1/3, 2/3.
        let t = DeclTable::parse("{ clamp { 0, 2, 4 } }").unwrap();
        assert!((t.lookup(1.0 / 3.0) - 2.0).abs() < 1e-5);
        // left/right remap the input.
        let t = DeclTable::parse("{ clamp min 0 max 500 left 0 right 600 {0:0, 1:1} }").unwrap();
        assert!((t.lookup(150.0) - 125.0).abs() < 1e-4);
    }

    #[test]
    fn install_weapon_jolt() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("install");
        let jolt = |w: &str| {
            let b = inst.decls.get("weapon", w).unwrap();
            WeaponJoltData::from_edit(b.block("edit").unwrap())
        };
        let cg = jolt("weapon/zion/player/sp/chaingun");
        assert_eq!((cg.additive_anim.as_str(), cg.hit), ("additiveJolt", (0.1, 0.2)));
        let mut rng = GameRng(99);
        let Some(ChannelCmd::Play { alias, blend_frames, alpha: Some(a), .. }) = weapon_jolt_hit(&cg, &mut rng, 150) else { panic!() };
        assert_eq!((alias.as_str(), blend_frames), ("additiveJolt", 4));
        assert!((0.1..=0.2).contains(&a));
        // No jolt anim: nothing, and the game RNG is not drawn.
        let har = jolt("weapon/zion/player/sp/heavy_rifle_heavy_ar");
        let before = rng.clone();
        assert_eq!(weapon_jolt_hit(&har, &mut rng, 150), None);
        assert_eq!(rng, before);
    }

    #[test]
    fn damage_maps_to_reaction() {
        assert_eq!(reaction_for_damage(60.0, HitReactionType::Generic, 0.0, 0.0, 60.0, 60.0), (HitReactionType::Generic, 60.0));
        // Pushback lerped by the damage's place in [minDamage, maxDamage].
        let (k, d) = reaction_for_damage(75.0, HitReactionType::Melee, 100.0, 200.0, 50.0, 100.0);
        assert_eq!(k, HitReactionType::Pushed);
        assert!((d - 150.0).abs() < 1e-4);
    }

    #[test]
    fn local_direction_is_view_relative() {
        let fwd = Vec3::new(0.0, 1.0, 0.0); // looking along +y
        // Damage travelling along -x (attacker on the player's left... in world +x); in view frame: right-to-left?
        let d = local_hit_direction(Vec3::new(-1.0, 0.0, 0.0), 0, 0, fwd, Vec3::Z);
        // view left = (-1, 0): the hit travels toward the player's left -> local +y -> yaw 90 -> dir_4 index 1 (e).
        assert!((d - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-6, "{d:?}");
        assert_eq!(to_yaw(d), 90.0);
        let s = dir_scalars(90.0);
        assert_eq!(&s[3..6], &[1.0, 2.0, 0.0]);
        // knockUp only: straight up -> yaw 0.
        let up = local_hit_direction(Vec3::new(-1.0, 0.0, 0.0), 0, 300, fwd, Vec3::Z);
        assert_eq!(to_yaw(up), 0.0);
    }

    fn install_web(subs: &[&str]) -> Option<(crate::install::Install, AnimWebRuntime)> {
        use std::sync::Arc;
        let doom = idres::find_install()?;
        let inst = crate::install::load(&doom).expect("install");
        let c = idres::Container::open(&doom.join("base"), "gameresources").unwrap();
        let text = String::from_utf8_lossy(&c.read_by_name("generated/decls/animweb/player/fp_hands_hit_reactions.decl").unwrap()).into_owned();
        let webdecl = Arc::new(idres::animweb::AnimWeb::parse(&text).unwrap());
        let data = Arc::new(crate::animweb::AnimData::load(&c, &webdecl, subs));
        let mut web = AnimWebRuntime::new(webdecl, data);
        web.hands_weights = false;
        Some((inst, web))
    }

    #[test]
    fn install_hit_reaction_data() {
        let Some(doom) = idres::find_install() else { return };
        let inst = crate::install::load(&doom).expect("install");
        let d = HitReactionData::from_decl(&inst.decls).unwrap();
        assert_eq!((d.anim_web.as_str(), d.default_sub_web.as_str(), d.default_state.as_str()), ("player/fp_hands_hit_reactions", "default", "idle"));
        assert_eq!(d.reactions.len(), 13);
        let g = &d.reactions[d.find("generic_1").unwrap()];
        assert_eq!((g.desc.kind, g.desc.angle_perturb_degs, g.desc.allow_weapon_alpha_override), (HitReactionType::Generic, 45.0, true));
        let e = &d.reactions[d.find("explosive_1").unwrap()];
        let t = e.desc.strength_table.as_ref().unwrap();
        // { clamp min 0 max 1 left 0 right 200 {0:0.2515, 0.0997:0.253, 0.1022:0.5, 0.2501:0.503, 0.2502:0.75, 1:0.7546} }
        assert!((t.lookup(0.0) - 0.251_524).abs() < 1e-5 && (t.lookup(30.0) - 0.5).abs() < 0.01 && (t.lookup(500.0) - 0.754_573).abs() < 1e-5);
        let p = &d.reactions[d.find("pushed").unwrap()];
        assert_eq!(p.impact.as_deref(), Some("pushed_impact"));
        let m = p.desc.movement_graph.as_ref().unwrap();
        assert_eq!((m.right, m.max), (600.0, 500.0));
        assert!((m.lookup(300.0) - 250.0).abs() < 1e-3);
        assert_eq!(d.reactions[d.find("shotgunner_knockback").unwrap()].state, None);
    }

    /// A generic hit from the player's right plays generic_1 or generic_2 in the weapon's sub-web, then the web paths
    /// back to idle and the reaction ends; the next generic hit picks the other one (the last is excluded).
    #[test]
    fn install_generic_hits_alternate() {
        let Some((inst, mut web)) = install_web(&["default", "heavy_ar"]) else { return };
        let d = HitReactionData::from_decl(&inst.decls).unwrap();
        let cv = HitReactionsCvars::from_cvars(&inst.cvars);
        let mut hr = HitReactions::new(&cv);
        assert!(hr.init(&mut web, &d));
        hr.set_weapon(&mut web, &d, "weapon/zion/player/sp/heavy_rifle_heavy_ar", "heavy_ar");
        assert_eq!(hr.weapon_sub.as_deref(), Some("heavy_ar"));
        let mut rng = GameRng(12345);
        let (mut now, mut played) = (1000, Vec::new());
        web.update(now);
        for hit in 0..2 {
            let inp = HitInput {
                dir: Vec3::new(0.0, 1.0, 0.0),
                kind: HitReactionType::Generic,
                amount: 20.0,
                zoomed: false,
                weapon_alpha: Some((0.5, 0.15)),
                view_yaw: 0.0,
                now,
            };
            let idx = hr.trigger(&mut web, &d, &cv, &mut rng, &inp).expect("reaction");
            played.push(d.reactions[idx].name.clone());
            assert!(hr.playing && hr.alpha == 0.5);
            let mut seen = Vec::new();
            for _ in 0..120 {
                hr.update(&mut web, &d, &cv, 0.016, now);
                now += 16;
                web.update(now);
                hr.web_events(&web);
                let s = web.current().map(|(sub, s)| format!("{sub}/{s}")).unwrap_or_default();
                if seen.last() != Some(&s) {
                    seen.push(s);
                }
                if !hr.playing {
                    break;
                }
            }
            assert!(!hr.playing, "hit {hit}: never returned to idle: {seen:?}");
            assert_eq!(seen.first().map(String::as_str), Some(format!("heavy_ar/{}", d.reactions[idx].state.as_deref().unwrap()).as_str()), "{seen:?}");
            assert_eq!(seen.last().map(String::as_str), Some("heavy_ar/idle"));
        }
        played.sort();
        assert_eq!(played, ["generic_1", "generic_2"]);
    }

    /// A PUSHED hit moves the player along the world hit direction by the pushback distance over the graph's 600 ms.
    #[test]
    fn install_push_moves_player() {
        let Some((inst, mut web)) = install_web(&["default"]) else { return };
        let d = HitReactionData::from_decl(&inst.decls).unwrap();
        let cv = HitReactionsCvars::from_cvars(&inst.cvars);
        let mut hr = HitReactions::new(&cv);
        hr.init(&mut web, &d);
        let mut rng = GameRng(7);
        let mut now = 5000;
        let inp = HitInput { dir: Vec3::new(-1.0, 0.0, 0.0), kind: HitReactionType::Pushed, amount: 120.0, zoomed: false, weapon_alpha: None, view_yaw: 90.0, now };
        let idx = hr.trigger(&mut web, &d, &cv, &mut rng, &inp).unwrap();
        assert_eq!(d.reactions[idx].name, "pushed");
        let mut total = Vec3::ZERO;
        for _ in 0..60 {
            now += 16;
            if let Some(m) = hr.update(&mut web, &d, &cv, 0.016, now) {
                total += m;
            }
            web.update(now);
            hr.web_events(&web);
        }
        // Local -x (pushed backwards) seen with view yaw 90 is world -y. The push stops at the last frame with
        // elapsed <= right (592 ms at 16 ms frames), so it never quite reaches the full 120.
        assert!((total - Vec3::new(0.0, -120.0 * 592.0 / 600.0, 0.0)).length() < 1e-3, "{total:?}");
    }

    #[test]
    fn yaw_spring_matches_engine() {
        for (k, t, p, rows) in golden::SPRINGS {
            let mut s = Spring::default();
            set_k_critical(&mut s, f32::from_bits(k));
            s.target = f32::from_bits(t);
            s.pos = f32::from_bits(p);
            for (i, (dt, want)) in golden::SPRING_FRAMES.iter().zip(rows).enumerate() {
                s.update(f32::from_bits(*dt));
                assert_eq!([s.target.to_bits(), s.pos.to_bits(), s.vel.to_bits()], want, "k {} frame {i}", f32::from_bits(k));
            }
        }
    }
}
