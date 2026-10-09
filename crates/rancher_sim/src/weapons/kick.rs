//! First-person view kick (recoil), decoded from DOOMx64.exe.
//!
//! New system (`weaponFeedBack.useNewKickSystem`): idWeaponKickComponent at player+0xced0, four
//! idWeaponKickAxis (yaw, pitch, roll, fov). Old system (BFG): pitch/yaw speeds in idPlayer.
//! Output offsets follow the game's conventions: pitch < 0 looks up, fov > 0 widens the view.

use super::interp::{AccelDecelSine, Interpolate};
use super::GameRng;

/// `idDeclWeapon::weaponKick_t` (ctor defaults 0x1406f19f0: recoil/recovery 16 ms).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponKick {
    pub kind: i32,
    pub kick: f32,
    pub max_kick: f32,
    pub recoil_ms: i32,
    pub recovery_ms: i32,
    pub recovery_delay_ms: i32,
    pub dampen: bool,
}

impl Default for WeaponKick {
    fn default() -> Self {
        Self { kind: KICK_NORMAL, kick: 0.0, max_kick: 0.0, recoil_ms: 16, recovery_ms: 16, recovery_delay_ms: 0, dampen: false }
    }
}

pub const KICK_NORMAL: i32 = 0;
pub const KICK_ONLY_POSITIVE: i32 = 1;
pub const KICK_ONLY_NEGATIVE: i32 = 2;
pub const KICK_ONLY_MAX_POSITIVE: i32 = 3;
pub const KICK_ONLY_MAX_NEGATIVE: i32 = 4;

pub fn kick_type(name: &str) -> i32 {
    match name.rsplit("::").next().unwrap_or(name) {
        "KICK_ONLY_POSITIVE" => KICK_ONLY_POSITIVE,
        "KICK_ONLY_NEGATIVE" => KICK_ONLY_NEGATIVE,
        "KICK_ONLY_MAX_POSITIVE" => KICK_ONLY_MAX_POSITIVE,
        "KICK_ONLY_MAX_NEGATIVE" => KICK_ONLY_MAX_NEGATIVE,
        _ => KICK_NORMAL,
    }
}

/// `idDeclWeapon::weaponFeedBack_t` (ctor 0x1406f19f0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeedBack {
    pub do_unique_kick: bool,
    pub do_smooth_kick: bool,
    pub pitch_kick_amount: f32,
    pub pitch_kick_amount_delta: f32,
    pub pitch_kick_top_bound: f32,
    pub yaw_kick_amount: f32,
    pub yaw_kick_amount_delta: f32,
    pub pitch_kick_speed_into: f32,
    pub pitch_kick_speed_into_min: f32,
    pub pitch_kick_speed_into_per_shot: f32,
    pub pitch_kick_speed_from: f32,
    pub yaw_kick_speed_into: f32,
    pub yaw_kick_speed_from: f32,
    pub kick_recovery_delay: i32,
    pub use_new_kick_system: bool,
    pub kick_yaw: WeaponKick,
    pub kick_pitch: WeaponKick,
    pub kick_roll: WeaponKick,
    pub kick_fov: WeaponKick,
    /// Units the player is pushed back per shot (FinishFire, scaled by g_weaponKickBackRatio).
    pub weapon_knockback: f32,
}

impl Default for FeedBack {
    fn default() -> Self {
        Self {
            do_unique_kick: false,
            do_smooth_kick: true,
            pitch_kick_amount: 0.0,
            pitch_kick_amount_delta: 0.0,
            pitch_kick_top_bound: 0.0,
            yaw_kick_amount: 0.0,
            yaw_kick_amount_delta: 0.0,
            pitch_kick_speed_into: 1.0,
            pitch_kick_speed_into_min: 1.0,
            pitch_kick_speed_into_per_shot: 1.0,
            pitch_kick_speed_from: 1.0,
            yaw_kick_speed_into: 1.0,
            yaw_kick_speed_from: 1.0,
            kick_recovery_delay: 0,
            use_new_kick_system: false,
            kick_yaw: WeaponKick::default(),
            kick_pitch: WeaponKick::default(),
            kick_roll: WeaponKick::default(),
            kick_fov: WeaponKick::default(),
            weapon_knockback: 0.0,
        }
    }
}

