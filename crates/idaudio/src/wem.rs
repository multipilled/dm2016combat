//! `.wem` media: RIFF/WAVE with Wwise's codec ids, and decoding to PCM.
//!
//! Every file in the install (bank-embedded and streamed) is `fmt ` tag 0x0002 = Wwise IMA ADPCM:
//! 1-6 channels, 11025-48000 Hz, 4 bits, `block_align = 36 * channels`, `fmt ` size 24 with
//! `cbSize = 6`, a u16 of unknown meaning (0, 0x10 or 0x18) and a u32 channel mask
//! (3 = L R, 4 = C, 0xB = L R LFE, 0xC = C LFE). Chunks seen: `fmt `, `JUNK` (alignment), `data`,
//! `smpl` (loop points), `cue `, `LIST`.

use anyhow::{Context, Result, bail, ensure};

use crate::adpcm;
use crate::read::Cursor;

pub const SPEAKER_FRONT_LEFT: u32 = 0x1;
pub const SPEAKER_FRONT_RIGHT: u32 = 0x2;
pub const SPEAKER_FRONT_CENTER: u32 = 0x4;
pub const SPEAKER_LFE: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// 0x0002 in a `.wem` (not Microsoft ADPCM).
    WwiseImaAdpcm,
    /// 0x0001 / 0xFFFE.
    Pcm,
    /// 0xFFFF; needs the Wwise codebook library to rebuild Vorbis headers.
    WwiseVorbis,
    /// 0x0166.
    Xma2,
    /// 0x3040 (WEM Opus), 0x3039 (Opus NX).
    Opus,
    Unknown(u16),
}

