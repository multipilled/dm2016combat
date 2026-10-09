//! CPU rasteriser for [`DrawList`]s: reproduces the engine's GUI shaders (guiblend, guiblend_coacgy and the
//! SDF font program), its blend equations and the stencil-based clip masks. Output is premultiplied RGBA.

use crate::render::{Blend, DrawList, Shader, StencilOp, TexRef, Vertex};
use crate::texture::Texture;

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Premultiplied RGBA, 0..1, gamma-space values (the GUI blends in display space).
    pub px: Vec<[f32; 4]>,
    pub stencil: Vec<u8>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Canvas {
        Canvas { width, height, px: vec![[0.0; 4]; width * height], stencil: vec![0; width * height] }
    }

    pub fn clear(&mut self) {
        self.px.fill([0.0; 4]);
        self.stencil.fill(0);
    }

    /// Premultiplied RGBA8 (for uploading to a GPU texture blended with premultiplied alpha).
    pub fn to_rgba8_premultiplied(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            out.extend(p.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
        }
        out
    }

    /// Composites over an opaque background colour (0..1 RGB) into RGBA8.
    pub fn over(&self, bg: [f32; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            for i in 0..3 {
                let v = p[i] + bg[i] * (1.0 - p[3].clamp(0.0, 1.0));
                out.push((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
            out.push(255);
        }
        out
    }

    /// Box-filters by an integer factor (premultiplied, so averaging is correct).
    pub fn downsample(&self, k: usize) -> Canvas {
        if k <= 1 {
            return Canvas { width: self.width, height: self.height, px: self.px.clone(), stencil: self.stencil.clone() };
        }
        let (w, h) = (self.width / k, self.height / k);
        let mut c = Canvas::new(w, h);
        let n = (k * k) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 4];
                for dy in 0..k {
                    for dx in 0..k {
                        let p = self.px[(y * k + dy) * self.width + x * k + dx];
                        for i in 0..4 {
                            acc[i] += p[i];
                        }
                    }
                }
                c.px[y * w + x] = acc.map(|v| v / n);
            }
        }
        c
    }
}

