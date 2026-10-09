//! 16-bit PCM WAV writer (WAVE_FORMAT_EXTENSIBLE with the channel mask when there are > 2 channels).

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

pub fn to_bytes(rate: u32, channels: u16, channel_mask: u32, samples: &[i16]) -> Vec<u8> {
    let extensible = channels > 2;
    let fmt_len: u32 = if extensible { 40 } else { 16 };
    let data_len = (samples.len() * 2) as u32;
    let block_align = channels * 2;
    let mut b = Vec::with_capacity(data_len as usize + 68);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(4 + 8 + fmt_len + 8 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&fmt_len.to_le_bytes());
    b.extend_from_slice(&(if extensible { 0xFFFEu16 } else { 1 }).to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
    b.extend_from_slice(&block_align.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    if extensible {
        b.extend_from_slice(&22u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(&channel_mask.to_le_bytes());
        // KSDATAFORMAT_SUBTYPE_PCM
        b.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
    }
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

pub fn write(path: &Path, rate: u32, channels: u16, channel_mask: u32, samples: &[i16]) -> Result<()> {
    let mut f = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(&to_bytes(rate, channels, channel_mask, samples))?;
    Ok(())
}
