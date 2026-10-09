//! `.bmd6anim` skeletal animation (idTech 6 MD6), decoded from the user's install.
//!
//! File: magic `26 02 'A' 'M'`, two u32 timestamps, skeleton path (LE length string), translated and
//! normalized bounds (6 BE i16 each), BE u32 size, then `idMD6AnimData` (big-endian):
//! 8-byte pointer slot, u32 totalSize, u16 size, flags, numFrames, frameRate, numFrameSets,
//! frameSetTblOffset, frameSetOffsetTblOffset, constR/S/T/U offsets, nextSize, jointWeightsOffset,
//! start/end deltas. All offsets are relative to the start of `idMD6AnimData`.
//!
//! At +0x90: u16, u16 skeleton checksum, then 8 u16 offsets (relative to +0x90) of joint lists in the
//! order constR, constS, constT, constU, animR, animS, animT, animU. A joint list is a u8 total followed
//! by `(u8 count, u8 firstJoint)` runs. Constant tables hold one value per listed joint; each table is
//! padded to 16 bytes. Rotations are 6-byte smallest-three quaternions; S/T are vec3 f32; U is f32.
//!
//! Frame sets (located via the offset table, in 16-byte units) begin with `frameSetData_t`
//! (0x30 bytes): first/range/bits/next offsets for R,S,T,U, total size, frame start, frame range.
//! "first" holds each animated channel's key at the set's first frame; "next" points at the following
//! set's first keys (the key at frame start + range). "range" holds extra keys, channel by channel,
//! and "bits" one MSB-first mask per channel of ceil(range/8) bytes marking which frames have them.

use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone)]
pub struct Channels<T> {
    /// Skeleton joint index per channel.
    pub joints: Vec<u16>,
    /// Keys per channel as (frame, value), sorted by frame, covering the whole animation.
    pub keys: Vec<Vec<(u32, T)>>,
}

impl<T> Default for Channels<T> {
    fn default() -> Self {
        Self { joints: Vec::new(), keys: Vec::new() }
    }
}

#[derive(Debug, Clone)]
pub struct Md6Anim {
    pub skeleton: String,
    pub skel_checksum: u16,
    /// animData +0x0e: 0x400 = starts from the bind pose, 0x101 = additive; bits 2/4/8/0x10 zero origin-delta
    /// components (ANIMWEB.md 3b).
    pub flags: u16,
    pub num_frames: u32,
    pub frame_rate: u32,
    pub const_r: Vec<(u16, [f32; 4])>,
    pub const_s: Vec<(u16, [f32; 3])>,
    pub const_t: Vec<(u16, [f32; 3])>,
    pub const_u: Vec<(u16, f32)>,
    pub rot: Channels<[f32; 4]>,
    pub scale: Channels<[f32; 3]>,
    pub trans: Channels<[f32; 3]>,
    pub user: Channels<f32>,
}

fn be16(b: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(b.get(o..o + 2).context("anim truncated")?.try_into()?))
}
fn bef(b: &[u8], o: usize) -> Result<f32> {
    Ok(f32::from_be_bytes(b.get(o..o + 4).context("anim truncated")?.try_into()?))
}

/// Smallest-three quaternion: 15-bit components in [-1/√2, 1/√2]; the dropped component's index is
/// `(u1 >> 15) << 1 | (u0 >> 15)`, and the result is the decoded array rotated by that index.
pub fn decode_quat(b: &[u8], o: usize) -> Result<[f32; 4]> {
    let u0 = be16(b, o)?;
    let u1 = be16(b, o + 2)?;
    let u2 = be16(b, o + 4)?;
    let idx = ((((u0 >> 1) as u32) | (u1 & 0x8000) as u32) >> 14) as usize;
    let c = |u: u16| (u & 0x7fff) as f32 * 4.315969e-05 - 0.70710677;
    let (a, bb, cc) = (c(u0), c(u1), c(u2));
    let d = (1.0 - a * a - bb * bb - cc * cc).max(0.0).sqrt();
    let tmp = [a, bb, cc, d];
    Ok([tmp[idx & 3], tmp[(idx + 1) & 3], tmp[(idx + 2) & 3], tmp[(idx + 3) & 3]])
}

