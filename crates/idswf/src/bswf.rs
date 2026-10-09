//! Binary SWF (`.bswf`) files, DOOM (2016) version 0x17.
//!
//! Layout recovered from the game's loader (`idSWF::LoadBinary`, 0x1419ba790 in Steam build 13954591).
//! Integers and floats are big-endian; strings are a little-endian u32 length plus bytes.
//!
//! ```text
//! u32 magic 'BSW\x17', u32 source timestamp
//! f32 frame width, f32 frame height, u16 frame rate (8.8)
//! i32 atlas width, i32 atlas height          (the companion .bimage, BC7)
//! sprite main timeline
//! i32 dictionary count, then per entry i32 type + body
//! ```
//!
//! Sprite (`idSWFSprite::Read`, 0x1417ee4a0): u16 frame count, i32 n + n×u32 frame offsets (n = frames + 1,
//! indices into the command list), i32 n + n×(u32 frame, string label), i32 n + n×(u32 tag, u32 length, bytes),
//! i32 n + n×(u32 length, bytes) DoInitAction buffers. Command bodies are ordinary little-endian SWF tag bodies.

use anyhow::{Context, Result, bail, ensure};

pub const MAGIC: u32 = 0x4253_5717;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub tl: [f32; 2],
    pub br: [f32; 2],
}

/// 2D affine matrix, stored in the engine's order (xx, yy, xy, yx, tx, ty):
/// x' = xx·x + xy·y + tx, y' = yx·x + yy·y + ty. SWF ScaleX/ScaleY are xx/yy, RotateSkew0 is yx and
/// RotateSkew1 is xy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub xx: f32,
    pub yy: f32,
    pub xy: f32,
    pub yx: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { xx: 1.0, yy: 1.0, xy: 0.0, yx: 0.0, tx: 0.0, ty: 0.0 };

    pub fn transform(&self, p: [f32; 2]) -> [f32; 2] {
        [self.xx * p[0] + self.xy * p[1] + self.tx, self.yx * p[0] + self.yy * p[1] + self.ty]
    }

    /// `self` applied after `inner` (parent × child).
    pub fn mul(&self, inner: &Matrix) -> Matrix {
        Matrix {
            xx: self.xx * inner.xx + self.xy * inner.yx,
            xy: self.xx * inner.xy + self.xy * inner.yy,
            yx: self.yx * inner.xx + self.yy * inner.yx,
            yy: self.yx * inner.xy + self.yy * inner.yy,
            tx: self.xx * inner.tx + self.xy * inner.ty + self.tx,
            ty: self.yx * inner.tx + self.yy * inner.ty + self.ty,
        }
    }

    pub fn inverse(&self) -> Matrix {
        let det = self.xx * self.yy - self.xy * self.yx;
        if det.abs() < 1e-12 {
            return *self;
        }
        let inv = 1.0 / det;
        let xx = self.yy * inv;
        let yy = self.xx * inv;
        let xy = -self.xy * inv;
        let yx = -self.yx * inv;
        Matrix { xx, yy, xy, yx, tx: -(xx * self.tx + xy * self.ty), ty: -(yx * self.tx + yy * self.ty) }
    }

    /// The engine's fill-matrix inverse (0x14161db40): inverts the 2×2 part and drops the translation.
    pub fn inverse_linear(&self) -> Matrix {
        let det = self.xx * self.yy - self.xy * self.yx;
        if det.abs() < 1e-12 {
            return *self;
        }
        let inv = 1.0 / det;
        Matrix { xx: self.yy * inv, yy: self.xx * inv, xy: -self.xy * inv, yx: -self.yx * inv, tx: 0.0, ty: 0.0 }
    }

    pub fn lerp(&self, o: &Matrix, t: f32) -> Matrix {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Matrix { xx: l(self.xx, o.xx), yy: l(self.yy, o.yy), xy: l(self.xy, o.xy), yx: l(self.yx, o.yx), tx: l(self.tx, o.tx), ty: l(self.ty, o.ty) }
    }
}

