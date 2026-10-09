//! Wwise IMA ADPCM (WEM format tag 0x0002), bit-exact with the decoder in DOOMx64.exe.
//!
//! Exe references: block decoder 0x141dc3850 (tables: step 0x14299a580 i16[89], index 0x14299a560
//! i16[16]), per-channel driver 0x141dbd130.
//!
//! A frame is `block_align = 36 * channels` bytes holding one 36-byte block per channel, back to back
//! (not interleaved). A block is `i16 predictor, u8 step index, u8 reserved` and 32 data bytes and
//! yields 64 samples: the predictor itself, then the low and high nibble of bytes 0..31, then only the
//! low nibble of byte 31 (the final nibble is never decoded). A nibble expands as
//! `delta = (step * (2*(n & 7) + 1)) >> 3`, negated when `n & 8`, added to the previous sample with
//! i16 saturation; the step index moves by the index table and clamps to 0..=88.

pub const BLOCK_BYTES: usize = 36;
pub const BLOCK_SAMPLES: usize = 64;

const STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118,
    130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060,
    1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132,
    7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

#[inline]
fn expand(nibble: u8, sample: &mut i32, index: &mut i32) {
    let step = STEP[*index as usize];
    let delta = (step * (2 * i32::from(nibble & 7) + 1)) >> 3;
    let s = if nibble & 8 != 0 { *sample - delta } else { *sample + delta };
    *sample = s.clamp(-0x8000, 0x7fff);
    *index = (*index + INDEX[nibble as usize & 0xf]).clamp(0, 88);
}

/// Decodes one 36-byte block into 64 samples written at `out[0], out[stride], ...`.
pub fn decode_block(block: &[u8], out: &mut [i16], stride: usize) {
    let mut sample = i32::from(i16::from_le_bytes([block[0], block[1]]));
    // The exe indexes the step table with the raw byte; clamp so corrupt data cannot index out of range.
    let mut index = i32::from(block[2]).min(88);
    out[0] = sample as i16;
    let data = &block[4..BLOCK_BYTES];
    let mut o = stride;
    for &b in &data[..31] {
        expand(b & 0xf, &mut sample, &mut index);
        out[o] = sample as i16;
        expand(b >> 4, &mut sample, &mut index);
        out[o + stride] = sample as i16;
        o += 2 * stride;
    }
    expand(data[31] & 0xf, &mut sample, &mut index);
    out[o] = sample as i16;
}

/// Decodes whole frames of `data` to interleaved samples. Trailing bytes short of a frame are ignored,
/// as the engine does at end of file.
pub fn decode(data: &[u8], channels: usize, block_align: usize) -> Vec<i16> {
    assert!(channels > 0 && block_align == BLOCK_BYTES * channels, "Wwise ADPCM needs block_align = 36 * channels");
    let frames = data.len() / block_align;
    let mut out = vec![0i16; frames * BLOCK_SAMPLES * channels];
    for (f, frame) in data.chunks_exact(block_align).enumerate() {
        let base = f * BLOCK_SAMPLES * channels;
        for c in 0..channels {
            decode_block(&frame[c * BLOCK_BYTES..(c + 1) * BLOCK_BYTES], &mut out[base + c..], channels);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straight transcription of the exe loop (0x141dc3850), used as an oracle.
    fn reference_block(block: &[u8]) -> Vec<i16> {
        let mut out = Vec::new();
        let mut hist = i32::from(i16::from_le_bytes([block[0], block[1]]));
        let mut idx = i32::from(block[2]);
        out.push(hist as i16);
        let step = |i: i32| STEP[i as usize];
        let nib = |n: u8, hist: &mut i32, idx: &mut i32| {
            let t = step(*idx) * ((i32::from(n & 7)) * 2 + 1);
            let d = (t + ((t >> 31) & 7)) >> 3;
            let v = if n & 8 == 0 { d } else { -d } + *hist;
            *hist = if v as i16 as i32 != v { if v < -0x8000 { -0x8000 } else { 0x7fff } } else { v };
            *idx = (INDEX[(n & 0xf) as usize] + *idx).clamp(0, 88);
            *hist as i16
        };
        for i in 0..31 {
            let b = block[4 + i];
            out.push(nib(b & 0xf, &mut hist, &mut idx));
            out.push(nib(b >> 4, &mut hist, &mut idx));
        }
        out.push(nib(block[35] & 0xf, &mut hist, &mut idx));
        out
    }

    #[test]
    fn matches_exe_transcription() {
        let mut seed = 0x1234_5678u32;
        for _ in 0..200 {
            let mut block = [0u8; BLOCK_BYTES];
            for b in &mut block {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            block[2] %= 89;
            let mut out = [0i16; BLOCK_SAMPLES];
            decode_block(&block, &mut out, 1);
            assert_eq!(out.to_vec(), reference_block(&block));
        }
    }

    #[test]
    fn stereo_frames_are_blocked_per_channel() {
        let mut frame = [0u8; 72];
        frame[0..2].copy_from_slice(&1000i16.to_le_bytes());
        frame[36..38].copy_from_slice(&(-1000i16).to_le_bytes());
        let pcm = decode(&frame, 2, 72);
        assert_eq!(pcm.len(), 128);
        assert_eq!((pcm[0], pcm[1]), (1000, -1000));
    }
}
