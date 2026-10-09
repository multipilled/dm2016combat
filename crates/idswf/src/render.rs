//! Renderer-agnostic draw lists, following the engine's SWF renderer:
//! `idSWF::Render` 0x141620420, `RenderSprite` 0x141622330, `RenderShape` 0x141621860,
//! `RenderMorphShape` 0x141620ce0, `RenderEditText` 0x14161e230, render state bits 0x14161d5d0.
//!
//! Vertices are in GUI pixels. Colours follow the engine's vertex format: `color` is the multiply term and
//! `add` the additive term of the colour transform, both clamped to 8 bits like the engine packs them.

use std::sync::Arc;

use crate::bswf::{DictEntry, FillKind, Matrix, Shape};
use crate::player::{ObjId, Player};
use crate::tags::ColorXform;

/// `swf_safeFrame` default: space between anchored UI elements and the screen edge.
pub const SAFE_FRAME: f32 = 0.005;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TexRef {
    /// Plain white (`guiSolid`, `_white`).
    White,
    /// The SWF's own atlas (`<swf>.bimage`), stored as Co/A/Cg/Y.
    Atlas,
    /// SDF font atlas by face name.
    Font(Arc<str>),
    /// A material decl set on a sprite through its `material` property.
    Material(Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shader {
    /// guiblend: `c = tex * (color + add)`, premultiplied output.
    Gui,
    /// guiblend_coacgy: decode Co/A/Cg/Y, `c = rgba * color + add`.
    Atlas,
    /// fontoutlineglowshadow without effects: SDF coverage times vertex colour.
    Sdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StencilOp {
    /// Draw where stencil == `stencil`.
    Draw,
    /// Mask layer: where stencil == `stencil`, increment.
    Incr,
    /// End of mask: where stencil == `stencil` + 1, decrement.
    Decr,
}

/// GPU blend equations used for each SWF blend mode (from the GLS bits built at 0x14161d5d0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    /// ONE, ONE_MINUS_SRC_ALPHA (normal, and every mode the engine doesn't special-case)
    Normal,
    /// ONE, ONE (SWF add)
    Add,
    /// DST_COLOR, ONE_MINUS_SRC_ALPHA (SWF multiply)
    Multiply,
    /// DST_COLOR, ONE with reverse subtract (SWF screen)
    Screen,
    /// ONE, ONE, max (lighten)
    Lighten,
    /// ONE, ONE, min (darken)
    Darken,
    /// ONE, ONE, reverse subtract (difference, subtract)
    Subtract,
    /// DST_COLOR, ONE (overlay, hard light)
    Overlay,
}

impl Blend {
    pub fn from_swf(mode: u8) -> Blend {
        match mode {
            3 => Blend::Multiply,
            4 => Blend::Screen,
            5 => Blend::Lighten,
            6 => Blend::Darken,
            7 | 9 => Blend::Subtract,
            8 => Blend::Add,
            13 | 14 => Blend::Overlay,
            _ => Blend::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub add: [f32; 4],
}

#[derive(Debug, Clone)]
pub struct Batch {
    pub texture: TexRef,
    pub shader: Shader,
    pub blend: Blend,
    pub stencil: u8,
    pub op: StencilOp,
    pub verts: Vec<Vertex>,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct DrawList {
    pub width: f32,
    pub height: f32,
    pub batches: Vec<Batch>,
}

impl DrawList {
    fn push(&mut self, b: Batch) {
        if b.indices.is_empty() {
            return;
        }
        if let Some(last) = self.batches.last_mut() {
            if last.texture == b.texture && last.shader == b.shader && last.blend == b.blend && last.stencil == b.stencil && last.op == b.op {
                let base = last.verts.len() as u32;
                last.verts.extend_from_slice(&b.verts);
                last.indices.extend(b.indices.iter().map(|i| i + base));
                return;
            }
        }
        self.batches.push(b);
    }
}

#[derive(Debug, Clone)]
struct State {
    m: Matrix,
    cx: ColorXform,
    blend: u8,
    stencil: u8,
    material: Option<(Arc<str>, i32, i32)>,
}

/// Quantises a colour the way the engine packs vertex colours (0x141a59ef0: (int)(v*255) clamped).
fn pack(c: [f32; 4]) -> [f32; 4] {
    c.map(|v| ((v * 255.0) as i32).clamp(0, 255) as f32 / 255.0)
}

fn pack_add(a: [f32; 4]) -> [f32; 4] {
    pack(a.map(|v| v * 0.5 + 0.5)).map(|v| v * 2.0 - 1.0)
}

/// Builds the draw list for the whole player at GUI size `gui_w` x `gui_h` (the engine scales the stage
/// uniformly to fit and centres it).
pub fn draw(p: &Player, gui_w: f32, gui_h: f32) -> DrawList {
    draw_with(p, gui_w, gui_h, false)
}

/// Like [`draw`], but clip layers are applied by clipping the geometry against the mask triangles, so the
/// result has only `StencilOp::Draw` batches with stencil 0 (for renderers without a stencil buffer).
pub fn draw_clipped(p: &Player, gui_w: f32, gui_h: f32) -> DrawList {
    draw_with(p, gui_w, gui_h, true)
}

fn draw_with(p: &Player, gui_w: f32, gui_h: f32, geometric: bool) -> DrawList {
    let swf = &p.swf;
    let s = (gui_w / swf.frame_width).min(gui_h / swf.frame_height);
    let state = State {
        m: Matrix { xx: s, yy: s, xy: 0.0, yx: 0.0, tx: (gui_w - swf.frame_width * s) * 0.5, ty: (gui_h - swf.frame_height * s) * 0.5 },
        cx: ColorXform::IDENTITY,
        blend: 0,
        stencil: 0,
        material: None,
    };
    let mut out = DrawList { width: gui_w, height: gui_h, batches: Vec::new() };
    let mut ctx = Ctx { p, out: &mut out, gui_w, gui_h, geometric, masks: Vec::new() };
    ctx.sprite(p.root, &state);
    out
}

type Tri = [[f32; 2]; 3];

struct Ctx<'a> {
    p: &'a Player,
    out: &'a mut DrawList,
    gui_w: f32,
    gui_h: f32,
    /// Clip masks geometrically instead of emitting stencil batches.
    geometric: bool,
    /// Active clip layers (stencil level, mask triangles in GUI space), innermost last.
    masks: Vec<(u8, Vec<Tri>)>,
}

/// Clips `poly` (positions + barycentric weights) to the inside of triangle `t`.
fn clip_poly(poly: &[([f32; 2], [f32; 3])], t: &Tri) -> Vec<([f32; 2], [f32; 3])> {
    let area = (t[1][0] - t[0][0]) * (t[2][1] - t[0][1]) - (t[1][1] - t[0][1]) * (t[2][0] - t[0][0]);
    if area.abs() < 1e-12 {
        return Vec::new();
    }
    let sign = area.signum();
    let mut cur: Vec<([f32; 2], [f32; 3])> = poly.to_vec();
    for e in 0..3 {
        let (a, b) = (t[e], t[(e + 1) % 3]);
        let side = |p: [f32; 2]| ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])) * sign;
        let mut next = Vec::with_capacity(cur.len() + 2);
        for i in 0..cur.len() {
            let (p, q) = (cur[i], cur[(i + 1) % cur.len()]);
            let (sp, sq) = (side(p.0), side(q.0));
            if sp >= 0.0 {
                next.push(p);
            }
            if (sp >= 0.0) != (sq >= 0.0) {
                let k = sp / (sp - sq);
                let lerp = |x: f32, y: f32| x + (y - x) * k;
                next.push(([lerp(p.0[0], q.0[0]), lerp(p.0[1], q.0[1])], [lerp(p.1[0], q.1[0]), lerp(p.1[1], q.1[1]), lerp(p.1[2], q.1[2])]));
            }
        }
        cur = next;
        if cur.len() < 3 {
            return Vec::new();
        }
    }
    cur
}

fn mix_vertex(v: [&Vertex; 3], w: [f32; 3], pos: [f32; 2]) -> Vertex {
    let m4 = |f: fn(&Vertex) -> [f32; 4]| -> [f32; 4] { std::array::from_fn(|i| f(v[0])[i] * w[0] + f(v[1])[i] * w[1] + f(v[2])[i] * w[2]) };
    Vertex {
        pos,
        uv: [v[0].uv[0] * w[0] + v[1].uv[0] * w[1] + v[2].uv[0] * w[2], v[0].uv[1] * w[0] + v[1].uv[1] * w[1] + v[2].uv[1] * w[2]],
        color: m4(|v| v.color),
        add: m4(|v| v.add),
    }
}

/// Anchor mode from a sprite's instance name (table at 0x1428855d0; 5 = none).
fn anchor_mode(name: &str) -> u32 {
    const NAMES: [&str; 19] = [
        "_fullScreen",
        "_topLeft",
        "_top",
        "_topRight",
        "_left",
        "_center",
        "_right",
        "_bottomLeft",
        "_bottom",
        "_bottomRight",
        "_absTopLeft",
        "_absTop",
        "_absTopRight",
        "_absLeft",
        "_absRight",
        "_absBottomLeft",
        "_absBottom",
        "_absBottomRight",
        "_absCenter",
    ];
    if !name.starts_with('_') {
        return 5;
    }
    if let Some(i) = NAMES.iter().position(|n| n.eq_ignore_ascii_case(name)) {
        return i as u32;
    }
    if name.contains("_absLeft") {
        return 13;
    }
    if name.contains("_absRight") {
        return 14;
    }
    5
}

impl Ctx<'_> {
    fn sprite(&mut self, id: ObjId, state: &State) {
        let Some(sp) = self.p.sprite(id) else { return };
        if state.cx.mul[3] + state.cx.add[3] <= 0.001 {
            return;
        }
        let swf = self.p.swf.clone();
        let mut masks: Vec<(u16, usize)> = Vec::new(); // (clip depth, display index)
        for (di, d) in sp.display.iter().enumerate() {
            // Close masks whose range ended.
            let mut k = 0;
            while k < masks.len() {
                if masks[k].0 < d.depth {
                    let md = &sp.display[masks[k].1];
                    let st = State { stencil: state.stencil + (masks.len() as u8 - 1), ..state.clone() };
                    self.mask(md, &st, StencilOp::Decr);
                    masks.remove(k);
                } else {
                    k += 1;
                }
            }
            if d.clip_depth != 0 {
                let st = State { stencil: state.stencil + masks.len() as u8, ..state.clone() };
                masks.push((d.clip_depth, di));
                self.mask(d, &st, StencilOp::Incr);
                continue;
            }
            if !d.visible {
                continue;
            }
            let child = self.child_state(state, d, masks.len() as u8, sp.material.clone().map(|m| (m, sp.material_width, sp.material_height)));
            match swf.dict.get(d.character as usize) {
                Some(DictEntry::Shape(shape)) => self.shape(shape, &child, None),
                Some(DictEntry::Morph(shape)) => self.shape(shape, &child, Some(d.ratio)),
                Some(DictEntry::EditText(_)) => {
                    if let Some(t) = d.inst {
                        self.edit_text(t, &child);
                    }
                }
                Some(DictEntry::Sprite(_)) => {
                    if let Some(c) = d.inst {
                        let mut cs = child.clone();
                        self.anchor(c, d, state, &mut cs);
                        self.sprite(c, &cs);
                    }
                }
                _ => {}
            }
        }
        for (i, &(_, di)) in masks.iter().enumerate().rev() {
            let st = State { stencil: state.stencil + i as u8, ..state.clone() };
            self.mask(&sp.display[di], &st, StencilOp::Decr);
        }
    }

    fn child_state(&self, state: &State, d: &crate::player::Display, nmasks: u8, material: Option<(Arc<str>, i32, i32)>) -> State {
        State {
            m: state.m.mul(&d.matrix),
            cx: d.cxform.concat(&state.cx),
            blend: if d.blend == 0 { state.blend } else { d.blend },
            stencil: state.stencil + nmasks,
            material: material.or_else(|| state.material.clone()),
        }
    }

    /// Screen-edge anchoring of named sprites (RenderSprite's switch on the child's anchor mode).
    fn anchor(&self, child: ObjId, d: &crate::player::Display, parent: &State, cs: &mut State) {
        let name = self.p.instance_name(child);
        let mode = anchor_mode(&name);
        if mode == 5 {
            return;
        }
        let swf = &self.p.swf;
        let (fw, fh) = (swf.frame_width, swf.frame_height);
        let (gw, gh) = (self.gui_w, self.gui_h);
        let (sx, sy) = (SAFE_FRAME * fw, SAFE_FRAME * fh);
        let (px, py) = (parent.m.xx, parent.m.yy);
        let (ex, ey) = (d.matrix.tx, d.matrix.ty);
        let (nx, ny) = (cs.m.tx, cs.m.ty);
        let left = (sx + ex) * px;
        let right = gw - ((fw - ex) + sx) * px;
        let top = (sy + ey) * py;
        let bottom = gh - ((fh - ey) + sy) * py;
        let (x, y) = match mode {
            0 => {
                cs.m.xx = gw / fw;
                cs.m.yy = gh / fh;
                (ex * px, ey * py)
            }
            1 => (left, top),
            2 => (nx, top),
            3 => (right, top),
            4 => (left, ny),
            6 => (right, ny),
            7 => (left, bottom),
            8 => (nx, bottom),
            9 => (right, bottom),
            10 => (ex * px, ey * py),
            11 => (nx, ey * py),
            12 => (gw - (fw - ex) * px, ey * py),
            13 => (px * ex, ny),
            14 => (gw - (fw - ex) * px, ny),
            15 => (ex * px, gh - (fh - ey) * py),
            16 => (nx, gh - (fh - ey) * py),
            17 => (gw - (fw - ex) * px, gh - (fh - ey) * py),
            18 => (gw * 0.5 - (fw * 0.5 - ex) * px, ny),
            _ => (nx, ny),
        };
        cs.m.tx = x;
        cs.m.ty = y;
    }

    /// Renders a mask layer's geometry into the stencil.
    fn mask(&mut self, d: &crate::player::Display, parent: &State, op: StencilOp) {
        let st = State { m: parent.m.mul(&d.matrix), cx: ColorXform::IDENTITY, blend: 0, stencil: parent.stencil, material: None };
        let mut tmp = DrawList::default();
        {
            let mut sub = Ctx { p: self.p, out: &mut tmp, gui_w: self.gui_w, gui_h: self.gui_h, geometric: false, masks: Vec::new() };
            match self.p.swf.dict.get(d.character as usize) {
                Some(DictEntry::Shape(s)) => sub.shape(s, &st, None),
                Some(DictEntry::Morph(s)) => sub.shape(s, &st, Some(d.ratio)),
                Some(DictEntry::Sprite(_)) => {
                    if let Some(c) = d.inst {
                        sub.sprite(c, &st);
                    }
                }
                _ => {}
            }
        }
        if self.geometric {
            match op {
                StencilOp::Incr => {
                    let mut tris = Vec::new();
                    for b in &tmp.batches {
                        for t in b.indices.chunks_exact(3) {
                            tris.push([b.verts[t[0] as usize].pos, b.verts[t[1] as usize].pos, b.verts[t[2] as usize].pos]);
                        }
                    }
                    self.masks.push((parent.stencil, tris));
                }
                _ => {
                    if let Some(i) = self.masks.iter().rposition(|m| m.0 == parent.stencil) {
                        self.masks.remove(i);
                    }
                }
            }
            return;
        }
        for mut b in tmp.batches {
            b.op = op;
            b.stencil = parent.stencil;
            self.out.push(b);
        }
    }

    /// Adds a draw batch, clipping it against the active masks in geometric mode.
    fn emit(&mut self, mut b: Batch) {
        if !self.geometric || b.op != StencilOp::Draw {
            self.out.push(b);
            return;
        }
        b.stencil = 0;
        if self.masks.is_empty() {
            self.out.push(b);
            return;
        }
        let mut verts = Vec::new();
        let mut indices = Vec::new();
        for t in b.indices.chunks_exact(3) {
            let v = [&b.verts[t[0] as usize], &b.verts[t[1] as usize], &b.verts[t[2] as usize]];
            let mut pieces = vec![vec![(v[0].pos, [1.0, 0.0, 0.0]), (v[1].pos, [0.0, 1.0, 0.0]), (v[2].pos, [0.0, 0.0, 1.0])]];
            for (_, mask) in &self.masks {
                let mut next = Vec::new();
                for poly in &pieces {
                    for mt in mask {
                        let c = clip_poly(poly, mt);
                        if c.len() >= 3 {
                            next.push(c);
                        }
                    }
                }
                pieces = next;
                if pieces.is_empty() {
                    break;
                }
            }
            for poly in pieces {
                let base = verts.len() as u32;
                for (pos, w) in &poly {
                    verts.push(mix_vertex(v, *w, *pos));
                }
                for k in 1..poly.len() as u32 - 1 {
                    indices.extend_from_slice(&[base, base + k, base + k + 1]);
                }
            }
        }
        b.verts = verts;
        b.indices = indices;
        self.out.push(b);
    }

    fn shape(&mut self, shape: &Shape, st: &State, ratio: Option<f32>) {
        let m = &st.m;
        let cx = &st.cx;
        let t = ratio.unwrap_or(0.0);
        let lerp2 = |a: [f32; 2], b: [f32; 2]| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        let tl = if ratio.is_some() { lerp2(shape.start_bounds.tl, shape.end_bounds.tl) } else { shape.start_bounds.tl };
        for fill in &shape.fills {
            let style = &fill.style;
            let mut color = [1.0f32; 4];
            let mut texture = TexRef::White;
            let mut shader = Shader::Gui;
            // Texture-space mapping: st = inv.transform((v - tl) / size * 20) * scale + offset.
            let mut inv = Matrix::IDENTITY;
            let mut size = [1.0f32, 1.0];
            let mut atlas: Option<([f32; 2], [f32; 2])> = None;
            if let Some((mat, w, h)) = &st.material {
                texture = TexRef::Material(mat.clone());
                inv = Matrix { xx: 0.05, yy: 0.05, xy: 0.0, yx: 0.0, tx: 0.0, ty: 0.0 };
                if *w > 0 {
                    size[0] = *w as f32;
                }
                if *h > 0 {
                    size[1] = *h as f32;
                }
            } else {
                match style.fill_kind() {
                    FillKind::Solid => {
                        let (a, b) = (style.start_color, style.end_color);
                        color = std::array::from_fn(|i| {
                            let x = a[i] as f32 / 255.0;
                            let y = b[i] as f32 / 255.0;
                            x + (y - x) * t
                        });
                    }
                    FillKind::Bitmap if style.bitmap_id != 0xffff => {
                        if let Some(DictEntry::Image(img)) = self.p.swf.dict.get(style.bitmap_id as usize) {
                            texture = TexRef::Atlas;
                            shader = Shader::Atlas;
                            color = img.channel_scale;
                            size = [img.size[0] as f32, img.size[1] as f32];
                            let aw = self.p.swf.atlas_width as f32;
                            let ah = self.p.swf.atlas_height as f32;
                            atlas = Some(([img.size[0] as f32 / aw, img.size[1] as f32 / ah], [img.atlas_offset[0] as f32 / aw, img.atlas_offset[1] as f32 / ah]));
                            let fm = if ratio.is_some() { style.start_matrix.lerp(&style.end_matrix, t) } else { style.start_matrix };
                            inv = fm.inverse_linear();
                        }
                    }
                    _ => {}
                }
            }
            let color = pack(std::array::from_fn(|i| (color[i] * cx.mul[i]).clamp(0.0, 1.0)));
            let add = pack_add(cx.add);
            if cx.add[3] + color[3] <= 0.001 && cx.add[3] + cx.mul[3] * 1.0 <= 0.001 {
                continue;
            }
            let non_solid = st.material.is_some() || style.fill_kind() != FillKind::Solid;
            let mut verts = Vec::with_capacity(fill.start_verts.len());
            for (i, &v0) in fill.start_verts.iter().enumerate() {
                let v = if ratio.is_some() { lerp2(v0, *fill.end_verts.get(i).unwrap_or(&v0)) } else { v0 };
                let mut uv = [0.0, 0.0];
                if non_solid {
                    let u0 = (v[0] - tl[0]) / size[0] * 20.0;
                    let v1 = (v[1] - tl[1]) / size[1] * 20.0;
                    let mut s = inv.xx * u0 + inv.xy * v1 + inv.tx;
                    let mut tt = inv.yx * u0 + inv.yy * v1 + inv.ty;
                    if let Some((scale, off)) = atlas {
                        s = s * scale[0] + off[0];
                        tt = tt * scale[1] + off[1];
                    }
                    uv = [s.clamp(0.0, 1.0), tt.clamp(0.0, 1.0)];
                }
                verts.push(Vertex { pos: m.transform(v), uv, color, add });
            }
            let indices = fill.indices.iter().map(|&i| i as u32).collect();
            self.emit(Batch { texture, shader, blend: Blend::from_swf(st.blend), stencil: st.stencil, op: StencilOp::Draw, verts, indices });
        }
        for line in &shape.lines {
            let c: [f32; 4] = std::array::from_fn(|i| {
                let x = line.start_color[i] as f32 / 255.0;
                let y = line.end_color[i] as f32 / 255.0;
                ((x + (y - x) * t) * cx.mul[i]).clamp(0.0, 1.0)
            });
            if cx.add[3] + c[3] <= 0.001 {
                continue;
            }
            let (color, add) = (pack(c), pack_add(cx.add));
            let verts = line
                .start_verts
                .iter()
                .enumerate()
                .map(|(i, &v0)| {
                    let v = if ratio.is_some() { lerp2(v0, *line.end_verts.get(i).unwrap_or(&v0)) } else { v0 };
                    Vertex { pos: m.transform(v), uv: [0.0, 0.0], color, add }
                })
                .collect();
            let indices = line.indices.iter().map(|&i| i as u32).collect();
            self.emit(Batch { texture: TexRef::White, shader: Shader::Gui, blend: Blend::from_swf(st.blend), stencil: st.stencil, op: StencilOp::Draw, verts, indices });
        }
    }

    fn edit_text(&mut self, tid: ObjId, st: &State) {
        let p = self.p;
        let Some(t) = p.text(tid) else { return };
        let Some(DictEntry::EditText(def)) = p.swf.dict.get(t.def as usize) else { return };
        let Some(DictEntry::Font(sfont)) = p.swf.dict.get(def.font_id as usize) else { return };
        let Some(font) = p.font(&sfont.name) else { return };
        let text = p.localize(&t.text).into_owned();
        if text.is_empty() {
            return;
        }
        let m = &st.m;
        let cx = &st.cx;
        let sx = (m.xx * m.xx + m.yx * m.yx).sqrt();
        let sy = (m.yy * m.yy + m.xy * m.xy).sqrt();
        if sx <= 0.0 || sy <= 0.0 {
            return;
        }
        let (n_xx, n_xy, n_yy, n_yx) = (m.xx / sx, m.xy / sx, m.yy / sy, m.yx / sy);
        let to_screen = |x: f32, y: f32| [x * n_xx + y * n_xy + m.tx, x * n_yx + y * n_yy + m.ty];
        let scale = def.font_height as f32 * 0.05 * sy / font.point_size as f32;
        let color: [f32; 4] = std::array::from_fn(|i| t.color[i] as f32 / 255.0 * cx.mul[i] + cx.add[i]);
        if color[3] <= 0.001 {
            return;
        }
        let color = pack(color);
        let flags = def.flags;
        let x0 = (def.left_margin as f32 * 0.05 + def.bounds.tl[0]) * sx;
        let x1 = (def.bounds.br[0] - def.right_margin as f32 * 0.05) * sx;
        let y0 = def.bounds.tl[1] * sy;
        let mut y1 = def.bounds.br[1] * sy;
        let mut width = x1 - x0;
        if flags & 0x10 != 0 {
            width = p.swf.frame_width - x0;
            y1 = p.swf.frame_height;
        }
        let line_h = (font.ascender as f32 - font.descender as f32) * scale;
        let spacing = def.leading as f32 * 0.05 * scale + line_h;
        let max_lines = (((y1 - y0) / spacing) as i32).max(1) as usize;
        let password = flags & 0x04 != 0;
        let glyph_of = |c: char| if password { font.glyph('*' as u32) } else { font.glyph(c as u32) };

        // Line layout (wrap at spaces/hyphens for word-wrapped fields, ellipsis otherwise).
        let mut lines: Vec<(Vec<char>, f32)> = Vec::new();
        let mut cur: Vec<char> = Vec::new();
        let mut cur_w = 0.0f32;
        let mut last_break: Option<(usize, f32)> = None;
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        let mut truncated = false;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            if c == '\r' {
                continue;
            }
            if c == '^' && i < chars.len() && (chars[i].is_ascii_alphanumeric()) {
                i += 1;
                continue;
            }
            if c == '\n' {
                if flags & 0x02 != 0 {
                    lines.push((std::mem::take(&mut cur), cur_w));
                    cur_w = 0.0;
                    last_break = None;
                }
                continue;
            }
            let adv = glyph_of(c).map(|g| g.advance as f32 * scale).unwrap_or(0.0);
            if cur_w + adv > width && !cur.is_empty() {
                if flags & 0x03 == 0 {
                    // Single line: replace the last three glyphs by an ellipsis and stop.
                    let keep = cur.len().saturating_sub(3);
                    cur.truncate(keep);
                    cur.extend(['.', '.', '.']);
                    cur_w = cur.iter().map(|&c| glyph_of(c).map(|g| g.advance as f32 * scale).unwrap_or(0.0)).sum();
                    truncated = true;
                    break;
                }
                match last_break {
                    Some((pos, w)) if pos > 0 => {
                        let rest: Vec<char> = cur.split_off(pos);
                        lines.push((std::mem::take(&mut cur), w));
                        cur = rest.into_iter().skip_while(|&c| c == ' ').collect();
                        cur_w = cur.iter().map(|&c| glyph_of(c).map(|g| g.advance as f32 * scale).unwrap_or(0.0)).sum();
                    }
                    _ => {
                        lines.push((std::mem::take(&mut cur), cur_w));
                        cur_w = 0.0;
                    }
                }
                last_break = None;
            }
            cur.push(c);
            cur_w += adv;
            if c == ' ' || c == '-' {
                last_break = Some((cur.len(), cur_w));
            }
        }
        if !cur.is_empty() || truncated {
            lines.push((cur, cur_w));
        }
        lines.truncate(max_lines);

        let align = t.align;
        let mut verts = Vec::new();
        let mut indices = Vec::new();
        let (aw, ah) = (font.atlas.width as f32, font.atlas.height as f32);
        let pad = font.padding as f32;
        for (li, (line, lw)) in lines.iter().enumerate() {
            let (mut x, pad_mul) = match align {
                1 => (x1 - lw, 2.0),
                2 => ((width - lw) * 0.5 + x0, 2.0),
                _ => (x0, 1.0),
            };
            x -= pad * scale * pad_mul;
            let top = li as f32 * spacing + y0;
            let baseline = top + font.ascender as f32 * scale;
            for &c in line {
                let Some(g) = glyph_of(c) else { continue };
                let gx = x + g.left as f32 * scale;
                let gy = baseline - g.top as f32 * scale;
                let gx1 = gx + g.width as f32 * scale + 1.0;
                let gy1 = gy + g.height as f32 * scale + 1.0;
                if g.width > 0 && g.height > 0 {
                    let u0 = (g.s as f32 - 0.5) / aw;
                    let v0 = (g.t as f32 - 0.5) / ah;
                    let u1 = (g.s as f32 + g.width as f32 + 0.5) / aw;
                    let v1 = (g.t as f32 + g.height as f32 + 0.5) / ah;
                    let base = verts.len() as u32;
                    for (px, py, u, v) in [(gx, gy, u0, v0), (gx1, gy, u1, v0), (gx1, gy1, u1, v1), (gx, gy1, u0, v1)] {
                        verts.push(Vertex { pos: to_screen(px, py), uv: [u, v], color, add: [0.0; 4] });
                    }
                    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                }
                x += g.advance as f32 * scale;
            }
        }
        self.emit(Batch {
            texture: TexRef::Font(Arc::from(sfont.name.as_str())),
            shader: Shader::Sdf,
            blend: Blend::from_swf(st.blend),
            stencil: st.stencil,
            op: StencilOp::Draw,
            verts,
            indices,
        });
    }
}

