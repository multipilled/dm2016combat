//! Real-time software mixer for idaudio voices: resampling (rate × pitch), per-source-channel stereo
//! gains, one-pole LPF/HPF, start/stop at exact sample times, loops (WEM `smpl` loop points), fades and
//! a shared reverb fed by per-voice sends. Engine-agnostic: the game thread drives it through
//! [`crate::engine::Engine`]; the audio thread calls [`Mixer::render`]. Output is interleaved stereo
//! f32 at `rate`.

use std::sync::Arc;

use crate::wem::Decoded;

/// Decoded media shared by every voice that plays it.
#[derive(Debug)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: Vec<i16>,
    pub loop_points: Option<(u32, u32)>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

impl From<Decoded> for Pcm {
    fn from(d: Decoded) -> Self {
        Pcm { rate: d.rate, channels: d.channels, channel_mask: d.channel_mask, samples: d.samples, loop_points: d.loop_points }
    }
}

pub const MAX_CHANNELS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceParams {
    /// Linear gain from each source channel to the left / right output (dry path).
    pub gains: [[f32; 2]; MAX_CHANNELS],
    /// Linear gain of the reverb send, applied to the mean of the source channels (independent of the
    /// dry gains, as Wwise aux sends are).
    pub send: f32,
    /// Playback-rate multiplier from pitch (2^(cents/1200)); sample-rate conversion is added internally.
    pub pitch: f32,
    /// Low-pass cutoff in Hz (0 = bypass).
    pub lpf_hz: f32,
    /// High-pass cutoff in Hz (0 = bypass).
    pub hpf_hz: f32,
}

impl Default for VoiceParams {
    fn default() -> Self {
        Self { gains: [[0.0; 2]; MAX_CHANNELS], send: 0.0, pitch: 1.0, lpf_hz: 0.0, hpf_hz: 0.0 }
    }
}

pub struct VoiceStart {
    /// Caller's handle, reported back by [`Mixer::drain_finished`].
    pub tag: u64,
    pub pcm: Arc<Pcm>,
    /// Absolute mixer frame to start at (use [`Mixer::clock`] + delay).
    pub start_frame: u64,
    /// 0 = loop forever, n = play n times.
    pub loops: u32,
    pub params: VoiceParams,
}

struct OnePole {
    a: f32,
    z: [f32; 2],
}

impl OnePole {
    fn new() -> Self {
        Self { a: 1.0, z: [0.0; 2] }
    }
    fn set(&mut self, hz: f32, rate: f32) {
        self.a = if hz <= 0.0 { 1.0 } else { 1.0 - (-2.0 * std::f32::consts::PI * hz.min(rate * 0.49) / rate).exp() };
    }
    #[inline]
    fn low(&mut self, c: usize, x: f32) -> f32 {
        self.z[c] += self.a * (x - self.z[c]);
        self.z[c]
    }
}

struct MixVoice {
    tag: u64,
    pcm: Arc<Pcm>,
    start_frame: u64,
    pos: f64,
    loops_left: u32,
    target: VoiceParams,
    cur: VoiceParams,
    lpf: OnePole,
    hpf: OnePole,
    fade: f32,
    fade_step: f32,
    paused: bool,
    done: bool,
}

/// Reverb tuning (Freeverb-style network; an approximation of Wwise RoomVerb aux buses).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbParams {
    /// 0..1, longer tails as it grows.
    pub room_size: f32,
    /// 0..1 high-frequency damping in the tail.
    pub damping: f32,
    /// Output gain of the wet signal.
    pub wet: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self { room_size: 0.7, damping: 0.5, wet: 0.35 }
    }
}

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

struct Reverb {
    combs: [Vec<Comb>; 2],
    alls: [Vec<Allpass>; 2],
    p: ReverbParams,
}

impl Reverb {
    fn new(rate: u32) -> Self {
        let s = rate as f32 / 44100.0;
        let comb_len = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
        let all_len = [556, 441, 341, 225];
        let mk = |spread: usize| {
            (
                comb_len.iter().map(|l| Comb { buf: vec![0.0; ((l + spread) as f32 * s) as usize], i: 0, store: 0.0 }).collect(),
                all_len.iter().map(|l| Allpass { buf: vec![0.0; ((l + spread) as f32 * s) as usize], i: 0 }).collect(),
            )
        };
        let (c0, a0) = mk(0);
        let (c1, a1) = mk(23);
        Self { combs: [c0, c1], alls: [a0, a1], p: ReverbParams::default() }
    }

