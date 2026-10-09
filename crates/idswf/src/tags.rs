//! Timeline command bodies: ordinary little-endian SWF tags (PlaceObject2/3, RemoveObject2, DoAction).
//!
//! The engine reads MATRIX with translation in twips scaled by 0.05 and CXFORMWITHALPHA terms scaled by 1/256
//! (bitstream readers at 0x141a48950 / 0x141a48310).

use anyhow::{Result, bail};

use crate::bswf::{Command, Matrix};

pub const TAG_DO_ACTION: u32 = 12;
pub const TAG_PLACE_OBJECT2: u32 = 26;
pub const TAG_REMOVE_OBJECT2: u32 = 28;
pub const TAG_PLACE_OBJECT3: u32 = 70;

/// Colour transform: c' = c * mul + add, every term normalised so 1.0 = 256 in the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorXform {
    pub mul: [f32; 4],
    pub add: [f32; 4],
}

impl Default for ColorXform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl ColorXform {
    pub const IDENTITY: ColorXform = ColorXform { mul: [1.0; 4], add: [0.0; 4] };

    /// `self` (the child) seen through `parent`: mul = child.mul · parent.mul, add = child.add · parent.mul + parent.add.
    pub fn concat(&self, parent: &ColorXform) -> ColorXform {
        let mut o = ColorXform::IDENTITY;
        for i in 0..4 {
            o.mul[i] = self.mul[i] * parent.mul[i];
            o.add[i] = self.add[i] * parent.mul[i] + parent.add[i];
        }
        o
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClipAction {
    pub events: u32,
    pub key_code: u8,
    pub actions: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaceObject {
    pub version: u8,
    pub is_move: bool,
    pub depth: u16,
    pub character: Option<u16>,
    pub matrix: Option<Matrix>,
    pub cxform: Option<ColorXform>,
    pub ratio: Option<u16>,
    pub name: Option<String>,
    pub clip_depth: Option<u16>,
    pub class_name: Option<String>,
    pub blend_mode: Option<u8>,
    pub cache_as_bitmap: Option<u8>,
    pub visible: Option<bool>,
    pub has_filters: bool,
    pub clip_actions: Vec<ClipAction>,
}

#[derive(Debug, Clone)]
pub enum Tag {
    Place(PlaceObject),
    Remove { depth: u16 },
    DoAction(Vec<u8>),
    Other(u32),
}

pub fn decode(cmd: &Command) -> Result<Tag> {
    Ok(match cmd.tag {
        TAG_PLACE_OBJECT2 => Tag::Place(place_object(&cmd.data, 2)?),
        TAG_PLACE_OBJECT3 => Tag::Place(place_object(&cmd.data, 3)?),
        TAG_REMOVE_OBJECT2 => {
            if cmd.data.len() < 2 {
                bail!("short RemoveObject2");
            }
            Tag::Remove { depth: u16::from_le_bytes([cmd.data[0], cmd.data[1]]) }
        }
        TAG_DO_ACTION => Tag::DoAction(cmd.data.clone()),
        t => Tag::Other(t),
    })
}

fn place_object(b: &[u8], version: u8) -> Result<PlaceObject> {
    let mut r = Bits::new(b);
    let f = r.u8()?;
    let f2 = if version == 3 { r.u8()? } else { 0 };
    let mut p = PlaceObject { version, is_move: f & 0x01 != 0, depth: r.u16()?, ..Default::default() };
    if version == 3 && (f2 & 0x08 != 0 || (f2 & 0x10 != 0 && f & 0x02 != 0)) {
        p.class_name = Some(r.cstr()?);
    }
    if f & 0x02 != 0 {
        p.character = Some(r.u16()?);
    }
    if f & 0x04 != 0 {
        p.matrix = Some(r.matrix()?);
    }
    if f & 0x08 != 0 {
        p.cxform = Some(r.cxform()?);
    }
    if f & 0x10 != 0 {
        p.ratio = Some(r.u16()?);
    }
    if f & 0x20 != 0 {
        p.name = Some(r.cstr()?);
    }
    if f & 0x40 != 0 {
        p.clip_depth = Some(r.u16()?);
    }
    if version == 3 {
        if f2 & 0x01 != 0 {
            p.has_filters = true;
            skip_filters(&mut r)?;
        }
        if f2 & 0x02 != 0 {
            p.blend_mode = Some(r.u8()?);
        }
        if f2 & 0x04 != 0 {
            p.cache_as_bitmap = Some(r.u8()?);
        }
        if f2 & 0x20 != 0 {
            p.visible = Some(r.u8()? != 0);
            if r.remaining() >= 4 {
                r.bytes(4)?; // background colour
            }
        }
    }
    if f & 0x80 != 0 {
        p.clip_actions = clip_actions(&mut r)?;
    }
    Ok(p)
}

fn clip_actions(r: &mut Bits) -> Result<Vec<ClipAction>> {
    r.u16()?; // reserved
    r.u32()?; // all event flags
    let mut out = Vec::new();
    loop {
        let events = r.u32()?;
        if events == 0 {
            break;
        }
        let size = r.u32()? as usize;
        let mut key_code = 0;
        let mut size = size;
        if events & 0x0002_0000 != 0 {
            key_code = r.u8()?;
            size = size.saturating_sub(1);
        }
        out.push(ClipAction { events, key_code, actions: r.bytes(size)?.to_vec() });
    }
    Ok(out)
}

fn skip_filters(r: &mut Bits) -> Result<()> {
    let n = r.u8()?;
    for _ in 0..n {
        let id = r.u8()?;
        let len = match id {
            0 => 23, // drop shadow
            1 => 9,  // blur
            2 => 15, // glow
            3 => 27, // bevel
            4 | 7 => {
                // gradient glow / bevel
                let nc = r.u8()? as usize;
                nc * 5 + 19
            }
            5 => {
                // convolution
                let x = r.u8()? as usize;
                let y = r.u8()? as usize;
                8 + x * y * 4 + 5
            }
            6 => 80, // colour matrix
            _ => bail!("unknown filter {id}"),
        };
        r.bytes(len)?;
    }
    Ok(())
}

/// Little-endian SWF reader with MSB-first bit fields.
pub(crate) struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
    bit: u32,
    cur: u8,
}

impl<'a> Bits<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0, bit: 0, cur: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }
    fn align(&mut self) {
        self.bit = 0;
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.align();
        let Some(s) = self.b.get(self.pos..self.pos + n) else { bail!("tag ends early ({} + {n} > {})", self.pos, self.b.len()) };
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        let s = self.bytes(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    pub fn u32(&mut self) -> Result<u32> {
        let s = self.bytes(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub fn cstr(&mut self) -> Result<String> {
        self.align();
        let rest = &self.b[self.pos..];
        let n = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.pos += (n + 1).min(rest.len());
        Ok(s)
    }
    fn ubits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            if self.bit == 0 {
                let Some(&c) = self.b.get(self.pos) else { bail!("bitfield runs past tag end") };
                self.cur = c;
                self.pos += 1;
                self.bit = 8;
            }
            self.bit -= 1;
            v = (v << 1) | ((self.cur >> self.bit) & 1) as u32;
        }
        Ok(v)
    }
    fn sbits(&mut self, n: u32) -> Result<i32> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.ubits(n)?;
        Ok(((v << (32 - n)) as i32) >> (32 - n))
    }
    pub fn matrix(&mut self) -> Result<Matrix> {
        self.align();
        let mut m = Matrix::IDENTITY;
        if self.ubits(1)? != 0 {
            let n = self.ubits(5)?;
            m.xx = self.sbits(n)? as f32 / 65536.0;
            m.yy = self.sbits(n)? as f32 / 65536.0;
        }
        if self.ubits(1)? != 0 {
            let n = self.ubits(5)?;
            m.yx = self.sbits(n)? as f32 / 65536.0; // RotateSkew0
            m.xy = self.sbits(n)? as f32 / 65536.0; // RotateSkew1
        }
        let n = self.ubits(5)?;
        m.tx = self.sbits(n)? as f32 * 0.05;
        m.ty = self.sbits(n)? as f32 * 0.05;
        self.align();
        Ok(m)
    }
    pub fn cxform(&mut self) -> Result<ColorXform> {
        self.align();
        let has_add = self.ubits(1)? != 0;
        let has_mul = self.ubits(1)? != 0;
        let n = self.ubits(4)?;
        let mut c = ColorXform::IDENTITY;
        if has_mul {
            for i in 0..4 {
                c.mul[i] = self.sbits(n)? as f32 / 256.0;
            }
        }
        if has_add {
            for i in 0..4 {
                c.add[i] = self.sbits(n)? as f32 / 256.0;
            }
        }
        self.align();
        Ok(c)
    }
}