fn joint_list(b: &[u8], o: usize) -> Result<Vec<u16>> {
    let total = *b.get(o).context("joint list")? as usize;
    let mut out = Vec::with_capacity(total);
    let mut p = o + 1;
    while out.len() < total {
        let count = *b.get(p).context("joint list run")? as usize;
        let first = *b.get(p + 1).context("joint list run")? as usize;
        for j in 0..count {
            out.push((first + j) as u16);
        }
        p += 2;
    }
    ensure!(out.len() == total, "joint list overrun");
    Ok(out)
}

impl Md6Anim {
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.get(0..4) != Some(&[0x26, 0x02, b'A', b'M']) {
            bail!("bad bmd6anim magic");
        }
        let n = u32::from_le_bytes(b[12..16].try_into()?) as usize;
        let skeleton = String::from_utf8_lossy(b.get(16..16 + n).context("skeleton name")?).into_owned();
        let s = 16 + n + 24 + 4;
        let h = |i: usize| be16(b, s + 12 + i * 2);
        let flags = h(1)?;
        let num_frames = h(2)? as u32;
        let frame_rate = h(3)? as u32;
        let num_sets = h(4)? as usize;
        let fs_ofs_tbl = h(6)? as usize;
        let (c_r, c_s, c_t, c_u) = (h(7)? as usize, h(8)? as usize, h(9)? as usize, h(10)? as usize);

        let info = s + 0x90;
        let skel_checksum = be16(b, info + 2)?;
        let mut lists = Vec::with_capacity(8);
        for i in 0..8 {
            lists.push(joint_list(b, info + be16(b, info + 4 + i * 2)? as usize)?);
        }
        let mut a = Md6Anim {
            skeleton,
            skel_checksum,
            flags,
            num_frames,
            frame_rate,
            const_r: Vec::new(),
            const_s: Vec::new(),
            const_t: Vec::new(),
            const_u: Vec::new(),
            rot: Channels { joints: lists[4].clone(), keys: vec![Vec::new(); lists[4].len()] },
            scale: Channels { joints: lists[5].clone(), keys: vec![Vec::new(); lists[5].len()] },
            trans: Channels { joints: lists[6].clone(), keys: vec![Vec::new(); lists[6].len()] },
            user: Channels { joints: lists[7].clone(), keys: vec![Vec::new(); lists[7].len()] },
        };
        for (i, &j) in lists[0].iter().enumerate() {
            a.const_r.push((j, decode_quat(b, s + c_r + i * 6)?));
        }
        let v3 = |o: usize| -> Result<[f32; 3]> { Ok([bef(b, o)?, bef(b, o + 4)?, bef(b, o + 8)?]) };
        for (i, &j) in lists[1].iter().enumerate() {
            a.const_s.push((j, v3(s + c_s + i * 12)?));
        }
        for (i, &j) in lists[2].iter().enumerate() {
            a.const_t.push((j, v3(s + c_t + i * 12)?));
        }
        for (i, &j) in lists[3].iter().enumerate() {
            a.const_u.push((j, bef(b, s + c_u + i * 4)?));
        }

