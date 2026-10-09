//! Per-frame player input, in the game's own encoding (signed chars, ±127 at full deflection).

/// Buttons the movement code inspects.
pub mod button {
    /// Held crouch key (CheckDuck's toggle logic reads it separately from `up`).
    pub const CROUCH: u32 = 0x20;
    /// Walk modifier: forces the walk speed state.
    pub const WALK: u32 = 0x20_0000;
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UserCmd {
    pub forward: i8,
    pub right: i8,
    /// > 0 jump, < 0 crouch.
    pub up: i8,
    pub buttons: u32,
    /// pitch, yaw, roll in degrees; yaw counter-clockwise from +X, positive pitch looks down.
    pub angles: [f32; 3],
}

impl UserCmd {
    pub fn has(&self, b: u32) -> bool {
        self.buttons & b != 0
    }
}

/// The engine frame clock with `com_fixedTic 1` and `com_adaptiveTick 1` in immediate mode (the
/// shipped defaults): every rendered frame runs exactly one game frame (0x1415a2500 leaves no residual),
/// and that frame's length follows the previous real frame (0x1415a3890):
///
/// ```text
/// us  = clamp(real frame us, 1e6/com_adaptiveTickMaxHz, 1e6/com_adaptiveTickMinHz)
/// s   = u64(us*0.96 + carry)          hz = ceil(1e6 / s)          carry = s - 1e6/hz
/// x   = 1000/hz + frac                msec = int(x)               frac = x - floor(x)   (0x14158ecd0)
/// ```
#[derive(Debug, Clone)]
pub struct AdaptiveTick {
    pub min_hz: i32,
    pub max_hz: i32,
    /// Game rate chosen for the last frame.
    pub hz: i32,
    carry_us: u64,
    frac: f32,
}

impl AdaptiveTick {
    pub fn new(min_hz: i32, max_hz: i32) -> Self {
        Self { min_hz, max_hz, hz: 60, carry_us: 0, frac: 0.0 }
    }

    /// Game msec for the frame that follows a real frame of `frame_us` microseconds.
    pub fn next(&mut self, frame_us: u64) -> i32 {
        let min_us = 1_000_000 / self.max_hz as u64;
        let max_us = 1_000_000 / self.min_hz as u64;
        let us = frame_us.max(min_us).min(max_us);
        let scaled = (us as f32 * f32::from_bits(0x3f75_c290) + self.carry_us as f32) as u64;
        let hz = (1.0e6f32 / scaled as f32).ceil() as i32;
        self.carry_us = scaled.wrapping_sub(1_000_000 / hz as u64);
        self.set_hz(hz)
    }

    /// The game timer's SetHz (0x14158ec70 -> 0x14158ecd0): whole msec per frame, fraction carried.
    pub fn set_hz(&mut self, hz: i32) -> i32 {
        if hz > 0 {
            self.hz = hz;
        }
        let x = 1000.0 / self.hz as f32 + self.frac;
        self.frac = x - x.floor();
        x as i32
    }
}
