//! ActionScript 2 bytecode (SWF DoAction / DoInitAction / function bodies): decoding and disassembly.

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum PushValue {
    Str(String),
    Float(f32),
    Null,
    Undefined,
    Register(u8),
    Bool(bool),
    Double(f64),
    Int(i32),
    Constant(u16),
}

#[derive(Debug, Clone)]
pub struct FuncParam {
    pub register: u8,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<FuncParam>,
    pub register_count: u8,
    /// DefineFunction2 flags (bit 0 preload this, 1 suppress this, 2 preload arguments, 3 suppress arguments,
    /// 4 preload super, 5 suppress super, 6 preload _root, 7 preload _parent, 8 preload _global).
    pub flags: u16,
    pub v2: bool,
    /// Byte range of the body inside the enclosing action buffer.
    pub body: std::ops::Range<usize>,
}

#[derive(Debug, Clone)]
pub enum Action {
    Simple(u8),
    GotoFrame(u16),
    GetUrl(String, String),
    StoreRegister(u8),
    ConstantPool(Vec<String>),
    WaitForFrame(u16, u8),
    SetTarget(String),
    GotoLabel(String),
    WaitForFrame2(u8),
    DefineFunction(Function),
    Try { flags: u8, try_size: u16, catch_size: u16, finally_size: u16, catch: String },
    With(u16),
    Push(Vec<PushValue>),
    Jump(i16),
    GetUrl2(u8),
    If(i16),
    Call,
    GotoFrame2 { play: bool, bias: Option<u16> },
    Unknown(u8, Vec<u8>),
}

/// Decodes the action at `pc`, returning it and the offset of the next action. Function bodies are not
/// skipped here: the returned `next` points at the body (the caller decides whether to execute it).
pub fn decode(code: &[u8], pc: usize) -> Result<(Action, usize)> {
    let Some(&op) = code.get(pc) else { bail!("pc {pc} past end of {} bytes", code.len()) };
    if op < 0x80 {
        return Ok((Action::Simple(op), pc + 1));
    }
    let Some(lb) = code.get(pc + 1..pc + 3) else { bail!("action {op:#x} header truncated") };
    let len = u16::from_le_bytes([lb[0], lb[1]]) as usize;
    let start = pc + 3;
    let Some(p) = code.get(start..start + len) else { bail!("action {op:#x} payload truncated") };
    let next = start + len;
    let mut r = Rd { b: p, pos: 0 };
    let a = match op {
        0x81 => Action::GotoFrame(r.u16()?),
        0x83 => Action::GetUrl(r.cstr()?, r.cstr()?),
        0x87 => Action::StoreRegister(r.u8()?),
        0x88 => {
            let n = r.u16()?;
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(r.cstr()?);
            }
            Action::ConstantPool(v)
        }
        0x8a => Action::WaitForFrame(r.u16()?, r.u8()?),
        0x8b => Action::SetTarget(r.cstr()?),
        0x8c => Action::GotoLabel(r.cstr()?),
        0x8d => Action::WaitForFrame2(r.u8()?),
        0x8e => {
            let name = r.cstr()?;
            let np = r.u16()?;
            let register_count = r.u8()?;
            let flags = r.u16()?;
            let mut params = Vec::with_capacity(np as usize);
            for _ in 0..np {
                params.push(FuncParam { register: r.u8()?, name: r.cstr()? });
            }
            let size = r.u16()? as usize;
            Action::DefineFunction(Function { name, params, register_count, flags, v2: true, body: next..next + size })
        }
        0x9b => {
            let name = r.cstr()?;
            let np = r.u16()?;
            let mut params = Vec::with_capacity(np as usize);
            for _ in 0..np {
                params.push(FuncParam { register: 0, name: r.cstr()? });
            }
            let size = r.u16()? as usize;
            Action::DefineFunction(Function { name, params, register_count: 0, flags: 0, v2: false, body: next..next + size })
        }
        0x8f => {
            let flags = r.u8()?;
            let try_size = r.u16()?;
            let catch_size = r.u16()?;
            let finally_size = r.u16()?;
            let catch = if flags & 4 != 0 { format!("r{}", r.u8()?) } else { r.cstr()? };
            Action::Try { flags, try_size, catch_size, finally_size, catch }
        }
        0x94 => Action::With(r.u16()?),
        0x96 => {
            let mut v = Vec::new();
            while r.pos < p.len() {
                v.push(match r.u8()? {
                    0 => PushValue::Str(r.cstr()?),
                    1 => PushValue::Float(f32::from_bits(r.u32()?)),
                    2 => PushValue::Null,
                    3 => PushValue::Undefined,
                    4 => PushValue::Register(r.u8()?),
                    5 => PushValue::Bool(r.u8()? != 0),
                    6 => {
                        let hi = r.u32()? as u64;
                        let lo = r.u32()? as u64;
                        PushValue::Double(f64::from_bits((hi << 32) | lo))
                    }
                    7 => PushValue::Int(r.u32()? as i32),
                    8 => PushValue::Constant(r.u8()? as u16),
                    9 => PushValue::Constant(r.u16()?),
                    t => bail!("unknown push type {t}"),
                });
            }
            Action::Push(v)
        }
        0x99 => Action::Jump(r.u16()? as i16),
        0x9a => Action::GetUrl2(r.u8()?),
        0x9d => Action::If(r.u16()? as i16),
        0x9e => Action::Call,
        0x9f => {
            let f = r.u8()?;
            let bias = if f & 2 != 0 { Some(r.u16()?) } else { None };
            Action::GotoFrame2 { play: f & 1 != 0, bias }
        }
        _ => Action::Unknown(op, p.to_vec()),
    };
    Ok((a, next))
}

