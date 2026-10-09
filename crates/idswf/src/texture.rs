//! CPU decoding of `.bimage` textures to RGBA8 (BC1/BC3/BC7 via `bcdec_rs`, plus raw RGBA8 and L8).

use anyhow::{Result, bail, ensure};
use idres::bimage::BImage;

/// Straight (non-premultiplied) RGBA8, row-major, top row first.
#[derive(Debug, Clone, Default)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Texture {
    pub fn white() -> Texture {
        Texture { width: 1, height: 1, rgba: vec![255; 4] }
    }

    /// Bilinear sample at normalised coordinates (clamped to the edge), returns 0..1 RGBA.
    pub fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        if self.width == 0 || self.height == 0 {
            return [1.0; 4];
        }
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let x0f = x.floor();
        let y0f = y.floor();
        let fx = x - x0f;
        let fy = y - y0f;
        let (w, h) = (self.width as i32, self.height as i32);
        let cx = |x: i32| x.clamp(0, w - 1) as usize;
        let cy = |y: i32| y.clamp(0, h - 1) as usize;
        let (x0, y0) = (x0f as i32, y0f as i32);
        let (xa, xb, ya, yb) = (cx(x0), cx(x0 + 1), cy(y0), cy(y0 + 1));
        let px = |x: usize, y: usize| {
            let o = (y * self.width as usize + x) * 4;
            &self.rgba[o..o + 4]
        };
        let (a, b, c, d) = (px(xa, ya), px(xb, ya), px(xa, yb), px(xb, yb));
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] as f32 + (b[i] as f32 - a[i] as f32) * fx;
            let bot = c[i] as f32 + (d[i] as f32 - c[i] as f32) * fx;
            out[i] = (top + (bot - top) * fy) * (1.0 / 255.0);
        }
        out
    }
}

/// Decodes mip 0 of a `.bimage`.
pub fn decode_bimage(bytes: &[u8]) -> Result<Texture> {
    let img = BImage::parse(bytes)?;
    let mip = img.mips.iter().find(|m| m.level == 0 && m.dest_z == 0).or(img.mips.first());
    let Some(mip) = mip else { bail!("bimage has no mips") };
    let data = &bytes[mip.data.clone()];
    decode(img.format, mip.width, mip.height, data)
}

pub fn decode(format: u8, width: u32, height: u32, data: &[u8]) -> Result<Texture> {
    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    match format {
        10 | 11 | 23 => {
            let block_bytes = if format == 10 { 8 } else { 16 };
            let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
            ensure!(data.len() >= bw * bh * block_bytes, "block data too short ({} < {})", data.len(), bw * bh * block_bytes);
            let mut tile = [0u8; 64];
            for by in 0..bh {
                for bx in 0..bw {
                    let blk = &data[(by * bw + bx) * block_bytes..][..block_bytes];
                    match format {
                        10 => bcdec_rs::bc1(blk, &mut tile, 16),
                        11 => bcdec_rs::bc3(blk, &mut tile, 16),
                        _ => bcdec_rs::bc7(blk, &mut tile, 16),
                    }
                    for row in 0..4 {
                        let y = by * 4 + row;
                        if y >= h {
                            break;
                        }
                        let cols = 4.min(w - bx * 4);
                        let dst = (y * w + bx * 4) * 4;
                        rgba[dst..dst + cols * 4].copy_from_slice(&tile[row * 16..row * 16 + cols * 4]);
                    }
                }
            }
        }
        3 => {
            ensure!(data.len() >= w * h * 4, "rgba8 data too short");
            rgba.copy_from_slice(&data[..w * h * 4]);
        }
        5 => {
            ensure!(data.len() >= w * h, "l8 data too short");
            for (i, &v) in data[..w * h].iter().enumerate() {
                rgba[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, v]);
            }
        }
        6 => {
            ensure!(data.len() >= w * h * 2, "la8 data too short");
            for i in 0..w * h {
                let (l, a) = (data[i * 2], data[i * 2 + 1]);
                rgba[i * 4..i * 4 + 4].copy_from_slice(&[l, l, l, a]);
            }
        }
        f => bail!("unsupported bimage format {f}"),
    }
    Ok(Texture { width, height, rgba })
}
