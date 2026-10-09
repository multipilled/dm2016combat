//! idTech interpolators used by weapon kick and spread, as compiled into DOOMx64.exe.
//! Times are integer game milliseconds.

const K960: f32 = 0.001_041_666_7; // 1/960, the exe's extrapolation time scale (0x140cecad0)
const SQRT_1_2: f32 = 0.707_106_77;

fn snap(x: f32) -> f32 {
    if x.abs() <= 1e-18 { 0.0 } else { x }
}

/// `idInterpolate<float>`; GetCurrentValue 0x140362990.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Interpolate {
    pub start_time: i32,
    pub duration: i32,
    pub inv_duration: f32,
    pub start: f32,
    pub end: f32,
    current_time: i32,
    current: f32,
}

impl Interpolate {
    /// Field-for-field what the game writes when it restarts an interpolation (current time = start - 1).
    pub fn init(&mut self, start_time: i32, duration: i32, start: f32, end: f32) {
        let inv_duration = if duration == 0 { 0.0 } else { 1.0 / duration as f32 };
        *self = Self { start_time, duration, inv_duration, start, end, current_time: start_time - 1, current: start };
    }

    pub fn value(&mut self, t: i32) -> f32 {
        if t != self.current_time {
            self.current_time = t;
            let dt = t - self.start_time;
            let d = self.duration;
            let (before, after) = if d < 0 { (dt >= 0, dt <= d) } else { (dt < 1, d <= dt) };
            self.current = if before {
                self.start
            } else if after {
                self.end
            } else {
                let f = dt as f32;
                (1.0 - f * self.inv_duration) * snap(self.start) + snap(self.end) * f * self.inv_duration
            };
        }
        self.current
    }

    pub fn end_time(&self) -> i32 {
        self.start_time + self.duration
    }
}

/// `idExtrapolate<float>` as used by the accel/decel interpolator; GetCurrentValue 0x140cecad0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Extrapolate {
    kind: u32,
    start_time: i32,
    duration: i32,
    start: f32,
    base_speed: f32,
    speed: f32,
    current_time: i32,
    current: f32,
    inv_duration: f32,
}

const EX_LINEAR: u32 = 2;
const EX_ACCELSINE: u32 = 0x10;
const EX_DECELSINE: u32 = 0x20;
const EX_NOSTOP: u32 = 0x40;

impl Extrapolate {
    fn reinit(&mut self, kind: u32, start_time: i32, duration: i32, start: f32) {
        self.kind = kind;
        self.start_time = start_time;
        self.duration = duration;
        self.inv_duration = if duration != 0 { 1.0 / duration as f32 } else { 1.0 };
        self.start = start;
        self.current_time = -1;
        self.current = start;
    }

    fn value(&mut self, t: i32) -> f32 {
        if t == self.current_time {
            return self.current;
        }
        self.current_time = t;
        if t < self.start_time {
            return self.current;
        }
        let kind = self.kind & !EX_NOSTOP;
        if self.duration == 0 && kind.wrapping_sub(1) > 1 {
            return self.current;
        }
        let mut t = t;
        if self.kind & EX_NOSTOP == 0 && self.start_time + self.duration < t {
            t = self.start_time + self.duration;
        }
        let dt = (t - self.start_time) as f32;
        let dur = self.duration as f32;
        let f = dt * self.inv_duration;
        match kind {
            1 => self.current = dt * K960 * self.base_speed + self.start,
            EX_LINEAR => self.current = (self.speed + self.base_speed) * dt * K960 + self.start,
            4 => self.current = f * 0.5 * f * dur * K960 * self.speed + f * self.base_speed * dur * K960 + self.start,
            8 => self.current = (f - f * 0.5 * f) * dur * K960 * self.speed + f * self.base_speed * dur * K960 + self.start,
            EX_ACCELSINE => {
                let c = (f * std::f32::consts::FRAC_PI_2).cos();
                self.current = (1.0 - c) * dur * K960 * SQRT_1_2 * self.speed + f * self.base_speed + self.start;
            }
            EX_DECELSINE => {
                let s = (f * std::f32::consts::FRAC_PI_2).sin();
                self.current = s * dur * K960 * SQRT_1_2 * self.speed + f * self.base_speed + self.start;
            }
            _ => {}
        }
        self.current
    }
}