/// The sprite's state (matrix, colour) after its own placement, for a `gui_w` x `gui_h` GUI, with the ancestors' visibility
/// and transparency applied; `None` when it or an ancestor is hidden or fully transparent.
fn placed_state(p: &Player, gui_w: f32, gui_h: f32, id: ObjId) -> Option<State> {
    // Ancestor chain root -> id with the display entry of each child.
    let mut chain = Vec::new();
    let mut cur = id;
    while cur != p.root {
        let (parent, depth) = p.parent_depth(cur)?;
        let d = p.sprite(parent)?.display.iter().find(|d| d.depth == depth && d.inst == Some(cur))?.clone();
        if !d.visible {
            return None;
        }
        chain.push((parent, d));
        cur = parent;
    }
    chain.reverse();
    let swf = &p.swf;
    let s = (gui_w / swf.frame_width).min(gui_h / swf.frame_height);
    let mut state = State {
        m: Matrix { xx: s, yy: s, xy: 0.0, yx: 0.0, tx: (gui_w - swf.frame_width * s) * 0.5, ty: (gui_h - swf.frame_height * s) * 0.5 },
        cx: ColorXform::IDENTITY,
        blend: 0,
        stencil: 0,
        material: None,
    };
    let mut out = DrawList::default();
    let ctx = Ctx { p, out: &mut out, gui_w, gui_h, geometric: true, masks: Vec::new() };
    for (parent, d) in &chain {
        let sp = p.sprite(*parent)?;
        let mut child = ctx.child_state(&state, d, 0, sp.material.clone().map(|m| (m, sp.material_width, sp.material_height)));
        if let Some(c) = d.inst {
            ctx.anchor(c, d, &state, &mut child);
        }
        if child.cx.mul[3] + child.cx.add[3] <= 0.001 {
            return None;
        }
        state = child;
    }
    Some(state)
}