/// `idWeaponKickAxis`: linear recoil into the kick, then sine ease-out recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KickAxis {
    pub recoil: Interpolate,
    pub recovery: AccelDecelSine,
}

impl KickAxis {
    /// idWeaponKickAxis::Add 0x140f2f4a0.
    pub fn add(&mut self, now: i32, k: &WeaponKick, rng: &mut GameRng) {
        if k.kick <= 0.0 {
            return;
        }
        if k.max_kick <= 0.0 {
            // The game prints "idWeaponKickAxis::Init given ... maxKick <= 0" and ignores the kick.
            return;
        }
        let dir = match k.kind {
            KICK_NORMAL => {
                let r = rng.next01() - 0.5;
                r + if r <= 0.0 { -0.5 } else { 0.5 }
            }
            KICK_ONLY_POSITIVE => -(rng.next01() * 0.5 + 0.5),
            KICK_ONLY_NEGATIVE => rng.next01() * 0.5 + 0.5,
            KICK_ONLY_MAX_POSITIVE => -1.0,
            KICK_ONLY_MAX_NEGATIVE => 1.0,
            _ => 0.0,
        };
        let cur = self.value(now);
        let mut target = if !k.dampen {
            let mut t = cur + dir * k.kick;
            if k.max_kick <= t {
                t = k.max_kick;
            }
            if t <= -k.max_kick {
                t = -k.max_kick;
            }
            t
        } else {
            let f = cur / k.max_kick;
            (1.0 - (3.0 - (f + f)) * f * f) * dir * k.kick + cur
        };
        if target.abs() < 0.0001 {
            target = 0.0;
        }
        self.recoil.init(now, k.recoil_ms, cur, target);
        self.recovery.init(k.recovery_delay_ms + k.recoil_ms + now, 0, k.recovery_ms, k.recovery_ms, target, 0.0);
    }

    /// idWeaponKickAxis evaluate 0x140f2f420.
    pub fn value(&mut self, now: i32) -> f32 {
        let v = if now < self.recoil.end_time() { self.recoil.value(now) } else { self.recovery.value(now) };
        if v.abs() < 0.0001 { 0.0 } else { v }
    }
}

/// Kick offsets in degrees, game convention (player+0xd058..0xd064).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ViewKick {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub fov: f32,
}

/// `idWeaponKickComponent` (new system).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KickComponent {
    pub yaw: KickAxis,
    pub pitch: KickAxis,
    pub roll: KickAxis,
    pub fov: KickAxis,
}

impl KickComponent {
    /// 0x140f2f780: yaw, pitch, roll, fov in that order (RNG order matters).
    pub fn kick(&mut self, now: i32, fb: &FeedBack, rng: &mut GameRng) {
        self.yaw.add(now, &fb.kick_yaw, rng);
        self.pitch.add(now, &fb.kick_pitch, rng);
        self.roll.add(now, &fb.kick_roll, rng);
        self.fov.add(now, &fb.kick_fov, rng);
    }

    /// 0x140f2f220.
    pub fn offsets(&mut self, now: i32) -> ViewKick {
        ViewKick { yaw: self.yaw.value(now), pitch: self.pitch.value(now), roll: self.roll.value(now), fov: self.fov.value(now) }
    }
}

/// Game-time units per second used to turn deg/s into interpolation durations: idGameTimeManagerLocal vfunc
/// +0x138 0x140362a70 -> +0x140 = the timer's ticks per second (960: game time is idTypesafeTime<int, 960>).
const TIMER_MS_PER_SEC: f32 = 960.0;