/// Rasterises `list` into `canvas`, mapping GUI coordinates with `pos * scale + offset`.
pub fn rasterize<'t>(list: &DrawList, canvas: &mut Canvas, scale: [f32; 2], offset: [f32; 2], textures: &dyn Fn(&TexRef) -> Option<&'t Texture>) {
    let white = Texture::white();
    for b in &list.batches {
        let tex = match &b.texture {
            TexRef::White => &white,
            t => textures(t).unwrap_or(&white),
        };
        for tri in b.indices.chunks_exact(3) {
            let v: [&Vertex; 3] = [&b.verts[tri[0] as usize], &b.verts[tri[1] as usize], &b.verts[tri[2] as usize]];
            let p = v.map(|v| [v.pos[0] * scale[0] + offset[0], v.pos[1] * scale[1] + offset[1]]);
            triangle(canvas, p, v, tex, b.shader, b.blend, b.op, b.stencil);
        }
    }
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

#[allow(clippy::too_many_arguments)]
fn triangle(c: &mut Canvas, p: [[f32; 2]; 3], v: [&Vertex; 3], tex: &Texture, shader: Shader, blend: Blend, op: StencilOp, stencil: u8) {
    let area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-12 || !area.is_finite() {
        return;
    }
    // Orient counter-clockwise in screen space (y down => positive area).
    let (p, v) = if area < 0.0 { ([p[0], p[2], p[1]], [v[0], v[2], v[1]]) } else { (p, v) };
    let area = area.abs();
    let minx = p.iter().map(|q| q[0]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
    let maxx = p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil().min(c.width as f32) as i64;
    let miny = p.iter().map(|q| q[1]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
    let maxy = p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil().min(c.height as f32) as i64;
    if minx >= maxx || miny >= maxy {
        return;
    }
    // Top-left rule: an edge owns pixels exactly on it only if it is a top or left edge.
    let owns = |a: [f32; 2], b: [f32; 2]| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        (dy == 0.0 && dx < 0.0) || dy > 0.0
    };
    let bias = [owns(p[1], p[2]), owns(p[2], p[0]), owns(p[0], p[1])];
    // Screen-space UV gradients for the SDF shader's dFdx/dFdy.
    let inv = 1.0 / area;
    let grad = |f: [f32; 3]| {
        let dx = (f[0] * (p[1][1] - p[2][1]) + f[1] * (p[2][1] - p[0][1]) + f[2] * (p[0][1] - p[1][1])) * inv;
        let dy = (f[0] * (p[2][0] - p[1][0]) + f[1] * (p[0][0] - p[2][0]) + f[2] * (p[1][0] - p[0][0])) * inv;
        (dx, dy)
    };
    let (dudx, _) = grad([v[0].uv[0], v[1].uv[0], v[2].uv[0]]);
    let (_, dvdy) = grad([v[0].uv[1], v[1].uv[1], v[2].uv[1]]);
    let pixel_delta = ((dudx.abs() + dvdy.abs()) * 32.0).clamp(0.0, 0.5);
    let flat = v[0].color == v[1].color && v[1].color == v[2].color && v[0].add == v[1].add && v[1].add == v[2].add;

    for y in miny..maxy {
        let py = y as f32 + 0.5;
        for x in minx..maxx {
            let px = x as f32 + 0.5;
            let q = [px, py];
            let w0 = edge(p[1], p[2], q);
            let w1 = edge(p[2], p[0], q);
            let w2 = edge(p[0], p[1], q);
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            if (w0 == 0.0 && !bias[0]) || (w1 == 0.0 && !bias[1]) || (w2 == 0.0 && !bias[2]) {
                continue;
            }
            let idx = y as usize * c.width + x as usize;
            match op {
                StencilOp::Incr => {
                    if c.stencil[idx] == stencil {
                        c.stencil[idx] = stencil.saturating_add(1);
                    }
                    continue;
                }
                StencilOp::Decr => {
                    if c.stencil[idx] == stencil + 1 {
                        c.stencil[idx] = stencil;
                    }
                    continue;
                }
                StencilOp::Draw => {
                    if c.stencil[idx] != stencil {
                        continue;
                    }
                }
            }
            let (b0, b1, b2) = (w0 * inv, w1 * inv, w2 * inv);
            let uv = [v[0].uv[0] * b0 + v[1].uv[0] * b1 + v[2].uv[0] * b2, v[0].uv[1] * b0 + v[1].uv[1] * b1 + v[2].uv[1] * b2];
            let (color, add) = if flat {
                (v[0].color, v[0].add)
            } else {
                (
                    std::array::from_fn(|i| v[0].color[i] * b0 + v[1].color[i] * b1 + v[2].color[i] * b2),
                    std::array::from_fn(|i| v[0].add[i] * b0 + v[1].add[i] * b1 + v[2].add[i] * b2),
                )
            };
            let src = shade(shader, tex, uv, color, add, pixel_delta);
            let dst = &mut c.px[idx];
            *dst = blend_px(blend, src, *dst);
        }
    }
}

fn sat(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = sat((x - e0) / (e1 - e0));
    t * t * (3.0 - 2.0 * t)
}

/// Premultiplied source colour from one of the GUI shaders.
fn shade(shader: Shader, tex: &Texture, uv: [f32; 2], color: [f32; 4], add: [f32; 4], pixel_delta: f32) -> [f32; 4] {
    match shader {
        Shader::Gui => {
            let t = tex.sample(uv[0], uv[1]);
            let c: [f32; 4] = std::array::from_fn(|i| t[i] * (color[i] + add[i]));
            let a = sat(c[3]);
            [sat(c[0]) * a, sat(c[1]) * a, sat(c[2]) * a, a]
        }
        Shader::Atlas => {
            let t = tex.sample(uv[0], uv[1]);
            let co = t[0] - 132.0 / 255.0;
            let cg = t[2] - 132.0 / 255.0;
            let y = t[3];
            let rgba = [y + co - cg, y + cg, y - co - cg, t[1]];
            let c: [f32; 4] = std::array::from_fn(|i| rgba[i] * color[i] + add[i]);
            let a = sat(c[3]);
            [sat(c[0]) * a, sat(c[1]) * a, sat(c[2]) * a, a]
        }
        Shader::Sdf => {
            let dist = tex.sample(uv[0], uv[1])[3];
            let alpha = smoothstep(0.5 - pixel_delta, 0.5 + pixel_delta, dist);
            let a = sat(alpha * color[3]);
            [color[0] * a, color[1] * a, color[2] * a, a]
        }
    }
}

fn blend_px(mode: Blend, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    std::array::from_fn(|i| {
        let v = match mode {
            Blend::Normal => s[i] + d[i] * (1.0 - s[3]),
            Blend::Add => s[i] + d[i],
            Blend::Multiply => s[i] * d[i] + d[i] * (1.0 - s[3]),
            Blend::Screen => d[i] - s[i] * d[i],
            Blend::Lighten => s[i].max(d[i]),
            Blend::Darken => s[i].min(d[i]),
            Blend::Subtract => d[i] - s[i],
            Blend::Overlay => d[i] + s[i] * d[i],
        };
        sat(v)
    })
}