        for set in 0..num_sets {
            let fs = s + be16(b, s + fs_ofs_tbl + set * 2)? as usize * 16;
            let f = |i: usize| -> Result<usize> { Ok(be16(b, fs + i * 2)? as usize) };
            let start = f(17)? as u32;
            let range = f(18)? as u32;
            let mask_bytes = range.div_ceil(8) as usize;
            let has = |bits_ofs: usize, ch: usize, frame: u32| -> bool {
                let byte = b[fs + bits_ofs + ch * mask_bytes + (frame / 8) as usize];
                byte & (0x80 >> (frame % 8)) != 0
            };
            // Rotations.
            let (mut rp, bits) = (fs + f(4)?, f(8)?);
            for ch in 0..a.rot.joints.len() {
                a.rot.keys[ch].push((start, decode_quat(b, fs + f(0)? + ch * 6)?));
                for fr in 1..range {
                    if has(bits, ch, fr) {
                        a.rot.keys[ch].push((start + fr, decode_quat(b, rp)?));
                        rp += 6;
                    }
                }
            }
            // Scales and translations share the vec3 layout.
            for (chans, first, rng, bits) in [(&mut a.scale, f(1)?, f(5)?, f(9)?), (&mut a.trans, f(2)?, f(6)?, f(10)?)] {
                let mut p = fs + rng;
                for ch in 0..chans.joints.len() {
                    chans.keys[ch].push((start, v3(fs + first + ch * 12)?));
                    for fr in 1..range {
                        if has(bits, ch, fr) {
                            chans.keys[ch].push((start + fr, v3(p)?));
                            p += 12;
                        }
                    }
                }
            }
            let mut p = fs + f(7)?;
            for ch in 0..a.user.joints.len() {
                a.user.keys[ch].push((start, bef(b, fs + f(3)? + ch * 4)?));
                for fr in 1..range {
                    if has(f(11)?, ch, fr) {
                        a.user.keys[ch].push((start + fr, bef(b, p)?));
                        p += 4;
                    }
                }
            }
        }
        Ok(a)
    }

    /// The md6 decode starts this anim from identity (q = 0,0,0,1, S = 1, T = 0) instead of the skeleton's bind
    /// pose: additive anims (flags & 0x101) and anims without flag 0x400.
    pub fn base_identity(&self) -> bool {
        self.flags & 0x400 == 0 || self.flags & 0x101 != 0
    }

    pub fn duration_secs(&self) -> f32 {
        if self.frame_rate == 0 { 0.0 } else { (self.num_frames.saturating_sub(1)) as f32 / self.frame_rate as f32 }
    }
}

fn bracket<T: Copy>(keys: &[(u32, T)], frame: f32) -> Option<(T, T, f32)> {
    let first = keys.first()?;
    if frame <= first.0 as f32 || keys.len() == 1 {
        return Some((first.1, first.1, 0.0));
    }
    let i = keys.partition_point(|k| (k.0 as f32) <= frame);
    if i >= keys.len() {
        let l = keys[keys.len() - 1].1;
        return Some((l, l, 0.0));
    }
    let (a, b) = (keys[i - 1], keys[i]);
    let t = (frame - a.0 as f32) / (b.0 - a.0).max(1) as f32;
    Some((a.1, b.1, t))
}

pub fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let s = if dot < 0.0 { -1.0 } else { 1.0 };
    let mut q = [0.0; 4];
    for i in 0..4 {
        q[i] = a[i] * (1.0 - t) + b[i] * s * t;
    }
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt().max(1e-12);
    q.map(|v| v / l)
}

/// A sampled local pose: per-joint overrides on top of the skeleton bind pose.
#[derive(Debug, Clone, Default)]
pub struct Pose {
    pub rot: Vec<(u16, [f32; 4])>,
    pub trans: Vec<(u16, [f32; 3])>,
    pub scale: Vec<(u16, [f32; 3])>,
}

impl Md6Anim {
    /// Samples at `frame` (fractional), interpolating between stored keys (INTERIM: nlerp/linear;
    /// the game's interpolation routine is not yet verified).
    pub fn sample(&self, frame: f32) -> Pose {
        let mut p = Pose::default();
        p.rot.extend(self.const_r.iter().copied());
        p.trans.extend(self.const_t.iter().copied());
        p.scale.extend(self.const_s.iter().copied());
        for (ch, &j) in self.rot.joints.iter().enumerate() {
            if let Some((a, b, t)) = bracket(&self.rot.keys[ch], frame) {
                p.rot.push((j, nlerp(a, b, t)));
            }
        }
        let lerp3 = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
        for (ch, &j) in self.trans.joints.iter().enumerate() {
            if let Some((a, b, t)) = bracket(&self.trans.keys[ch], frame) {
                p.trans.push((j, lerp3(a, b, t)));
            }
        }
        for (ch, &j) in self.scale.joints.iter().enumerate() {
            if let Some((a, b, t)) = bracket(&self.scale.keys[ch], frame) {
                p.scale.push((j, lerp3(a, b, t)));
            }
        }
        p
    }
}