pub fn simple_name(op: u8) -> &'static str {
    match op {
        0x00 => "end",
        0x04 => "nextFrame",
        0x05 => "prevFrame",
        0x06 => "play",
        0x07 => "stop",
        0x08 => "toggleQuality",
        0x09 => "stopSounds",
        0x0a => "add",
        0x0b => "subtract",
        0x0c => "multiply",
        0x0d => "divide",
        0x0e => "equals",
        0x0f => "less",
        0x10 => "and",
        0x11 => "or",
        0x12 => "not",
        0x13 => "stringEquals",
        0x14 => "stringLength",
        0x15 => "stringExtract",
        0x17 => "pop",
        0x18 => "toInteger",
        0x1c => "getVariable",
        0x1d => "setVariable",
        0x20 => "setTarget2",
        0x21 => "stringAdd",
        0x22 => "getProperty",
        0x23 => "setProperty",
        0x24 => "cloneSprite",
        0x25 => "removeSprite",
        0x26 => "trace",
        0x27 => "startDrag",
        0x28 => "endDrag",
        0x29 => "stringLess",
        0x2a => "throw",
        0x2b => "castOp",
        0x2c => "implementsOp",
        0x30 => "randomNumber",
        0x31 => "mbStringLength",
        0x32 => "charToAscii",
        0x33 => "asciiToChar",
        0x34 => "getTime",
        0x35 => "mbStringExtract",
        0x36 => "mbCharToAscii",
        0x37 => "mbAsciiToChar",
        0x3a => "delete",
        0x3b => "delete2",
        0x3c => "defineLocal",
        0x3d => "callFunction",
        0x3e => "return",
        0x3f => "modulo",
        0x40 => "newObject",
        0x41 => "defineLocal2",
        0x42 => "initArray",
        0x43 => "initObject",
        0x44 => "typeOf",
        0x45 => "targetPath",
        0x46 => "enumerate",
        0x47 => "add2",
        0x48 => "less2",
        0x49 => "equals2",
        0x4a => "toNumber",
        0x4b => "toString",
        0x4c => "pushDuplicate",
        0x4d => "stackSwap",
        0x4e => "getMember",
        0x4f => "setMember",
        0x50 => "increment",
        0x51 => "decrement",
        0x52 => "callMethod",
        0x53 => "newMethod",
        0x54 => "instanceOf",
        0x55 => "enumerate2",
        0x60 => "bitAnd",
        0x61 => "bitOr",
        0x62 => "bitXor",
        0x63 => "bitLShift",
        0x64 => "bitRShift",
        0x65 => "bitURShift",
        0x66 => "strictEquals",
        0x67 => "greater",
        0x68 => "stringGreater",
        0x69 => "extends",
        _ => "?",
    }
}

/// Human-readable listing (for swfx); nested function bodies are indented.
pub fn disassemble(code: &[u8], out: &mut String) {
    let mut pool: Vec<String> = Vec::new();
    disasm_range(code, 0, code.len(), 0, &mut pool, out);
}

fn disasm_range(code: &[u8], mut pc: usize, end: usize, indent: usize, pool: &mut Vec<String>, out: &mut String) {
    use std::fmt::Write;
    let pad = "  ".repeat(indent);
    while pc < end {
        let (a, next) = match decode(code, pc) {
            Ok(x) => x,
            Err(e) => {
                let _ = writeln!(out, "{pad}{pc:5}: <{e}>");
                return;
            }
        };
        let text = match &a {
            Action::Simple(op) => simple_name(*op).to_string(),
            Action::Push(v) => {
                let items: Vec<String> = v
                    .iter()
                    .map(|p| match p {
                        PushValue::Constant(i) => format!("{:?}", pool.get(*i as usize).map(|s| s.as_str()).unwrap_or("<bad const>")),
                        PushValue::Str(s) => format!("{s:?}"),
                        PushValue::Register(r) => format!("r{r}"),
                        other => format!("{other:?}"),
                    })
                    .collect();
                format!("push {}", items.join(", "))
            }
            Action::ConstantPool(v) => {
                *pool = v.clone();
                format!("constantPool [{} strings]", v.len())
            }
            Action::Jump(o) => format!("jump {}", next as i64 + *o as i64),
            Action::If(o) => format!("if {}", next as i64 + *o as i64),
            Action::DefineFunction(f) => {
                let params: Vec<String> = f.params.iter().map(|p| if f.v2 { format!("r{}:{}", p.register, p.name) } else { p.name.clone() }).collect();
                format!("function{} {}({}) regs {} flags {:#x}", if f.v2 { "2" } else { "" }, f.name, params.join(", "), f.register_count, f.flags)
            }
            other => format!("{other:?}"),
        };
        let _ = writeln!(out, "{pad}{pc:5}: {text}");
        if let Action::DefineFunction(f) = &a {
            disasm_range(code, f.body.start, f.body.end.min(code.len()), indent + 1, pool, out);
            pc = f.body.end;
        } else {
            pc = next;
        }
        if matches!(a, Action::Simple(0)) {
            break;
        }
    }
}

struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Rd<'_> {
    fn u8(&mut self) -> Result<u8> {
        let Some(&v) = self.b.get(self.pos) else { bail!("action payload truncated") };
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes([self.u8()?, self.u8()?, self.u8()?, self.u8()?]))
    }
    fn cstr(&mut self) -> Result<String> {
        let rest = &self.b[self.pos.min(self.b.len())..];
        let n = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.pos += (n + 1).min(rest.len());
        Ok(s)
    }
}