/// GUI-space position of the sprite's local origin and its horizontal scale (GUI pixels per stage unit), for pointer
/// maths on markers that draw nothing (slider extents). `None` when it or an ancestor is hidden.
pub fn sprite_origin(p: &Player, gui_w: f32, gui_h: f32, id: ObjId) -> Option<([f32; 2], f32)> {
    let st = placed_state(p, gui_w, gui_h, id)?;
    Some(([st.m.tx, st.m.ty], st.m.xx))
}

/// GUI-space bounding box (x0, y0, x1, y1) of what sprite `id` draws, for a `gui_w` x `gui_h` GUI: the same
/// matrices, colour and anchoring as [`draw`], so it matches the picture (for pointer hit tests). `None` when
/// the sprite or an ancestor is hidden or fully transparent, or it draws nothing.
pub fn sprite_bounds(p: &Player, gui_w: f32, gui_h: f32, id: ObjId) -> Option<[f32; 4]> {
    let state = placed_state(p, gui_w, gui_h, id)?;
    let mut out = DrawList::default();
    let mut ctx = Ctx { p, out: &mut out, gui_w, gui_h, geometric: true, masks: Vec::new() };
    ctx.sprite(id, &state);
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for v in out.batches.iter().flat_map(|b| &b.verts) {
        b = [b[0].min(v.pos[0]), b[1].min(v.pos[1]), b[2].max(v.pos[0]), b[3].max(v.pos[1])];
    }
    (b[0] <= b[2]).then_some(b)
}