#[derive(Debug, Clone)]
pub struct Image {
    /// `None` when the image lives in the SWF's own atlas (`<swf>.bimage`); otherwise a material decl name.
    pub material: Option<String>,
    pub size: [i32; 2],
    pub atlas_offset: [i32; 2],
    pub channel_scale: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillKind {
    Solid,
    Gradient,
    Bitmap,
    Other(u8),
}

#[derive(Debug, Clone, Copy)]
pub struct GradientRecord {
    pub start_ratio: u8,
    pub end_ratio: u8,
    pub start_color: [u8; 4],
    pub end_color: [u8; 4],
}

#[derive(Debug, Clone)]
pub struct FillStyle {
    /// 0 solid, 1 gradient, 4 bitmap (SWF fill type >> 4).
    pub kind: u8,
    /// gradient: 0 linear, 2 radial, 3 focal; bitmap: 0 repeat, 1 clip, 2 repeat (hard), 3 clip (hard).
    pub sub_type: u8,
    pub start_color: [u8; 4],
    pub end_color: [u8; 4],
    pub start_matrix: Matrix,
    pub end_matrix: Matrix,
    pub gradient: Vec<GradientRecord>,
    pub focal_point: f32,
    pub bitmap_id: u16,
}

impl FillStyle {
    pub fn fill_kind(&self) -> FillKind {
        match self.kind {
            0 => FillKind::Solid,
            1 => FillKind::Gradient,
            4 => FillKind::Bitmap,
            k => FillKind::Other(k),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShapeFill {
    pub style: FillStyle,
    pub start_verts: Vec<[f32; 2]>,
    pub end_verts: Vec<[f32; 2]>,
    pub indices: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct ShapeLine {
    pub start_width: u16,
    pub end_width: u16,
    pub start_color: [u8; 4],
    pub end_color: [u8; 4],
    pub start_verts: Vec<[f32; 2]>,
    pub end_verts: Vec<[f32; 2]>,
    pub indices: Vec<u16>,
}

/// A pre-triangulated shape (also used for morph shapes, which carry distinct start/end data).
#[derive(Debug, Clone)]
pub struct Shape {
    pub start_bounds: Rect,
    pub end_bounds: Rect,
    pub fills: Vec<ShapeFill>,
    pub lines: Vec<ShapeLine>,
}

#[derive(Debug, Clone)]
pub struct FontGlyph {
    pub code: u16,
    pub advance: i16,
    pub verts: Vec<[f32; 2]>,
    pub indices: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct Font {
    /// Font face name, resolved by the engine's font manager (e.g. "TT Supermolot" -> fonts/tt_supermolot).
    pub name: String,
    pub ascent: i16,
    pub descent: i16,
    pub leading: i16,
    pub glyphs: Vec<FontGlyph>,
}

#[derive(Debug, Clone, Copy)]
pub struct TextRecord {
    pub font_id: u16,
    pub color: [u8; 4],
    pub x_offset: i16,
    pub y_offset: i16,
    pub text_height: u16,
    pub first_glyph: u16,
    pub num_glyphs: u8,
}

#[derive(Debug, Clone)]
pub struct Text {
    pub bounds: Rect,
    pub matrix: Matrix,
    pub records: Vec<TextRecord>,
    /// (glyph index into the font, advance)
    pub glyphs: Vec<(i32, i32)>,
}

#[derive(Debug, Clone)]
pub struct EditText {
    pub bounds: Rect,
    pub flags: u32,
    pub font_id: u16,
    pub font_height: u16,
    pub color: [u8; 4],
    pub max_length: u16,
    pub align: i32,
    pub left_margin: u16,
    pub right_margin: u16,
    pub indent: u16,
    pub leading: i16,
    pub variable: String,
    pub initial_text: String,
}

#[derive(Debug, Clone)]
pub struct Command {
    pub tag: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Sprite {
    pub frame_count: u16,
    /// `frame_offsets[f]..frame_offsets[f + 1]` are the commands of frame `f` (0-based).
    pub frame_offsets: Vec<u32>,
    /// (0-based frame, label)
    pub frame_labels: Vec<(u32, String)>,
    pub commands: Vec<Command>,
    pub do_init_actions: Vec<Vec<u8>>,
}

impl Sprite {
    pub fn frame_commands(&self, frame: usize) -> &[Command] {
        let a = self.frame_offsets.get(frame).copied().unwrap_or(0) as usize;
        let b = self.frame_offsets.get(frame + 1).copied().unwrap_or(a as u32) as usize;
        &self.commands[a.min(self.commands.len())..b.min(self.commands.len())]
    }

    pub fn find_label(&self, label: &str) -> Option<u32> {
        self.frame_labels.iter().find(|(_, l)| l.eq_ignore_ascii_case(label)).map(|(f, _)| *f)
    }
}

#[derive(Debug, Clone)]
pub enum DictEntry {
    None,
    Image(Image),
    Shape(Shape),
    Morph(Shape),
    Sprite(Sprite),
    Font(Font),
    Text(Text),
    EditText(EditText),
}

impl DictEntry {
    pub fn type_name(&self) -> &'static str {
        match self {
            DictEntry::None => "none",
            DictEntry::Image(_) => "image",
            DictEntry::Shape(_) => "shape",
            DictEntry::Morph(_) => "morph",
            DictEntry::Sprite(_) => "sprite",
            DictEntry::Font(_) => "font",
            DictEntry::Text(_) => "text",
            DictEntry::EditText(_) => "edittext",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Swf {
    pub timestamp: u32,
    pub frame_width: f32,
    pub frame_height: f32,
    /// 8.8 fixed point frames per second.
    pub frame_rate: u16,
    pub atlas_width: i32,
    pub atlas_height: i32,
    pub main: Sprite,
    pub dict: Vec<DictEntry>,
}

impl Swf {
    pub fn parse(bytes: &[u8]) -> Result<Swf> {
        let mut r = Reader { b: bytes, pos: 0 };
        let magic = r.u32()?;
        ensure!(magic == MAGIC, "bad bswf magic {magic:#x}");
        let timestamp = r.u32()?;
        let frame_width = r.f32()?;
        let frame_height = r.f32()?;
        let frame_rate = r.u16()?;
        let atlas_width = r.i32()?;
        let atlas_height = r.i32()?;
        let main = read_sprite(&mut r).context("main sprite")?;
        let n = r.count()?;
        let mut dict = Vec::with_capacity(n);
        for i in 0..n {
            let at = r.pos;
            let e = read_entry(&mut r).with_context(|| format!("dictionary entry {i} at {at:#x}"))?;
            dict.push(e);
        }
        ensure!(r.pos == bytes.len(), "{} trailing bytes", bytes.len() - r.pos);
        Ok(Swf { timestamp, frame_width, frame_height, frame_rate, atlas_width, atlas_height, main, dict })
    }

    pub fn frames_per_second(&self) -> f32 {
        self.frame_rate as f32 / 256.0
    }

    pub fn sprite(&self, id: u16) -> Option<&Sprite> {
        match self.dict.get(id as usize)? {
            DictEntry::Sprite(s) => Some(s),
            _ => None,
        }
    }
}

fn read_entry(r: &mut Reader) -> Result<DictEntry> {
    let kind = r.i32()?;
    Ok(match kind {
        0 => DictEntry::None,
        1 => {
            let name = r.string()?;
            let material = if name.starts_with('.') { None } else { Some(name) };
            let mut size = [0; 2];
            let mut atlas_offset = [0; 2];
            for j in 0..2 {
                size[j] = r.i32()?;
                atlas_offset[j] = r.i32()?;
            }
            let mut channel_scale = [0.0; 4];
            for c in &mut channel_scale {
                *c = r.f32()?;
            }
            DictEntry::Image(Image { material, size, atlas_offset, channel_scale })
        }
        2 | 3 => {
            let start_bounds = r.rect()?;
            let end_bounds = r.rect()?;
            let nf = r.count()?;
            let mut fills = Vec::with_capacity(nf);
            for _ in 0..nf {
                let kind = r.u8()?;
                let sub_type = r.u8()?;
                let start_color = r.rgba()?;
                let end_color = r.rgba()?;
                let start_matrix = r.matrix()?;
                let end_matrix = r.matrix()?;
                let ng = r.u8()? as usize;
                ensure!(ng <= 16, "{ng} gradient records");
                let mut gradient = Vec::with_capacity(ng);
                for _ in 0..ng {
                    gradient.push(GradientRecord { start_ratio: r.u8()?, end_ratio: r.u8()?, start_color: r.rgba()?, end_color: r.rgba()? });
                }
                let focal_point = r.f32()?;
                let bitmap_id = r.u16()?;
                let start_verts = r.verts()?;
                let end_verts = r.verts()?;
                let indices = r.indices()?;
                fills.push(ShapeFill {
                    style: FillStyle { kind, sub_type, start_color, end_color, start_matrix, end_matrix, gradient, focal_point, bitmap_id },
                    start_verts,
                    end_verts,
                    indices,
                });
            }
            let nl = r.count()?;
            let mut lines = Vec::with_capacity(nl);
            for _ in 0..nl {
                lines.push(ShapeLine {
                    start_width: r.u16()?,
                    end_width: r.u16()?,
                    start_color: r.rgba()?,
                    end_color: r.rgba()?,
                    start_verts: r.verts()?,
                    end_verts: r.verts()?,
                    indices: r.indices()?,
                });
            }
            let shape = Shape { start_bounds, end_bounds, fills, lines };
            if kind == 2 { DictEntry::Shape(shape) } else { DictEntry::Morph(shape) }
        }
        4 => DictEntry::Sprite(read_sprite(r)?),
        5 => {
            let name = r.string()?;
            let ascent = r.i16()?;
            let descent = r.i16()?;
            let leading = r.i16()?;
            let n = r.count()?;
            let mut glyphs = Vec::with_capacity(n);
            for _ in 0..n {
                glyphs.push(FontGlyph { code: r.u16()?, advance: r.i16()?, verts: r.verts()?, indices: r.indices()? });
            }
            DictEntry::Font(Font { name, ascent, descent, leading, glyphs })
        }
        6 => {
            let bounds = r.rect()?;
            let matrix = r.matrix()?;
            let n = r.count()?;
            let mut records = Vec::with_capacity(n);
            for _ in 0..n {
                records.push(TextRecord {
                    font_id: r.u16()?,
                    color: r.rgba()?,
                    x_offset: r.i16()?,
                    y_offset: r.i16()?,
                    text_height: r.u16()?,
                    first_glyph: r.u16()?,
                    num_glyphs: r.u8()?,
                });
            }
            let n = r.count()?;
            let mut glyphs = Vec::with_capacity(n);
            for _ in 0..n {
                glyphs.push((r.i32()?, r.i32()?));
            }
            DictEntry::Text(Text { bounds, matrix, records, glyphs })
        }
        7 => DictEntry::EditText(EditText {
            bounds: r.rect()?,
            flags: r.u32()?,
            font_id: r.u16()?,
            font_height: r.u16()?,
            color: r.rgba()?,
            max_length: r.u16()?,
            align: r.i32()?,
            left_margin: r.u16()?,
            right_margin: r.u16()?,
            indent: r.u16()?,
            leading: r.i16()?,
            variable: r.string()?,
            initial_text: r.string()?,
        }),
        k => bail!("unknown dictionary entry type {k}"),
    })
}

fn read_sprite(r: &mut Reader) -> Result<Sprite> {
    let frame_count = r.u16()?;
    let n = r.count()?;
    let mut frame_offsets = Vec::with_capacity(n);
    for _ in 0..n {
        frame_offsets.push(r.u32()?);
    }
    let n = r.count()?;
    let mut frame_labels = Vec::with_capacity(n);
    for _ in 0..n {
        let f = r.u32()?;
        frame_labels.push((f, r.string()?));
    }
    let n = r.count()?;
    let mut commands = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = r.u32()?;
        let len = r.u32()? as usize;
        commands.push(Command { tag, data: r.take(len)?.to_vec() });
    }
    let n = r.count()?;
    let mut do_init_actions = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.u32()? as usize;
        do_init_actions.push(r.take(len)?.to_vec());
    }
    Ok(Sprite { frame_count, frame_offsets, frame_labels, commands, do_init_actions })
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos + n).with_context(|| format!("unexpected end of file at {:#x} (+{n})", self.pos))?;
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.i32()?;
        ensure!((0..=0x100_0000).contains(&n), "bad count {n} at {:#x}", self.pos - 4);
        Ok(n as usize)
    }
    fn rgba(&mut self) -> Result<[u8; 4]> {
        Ok(self.take(4)?.try_into()?)
    }
    fn vec2(&mut self) -> Result<[f32; 2]> {
        Ok([self.f32()?, self.f32()?])
    }
    fn rect(&mut self) -> Result<Rect> {
        Ok(Rect { tl: self.vec2()?, br: self.vec2()? })
    }
    fn matrix(&mut self) -> Result<Matrix> {
        Ok(Matrix { xx: self.f32()?, yy: self.f32()?, xy: self.f32()?, yx: self.f32()?, tx: self.f32()?, ty: self.f32()? })
    }
    fn verts(&mut self) -> Result<Vec<[f32; 2]>> {
        let n = self.count()?;
        (0..n).map(|_| self.vec2()).collect()
    }
    fn indices(&mut self) -> Result<Vec<u16>> {
        let n = self.count()?;
        (0..n).map(|_| self.u16()).collect()
    }
    fn string(&mut self) -> Result<String> {
        let n = u32::from_le_bytes(self.take(4)?.try_into()?) as usize;
        ensure!(n < 0x10_0000, "bad string length {n} at {:#x}", self.pos - 4);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}