/// `idInterpolateAccelDecelSine<float>`: Init 0x140ceed20, SetPhase 0x140cf64e0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AccelDecelSine {
    pub start_time: i32,
    pub accel: i32,
    pub linear: i32,
    pub decel: i32,
    pub start: f32,
    pub end: f32,
    ex: Extrapolate,
}

impl AccelDecelSine {
    pub fn init(&mut self, start_time: i32, accel: i32, decel: i32, duration: i32, start: f32, end: f32) {
        self.start_time = start_time;
        self.accel = accel;
        self.decel = decel;
        self.start = start;
        self.end = end;
        if duration < 1 {
            // The game returns here without touching the extrapolator.
            self.linear = 0;
            return;
        }
        if accel + decel > duration {
            let a = accel * duration / (accel + decel);
            self.accel = a;
            self.decel = duration - a;
        }
        self.linear = duration - self.accel - self.decel;
        let speed = 960.0 / ((self.accel + self.decel) as f32 * SQRT_1_2 + self.linear as f32) * (end - start);
        let (kind, dur) = if self.accel != 0 {
            (EX_ACCELSINE, self.accel)
        } else if self.linear != 0 {
            (EX_LINEAR, self.linear)
        } else {
            (EX_DECELSINE, self.decel)
        };
        self.ex.reinit(kind, start_time, dur, start);
        self.ex.base_speed = 0.0;
        self.ex.speed = speed;
    }

    fn set_phase(&mut self, t: i32) {
        let dt = t - self.start_time;
        if dt < self.accel {
            if self.ex.kind != EX_ACCELSINE {
                self.ex.reinit(EX_ACCELSINE, self.start_time, self.accel, self.start);
            }
        } else if dt < self.linear + self.accel {
            if self.ex.kind != EX_LINEAR {
                let v = self.accel as f32 * K960 * SQRT_1_2 * self.ex.speed + self.start;
                self.ex.reinit(EX_LINEAR, self.accel + self.start_time, self.linear, v);
            }
        } else if self.ex.kind != EX_DECELSINE {
            let v = self.end - self.decel as f32 * K960 * SQRT_1_2 * self.ex.speed;
            self.ex.reinit(EX_DECELSINE, self.linear + self.accel + self.start_time, self.decel, v);
        }
    }

    pub fn end_time(&self) -> i32 {
        self.start_time + self.accel + self.linear + self.decel
    }

    /// SetPhase, extrapolate, then the end value once the whole curve has elapsed (as 0x140f2f420 reads it).
    pub fn value(&mut self, t: i32) -> f32 {
        self.set_phase(t);
        let v = self.ex.value(t);
        if self.end_time() <= t { self.end } else { v }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_interp_matches_game_edges() {
        let mut i = Interpolate::default();
        i.init(100, 67, 1.0, 3.0);
        assert_eq!(i.value(100), 1.0);
        assert_eq!(i.value(167), 3.0);
        assert!((i.value(133) - (1.0 + 2.0 * 33.0 / 67.0)).abs() < 1e-5);
    }

    #[test]
    fn pure_decel_sine_is_sine_ease_out() {
        let mut a = AccelDecelSine::default();
        a.init(1000, 0, 350, 350, 8.5, 0.0);
        assert_eq!(a.value(900), 8.5);
        assert!((a.value(1000) - 8.5).abs() < 1e-4);
        for t in [1050, 1175, 1300] {
            let expect = 8.5 * (1.0 - (std::f32::consts::FRAC_PI_2 * (t - 1000) as f32 / 350.0).sin());
            assert!((a.value(t) - expect).abs() < 1e-4, "{t}: {} vs {expect}", a.value(t));
        }
        assert_eq!(a.value(1350), 0.0);
    }
}