/// Old kick system state (idPlayer+0xd058..0xd0c0), used when useNewKickSystem is false (BFG).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OldKick {
    pub yaw: f32,
    pub pitch: f32,
    pub pitch_target: f32,
    pub speed_into_scale: f32,
    pub speed_into: f32,
    pub speed_from: f32,
    pub yaw_speed_into: f32,
    pub yaw_speed_from: f32,
    pub recover_at: i32,
    pub yaw_into_active: bool,
    pub yaw_into: Interpolate,
    pub yaw_back: Interpolate,
}

impl OldKick {
    /// Old-system part of idPlayer::WeaponFired 0x140e47b80.
    pub fn kick(&mut self, now: i32, fb: &FeedBack, firing_interval: i32, rng: &mut GameRng) {
        let pitch_amount = fb.pitch_kick_amount;
        let r1 = rng.next01();
        let r2 = rng.next01();
        let yaw_kick = ((r2 + r2) - 1.0) * fb.yaw_kick_amount_delta + fb.yaw_kick_amount * ((r1 + r1) - 1.0);
        let mut scale = fb.pitch_kick_speed_into_per_shot + self.speed_into_scale;
        if 1.0 <= scale {
            scale = 1.0;
        }
        if scale <= fb.pitch_kick_speed_into_min {
            scale = fb.pitch_kick_speed_into_min;
        }
        self.speed_into_scale = scale;
        self.speed_into = scale * fb.pitch_kick_speed_into;
        self.speed_from = fb.pitch_kick_speed_from;
        self.yaw_speed_into = fb.yaw_kick_speed_into;
        self.yaw_speed_from = fb.yaw_kick_speed_from;
        self.recover_at = now + if fb.kick_recovery_delay == -1 { firing_interval } else { fb.kick_recovery_delay };
        let cur_yaw = self.yaw;
        let r3 = rng.next01() * fb.pitch_kick_amount_delta;
        let yaw_target = if !fb.do_unique_kick {
            self.pitch_target = (self.pitch - pitch_amount) - r3;
            let lim = yaw_kick.abs() * 2.0;
            let mut t = cur_yaw + yaw_kick;
            if lim <= t {
                t = lim;
            }
            if t <= -lim {
                t = -lim;
            }
            t
        } else {
            self.pitch_target = -pitch_amount - r3;
            yaw_kick
        };
        if fb.pitch_kick_top_bound != 0.0 && self.pitch_target <= -fb.pitch_kick_top_bound {
            self.pitch_target = -fb.pitch_kick_top_bound;
        }
        let dur = ((yaw_target - cur_yaw).abs() * TIMER_MS_PER_SEC / self.yaw_speed_into) as i32;
        self.yaw_into.init(now, dur, cur_yaw, yaw_target);
        self.yaw_back = Interpolate::default();
        self.yaw_back.init(0, 0, 0.0, 0.0);
        self.yaw_into_active = true;
    }

    /// Old-system branch of idPlayer::UpdateWeaponKick 0x140e472c0.
    pub fn update(&mut self, now: i32, frame_ms: i32) -> ViewKick {
        let dt = frame_ms as f32 * 0.001;
        let mut pitch = self.pitch - dt * self.speed_into;
        if pitch <= self.pitch_target {
            pitch = self.pitch_target;
        }
        if 0.0 <= self.pitch_target {
            self.speed_into_scale = 0.0;
        } else if self.recover_at < now {
            let mut t = dt * self.speed_from + self.pitch_target;
            if 0.0 <= t {
                t = 0.0;
            }
            self.pitch_target = t;
        }
        self.pitch = pitch;
        let yaw = if !self.yaw_into_active {
            self.yaw_back.value(now)
        } else {
            let y = self.yaw_into.value(now);
            if self.yaw_into.end_time() <= now {
                self.yaw_into_active = false;
                let dur = (y.abs() * TIMER_MS_PER_SEC / self.yaw_speed_from) as i32;
                self.yaw_back.init(now, dur, y, 0.0);
            }
            y
        };
        self.yaw = yaw;
        ViewKick { yaw, pitch, roll: 0.0, fov: 0.0 }
    }
}