    #[inline]
    fn process(&mut self, input: f32) -> [f32; 2] {
        let feedback = 0.7 + 0.28 * self.p.room_size;
        let damp = 0.4 * self.p.damping;
        let mut out = [0.0f32; 2];
        for ch in 0..2 {
            let mut acc = 0.0;
            for c in &mut self.combs[ch] {
                let y = c.buf[c.i];
                c.store = y * (1.0 - damp) + c.store * damp;
                c.buf[c.i] = input + c.store * feedback;
                c.i = (c.i + 1) % c.buf.len();
                acc += y;
            }
            for a in &mut self.alls[ch] {
                let b = a.buf[a.i];
                a.buf[a.i] = acc + b * 0.5;
                acc = b - acc;
                a.i = (a.i + 1) % a.buf.len();
            }
            out[ch] = acc * self.p.wet;
        }
        out
    }
}

pub struct Mixer {
    pub rate: u32,
    clock: u64,
    voices: Vec<MixVoice>,
    finished: Vec<u64>,
    reverb: Reverb,
    pub master_gain: f32,
    scratch: Vec<f32>,
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, clock: 0, voices: Vec::new(), finished: Vec::new(), reverb: Reverb::new(rate), master_gain: 1.0, scratch: Vec::new() }
    }

    /// Frames rendered so far.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }

    pub fn start(&mut self, v: VoiceStart) {
        let mut lpf = OnePole::new();
        let mut hpf = OnePole::new();
        lpf.set(v.params.lpf_hz, self.rate as f32);
        hpf.set(v.params.hpf_hz, self.rate as f32);
        self.voices.push(MixVoice {
            tag: v.tag,
            pcm: v.pcm,
            start_frame: v.start_frame,
            pos: 0.0,
            loops_left: v.loops,
            target: v.params,
            cur: v.params,
            lpf,
            hpf,
            fade: 1.0,
            fade_step: 0.0,
            paused: false,
            done: false,
        });
    }

    pub fn set_params(&mut self, tag: u64, p: VoiceParams) {
        for v in self.voices.iter_mut().filter(|v| v.tag == tag) {
            v.target = p;
        }
    }

    /// Fades the voice out over `fade_frames` (0 = immediately) and removes it.
    pub fn stop(&mut self, tag: u64, fade_frames: u32) {
        for v in self.voices.iter_mut().filter(|v| v.tag == tag) {
            if fade_frames == 0 || self.clock < v.start_frame {
                v.done = true;
            } else {
                v.fade_step = -v.fade / fade_frames as f32;
            }
        }
    }

    pub fn pause(&mut self, tag: u64, paused: bool) {
        for v in self.voices.iter_mut().filter(|v| v.tag == tag) {
            v.paused = paused;
        }
    }

    pub fn is_playing(&self, tag: u64) -> bool {
        self.voices.iter().any(|v| v.tag == tag && !v.done)
    }

    /// Tags of voices that ended (played out or stopped) since the last call.
    pub fn drain_finished(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.finished)
    }

    pub fn set_reverb(&mut self, p: ReverbParams) {
        self.reverb.p = p;
    }

    /// Mixes the next `out.len() / 2` stereo frames into `out` (overwritten).
    pub fn render(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        out.fill(0.0);
        self.scratch.clear();
        self.scratch.resize(frames, 0.0);
        let rate = self.rate as f32;
        let block_start = self.clock;
        for v in &mut self.voices {
            if v.done || v.paused {
                continue;
            }
            let ch = v.pcm.channels.max(1) as usize;
            let total = v.pcm.frames();
            if total == 0 {
                v.done = true;
                continue;
            }
            let first = v.start_frame.saturating_sub(block_start).min(frames as u64) as usize;
            if first >= frames {
                continue;
            }
            v.lpf.set(v.target.lpf_hz, rate);
            v.hpf.set(v.target.hpf_hz, rate);
            let step = f64::from(v.pcm.rate) / f64::from(self.rate) * f64::from(v.target.pitch.max(0.01));
            let n = frames - first;
            // Ramp gains from the previous block's values to the targets across this block.
            let mut g = v.cur.gains;
            let mut dg = [[0.0f32; 2]; MAX_CHANNELS];
            for c in 0..ch.min(MAX_CHANNELS) {
                for s in 0..2 {
                    dg[c][s] = (v.target.gains[c][s] - g[c][s]) / n as f32;
                }
            }
            let mut send = v.cur.send;
            let dsend = (v.target.send - send) / n as f32;
            let (loop_start, loop_end) = match v.pcm.loop_points {
                Some((a, b)) if (b as usize) < total && a < b => (a as f64, b as f64 + 1.0),
                _ => (0.0, total as f64),
            };
            let s = &v.pcm.samples;
            for i in first..frames {
                // Loop points apply while passes remain; the last pass plays to the end of the file.
                let end = if v.loops_left == 1 { total as f64 } else { loop_end };
                if v.pos >= end {
                    if v.loops_left == 1 {
                        v.done = true;
                        break;
                    }
                    if v.loops_left > 1 {
                        v.loops_left -= 1;
                    }
                    v.pos = loop_start + (v.pos - loop_end).max(0.0);
                }
                let p0 = v.pos.floor() as usize;
                let frac = (v.pos - p0 as f64) as f32;
                let p1 = if p0 + 1 < total { p0 + 1 } else if v.loops_left != 1 { loop_start as usize } else { p0 };
                let (mut l, mut r, mut mono) = (0.0f32, 0.0f32, 0.0f32);
                for c in 0..ch.min(MAX_CHANNELS) {
                    let a = f32::from(s[p0 * ch + c]);
                    let b = f32::from(s[p1 * ch + c]);
                    let x = (a + (b - a) * frac) * (1.0 / 32768.0);
                    mono += x;
                    l += x * g[c][0];
                    r += x * g[c][1];
                    g[c][0] += dg[c][0];
                    g[c][1] += dg[c][1];
                }
                if v.lpf.a < 1.0 {
                    l = v.lpf.low(0, l);
                    r = v.lpf.low(1, r);
                }
                if v.hpf.a < 1.0 {
                    l -= v.hpf.low(0, l);
                    r -= v.hpf.low(1, r);
                }
                if v.fade_step != 0.0 {
                    v.fade += v.fade_step;
                    if v.fade <= 0.0 {
                        v.done = true;
                        break;
                    }
                }
                l *= v.fade;
                r *= v.fade;
                out[2 * i] += l;
                out[2 * i + 1] += r;
                self.scratch[i] += mono / ch as f32 * send * v.fade;
                send += dsend;
                v.pos += step;
            }
            v.cur = v.target;
        }
        for i in 0..frames {
            let w = self.reverb.process(self.scratch[i]);
            out[2 * i] = (out[2 * i] + w[0]) * self.master_gain;
            out[2 * i + 1] = (out[2 * i + 1] + w[1]) * self.master_gain;
        }
        self.clock += frames as u64;
        let finished = &mut self.finished;
        self.voices.retain(|v| {
            if v.done {
                finished.push(v.tag);
            }
            !v.done
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, frames: usize) -> Arc<Pcm> {
        let samples = (0..frames).map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 16000.0) as i16).collect();
        Arc::new(Pcm { rate, channels: 1, channel_mask: 4, samples, loop_points: None })
    }

    fn center() -> VoiceParams {
        let mut p = VoiceParams::default();
        p.gains[0] = [std::f32::consts::FRAC_1_SQRT_2; 2];
        p
    }

    #[test]
    fn starts_on_its_frame_and_ends() {
        let mut m = Mixer::new(48000);
        m.start(VoiceStart { tag: 7, pcm: tone(48000, 1000), start_frame: 300, loops: 1, params: center() });
        let mut out = vec![0.0; 2 * 2048];
        m.render(&mut out);
        assert!(out[..600].iter().all(|&x| x == 0.0));
        assert!(out[600..2600].iter().any(|&x| x != 0.0));
        assert!(out[2 * 1300..].iter().all(|&x| x == 0.0));
        assert_eq!(m.drain_finished(), vec![7]);
    }

    #[test]
    fn resamples_and_pitches() {
        // 24 kHz source at 48 kHz output plays twice as many frames; +1200 cents halves that again.
        let mut m = Mixer::new(48000);
        m.start(VoiceStart { tag: 1, pcm: tone(24000, 1000), start_frame: 0, loops: 1, params: center() });
        let mut p = center();
        p.pitch = 2.0;
        m.start(VoiceStart { tag: 2, pcm: tone(24000, 1000), start_frame: 0, loops: 1, params: p });
        let mut out = vec![0.0; 2 * 1500];
        m.render(&mut out);
        assert_eq!(m.drain_finished(), vec![2]);
        let mut out = vec![0.0; 2 * 1000];
        m.render(&mut out);
        assert_eq!(m.drain_finished(), vec![1]);
    }

    #[test]
    fn loops_and_stops_with_fade() {
        let mut m = Mixer::new(48000);
        m.start(VoiceStart { tag: 3, pcm: tone(48000, 100), start_frame: 0, loops: 0, params: center() });
        let mut out = vec![0.0; 2 * 4800];
        m.render(&mut out);
        assert!(m.is_playing(3));
        m.stop(3, 480);
        m.render(&mut out);
        assert_eq!(m.drain_finished(), vec![3]);
        assert!(out[2 * 600..].iter().all(|&x| x == 0.0));
    }
}