impl Codec {
    pub fn from_tag(tag: u16) -> Self {
        match tag {
            0x0002 => Codec::WwiseImaAdpcm,
            0x0001 | 0xFFFE => Codec::Pcm,
            0xFFFF => Codec::WwiseVorbis,
            0x0166 => Codec::Xma2,
            0x3039 | 0x3040 => Codec::Opus,
            t => Codec::Unknown(t),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WemInfo {
    pub format_tag: u16,
    pub codec: Codec,
    pub channels: u16,
    pub sample_rate: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    /// Bytes after `cbSize` in `fmt `.
    pub fmt_extra: Vec<u8>,
    pub channel_mask: u32,
    /// Offset of the `data` payload in the file and its declared size.
    pub data_offset: usize,
    pub data_size: usize,
    /// Bytes of `data` actually present (less than `data_size` for a bank's prefetch copy).
    pub data_available: usize,
    /// `smpl` loop: first and last sample (inclusive), if any.
    pub loop_points: Option<(u32, u32)>,
    /// `cue ` sample positions.
    pub cues: Vec<u32>,
    pub chunks: Vec<([u8; 4], usize)>,
}

impl WemInfo {
    pub fn is_truncated(&self) -> bool {
        self.data_available < self.data_size
    }

    /// Samples per channel of the complete file.
    pub fn total_frames(&self) -> u64 {
        match self.codec {
            Codec::WwiseImaAdpcm if self.block_align > 0 => {
                (self.data_size / self.block_align as usize * adpcm::BLOCK_SAMPLES) as u64
            }
            Codec::Pcm if self.block_align > 0 => (self.data_size / self.block_align as usize) as u64,
            _ => 0,
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.total_frames() as f64 / f64::from(self.sample_rate.max(1))
    }
}

fn default_mask(channels: u16) -> u32 {
    match channels {
        1 => SPEAKER_FRONT_CENTER,
        2 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        n => (1u32 << n.min(31)) - 1,
    }
}

pub fn parse(bytes: &[u8]) -> Result<WemInfo> {
    let mut r = Cursor::new(bytes);
    let magic = r.bytes(4)?;
    ensure!(magic == b"RIFF", "not a RIFF file (starts {magic:02x?})");
    let _riff_size = r.u32()?;
    ensure!(r.bytes(4)? == b"WAVE", "RIFF is not WAVE");

    let mut fmt: Option<&[u8]> = None;
    let mut data = None;
    let mut loop_points = None;
    let mut cues = Vec::new();
    let mut chunks = Vec::new();
    while r.remaining() >= 8 {
        let tag: [u8; 4] = r.bytes(4)?.try_into()?;
        let size = r.u32()? as usize;
        let offset = r.pos;
        chunks.push((tag, size));
        if &tag == b"data" {
            data = Some((offset, size, size.min(r.remaining())));
            break;
        }
        let body = r.bytes(size).with_context(|| format!("{} chunk truncated", String::from_utf8_lossy(&tag)))?;
        match &tag {
            b"fmt " => fmt = Some(body),
            b"smpl" if body.len() >= 36 => {
                let mut s = Cursor::new(body);
                s.skip(28)?;
                if s.u32()? > 0 && body.len() >= 60 {
                    s.skip(4 + 8)?; // sampler data, cue id, type
                    loop_points = Some((s.u32()?, s.u32()?));
                }
            }
            b"cue " if body.len() >= 4 => {
                let mut c = Cursor::new(body);
                for _ in 0..c.u32()? {
                    c.skip(20)?; // id, position, fcc chunk, chunk start, block start
                    cues.push(c.u32()?);
                }
            }
            _ => {}
        }
        if size & 1 == 1 && r.remaining() > 0 {
            r.skip(1)?;
        }
    }
    let fmt = fmt.context("no fmt chunk")?;
    let (data_offset, data_size, data_available) = data.context("no data chunk")?;
    let mut f = Cursor::new(fmt);
    let format_tag = f.u16()?;
    let channels = f.u16()?;
    let sample_rate = f.u32()?;
    let avg_bytes_per_sec = f.u32()?;
    let block_align = f.u16()?;
    let bits_per_sample = f.u16()?;
    let fmt_extra = if f.remaining() >= 2 {
        let cb = f.u16()? as usize;
        f.bytes(cb.min(f.remaining()))?.to_vec()
    } else {
        Vec::new()
    };
    let channel_mask = match (format_tag, fmt_extra.len()) {
        (0xFFFE, n) if n >= 6 => u32::from_le_bytes(fmt_extra[2..6].try_into()?),
        (0x0002, 6) => u32::from_le_bytes(fmt_extra[2..6].try_into()?),
        _ => default_mask(channels),
    };
    ensure!(channels > 0, "zero channels");
    Ok(WemInfo {
        format_tag,
        codec: Codec::from_tag(format_tag),
        channels,
        sample_rate,
        avg_bytes_per_sec,
        block_align,
        bits_per_sample,
        fmt_extra,
        channel_mask,
        data_offset,
        data_size,
        data_available,
        loop_points,
        cues,
        chunks,
    })
}

/// Decoded PCM, interleaved in the file's channel order (WAVE mask order: L, R, C, LFE, ...).
#[derive(Debug, Clone)]
pub struct Decoded {
    pub rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: Vec<i16>,
    pub loop_points: Option<(u32, u32)>,
    /// True when only a prefetch prefix was available.
    pub truncated: bool,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }

    /// Stereo fold-down: L/R straight, centre and mono at -3 dB into both sides, LFE and any other
    /// channels dropped. An approximation of Wwise's standard 2.0 downmix for non-positioned voices.
    pub fn downmix_stereo(&self) -> Vec<i16> {
        let ch = self.channels as usize;
        let mut gains = Vec::with_capacity(ch);
        let mut bit = 0;
        for _ in 0..ch {
            while bit < 32 && self.channel_mask & (1 << bit) == 0 {
                bit += 1;
            }
            let speaker = if bit < 32 { 1u32 << bit } else { 0 };
            bit += 1;
            gains.push(match speaker {
                SPEAKER_FRONT_LEFT => (1.0, 0.0),
                SPEAKER_FRONT_RIGHT => (0.0, 1.0),
                SPEAKER_FRONT_CENTER => (std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2),
                _ => (0.0, 0.0),
            });
        }
        let mut out = Vec::with_capacity(self.frames() * 2);
        for frame in self.samples.chunks_exact(ch) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for (s, (gl, gr)) in frame.iter().zip(&gains) {
                l += f32::from(*s) * gl;
                r += f32::from(*s) * gr;
            }
            out.push(l.round().clamp(-32768.0, 32767.0) as i16);
            out.push(r.round().clamp(-32768.0, 32767.0) as i16);
        }
        out
    }
}

/// Decodes a `.wem` (or a bank's prefetch prefix of one, yielding the available part).
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let info = parse(bytes)?;
    let data = &bytes[info.data_offset..info.data_offset + info.data_available];
    let samples = match info.codec {
        Codec::WwiseImaAdpcm => {
            ensure!(info.bits_per_sample == 4, "ADPCM with {} bits per sample", info.bits_per_sample);
            ensure!(
                info.block_align as usize == adpcm::BLOCK_BYTES * info.channels as usize,
                "ADPCM block_align {} for {} channels",
                info.block_align,
                info.channels
            );
            adpcm::decode(data, info.channels as usize, info.block_align as usize)
        }
        Codec::Pcm => {
            ensure!(info.bits_per_sample == 16, "PCM with {} bits per sample", info.bits_per_sample);
            data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
        }
        Codec::WwiseVorbis => bail!("Wwise Vorbis (0xFFFF) needs the codebook library to rebuild headers; not implemented"),
        Codec::Xma2 => bail!("XMA2 (0x0166) is a console codec; not implemented"),
        Codec::Opus => bail!("WEM Opus ({:#06x}) not implemented", info.format_tag),
        Codec::Unknown(t) => bail!("unknown WEM codec {t:#06x}"),
    };
    Ok(Decoded {
        rate: info.sample_rate,
        channels: info.channels,
        channel_mask: info.channel_mask,
        samples,
        loop_points: info.loop_points,
        truncated: info.is_truncated(),
    })
}
